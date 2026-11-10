# VictoriaLogs storage engine — research notes for llmobs spec 03

Repo: `/home/sebastien/travaux/VictoriaLogs` (Go). HEAD `8befc88`, cloned 2026-09-21. The `tip` section of `docs/victorialogs/CHANGELOG.md` sits above v1.52.0 (released 2026-07-16). I could not run `git log` because this session's sandbox blocks git commands outside its own worktree, so the exact commit date is unknown: it is on or before 2026-09-21. The lessons in §13 come from the CHANGELOG instead of the git log.

All paths below are relative to the repo root. Storage code lives in `lib/logstorage/` (`storage.go`, `partition.go`, `datadb.go`, `part*.go`, `inmemory_part.go`, `block_*.go`, `indexdb.go`, `delete_task.go`). Filesystem helpers are vendored from VictoriaMetrics under `vendor/github.com/VictoriaMetrics/VictoriaMetrics/lib/{fs,filestream,mergeset,memory}`.

Storage hierarchy: `Storage` → per-day `partition` (`partitions/YYYYMMDD/`) → each partition holds an `indexdb` (stream registry, VictoriaMetrics `mergeset`) and a `datadb` (log rows) → `datadb` holds in-memory, small and big **parts** → a part holds **blocks**, one stream per block.

---

## 1. Write path and durability

- **No WAL.** Ingest goes through `Storage.MustAddRows` (`storage.go:1147-1225`), which routes rows by day to a partition (hot-partition fast path, then per-day split), then to `partition.mustAddRows` (`partition.go:135-178`). That registers new streams in indexdb and appends to `datadb.rb`, a `rowsBuffer` sharded per CPU (`datadb.go:716-811`).
- **The acknowledgement comes when the rows are in RAM.** The HTTP handler's `logMessageProcessor` flushes into `MustAddRows` when the request ends, and periodically about every 1 s (`app/vlinsert/insertutil/common_params.go:226-245, 319-334`). The response then returns. Nothing is fsynced on the ack path.
- **Data-loss window:**
  - A shard of the rows buffer becomes an in-memory part after 1 s (`time.AfterFunc(time.Second…)`, `datadb.go:778-787`) or once it reaches about 1.75 MB (7/8 of 2 MiB, `log_rows.go:103-105`).
  - In-memory parts get a `flushDeadline = now + flushInterval` (`datadb.go:820`) and are written to disk by a ticker (`datadb.go:277-305`).
  - `-inmemoryDataFlushInterval` defaults to **5 s**, with a minimum of 1 s (`app/vlstorage/main.go:49-52`, `storage.go:634`). Merging in-memory parts keeps the earliest deadline of the inputs (`getFlushToDiskDeadline`, `datadb.go:1132-1140`), so a merge never extends the window.
  - Worst case: about 1 s + 5 s (plus flush duration) of acknowledged data is lost on SIGKILL, OOM or power loss.
- **How they make up for it:** durability is pushed to the edge. `vlagent` has an on-disk persistent queue (`-remoteWrite.tmpDataPath`, `docs/victorialogs/vlagent.md:30,68-72`), and HA means sending each log to two single-node instances (`docs/victorialogs/README.md:426-449`).
- **Recovery** needs no replay:
  - read `parts.json`;
  - delete every directory not listed in it (`mustRemoveUnusedDirs`, `datadb.go:1340-1363`);
  - delete partition dirs that were only partly removed (`storage.go:700-705`);
  - recreate a missing `indexdb/`, `datadb/` or `parts.json` when a crash happened during partition creation (`partition.go:83-109`, `datadb.go:1299-1335`).
- **Graceful shutdown** flushes everything with a "final" merge that can't be interrupted (`mustCloseDatadb`, `datadb.go:1233-1274`).

## 2. In-memory parts

