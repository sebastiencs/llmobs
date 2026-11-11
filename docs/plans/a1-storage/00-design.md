# A1 · Storage engine — design

Top-level design of the storage track (Wave 1, track A1). It settles what specs 03 and 04 leave open or got wrong, fixes the public API of `llmobs-storage`, and cuts the work into milestones. Each milestone then gets its own implementation plan (`01-m1-….md`, …), written just before it starts, from the code that exists by then.

**Status:** accepted. The decisions below are in `docs/mvp-decisions.md` and the specs carry the changes listed in § Spec changes. Nothing is implemented yet.

**Read first:**
- `docs/spec/03-storage.md` and `docs/spec/05-query.md` (the storage engine and the query layer);
- `docs/spec/01-data-model.md` (schemas, trace merge rules), `docs/spec/04-metadata-db.md` § Storage catalog;
- `docs/research/storage/README.md` (what six other systems do, and the gaps G1–G24 this design resolves);
- `AGENTS.md`.

## Decisions

| # | Decision | Source |
|---|---|---|
| D83 | **Storage owns its catalog.** `llmobs-storage` keeps its tables in `<data_dir>/catalog.db`, with its own migrations and its own writer connection (`synchronous=FULL`). The server's `meta.db` keeps deletion requests (audit, cascades) and hands them to storage, idempotently by deletion id. Backups cover both files. | User |
| D84 | **Files stay per `(project, partition day)`.** A flush writes its files in parallel and commits them in one catalog transaction. Compaction's size-ratio rule bounds the number of small files. | User |
| D85 | **Spans with absurd start times are rejected** at normalization: before 2000-01-01, before `ingest_time − retention_days`, or more than 1 day after `ingest_time`. They count in `partial_success.rejected_spans`. | User |
| D86 | **A trace lives in one day.** Spans and trace partials are partitioned by the trace's **anchor day**, the partition day of its first partial, instead of their own start day. The anchor differs from a row's own UTC day by at most one day, so day pruning uses one day of slack. Retention and compaction then treat a trace that crosses midnight as one unit. Revisited in the M3 plan: it may be dropped (§ Trace anchor › Exit ramp). | User |
| D87 | **Durability and commit protocol.** A file is durable (fsynced file and directory on local disk; completed PUT on S3) before any catalog row makes it active. Catalog commits are serialized and compare-and-set on their input files. Obsolete files are deleted only when no read snapshot references them. Details in § Durability and § Commit protocol. | Research G1–G4, G8–G10, G20 |
| D88 | **`traces` files are derived from `spans` files.** Every `traces` file records the `spans` files it was computed from. Whatever replaces a `spans` file (compaction, deletion rewrite) recomputes the `traces` files derived from it. Duplicate spans therefore stop inflating trace totals once their spans are compacted, and deletion never needs separate logic for `traces`. | Research G17 |

## Scope

**A1 owns** `crates/storage/` except `src/filter/` and `src/sql/` (track A2, see § Boundaries). It delivers:
- the WAL, memtable, flush, Parquet writing, `catalog.db`, compaction, deletion, retention, media blob store and GC;
- the DataFusion table providers for `spans`, `traces` and `scores`, and the per-request `SessionContext`;
- crash, concurrency and fault-injection tests, and benchmarks.

**Not A1:**
- OTLP decoding and normalization (B1), including the D85 rejection rule;
- HTTP/gRPC handlers, the live bus and SSE (server, Wave 2);
- the filter DSL and the SQL sandbox (A2);
- the server's deletion API and cascades in `meta.db` (D, Wave 2).

## Boundaries

```
llmobs-server ──append_spans / append_scores──▶ llmobs-storage ──▶ WAL, memtable, files
              ──query(project) → context + providers──▶  providers: spans, traces, scores
              ──register_deletion / deletion_status──▶  tombstones, rewriter
              ──apply_retention(project, keep_from)──▶  partition drops
              ──put_media / get_media──▶            media blobs
llmobs-storage/src/filter, src/sql (A2) ──▶ DataFusion Expr / validated LogicalPlan, consumed by the providers
```

