# Quickwit storage layer: research notes for llmobs spec 03

**Repo:** `/home/sebastien/travaux/quickwit`. Paths below are relative to `quickwit/` unless they start with `docs/`.

**Latest commit:** `c02cc1c8a2`, 2026-09-18 ("Add indexers AZ ready gate…").

**How the history was read:** git commands could not run against this repo from the sandbox. The history was extracted read-only by parsing the pack file (3,843 commits reachable from `main`).

**Scope:**
- Quickwit stores logs and traces in tantivy "splits".
- Since 2026 it also has a **Parquet + DataFusion path for metrics**, in the crates `quickwit-parquet-engine`, `quickwit-datafusion`, `quickwit-df-core` and `quickwit-dst`. That path is the closest analogue to our design, so it is covered too.
- There is also a design doc (`docs/internals/tantivy-parquet-architecture.md`, 2026-01) proposing to move log and trace columns to Parquet. It would store attributes as an Arrow Map ("VARIANT"), measured at +10% storage.

**What is not checked out here:** `mrecordlog`, the WAL library, is an external git dependency (`Cargo.toml:184`, rev `3b3562ef`) and is not vendored. Its internal framing is therefore described only as it shows through Quickwit's usage.

---

## 1. Write path (WAL)

**Library.** The ingest-v2 WAL is `mrecordlog`, a multiplexed record log. One log per node holds many named *queues*, one queue per shard, identified as `index_uid/source_id/shard_id`.
- Each record is an `MRecord`.
- The first byte is a header version (`quickwit-ingest/src/ingest_v2/mrecord.rs:24-37`):
  - V0 is `[version, type]` followed by the raw doc bytes. The types are `Doc` and `Commit`.
  - V1 is a protobuf `MRecordV1`, added in #6521 (2026-06).
- If a record has an unknown version or type, it is *skipped and counted in a metric* rather than misread (`mrecord.rs:53-75`).
- Writers still emit V0 so that rolling back to an older binary stays safe (`mrecord.rs:78-88`).
- Checksums, block framing and file rotation live inside mrecordlog. From upstream memory, and not verifiable here, it uses fixed-size blocks with CRC'd frames in rolling files, and a file is removed once every queue has truncated past it.

**Fsync policy: there is none.** `PersistPolicy::OnDelay { interval: 5s, action: Flush }` is used, with `// TODO maybe we want to fsync too?` (`ingest_v2/state.rs:252-258`). The internal doc says the same: "flushes every 5 seconds without fsync" (`docs/internals/compaction-architecture.md:81`).
- There is no group commit in our sense.
- Writes are acknowledged after `append_records` returns, i.e. once the data is in the userspace buffer or page cache (`ingest_v2/ingester.rs:540-605`).
- Durability against power loss is therefore at most about 5 s worse than an fsync design.
- Replication (`replication_factor`) is documented as "not working yet" (`docs/internals/ingest-v2.md:28`).

**Commit types on the ingest request** (`quickwit-proto/protos/quickwit/ingest.proto:44-49`):
- `Auto`.
- `WaitFor`: block until the docs are in a published split.
- `Force`: append a `Commit` record, which forces a split cut, then wait (`mrecordlog_utils.rs:57-70`).
- `publish_tracker.rs:15-27` waits for shard-position publish events.

**Capacity and backpressure.** Before each append, `check_enough_capacity` compares the WAL's *disk* and *memory* usage with the configured limits. If a limit would be exceeded, it returns `WalFull` (`ingester.rs:430-450`, `mrecordlog_utils.rs:107-140`). The WAL keeps un-truncated records in memory as well, which is how indexers fetch them. Each shard also has a byte rate limiter (`ingester.rs:452-468`).