- **Structure.** An in-memory part (`inmemory_part.go:14-28`) uses the **exact same encoded format as an on-disk part**, held in `chunkedbuffer`s, one per file. Building it sorts the rows by `(streamID, timestamp)` (`log_rows.go:167-174`) and cuts blocks at 2 MiB uncompressed or at a stream change (`inmemory_part.go:71-107`). Flushing to disk is a plain parallel write of the buffers (`MustStoreToDisk`, `inmemory_part.go:110-149`).
- **Limits:**
  - in-memory parts are at most 10% of allowed memory divided by 20 parts per partition, and at least 1 MB (`getMaxInmemoryPartSize`, `datadb.go:1142-1146`);
  - allowed memory defaults to 60% of RAM, cgroup-aware (`vendor/.../lib/memory/memory.go:14`);
  - `maxInmemoryPartsPerPartition = 20` is a merge-policy hint, not a hard cap. The comment says it may be exceeded when merges fall behind (`datadb.go:28-32`).
- **Merging.** In-memory parts are merged into larger in-memory parts by `inmemoryPartsMerger` (`datadb.go:363-383`). A merge writes its output to a file once any input is a file part, or once the output exceeds the in-memory size limit (`getDstPartType`, `datadb.go:663-677`).
- **Backpressure** is only CPU-based: building an in-memory part takes a slot in a global semaphore sized to the CPU count (`inmemoryPartsConcurrencyCh`, `datadb.go:231, 814-818`). There is no memory-based 429. Ingest only returns 429 in read-only mode, when disk is low (§11).
- **Unflushed data is searchable once it is an in-memory part**, because the search path includes `ddb.inmemoryParts` (`storage_search.go:1532-1550`). Rows still in the rows buffer (at most ~1 s) are not visible. Tests call `DebugFlush`.

## 3. On-disk part layout

- Path: `<data>/partitions/YYYYMMDD/datadb/<16-hex mergeIdx>/` (`datadb.go:679-686`; `mergeIdx` is seeded from `time.Now().UnixNano()`, `datadb.go:214`). Files (`filenames.go`):
  - `metadata.json`: the part header (`part_header.go:16-40`): format version, compressed and uncompressed size, row count, block count, min/max timestamp, number of shards;
  - `metaindex.bin`: index-block headers, loaded fully in memory when the part opens (`part.go:132-137`). Each covers a min `streamID` and a min/max timestamp (`index_block_header.go:12-30`);
  - `index.bin`: block headers (`block_header.go:14-41`): streamID, row count, uncompressed size, timestamps header with min/max, and offsets of the column headers. Index blocks are about 128 KiB (`consts.go:21`);
  - `columns_header_index.bin`, `columns_header.bin`: one `columnHeader` per column per block (`block_header.go:584-616`): value type, **min/max for numeric, IPv4 and timestamp columns**, a small dictionary (≤ 8 values, ≤ 256 B) for low-cardinality columns, and offsets into the values and bloom files. Constant columns are stored inline in the header (`consts.go:42-46, 66-75`);
  - `timestamps.bin`;
  - `message_values.bin` and `message_bloom.bin` for `_msg`;
  - `values.bin<N>` and `bloom.bin<N>`: up to **128 shard files** (`consts.go:16`), with a column→shard map in `column_idxs.bin` (`part.go:202-225`). Each column gets its own file until there are 128, so reading one field of a wide event doesn't touch the others (CHANGELOG v1.11.0, line 960).
- **Block size:** ≤ 2 MiB uncompressed (`consts.go:26`), ≤ 8 Mi rows, ≤ 2000 columns per block (`consts.go:29-35`).
- **Bloom filters:** one per column per block, 16 bits per token and 6 hashes (`bloomfilter.go:15-19`). For strings the tokens are words; numeric types store the encoded values (`block_header.go:567-583`).
- **Compression:** zstd, level 1, 2 or 3 depending on block size; blocks under 128 B are stored plain (`encoding.go:343-370`). Per-block type detection (uint8…64, int64, float64, IPv4, ISO8601, dict, const, string) does most of the size reduction.
- **Sort order:** within a part, blocks are ordered by streamID (tenant first, then a 128-bit hash of the stream labels; `stream_id.go:8-22`), then by timestamp. Parts in the same day **overlap in time**. There is no time slicing inside a day.
- **Size targets:**
  - small parts ≤ remaining RAM / 15 (they should stay in the OS page cache) and ≤ free disk space (`datadb.go:1172-1183`);
  - big parts ≤ min(free disk − reserved, 1 TB) (`datadb.go:22-26, 1185-1188`);
  - big parts are written with `nocache` (fadvise DONTNEED) so merges don't evict hot data (`datadb.go:570-571`, `vendor/.../filestream/filestream.go:18-22`).
