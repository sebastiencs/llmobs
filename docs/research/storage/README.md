# Storage research

What other systems do for the problems spec 03 (storage) and spec 05 (query) solve, gathered before the A1 storage design. Six read-only studies, one per source. The per-source notes in this folder hold the details and the file/commit citations. This page is the synthesis.

Nothing was copied from these codebases (AGENTS.md § Reference implementations). Where a note says "unverified", the claim could not be checked in the checkout.

## Sources

| Source | Notes | Snapshot | Storage model | Why it matters |
|---|---|---|---|---|
| GreptimeDB (mito2) | [greptimedb.md](greptimedb.md) | `edbcb8224`, 2026-09-21 | WAL → Arrow memtable → Parquet SSTs, manifest of delta files, TWCS compaction, own scan operator on DataFusion | Closest engine to ours; same DataFusion 55 / arrow 59 |
| Parseable | [parseable.md](parseable.md) | `503fa56`, 2026-09-18 | Arrow IPC staging files → per-minute Parquet on object store, JSON manifests, no compaction | Same dependency family (DataFusion 55, object_store 0.13.2); a catalogue of what goes wrong without fsync and compaction |
| OpenObserve | [openobserve.md](openobserve.md) | `8dcaf5e8`, 2026-09-20 | WAL → memtable → local Parquet → mover to object store; `file_list` table in SQLite/Postgres | Its traces design (per-trace partial rows merged by compaction) is the nearest to our `traces` table |
| Quickwit | [quickwit.md](quickwit.md) | `c02cc1c8a2`, 2026-09-18 | WAL (mrecordlog) → tantivy splits; Postgres or file metastore; new Parquet + DataFusion metrics path | Metastore lifecycle, exactly-once checkpoints, delete opstamps, failpoint and TLA+ testing |
| VictoriaLogs | [victorialogs.md](victorialogs.md) | `8befc88` (v1.52.0+) | No WAL; in-memory parts in on-disk format; LSM merges; per-day partitions; `parts.json` catalog | Lean resource use; crash-safe file publishing; merge write-amplification rule |
| Competitors | [competitors.md](competitors.md) | clones of 2026-11-09 | Phoenix and MLflow (SQL rows), LangWatch (Elasticsearch + event store); Langfuse, Opik, Helicone, Laminar on ClickHouse | How trace aggregates, late spans, sessions, filters and deletes are handled in practice |

## What the research confirms in spec 03