**I/O errors.**
- If an append fails, the *shard is closed* and the client gets `NodeUnavailable` (`ingester.rs:560-610`). The router retries, up to `MAX_PERSIST_ATTEMPTS` (`router.rs:451-480`).
- The async wrapper runs mrecordlog in `spawn_blocking`. If that future is cancelled, the log becomes "poisoned" and the process **aborts** (`mrecordlog_async.rs:36-50`; commit `05311f1cec`).

**Recovery.** At startup the WAL is opened asynchronously (`aad9b34999`). Every queue is recovered as a *closed* shard, including empty ones (`state.rs:236-330`). The comment there is a lesson: they used to delete empty queues at startup, and that silently diverged from the control plane's view of shards. If the WAL fails to open, the ingester goes to `Failed` and refuses work (`state.rs:268-272`).

**Truncation after publish.**
1. The publisher commits the split, together with the checkpoint delta, to the metastore.
2. It then sends `SuggestTruncate` to the source (`quickwit-indexing/src/actors/publisher.rs:134-149`).
3. The source asks the ingester to truncate the queue up to that position (`ingester.rs:780-813`).

## 2. Buffering before a split is published, and commit triggers

**Pipeline.** Source → DocProcessor → Indexer → (Serializer) → Packager → Uploader → Sequencer → Publisher, with one actor chain per (index, source, shard set).
- The Indexer builds an in-progress tantivy index, the "workbench".
- A commit timer is armed when the *first* document of a workbench arrives (`actors/indexer.rs:254-268`).

**Commit triggers** (`indexer.rs:600-620`, `485-495`):

| Trigger | Value |
|---|---|
| `Timeout` | `commit_timeout_secs`, default **60 s** (`quickwit-config/src/index_config/mod.rs:227`); the OTel traces index uses **5 s** (`quickwit-opentelemetry/src/otlp/traces.rs:168-169`) |
| `MemoryLimit` | `heap_size`, default 2 GB (`index_config/mod.rs:72-73`) |
| `NumDocsLimit` | `split_num_docs_target`, default 10M |
| `ForceCommit` | explicit |
| `Drained` / `NoMoreDocs` | source exhausted |

**Backpressure.** It comes from bounded actor mailboxes; for example, the Parquet packager mailbox is `Bounded(1)`. There is also a node-level `max_indexing_write_throughput` limiter, added in `f2f2a5c7ed` (2026-09).

**Can unpublished data be queried? No.** Searchers only see `Published` splits listed from the metastore. Time-to-searchability is the commit timeout plus indexing, upload and publish, typically 5–60 s. Our memtable gives us a strictly better freshness model.

## 3. Files

**Split structure.** A split is one file: the concatenated tantivy files followed by a footer (`docs/internals/split-format.md`). The footer contains:
- the JSON byte ranges of every file in the bundle;
- a **hotcache**, a small pre-extracted set of the byte ranges needed to open the index;
- an optional trailer with magic `QWFT`.

**One GET to open a split.** The metastore stores `footer_offsets` (`quickwit-metastore/src/split_metadata.rs:120-125`), so a split can be opened with a single ranged GET and no HEAD request.

**Split metadata** (`split_metadata.rs:60-137`):
- `partition_id`
- `time_range` (inclusive, seconds)
- `tags`
- `maturity`
- `delete_opstamp`
- `num_merge_ops`
- `doc_mapping_uid`: splits are only merged when their doc mappings match
- `footer_offsets`

**Partitioning.**
- **Time:** none at the file level. Splits come out in arrival order, and the merge policy is relied on to keep time ranges tight.
- **Tag / partition key:** a `partition_key` expression such as `hash_mod(service_name, 100)` routes docs to separate splits. Above `max_num_partitions` (default 200), docs spill into an "OTHER" split (`indexer.rs:63,153-189`).
- The OTel traces index ships with partition key and tags **commented out** (`traces.rs:165-166`).

**Naming.** Split IDs are ULIDs, so names are flat: `<index_uri>/<split_id>.split`. Parquet files are `metrics_<ulid>.parquet` (`quickwit-parquet-engine/src/split/metadata.rs:84-90`).

