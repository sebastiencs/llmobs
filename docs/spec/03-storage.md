# 03 — Storage engine

The columnar store holds three tables: `spans`, `traces` (derived partial summaries) and `scores` (01). It is a small LSM-style engine: **WAL → memtable → L0 Parquet files → L1 Parquet files**, with a catalog in its own SQLite file and DataFusion as the query engine.

Configuration keys named in this spec (`wal.sync`, `flush.max_bytes`, …) live under the `[storage]` section of the server config (12, D90).

`llmobs-storage` is policy-free: it never reads `meta.db`. Retention days, project lifecycle and deletion scopes reach it through its API (D83). The design behind this spec, with its public API and milestones, is `docs/plans/a1-storage/00-design.md`; the research behind it is `docs/research/storage/`.

## Layout on disk / object store

```
<data_dir>/
  LOCK                    exclusive flock: one process per data directory (12)
  meta.db                 SQLite, server metadata (04)
  catalog.db              SQLite, storage catalog (§4), synchronous=FULL
  wal/
    spans/000000000042.wal
    scores/000000000007.wal
  tmp/                    query spill files
  cache/                  S3 read cache (§6), only with an S3 object store
  objects/                only when object store = local
    tables/<table>/project=<project_id>/day=<YYYY-MM-DD>/<level>-<uuidv7>.parquet
    media/<project_id>/<sha256>
```

With `storage.object_store = s3://bucket/prefix`, the `objects/` tree lives in the bucket; `catalog.db`, `meta.db` and `wal/` stay on local disk, which **must be persistent** (an ephemeral disk loses up to `flush.max_age` of acknowledged data on machine loss — documented).

**Partition key:** `(project_id, day)`.
- For `spans` and `traces`, `day` is the trace's **anchor day** (§ Trace anchor). It equals the UTC date of the row's own `start_time` or differs from it by at most one day.
- For `scores`, `day` is the UTC date of `created_at`.

A late span lands in its trace's anchor partition, creating a new L0 file there; that is fine.

Spans are expected to carry plausible start times. Normalization rejects spans that start before 2000-01-01, before `ingest_time − retention_days`, or more than one day after `ingest_time` (02 rule 15, D85).

## Trace anchor

A trace is anchored to the partition day of its **first partial**: the first time a flush writes rows for it, they go to the UTC day of their `start_time`. Later rows of the same trace go to that anchor day if their own UTC day is within one day of it; otherwise they go to their own day. Retention and compaction therefore handle a trace that crosses midnight as one unit (D86).

Only traces near midnight need remembering. The storage keeps a map `trace_id → anchor day` for every partial whose rows start within `anchor.window` (default = `trace_lookback`, 1 h) of a UTC day boundary, on either side, and drops entries `2 × anchor.window` after the boundary. At startup the map is rebuilt from the WAL replay and from the `trace_id` column of the `traces` files whose time range touches the current boundary window.

Because rows may sit in the day before or after their own date, day pruning (§6) widens the day range by one day on each side. File pruning by `min_time`/`max_time`, which hold the real times, stays exact.

## 1. Write-ahead log

One WAL per table (`spans`, `scores`), each a sequence of segment files (max 64 MiB, then rotate).

Record format. Every integer is little-endian. The checksum covers `seq` as 8 little-endian bytes, then `kind` as 1 byte, then the payload. A payload longer than 256 MiB is rejected.

```
u32  payload_len
u32  crc32c(seq || kind || payload)
u64  seq            monotonically increasing per table
u8   kind           1 = row batch; any other value, including the reserved 2, is invalid
[..] payload        Arrow IPC stream (one RecordBatch, table schema, already normalized),
                    with LZ4-frame body compression
```

**Group commit.** Ingest tasks send `(RecordBatch, oneshot ack)` to a single WAL writer task per table over a bounded channel. The writer:
1. drains everything available (up to 8 MiB or 2 ms after the first item);
2. writes all records with one `write_all` and calls `fdatasync`;
3. inserts the batches into the active memtable in `seq` order and publishes a new read state (§4);
4. completes every ack.