- Storage is **policy-free**: it never reads `meta.db`. Retention days, project lifecycle and deletion scopes come in through API calls.
- The server publishes live events after `append_spans` returns, using the batch it already has. Storage has no pub/sub.
- A2 builds the user-facing views of spec 05 over A1's providers (D89): hex IDs, hidden `project_id`, `sessions`, the hex-to-binary literal rewrite, default exclusions. It also produces DataFusion `Expr`s and validated plans from the DSL and SQL. A1's providers expose the stored schemas (`llmobs-core`) and accept any `Expr` DataFusion pushes down. The contract between them is those schemas and the `ProjectQuery` that `Storage::query` returns. A2 can start right away and test against in-memory tables.

## Public API

The whole public surface of `llmobs-storage`. Names may be adjusted in M1, but no other public items are added without updating this section.

```rust
pub struct Storage { /* private */ }

impl Storage {
    /// Takes an exclusive lock on `data_dir`, runs catalog migrations, replays the WALs,
    /// and starts the background tasks. Returns only after replay: ingestion cannot run
    /// before recovery (G3).
    pub async fn open(config: StorageConfig) -> Result<Storage, OpenError>;

    /// Durable when it returns Ok (per `wal.sync`), and visible to queries.
    /// `batch` uses `llmobs_core` `spans_schema()`; every row has the given project.
    pub async fn append_spans(&self, project: ProjectId, batch: RecordBatch) -> Result<AppendReceipt, AppendError>;
    pub async fn append_scores(&self, project: ProjectId, batch: RecordBatch) -> Result<AppendReceipt, AppendError>;

    /// A `SessionContext` configured for one query (memory cap, parallelism) and the
    /// project's providers (`spans`, `traces`, `scores`), all bound to one read snapshot.
    /// Nothing is registered by name: the query layer registers its views (D89).
    pub fn query(&self, project: ProjectId, options: QueryOptions) -> Result<ProjectQuery, QueryError>;

    /// Idempotent by `deletion.id`. Rows in scope are hidden from every query once this returns.
    pub async fn register_deletion(&self, deletion: Deletion) -> Result<(), DeletionError>;
    pub async fn deletion_status(&self, id: Uuid) -> Result<DeletionStatus, DeletionError>;

    /// Drops every partition of `project` older than `keep_from` (a UTC day), and drops late
    /// rows for those days at flush from then on (G13).
    pub async fn apply_retention(&self, project: ProjectId, keep_from: NaiveDate) -> Result<RetentionReport, StorageError>;
    /// Hides the project at once, then deletes its files, media and catalog rows.
    pub async fn drop_project(&self, project: ProjectId) -> Result<(), StorageError>;

    pub async fn put_media(&self, project: ProjectId, media: MediaBlob) -> Result<(), StorageError>;
    pub async fn get_media(&self, project: ProjectId, sha256: &Sha256, range: Option<Range<u64>>) -> Result<MediaRead, StorageError>;

    /// Disk space, WAL state and backlog, for `/healthz` and metrics.
    pub fn health(&self) -> StorageHealth;
    /// Flushes both memtables and waits for the background tasks to stop.
    pub async fn shutdown(self) -> Result<(), StorageError>;
}
```

- `StorageConfig` is the storage section of the server config (D59). It is `serde` with `deny_unknown_fields`, and its defaults are the values in spec 03. Its fields are listed in § Configuration.
- `AppendReceipt { seq, rows }`.
- Errors are `thiserror` enums, one per operation: `OpenError`, `AppendError`, `QueryError`, `DeletionError`, and `StorageError` for the rest. `AppendError` separates the cases the server maps to HTTP:
  - `Backpressure` (429);
  - `DiskFull` (503);
  - `WalFailed` (503 and unhealthy, G4);
  - `ShuttingDown` (503);
  - `InvalidBatch` (500, a bug in the caller).

  No error message contains row data.

## Crate layout

```
crates/storage/src/
  lib.rs            Storage, the public API above
  config.rs         StorageConfig and defaults
  error.rs          error enums
  wal/              segment files, record codec, writer task (group commit), replay
  memtable.rs       active/frozen per table, partitioning, byte accounting, coalescing
  anchor.rs         trace anchor days near midnight (D86)
  flush.rs          frozen memtable → files → catalog edit
  objects.rs        object store access: durable local writes (G1), S3 via object_store, parallel uploads
  parquet.rs        writer properties per table (spec 03 §3), footer/statistics capture
  partials.rs       trace partials from span batches (spec 01 merge rules, G18 ordering)
  catalog/          catalog.db migrations, rows, in-memory catalog, committer, edits
  snapshot.rs       ReadState: catalog + memtable + tombstones, published atomically
  query/            SessionContext setup, SpansProvider, TracesProvider, ScoresProvider,
                    TombstoneFilterExec, memory pool and admission, metadata and S3 read caches
  compaction.rs     L0 merge, day close, size-ratio rule, traces rebuild
  deletion.rs       tombstones, rewriter, deletion sequence
  retention.rs      partition drops, dropped-day watermark
  media.rs          media blobs and their GC
  gc.rs             obsolete and staged file removal
  filter/  sql/     track A2
```

