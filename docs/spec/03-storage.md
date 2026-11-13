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
    spans/00000000000000000042.wal
    scores/00000000000000000007.wal
  tmp/
    query/                query spill files (§6), emptied at open
    compaction/           S3-mode compaction outputs being written (§5)
  cache/                  S3 read cache (§6), only with an S3 object store
    <file name>/size, <file name>/<block index>
  objects/                only when object store = local
    tables/<table>/project=<project_id>/day=<YYYY-MM-DD>/<level>-<uuidv7>.parquet
    media/<project_id>/<sha256>
```

With `storage.object_store = s3://bucket/prefix`, the `objects/` tree lives in the bucket; `catalog.db`, `meta.db` and `wal/` stay on local disk, which **must be persistent** (an ephemeral disk loses up to `flush.max_age` of acknowledged data on machine loss — documented). Absent, `object_store` is `file://<data_dir>/objects`. A local path is `file://<absolute path>`.

A local object write lands in the destination directory as `.<filename>.tmp-<uuidv7>`, then `sync_all`, rename onto `<filename>`, and an fsync of that directory. Startup deletes leftover files whose names start with `.` and contain `.tmp-`.

**Partition key:** `(project_id, day)`.
- For `spans` and `traces`, `day` is the trace's **anchor day** (§ Trace anchor). It equals the UTC date of the row's own `start_time` or differs from it by at most one day.
- For `scores`, `day` is the UTC date of `created_at`.

A late span lands in its trace's anchor partition, creating a new L0 file there; that is fine.

Spans are expected to carry plausible start times. Normalization rejects spans that start before 2000-01-01, before `ingest_time − retention_days`, or more than one day after `ingest_time` (02 rule 15, D85).

## Trace anchor

A trace is anchored to the partition day of its **first partial**: the first time a flush writes rows for it, they go to the UTC day of their `start_time`. Later rows of the same trace go to that anchor day if their own UTC day is within one day of it; otherwise they go to their own day. Retention and compaction therefore handle a trace that crosses midnight as one unit (D86).

Only traces near midnight need remembering. The storage keeps a map `(project_id, trace_id) → anchor day` for every trace with a row starting within `anchor.window` (default = `trace_lookback`, 1 h) of a UTC midnight, on either side. The nearest midnight wins; exactly noon rounds up.

- **Assignment is per row.** A row goes to its trace's anchor day when its own UTC day is at most one day away, and to its own day otherwise. A row two or more days from the anchor stays in its own day, where the one-day query slack finds it.
- **Expiry runs on ingest time.** An entry made by a flush expires at that flush's largest `ingest_time` + `2 × anchor.window`; any flush with rows for a trace that already has an entry refreshes that entry's deadline the same way, even when none of its rows is near midnight. Expired entries are dropped after each commit once the clock (the largest `ingest_time` any flush has committed) passes their deadline. Because this counts ingest time, not event age, backfilled data (old events ingested now) is anchored like live data.
- **Rebuild at startup.** The map is rebuilt from the active `traces` files whose `max_ingest_time` is within `2 × anchor.window` of the clock. Only the row groups whose `last_ingest_time` statistics have a max in that window are read, projected to `trace_id`, `start_time`, `end_time` and `last_ingest_time`; rows with `last_ingest_time ≥ clock − 2 × anchor.window` and `start_time` or `end_time` near midnight restore an entry in the file's partition day. A partial's `start_time` is the trace's earliest start, so `end_time` is what restores a trace whose child ends near midnight. If several files name a trace, the earlier day and the later expiry win.
- **Replayed rows.** Rows replayed from the WAL get their anchor when they flush.
- **Known limit.** A trace with no row near midnight before a flush, and rows past midnight later, gets an entry only from its later rows. Example: rows at 22:00 flushed, then rows at 00:30. Its earlier rows stay in the earlier day. The anchor guarantees one day only for traces whose rows reach the window before midnight.

Because rows may sit in the day before or after their own date, day pruning (§6) widens the day range by one day on each side. File pruning by `min_time`/`max_time`, which hold the real times, stays exact. Memtable pruning stays exact.

## 1. Write-ahead log

One WAL per table (`spans`, `scores`), each a sequence of segment files. A segment is named with the seq of its first record, zero-padded to 20 digits, plus `.wal`. Segments rotate at `storage.wal.segment_bytes` (default 64 MiB). Rotation happens between groups, so one group can make a segment larger than the limit.

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

**Checkpoint.** The catalog stores `flushed_seq` per table (highest WAL seq whose rows are in Parquet files). WAL segments are deleted only after the catalog transaction that advanced `flushed_seq` has returned (§2). Segment `k` is deleted when a later segment exists and `first_seq(k + 1) - 1 <= flushed_seq`. The newest segment stays, because the writer may still be appending to it (D120). At low traffic that segment can hold flushed and unflushed records; replay skips the flushed ones. The extra bytes are at most one segment (`wal.segment_bytes`).

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