This gives one fsync per group, so throughput is bounded by disk bandwidth, not fsync latency. Because one task both writes the WAL and fills the memtable, memtable order equals WAL order, and an acknowledged write is visible to the next query. Config `wal.sync = "always" | "<duration>"` (default `"always"`). A duration such as `"5ms"` acknowledges after the write and fsyncs at most that often; it trades durability for speed and is documented: a power loss can lose the last interval, a process crash loses nothing.

The `wal/<table>/` directory is fsynced when a segment is created or deleted.

**Write errors are final.** If a write or `fdatasync` fails, the writer fails every pending ack (`WalFailed`), refuses further writes and reports the storage unhealthy; the operator restarts the process. The failed fsync is never retried, because the kernel may already have dropped the dirty pages. The write-and-sync step runs on a blocking thread and cannot be cancelled halfway.

**Checkpoint.** The catalog stores `flushed_seq` per table (highest WAL seq whose rows are in Parquet files). Segments whose max seq ≤ `flushed_seq` are deleted, and only after the catalog transaction that advanced `flushed_seq` has returned (§2).

**Recovery.** At startup, before ingestion opens, for each table:
- read segments in order, skip records with seq ≤ `flushed_seq`, and insert the rest into the memtable;
- a record with a bad CRC or short read at the **tail** of the last segment is a torn write: truncate the segment there;
- a bad record anywhere else is corruption: refuse to start unless `--wal-skip-corrupt`;
- the next seq is `max(last valid record's seq, flushed_seq) + 1`, so a sequence number is never reused, even when `every_ms` lost a tail.

## 2. Memtable

Per table: `active` (accepting inserts) and at most one `frozen` (being flushed). Each is a map `partition → Vec<RecordBatch>` plus byte accounting. Inserts append the WAL'd batch (split by partition) — no row-by-row building, so inserts are cheap.

- **Accounting** uses the Arrow buffers' real memory size (`get_array_memory_size`), not the encoded size: small OTLP batches carry a large fixed overhead per column.
- **Coalescing:** when a partition holds more than 64 batches smaller than 1 MiB, they are concatenated into one batch.

**Flush triggers:** active size ≥ `flush.max_bytes` (default 256 MiB), oldest row age ≥ `flush.max_age` (default 120 s), or shutdown.

**Backpressure:** if `active + frozen` exceeds `memtable.hard_limit` (default 1 GiB) ingestion answers `429 Retry-After: 1` / `RESOURCE_EXHAUSTED`.

### Flush

1. **Freeze.** Under the table-state write lock: move `active` → `frozen`, create a new empty `active`, record `frozen_max_seq`.
2. **Prepare, outside the lock,** per partition of `frozen`:
   - drop rows hidden by tombstones (§7) and rows older than the project's retention watermark (§8);
   - drop duplicate `(trace_id, span_id)` rows, keeping the greatest `ingest_time` (spans);
   - sort (spans: `trace_id, start_time`; scores: `created_at`);
   - for `spans`, also compute **trace partials** (`GROUP BY trace_id` with the merge rules of 01) for a `traces` file derived from that spans file (§3).
3. **Stage.** One small catalog transaction records every output path in `staged_objects`.
4. **Write.** Write the files, up to `flush.upload_concurrency` (default 8) at a time. Each file is durable before the next step:
   - local disk: write to a temp file in the target directory, `sync_all`, rename, fsync the directory;
   - S3: the PUT or multipart upload has completed.

   An upload that fails ambiguously (error, but the object may exist) is retried under a new name; the old name stays staged and GC removes it.
5. **Commit.** One catalog transaction, through the committer (§4):
   - insert the new `data_files` rows as `active`, and the `file_sources` rows;
   - remove the `staged_objects` rows;
   - set `flushed_seq = frozen_max_seq`, compare-and-set (it must be greater than the current value).

   The same commit publishes a read state with the new files and without `frozen`.
