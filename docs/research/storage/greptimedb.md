# GreptimeDB storage engine (mito2) — research notes for llmobs spec 03

Repo: `/home/sebastien/travaux/greptimedb`. Local checkout HEAD `edbcb8224` (2026-09-21, "fail startup on duplicate region engine configs"). Upstream `main` is ahead (to 2026-11-10); some lessons below come from upstream commits after the local checkout, marked "(upstream)".

Method note: `git log` was blocked in this session (worktree isolation forbids git against other repos), so commit history was searched with `gh search commits --repo GreptimeTeam/greptimedb` + `gh api .../commits/<sha>`. Code citations are from the local checkout. Paths are relative to `src/`.

Same stack as ours: `datafusion =55.1.0` (patched via a GreptimeTeam fork), `arrow`/`parquet =59.2.0` (root `Cargo.toml` lines 104, 133, 206, 368). Object storage goes through OpenDAL 0.59 (`object-store/Cargo.toml:30`), not `object_store`.

Vocabulary: a *table* is split into *regions* (partitions by key ranges); a region is the unit of storage: one directory, one manifest, one memtable set, one WAL namespace, one writer. A *datanode* hosts many regions. Rows carry `(primary key, timestamp, sequence, op_type)`; dedup and deletes are by `(pk, ts)` with the highest sequence winning.

---

## 1. Write path