**Parquet writer** (`parquet-engine/src/storage/config.rs:23-99`, `188-263`):
- zstd(3), 128K-row row groups, 1 MiB pages.
- Page-level statistics plus column and offset index, truncated to 64 bytes.
- Native `sorting_columns` declared.
- Bloom filters only on selected columns, at fpp 0.05.
- **Physical column order** puts sort columns first, so a streaming merge can read the sort keys before the body (`storage/writer.rs:350-412`).
- Debug builds re-verify sortedness after writing (`writer.rs:303-345`).

## 4. Metastore

**Two implementations.**

*File-backed* (one `metastore.json` per index on the object store):
- Each mutation clones the in-memory index, applies the change, PUTs the whole file, and swaps it in only if the PUT succeeds.
- If the write outcome is uncertain, the cache is discarded and reloaded (`metastore/file_backed/mod.rs:213-260`).
- Readers can poll. It assumes a single writer.

*Postgres* (`metastore/postgres/`). It holds:
- indexes;
- sources with their checkpoints;
- shards;
- splits, with state, time range, tags, maturity timestamp, delete_opstamp and JSON metadata;
- delete tasks;
- `metrics_splits` for Parquet (migrations 26–28).

**Lifecycle:** `Staged → Published → MarkedForDeletion → (row deleted)` (`split_metadata.rs:372-410`).

**Stage before upload.** The uploader **stages the metastore row before uploading the object** (`actors/uploader.rs:~330-365`; Parquet: `parquet_uploader.rs:263-335`). As a result, every object in storage has a catalog row, and orphans are just old `Staged` rows.

**Atomic publish.** `publish_splits(staged_ids, replaced_ids, checkpoint_delta, publish_token)` does the following in one transaction (file-backed: `file_backed_index/mod.rs:436-482`):
- staged → published;
- the replaced splits (merge inputs) → marked for deletion; **they must currently be Published, or the whole publish fails**;
- the source checkpoint delta is applied.

In Postgres, the Parquet version is a single CTE that **checks the affected row counts exactly** and rolls back on any mismatch (`postgres/metastore.rs:2382-2447`).

**Checkpoint deltas.** They are `(from, to]` per WAL partition. A delta that starts *before* the current checkpoint is rejected as incompatible, which detects double publishes (`checkpoint.rs:310-325`). This is the exactly-once mechanism: "split is published" and "WAL consumed to X" are committed together (`compaction-architecture.md:89-91`). The publish token, tie-broken by a time-based plan ID since `449d08acf6`, fences zombie pipelines.

**Consistent split list.** Every search calls `list_splits(Published, time range, tags)` afresh (`quickwit-search/src/root.rs:1212-1290`). Merges swap splits atomically, and deleted splits stay readable during the grace period.

**GC** (`quickwit-index-management/src/garbage_collection.rs:100-190`, janitor every 10 min):
- `Staged` rows older than **24 h** are marked for deletion (`janitor/src/actors/garbage_collector.rs`).
- Splits marked for deletion longer than `split_deletion_grace_period` (**32 min** by default, clamped to at most 48 h; `quickwit-common/src/shared_consts.rs:25-47`) are bulk-deleted from storage first, then their rows are removed.
- Since `2dcc696433`, the GC paginates and keeps going after an error.

## 5. Merge and compaction

**StableLogMergePolicy** (the default; `merge_policy/stable_log_merge_policy.rs:26-60`):
- Splits are sorted newest first and bucketed into log-scale levels by doc count. The first level holds 3 × `min_level_num_docs` (100K), and each following level is about 3× larger.
- Within a level it merges `merge_factor` (10) to `max_merge_factor` (12) splits, without exceeding `split_num_docs_target` (10M).