The milestone plans may merge or split modules when the code shows a better cut. AGENTS.md § No unnecessary abstractions applies: no traits except where two real implementations exist.

## Concurrency model

- **One WAL writer task per table.** Appenders send `(batch, ack)` over a bounded channel. Per group, the writer:
  1. drains the channel (spec 03 §1 limits);
  2. writes the records and fsyncs;
  3. inserts the batches into the active memtable in sequence order;
  4. publishes a new `ReadState`;
  5. completes the acks.

  A single writer means memtable order equals WAL order, and an acknowledged write is visible to the next query.
- **One flush task per table**, triggered by size, age, shutdown or a full hard limit. It freezes `active` under the table lock and writes the frozen batches without holding the lock.
- **One committer.** Every change to the set of files (flush, compaction, deletion rewrite, retention, project drop, GC) is a `CatalogEdit` applied by the committer. It runs the SQLite transaction, checks the edit's preconditions, and publishes the new `ReadState`, all under one mutex, so commits and snapshots are totally ordered (G8).
- **Background jobs** (compaction, deletion rewriter, retention, GC, media GC) are tokio tasks with bounded parallelism. CPU-heavy work (sorting, Parquet encoding) runs on `spawn_blocking` or a dedicated pool, never on the async workers. A job **claims** its input files in memory before reading them, so compaction and the rewriter never pick the same file. Claims don't survive a restart and need not: an unfinished job is simply redone.
- **Readers** load the current `ReadState` (an `Arc` behind a `std::sync::RwLock`, locked only to clone the `Arc`): an `Arc` of the in-memory catalog, the memtable batches (`active` and `frozen`, as `Arc<RecordBatch>` lists) and the tombstone sets. All three come from the same publish, so a row is seen exactly once: either in a memtable or in a file, never both, never neither (G7).

## Durability

Invariants the tests check (§ Testing):

1. **Acknowledged means durable.** With `wal.sync = "always"`, an acknowledged batch survives a crash and a power loss. With a duration (`"5ms"`), it survives a crash; a power loss can lose the last interval.
2. **A file is durable before it is active.**
   - Local disk: `objects.rs` writes to a temp file in the target directory, calls `sync_all`, renames it into place and fsyncs the directory. `object_store` 0.13's `LocalFileSystem` does none of this (checked); it is used for reads only, until the family moves to an object_store version with fsync.
   - S3: the PUT or multipart upload has completed.

   An upload that fails ambiguously (error, but the object may exist) is treated as failed. The file is retried under a new name, and the old name is left to GC.
3. **Staged before written.** A flush or rewrite first records its output paths in `staged_objects` in a small transaction, then writes the objects, then inserts their `data_files` rows and removes the `staged_objects` rows in the commit. A crash leaves only `staged_objects` rows. GC deletes those (and their objects, if any) after 1 h, so the bucket is never listed (G10). On local disk, startup also removes leftover temp files.
4. **The WAL checkpoint advances only with its files.** `flushed_seq` advances only in the transaction that activates the files holding those rows. It never moves backwards: the update is compare-and-set. `catalog.db` uses `synchronous=FULL`, and WAL segments are deleted only after that transaction has returned (G2).
5. **Sequence numbers never repeat.** After restart, the next seq is `max(last valid WAL seq, flushed_seq) + 1` (G3).
6. **WAL I/O errors are final.** A failed write or fsync fails every pending ack with `WalFailed` and stops the writer. There is no fsync retry, because the kernel may have dropped the dirty pages. `health()` reports it and the operator restarts the process. The write-and-sync step runs on a blocking thread and is not cancellable (G4).
7. **WAL records are compressed.** Arrow IPC with LZ4-frame body compression (G5). The record framing of spec 03 §1 is unchanged. The `wal/<table>/` directory is fsynced when a segment is created or deleted.
8. **One process per data directory**, enforced with an exclusive `flock` on `<data_dir>/LOCK` (spec 12).

