# 03 — Storage engine

The columnar store holds three tables: `spans`, `traces` (derived partial summaries) and `scores` (01). It is a small LSM-style engine: **WAL → memtable → L0 Parquet files → L1 Parquet files**, with a catalog in SQLite and DataFusion as the query engine.

## Layout on disk / object store

```
<data_dir>/
  meta.db                 SQLite (metadata + catalog), WAL mode
  wal/
    spans/000000000042.wal
    scores/000000000007.wal
  objects/                only when object store = local
    tables/<table>/project=<project_id>/day=<YYYY-MM-DD>/<level>-<uuidv7>.parquet
    media/<project_id>/<sha256>
```

With `storage.object_store = s3://bucket/prefix`, the `objects/` tree lives in the bucket; `meta.db` and `wal/` stay on local disk, which **must be persistent** (an ephemeral disk loses up to `flush.max_age` of acknowledged data on machine loss — documented).

Partition key: `(project_id, day)` where day is the UTC date of `start_time` (spans, traces) or `created_at` (scores). A late span lands in the partition of its own `start_time`, creating a new L0 file there; that is fine.

## 1. Write-ahead log

One WAL per table (`spans`, `scores`), each a sequence of segment files (max 64 MiB, then rotate).

Record format:

```
u32  payload_len
u32  crc32c(seq || kind || payload)
u64  seq            monotonically increasing per table
u8   kind           1 = row batch, 2 = reserved
[..] payload        Arrow IPC stream (one RecordBatch, table schema, already normalized)
```

**Group commit.** Ingest tasks send `(RecordBatch, oneshot ack)` to a single WAL writer task over a bounded channel. The writer drains everything available (up to 8 MiB or 2 ms after the first item), writes all records with one `write_all`, calls `fdatasync`, then completes every ack. This gives one fsync per group, so throughput is bounded by disk bandwidth, not fsync latency. Config `wal.sync = always | every_ms(N)` (default `always`; `every_ms` trades durability for speed and is documented).

**Checkpoint.** The catalog stores `flushed_seq` per table (highest WAL seq whose rows are in Parquet files). Segments whose max seq ≤ `flushed_seq` are deleted.

**Recovery.** At startup, for each table, read segments in order, skip records with seq ≤ `flushed_seq`, insert the rest into the memtable. A record with a bad CRC or short read at the **tail** of the last segment is a torn write: truncate the segment there. A bad record anywhere else is corruption: refuse to start unless `--wal-skip-corrupt`.

## 2. Memtable

Per table: `active` (accepting inserts) and at most one `frozen` (being flushed). Each is a map `partition → Vec<RecordBatch>` plus byte accounting. Inserts append the WAL'd batch (split by partition) — no row-by-row building, so inserts are cheap.

**Flush triggers:** active size ≥ `flush.max_bytes` (default 256 MiB), oldest row age ≥ `flush.max_age` (default 120 s), or shutdown.

**Backpressure:** if `active + frozen` exceeds `memtable.hard_limit` (default 1 GiB) ingestion answers `429 Retry-After: 1` / `RESOURCE_EXHAUSTED`.

### Flush

1. Under the table-state write lock: move `active` → `frozen`, create a new empty `active`, record `frozen_max_seq`.
2. Outside the lock, per partition of `frozen`:
   - drop rows hidden by tombstones (§7);
   - sort (spans: `trace_id, start_time`; scores: `created_at`);
   - write one L0 Parquet file;
   - for `spans`, also compute **trace partials** (`GROUP BY trace_id` with the merge rules of 01) and write one L0 file to `traces`.
3. One SQLite transaction: insert the new `data_files` rows and set `flushed_seq = frozen_max_seq`.
4. Under the write lock: append the new files to the in-memory catalog and drop `frozen`.

Steps 3–4 make the switch atomic for readers: a query snapshot sees either (frozen, without the new files) or (the new files, without frozen), never both or neither.

## 3. Parquet files