**Backpressure:** if `active + frozen` exceeds `memtable.hard_limit` (default 1 GiB) ingestion answers `503 Retry-After: 1` / `UNAVAILABLE` (D-G-3; `429` is kept for per-org limits).

### Flush

1. **Freeze.** Under the table-state write lock: move `active` → `frozen`, create a new empty `active`, record `frozen_max_seq`.
2. **Prepare, outside the lock.** For `spans`, first regroup each project's frozen rows by trace and assign every trace's rows to its anchor day (§ Trace anchor): the memtable partitions by each row's own day, files by the anchor day. Then, per output partition:
   - drop the partitions of dropped projects (§7) and those older than the project's retention watermark (§8), then the rows covered by pending deletions (§7). The tombstones and project states are read once per attempt; every output records the highest `deletion_seq` known then. A partition left with no row writes no file, and a flush left with no file commits only `flushed_seq`;
   - drop duplicate `(trace_id, span_id)` rows, keeping the greatest `ingest_time` (spans);
   - sort (spans: `trace_id, start_time`; scores: `created_at`);
   - for `spans`, also compute the **trace partials** of exactly the rows of that spans file, one per trace, with the merge operator (spec 01 merge rules, D152), for a `traces` file derived from it (§3).
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
6. **Truncate.** Delete the WAL segments covered by `flushed_seq`, keeping the newest (§1, D120).

Step 5 makes the switch atomic for readers: a query snapshot sees either (frozen, without the new files) or (the new files, without frozen), never both or neither.

A failure in steps 2–5 keeps `frozen` and retries that same view under new file names. The wait starts at 1 s, doubles, and is capped at 60 s, and it resets after an attempt that writes nothing or commits. A shutdown signal ends the wait that is in progress. The two flush tasks then have 30 s together to finish; a task still running is cancelled, and its rows stay in the WAL for the next open. The staged names of a failed attempt stay for GC (§5). A failure of step 6 does not undo the commit: the sequence is remembered and truncation is retried with the same backoff. The task also wakes on a timer (50 ms when no age deadline is sooner), so age and data recovered at startup flush without another append.

## 3. Parquet files

| Setting | Value |
|---|---|
| Compression | zstd level 3 |
| Row group | closed when in-progress size ≥ 64 MiB or 100 000 rows (manual `flush()` on `ArrowWriter`) |
| Data page | 1 MiB, max 20 000 rows/page; page index on |
| Statistics | page-level for all scalar columns |
| Dictionary | on for dictionary-typed columns; off for `input`, `output`, `attributes` |
| Bloom filters | `trace_id`, `session_id`, `user_id` (fpp 0.01) on `spans`; `trace_id`, `session_id`, `user_id` on `traces`; `trace_id`, `score_id` on `scores` |
| Sort order (within a file) | `spans`: `trace_id, start_time` · `traces`: `start_time, trace_id` (the `trace_id` tie-break makes files byte-identical across runs) · `scores`: `created_at` (declared in file metadata so DataFusion can exploit it; declared only because the writer sorted) |
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
CREATE INDEX file_sources_by_spans ON file_sources (spans_file_id);

-- objects being written; anything left here after a crash is garbage (§5 GC)
CREATE TABLE staged_objects (path TEXT PRIMARY KEY, created_at INTEGER NOT NULL);

CREATE TABLE table_state (table_name TEXT PRIMARY KEY, flushed_seq INTEGER NOT NULL);

CREATE TABLE projects (                -- only dropped projects (§7)
  project_id TEXT PRIMARY KEY,
  state TEXT NOT NULL,                -- 'dropping' (media not yet deleted) | 'dropped'
  dropped_at INTEGER NOT NULL
);
CREATE TABLE retention_watermarks (project_id TEXT PRIMARY KEY, keep_from TEXT NOT NULL);  -- §8