## Commit protocol

A `CatalogEdit` contains:
- `add`: new file rows, whose paths must be in `staged_objects`;
- `remove`: input files with their expected state, normally `active`;
- optionally, the new `flushed_seq` for a table (compare-and-set: it must be greater than the current value);
- optionally, deletion progress (§ Deletion).

The committer applies it only if every precondition holds. Otherwise it rolls back and reports a conflict; the job drops its outputs (they stay in `staged_objects` for GC) and retries later.

**Removing a file** sets it `obsolete` and moves its `FileMeta` to a retired list. GC deletes the object once two conditions hold (G9):
- no `ReadState` still holds it (the `Arc<FileMeta>` count shows this);
- `gc.grace` has passed, which also covers a restart, when the counts are lost. `gc.grace` defaults to 5 min and must exceed the longest query timeout.

## Trace anchor (D86)

A trace is anchored to the partition day of its **first partial**, the first time a flush writes rows for it. Later rows of the same trace go to that day if their own UTC day is within one day of it. Otherwise they go to their own day, as the fallback.

Only traces near midnight need remembering. A row whose own day equals its trace's anchor day needs no lookup. The anchor matters only for a trace whose rows straddle a day boundary, and with `trace_lookback` = 1 h those rows start within an hour of midnight. So `anchor.rs` keeps:
- a map `trace_id → anchor day` for every partial whose rows start within `anchor.window` (default = `trace_lookback`, 1 h) of a day boundary, on either side;
- entries are dropped `2 × anchor.window` after the boundary.

That is about 1/12 of a day's traces, held for about 2 h. At startup the map is rebuilt from:
- the WAL replay;
- the `trace_id` column of the `traces` files whose time range touches the current boundary window.

Rows in a partition whose day differs from their own start day are normal rows. Files still record their real `min_time`/`max_time`. Day pruning (spec 03 §6 step 1) widens the day range by one day on each side, and file pruning by `min_time`/`max_time` stays exact.

The partition key is `(project_id, anchor day)` for `spans` and `traces`; `scores` keep the day of `created_at`.

### Exit ramp

The anchor is the decision most worth dropping if it turns out costly. It buys two things:
- retention removes a trace that crosses midnight as a whole;
- day close produces one fully merged row per such trace.

It costs a structure rebuilt at startup, partition days that differ from row dates, and one day of pruning slack.

**Without it**, partials stay in the day of their own start time. The read path already merges across days through the padded window, so query results are the same. Only two things change:
- retention may remove the early spans of a trace that crossed midnight at the retention cut-off;
- a crossing trace keeps two partials after day close.

Both would be documented limits.

**To keep the exit cheap:**
- all anchor logic stays in `anchor.rs`;
- the flush asks it for one partition day per trace;
- nothing else (catalog, compaction, deletion) knows anchors exist, apart from the one day of slack in day pruning.

The M3 plan decides, with the S1 results and the cost of the anchor's startup rebuild in hand. If the anchor is dropped, D86 is reversed in `docs/mvp-decisions.md`, with the reason.

## Trace partials and the `traces` view

- **Partials come from spans** (D88). A flush computes one partial per `(trace, file)` from the span rows it writes. Each `traces` file records its source `spans` files in `file_sources`. Compaction and the rewriter replace a set of `spans` files together with every `traces` file derived from them, and recompute those partials from the new `spans` files.
- **Day close** rebuilds the partition's `traces` from all of its `spans` files: one partial per trace (anchored traces make this the whole trace), `fully_merged = true`.
- **Deterministic merges.** "First non-null" and root selection order by `(start_time, span_id)` in partials and at merge time (G18). Spec 01's merge table gets this rule.
- **Duplicates.** A flush drops duplicate `(trace_id, span_id)` rows within the frozen memtable, keeping the greatest `ingest_time`. Duplicates across flushes remain until compaction merges the files. Until then the read-time merge counts them twice. This is documented: it is bounded by the L0 lifetime, about 10 min.
- **Window.** Partials are scanned on `[a − trace_lookback, b + trace_lookback]`, merged, and the merged `start_time` is filtered to `[a, b]` (G15).
- **Pushdown.** Only `start_time` ranges and `trace_id` equality / `IN` are pushed into the partial scan. Every other filter applies after the merge (G16). The "latest N" fast path (spec 03 §6) stays, with the same post-merge filter.
- **Memtable partials** are computed on the fly from all memtable span batches of the project whose time range overlaps the padded window, not only those inside it.

