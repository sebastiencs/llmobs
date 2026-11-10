# OpenObserve storage engine: research notes for llmobs spec 03

Repo: `/home/sebastien/travaux/openobserve`, local HEAD `8dcaf5e8` (commit date 2026-09-20, read through the GitHub API, because this session could not run `git` on a repo outside its worktree). The commit history in §13 comes from `gh search commits` and `gh pr view` on `openobserve/openobserve`. That history runs up to 2026-11-10, so it includes some PRs newer than the local checkout. Those PRs are marked "(post-HEAD)". Paths below are relative to `src/`.

Short version: OpenObserve has a **two-stage local pipeline**. The first stage is WAL plus memtable, persisted to local Parquet. The second is a separate "mover" job that merges the local Parquet files, uploads them, and then registers them in `file_list`. The two stages are coordinated with file renames, lock files, and in-memory sets, not with a sequence-number checkpoint. Durability is weak by default: there is no fsync, and the write is acknowledged before the WAL buffer is flushed. Most of the bugs they fixed recently come from this loose coordination. The traces part is the most transferable: OpenObserve keeps a **trace time index** of per-trace partial rows that are merged by `GROUP BY trace_id` during compaction. That is the same idea as our `traces` partials.

---

## 1. Write path

- **Buffering before the WAL.** The ingest handler converts each request to Arrow `RecordBatch`es per (stream, schema, hour partition) (`core/src/ingestion/mod.rs:520-575`). `Writer::write_batch` does the CPU work first: Arrow conversion and IPC serialization (`ingester/src/writer.rs:376-478`). An optional bounded per-writer queue exists (`ZO_WAL_WRITE_QUEUE_ENABLED`, default **false**; `config/src/config.rs:1966-1977`). When the queue is on, the call returns once the batch is **enqueued**, so the ack comes before the WAL write. When it is off (the default), `consume_processed` runs inline.
- **Writer sharding.** There is one `Writer` per (bucket, org, stream_type). The bucket count is derived from the memtable budget. Each writer owns exactly **one WAL file and one memtable** (`ingester/src/writer.rs:43-52, 128-148`).
- **Segment file format** (`wal/src/lib.rs:26-37`, `wal/src/writer.rs:38-104`):
  - The file starts with a 13-byte magic (`OPENOBSERVEV3`), then a u32 header length, then a key/value header.
  - The file is **preallocated** with `set_len(max_file_size_on_disk)` (512 MiB).
- **Record framing** (`wal/src/writer.rs:116-169`):
  - The record header is a u32 BE CRC32 followed by a u32 BE compressed length.
  - The payload is snappy frame-compressed.
  - The CRC covers the **compressed** bytes. The writer computes it while streaming into the compressor.
  - Since PR #13171, the payload is an Arrow IPC stream with a `0xFFFF` marker. The old format was JSON.
  - The file name is the writer's creation time in µs (`writer.rs:357-375`).
- **Reading.** A checksum of 0 means EOF: that is how the reader finds the end of the preallocated, zero-filled tail (`wal/src/reader.rs:147-160`). A real record whose CRC happens to be 0 would be read as EOF, roughly once every 4 billion records.
- **Rotation** (`writer.rs:527-605, 667-704`) happens when any of these is true:
  - WAL bytes plus the new entry exceed 512 MiB;
  - memtable JSON or Arrow bytes exceed `max_file_size_in_memory` (512 MiB);
  - the file is older than `max_file_retention_time` (600 s; a `check_ttl` loop also sends `Rotate` every 600 s, `ingester/src/lib.rs:146-158`).

  On rotation the writer creates a new WAL, syncs the old one, swaps the memtable, and inserts the old memtable into the global `IMMUTABLES` map, keyed by the old WAL path.