- **Partitioning:** one directory per UTC day, `YYYYMMDD` (`partition.go:284-298`).
- **Format versioning:** `partFormatLatestVersion = 3`, and readers still accept v0–v2 (`part.go:121-171`, `part_header.go:74-93`). Old parts are only rewritten by merges.

## 4. Catalog

- Each `datadb/` directory has a **`parts.json`**: a sorted JSON array of the names of its file parts. In-memory parts are never listed (`datadb.go:1276-1297`).
- Commit protocol for a new part (`datadb.go:586-605, 1023-1077`):
  1. Write the part directory **directly at its final name**; there is no temp dir and no rename. Every file is fsynced on close (`filestream.go:270-272`). Then write `metadata.json` and fsync the part dir and its parent (`fs.MustSyncPathAndParentDir`).
  2. Under `partsLock`, swap the inputs for the output in memory and rewrite `parts.json` with `fs.MustWriteAtomic`: write `parts.json.tmp.N`, fsync it, rename it, fsync the dir (`vendor/.../lib/fs/fs.go:71-99`). **This rewrite is the commit point.**
  3. Mark the old parts `mustDrop` and decrement their refcount. The **last reader to drop its reference deletes the directory** (`partWrapper.decRef`, `datadb.go:139-161`). Deletion is by refcount, with no time-based grace period.
- **Crash cleanup:**
  - At open, every directory not in `parts.json` is deleted (`datadb.go:1340-1363`).
  - A part that is listed in `parts.json` but missing on disk is a FATAL error that needs manual action (`datadb.go:176-186`).
  - Recursive deletes first write a `.delete-this-dir` marker and fsync it. At startup, a directory containing the marker, or an empty directory, is deleted again (`vendor/.../lib/fs/dir_remover.go:18-104`, used at `storage.go:701-705`). This works without atomic directory renames, which is deliberate because NFS and object-store FUSE mounts don't support them.
- Snapshots: hard-link every file part and write its own `parts.json` (`datadb.go:986-1021`).
- A `flock.lock` file makes sure only one process uses the data directory (`storage.go:654`, `fs.go:284-294`).

## 5. Merging (compaction)

- **Three tiers per partition:** in-memory, small and big. Each has its own merger goroutines (`datadb.go:236-427`). They loop while `appendPartsToMerge` finds work and exit when it finds none. A new part restarts the merger of its tier (`swapSrcWithDstParts`, `datadb.go:1040-1054`).
- **Choosing what to merge** (`appendPartsToMerge`, `datadb.go:1366-1424`):
  - drop parts larger than `maxOutBytes / 1.7`;
  - sort by size, ascending, with the newest first on ties;
  - search every contiguous window of 2 to 15 parts (`defaultPartsToMerge = 15`). Skip windows where the largest part is more than `len` times the smallest; pick the window that maximises `output size / largest input size`;
  - **merge only if that ratio is ≥ 7.5** (`defaultPartsToMerge / 2`).
  - With this rule a byte is rewritten about log₇.₅(final / initial) times, i.e. bounded write amplification. The constants were chosen empirically (`datadb.go:34-46`).