6. **Truncate.** Delete the WAL segments whose max seq ≤ `flushed_seq`.

Step 5 makes the switch atomic for readers: a query snapshot sees either (frozen, without the new files) or (the new files, without frozen), never both or neither.

## 3. Parquet files

| Setting | Value |
|---|---|
| Compression | zstd level 3 |
| Row group | closed when in-progress size ≥ 64 MiB or 100 000 rows (manual `flush()` on `ArrowWriter`) |
| Data page | 1 MiB, max 20 000 rows/page; page index on |
| Statistics | page-level for all scalar columns |
| Dictionary | on for dictionary-typed columns; off for `input`, `output`, `attributes` |
| Bloom filters | `trace_id`, `session_id`, `user_id` (fpp 0.01) on `spans`; `trace_id`, `session_id`, `user_id` on `traces`; `trace_id`, `score_id` on `scores` |
| Sort order (within a file) | `spans`: `trace_id, start_time` · `traces`: `start_time` · `scores`: `created_at` (declared in file metadata so DataFusion can exploit it; declared only because the writer sorted) |
| Time slicing (across files) | every file covers a contiguous `[min_time, max_time]` slice; see below |
| KV metadata | `llmobs.table`, `llmobs.schema_version`, `llmobs.level`, `llmobs.min_time`, `llmobs.max_time`, `llmobs.min_ingest_time`, `llmobs.max_ingest_time` |

**Time-sliced files, trace-sorted rows** (same pattern as Opik, LangWatch and Laminar, which partition by time and sort by trace ID inside the partition):

- *Across files*, data is organized by time: L0 files cover one flush window; compaction merges inputs in time order and cuts outputs at time boundaries, so each output file covers a contiguous, non-overlapping time slice. Time-range scans (metrics, alerts, span search) prune at file granularity using `min_time`/`max_time`.
- *Within a file*, spans are sorted by `trace_id, start_time`: opening a trace reads one contiguous run of rows (usually one page) per file instead of rows scattered over many row groups.
- The time-ordered access path for lists is the `traces` table (sorted by `start_time`), the equivalent of Laminar's time projection / LangWatch's `trace_analytics`.

**`traces` files are derived from `spans` files** (D88). Every `traces` file is computed from a set of `spans` files of the same partition, recorded in `file_sources`. The sets of the active `traces` files are disjoint and together cover every active `spans` file of the partition. Whatever replaces `spans` files (compaction, deletion rewrite, retention) replaces the `traces` files derived from them in the same commit, recomputing their partials from the new `spans` files. Duplicate spans therefore stop counting twice once their spans files are merged, and `traces` needs no deletion logic of its own.

**Schema evolution:** only adding nullable columns. Each file carries its own schema; the reader uses DataFusion's schema adapter to fill missing columns with nulls. Renames or type changes need a new column.

## 4. Catalog

`catalog.db` belongs to `llmobs-storage` (D83): its own migrations (`sqlx::migrate!`, forward-only), its own writer connection, `journal_mode=WAL`, `synchronous=FULL` (a commit that advances `flushed_seq` must survive a power loss, since the WAL segments it covers are deleted afterwards).