**ConstWriteAmplificationMergePolicy** (`const_write_amplification.rs:30-50`):
- It exists **because the log policy makes small tenants pay high write amplification**: about 7× for a tiny tenant, merging toward the same 10M target.
- It merges only splits with the same `num_merge_ops` and stops at `max_merge_ops` (4). Write amplification is bounded by construction.
- The Parquet merge policy is the same idea: merge factor 10, max ops 4, 256 MiB target (`parquet-engine/src/merge/policy/const_write_amplification.rs:67-222`).

**Maturity.** A split becomes mature once it reaches the target size, the max merge ops, or `maturation_period` (48 h). Mature splits are never merged again (`8a684d136e`, `9b6906bd24`). The maturity period is capped by the retention period (`db1751bd44`).

**Merge scope.**
- Tantivy: (index, source, node, partition_id, doc_mapping_uid) (`merge_planner.rs:36-50`).
- Parquet: (index, partition, sort_fields, window_start, window_duration, rg_prefix_len) (`merge/policy/scope.rs:37-94`).

**Crash and respawn.** The planner tags messages with an *incarnation* timestamp, so stale merge plans from a crashed incarnation are ignored. This fixed duplicate merges after respawn in `3c1ade4550` (`merge_planner.rs:87-155`).

**Merges and deletes.**
- The merge output's `delete_opstamp` is the **minimum** of its inputs' opstamps (`merge_executor.rs:286-290`).
- Delete operations are themselves single-split "merges" that rewrite one split (`merge_executor.rs:385-500`).

**Parquet merge engines.**
- *In-memory k-way merge* (the default): every input is read fully into memory.
- *Streaming column-major merge*: it reads the sort columns, computes the merge order, then streams the body columns page by page into M output writers. Memory is bounded per input (`merge/streaming.rs`). It is gated off by default (`parquet_merge_executor.rs:245-275`).
- After a merge they check that rows in equal rows out (invariant MP-4).

## 6. Query

**Pruning.**
- *Time:* the root extracts timestamp bounds from the query AST (`root.rs:1613-1660`), and the metastore filters on split time-range overlap.
- *Tags:* splits carry `field:value` tags plus a `field!` marker, and only when the field has at most 1,000 distinct values (`packager.rs:36-41`). Query terms become a boolean tag filter evaluated in the metastore (`quickwit-doc-mapper/src/tag_pruning.rs`).
- *Parquet path:* metric-name and time bounds are pushed to the metastore. Tags and zonemap regexes are pruned on the client side, and everything else goes to DataFusion's ParquetSource (bloom filters, page index, filter pushdown) (`quickwit-datafusion/src/sources/metrics/table_provider.rs:231-317`). The file size comes from the metastore, so no HEAD request is needed.

**Merging results.**
- Each leaf merges its own splits incrementally.
- The root merges top-K hits and intermediate aggregations, then fetches the docs (`root.rs:795-904`).
- Splits are placed on searchers by rendezvous hashing, for cache affinity.

**Caches** (defaults in `node_config/mod.rs:408-461`):

| Cache | Default |
|---|---|
| Split footer cache | 500 MB |
| Fast-field cache | 1 GB |
| **Partial request cache** | 64 MB |
| Predicate cache | 256 MB, including negative "term absent" entries |
| Optional on-disk split cache | — |

The partial request cache is keyed by split ID plus the request with its time range clamped to the split. That works because splits are immutable (`leaf_cache.rs:80-150`). The Parquet path has **no footer cache yet**; this is a recorded gap (`docs/internals/adr/gaps/007-no-multi-level-caching.md`).

**Limits:**
- `max_num_concurrent_split_searches` (100);
- a warmup memory budget;
- an aggregation memory limit (500 MB);
- a timeout (30 s);
- an optional `max_splits_per_search`;
- hedged retries on slow S3 GETs (`quickwit-storage/src/timeout_and_retry_storage.rs:31-37`).

## 7. Deletes and retention