## Trace-list spike (S1)

The `traces` view is the least proven part of this design. Laminar merges partials on read as we plan to; Phoenix, Opik and Langfuse aggregate from spans instead, and Langfuse dropped partials. It is also the UI's main path. S1 measures it before M3 builds on it.

The detailed plan is `02-s1-trace-list-benchmark.md`. It adds a one-day query (Q6), an informational "wrong pushdown" variant, and a measurement of the anchor's startup cost to what follows.

**What it is:** a throwaway prototype, run alongside M1, by one agent. It lives on its own branch and is **never merged**. Only the results document is. It uses DataFusion 55 directly, not `llmobs-storage`.

**Data:** synthetic, written straight to Parquet with spec 03 §3's settings:
- one project, 7 days, 5 million traces with 10 spans each;
- `traces` partials for 40% of the traces split across 2–3 partials (late spans);
- 2 minutes of the newest data held as in-memory batches instead of files.

**Queries**, each run cold (fresh process, page cache dropped where possible) and warm:

| # | Query | Proposed target (warm, p95, 8 cores, NVMe) |
|---|---|---|
| Q1 | Latest 50 traces, no filter | 200 ms |
| Q2 | Latest 50 traces with `cost_total > x` (post-merge filter), 7 days | 2 s |
| Q3 | Latest 50 traces with `environment = 'production'` | 500 ms |
| Q4 | One trace by `trace_id`, with an `around` hint / without | 100 ms / 1 s |
| Q5 | Hourly count and cost sum over 7 days (dashboard series) | 2 s |

**Variants:**
- (a) a plain SQL `GROUP BY trace_id` over a union of the partial files and the memory batches;
- (b) (a) plus the widening-window fast path for Q1–Q3;
- (c) (b) with 80% of the days already fully merged (day close done), to show what compaction buys.