- **A real WAL with group commit is worth it.** None of the engines fsyncs before acknowledging by default (GreptimeDB syncs every 5 s, Quickwit flushes without fsync, OpenObserve and Parseable don't sync, VictoriaLogs has no WAL). Each pays with a data-loss window and, for Parseable and OpenObserve, a history of data-loss bugs. Group commit makes the fsync cheap. Our 120 s memtable and Parquet-on-S3 target rule out VictoriaLogs' "flush tiny parts every few seconds" model.
- **SQLite catalog over JSON manifests on the object store.** Parseable's per-day JSON manifests needed compression, ranged reads, blocking-pool decoding, and still had an overwrite data-loss bug. OpenObserve's `file_list` table and Quickwit's Postgres metastore are the working designs.
- **One transaction for "new files + WAL checkpoint".** Quickwit publishes splits and advances its checkpoint in one transaction, the same as our `data_files` + `flushed_seq` commit.
- **Compaction is required.** Parseable has none and its manifests drown in per-minute files.
- **Typed span schema.** Phoenix and MLflow spent years moving tokens, cost, session, user and model out of JSON into typed columns. Parseable flattens every attribute into its own column and hit type conflicts and a 1,000-column cap. Our typed columns plus attribute maps are right.
- **Per-trace partial rows merged on read** (our `traces` table). Laminar (`traces_agg`) and OpenObserve (trace time index) do the same. Langfuse tried it and dropped it, Phoenix, Opik and Langfuse compute aggregates from spans on read.
- **Sessions computed on read.** Every competitor does; Helicone dropped its materialized session table, and Phoenix's session table costs it lock races and orphan sweeps.
- **Tombstones hide deleted rows immediately.** VictoriaLogs and Quickwit only delete by rewriting, so deleted rows stay visible until the rewrite finishes. Nobody else offers per-user GDPR deletes.
- **Trace-ID bloom filters and binary IDs.** Parseable has neither and scans the whole time range for one trace.

## Gaps and problems found in spec 03

Each gap lists the evidence and a proposed resolution. The A1 design doc decides them; the ones that need a user decision are marked **(decision)**.

### Durability and recovery

**G1. Flushed Parquet files are never fsynced on local disk.** `object_store` 0.13.2's `LocalFileSystem` (our pinned version) writes a staging file and renames it, with no `sync_all`/`sync_data` anywhere in `local.rs` (checked). Opt-in fsync arrives in 0.14. So after a power loss, the catalog can point at an empty or partial file whose WAL segments were already deleted. Parseable's recurring zero-length/invalid Parquet bugs and VictoriaLogs #505 are this bug. *Resolution:* write local objects ourselves (temp file, `sync_all`, rename, fsync of the directory) until the family moves to object_store ≥ 0.14; fsync `wal/` on segment create and delete.

**G2. `synchronous=NORMAL` (spec 04) can roll back the `flushed_seq` commit on power loss**, after the WAL segments it covers were deleted. *Resolution:* the catalog writer commits with `synchronous=FULL`, or segments are deleted only after a later, durable checkpoint.

**G3. Startup ordering and sequence numbers.** OpenObserve lost acknowledged writes when replay ran after the server opened (#14971). GreptimeDB restarts sequences from the manifest. *Resolution:* replay completes before ingestion opens; an exclusive `flock` on the data directory; the next seq is `max(last WAL seq, flushed_seq) + 1`; `flushed_seq` only moves forward (compare-and-set, as Quickwit rejects backward checkpoints).

**G4. WAL I/O errors are unspecified.** *Resolution:* on a failed write or fsync, the writer fails every pending ack, refuses further writes (no fsync retry: the page cache state is unknown after a failed fsync), and the server reports unhealthy. The write-and-sync step is not cancellable.

**G5. WAL volume.** Uncompressed Arrow IPC is 5–7× the compressed Parquet output. OpenObserve compresses WAL bodies with snappy, Parseable with LZ4. *Resolution:* Arrow IPC body compression (LZ4 frame) in WAL records.

### Memtable and read consistency

**G6. Memtable accounting.** OpenObserve's size-based rotation never fired: with one-row batches, Arrow memory was 13× the JSON size (#13421). *Resolution:* account with Arrow's actual memory size and coalesce small batches per partition.

**G7. Query/flush races.** OpenObserve had data visible nowhere during a rotation (#16645) and double-counted during a move (#16642); Parseable has a documented double-visibility window. Spec 03's frozen-memtable swap is right, but it needs readers to take the memtable and catalog snapshot together. *Resolution:* one snapshot object (catalog + memtable + tombstones) published atomically, and a deterministic race test with failpoints.

### Catalog and file lifecycle

**G8. Concurrent rewriters.** Flush, compaction, the deletion rewriter and retention all commit file changes. GreptimeDB and Quickwit check that inputs are still live at commit time. *Resolution:* one committer that serializes the SQLite commit and the snapshot publish; each commit checks that its input files are still `active` (compare-and-set); a job claims its input files so compaction and the deletion rewriter don't pick the same file.

**G9. Deleting obsolete files.** A fixed 10-minute grace breaks long queries. GreptimeDB and VictoriaLogs delete a file only when no snapshot references it. *Resolution:* reference-count files through snapshots, plus a grace period on S3 (a crash loses the counts), at least the longest query timeout.

**G10. Orphan detection by listing the bucket.** Quickwit writes the catalog row as `staged` before uploading, so orphans are old staged rows. *Resolution:* staged rows instead of listing; listing remains only for local disk recovery if needed. Also store each file's footer offset and size so opening needs no HEAD request.

**G11. Many small files with many projects.** Partitioning by `(project, day)` writes one L0 file per active project per flush, for both `spans` and `traces`, uploaded one after another. GreptimeDB built a separate engine for this problem. **(decision)** Keep per-project files and upload a flush's files in parallel under one commit, or let L0 files hold several projects and split by project at L0→L1.

### Compaction

**G12. Write amplification and resources.** A small project's growing L1 file gets rewritten every ~10 minutes (O(n²) bytes per day). Merges sort a whole output slice in memory (several GiB decoded for 512 MiB compressed). Compaction needs disk space before it frees any. *Resolution:* a size-ratio rule (VictoriaLogs merges only when the output is ≥ 7.5× the largest input; Quickwit marks splits mature by age), a memory bound on merges (sort per row group, or external merge of sorted runs), and a disk-space reservation for merge outputs.

**G13. Late data in closed days, and retention.** OpenObserve's late files for closed hours stayed small forever (#13880). VictoriaLogs remembers dropped days so late rows can't recreate them. *Resolution:* new L0 files in a closed day re-trigger a day close; spans older than the project's retention are dropped at ingest and at flush.

**G14. Absurd timestamps.** LangWatch had an incident when a zero start time landed in an expired 1970 partition. **(decision)** Reject, clamp, or partition by ingest time.

### The `traces` view

**G15. The trace time window only pads backwards.** Spec 03 §6 scans partials on `[a − 1h, b]`. Partials of a trace starting in `[a, b]` that were written after `b` are missed, so tokens, cost, end time and errors come out low. Laminar pads ±3 h. *Resolution:* pad both sides.

**G16. Filters can't be pushed into partials.** A non-root partial usually has no session or user, and `cost_total > 0.1` can fail on every partial and pass on the merged row. LangWatch hit this. *Resolution:* only time and `trace_id` filters go to partials; other filters apply after the merge (or: find candidate trace IDs, fetch all their partials, merge, filter).

**G17. Duplicate spans inflate trace totals for good.** Compaction dedups `spans` rows, but each `traces` partial already counted the duplicate. Laminar's code says so. *Resolution:* compaction rebuilds `traces` partials from the deduplicated spans instead of merging partials, plus a short-window dedup of recent span IDs at ingest.

**G18. Merge determinism.** "First non-null" and "several roots" depend on flush order. *Resolution:* order by `(start_time, span_id)` everywhere.

**G19. Traces that cross midnight.** Per-day compaction never collapses them to one partial, and retention deletes half. LangWatch moved to a fixed "first seen" anchor after the same problems. **(decision)** Accept and document, or anchor a trace's partials to one day.

### Deletion

**G20. The deletion rewriter never finishes.** A rewritten file keeps its `min_ingest_time`, so it stays a candidate for ever. And a compaction that started before a deletion can commit after it was marked applied, bringing rows back. *Resolution:* a per-file `deletions_applied_seq` (Quickwit's delete opstamp; a merge output takes the minimum of its inputs), plus the compare-and-set commit of G8.

### Query resources

**G21. Memory pool per query.** 2 GiB per query × concurrent queries can exceed RAM. *Resolution:* one process-wide DataFusion pool with a per-query cap, and a short admission wait before answering 503 (Parseable waits 10 s).

**G22. Metadata cache.** DataFusion 55 has a file metadata cache (`with_metadata_cache_limit`). A custom `ParquetFileReaderFactory` may only be needed for bloom filters. *Resolution:* check in M1 before writing our own.

**G23. Single-trace lookup without a time hint.** OpenObserve aggregates the bloom filters of all files of an hour into one file, so a `trace_id` lookup is one ranged read per hour. A candidate for the post-MVP `trace_index` of spec 03 §6.

### Testing

**G24.** Every engine with a good record tests failure paths explicitly. *Resolution:* failpoints at each stage of flush, compaction and deletion (Quickwit, GreptimeDB); a test that counts fsyncs; an object store that reports failure after a successful write (GreptimeDB `1cb01d0d4`); kill -9 tests across "Parquet written → SQLite commit → WAL deletion"; model-checked invariants such as "rows in = rows out" (Quickwit's Stateright models).

## For the filter DSL and query track (A2), not A1

- **"Trace contains a span where…"**: Phoenix, LangWatch and MLflow all express it. A `has_span(...)` function compiled to a semi-join on `spans` covers it.
- **Safety limits** on the DSL: nesting depth, expression size, `IN` list size, NUL characters.
- **Sessions view over a time window**: Phoenix lists sessions that overlap the window. A plain `GROUP BY` over the window gives partial totals. Spec 05 should say which.

## Rejected alternatives

- **No WAL, small L0 files every 2–5 s, relying on `(trace_id, span_id)` dedup** (VictoriaLogs-style). Cheaper writes on local disk, but it multiplies small files on S3 and gives a data-loss window.
- **JSON manifests on the object store** (Parseable). See above.
- **A separate "mover" stage that uploads local Parquet later** (OpenObserve). It duplicates rows when a crash lands between registering and deleting the local file.
- **Hiding deletes only by rewriting** (VictoriaLogs, Quickwit). Deleted data stays visible for hours.
- **One column per attribute** (Parseable). Type conflicts and column caps.