**Delete tasks** (`docs/overview/concepts/deletes.md`; `quickwit-janitor/src/actors/delete_task_planner.rs:46-73`):
- Each delete query gets a monotonically increasing **opstamp**.
- Every split records the `delete_opstamp` it has been cleaned up to, and new splits start at the latest opstamp.
- The planner fetches splits whose opstamp is older than the latest and checks the pending queries against their time range and tags. It then runs a leaf search to confirm hits:
  - **no hit:** it bumps the split's opstamp, with no rewrite;
  - **hit:** the split is rewritten, batching every pending delete in one pass.
- **Deletes only run on mature splits.** Immature ones wait up to `maturation_period` (48 h) or are cleaned by merges.
- **Deletes are not applied at query time.** Deleted docs stay visible until the rewrite, and the docs say so.

**Retention** (`janitor/src/retention_policy_execution.rs:30-100`): every published split with `time_range.end <= now − period` is marked for deletion, and GC removes it after the grace period. The schedule has jitter (`623b4d0728`).

## 8. Tenancy

Isolation is at the **index** level. Each index has its own doc mapping, indexing settings (commit timeout, merge policy, heap), retention and search settings, plus optional `partition_id` sub-isolation. There is no per-tenant memtable or WAL: shards are per (index, source).

## 9. Object storage

**Abstraction.** The `Storage` trait has implementations for S3, Azure, GCS and OpenDAL, local disk, RAM, and prefix wrappers. A bundle layer provides file-in-split access.

**S3 uploads.**
- Multipart above a threshold, with aggressive `RetryParams`.
- An MD5 per part (`object_storage/s3_compatible_storage.rs:395-520`).
- A concurrency semaphore for uploads (`uploader.rs:58-59`).

**Bugs worth knowing about:**
- Azure multipart blocks were committed in completion order, which corrupted splits about every 45 min in production (`4bb6e2a93a`).
- The GCS put caused memory growth (`5ce4761fed`).

**Local disk use.**
- An `IndexingSplitCache` keeps freshly built splits on the indexer's local disk, so merges do not have to re-download them (`split_store/indexing_split_cache.rs`).
- An `AsyncDebouncer` coalesces concurrent GETs of the same range (`debouncer.rs:33`).

## 10. Concurrency

**Actor framework.** `quickwit-actors` provides bounded mailboxes, supervisors, and `protect_future` for heartbeats. There is one pipeline per (index, source), and a single publisher per pipeline, fenced by a publish lock and token.

**Sequencer.** Uploads run concurrently, but the Sequencer reserves an order slot so that publishes, and therefore checkpoints, go out in order (`actors/sequencer.rs:21-35`).

**Ingester state.** It is behind two locks taken in a fixed order to avoid ABBA deadlocks (`state.rs:243-246`; `4785133e1b`).

## 11. Failure handling and testing

- **Failpoints** (`fail` crate) are placed before and after the packager, uploader, publisher, merge and WAL append. They have their own test binary that panics at each stage and checks the pipeline recovers (`quickwit-indexing/failpoints/mod.rs:15-160`).
- **Verification stack.** TLA+ specs (`docs/internals/specs/tla/*.tla`) are mirrored by Stateright models in `quickwit-dst` (despite the name, these are not a simulator runtime). A shared `check_invariant!` macro panics in debug builds and records a metric in release builds.
- **Invariants** include:
  - MP-4: rows published equal rows ingested, even across crashes;
  - MP-7: no split is in two merges;
  - MC-1: the row multiset is preserved by a merge;
  - SS-1..5: sort-schema invariants.
- **Supporting docs:** `docs/internals/VERIFICATION.md` enforces "write the simulation tests first". There are also metastore backward-compatibility tests and `BENCHMARKING.md`.

## 12. OTLP traces