- **Concurrency:** three global semaphores, each sized to the CPU count and shared by every partition (`datadb.go:230-234`). Several merges can run in the same partition on disjoint sets of parts; the `isInMerge` flag is protected by `partsLock` (`datadb.go:432-450`).
- **Disk-space reservation:** a merge that goes to disk reserves the compressed size of its inputs in a global atomic counter, and checks it against the free space. A non-final merge is skipped if the reservation fails. A final merge (a flush of in-memory data) goes ahead anyway: the code accepts the risk rather than drop data (`datadb.go:500-515, 1199-1221`).
- **Interruptible:** non-final merges stop when `stopCh` closes, and the partial output is removed (`datadb.go:594-602`). A merge longer than a minute is logged with its throughput (`datadb.go:620-631`).
- **Forced merge:** `/internal/force_merge` merges every part of a partition, even when there is only one, so that deletes and retention get applied (`datadb.go:1471-1501`).
- Rows are **not** deduplicated during merges: logs have no identity.

## 6. Query

- **Pruning, in order:**
  1. **Partitions** by day, with a binary search over a sorted slice (`storage_search.go:1379-1414`).
  2. **Parts** by `ph.Min/MaxTimestamp` (`storage_search.go:1532-1550`).
  3. **Stream and tenant:** the partition's indexdb turns a `{stream}` filter into a list of streamIDs (`storage_search.go:1430-1458`). The in-memory metaindex is then binary-searched by tenant or streamID, and each index block is checked against the time range (`storage_search.go:1607-1650`).
  4. **Block headers:** streamID and timestamp min/max.
  5. **Column headers:** numeric min/max, dictionary, const value, and **bloom filters per column per block** for token or exact filters (`block_search.go`).
- **Parallelism:**
  - a pool of `parallelReaders` workers (default 2 × CPU, overridable per query, capped at 2000, `consts.go:6`, `app/vlstorage/main.go:33`) consumes batches of blocks from a channel;
  - partitions are scanned concurrently, at most one per CPU, with a global limiter "for memory usage" (`storage_search.go:1300-1376, 1416`).