CREATE TABLE media (                   -- §9
  project_id TEXT NOT NULL, sha256 TEXT NOT NULL,   -- 64 lowercase hex
  mime_type TEXT NOT NULL, size_bytes INTEGER NOT NULL,
  first_seen_at INTEGER NOT NULL, last_seen_at INTEGER NOT NULL,
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

**Read state.** Readers load one `ReadState` (an `Arc` behind a `std::sync::RwLock`, held only to clone the `Arc`): the in-memory catalog, the memtable batches (`active` and `frozen` of both tables, as `Arc<RecordBatch>` lists), the tombstone sets (§7) and the project states (retention watermarks §8, dropped projects §7), all from the same publish. A row is therefore seen exactly once, in a memtable or in a file.

**Committer.** Every change to the set of files is a catalog edit applied by one committer, which runs the SQLite transaction and publishes the new `ReadState` under one mutex, so commits and snapshots are totally ordered. This covers flush, compaction, deletion rewrite, retention, project drop and GC. An edit has:
- the files to add, whose paths must be in `staged_objects`;
- the files to remove, with their expected state (normally `active`);
- optionally a new `flushed_seq` (compare-and-set: it must be greater);
- optionally files whose `deletions_applied_seq` rises without a rewrite (§7); each must be active and the new value greater;
- optionally active files to mark `quarantined` (§10), under the same dependants rule as removed files.

An added file is refused when its project is dropped (§7) or its day is before the project's retention watermark (§8), so a flush or compaction prepared before a drop or a retention cannot bring the data back. Retention and project drop commit through the same path, with their own rows (`retention_watermarks`, `projects`, `deletions`) written in the same transaction.

If a precondition fails, the transaction rolls back and the job gets a conflict: its outputs stay staged and are collected by GC, and the job retries later.

**Claims.** Background jobs claim their input files in memory before reading them, so compaction and the deletion rewriter never work on the same file. Claims don't survive a restart and need not: an interrupted job is simply redone.

**Metadata cache.** One process-wide DataFusion `RuntimeEnv` is built at open; each `Storage::query` derives its own from it with a different memory pool (§6), sharing the caches, the object store and the spill directory. `query.metadata_cache_bytes` (default 512 MiB; zero disables it) is `CacheManagerConfig`'s file-metadata cache limit. The cache stores decoded Parquet footers. With a cache configured, DataFusion also loads column and offset indexes into that metadata. Bloom-filter bitsets are not part of it: the footer records their offsets, and a pruning read fetches the bitset from the object. Files are immutable, so entries never go stale. There is no custom `ParquetFileReaderFactory`.

## 5. Compaction

A background task runs a **compaction pass** every `compaction.interval` (default 30 s). A pass looks at every `(table, project, day)` partition of `spans` and `scores` in the current catalog, picks at most one job per partition, and runs the jobs with at most `compaction.threads` (default `max(1, cores / 4)`) at a time. `traces` files are never compacted on their own: they follow their `spans` files (below).

Times below are wall-clock nanoseconds (`now`). A file's age is `now` minus its `max_ingest_time`.

**Policy, per partition.** A partition with any file claimed by a running job gets no job.
- *Day close* is due when `now ≥ end of the partition day + compaction.close_delay` (default 2 h), the partition is not closed, and its newest L0 file, if any, is at least `compaction.close_delay` old. A partition is **closed** when:
  - `spans`: it has no L0 file, and either it has at most one file or its files total more than `compaction.target_file_size` (default 512 MiB);
  - `scores`: every file has `fully_merged = true`.

  The day close job takes every file of the partition when their total is at most `compaction.target_file_size`, and for `scores` always. Otherwise (`spans` only) it takes only the L0 files. The outputs of a `spans` job that took every file of the partition, and of a `scores` day close, have `fully_merged = true`. A `scores` L0 merge never does, since it keeps deleted versions. A new L0 file in a closed day (late data) makes it not closed, so another day close runs after `compaction.close_delay`.
- *L0 merge*, when no day close is due: when the partition has at least `compaction.l0_files` (default 8) L0 files, or at least `compaction.l0_bytes` (default 512 MiB) of L0, or at least two L0 files and the newest is at least `compaction.l0_age` (default 10 min) old. Inputs are all L0 files of the partition plus L1 files chosen by the **size ratio**: take the L1 files in increasing size order, and add each one while the output so far (the summed size of the inputs chosen) is at least `compaction.size_ratio` (default 4) times its size; stop at the first that fails. A partition with one big L1 file and a trickle of L0 files therefore gets a second L1 file instead of rewriting the big one every few minutes. A job needs at least two input files.

**Slicing.** A job's output is cut into `n = max(1, ceil(input bytes / compaction.target_file_size))` L1 files, each holding one contiguous range of the table's time column (`start_time` for `spans`, `created_at` for `scores`). The boundaries are the time values at ranks `⌊len × i / n⌋` (i = 1 … n − 1) of the sorted time values of all input rows; equal boundaries collapse into one. A slice holds the rows with `lower ≤ time < upper`; the first slice has no lower bound and the last no upper bound. Each slice is a streaming k-way merge of the inputs' sorted runs (`spans`: `trace_id, start_time`; `scores`: `created_at`), filtered to the slice. A trace crossing a slice boundary is split across two files, which is fine.

**During a merge:**
- `spans`: duplicate `(trace_id, span_id)` rows within a slice are collapsed, keeping the greatest `ingest_time` (the later row on a tie), as at flush. Two copies of a span whose `start_time`s fall in different slices both stay.
- `traces`: each `spans` output gets its own `traces` output, computed from exactly its rows by the merge operator (one partial per trace, D152), at the same level and with the same `fully_merged`. The `traces` files derived from the `spans` inputs (§3) are removed in the same commit. A day that fully merges therefore ends with one `traces` file and `partial_count = 1` for every trace; a bigger day has one partial per trace and `spans` file the trace reaches (D-A1-1).
- `scores`: last-writer-wins on `score_id`, keeping the greatest `(updated_at, ingest_time)` (the later row on a tie), as the `scores` view does. Rows with `deleted = true` are dropped **only by a day close**, which always takes every file of the partition: every version of a score shares its `created_at`, so they are all among the inputs.
- Tombstones (§7): the deletions pending when the job starts are applied to `spans` and `scores` rows before the dedup, and every output (`traces` included) records the highest `deletion_seq` known when the job started. A slice left with no row produces no output.

**Resources:**
- A merge streams its inputs (one batch per input at a time) instead of sorting a whole slice in memory. Merge memory across running jobs is capped by `compaction.max_memory` (default 1 GiB), a DataFusion memory pool of its own.
- Each `spans` and `scores` output is written while it is merged, into a temp file, then published durably. On local disk the temp file sits in the target directory and is synced and renamed like any object write. With S3 it is `<data_dir>/tmp/compaction/<uuidv7>.parquet`, uploaded, then removed. `traces` outputs are small and are built in memory (D-A1-2).
- Before it reads anything, every job (merges and rewrites, local and S3 mode alike, since outputs stream through a local temp file in both) reserves the sum of its `spans`/`scores` input sizes against free disk minus the other running jobs' reservations. A job that doesn't fit fails as deferred and a later pass retries it (§10, D-A1-9).

**Commit:** one catalog edit (§4) adds the outputs (`spans` with their `traces`, or `scores`) and the `file_sources` rows of the new pairs, and marks the inputs and the `traces` files derived from them `obsolete` with `obsoleted_at` = the committer's wall clock at commit. Shutdown stops compaction between slices or before a commit, never inside one. The committer refuses an edit that removes a `spans` file without its active `traces` file, or a `traces` file without every one of its sources.

**GC of obsolete files.** Every 10 min, GC deletes an obsolete object and then its `data_files` row (its `file_sources` rows go with it), once `obsoleted_at + gc.grace ≤ now` (`gc.grace`, default 5 min, at least the longest query timeout) **and** no `ReadState` of this process still references the file (reference counts on `FileMeta`). After a restart the counts are gone and the grace alone decides.

**Crash safety:** an output written but never committed is still in `staged_objects`. GC deletes staged objects older than 1 h and their rows, so the bucket is never listed. Startup removes leftover temp files: in the object directories in local mode, and in `<data_dir>/tmp/compaction/` in S3 mode. A job interrupted by a crash or a conflict is redone by a later pass.

## 6. Query path

For each request, `Storage::query` returns a `ProjectQuery`: the project's `TableProvider`s, all bound to one `ReadState` (catalog, memtable and tombstones, §4), plus a `RuntimeEnv` of the query's own, built from the process one at open (object store, metadata cache and spill directory) with a memory pool of its own (§ Resource limits, D-A1-8), `target_partitions`, and the physical optimizer rules the session appends (the trace list fast path, D153). `Storage::query` takes `QueryOptions` (`around`, D154). The query layer (05, D91) builds the DataFusion session from it. The providers expose the stored Arrow schemas of 01 (binary IDs, `project_id`). They don't register themselves: the query layer (05) registers the user-facing views over them (hex IDs, hidden `project_id`, `sessions`, default exclusions, D89). Because each provider is constructed for a single project, the user cannot name another project's data. A dropped project (§7) gets empty tables.

### `SpansProvider::scan(projection, filters, limit)`

1. Extract a time range from the filters on `start_time`:
   - the comparisons `=, <, <=, >, >=, BETWEEN`;
   - an `OR` gives the union of its sides' ranges; a side that is `start_time IS NULL` on the non-null column is dropped.

   Prune file partitions by `day`, widened by one day on each side (§ Trace anchor), and files by `min_time`/`max_time`. A `trace_id` lookup (equality or `IN` with 16-byte literals) without a time range, with `QueryOptions::around` set, reads only the `spans` and `traces` files that overlap `around ± 1 day` (D154); `scores` files are never pruned by `around`.
2. Build a `ParquetExec` (DataFusion's Parquet scan) over the remaining files, with filter pushdown on, page-index pruning, bloom-filter pruning (equality on `trace_id`, `session_id`, `user_id`) and the metadata cache.
3. Build a `MemoryExec` (in-memory scan) over the `active` and `frozen` batches of the same partitions. Same filters, applied in-memory.
4. Combine both with a `UnionExec`. Files and batches that a pending deletion may cover go through the **tombstone filter** (§7) first.

### `TracesProvider` (exposed as the logical view `traces`)

A `TracesExec` reads (a) the partial rows from `traces` files and (b) partials computed from the memtable span batches, hash-partitions them by `trace_id`, sorts each partition by `trace_id`, and merges each trace's run of partials with the merge operator: spec 01's rules, written once in Rust and shared with the flush and compaction (D152). Merged rows stream out as each run ends (D-A1-13). The order rules of spec 01 § Order make results independent of flush order. A SQL `GROUP BY trace_id` is not used: its ordered `first_value` and list-union aggregates cost several microseconds per trace (`docs/research/storage/traces-benchmark.md`).

- *Columns:* only the columns the query projects or filters on are merged, plus the order keys (`trace_id`, `start_time`, `has_root`, `root_span_id`).
- *Pushdown:* the provider accepts every filter as exact, so DataFusion keeps no filter above it. Only `start_time` ranges and `trace_id` equality / `IN` prune the partial scans. Every filter is evaluated on merged rows: a non-root partial usually has no session or user, and `cost_total > 0.1` can fail on every partial and pass on the merged row.
- *Time window:* when the filter bounds `start_time` to `[a, b]`, partials are scanned on `[a − trace_lookback, b + trace_lookback]` (default `trace_lookback` = 1 h), merged, and the merged `start_time` is then filtered to `[a, b]`. Memtable partials come from every memtable span batch of the project whose time range overlaps that padded window; each batch's partials are computed once and cached with the batch. A trace with partials more than 1 h apart may show up with a late start time; this is documented as a known limit.
- *Memory:* the sort holds the partials and spills to `tmp/query/` past its share of the query's memory (§ Resource limits). The merge holds the traces of at most 1,024 sorted partials at a time and reserves their size, unspillable. A range query's memory therefore doesn't grow with the traces in its window; its time and spill volume do. It needs about `partitions × (2 × input batch + 10 MiB)` (DataFusion's per-sort merge reservation) and fails as too large below that (D-A1-13).
- *Duplicates:* a span retried by a client and stored in two flushes is counted twice by the merge until compaction merges the two files (≈ 10 min, §5). Documented.
- *Fast path for "latest N traces"* (trace list, D153):
  - **Recognising it.** A physical optimizer rule, which `ProjectQuery.optimizer_rules` hands to the query layer's session, matches `ORDER BY start_time DESC, trace_id DESC LIMIT k` over the `traces` scan. Projections may sit in between, including the view's hex-encoded `trace_id`.
  - **Other queries.** A query with another sort, or with a filter DataFusion keeps above the scan (a score-function join), runs the full merge.
  - **Phase 1, rank.** The scan reads disjoint slices back from `b`, or from the newest data when `b` is open: 15 min, then up to 1 h, 6 h, 24 h, then the rest of the range. Each slice is padded by `trace_lookback`. In each slice, it merges only the filter's columns and collects the matching `(start_time, trace_id)`. It stops after the first slice that brings the total to `k`.
  - **Phase 2, fetch.** It merges all requested columns for the best `k` traces, each within its own slice's padded window, so their values are the ones phase 1 filtered on.
  - **Result.** The first `k` rows of the full merge, except for traces with partials more than `trace_lookback` apart: the known limit above.
  - TopK over a full `GROUP BY trace_id` does not bound the aggregate, which is why the rule exists.
- *Single-trace lookup* (`trace_id = X`), in order of preference:
  1. a time hint (`?around=<ts>`, always present in links generated by the UI, API list responses and SDK helpers; passed as `QueryOptions::around`, D154) narrows the search to files overlapping ±1 day;
  2. otherwise, trace-ID bloom filters select candidate files across all of the project's partitions — a few microseconds per file. The bitsets are not in the metadata cache (§4): a pruning read fetches each one from the object;
  3. post-MVP, if per-project file counts make (2) slow: either a `trace_index` table (trace_id → min/max day), sorted by `trace_id` and maintained at compaction, as LangWatch and Opik do; or one combined bloom-filter file per partition, laid out so a lookup is one ranged read per partition, as OpenObserve does.

  Within each candidate file, the `trace_id` sort order means the page index locates the trace's rows directly.

### `ScoresProvider` (logical view `scores`)

Scan, then `ROW_NUMBER() OVER (PARTITION BY score_id ORDER BY updated_at DESC) = 1`, then filter out `deleted`. Once a day has been closed, its files are already de-duplicated, and the plan skips the window step for them (a `fully_merged` flag per file).

### Resource limits

- **Memory:** one process-wide pool of `query.max_memory_total` (default 50% of RAM or of the cgroup limit, at most 8 GiB). Each query has a DataFusion memory pool of its own over it, capped at `query.max_memory` (default 2 GiB). Sorts, repartitions and spillable aggregates spill to `tmp/query/`; an operator that can't spill fails the query as too large (422). Within a query, each operator that can spill holds at most `(query.max_memory − memory held by operators that can't spill) / number of operators that can spill`, as DataFusion's `FairSpillPool`, and spills past it; operators that can't spill take what is left, first come first served (D-A1-13). Compaction has its own pool (§5) and is not counted here.
- **Admission:** `Storage::query` first takes `query.initial_memory` (default 64 MiB) from the process-wide pool, waiting up to `query.admission_wait` (default 5 s) for it, then fails as busy (503). The query's pool uses those bytes first and takes more as it grows; everything goes back when the query's runtime is dropped (D-A1-8).
- **Parallelism:** `target_partitions = cores`.
- **Timeouts:** UI 30 s, SQL console 60 s, MCP 30 s.

### S3 read cache

With an S3 object store, every page read is a ranged GET. The S3 store is wrapped by a cache store, so queries, compaction and the media GC all read through it (D-A1-11):
- data files (`tables/…`) are cached in 1 MiB blocks under `cache/<file name>/<block index>`, each ending with a CRC-32C of its bytes; a block that fails its check is fetched again. `cache/<file name>/size` holds the object size (one HEAD per file, the first time). Media blobs, writes and listings pass through;
- a range read serves the blocks it has and fetches the missing ones in one coalesced request;
- files are immutable, so entries never go stale;
- eviction is LRU by block, within `query.read_cache_bytes` (default 10% of the file system holding `data_dir`, at most 100 GiB; 0 turns it off);
- a delete through the store (GC) drops the file's blocks;
- flush and compaction write their fresh outputs into the cache after uploading them, since recent data is what gets read;
- the cache survives a restart: open rebuilds its index from the files, oldest first by modification time;
- it writes nothing while free disk is under `2 × storage.min_free_bytes`, and gives blocks back while free disk is under `storage.min_free_bytes` (§10);
- verification of a suspect file (§10) reads the S3 store directly, never the cache.

There is no custom `ParquetFileReaderFactory`; the metadata cache is DataFusion's built-in one (§4). With a local object store there is no cache.

## 7. Deletion (GDPR)

### Requests

A request is created by the server in `meta.db` (04), with its cascades, and handed to storage with `register_deletion`. It has a `kind`, a `created_at` that serves as the **cut-off**, and a resolved scope:

| Kind | Scope stored | Rows deleted |
|---|---|---|
| `traces` | `deletion_traces(trace_id…)`. A filter-based request is resolved to trace IDs when it is created. The set is extended with the **evaluation traces** whose `metadata['llmobs.eval.target_trace_id']` is in it (they contain copies of the evaluated data). | `spans` and `scores` rows of those traces with `ingest_time ≤ cut-off` |
| `user` | `user_id`, plus the trace IDs and session IDs resolved at creation, plus their evaluation traces | with `ingest_time ≤ cut-off`: `spans` rows of those traces or with that `user_id`; `scores` rows of those traces, with that `user_id`, or with one of those `session_id`s (D-A1-4) |
| `project` | `project_id` | everything in the project (`drop_project`) |
| `org` | `org_id` | every project of the org (`drop_project` for each), plus org-level metadata in `meta.db` |

Session IDs match `scores` only: a session shared with another user keeps that user's spans, and the user's own spans in it are already covered by the user's trace IDs and `user_id` (D-A1-4). `traces` rows follow from the `spans` rows (below).

The cut-off makes the result deterministic: data ingested after the request is new data, not covered by the deletion.

**Handoff.** `register_deletion` is idempotent by deletion id. It stores the request and its resolved scope in `catalog.db` with the next **`deletion_seq`** (1, 2, … across all projects), then publishes the tombstones in a new read state, and returns only after both. A request for a dropped project is stored as already applied, without its scope, and publishes nothing. The server:
1. creates the request in `meta.db`;
2. calls `register_deletion`;
3. sets `handed_over_at`.

At startup it re-registers every request not yet handed over, and storage ignores ids it already knows. The server polls `deletion_status` (`pending` or `applied`) to mark its own row `applied`.

### Making data invisible immediately

Pending `traces` and `user` deletions are kept in memory per project as the **tombstone set**: each deletion's `deletion_seq`, cut-off, trace IDs, user and sessions. One function turns a set of deletions into a DataFusion physical predicate "this row is covered" (D-A1-3); queries, flush and compaction all use it, so the rule exists once:
- `spans`: `ingest_time ≤ cut-off AND (trace_id IN … OR user_id IN …)`;
- `scores`: the same, `OR session_id IN …`;
- `traces` partials: `trace_id IN …` only. A partial carries no per-row ingest time, so a query hides a covered trace's partials whole; the files rewritten by the rewriter recompute their partials from the remaining spans.

Every `IN` term is guarded by `IS NOT NULL` (`scores.trace_id`, `user_id` and `session_id` are nullable), so the predicate is never null: a row with no `user_id`, or a session score with no `trace_id`, is never covered by a term on that column.

Known limits:
- `EXPLAIN` in the SQL console prints the predicate, so project members can see the IDs of a pending deletion until it is applied (accepted, D-A1-3).
- For a `user` deletion, the `traces` view hides partials by the resolved trace IDs only; the server resolves every trace with a span of the user ingested up to the cut-off.
- A row stamped with `ingest_time ≤ cut-off` but appended after its deletion is applied is neither dropped nor hidden. The server stamps `ingest_time` at receipt, so this needs a request in flight for longer than the deletion takes to apply.

A deletion may cover rows of a `spans` or `scores` file only when its `deletion_seq` is above the file's `deletions_applied_seq` and its cut-off is at or after the file's `min_ingest_time`; of a `traces` file, only when its `deletion_seq` is above the file's `deletions_applied_seq` (a `traces` file's `min_ingest_time` is the smallest per-trace *latest* ingest time, so it can't bound a cut-off); of a memtable batch, only when its cut-off is at or after the batch's `min_ingest_time`. The query path filters only those files and batches:
- memtable batches are filtered in memory when the read is planned (with fresh `traces` partials computed from what remains);
- files are scanned with the predicate's columns added to the projection, then a `FilterExec` keeps the rows not covered, then a projection restores the requested columns. Other files are scanned as usual.

`project` and `org` deletions are immediate: `drop_project` marks the project dropped in `catalog.db` and removes it from every new read state in the same commit; the server returns 404 for it.

### Cascades, inside the server's request transaction (`meta.db`)

- Dataset items whose `source_trace_id` is in scope are hard-deleted, all versions.
- `experiment_items` and `eval_jobs` referencing those traces are deleted.
- `alert_events` hold only aggregates and are kept.

### Physical erasure: the deletion rewriter

Every file records `deletions_applied_seq`: no row in scope of any deletion with `deletion_seq` up to that number remains in it.
- A flush sets it to the highest `deletion_seq` known when it started, since it drops every row the pending deletions cover (§2). A flush left with no row commits only its `flushed_seq`.
- Compaction and the rewriter set it to the highest `deletion_seq` known when the job started, since they apply all of those. A `traces` output gets the value of its `spans` output.

After each compaction pass, the compaction task runs a **deletion pass** (D-A1-5):

1. **Candidates:** the active `spans` and `scores` files that a pending deletion may cover (rule above), skipping files claimed by a running job. The pass claims them.
2. **Check:** a scan of the file's key columns with the predicate pushed down (statistics, and the bloom filters on `trace_id`, plus `user_id` and `session_id` for `spans`, skip row groups) and `LIMIT 1`.
   - A covered row: the file is **rewritten** without its covered rows by a compaction job of kind `rewrite`: same level, same `fully_merged`, the `traces` file derived from it recomputed (§3), committed like a compaction (§4). A file left with no row is removed with its `traces` file and nothing replaces it.
   - No covered row: its `deletions_applied_seq` is raised to the known sequence in one catalog edit, without a rewrite.
   - **Quarantined files** (§10) can't be rewritten: the quarantined `spans` and `scores` files of the project that a pending deletion may cover (same rule) are marked `obsolete`, with the quarantined `traces` files derived from them, and GC deletes them.
3. **Applied:** a deletion is applied once:
   - every active `spans` and `scores` file of the project with `min_ingest_time ≤ cut-off` has `deletions_applied_seq ≥` its `deletion_seq` (`traces` files follow their source `spans` files, §3);
   - no memtable batch of the project has `min_ingest_time ≤ cut-off`. A batch that held covered rows is dropped by the next flush, which writes none of them.

   Both are read from one published read state, so a flush committing in between cannot make a deletion look applied. Then `status = 'applied'`, `applied_at` is set, its scope rows (`deletion_traces`, `deletion_users`, `deletion_sessions`) are deleted, its tombstone entries are removed, and media GC runs for the project (§9). An applied request keeps only its id, project, kind, cut-off, sequence and dates; the audit record is the server's `meta.db` row.

Compaction and the rewriter never run on the same file (claims, §4), and a compaction that started before a deletion records an older `deletions_applied_seq`, so its outputs remain candidates. Deleted rows cannot come back through a merge.

**Guarantee:** the bytes are gone from Parquet files within `deletion.max_delay` (default 24 h). A pending deletion older than that is logged once as a warning. The WAL holds data only until the next flush checkpoint (minutes), so it ages out by itself. The meaning of GDPR deletion for backups is covered in 12 §Backups.

### Project drop

`drop_project(project)` (D-A1-7):
1. waits for appends in flight, and holds new ones, while it commits;
2. commits one edit that obsoletes every file of the project in all three tables (quarantined files included), records the project as `dropping` in `projects`, removes its retention watermark, and marks its pending deletions applied and erases their scope rows;
3. publishes the read state without the project: its queries return empty tables, and `append_*` for it fail with `ProjectDropped`;
4. deletes its media objects and `media` rows, then sets the project `dropped`.

GC removes the obsolete files like any others (§5), so no listing of the `project=` prefixes is needed. If step 4 fails or the process stops, the GC task finishes it. Rows of the project still in the WAL or memtable are dropped by the next flush, and the committer refuses any file of a dropped project (§4). A `project_id` is never reused, so a dropped project is never re-created.

## 8. Retention

Each project has `retention_days` in `meta.db` (null means keep forever). The server calls `apply_retention(project, keep_from = today − retention_days)` hourly. Storage then:
- commits one catalog edit that obsoletes every file of the project in days before the watermark, across `spans`, `traces` and `scores`, quarantined files included (no file is rewritten, because partitions are per day);
- records the watermark in `retention_watermarks`, in the same transaction. The watermark is the later of the stored one and `keep_from`: it never moves back.

From then on, a flush drops rows of that project older than the watermark, queries don't show memtable rows older than it, and the committer refuses a file older than it (§4), so late rows cannot recreate a dropped day. Memtable `spans` are keyed by each row's own day and files by the anchor day, so a span next to midnight at the watermark can be hidden in the memtable and kept by the flush, or the reverse; the file is what counts. Retention is day-granular. Since a trace lives in one anchor day (§ Trace anchor), it is dropped as a whole.

## 9. Media

Media blobs live under `media/<project_id>/<sha256>` (64 lowercase hex) in the object store; their index is the `media` table of `catalog.db`.

`put_media` checks that the SHA-256 of the bytes matches the given hash, and fails otherwise (D-A1-6). For a hash already stored it only refreshes `last_seen_at`. Otherwise it records the path in `staged_objects`, writes the blob durably (local: temp file, `sync_all`, rename, directory fsync; S3: a plain `PutObject`, since the name is the content's hash and an overwrite writes the same bytes), then inserts the `media` row and removes the staged row in one transaction. Staged-object GC deletes an object left without a row, and leaves alone a staged path whose `media` row exists. `get_media` reads a blob, whole or a byte range.

**GC** runs daily, and for one project right after one of its deletions is applied (§7). For each project, it reads only the `media_refs` column of `spans` (a cheap projection, through the same read path as queries, so rows hidden by tombstones don't count) to build the set of live hashes. It then deletes the objects and `media` rows that are absent from the set and whose `last_seen_at` is more than 1 h old. The age guard covers a span appended just after its blob; since every `put_media` refreshes `last_seen_at`, a blob reused by new spans is never collected under them (D-A1-6).

A dropped project's media is deleted by `drop_project` (§7).

## 10. Failure handling

| Situation | Behaviour |
|---|---|
| Free disk under `storage.min_free_bytes` (default 2 GiB) | Free disk is measured every second: the least free of the file systems holding `data_dir` and, in local mode, the object store root. Below the threshold `append_spans`, `append_scores` and `put_media` fail with `DiskFull` (503). Flush, compaction, GC, retention and deletions keep running: they reclaim space. A compaction job reserves its input size before it starts and is deferred to a later pass when it doesn't fit (§5); flushes don't reserve (D-A1-9) |
| WAL write or fsync error | pending and new appends fail with `WalFailed` (503), health reports it, restart required (§1) |
| Object store errors during flush | retry after 1 s, doubling up to 60 s (§2), while the memtable keeps the data and backpressure applies past the hard limit |
| Ambiguous upload (error reported, object may exist) | retried under a new name; the old one is collected from `staged_objects` (§2) |
| Crash | WAL replay before ingestion opens; staged objects are GC'd |
| Corrupted Parquet file | A read that fails with a decode error makes the files it read **suspects**: compaction and rewrite jobs (their inputs), the deletion pass's check scan, the anchor rebuild at open (which skips the file instead of failing), the media GC and queries (every file the query scanned). After each deletion pass, a verify pass reads each suspect again, whole, from the object store (never the read cache). A store error puts it back in the queue; a file read whole with the catalog's row count is healthy and isn't re-verified for an hour; a decode error, a wrong size or row count, or a missing object makes it **corrupt**, logged as an error. A corrupt `spans` file is quarantined with its `traces` file, a corrupt `scores` file alone: state `quarantined`, out of every snapshot, object kept for inspection, its rows lost. A corrupt `traces` file is rebuilt from its `spans` file by a rewrite job instead; nothing is lost. Quarantined files become `obsolete` (and GC deletes them) when retention drops their day, when their project is dropped, or when a pending deletion may cover them (§7). There is no periodic scrub (D-A1-10) |

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