**Doc mapping** (`quickwit-opentelemetry/src/otlp/traces.rs:57-173`):
- One document per span.
- `trace_id` is bytes, indexed and fast.
- `span_start_timestamp_nanos` is the timestamp field.
- `span_duration_millis` is computed at ingest.
- `is_root` is derived from "no parent".
- `span_fingerprint` (service, kind, name) supports operation listing.
- Attributes are JSON fields with the raw tokenizer.
- Commit timeout is 5 s. There are no tags, no partition key and no retention.
- There is no sorting or dedup at ingest (`traces.rs:620-643`).

**GetTrace.**
- It runs an OR of TermQueries on `trace_id` over `[now − 72 h, now]`, with **no time hint** and a cap of **10,000 spans** (`quickwit-jaeger/src/lib.rs:185-211`, `294-346`; defaults at `node_config/mod.rs:852-901`).
- Every split in the window is opened, because `trace_id` is not a tag. Negative predicate-cache entries make repeated misses cheap.

**FindTraces** works in two phases:
1. A custom aggregation, `FindTraceIdsCollector`, returns the top-K trace IDs ordered by their *newest matching span's* timestamp (`quickwit-search/src/find_trace_ids_collector.rs`). It works on term ordinals per segment and dedups by trace ID during merges. All predicates are **span-level**; Jaeger's min and max duration filter applies to *span* duration.
2. Fetch spans for those IDs in `[min − 1 h, max + 1 h]`, using `max_trace_duration` = 3600 s, with one shared 10K-span budget. Traces beyond that budget are **silently truncated** (`lib.rs:152-183`, `244-291`).

**Spans of one trace in many splits.**
- No trace-level row or materialization exists. Spans are grouped at read time.
- The REST path groups with `chunk_by(trace_id)`, which only groups *adjacent* spans, so the stream is explicitly sorted by trace_id first (`lib.rs:368-370`; `quickwit-serve/src/jaeger_api/model.rs:27-35`).
- Late spans outside the ±1 h window are missed.
- Trace duration and root service are left to the UI client.

**A correction to an inference.** Commit `e69c2c1f64` ("fix trace fragmentation") is about Quickwit's *own* tracing spans for search instrumentation, not about stored trace data. Its full message lists only span-nesting changes.

**Dedup.** Older code deduplicated logs and traces by (service, timestamp), which dropped distinct records that shared that key. It was removed in `fb6be52f12`.

## 13. Lessons from the history

| Commit | Lesson |
|---|---|
| `05311f1cec` | WAL I/O runs in `spawn_blocking`; a cancelled future poisons the log, and the process then aborts. Don't let async cancellation interrupt a WAL write halfway. |
| `472d173807`, `98be4f6faf`, `f38bdfd84b` | Bound the WAL's disk and memory; reset or close shards when the WAL is nearly full; rate-limit the "WAL full" logs. |
| `e05b708181`, `d5264f07c4`, `74b39ac1cd`, comment at `state.rs:286-294` | Deleting WAL queues eagerly (on EOF or at startup) made the WAL diverge from the metadata. Only delete after the authority confirms. |
| `191a2c63e7` + `mrecord.rs` | Version byte on every WAL record; unknown records are skipped and counted, never misread. |
| `3c1ade4550` | Duplicate merges after a pipeline crash and respawn; fixed with an incarnation ID on plan messages. |
| `d10d263e98` / `f195b910ff` / `c0c92d35e3` | Race in "publish wait" (ingest `WaitFor`); fixed and then given a workbench test. |
| `8c25901368` | The same shard appearing twice in one publish batch overwrote its position; now the max is kept. Checkpoint merges must be monotonic. |
| `449d08acf6` | Publish-token tie-break by time-based plan ID, fencing zombie writers. |
| `9b6906bd24`, `8a684d136e`, `db1751bd44` | Splits must *become mature* by age, otherwise delete tasks and merges stall; cap maturity by retention. |
| `2dcc696433`, `20b49569c9`, `6b4acd0b5c` | GC must paginate, batch-delete, and not stop at the first error. |
| `7e274a1c6d`, `b570523aa7` | They stripped Postgres transactions and triggers where possible for throughput, keeping a transaction only for publish. |
| `4bb6e2a93a` | Multipart part ordering corrupted objects; enable integrity checks in tests. |
| `995bb627cd` | Infinite loop in the split cache when eviction ran from an inconsistent state. |
| `6fdced26bc`, `24ec1af4e2` (Parquet merge) | Reject unsorted input *before* writing a file. Two row groups with the same prefix key silently overwrote rows. Parquet min == max does not prove alignment when nulls are present (check null_count). |
| `fb6be52f12` | A wrong dedup key silently dropped data. |
| `623b4d0728` | Jitter periodic retention so jobs don't stampede. |