```sql
CREATE TABLE data_files (
  id TEXT PRIMARY KEY,                -- UUIDv7
  table_name TEXT NOT NULL,           -- 'spans' | 'traces' | 'scores'
  project_id TEXT NOT NULL,
  day TEXT NOT NULL,                  -- partition day 'YYYY-MM-DD'
  level INTEGER NOT NULL,             -- 0 | 1
  path TEXT NOT NULL,                 -- relative to object store root
  size_bytes INTEGER NOT NULL,
  footer_size INTEGER NOT NULL,       -- open with one ranged read, no HEAD
  row_count INTEGER NOT NULL,
  min_time INTEGER NOT NULL, max_time INTEGER NOT NULL,
  min_ingest_time INTEGER NOT NULL, max_ingest_time INTEGER NOT NULL,
  fully_merged INTEGER NOT NULL DEFAULT 0,
  deletions_applied_seq INTEGER NOT NULL,  -- §7
  state TEXT NOT NULL DEFAULT 'active',    -- 'active' | 'obsolete' | 'quarantined'
  created_at INTEGER NOT NULL, obsoleted_at INTEGER
);
CREATE INDEX data_files_lookup ON data_files(table_name, project_id, day) WHERE state = 'active';
CREATE INDEX data_files_gc ON data_files(obsoleted_at) WHERE state = 'obsolete';

-- traces file → the spans files it was computed from (§3)
CREATE TABLE file_sources (
  traces_file_id TEXT NOT NULL REFERENCES data_files(id) ON DELETE CASCADE,
  spans_file_id TEXT NOT NULL REFERENCES data_files(id) ON DELETE CASCADE,
  PRIMARY KEY (traces_file_id, spans_file_id)
);

-- objects being written; anything left here after a crash is garbage (§5 GC)
CREATE TABLE staged_objects (path TEXT PRIMARY KEY, created_at INTEGER NOT NULL);

CREATE TABLE table_state (table_name TEXT PRIMARY KEY, flushed_seq INTEGER NOT NULL);

CREATE TABLE projects (                -- only projects with a storage-side state
  project_id TEXT PRIMARY KEY,
  state TEXT NOT NULL                 -- 'dropping'
);
CREATE TABLE retention_watermarks (project_id TEXT PRIMARY KEY, keep_from TEXT NOT NULL);  -- §8

CREATE TABLE media (                   -- §9
  project_id TEXT NOT NULL, sha256 TEXT NOT NULL,
  mime_type TEXT NOT NULL, size_bytes INTEGER NOT NULL, first_seen_at INTEGER NOT NULL,
  PRIMARY KEY (project_id, sha256)
);

CREATE TABLE deletions (               -- §7; the request itself is in meta.db (04)
  id TEXT PRIMARY KEY,                -- same id as meta.db deletions.id
  deletion_seq INTEGER NOT NULL UNIQUE,
  project_id TEXT NOT NULL,
  kind TEXT NOT NULL,                 -- 'traces' | 'user'
  cutoff INTEGER NOT NULL,            -- the request's created_at
  status TEXT NOT NULL DEFAULT 'pending',  -- 'pending' | 'applied'
  registered_at INTEGER NOT NULL, applied_at INTEGER
);
CREATE TABLE deletion_traces (deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
                              trace_id BLOB NOT NULL, PRIMARY KEY (deletion_id, trace_id));
CREATE TABLE deletion_users (deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
                             user_id TEXT NOT NULL, PRIMARY KEY (deletion_id, user_id));
CREATE TABLE deletion_sessions (deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
                                session_id TEXT NOT NULL, PRIMARY KEY (deletion_id, session_id));
```

At startup all active rows are loaded into an in-memory structure:

```
Catalog { tables: { table → { project_id → BTreeMap<day, Vec<Arc<FileMeta>>> } } }
FileMeta { id, path, level, size, footer_size, rows, min_time, max_time,
           min_ingest_time, max_ingest_time, fully_merged, deletions_applied_seq }
```

**Read state.** Readers load one `ReadState` (an `Arc` behind a `std::sync::RwLock`, held only to clone the `Arc`): the in-memory catalog, the memtable batches (`active` and `frozen` of both tables, as `Arc<RecordBatch>` lists) and the tombstone sets (§7), all from the same publish. A row is therefore seen exactly once, in a memtable or in a file.

**Committer.** Every change to the set of files is a catalog edit applied by one committer, which runs the SQLite transaction and publishes the new `ReadState` under one mutex, so commits and snapshots are totally ordered. This covers flush, compaction, deletion rewrite, retention, project drop and GC. An edit has:
- the files to add, whose paths must be in `staged_objects`;
- the files to remove, with their expected state (normally `active`);
- optionally a new `flushed_seq` (compare-and-set: it must be greater);
- optionally deletion progress (§7).