- **fsync policy.** `Writer::sync` does a `BufWriter` flush plus `sync_data`, unless `ZO_WAL_FSYNC_DISABLED`, which **defaults to true** (`config/src/config.rs:1964`, `wal/src/writer.rs:171-185`). Logs and traces pass `fsync = !wal_fsync_disabled`. Metrics always pass `false` (`core/src/metrics/ingest.rs:348`). When fsync is on, it runs **per request** under the WAL write lock, with no group commit (`writer.rs:516-521`).
- **Ack point.** In the default configuration, the HTTP/gRPC call returns after `wal.write()` into a 16 KiB `BufWriter` and the memtable insert. The data is not flushed to the OS and not fsynced. A process crash can lose the unflushed buffer of each writer. A machine crash can lose everything in the page cache.
- **Recovery** (`ingester/src/lib.rs:108-140`, `ingester/src/wal.rs:142-260`):
  - First, a **lock-file** step runs synchronously; see §2 below.
  - WAL replay then runs **in a background task, after the servers are already accepting traffic**.
  - Each leftover `.wal` file is replayed into a fresh memtable and persisted straight to local Parquet; it does not go back into the live memtable.
  - A CRC mismatch, a length mismatch, or an undecodable entry anywhere in the file is **logged and skipped**. Replay continues with the next frame, and torn tails are not told apart from corruption in the middle of a file.
  - The header parser `unwrap`s on malformed bytes (`wal/src/reader.rs:101-130`), so a corrupt header panics.
- **WAL truncation.** There is no sequence number and no checkpoint. A WAL file is deleted **as a whole** once its memtable has been persisted to local Parquet (`ingester/src/immutable.rs:97-152`).

## 2. Memtable and the immutable stage

- **Structure.** The memtable is a `HashMap<org/stream, Stream>`. A `Stream` is a `BTreeMap<schema_key, Partition>`, and a `Partition` is a `BTreeMap<hour_key, Vec<Arc<RecordBatchEntry>>>` (`ingester/src/memtable.rs:36-80`, `stream.rs`, `partition.rs:37-120`). The structure is append-only, holds one batch per request, and keeps a union schema per partition. Each entry carries min_ts and max_ts, so readers prune whole batches by time (`partition.rs:330-343`).
- **Memtable ids.** Every memtable has a monotonically increasing id that starts at the current time in µs. The id is embedded in the persisted file names. Readers use it to deduplicate overlapping views (§6).
- **Flush and persist.**
  - A loop runs every `mem_persist_interval` (2 s) and hands each immutable to `mem_dump_thread_num` workers (`lib.rs:160-199`, `immutable.rs:255-275`).
  - Persist writes one Parquet file per (stream, schema, hour, ≤100k-row chunk) into `wal/files/...` as `.par` (`partition.rs:180-300`).
  - The workers then follow a lock-file protocol (`immutable.rs:97-152`, documented in `wal.rs:36-55`): (1) write the `.par` files; (2) write `<wal>.lock`, listing them; (3) delete the `.wal`; (4) rename `.par` to `.parquet`; (5) delete the `.lock`.
  - At startup, a leftover `.lock` means steps 3 to 5 are completed. A `.par` without a lock is an orphan and gets deleted.
  - **Neither the `.par` files nor the directory are fsynced** before the WAL is deleted (`partition.rs:258-268`).