| Setting | Value |
|---|---|
| Compression | zstd level 3 |
| Row group | closed when in-progress size ≥ 64 MiB or 100 000 rows (manual `flush()` on `ArrowWriter`) |
| Data page | 1 MiB, max 20 000 rows/page; page index on |
| Statistics | page-level for all scalar columns |
| Dictionary | on for dictionary-typed columns; off for `input`, `output`, `attributes` |
| Bloom filters | `trace_id`, `session_id`, `user_id` (fpp 0.01) on `spans`; `trace_id`, `session_id`, `user_id` on `traces`; `trace_id`, `score_id` on `scores` |
| Sort order (within a file) | `spans`: `trace_id, start_time` · `traces`: `start_time` · `scores`: `created_at` (declared in file metadata so DataFusion can exploit it) |
| Time slicing (across files) | every file covers a contiguous `[min_time, max_time]` slice; see below |
| KV metadata | `llmobs.table`, `llmobs.schema_version`, `llmobs.level`, `llmobs.min_time`, `llmobs.max_time`, `llmobs.min_ingest_time`, `llmobs.max_ingest_time` |

**Time-sliced files, trace-sorted rows** (same pattern as Opik, LangWatch and Laminar, which partition by time and sort by trace ID inside the partition):

- *Across files*, data is organized by time: L0 files cover one flush window; compaction merges inputs in time order and cuts outputs at time boundaries, so each output file covers a contiguous, non-overlapping time slice. Time-range scans (metrics, alerts, span search) prune at file granularity using `min_time`/`max_time`.
- *Within a file*, spans are sorted by `trace_id, start_time`: opening a trace reads one contiguous run of rows (usually one page) per file instead of rows scattered over many row groups.
- The time-ordered access path for lists is the `traces` table (sorted by `start_time`), the equivalent of Laminar's time projection / LangWatch's `trace_analytics`.

**Schema evolution:** only adding nullable columns. Each file carries its own schema; the reader uses DataFusion's schema adapter to fill missing columns with nulls. Renames or type changes need a new column.

## 4. Catalog

SQLite table `data_files` (04) plus `table_state(table_name, flushed_seq)`. At startup all active rows are loaded into an in-memory structure:

```
Catalog { tables: { table → { project_id → BTreeMap<day, Vec<FileMeta>> } } }
FileMeta { id, path, level, size, rows, min_time, max_time, min_ingest_time, max_ingest_time }
```

Readers take an `Arc` snapshot (copy-on-write; `arc-swap`). Writers (flush, compaction, deletion rewrite, retention) commit in SQLite first, then publish a new snapshot.

**Metadata cache:** an LRU cache (default 512 MiB) of decoded Parquet footers, page indexes and bloom filters keyed by file id, plugged into DataFusion via a custom `ParquetFileReaderFactory`. Files are immutable, so entries never go stale.

## 5. Compaction

Runs in a background task with bounded parallelism (`compaction.threads`, default `cores / 4`).

**Policy, per `(table, project, day)` partition:**
- *L0 merge*: when the partition has ≥ 8 L0 files, or ≥ 512 MiB of L0, or its newest L0 file is older than 10 min. Inputs are all L0 files plus any L1 file smaller than half the target. Inputs are merged in time order and the output is cut into L1 files of about `compaction.target_file_size` (default 512 MiB), each covering a contiguous time slice that does not overlap its neighbours; rows inside each output file are sorted by the table's sort order. (For `spans` this means: k-way merge by `start_time` to choose slice boundaries, then sort each slice by `trace_id, start_time`. A trace crossing a slice boundary is split across two files, which is fine.)
- *Day close*: once the day has ended plus `compaction.close_delay` (default 2 h), the partition's `traces` and `scores` files are rewritten into a minimal set, fully merged.

**During a merge:**
- `spans`: duplicate `(trace_id, span_id)` rows are collapsed, keeping the greatest `ingest_time`.
- `traces`: partials of the same `trace_id` are merged with the rules of 01. A trace whose partials end up in one file has `partial_count = 1`.
- `scores`: last-writer-wins on `score_id` by `updated_at`. Rows with `deleted = true` are dropped **only during a day-close rewrite**, since that is the only time every version of the score is guaranteed to be among the inputs.
- Tombstones (§7) are applied to every table.

**Commit:** one SQLite transaction inserts the outputs and marks the inputs `obsolete` with `obsoleted_at = now`, then a new catalog snapshot is published. Obsolete objects are physically deleted by the GC task after `gc.grace` (default 10 min), so queries already running on the old snapshot can finish.