If a precondition fails, the transaction rolls back and the job gets a conflict: its outputs stay staged and are collected by GC, and the job retries later.

**Claims.** Background jobs claim their input files in memory before reading them, so compaction and the deletion rewriter never work on the same file. Claims don't survive a restart and need not: an interrupted job is simply redone.

**Metadata cache:** an LRU cache (default 512 MiB) of decoded Parquet footers, page indexes and bloom filters keyed by file id, plugged into DataFusion through its reader factory. Files are immutable, so entries never go stale. DataFusion 55's built-in file metadata cache is used for whatever it covers; a custom `ParquetFileReaderFactory` adds only what is missing.

## 5. Compaction

Runs in a background task with bounded parallelism (`compaction.threads`, default `cores / 4`).

**Policy, per `(table, project, day)` partition:**
- *L0 merge*: when the partition has ≥ 8 L0 files, or ≥ 512 MiB of L0, or its newest L0 file is older than 10 min. Inputs are all L0 files of the partition, plus an L1 file only if the merged output would be at least `compaction.size_ratio` (default 4) times that L1 file. A partition with one big L1 file and a trickle of L0 files therefore gets a second L1 file instead of rewriting the big one every few minutes. Inputs are merged in time order and the output is cut into L1 files of about `compaction.target_file_size` (default 512 MiB), each covering a contiguous time slice that does not overlap its neighbours; rows inside each output file are sorted by the table's sort order. (For `spans`, slice boundaries come from the input files' time ranges and a `start_time` histogram; each slice is then a k-way merge of the inputs' sorted runs by `trace_id, start_time`. A trace crossing a slice boundary is split across two files, which is fine.)
- *Day close*: once the day has ended plus `compaction.close_delay` (default 2 h), the partition is rewritten into a minimal set. `spans` are fully merged when their total is at most `compaction.target_file_size`, otherwise only the L0 files are merged. `traces` are rebuilt from all of the partition's `spans` files, one partial per trace, `fully_merged = true`. `scores` are fully merged. A new L0 file in a closed day (late data) triggers another day close after `compaction.close_delay`.

**During a merge:**
- `spans`: duplicate `(trace_id, span_id)` rows are collapsed, keeping the greatest `ingest_time`.
- `traces`: never merged on their own. The `traces` files derived from the `spans` inputs (§3) are replaced in the same commit by partials recomputed from the `spans` outputs. A trace whose spans end up in one file has `partial_count = 1`.
- `scores`: last-writer-wins on `score_id` by `updated_at`. Rows with `deleted = true` are dropped **only during a day-close rewrite**, since that is the only time every version of the score is guaranteed to be among the inputs.
- Tombstones (§7) are applied to every table. Outputs record the highest `deletion_seq` known when the job started.

**Resources:**
- A merge streams its inputs (one batch per input at a time) instead of sorting a whole slice in memory. Merge memory across running merges is capped by `compaction.max_memory` (default 1 GiB).
- In local mode a merge first reserves its estimated output size (the sum of its input sizes) against free disk, and waits if it doesn't fit. Flushes don't reserve.

**Commit:** one catalog edit (§4) adds the outputs and marks the inputs `obsolete` with `obsoleted_at = now`. GC physically deletes an obsolete object once no `ReadState` still references it (reference counts on `FileMeta`) **and** `gc.grace` (default 5 min, at least the longest query timeout) has passed, which also covers restarts, when the counts are lost.

**Crash safety:** an output written but never committed is still in `staged_objects`. GC deletes staged objects older than 1 h and their rows, so the bucket is never listed. In local mode, startup also removes leftover temp files.

## 6. Query path

For each request, `llmobs-storage` builds a DataFusion `SessionContext` and the project's `TableProvider`s, all bound to one `ReadState` (catalog, memtable and tombstones, §4). The providers expose the stored Arrow schemas of 01 (binary IDs, `project_id`). They don't register themselves: the query layer (05) registers the user-facing views over them (hex IDs, hidden `project_id`, `sessions`, default exclusions, D89). Because each provider is constructed for a single project, the user cannot name another project's data. A dropped project (§7) gets empty tables.