**Outcome:** `docs/research/storage/traces-benchmark.md` with the numbers, the plans DataFusion chose, and one of three conclusions:
- the design holds as written;
- the design holds with a custom merge operator (GreptimeDB's route) for the merge step;
- partials don't pay off, and the `traces` view aggregates from `spans` at read time, with M3 replanned around that.

The M3 plan is written only after this.

## Compaction

Spec 03 §5, with these changes:
- **Size ratio** (G12). An L0 merge always takes every L0 file of the partition. It includes an L1 file only if the output is at least `compaction.size_ratio` (default 4) times that L1 file. A partition with one big L1 file and a trickle of L0 files therefore produces a second L1 file instead of rewriting the big one every 10 minutes.
- **Day close** of `spans` merges all files of the partition when their total is at most `compaction.target_file_size`. Above that, it merges only the L0 files.
- **Memory bound.** Each merge streams its inputs in `(trace_id, start_time)` order: the inputs are sorted runs, so a k-way merge needs one batch per input. Slice boundaries come from the file time ranges and a `start_time` histogram, not from sorting the whole slice. Merge memory is capped by `compaction.max_memory` (default 1 GiB) across running merges.
- **Disk reservation.** Before a merge starts, it reserves its estimated output size (the sum of its input sizes) from free disk, local mode only. A merge that doesn't fit waits. Flushes don't reserve (VictoriaLogs' rule).
- **Late data** re-opens a closed day. A new L0 file in a day already closed triggers another day close after `compaction.close_delay` (G13).
- **Outputs** apply every tombstone known when the job started, and record that deletion sequence (§ Deletion).

## Deletion

Spec 03 §7, with the bookkeeping that makes it terminate (G20):
- Every registered deletion gets a **deletion sequence number** (`deletion_seq`, increasing, in `catalog.db`).
- Every file records `deletions_applied_seq`: no row of any deletion up to that number remains in it.
  - A flush sets it to the highest `deletion_seq` whose tombstones it applied.
  - Compaction and the rewriter set it to the highest `deletion_seq` known when the job started, since they apply all of those.
- A deletion is **applied** when two conditions hold:
  - every active file of the project with `min_ingest_time ≤ cut-off` has `deletions_applied_seq ≥` its seq;
  - no memtable batch older than the cut-off remains.

  Then its tombstones are dropped from memory, and storage's row is marked applied.
- `register_deletion` persists the request and its resolved scope (trace IDs, users, sessions) in `catalog.db`, then publishes the tombstones in a new `ReadState`. It returns only after both. `deletion_status` reads storage's row.
- **Server side** (Wave 2, documented here so the boundary is clear):
  1. it inserts the request into `meta.db` with the cascades;
  2. it calls `register_deletion`;
  3. it marks its row handed over.

  At startup it re-registers every row that wasn't handed over; storage ignores ids it already knows. It polls `deletion_status` to set `applied`.

## Retention and project drop

- `apply_retention(project, keep_from)` commits one edit that obsoletes every file of the project in days before `keep_from`, and records `keep_from` in `retention_watermarks`. From then on, a flush drops rows of that project older than the watermark (G13). The server calls it hourly with `today − retention_days`.
- `drop_project` marks the project dropped in `catalog.db` and removes it from every new `ReadState` in the same commit. The query provider returns empty tables for it. A background task then deletes its objects under `project=` and `media/<project_id>/` and its rows.

## Catalog (`catalog.db`, D83)

Tables (migrations owned by `llmobs-storage`, `sqlx::migrate!` like the server's). The full schema is in spec 03 §4.

| Table | Content |
|---|---|
| `data_files` | The former spec 04 columns, plus `deletions_applied_seq` and `footer_size` (so a file opens with one ranged read and no HEAD, G10). `state` ∈ `active` · `obsolete` · `quarantined`. |
| `staged_objects` | `(path, created_at)`: outputs being written (§ Durability item 3). |
| `file_sources` | `(traces_file_id, spans_file_id)`, D88. |
| `table_state` | `(table_name, flushed_seq)`. |
| `deletions` | Storage's view: id, project, kind, cut-off, `deletion_seq`, status, applied_at. |
| `deletion_traces`, `deletion_users`, `deletion_sessions` | The resolved scope. |
| `retention_watermarks` | `(project_id, keep_from)`. |
| `projects` | `(project_id, state)` with `dropping` for project drops in progress. |
| `media` | Spec 04's `media` table, moved here: storage owns the blobs and their index. |

`meta.db` keeps its own `deletions` and `deletion_*` tables as the audit trail and cascade record (spec 04), minus everything listed above.

## Query resources

- One process-wide DataFusion memory pool of `query.max_memory_total` (default 50% of RAM, at most 8 GiB), with each query capped at `query.max_memory` (default 2 GiB), spilling to `<data_dir>/tmp` (G21).
- **Admission.** A query that can't reserve its initial memory waits up to `query.admission_wait` (default 5 s), then fails with a "busy" error that the server maps to 503.
- **Metadata cache.** M2 first checks whether DataFusion 55's built-in file metadata cache covers footers and page indexes (G22). The custom `ParquetFileReaderFactory` of spec 03 §4 is written only for what is missing (bloom filters are the likely gap).
- **S3 read cache** (M6). In S3 mode every page read is a ranged GET: tens of milliseconds each, and charged per request. Parseable (hot tier), OpenObserve (disk cache) and GreptimeDB (read and write caches) all keep data locally for that reason.
  - A bounded on-disk cache under `<data_dir>/cache/` holds byte ranges of data files, keyed by file id and range.
  - Files are immutable, so entries never go stale. Eviction is LRU by bytes (`query.read_cache_bytes`, default 10% of the disk, at most 100 GiB). Entries of obsolete files are dropped when GC deletes the file.
  - A flush or compaction in S3 mode also writes its fresh output into the cache, since recent data is what gets read.
  - In local mode the cache is off: the files are already local.
  - It sits in the same reader factory as the metadata cache, so the providers don't see it.
  - Until M6 lands, S3 mode is correct but not fast. That is a documented MVP limit if M6 slips.

## Configuration

`StorageConfig`, the `[storage]` section (D90). Sizes are strings with a binary unit (`"256MiB"`), durations strings with a unit (`"120s"`). Defaults are spec 03's unless stated here. Each milestone adds the fields it uses, and no earlier (AGENTS.md § No placeholders).

- top level: `data_dir`, `object_store` (`local` or `s3://bucket/prefix`; credentials from the standard AWS environment), `min_free_bytes`;
- `wal`: `sync` (`"always"` or a duration), `segment_bytes`, `group_max_bytes`, `group_max_wait`, `skip_corrupt`;
- `flush`: `max_bytes`, `max_age`, `upload_concurrency` (default 8, D84);
- `memtable`: `hard_limit`;
- `compaction`: `threads`, `target_file_size`, `l0_max_files`, `l0_max_bytes`, `l0_max_age`, `close_delay`, `size_ratio`, `max_memory`;
- `query`: `max_memory_total`, `max_memory`, `target_partitions`, `metadata_cache_bytes`, `read_cache_bytes` (S3 mode only), `trace_lookback`, `admission_wait`;
- `gc.grace`, `deletion.max_delay`, `anchor.window`.

## Testing

AGENTS.md requires crash-recovery and concurrency tests for storage. The tools, built in M1 and M2 and used by every milestone:

- **Crash harness.** An integration test re-executes its own test binary as a child process that ingests a deterministic stream and records acknowledged seqs. The parent `kill -9`s the child at random points, reopens the storage and checks the invariants:
  - every acknowledged row is present;
  - no row appears twice once compacted;
  - every catalog row's object exists;
  - `flushed_seq` never decreases.

  M1 checks the WAL invariants that exist then: every acknowledged batch is present with its row count, every stored batch is complete or absent, no `span_id` appears twice, and sequence numbers increase across runs and never repeat. The catalog, `flushed_seq` and post-compaction checks arrive with M2 and M4.

  Runs a fixed number of iterations in `just test` and many more in a manual soak mode.
- **Pause points.** Test-only hooks (`#[cfg(test)]`, inside the crate) that block a task at a named step (after the WAL write, after the Parquet write, before the commit, after the commit, before WAL deletion). They make race tests deterministic: a query during a flush, a compaction racing a deletion, a retention racing a flush.
- **Fault-injecting object store.** A wrapper implementing `object_store::ObjectStore` around the real one that fails, delays, or reports failure after the write succeeded (GreptimeDB's ambiguous-upload case). Two real implementations of an external trait, so allowed.
- **Durability ordering.** A test-only recorder of fsync/rename/commit calls asserts the order of § Durability items 2–4 for flush and compaction.
- **Model test.** A randomized sequence of append, flush, compact, delete, retention, restart and query operations, checked against a simple in-memory model of the expected rows (Quickwit's "rows in = rows out").
- **Benchmarks** (M6): `criterion` micro-benchmarks in `crates/storage/benches`, and an end-to-end load generator in `tools/bench-storage` (not shipped) that reports ingest throughput, flush and compaction rates, and query latency against spec 03's capacity numbers.

## Milestones

Each milestone ends merged, with `just check` green, and gets its own implementation plan of small sub-phases.

| # | Milestone | Exit criteria | Depends on |
|---|---|---|---|
| S1 | **Trace-list benchmark (spike).** Throwaway prototype, run alongside M1, see § Trace-list spike. | Results in `docs/research/storage/traces-benchmark.md` and a go / change decision on the `traces` design, before the M3 plan is written. | — |
| M1 | **Walking skeleton.** `Storage::open`/`append_*`/`query`/`shutdown`; WAL (records, group commit, segments, replay, error poisoning); memtable without flush; `SpansProvider` and `ScoresProvider` over the memtable; crash harness and pause points. | Appended spans are queryable through `Storage::query`. Kill -9 at any point loses no acknowledged row. The WAL failure tests pass. | — |
| M2 | **Flush and catalog.** `catalog.db` with migrations and the committer; durable local writes and S3 uploads; Parquet writer settings; flush with staged rows, `flushed_seq` and WAL truncation; providers over files plus memtable; metadata cache; staged-row GC. | Data survives restart after flush. Crash tests cover every flush step. A query during a flush sees each row once (pause-point test). Works against local disk and an S3-compatible store (MinIO in a test, if available). | M1 |
| M3 | **Traces.** Partials, `file_sources`, the anchor (D86, unless dropped: § Trace anchor › Exit ramp), `TracesProvider` (window, pushdown rule, latest-N fast path, single-trace lookup with bloom filters and `?around=`), duplicate drop at flush. | Golden tests on the traces view: late partials, midnight crossing, duplicates, filters that only pass after the merge. The S1 latency targets hold on the real implementation. | M2, S1 |
| M4 | **Compaction and GC.** L0 merge with the size ratio, day close, streaming merge with a memory cap, disk reservation, traces rebuild from spans, obsolete-file GC with reference counts. | Write amplification measured on a synthetic day stays bounded. Crash and race tests (compaction vs query, vs flush). | M3 (traces rebuild); spans/scores parts could start after M2 |
| M5 | **Deletion, retention, project drop, media.** Tombstones and `TombstoneFilterExec`, `deletion_seq` bookkeeping, rewriter, `apply_retention`, `drop_project`, media blobs and GC. | Deleted rows vanish from queries at once and from files within `deletion.max_delay`. The race test "compaction started before a deletion commits after it" keeps the rows gone. | M2 (deletion of spans/scores); M3 for traces |
| M6 | **Resources and hardening.** Process-wide memory pool and admission, disk-full behaviour, quarantine of corrupt files, the S3 read cache (§ Query resources), benchmarks. | Benchmarks recorded against spec 03's capacity check, on local disk and on S3 with a warm and a cold cache. Disk-full and corrupt-file tests. | M2 |

**What can run in parallel:**
- A2 runs alongside the whole of A1.
- S1 runs alongside M1 (it touches no A1 module).
- After M2, three lanes can run at once on separate modules:
  - M3 (traces);
  - M5's deletion and retention for `spans` and `scores`;
  - M6.
- M4 comes after M3, because compaction has to rebuild `traces`. The `traces` parts of M5 come after M3.
- Each lane only edits its own modules. Changes to the committer or to `ReadState` go through one lane at a time.

## Spec changes

**Applied** (together with this design): the specs below and the decision log now describe this design, so implementers follow the specs as usual. What changed:

- **Spec 03:**
  - § Layout: add `catalog.db` and `LOCK`.
  - §1: LZ4 IPC bodies, WAL error handling, fsync of the WAL directory, seq after restart.
  - §2: flush uses staged rows, durable writes and parallel uploads; duplicates dropped within a flush.
  - §3: partition key is the anchor day.
  - §4: `catalog.db`, the committer, compare-and-set edits, reference-counted GC, staged rows instead of listing the bucket.
  - §5: size ratio, streaming merge and memory cap, disk reservation, late data re-opening a day, traces rebuilt from spans.
  - §6: window padded on both sides, pushdown rule, process-wide memory pool and admission, metadata cache check, the S3 read cache.
  - § Layout: `cache/` (S3 mode).
  - §7: `deletion_seq` and `deletions_applied_seq`, the handoff from the server.
  - §8: retention watermark.
  - §10: WAL failure, ambiguous uploads.
  - New § Trace anchor.
- **Spec 04:** § Storage catalog moves to spec 03 (it is `catalog.db` now); `meta.db` keeps `deletions` / `deletion_*` as the audit record, with a `handed_over_at` column.
- **Spec 01:**
  - partition key = anchor day for `spans` and `traces`;
  - the `(start_time, span_id)` ordering in the trace merge table;
  - `traces` derived from `spans` files.
- **Spec 02:** rule 15 gains the D85 start-time bounds (B1 implements them).
- **Spec 12:**
  - § Backups: back up `catalog.db` together with `meta.db`, in the same snapshot; restore uses `llmobs admin gc-orphans`, since GC no longer lists the bucket;
  - § Process lifecycle: storage opens (migrations, replay) before ingestion;
  - § Upgrades: a pre-migration copy of `catalog.db`.
- **Spec 09 § CLI:** `llmobs admin gc-orphans`.
- **Spec 02 § Media extraction:** blobs are stored through `llmobs-storage`.
- **Spec README:** the architecture diagram shows `catalog.db`.
- **`docs/mvp-decisions.md`:** D83–D88.
- **AGENTS.md § Layout:** the storage line mentions `catalog.db`.

## Open questions for the milestone plans

These are left to the plan that needs them, because the answer depends on code or measurements:
- Does DataFusion 55's file metadata cache include bloom filters? (M2)
- Is MinIO available for an S3 integration test in CI, or does S3 coverage rely on the fault-injecting wrapper around `InMemory`? (M2)
- What is the exact `start_time` histogram used to cut compaction slices? (M4)