- **Backpressure.** The ingest path calls three checks (`core/src/ingestion/mod.rs:616-622`, `ingester/src/writer.rs:54-115`). Each failure returns `ResourceError`.
  - `check_memtable_size`: total Arrow bytes in memtables at or above `mem_table_max_size`, which defaults to 25% or 50% of RAM (`config.rs:4155-4162`).
  - A memory circuit breaker on node RSS.
  - A disk circuit breaker, with a threshold as a percentage or in MB.

  The memtable accounting uses Arrow bytes. A bug caused the rotation threshold to look only at WAL (JSON) bytes (PR #13421).
- **Queries over unflushed data.** Readers go through `read_from_memtable` and then `read_from_immutable`, with the immutable read skipping memtable ids already returned. They then scan the local `wal/files/**.parquet` tree from disk (§6).

## 3. Files

- **Writer settings** (`config/utils/parquet.rs:77-133`, `config.rs:110-111`):
  - Rows per row group are capped at 128 Ki (`PARQUET_MAX_ROW_GROUP_SIZE`). Ingester files are chunked at 100k rows.
  - Compression is configurable, default zstd at the crate's default level.
  - `_timestamp` uses `DELTA_BINARY_PACKED` encoding with dictionary encoding disabled.
  - Page size and the statistics level are left at the parquet-rs defaults, so page-level stats and the page index are on.
  - Bloom filters are written only for the stream's configured fields, with fpp at the default and NDV set to max(1000, rows/100).
  - The file's KV metadata holds `min_ts`, `max_ts`, `records` and `original_size`.
  - A Vortex format path exists next to Parquet.
- **Sort order.** Files are written `ORDER BY _timestamp DESC` (`search/src/datafusion/merge/mode.rs:43-46, 181`). The query side declares this order to DataFusion (`exec.rs:744-754`) and sets `split_file_groups_by_statistics` (`exec.rs:107-113`). Metrics have their own hash-sorted layouts.
- **Target sizes.** The ingester upload merges local files up to min(`max_file_size_on_disk`, `compact.max_file_size`) = 512 MiB of original size (`jobs/src/job/files/parquet.rs:672-690`). The compactor target is `ZO_COMPACT_MAX_FILE_SIZE`, 2048 MiB by default (`config.rs:2902`).
- **Partitioning and naming.** Objects are stored at `files/{org}/{stream_type}/{stream}/{YYYY}/{MM}/{DD}/{HH}/[{partition_key}={value}/]{id}.parquet`, partitioned **hourly** (daily is optional), with optional user partition keys (`config/utils/parquet.rs:154-170`). Local WAL Parquet files add the writer index and the schema key into the path (`ingester/src/lib.rs:202-212`).
- **Inverted index.** A tantivy index (`.ttv`) is built **per Parquet file** at upload time for full-text and index fields (`jobs/src/job/files/parquet.rs:860-900`).
- **Bloom layer.** A separate bloom layer has a transposed per-hour `.bf` file; see §6.

## 4. Catalog (`file_list`)

- **Schema** (`infra/src/file_list/sqlite.rs:1740-1846`). The tables are:
  - `file_list(id, account, org, stream, date "YYYY/MM/DD/HH", file, deleted, flattened, min_ts, max_ts, records, original_size, compressed_size, index_size, bloom_ver, updated_at)`;
  - `file_list_history`;
  - `file_list_deleted(org, stream, date, file, index_file, flattened, created_at)`, the pending physical deletions;
  - `file_list_jobs`, the compaction job queue: one row per (stream, hour offset), with node, status and `dumped`;
  - `stream_stats`, and the dump tables.

  Indexes include `(stream, max_ts, min_ts)` and `(stream, date)` (`sqlite.rs:1955-1990`). The same schema exists on Postgres.
- **Range queries.** Files are found with `WHERE stream = ? AND max_ts >= start AND max_ts <= end + partition_duration AND min_ts <= end` (`sqlite.rs:390-432`, `file_list/mod.rs:742-756`). The upper bound on `max_ts` turns the overlap test into a bounded range scan on the index. That trick is worth reusing.
- **Atomic commits.** `inner_batch_process` inserts the new files and deletes the old file rows **in one transaction** (`sqlite.rs:1600-1730`). However, `write_file_list` in the compactor commits that transaction and then inserts into `file_list_deleted` in a **second transaction** (`compaction/src/merge/stream.rs:323-389`, `retention.rs:612-680`). A crash between the two leaks the input objects: they are no longer in the catalog and are never deleted.
- **Readers.** Each query runs a plain SQL query against `file_list`. There is no snapshot object. Consistency comes from the **delayed physical delete**: `run_delay_deletion` removes objects only after `ZO_COMPACT_DELETE_FILES_DELAY_MINUTES`, default **120 min** (`compaction/src/lib.rs:401-445`, `deleted.rs`, `config.rs:2923, 4417-4419`).
- **Orphans.** There is no automatic orphan scan. A `gc-file-list` CLI cleans stale objects by hand (commit 31ddecd23). An upload whose registration then fails leaves an orphan object (PR #15968 discusses this).

## 5. Compaction

There are two levels, and both re-merge time-sorted files.

1. **Ingester mover** (`jobs/src/job/files/parquet.rs`).
   - Every `file_push_interval` (2 s), it walks the entire `wal/files` tree.
   - It groups files by stream and hour prefix.
   - It waits until a group reaches 512 MiB or its oldest file is older than `max_file_retention_time`.
   - It merges the files with DataFusion (`SELECT * ... ORDER BY _timestamp DESC`), uploads with `storage::put`, builds the tantivy index, and calls `file_list::set`. Only then does it delete the local files (`parquet.rs:359-645`).
   - A crash after `file_list::set` and before the local delete leaves the local `.parquet` on disk. The "removing" marker lives only in memory (`db/src/file_list/local.rs:72-82`), so the file is **re-uploaded under a new name and produces duplicate rows**.
   - Files held by a running query go to a persisted pending-delete list.
2. **Compactor** (`compaction/src/merge/*`).
   - Jobs are per (stream, hour) rows in `file_list_jobs`, assigned to nodes with consistent hashing.
   - The planner groups files below the size target into batches, up to `compact.max_file_size` (`merge/plan.rs`, `size_bounded_groups`).
   - **Incremental mode** applies to the current, still-open hour, triggered when the ingester has uploaded 3× the needed file count. It seals only full groups and carries the remainder forward to avoid rewriting the same data twice (`compaction/src/incremental.rs`, `plan.rs:150-175`).
   - The final hour merge waits for a **settle window** of 3 × `max_file_retention_time` (30 min) after the hour **ends** (PR #13880).
   - The commit is the same add-plus-delete transaction as in §4, followed by `file_list_deleted` and the delayed object delete.
   - Spans are **not deduplicated** during the merge.
   - Write amplification is roughly 3×: local Parquet, then the uploaded merged file, then the compacted file. Also, each local `.par` file is written once and then read back for the merge.

## 6. Query path

- **DataFusion integration.**
  - The table provider is a `ListingTableAdapter` that wraps DataFusion's `ListingTable` (`search/src/datafusion/table_provider/listing_adapter.rs:92-215`).
  - Files are not listed from storage. The planner registers the query's resolved file list in a global map keyed by query id and schema key (`storage/file_list.rs:46-80`). Custom object stores `memory:///` (object storage through the caches) and `wal:///` (local WAL Parquet) serve those paths (`exec.rs:155-170`, `exec.rs:770-790`).
  - A wrapper adds a tantivy-index filter, a timestamp filter and regrouping by statistics on top of the Parquet scan.
- **Session settings** (`exec.rs:70-150`):
  - `collect_statistics = true`;
  - `pushdown_filters` is configurable;
  - `split_file_groups_by_statistics` is on;
  - several DataFusion 54/55 features are disabled because they break distributed plans;
  - `enable_migration_aggregate = false`, with a comment noting a performance regression in DataFusion 55.
- **Memory pool.** A Greedy or Fair pool, at least 256 MiB, wrapped in `TrackConsumersPool` (`exec.rs:172-210`).
- **Pruning.**
  - Time range through the `file_list` SQL.
  - Partition keys.
  - Parquet stats and page index, through DataFusion.
  - The tantivy `.ttv` index per file.
  - A **transposed bloom layer** (`search/src/bloom_pruner.rs` header, `compaction/src/bloom/`, PR #12121). For each hour bucket, the compactor writes one `.bf` file in which the 32-byte SBBF block of *every* file for a given hash is stored contiguously. A `trace_id = X` lookup costs **one range read per hour group**, whatever the number of files, followed by a point check per file.
- **Merging memtable and files** (`search_service/src/grpc/flight.rs:330-430`). The order is:
  1. object-storage files (listed earlier by the leader);
  2. the memtable, then the immutables, deduplicated by memtable id;
  3. local WAL Parquet, skipping files whose memtable id was already read, and skipping pending-delete or removing files (`grpc/wal.rs:107-418, 624-720`).

  The local WAL Parquet set is found by **walking the directory on every query**. Files being read are refcounted ("lock_files") so the mover doesn't delete them. These steps are not atomic: a file moving from WAL to storage while a query runs can be missed or counted twice (PR #16642, "post-HEAD"). They fixed it only for PromQL, by reading the WAL before listing storage and deduplicating samples. For SQL the PR says "Rows have no key to deduplicate on".
- **Caches.** There are three:
  - a disk cache of object files, on by default, and a memory cache, off by default, with LRU, FIFO or time-LRU policies (`infra/src/cache/file_data/mod.rs`);
  - DataFusion's file-statistics cache (`storage/file_statistics_cache.rs`);
  - a result cache.
- **Resource limits.** Memory pool per query, `target_partitions`, enterprise work groups, the memory circuit breaker before scanning the WAL, and an "admission slot" for searches.

## 7. Deletes and retention

- **Row-level deletes by predicate: not found** in the OSS tree. You can delete whole streams or a time range (`/{org}/streams/{name}/data_by_time_range`, `api/management/src/request/stream/mod.rs:384-700`). There are no tombstones and nothing like a GDPR delete by user or trace.
- **Retention** (`compaction/src/retention.rs:256-680`). Jobs are generated per stream, per day range (minimum retention 3 days). Deletion marks the `file_list` rows deleted and adds them to `file_list_deleted`, after which objects go after the delay.
  - **On local disk, retention deletes the partition directories first, with `remove_dir_all`, and updates the catalog afterwards** (`retention.rs:491-503`). Running queries can then hit missing files, and the 120-min grace does not apply.
  - The mover also drops files older than retention at upload time (`parquet.rs:426-455`).
  - Trace ingest rejects spans older than retention (`core/src/traces/mod.rs:784-790`).
  - Compaction skips hours that retention will delete (`compaction/src/lib.rs` `run_merge`).

## 8. Tenancy

- The org is the top path component everywhere: WAL, local files, objects, and the `file_list.org` / `stream` key `"org/type/name"`.
- Memtable writers are keyed by org. An optional **shared memtable** (`feature_shared_memtable_enabled`) hashes many small orgs into shared buckets, to bound file counts (`writer.rs:117-135`).
- Queries resolve the stream inside the org given in the request path.
- Per-org blocklists exist for compaction and ingest.

## 9. Local disk vs object storage

- Storage goes through `object_store`. The `Local` backend wraps `LocalFileSystem` behind a `LimitStore` (`infra/src/storage/local.rs`). Remote backends are S3, GCS and Azure, with multiple accounts (`account` column).
- In local mode, the "upload" is a `put` into `data_stream_dir`.
- The WAL and the WAL Parquet files always stay on local disk.
- When `cache_latest_files` is enabled, newly uploaded files are also written into the disk cache at upload time (`parquet.rs:846-851`).

## 10. Concurrency

- Tokio throughout, with an optional dedicated `WAL_RUNTIME`.
- Global maps use tokio `RwLock`s: `WRITERS` per bucket, `IMMUTABLES`, `PROCESSING_TABLES`, `PROCESSING_FILES`, `WAL_PARQUET_METADATA`.
- Lock order inside a writer is WAL, then memtable.
- Persist, mover and merge pools have configurable thread counts.
- The compactor coordinates through DB job rows plus consistent hashing.
- Two rotation races were fixed: #5881, and #16645 (post-HEAD), described in §13.

## 11. Failure handling and tests

- Recovery uses the lock-file protocol and a background orphan `.par` sweep that skips files newer than process start.
- DB health is checked before each mover cycle, to avoid uploading objects that cannot be registered (`parquet.rs:140-147`).
- Registration retries 5 times (PR #15968).
- Tests:
  - WAL tests are unit-level round trips (`wal/tests/wal.rs`), plus a write benchmark (`wal/benches/write.rs`).
  - Newer PRs add targeted race tests: replay versus a live writer (#14971), and rotation visibility (#16645).
  - **No crash or fault-injection harness was found.** No kill-9 test exists in the repo; PR #13171 only describes a manual one.

## 12. Traces

- **Span schema.** The `Span` struct (`common/src/meta/traces.rs:22-45`) has `trace_id` and `span_id` as hex strings, flags, status, kind, operation_name, `start_time`, `end_time`, `duration`, references flattened to `reference_parent_span_id` and similar, service_name, and **attributes flattened into top-level dynamic columns**. Events and links are JSON strings. `_timestamp = start_time / 1000` (µs, `core/src/traces/mod.rs:784`). The schema grows by adding columns, with a "user-defined schema" option for streams with many fields.
- **Trace list.** It is computed **at query time** with SQL over raw spans: `SELECT trace_id, min(_timestamp), min(start_time), max(end_time), count(*), error count, count(DISTINCT service), root service and operation via CASE on empty parent, ... GROUP BY trace_id ORDER BY ... LIMIT` over the UI time range (`api/search/src/traces/mod.rs:~330-470`). Nothing is materialized. Results are cached in the result cache. A separate, unused write-only `trace_list_index` stream was **removed** as pure write amplification (PR #13809).
- **Trace detail.** It uses a time hint:
  - If the trace id is a UUIDv7, its embedded timestamp narrows the window to −60 s / +1 h (`config/src/ider.rs:98-130`, `traces/mod.rs:262-292`).
  - Otherwise it uses the **trace time index** (`core/src/traces/time_index.rs`). This is a metadata stream with rows `(_timestamp, trace_id, session_id, min_ts, max_ts)`, built at ingest per (trace, hour bucket). A trace spanning an hour boundary gets a row in both its start hour and its end hour (`time_index.rs:72-155`).
  - The index stream has bloom and inverted indexes on `trace_id` and `session_id`.
  - Compaction merges its partials with `SELECT MIN(_timestamp), trace_id, MAX(session_id), MIN(min_ts), MAX(max_ts) ... GROUP BY trace_id` (`search/src/datafusion/merge/mode.rs:47, 186-193`), which is the same as our partial merge.
  - Lookup runs a **windowed backward scan**: one shared scan per stream for a batch of ids. A key is complete once the covered region extends one expand window (24 h for traces, 7 d for sessions) past its leftmost hit (`api/search/src/traces/time_index/scan.rs` header, `time_index/mod.rs:40-75`).
- **Service graph.** A periodic daemon (enterprise) runs window SQL over the trace streams after a cache delay and writes the results as metrics streams. There are caps on edges and nodes, and no backfill beyond 24 h (`core/src/traces/service_graph/v4/mod.rs`).
- **Late or split spans.** Nothing special: every span is a row. Since the trace list is a `GROUP BY` at query time, late spans simply join their trace. A trace whose spans span the window edge is truncated to the window.

## 13. Lessons from history (commit / PR)

- **d07779cfc (#14971)**: *do not replay a live writer's WAL on startup.* Replay ran in the background after the servers were up. It listed and then deleted WAL files that live writers still had open, and later records were discarded as "duplicate persist". **That lost acknowledged data.** Lesson: finish recovery before accepting writes, or key recovery on an explicit boundary.
- **87bb2b690 (#16645, post-HEAD)**: rotation swapped the memtable and released the lock *before* publishing the old memtable to `IMMUTABLES`. A query in that gap saw neither and returned a short result. Lesson: freezing a memtable must be atomic with respect to readers. Their test holds a reader guard and asserts the invariant without sleeps.
- **ecadb0d01 (#16642, post-HEAD)**: queries read storage listing and WAL in an order that, raced against the mover's "register then delete", produced misses or doubles. Lesson: a reader needs one consistent snapshot of the in-memory data plus the catalog.
- **596386c5f (#13421)**: the two copies of the rotation threshold check drifted apart, so the memory-based rotation never fired. With one row per batch, Arrow size reached 13× the JSON size and ingestion ended with OOM rejections. Lessons: one predicate, and account by Arrow memory; many tiny batches are very costly.
- **2287444a1 (#15968)**: `file_list` registration errors were swallowed (`Ok(0)`). The WAL or local files were then deleted, and the data became **invisible: silent data loss**. Postgres `ON CONFLICT ... RETURNING` plus `fetch_one` also made the retries non-idempotent. Lesson: never drop the source before the catalog commit is confirmed, and make registration idempotent.
- **c57272786 (#13880)**: a refactor turned the settle-window check into dead code. Stragglers for a closed hour stayed small files forever. Lesson: compaction of a "closed" time partition must handle late files.
- **429e5148f (#13953)**: query file locks were released by control flow. Cancelled queries leaked them, and the disk filled with WAL files that could never be deleted. Lesson: tie read leases to RAII guards.
- **99a7485f6 (#5881), 538e98e76 (#7797)**: earlier rotation and persist concurrency bugs, including removal from the processing set too early.
- **e3288fae1 (#5021), 97516f880 (#6027)**: WAL replay into memory caused OOM, so replay was changed to persist each WAL file directly to Parquet.
- **8ed1f3b79 (#13171)**: switching the WAL payload from JSON to Arrow IPC gave 12× faster serialization, 61% smaller entries, 37× faster decode, and 5× faster replay.
- **94bddd7a7 (#13497)**: one file per stream × hour per rotation produced thousands of tiny files per second. A periodic directory walk couldn't keep up. They fixed it with "pack" files and an in-memory segment index.
- **3d0085c43 (#13809)**: a write-only index stream was deleted.
- **f208d3a8c (#681)**: an early "corrupted WAL file" fix.

## 14. Recommendations for llmobs spec 03

**Ideas to adopt**
- **§6 TracesProvider: confirmed.** OpenObserve's trace time index is our `traces` partial table: per-flush `GROUP BY trace_id` with MIN/MAX, merged again by compaction. Consider their **two-bucket rule**: also emit a partial into the partition of the trace's *end* when it crosses the boundary. A per-trace min/max time index then bounds detail lookups without `trace_lookback` guesswork.
- **§6 single-trace lookup option 2**: consider a **transposed per-partition bloom file**. Stores the trace_id SBBF blocks of all files in a (project, day) partition contiguously, so one read tests every file. It scales better than per-file footer blooms in the metadata cache when files are on S3 or not cached. It could replace the post-MVP `trace_index`.
- **§4 catalog query**: the bounded overlap trick (`max_ts BETWEEN a AND b + max_file_span`) lets an index serve a time-overlap query. We keep an in-memory catalog, so this only matters for the SQLite fallback.
- **§5**: incremental compaction that **carries the unfilled remainder** forward instead of sealing it; and an explicit **settle window** before final merges.
- **§1 WAL payload as Arrow IPC**: the spec already does this; OpenObserve's numbers confirm it.
- **§10**: a memory circuit breaker on process RSS in addition to `memtable.hard_limit`.

**Patterns to avoid**
- Acknowledging writes before fsync (their default), per-request fsync without group commit, and preallocated segments that use CRC 0 as the EOF marker. Our group-commit design is better. Keep it.
- Separating "persist locally" from "upload and register" with no persisted marker. That causes duplicate rows after a crash. Our single SQLite transaction (new `data_files` rows plus `flushed_seq`) is right.
- Splitting "catalog update" and "pending physical delete" into two transactions. Our `obsolete` state in the same row is better.
- Walking directories on every query, and one file per stream × hour per flush.
- Retention that deletes local directories before the catalog commit, bypassing grace.
- Treating CRC errors in the middle of a WAL file as skippable. Our spec refuses to start, which is correct.
- `unwrap` in the header parser, and `expect` when creating a WAL.

**Spec 03 gaps this research suggests**
1. **§1 / §2 durability of flushed files on the local backend.** `object_store::LocalFileSystem` writes a temp file and renames it, but (to our knowledge) does not fsync the file or its directory. Before the SQLite commit that advances `flushed_seq` and deletes WAL segments, the spec should require fsync of the Parquet file **and** its parent directory when `object_store = local`. It should also fsync the WAL directory on segment creation and rotation. OpenObserve skips this.
2. **§1 Recovery ordering.** State explicitly that WAL replay finishes **before** the ingest listener opens, and that replay never touches the active segment (OpenObserve #14971).
3. **§2 Atomic freeze.** The spec's step 1 is right. Add the invariant that a query snapshot is taken under the same lock as `active`/`frozen`/catalog, and add a deterministic test like the one in #16645. Also add an invariant test for steps 3–4 versus a concurrent query (#16642).
4. **§2 Batch fragmentation and accounting.** The memtable appends one batch per request. Count bytes with `get_array_memory_size`, not payload bytes. Consider `concat_batches` per partition once N small batches have accumulated: OpenObserve saw 13× overhead with one-row batches.
5. **§5 Late files after day close.** Late spans create new L0 files in an already-closed day. The spec should say that L0 merge and day close are re-triggered for a closed day when new L0 files appear. Otherwise `fully_merged` and `partial_count = 1` stop being true, and stragglers stay small (OpenObserve #13880).
6. **§5 GC grace.** OpenObserve uses 120 min. Our `gc.grace` of 10 min is fine only if every reader of an old snapshot, including exports and MCP, is bounded below it. Tie it to the maximum query timeout and document it, or hold read leases with RAII guards (#13953).
7. **§8 / ingest.** Reject or drop spans whose `start_time` is older than the project's retention, both at ingest and at flush. Otherwise late data recreates partitions that the next retention run deletes again. OpenObserve does both.
8. **§4 Registration idempotency.** On a retry after a lost SQLite response, inserting `data_files` must be idempotent (key on the file id or path).