### `SpansProvider::scan(projection, filters, limit)`

1. Extract a time range from the filters on `start_time` (`=, <, <=, >, >=, BETWEEN`). Prune partitions by `day`, widened by one day on each side (§ Trace anchor), and files by `min_time`/`max_time`.
2. Build a `ParquetExec` (DataFusion's Parquet scan) over the remaining files, with filter pushdown on, page-index pruning, bloom-filter pruning (equality on `trace_id`, `session_id`, `user_id`) and the metadata cache.
3. Build a `MemoryExec` (in-memory scan) over the `active` and `frozen` batches of the same partitions. Same filters, applied in-memory.
4. Combine both with a `UnionExec`, then wrap in the **tombstone filter** (§7) when there are pending tombstones whose `created_at` is at or after the file's or batch's `min_ingest_time`.

### `TracesProvider` (exposed as the logical view `traces`)

A plan that unions (a) the partial rows from `traces` files with (b) partials computed on the fly from the memtable span batches, then `GROUP BY trace_id` with the merge aggregates. "First non-null" values and the root are chosen by `(start_time, span_id)` order, both in partials and in the merge, so results don't depend on flush order.

- *Pushdown:* only `start_time` ranges and `trace_id` equality / `IN` are pushed into the partial scans. Every other filter applies after the merge: a non-root partial usually has no session or user, and `cost_total > 0.1` can fail on every partial and pass on the merged row.
- *Time window:* when the filter bounds `start_time` to `[a, b]`, partials are scanned on `[a − trace_lookback, b + trace_lookback]` (default `trace_lookback` = 1 h), merged, and the merged `start_time` is then filtered to `[a, b]`. Memtable partials come from every memtable span batch of the project whose time range overlaps that padded window. A trace with partials more than 1 h apart may show up with a late start time; this is documented as a known limit.
- *Duplicates:* a span retried by a client and stored in two flushes is counted twice by the merge until compaction merges the two files (≈ 10 min, §5). Documented.
- *Fast path for "latest N traces"* (trace list): scan widening windows back from `b` (15 min, 1 h, 6 h, 24 h, then the whole range) and stop once N merged rows pass the post-merge filters.
- *Single-trace lookup* (`trace_id = X`), in order of preference:
  1. a time hint (`?around=<ts>`, always present in links generated by the UI, API list responses and SDK helpers) narrows the search to files overlapping ±1 day;
  2. otherwise, trace-ID bloom filters (kept in the metadata cache) select candidate files across all of the project's partitions — a few microseconds per file;
  3. post-MVP, if per-project file counts make (2) slow: either a `trace_index` table (trace_id → min/max day), sorted by `trace_id` and maintained at compaction, as LangWatch and Opik do; or one combined bloom-filter file per partition, laid out so a lookup is one ranged read per partition, as OpenObserve does.

  Within each candidate file, the `trace_id` sort order means the page index locates the trace's rows directly.

### `ScoresProvider` (logical view `scores`)

Scan, then `ROW_NUMBER() OVER (PARTITION BY score_id ORDER BY updated_at DESC) = 1`, then filter out `deleted`. Once a day has been closed, its files are already de-duplicated, and the plan skips the window step for them (a `fully_merged` flag per file).

### Resource limits

- **Memory:** one process-wide DataFusion memory pool of `query.max_memory_total` (default 50% of RAM, at most 8 GiB), with each query capped at `query.max_memory` (default 2 GiB) and spilling to `tmp/`.
- **Admission:** a query that cannot reserve its initial memory waits up to `query.admission_wait` (default 5 s), then fails as busy (503).
- **Parallelism:** `target_partitions = cores`.
- **Timeouts:** UI 30 s, SQL console 60 s, MCP 30 s.

### S3 read cache

With an S3 object store, every page read is a ranged GET. A bounded on-disk cache under `cache/` holds byte ranges of data files, keyed by file id and range:
- files are immutable, so entries never go stale;
- eviction is LRU by bytes (`query.read_cache_bytes`, default 10% of the disk, at most 100 GiB);
- entries of a file are dropped when GC deletes it;
- flush and compaction also write their fresh outputs into the cache, since recent data is what gets read.

It sits in the same reader factory as the metadata cache. With a local object store it is off.

## 7. Deletion (GDPR)

### Requests

A request is created by the server in `meta.db` (04), with its cascades, and handed to storage with `register_deletion`. It has a `kind`, a `created_at` that serves as the **cut-off**, and a resolved scope:

| Kind | Scope stored | Rows deleted |
|---|---|---|
| `traces` | `deletion_traces(trace_id…)`. A filter-based request is resolved to trace IDs when it is created. The set is extended with the **evaluation traces** whose `metadata['llmobs.eval.target_trace_id']` is in it (they contain copies of the evaluated data). | rows of those traces with `ingest_time ≤ cut-off` |
| `user` | `user_id`, plus the trace IDs and session IDs resolved at creation, plus their evaluation traces | spans/traces of the user (by `user_id` and by resolved trace), scores on those traces and sessions, judge traces about them |
| `project` | `project_id` | everything in the project (`drop_project`) |
| `org` | `org_id` | every project of the org (`drop_project` for each), plus org-level metadata in `meta.db` |

The cut-off makes the result deterministic: data ingested after the request is new data, not covered by the deletion.

**Handoff.** `register_deletion` is idempotent by deletion id. It stores the request and its resolved scope in `catalog.db` with the next **`deletion_seq`**, then publishes the tombstones in a new read state, and returns only after both. The server:
1. creates the request in `meta.db`;
2. calls `register_deletion`;
3. sets `handed_over_at`.

At startup it re-registers every request not yet handed over, and storage ignores ids it already knows. The server polls `deletion_status` to mark its own row `applied`.

### Making data invisible immediately

Pending `traces` and `user` deletions are kept in memory per project as a **tombstone set**: `trace_ids: HashSet<[u8;16]>`, `users: HashMap<user_id, cutoff>`, `sessions: HashMap<session_id, cutoff>`. Each set has a cut-off per entry. The query path applies a physical filter (`TombstoneFilterExec`) that drops matching rows. It runs only on files and batches whose ingest-time range overlaps at least one cut-off.

`project` and `org` deletions are immediate: `drop_project` marks the project `dropping` in `catalog.db` and removes it from every new read state in the same commit; the server returns 404 for it.

### Cascades, inside the server's request transaction (`meta.db`)

- Dataset items whose `source_trace_id` is in scope are hard-deleted, all versions.
- `experiment_items` and `eval_jobs` referencing those traces are deleted.
- `alert_events` hold only aggregates and are kept.

### Physical erasure: the deletion rewriter

Every file records `deletions_applied_seq`: no row in scope of any deletion with `deletion_seq` up to that number remains in it.
- A flush sets it to the highest `deletion_seq` whose tombstones it applied.
- Compaction and the rewriter set it to the highest `deletion_seq` known when the job started, since they apply all of those.

A background task processes pending deletions oldest first:

1. Candidate files: the project's `spans` and `scores` files with `min_ingest_time ≤ cut-off` and `deletions_applied_seq < deletion_seq`. These are narrowed with the bloom filters (`trace_id`, `user_id`, `session_id`) and column statistics, then confirmed by a scan that projects only the key columns. A candidate without matching rows only gets its `deletions_applied_seq` raised.
2. Each affected file is rewritten without the matching rows. The output keeps the same level, the `traces` files derived from the rewritten `spans` files are recomputed (§3), and the commit works like a compaction commit (§4).
3. Any frozen memtable flushed after the request drops the matching rows at flush time (§2 step 2).
4. The deletion is **applied** once two conditions hold:
   - every active `spans` and `scores` file of the project with `min_ingest_time ≤ cut-off` has `deletions_applied_seq ≥` its `deletion_seq` (`traces` files follow their source `spans` files, §3);
   - no memtable batch older than the cut-off remains.

   Then `status = 'applied'`, `applied_at` is set, and its tombstone entries are removed.

Compaction and the rewriter never run on the same file (claims, §4), and a compaction that started before a deletion records an older `deletions_applied_seq`, so its outputs remain candidates. Deleted rows cannot come back through a merge.

For `project` and `org` deletions: the catalog rows are removed, the objects under the `project=` prefixes and `media/<project_id>/` are deleted, and then the storage rows.

**Guarantee:** the bytes are gone from Parquet files within `deletion.max_delay` (default 24 h; the rewriter alerts in its logs if it is falling behind). The WAL holds data only until the next flush checkpoint (minutes), so it ages out by itself. The meaning of GDPR deletion for backups is covered in 12 §Backups.

## 8. Retention

Each project has `retention_days` in `meta.db` (null means keep forever). The server calls `apply_retention(project, keep_from = today − retention_days)` hourly. Storage then:
- commits one catalog edit that obsoletes every file of the project in days before `keep_from`, across `spans`, `traces` and `scores` (no file is rewritten, because partitions are per day);
- records `keep_from` in `retention_watermarks`.

From then on, a flush drops rows of that project older than the watermark, so late rows cannot recreate a dropped day. Retention is day-granular. Since a trace lives in one anchor day (§ Trace anchor), it is dropped as a whole.

## 9. Media

Media blobs live under `media/<project_id>/<sha256>` in the object store; their index is the `media` table of `catalog.db`. Storage writes a blob durably (local: temp file, `sync_all`, rename, directory fsync; S3: `PutObject` with `If-None-Match: *`) and records its row; writing an existing hash is a no-op.

**GC** runs daily. For each project, it reads only the `media_refs` column of `spans` (a cheap projection) to build the set of live hashes. It then deletes the objects and `media` rows that are both absent from the set and older than 24 h; the age guard avoids racing with spans that are still being ingested.

## 10. Failure handling

| Situation | Behaviour |
|---|---|
| Free disk under `storage.min_free_bytes` (default 2 GiB) | ingestion answers 503; compaction keeps running (it reclaims space) within its disk reservation (§5) |
| WAL write or fsync error | pending and new appends fail with `WalFailed` (503), health reports it, restart required (§1) |
| Object store errors during flush | retry with backoff, while the memtable keeps the data and backpressure applies past the hard limit |
| Ambiguous upload (error reported, object may exist) | retried under a new name; the old one is collected from `staged_objects` (§2) |
| Crash | WAL replay before ingestion opens; staged objects are GC'd |
| Corrupted Parquet file | it is quarantined (state `quarantined`, excluded from snapshots) and an error is logged; data is lost only for that file |

## 11. Toward multi-node

Nothing here assumes a single node, apart from the WAL and memtable living in the same process as the querier. The planned split:

- **Ingesters** own the WAL and memtable, sharded by project.
- **Queriers** are stateless; they read files from S3 and ask ingesters for their memtable snapshot over Arrow Flight.
- **The catalog** moves to Postgres.

File formats and paths stay unchanged.

## Capacity sanity check

At 50k spans/s with an average normalized span of about 3 KiB (LLM spans 10–50 KiB, others under 1 KiB):
- Ingest is about 150 MiB/s of uncompressed Arrow; with LZ4 bodies the WAL writes roughly a third of that, which fits easily on NVMe.
- The memtable flushes every ~1.7 s at 256 MiB. With that rate, raise `flush.max_bytes` to 1–2 GiB to get bigger L0 files.
- With zstd at roughly 5–8× on JSON-heavy text, compressed output is about 20–30 MiB/s, or 2–2.5 TiB/day.

Sustained use at this rate needs S3 and a bigger box. The target box (8 cores, 32 GiB) is sized for bursts at that rate and sustained ingest about an order of magnitude lower.