## 14. Recommendations for llmobs spec 03

**§1 WAL**
- *Keep:* group commit with `fdatasync`. Quickwit acks before fsync (5 s window) and lists replication as unfinished, so our durability is stronger. Document the difference.
- *Adopt:* a per-record format/version byte. Our `kind` byte can serve this purpose. Since we have no compatibility to keep, recovery should **refuse** on an unknown kind rather than skip it.
- **Missing from the spec: WAL I/O error handling.** After a failed `write` or `fdatasync`, retrying the fsync is unsafe because the page cache may have dropped the dirty pages. The writer should fail every pending ack, mark itself poisoned, and make ingestion return 503 until restart, which replays the WAL. Quickwit closes the shard, or aborts when the log is poisoned. Also make the writer task immune to cancellation: run the I/O in a dedicated thread or `spawn_blocking` and never `select!` it away.
- *Adopt:* a `wait_for=queryable` style ingest option is unnecessary, because memtable data is queryable right after the ack. An SDK `flush()` that waits for the ack is enough.

**§2 Memtable and flush**
- *Adopt:* make the checkpoint update a **compare-and-set**. Use `UPDATE table_state SET flushed_seq = :new WHERE table_name = :t AND flushed_seq < :new`, which rejects regressions in the same way Quickwit's incompatible-delta check does.
- **Possible issue:** with S3 and "at most one frozen", the flush is serial. A flush writes one file per (project, day) for both spans and traces. With many active projects that is many sequential PUTs every flush window. Upload partitions concurrently within one flush and keep a single SQLite commit. Quickwit's Sequencer exists for exactly this: concurrent uploads, ordered publish.
- **Possible issue: L0 file explosion with many small projects.** Every flush creates 2 files per active (project, day). Quickwit caps partitions at 200 and sends the rest to an "OTHER" split. Consider a per-partition minimum size or age before writing L0, while keeping the rows in the memtable (the WAL already holds them), or accept it and rely on compaction. State the expected file counts.

**§3 Files**
- *Adopt:* store `file_size` and the **footer length** in `data_files` (04). The first read can then fetch the footer with one ranged GET and no HEAD (DataFusion `metadata_size_hint`). Quickwit stores `footer_offsets` and the file size in the metastore for this reason.
- *Adopt:* physical column order with the sort keys first (`trace_id`, `start_time`, then small columns, with `input`/`output`/`attributes` last). This helps any future streaming merge.
- *Adopt:* debug-build re-verification that written files are sorted. Add a row-count invariant per flush and per merge: rows out = rows in − dedup − tombstoned, counted (Quickwit MP-4 and MC-1).

**§4 Catalog**
- *Adopt:* **stage before upload.** Insert the `data_files` row in state `staged` (cheap in SQLite) *before* writing the object, and flip it to `active` in the commit. GC deletes staged rows older than a threshold, together with their objects. This replaces "list the bucket and delete unknown files older than 1 h": the listing is O(all objects), costs money on S3, and its 1 h heuristic can race a slow multipart upload. Keep the listing only as a rare fsck.
- *Matches us:* commit to the catalog first, then swap the in-memory snapshot (`arc-swap`). This is the same as Quickwit's file-backed metastore, including "discard and reload the cache on an uncertain write".