**Crash safety:** an output file written but never committed is an orphan. At startup and daily, the GC lists the object store and deletes any file older than 1 h that the catalog doesn't know.

## 6. Query path

`llmobs-storage` builds a DataFusion `SessionContext` per request. It registers the project's tables as custom `TableProvider`s bound to `(project_id, catalog snapshot, memtable snapshot, tombstone snapshot)`. Because the provider is constructed for a single project, the user cannot name another project's data.

### `SpansProvider::scan(projection, filters, limit)`

1. Extract a time range from the filters on `start_time` (`=, <, <=, >, >=, BETWEEN`) and prune partitions by `day` and files by `min_time`/`max_time`.
2. Build a `ParquetExec` (DataFusion's Parquet scan) over the remaining files, with filter pushdown on, page-index pruning, bloom-filter pruning (equality on `trace_id`, `session_id`, `user_id`) and the metadata cache.
3. Build a `MemoryExec` (in-memory scan) over the `active` and `frozen` batches of the same partitions. Same filters, applied in-memory.
4. Combine both with a `UnionExec`, then wrap in the **tombstone filter** (§7) when there are pending tombstones whose `created_at` is at or after the file's or batch's `min_ingest_time`.

### `TracesProvider` (exposed as the logical view `traces`)

A plan that unions (a) the partial rows from `traces` files with (b) partials computed on the fly from the memtable span batches, then `GROUP BY trace_id` with the merge aggregates. To keep it cheap:

- *Time window*: when the filter bounds `start_time` to `[a, b]`, partials are scanned on `[a − trace_lookback, b]` (default `trace_lookback` = 1 h), merged, and the merged `start_time` is then filtered to `[a, b]`. A trace with partials more than 1 h apart may show up with a late start time; this is documented as a known limit.
- *Fast path for "latest N traces"* (trace list, no aggregate filters): scan widening windows back from `b` (15 min, 1 h, 6 h, 24 h, then the whole range) and stop once N merged rows qualify.
- *Single-trace lookup* (`trace_id = X`), in order of preference:
  1. a time hint (`?around=<ts>`, always present in links generated by the UI, API list responses and SDK helpers) narrows the search to files overlapping ±1 day;
  2. otherwise, trace-ID bloom filters (kept in the metadata cache) select candidate files across all of the project's partitions — a few microseconds per file;
  3. post-MVP, if per-project file counts make (2) slow: a `trace_index` table (trace_id → min/max day), sorted by `trace_id` and maintained at compaction, as LangWatch and Opik do.

  Within each candidate file, the `trace_id` sort order means the page index locates the trace's rows directly.

### `ScoresProvider` (logical view `scores`)

Scan, then `ROW_NUMBER() OVER (PARTITION BY score_id ORDER BY updated_at DESC) = 1`, then filter out `deleted`. Once a day has been closed, its files are already de-duplicated, and the plan skips the window step for them (a `fully_merged` flag per file).

### Resource limits

Each query runs with:
- a DataFusion memory pool cap (`query.max_memory`, default 2 GiB, spills to `tmp/`),
- `target_partitions = cores`,
- a timeout (UI 30 s, SQL console 60 s, MCP 30 s).

## 7. Deletion (GDPR)

### Requests

A request is stored in SQLite `deletions` (04). It has a `kind`, a `created_at` that serves as the **cut-off**, and a resolved scope:

| Kind | Scope stored | Rows deleted |
|---|---|---|
| `traces` | `deletion_traces(trace_id…)`. A filter-based request is resolved to trace IDs when it is created. The set is extended with the **evaluation traces** whose `metadata['llmobs.eval.target_trace_id']` is in it (they contain copies of the evaluated data). | rows of those traces with `ingest_time ≤ cut-off` |
| `user` | `user_id`, plus the trace IDs and session IDs resolved at creation, plus their evaluation traces | spans/traces of the user (by `user_id` and by resolved trace), scores on those traces and sessions, judge traces about them |
| `project` | `project_id` | everything in the project |
| `org` | `org_id` | every project of the org, plus org-level metadata |

The cut-off makes the result deterministic: data ingested after the request is new data, not covered by the deletion.

### Making data invisible immediately

Pending `traces` and `user` deletions are kept in memory per project as a **tombstone set**: `trace_ids: HashSet<[u8;16]>`, `users: HashMap<user_id, cutoff>`, `sessions: HashMap<session_id, cutoff>`. Each set has a cut-off per entry. The query path applies a physical filter (`TombstoneFilterExec`) that drops matching rows. It runs only on files and batches whose ingest-time range overlaps at least one cut-off.

`project` and `org` deletions are immediate: the project is marked `deleting` in SQLite, its tables are removed from every new catalog snapshot, and the API returns 404 for it.

### Cascades, inside the request transaction

- Dataset items whose `source_trace_id` is in scope are hard-deleted, all versions.
- `experiment_items` and `eval_jobs` referencing those traces are deleted.
- `alert_events` hold only aggregates and are kept.

### Physical erasure: the deletion rewriter

A background task processes pending deletions oldest first:

1. Candidate files: the project's files across all three tables with `min_ingest_time ≤ cut-off`. These are narrowed with the bloom filters (`trace_id`, `user_id`, `session_id`) and column statistics, then confirmed by a scan that projects only the key columns.
2. Each affected file is rewritten without the matching rows. The output keeps the same level, and the commit works like a compaction commit.
3. Any frozen memtable flushed after the request drops the matching rows at flush time (step 2 of the flush).
4. When no candidate files remain, the deletion is marked `applied`, `applied_at` is set, and the tombstone entries are removed.

For `project` and `org` deletions: the catalog rows are removed, the objects under the `project=` prefixes and `media/<project_id>/` are deleted, and then the SQLite rows.

**Guarantee:** the bytes are gone from Parquet files within `deletion.max_delay` (default 24 h; the rewriter alerts in its logs if it is falling behind). The WAL holds data only until the next flush checkpoint (minutes), so it ages out by itself. The meaning of GDPR deletion for backups is covered in 12 §Backups.

## 8. Retention

Each project has `retention_days` (null means keep forever). A job runs hourly and drops every partition whose `day < today − retention_days` across `spans`, `traces` and `scores`, using the same catalog commit as compaction. No file is rewritten, because partitions are per day. Retention is therefore day-granular.

## 9. Media GC

Runs daily. For each project, it reads only the `media_refs` column of `spans` (a cheap projection) to build the set of live hashes. It then lists `media/<project_id>/` and deletes objects that are both absent from the set and older than 24 h; the age guard avoids racing with spans that are still being ingested. The matching `media` rows are deleted from SQLite too.

## 10. Failure handling

| Situation | Behaviour |
|---|---|
| Free disk under `storage.min_free_bytes` (default 2 GiB) | ingestion answers 503; compaction keeps running (it reclaims space) |
| Object store errors during flush | retry with backoff, while the memtable keeps the data and backpressure applies past the hard limit |
| Crash | WAL replay; orphaned files are GC'd |
| Corrupted Parquet file | it is quarantined (state `quarantined`, excluded from snapshots) and an error is logged; data is lost only for that file |

## 11. Toward multi-node

Nothing here assumes a single node, apart from the WAL and memtable living in the same process as the querier. The planned split:

- **Ingesters** own the WAL and memtable, sharded by project.
- **Queriers** are stateless; they read files from S3 and ask ingesters for their memtable snapshot over Arrow Flight.
- **The catalog** moves to Postgres.

File formats and paths stay unchanged.

## Capacity sanity check

At 50k spans/s with an average normalized span of about 3 KiB (LLM spans 10–50 KiB, others under 1 KiB):
- Ingest is about 150 MiB/s into the WAL, which fits on NVMe.
- The memtable flushes every ~1.7 s at 256 MiB. With that rate, raise `flush.max_bytes` to 1–2 GiB to get bigger L0 files.
- With zstd at roughly 5–8× on JSON-heavy text, compressed output is about 20–30 MiB/s, or 2–2.5 TiB/day.

Sustained use at this rate needs S3 and a bigger box. The target box (8 cores, 32 GiB) is sized for bursts at that rate and sustained ingest about an order of magnitude lower.