- **Single writer per region, batched by worker.** Regions are hashed onto a fixed set of `RegionWorker` loops (`num_workers` default cores/2, `mito2/src/config.rs:215`; hashing at `worker.rs:510-513`). Each worker loop is one tokio task: it waits for a request, then drains up to `worker_request_batch_size` (64) more with `try_recv` (`worker.rs:1001-1100`). All requests of one iteration go through `handle_write_requests` (`worker/handle_write.rs:43-167`).
- **Order: admission checks → WAL → memtable → ack.** Pressure checks first (reject / stall, below). Then one `WalWriter` collects one protobuf `WalEntry` per region (mutations + Arrow-Flight-encoded bulk parts; `wal.rs:195-227`, encoder `wal/encoder.rs`) and does **one `append_batch` per worker iteration** (`worker/handle_write.rs:612-657`). This is the first layer of group commit. Then rows go into memtables (one spawned task per region when several regions are involved), then the committed sequence is published, then the caller is notified. Acks are sent when `RegionWriteCtx` notifiers drop (`region_write_ctx.rs:34-70`). So **a write is acknowledged after the WAL append returns and the rows are in the memtable.**
- **Sequences.** Each row gets a sequence (`next_sequence += num_rows`, `region_write_ctx.rs:152-183`). Each region's WAL entry gets an entry id that must be contiguous; raft-engine rejects holes and overwrites of compacted entries (`log-store/src/raft_engine/log_store.rs:171-236`). On open, the sequence and entry id start from the manifest's flushed values (`region/version.rs` module docs), so ids never go backwards after a crash that lost unsynced WAL.
- **Local WAL = raft-engine** (TiKV's log engine, `raft-engine 0.4.1`). One shared log for all regions, one namespace per region (`raft_engine/log_store.rs:111-146`). Config (`common/wal/src/config/raft_engine.rs:23-63`, `config/datanode.example.toml:140-180`): segment `file_size` 128 MiB, `purge_threshold` 1 GiB, `purge_interval` 1 min, `RecoveryMode::TolerateTailCorruption`, batch compression above 8 KiB, log file recycling on. Framing and checksums are raft-engine's own (LogBatch with checksum and optional LZ4). I could not verify the exact on-disk format here because the crate source is not in the local cargo registry.
- **fsync policy: by default the write is NOT fsynced before ack.** `sync_write = false`, with a background `sync_period` task (default 5 s) calling `engine.sync()` (`raft_engine/log_store.rs:85-108, 131-134`). raft-engine does its own leader/follower write grouping across concurrent writers. Durability is "lose up to ~5 s on power loss" unless the operator sets `sync_write = true`. **Lesson (upstream `180e2716f`, 2026-10-28):** on raft-engine 0.4.2 `Engine::sync()` wrote an empty batch and returned *without fsync*, so the periodic sync never made acknowledged data durable. It was silently broken until someone checked.
- **Kafka WAL** (`log-store/src/kafka/`): many regions share a topic. Entries that do not fit a Kafka record are split into First/Middle/Last records (`kafka/util/record.rs`). Producer batching lives in `kafka/worker/produce.rs`. Replay has to read and filter other regions' entries (`wal.rs:108-135`, `RegionRawEntryReader`). Kafka WAL pruning *forces flushes* (`FlushReason::RemoteWalPrune`, `flush.rs:213-236`).
- **Object-store WAL (upstream, Sep–Nov 2026):** a new provider that writes batched WAL objects to S3 with conditional create, chained objects, reclaim boundaries, and a fault matrix (`d45f5d6ea`, `194bc2fb3`, `822042c7b`, `c8ea00b13`, `29d5854e6`). The early parts are in the local checkout (`log-store/src/object_store_wal/`).
- **Recovery** (`region/opener.rs:919-1010`): read from `flushed_entry_id + 1`. Entries with id ≤ flushed are warned about and either skipped or rejected (`allow_stale_entries`). Replayed rows keep their WAL sequence.
- **Truncation after flush:** when a flush commits, the worker calls `wal.obsolete(region, flushed_entry_id)` (`worker/handle_flush.rs:422-435`). raft-engine's purge task deletes a segment once no namespace still needs it (`raft_engine/log_store.rs:51-83`). A region that never flushes pins segments; the purge result reports such namespaces, but acting on it is a TODO (line 64).
- **WAL skipping:** a table, or a single request, can skip the WAL (`skip_wal`, `fa128adb8`, `737025760`). Such regions are flushed on close (`a026b3b28`).

## 2. Memtable

- **Structure.** The default is now the **BulkMemtable** (flat format, `memtable/bulk.rs`). It keeps a list of Arrow "parts" plus one unordered buffer for tiny writes (`BulkParts`, lines 184-203). In the background it merges groups of 16 small parts (`DEFAULT_MERGE_THRESHOLD`, line 71) into sorted multi-parts. Merged parts above the row/byte thresholds are **encoded as in-memory Parquet** (`EncodedBulkPart`) to cut memory. The byte threshold is `clamp(global_buffer/32, 64 MiB, 512 MiB)` (lines 93-119, 170-176). Parts being merged are flagged and reset by an RAII guard if the merge fails (lines 382-415). The older `TimeSeriesMemtable` (per-series map) still exists. Before flushing, the memtable waits for its background merge (`flush.rs:600-606`).
- **Time partitions.** The mutable memtable is split by time window (`memtable/time_partition.rs`, default window 1 day, line 46), so flush outputs follow time windows.
- **Versioning.** `MemtableVersion { mutable, immutables: Vec }` (`memtable/version.rs`). **Several immutables are allowed** because a failed flush leaves its immutable behind and the next freeze adds another.
- **Memory accounting and triggers** (`flush.rs:110-209`, config docs `datanode.example.toml:540-551`):
  - `global_write_buffer_size` defaults to 1/8 of RAM, capped at 1 GiB.
  - Flush the engine when mutable memory ≥ 50 % of it, or total ≥ 100 %.
  - **Stall** writes when total ≥ 100 %.
  - **Reject** at `global_write_buffer_reject_size` (default 2×).
  - Per-region `write_buffer_size`: a region stalls at 1× and rejects at 2×.
  - Periodic flush: `auto_flush_interval` 10 min, a default that was lowered in `ba3c5a939`.
  - Flush reasons are listed in `FlushReason` (`flush.rs:213-236`).
- **Backpressure.** A stall does not fail the request: requests wait in the worker's `stalled_requests` queue (`worker/handle_write.rs:81-99`). When any flush finishes, `notify_group()` wakes **all** workers. Each re-checks its stalled requests and may also flush its own largest memtables, "as this worker may have a large memtable" (`worker.rs:1053-1066`, `worker/handle_flush.rs:386-388`). **Lesson `0177f244e`:** "fix write stall that never recovers due to flush logic issues". The fix also changed engine-full handling to "flush multiple regions when engine is full".
- **Reads of unflushed data.** A scan snapshots the `Version`, which holds the mutable and immutable memtables plus the SST levels. It keeps memtables whose time range intersects the predicate (`read/scan_region.rs:520-540`) and merges them with SST ranges. Visibility is bounded by the committed sequence. **Lesson `d0fecdd6b`:** the committed sequence was published before bulk parts were installed, so a snapshot could permanently miss rows. Publish only after the rows are installed.

## 3. Files (SST Parquet)

- **Writer properties** (`sst/parquet/writer.rs:542-565`):
  - `ZSTD(ZstdLevel::default())`, which is **level 1** in parquet-rs 59 (`parquet-59.3.2/src/compression.rs:598-602`).
  - `Encoding::PLAIN` as the base, with dictionary left at its default (on).
  - Timestamp and sequence columns: `DELTA_BINARY_PACKED`, dictionary off. The `op_type` column is uncompressed.
  - Optional BYTE_STREAM_SPLIT for float columns (`sst/parquet.rs:80-100`).
  - `max_row_group_row_count = 100 * 1024` (`sst/parquet.rs:78`).
  - **Statistics and column-index truncation disabled** (`set_statistics_truncate_length(None)`, `set_column_index_truncate_length(None)`). Lesson `c92ab4217`: truncated min/max of the encoded primary key broke stats decoding and pruning.
  - Page size and statistics level are the parquet-rs defaults: 1 MiB pages, 20 000-row page limit, page-level statistics.
  - Region metadata JSON goes into the KV metadata.
- **No Parquet-native bloom filters.** Indexes are separate **Puffin** sidecar files (`<file_id>.<version>.puffin`, `sst/location.rs:50-58`). Kinds: inverted index (tags), "skipping" bloom index (granularity 10 240 rows, fpp 0.01, `datatypes/src/schema/column_schema.rs:74-76`), full-text (tantivy or bloom), vector. They can be built synchronously or **asynchronously after flush** (`IndexBuildMode::Async`, `worker/handle_flush.rs:439-452`) and rebuilt without rewriting data, since the index is versioned.
- **Sort order:** primary key, then timestamp, then sequence descending.
- **Output size:** soft `max_output_file_size` 512 MiB (`region/options.rs:452`). The writer cuts at the next series boundary after the limit, so one series is never split across files (`writer.rs:344-479`). SST write buffer is 8 MiB, which is also the multipart chunk.
- **Layout and naming:** `<table_dir>/<table_id>_<region_seq>/[data|metadata/]<uuid-v4>.parquet` (`sst/location.rs:42-47`, `store-api/src/storage/file.rs:41-43`). Directories are **not** partitioned by time. Time partitioning is logical: each file's `time_range` is in the manifest (`sst/file.rs:203-290`), alongside level, size, row and row-group counts, max sequence, num_series, and pk min/max.

## 4. Manifest

- **Storage.** Per region, in the same object store as the data: `<region>/manifest/`. It holds delta files `{version:020}.json` (optionally gzip), checkpoint files, and a `_last_checkpoint` pointer (`manifest/storage.rs:50-76`, `storage/checkpoint.rs`, `storage/delta.rs`).
- **Actions** (`manifest/action.rs`): `Change` (schema), `Edit {files_to_add, files_to_remove, flushed_entry_id, flushed_sequence, ...}`, `Truncate`, `Remove`. `RegionManifest` is the materialized state: `files: HashMap<FileId, FileMeta>`, `flushed_entry_id`, `flushed_sequence`, `removed_files` (with removal timestamps), and `manifest_version` (lines 135-160).
- **Atomic commit = one PUT of the next delta file.** `update_inner` bumps the version, saves the delta, then rebuilds the in-memory manifest (`manifest/manager.rs:570-620`). Correctness relies on the single writer per region. Upstream later added "publish manifest deltas by conditional create under a lease" (`50a2c4d37`).
- **Checkpoints:** every `manifest_checkpoint_distance` (10) versions, written asynchronously by `Checkpointer`. It advances the in-memory checkpoint version *before* deleting older deltas, and treats leftover deltas as harmless on recovery (`manifest/checkpointer.rs:35-75`). Lessons: `449243a17` "update manifest state before deleting delta files"; `dc2f2cbfa` "manifest recovery scans after last version if possible"; `d1e61a4ad` "chunk manifest object writes"; `d5c616a9f` added a manifest cache. Manifest size became a concern (`7f5b02f4f`).
- **Reader snapshots.** `VersionControl` holds an `Arc<Version>` (metadata, memtables, SST levels, flushed ids), replaced copy-on-write (`region/version.rs:15-30, 360-380`). Queries hold the `Arc`.
- **Obsolete-file deletion is reference-counted.** Each SST has a single `FileHandle`. When the last reference drops (no `Version` or scan holds it) and it was `mark_deleted`, the purger deletes the file (`sst/file.rs:716-751`, `sst/file_purger.rs`). So **a long-running query can never lose a file mid-scan.** For shared object storage there is also a GC worker:
  - lingering time 1 h for removed files;
  - 24 h for "unknown" (orphan) files;
  - an optional full listing to find orphans (`gc.rs:15-22, 157-209`; RFC `docs/rfcs/2025-07-23-global-gc-worker.md`).
- **Orphans from failed jobs.** Flush and compaction track written-but-uncommitted SSTs (`UncommittedSsts`) and delete them on failure, **unless the manifest write may have persisted** (`flush.rs:377-433, 546-567`). **Lesson `1cb01d0d4`:** a PUT can succeed on the server while the client sees a timeout, after which retries come back RateLimited or PermissionDenied. The code treated those errors as "not written" and deleted SSTs that the stored delta referenced. Treat every ambiguous error as "maybe committed"; leaving an orphan is the safe failure.

## 5. Compaction (TWCS)

- **Strategy: Time-Window Compaction Strategy** (`compaction/twcs.rs`). Files are bucketed by the time window of their max timestamp. The window is configured, or inferred from L0 spans among 1 h / 2 h / 12 h / 1 d / 1 w (`compaction/buckets.rs:23-79`). Only two levels exist: L0 from flush and L1 from compaction.
- **Triggers** (`region/options.rs:396-457`):
  - Active (latest) window: compact when ≥ 4 L0 files; L1 merge when ≥ 16 L1 files.
  - Inactive windows: ≥ 2 files; L1 merge at 8.
  - Max 16 input files (env-tunable).
  - In mixed L0/L1 picks, at most 2 L1 rows rewritten per L0 row (`twcs.rs:63-80`).
  - Compaction is scheduled after each successful flush (`worker/handle_flush.rs:502`), rate-limited by `min_compaction_interval`.
- **Concurrency:**
  - At most one compaction per region at a time.
  - Files picked as inputs are flagged `compacting` with an atomic `try_set_compacting` (`sst/file.rs:558-575`).
  - Global limits: `max_background_compactions` = cores/4, flushes = cores/2, purges = cores.
  - CPU-heavy work runs on a dedicated compact runtime (`spawn_blocking_compact`).
  - Optional compaction memory budget (`experimental_compaction_memory_limit`, `baffed8c6`).
  - Results come back to the region worker as a message, and the worker applies the edit (`worker/handle_compaction.rs:131-146`), so flush, compaction and DDL commits are serialized per region.
  - Remote compaction (on separate nodes) exists for clusters.
- **Delete markers survive** unless the compaction input covers every file that could hold an older version of the key (`twcs.rs:320-322`: not append mode, no overlap with other windows, selected inputs do not overlap unselected files). **Lesson `3c80df043`:** a partial selection dropped a marker while the masked row stayed in an unselected file, and the deleted row reappeared.
- **Termination and fairness lessons:**
  - `bb3f07b48` (upstream): L1 merges could rewrite forever, because outputs split by key created new overlaps. Every pick must strictly decrease a measure such as (conflicts, files).
  - `aec7ba34c`: prioritize newer windows.
  - `e38b84a9b` / `a0d798129`: candidate starvation; schedule fairly across regions.
  - `0fcf61c3f`: skip oversized tasks.
  - `98612800a`: the picker blocked the region worker; it was made async.
  - `6a084d9a4`: release SST write buffers during compaction.
  - `743261f05`: reading row-group stats with a logical column index instead of the Parquet leaf index (nested columns) made min/max pruning drop row groups. Compaction reads inputs with a time-window predicate, so it **silently lost data**.
- **Write amplification:** not measured in-repo. The design bounds it with the L1:L0 row ratio, the input cap, and "inactive windows only take closed merges".

## 6. Query path

- **DataFusion integration.**
  - `TableProvider` → `RegionScanExec`, a custom `ExecutionPlan` over a `RegionScanner` (`query/src/dummy_catalog.rs:150-230`, `table/src/table/scan.rs`). **It does not use DataFusion's Parquet scan**: mito2 has its own Parquet reader, because it must k-way merge memtables and SSTs and dedup by (pk, ts, seq).
  - Filters on tag and time columns are pushed down as `Exact`; others are `Inexact`.
  - The scanner reports `PartitionRange {start, end, num_rows}` time ranges per output partition (`store-api/src/region_engine.rs:280-300`).
  - Custom optimizer rules use those ranges: `ParallelizeScan`, `ScanHint`, `WindowedSort`/`PartSort`/`WindowSort` (`query/src/optimizer/windowed_sort.rs`, `query/src/part_sort.rs`, `query/src/window_sort.rs`). This makes `ORDER BY ts DESC LIMIT N` sort each time range on its own and stop once the top-k cannot change.
- **Scanners:** `SeqScan` (sorted by pk, ts within each range; merge + dedup), `UnorderedScan` (append mode, no dedup), `SeriesScan` (`read/scan_region.rs:402-475`).
- **Pruning order:**
  - File-level: by manifest time range (`db5a30c49`) and partition expression, before any I/O.
  - Then per file (`sst/parquet/reader.rs:895-991`): row-group min/max → full-text index → inverted index → bloom (skipping) index → full-text bloom → vector.
  - Then page index (optionally deferred) and a reduced-column "prefilter" pass.
  - Index results are cached (`index_result_cache`).
- **Dedup on read:** `read/flat_merge.rs` (k-way merge with a winner tree, hardened in `be482553b`) and `read/flat_dedup.rs` (LastRow / LastNonNull strategies; `filter_deleted` drops `Delete` rows). Tables in append mode skip dedup.
- **Caches** (`cache.rs:1011-1045`, defaults in `datanode.example.toml:552-620`):
  - SST metadata, compact and decoded, 512 MiB;
  - vector cache;
  - page (byte range) cache;
  - inverted, bloom and Puffin metadata caches;
  - selector-result and range-result caches;
  - prefilter cache;
  - **write cache**: a local-disk write-through cache, 5 GiB, TTL 8 h, 20 % reserved for indexes;
  - manifest cache, 256 MiB.
- **Resource limits:**
  - `scan_memory_limit` across all scans, with fail-fast or wait (`b3dc5674e` counts SST decoding against it).
  - The DataFusion `memory_pool_size` (default 50 % of RAM) is **process-wide**, greedy or fair, wrapped in `TrackConsumersPool` (`query/src/query_engine/state.rs:604-625`), with optional spill configuration (`runtime.rs:78-107`).
  - `max_concurrent_queries`, `max_concurrent_scan_files` 384.

## 7. Deletes and retention

- **Deletes are writes:** a row with `op_type = Delete` for an exact `(pk, ts)` key goes through WAL and memtable like an insert. SQL `DELETE ... WHERE` runs the query, keeps only the key columns, and writes delete markers (`query/src/datafusion.rs:256-268, 330-372`). There is no predicate tombstone in the engine.
- Markers hide rows at read time (dedup with `filter_deleted`). They are physically dropped only by a compaction whose input set is "closed" (§5).
- **Truncate:** full, or by time ranges. By-range truncate only drops files **fully contained** in the ranges, so no rewrite happens (`worker/handle_truncate.rs:131-170`).
- **TTL:** whole SSTs whose max timestamp has expired are removed *by the compaction picker* (`compaction/picker.rs:170-185`, `sst/version.rs:241-257`). **No query-time TTL filter** shows up in `read/`. Rows in partially expired files, or in files not yet compacted, remain visible.

## 8. Tenancy

Isolation is physical per table and region: own directory, manifest, memtables, WAL namespace and indexes. There is nothing per tenant inside a region. Small tables made regions too "heavy" (directories, manifests, many small files, metadata). The **metric engine** was built to multiplex many logical tables onto one physical mito region (`docs/rfcs/2023-07-10-metric-engine.md`, motivation section). This is the main warning for a design that splits files by a high-cardinality tenant key.

## 9. Local disk vs object storage

- OpenDAL behind an `ObjectStore` wrapper with layers (retry, metrics, LRU read cache) in `object-store/`.
- With S3, the **write cache** writes the SST to local disk first, uploads it, and keeps the local copy for reads (`cache/write_cache.rs:15-60`). On a cache miss, reads download the whole file to fill the cache (`f6afb10e3`).
- Index files can be preloaded on region open.
- The manifest lives in the object store next to the data, so a region can be recovered from the bucket alone.

## 10. Concurrency

- One tokio task per region worker. Requests are batched, and all region state transitions are serialized in that loop.
- Background flush, compaction and purge jobs run under schedulers with global caps and report back as `WorkerRequest::Background` messages (`flush.rs:390-434`).
- Memtable merges run on a shared dispatcher. Compaction CPU work runs on a separate runtime (`a9e2a89b7`).
- Lock-light reads: `Arc<Version>` snapshot; the bulk memtable has an `RwLock<BulkParts>`.

## 11. Failure handling and testing

- **Engine tests** in `mito2/src/engine/*_test.rs`: flush, compaction (88 KiB), open/replay, truncate, `scan_corrupt`, `filter_deleted`, `skip_wal`, parallel, and more. An `EventListener` hook trait (`engine/listener.rs`: `on_flush_begin`, `on_flush_commit_begin`, `on_write_stall`, `on_merge_ssts_finished`, ...) lets tests pause the engine at precise points for deterministic race tests (e.g. `d0fecdd6b` uses a bulk-install barrier).
- **Fuzz** in `tests-fuzz/targets/`: DDL, insert, failover, migration, remote-WAL prune, and `unstable/` (random kills of a standalone process via `UnstableProcessController`).
- **Fault injection:** a conditional-create fault matrix for the object-store WAL (`c8ea00b13`, `29d5854e6`), and an S3 WAL benchmark with crash/restart cells (`0e82b29b2`).
- **Benches:** `mito2/benches/` (memtable, WAL encode, flat merge, compaction picker, cache stream, index update), TSBS reports in `docs/benchmarks/tsbs/`, a log benchmark in `docs/benchmarks/log/`, and `scanbench` (`docs/scanbench.md`).

## 12. Lessons from history (selected)

| Commit | Lesson |
|---|---|
| `180e2716f` (upstream) | The periodic WAL fsync was a no-op on the dependency version in use. Acknowledged data was never durable. Test that fsync really happens. |
| `8553db3f2` | Before recording a WAL watermark in the manifest, wait until the WAL is durable up to that id. |
| `d0fecdd6b` | Publish the visibility watermark only after rows are installed; otherwise snapshots miss rows forever. |
| `0177f244e` | Write stall that never recovered; on engine-full, flush several regions and wake all workers. |
| `1cb01d0d4` | Ambiguous object-store errors (timeouts, 5xx followed by 429 or 403) can hide a successful PUT. Never delete outputs unless the commit provably failed. |
| `449243a17`, `dc2f2cbfa` | Manifest checkpoint ordering: update state before deleting deltas; recovery must tolerate leftovers. |
| `3c80df043` | Drop delete markers only when every older version is in the input. |
| `bb3f07b48`, `e38b84a9b`, `aec7ba34c`, `0fcf61c3f` | Compaction must terminate, be fair, favour new windows, and bound its task size. |
| `743261f05` | Wrong Parquet leaf index for stats with nested columns caused silent data loss in compaction. |
| `c92ab4217` | Statistics truncation broke pruning of encoded keys; it was disabled. |
| `98612800a`, `a9e2a89b7` | Keep CPU-heavy planning off the latency-critical loop or runtime. |
| `912db2241`, `3bd915865` | Kafka WAL: deduplicate multipart records; checkpoint handling during replay. |
| `3131cdbcb` | Bound the decompressed size of compressed ingest requests (memory-admission gap). |
| `f690bbb20` | They built a remote-WAL index for faster replay, then removed it (complexity not worth it). |

## 13. Recommendations for llmobs spec 03

**§1 WAL**
- Keep `wal.sync = always` with group commit. It is stricter than GreptimeDB's default, and their history shows how easily "periodic sync" turns into "no sync".
- Add a test that *counts* `fdatasync` calls, for example a sync counter in the writer plus a crash test that kills the process after ack.
- Initialize the next `seq` as `max(last seq in WAL, flushed_seq) + 1`. Otherwise, after a crash in `every_ms` mode lost the WAL tail, new records could get `seq ≤ flushed_seq` and be **skipped on the next replay**. The spec doesn't say this.
- Bound decompressed OTLP payload sizes before WAL admission.

**§2 Memtable/Flush**
- Add a **stall** stage between "flush now" and 429. One hard limit with an immediate 429 is fine for OTLP clients, but say which component wakes waiters.
- Make "flush finished" wake every waiter and re-evaluate pressure (their never-recovering stall bug).
- `Vec<RecordBatch>` per partition degrades with many tiny OTLP batches. Plan background coalescing of small batches, as BulkMemtable does: merge every 16 parts and optionally encode large ones.
- The spec allows one `frozen`. Say what happens when `max_age` fires while a flush keeps failing: GreptimeDB keeps several immutables. With ours, the active memtable grows until 429, which is acceptable if documented.

**§3 Parquet**
- Our settings match theirs closely: 100 k-row row groups, zstd (they use level 1, we use 3; benchmark both), page statistics, delta encoding for timestamps (worth adding for `start_time`, `end_time`, `ingest_time`), dictionary off for big text.
- Disable statistics truncation for any column we prune on.
- When reading row-group stats ourselves, map logical columns to Parquet leaf indexes. `attributes` is nested; use DataFusion's `StatisticsConverter`, not raw indexes.
- Parquet-native bloom filters are fine for us. Note that GreptimeDB found sidecar indexes useful because they can be built or rebuilt **asynchronously** without rewriting data.

**§4 Catalog**
- Use **refcounted file handles** for local deletion (purge when the last snapshot drops) rather than relying only on `gc.grace` = 10 min. A 60 s SQL query with a slow S3 read, or any future long export, could outlive the grace period. Keep the time grace only as a crash-safety net and for orphans.
- Hold one mutex across *SQLite commit + snapshot publish*. Otherwise two writers (flush and compaction) can publish snapshots in a different order than they committed and lose an edit.
- With S3, the catalog lives only on local `meta.db`; GreptimeDB keeps its manifest beside the data. Since our Parquet KV metadata is self-describing, specify a "rebuild catalog from object-store listing" recovery path (cross-check spec 12 backups).

**§5 Compaction**
- **Major flag: partitioning by `(project_id, day)` multiplies small files.** Every flush writes one L0 file per active project for both `spans` and `traces`: N projects × 720 flushes/day at `max_age` = 120 s. GreptimeDB built the metric engine precisely because per-small-table regions were too heavy. Options:
  - let L0 files hold many projects (sorted by `project_id, trace_id, start_time`, pruned by `project_id` stats) and split by project at L0→L1;
  - or a minimum size per partition before writing a dedicated file.
- Define how compaction, the deletion rewriter and retention avoid claiming the same input file: an in-memory `compacting` claim, plus a commit-time check in SQLite that every input is still `active`, aborting otherwise.
- Every compaction pick must strictly reduce file count or overlap; prioritize newest partitions; enforce fairness across projects; cap input bytes; set a compaction memory budget.
- Our "drop `deleted` scores only at day close" is the right rule (same as their `3c80df043` fix).

**§6 Query path**
- **Spans are not deduplicated on read** (only at compaction), so SDK retries show duplicate spans until compaction. Either dedup `(trace_id, span_id)` on read for L0 plus memtable, or document it.
- Make the DataFusion memory pool **process-wide** with a per-query cap. A 2 GiB per-query pool × concurrent queries can exceed 32 GiB.
- For "latest N traces", consider a GreptimeDB-style physical plan: expose per-file time ranges as ordered partition ranges and stop early, instead of re-planning widening windows.
- Their own Parquet reader exists because of merge and dedup by sequence. We avoid that by keeping dedup to compaction and window functions, which is a reasonable simplification for MVP.

**§7 Deletion**
- GreptimeDB has no predicate tombstones: it resolves to keys and writes markers. Our resolve-at-creation plus cut-off approach is consistent with that.
- Before deleting freshly written rewrite outputs on error, apply the ambiguous-commit rule (`1cb01d0d4`). Locally with SQLite this is mostly clear-cut; S3 uploads are where it matters.

**§8 Retention**
- Day-partition drop matches their file-level TTL.
- Unlike GreptimeDB, consider a query-time cut so expired days disappear exactly at the boundary instead of when the hourly job runs. That is cheap: drop partitions from the snapshot.

**§10 Failure handling**
- Add an EventListener-style test hook trait (`cfg(test)`) for deterministic race tests: flush commit, compaction commit, snapshot publish.
- Add a kill-at-random-point process test, as in `tests-fuzz/targets/unstable`.
- Add a test with an object store whose PUT succeeds but reports failure.