- **Caches:**
  - **There is no block or page cache in the process.** It relies on the OS page cache, using `pread` (`fs.MustOpenReaderAt`).
  - Two small two-generation caches built on `sync.Map`: `streamIDCache` for ingest and `filterStreamCache` for stream-filter results (`cache.go`, `storage.go:199-213`). Keys include a partition "generation" number, which is bumped on attach/detach (bug #657).
  - Caches are deliberately **not persisted** across restarts (`storage.go:1098-1103`).
  - `blockSearch` keeps per-block scratch caches only (`block_search.go:160-205`).
- **Memory limits:** each stateful pipe gets a budget of 20% of allowed memory and fails if it goes over (e.g. `pipe_sort.go:175-188`). Nothing spills to disk.
- **"Last N" queries:** a binary search over the time range until the window holds ≤ 2N rows (`app/vlstorage/lastnoptimization.go:42-122`). This is the same idea as our "widening windows" fast path.

## 7. Deletes and retention

- **Delete API** (`-delete.enable`, `docs/victorialogs/README.md:371-422`). Tasks are persisted in `delete_tasks.json`, written atomically (`delete_task.go`, `storage.go:452-479`), and processed **one at a time, partition by partition** (`storage.go:930-1062`).
- **Mechanism:**
  - flush the rows buffer;
  - find the parts with matching rows by running the filter with full pruning (`part.hasMatchingRows`, `storage_search.go:1562-1603`);
  - **merge all of those parts together with a `dropFilter`**, which skips matching rows (`datadb.go:1503-1532`, `block_stream_merger.go:22, 251-326`).
  - Parts that are already in a merge make the task re-queue (`needRepeat`).
- **No tombstones:** "logs scheduled for deletion may remain visible until the deletion task is complete" (README:415).
- The delete bound is the log's `_time ≤ task start`, not the ingest time (`storage.go:1004-1005`). Bug #1213 (v1.49.0) evaluated relative time filters at execution time instead of task start time, so delayed tasks deleted the wrong range.
- **Retention:**
  - Every hour (with jitter), drop whole day partitions older than `-retentionPeriod` (global; minimum 1 day). The dirs are removed with refcount plus the delete marker (`storage.go:786-829`).
  - The days that were dropped are remembered in `deletedPartitions`, so **late rows can't recreate a partition** retention already removed (`storage.go:1252-1301`). Rows outside `[now − retention or maxBackfillAge, now + futureRetention]` are rejected at ingest with a throttled warning (`storage.go:1166-1203`).
  - **Disk-usage retention** (`-retention.maxDiskSpaceUsageBytes` or `-retention.maxDiskUsagePercent`) drops the oldest days but always keeps the last two (`storage.go:831-908`).

## 8. Tenancy

`TenantID{AccountID, ProjectID}` (`tenant_id.go:15-24`) is the **first component of `streamID`**, so all of a tenant's blocks are contiguous inside each part (`stream_id.go:11-22`). Tenants share partitions and parts. Isolation comes from always searching with `tenantIDs` and binary-searching the metaindex and block headers (`storage_search.go:1607-1650`). Retention is global, not per tenant.

## 9. Object storage

Not supported. It is on the roadmap (`docs/victorialogs/Roadmap.md:18`, issue #48, PR #1155). Backups go to S3 through rclone from snapshots (README:468). The code is partly ready for high-latency filesystems: the parallel-reader knob (`consts.go:3-6`), parallel close (`part.go:176-200`) and delete-marker removal without renames (`dir_remover.go:32-37`). The data path assumes POSIX files, directory fsync and hard links.

## 10. Concurrency

- **Locks:**
  - `Storage.partitionsLock` protects the partition list and the hot partition;
  - `datadb.partsLock` protects the part lists and `isInMerge`, and `parts.json` is rewritten while holding it;
  - each rows-buffer shard has its own mutex.
- **Lifetimes:** partitions and parts are refcounted (`partitionWrapper`, `partWrapper`). Readers take references under the lock and release them when done.
- **Writers:** any number of goroutines ingest concurrently. There is no single writer per partition. Merges are concurrent on disjoint part sets.
- **Detaching a partition** waits until no query is using it (`storage.go:271-308`).

## 11. Failure handling and testing

- **Disk full:** `-storage.minFreeDiskSpaceBytes` (default **10 MB**) puts the store in read-only mode, and ingest returns 429 (`storage.go:1335-1338`, `app/vlstorage/main.go:555-569`). Merges reserve space (§5). Disk-usage retention exists too, but its docs warn that it can't prevent reaching 100% on small disks at high ingest rates (README:206-215).
- **Corruption:** there are **no checksums** in the format. Corrupt headers or metadata raise `logger.Panicf("FATAL…")`, so the process stops instead of quarantining the part (`block_search.go:291-393`, `part_header.go:62-93`).
- **Tests:**
  - unit tests for the part lifecycle, concurrent `MustAddRows`, delete tasks, partition detach/recreate and dropping stale partitions (`storage_test.go`, `partition_test.go`, `datadb_test.go`);
  - a merge-selection test, `TestAppendPartsToMergeManyParts`;
  - **I found no crash or fault-injection tests.** `grep -i "SIGKILL|crash"` only matches comments;
  - `apptest/` contains end-to-end tests of the HTTP API;
  - `benchmarks/gharchive/` is a comparative ingest/query benchmark.
- A fsync-latency panel was added to the dashboards in the tip (CHANGELOG:39).

## 12. Traces

This repo has no trace-specific storage. VictoriaTraces is a separate repo built on `lib/logstorage`, and it isn't checked out here. OTLP logs keep `trace_id` and `span_id` as ordinary hex fields (`app/vlinsert/opentelemetry/pb.go:313-325`). The docs tell users never to make `trace_id` a stream field, because it would create too many streams (high cardinality), and to query it as a plain field with an exact match (`docs/victorialogs/keyConcepts.md:273-303`). A trace_id lookup is therefore a scan that relies on per-block bloom filters, with no dedicated index and no handling of late spans.

## 13. Lessons from the CHANGELOG (git log was unavailable)

| Version / line | Fix | Lesson |
|---|---|---|
| v1.26.0 (CHANGELOG:703, #505) | "properly persist newly created data on unclean shutdown" | They missed file and dir fsyncs on part creation the first time. **Every new file *and* its parent dir must be fsynced before the catalog references it.** |
| v1.23.0 (:809, #8873) | self-healing after an OOM during daily partition creation | Creating a multi-file directory isn't atomic. Startup must repair a half-created state (`partition.go:83-109`, `datadb.go:1299-1335`). |
| v1.50.0 (:195, #657) | queries missed data after detach → remove → recreate of the same partition | Caches keyed by partition name must be invalidated by a generation number. |
| v1.50.0 (:194, #996) | panic on `.DS_Store` | Startup scans must ignore foreign files. |
| v1.49.0 (:227, #1213) | delete tasks evaluated relative time at execution time | A delete needs a cut-off fixed when it is created (we have this). |
| v1.39.0 (:436, #825) | panic in `block_stream_merger.go` during deletion | Merge-with-filter is where invariants break, e.g. a block becoming empty. Test it heavily. |
| code comment `datadb.go:1032` (#1020) | deadlock around `partsLock` in the swap | Keep the catalog-swap critical section minimal; never call back into mergers while holding the lock. |
| v0.5.0 (:1668) | "assisted merge" detection was broken → too many parts under high ingest | When merges fall behind, part counts explode. The current design has no hard cap. |
| v0.4.0 (:1698, #397) | panic when a merged block exceeded the column limit | Limits enforced at ingest must also be enforced when merging. |
| v0.4.0 (:1682, :1694) | read-only mode, then a false read-only bug | The free-space check is cached for 2 s (`fs.go:296-311`); be careful with thresholds. |
| v1.11.0 / v1.17.0 (:957, :890) | format changes that can't be downgraded, for per-field locality | Column locality in wide events dominated query cost. |

## 14. Recommendations for llmobs spec 03

**Adopt (as ideas):**

- **§4 Catalog.**
  - Commit only after durability: a new file must be `fsync`ed, **and its parent directory too**, before the SQLite row that names it is committed. That is VictoriaLogs bug #505.
  - **Risk in our stack:** `object_store` **0.13.2** (pinned by DataFusion 55, root `Cargo.toml:42-70`) `LocalFileSystem` never calls `sync_all` (`~/.cargo/registry/.../object_store-0.13.2/src/local.rs`, no fsync at all). Opt-in fsync only arrives in 0.14 (`local.rs:308-313`). For the local backend we must either write Parquet files through our own `std::fs` path with `sync_all` and a directory fsync, or fsync after the `put`.
  - Otherwise this sequence loses data on power loss: flush commits `flushed_seq`, WAL segments are deleted, the file was never durable, the data is gone.
- **§4 + spec 04.** `meta.db` uses `synchronous=NORMAL` (spec 04:3). In WAL mode that can lose the latest commits on power loss. That's fine for metadata, but **not for the transaction that advances `flushed_seq`**, because WAL segments are deleted after it.
  - Either run catalog commits with `synchronous=FULL` (a per-connection pragma on the writer), or delete WAL segments only after a later checkpoint.
  - Pick one and say so in the spec.
- **§4/§10 Startup cleanup.** At startup, with local storage and a single process (they use `flock`; we should too), every file under `objects/tables/` that the catalog doesn't know can be deleted **right away**. The 1 h age guard is only needed for the periodic GC while running, and for S3.
- **§5 Deleting obsolete files.** VictoriaLogs uses refcounts: the last reader deletes the file. Our `gc.grace = 10 min` is a time-based guess that a long query (60 s SQL limit, 2 GiB spill) can still beat. Prefer refcounting through the snapshot: the `Arc<FileMeta>` in the snapshot is dropped, then the file is deleted. Keep the grace period only for S3, or for a future multi-node querier.
- **§5 Compaction policy.** Take their write-amplification rule: merge 2–15 size-similar files only when output ≥ ~7.5 × the largest input (`datadb.go:1366-1424`). It is simple and bounds rewrites. Our "≥ 8 L0 or ≥ 512 MiB or 10 min" trigger has no write-amplification bound for L1 merges ("any L1 smaller than half target").
- **§5/§10 Disk reservation.** Compaction is listed as "keeps running (it reclaims space)", but it needs about the input size in **extra** space until the commit. Add a global reserved-bytes counter: skip a compaction whose inputs don't fit; never skip a memtable flush. Also cap the output size by free disk.
- **§8 Retention.** Remember the days that were dropped, and reject or drop late spans that would recreate them (`storage.go:1252-1301`). Add a max backfill age and a max future skew at ingest, with metrics. Disk-usage retention ("drop the oldest days, keep the last 2") is a useful safety valve for a single-box product; consider adding it.
- **§2 Memtable.** Derive the defaults from cgroup-aware memory (`memory.Allowed()` is 60% of the cgroup limit) instead of fixed 256 MiB / 1 GiB values. Also keep a CPU semaphore around batch→Arrow encoding and flush, so ingest slows down naturally.
- **§3 Pruning.** Their per-block min/max plus a bloom filter per column is the equivalent of our page index plus bloom filters. Our list of bloom columns (`trace_id`, `session_id`, `user_id`) is the right minimum. Also consider bloom filters on `span name` and `model`.
- **§6 Query path.** They have no in-process data cache and rely on the OS page cache. They write big merges with `fadvise(DONTNEED)` so compaction doesn't evict hot pages. Our 512 MiB metadata cache is fine. For Parquet data pages, rely on the page cache, and consider `posix_fadvise` on compaction reads and writes when the store is local.

**Avoid:**

- Acknowledging writes when they are only in RAM. That is fine for logs with a disk-buffered agent in front, but OTLP SDKs treat a 2xx as delivered, and our spec 02:14 promises "200 = fsynced WAL".
- Panicking on corruption. Our quarantine (§10) is better. Consider turning on Parquet page CRCs, because they have no checksums at all.
- Deletes that stay visible until the rewrite finishes. Our tombstone filter is better for GDPR. But copy their *merge-with-drop-filter* approach for the rewriter, and their rule for files already in a compaction: requeue, or let the compaction apply the tombstones (§7 already says tombstones apply during merges, which is good).

**Is our WAL + fsync worth it?** Yes, given *our* design. Not because VictoriaLogs proves it necessary.

- VictoriaLogs avoids a WAL because its in-memory parts reach disk every ~5 s as cheap local directories, and merges absorb the small files. The cost is a 5–6 s window of acknowledged data that can be lost.
- Our memtable stays in memory up to 120 s, and our targets are Parquet and S3, where a tiny file every 5 s per `(project, day)` would be bad: PUT cost, footers, file count. So the WAL is what allows big L0 files.
- With group commit, fsync latency isn't the real cost. The real costs are:
  - **bandwidth.** The WAL stores *uncompressed* Arrow IPC (~150 MiB/s at 50k spans/s), 5–7× the compressed Parquet output (spec capacity check). Enable Arrow IPC body compression (LZ4/ZSTD) in WAL records, or the WAL will dominate disk writes and SSD wear;
  - **complexity** (replay, torn tail, checkpoint).
- If we ever run local-only, a VictoriaLogs-style alternative is possible: no WAL, flush small L0 files every 2–5 s, and rely on our existing `(trace_id, span_id)` dedup to make client retries idempotent. Record it as a rejected alternative with these numbers.

**Possibly missing from spec 03:**

1. Per-`(project, day)` files plus a flush every 120 s, multiplied by many small projects, means many tiny L0 files. VictoriaLogs avoids this by making the tenant a *sort prefix* inside shared parts. Our per-project partitioning is still right for per-project retention and project deletion, both of which VictoriaLogs lacks. But compaction needs a minimum-size or age rule for small projects, and the flush could skip writing files below N rows until `max_age`.
2. Exclusive claims on input files, shared between compaction and the deletion rewriter (their `isInMerge` flag). The commit must check that its inputs are still active.
3. A `flock` on `data_dir`.
4. fsync of the `wal/` directory when a segment is created or rotated.
5. Startup repair of half-created state.
6. Crash and fault-injection tests, which VictoriaLogs has none of. Our AGENTS.md already requires crash tests for storage; they should cover file written → not fsynced → catalog committed.