**§5 Compaction**
- **Spec issue: write amplification for small projects.** "Newest L0 older than 10 min" combined with "inputs include any L1 smaller than half the target" means a small project's single growing L1 for the day is rewritten about every 10 min. That is O(n²) bytes over a day, up to ~100× amplification. Quickwit built `ConstWriteAmplification` (same-level-only merges, `max_merge_ops` = 4) for exactly this tenant-size skew. Fix: record `num_merge_ops` (or a generation) per file, merge only files of similar level or size, cap the merges per file, and leave the rest to day close.
- **Spec issue: commit conflicts.** Compaction, the deletion rewriter and retention can all touch the same input. Each commit must be compare-and-swap on input state: `UPDATE … SET obsolete WHERE id IN (…) AND state = 'active'`, and the transaction aborts unless the row count equals the number of inputs, exactly as Quickwit's publish does. The loser's outputs stay `staged` and are GC'd.
- **Memory:** "merge by start_time, then sort each slice by trace_id" materialises a whole output slice of up to 512 MiB *compressed*, which is several GiB decoded for JSON-heavy spans. Bound it (smaller slices, or spill to disk) and state the bound. Quickwit's default merge is still fully in-memory, and their streaming engine was hard to get right (F8/F12/F14 findings).
- *Adopt (for S3 mode):* keep L0 files on local disk until compacted, as Quickwit's `IndexingSplitCache` does, so compaction doesn't re-download them.

**§6 Query path**
- *Validated:* the `?around=` time hint, a 1 h `trace_lookback`, and the two-phase "find IDs, then fetch spans" approach. Quickwit uses the same 1 h `max_trace_duration`. Without a hint it scans 72 h of splits, and it silently truncates at 10K spans.
  - Our derived `traces` partials give trace-level filters (duration, root, cost) that Quickwit cannot offer.
  - Make sure any span cap returns a "truncated" flag rather than truncating silently.
  - Across files, spans need an explicit sort or group by trace_id; Quickwit hit the adjacent-only grouping problem.
- *Adopt later:* a per-file partial-aggregate cache keyed by (file id, plan with the time range clamped to the file). It works because files are immutable, and it fits dashboards and alerts.
- *Adopt later:* hedged retries for slow S3 GETs.
- *Matches us:* a single-flight debouncer on metadata-cache fills, which avoids a thundering herd on the footer cache.

**§7 Deletion**
- *Keep:* query-time tombstones. Quickwit's deletes are invisible until rewrite, which can take up to 48 h for immature splits, so we are better for GDPR.
- **Spec issue:** candidate selection is "files with `min_ingest_time ≤ cut-off`". A rewritten file keeps old ingest times, so it **stays a candidate forever**, and "no candidate files remain" is never true without re-scanning. Adopt Quickwit's **opstamp**:
  - give deletions a monotonically increasing `seq`;
  - store `deletions_applied_seq` on every data file: new flush output gets the current max, merge output gets the **min** of its inputs, and rewriter output gets the seq it applied;
  - candidates are then the files with `deletions_applied_seq < d.seq`.
- **Spec issue: race.** A compaction can read the tombstone snapshot before deletion D exists and commit after the rewriter has declared D applied and removed its tombstone. Rows deleted by D then reappear. The opstamp plus the CAS commit (§5) close this: the output carries the older seq, so D is not "applied" until that file is rewritten.

**§8 Retention:** day-partition drops are simpler than Quickwit's per-split rule and avoid their "merges extend time range" problem. Add jitter to the hourly job.

**§10 Failure handling:** add WAL write/fsync failure (above), "catalog commit failed after objects were uploaded" (the staged rows cover it), and a failpoint-style crash test at each step of flush, compaction, rewrite and GC. Quickwit's failpoints test binary is a good model. TLA+ is optional, but writing the invariants down (Quickwit's MP-* and MC-*) is cheap and helps.
