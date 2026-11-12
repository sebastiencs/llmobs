# A1 · M3a — Traces: write path

First half of milestone M3 (traces) of the storage track. The second half is `06-m3b-traces-read.md`. One agent does the sub-phases of both files in order, on one branch (`a1/m3-traces`). Each sub-phase ends with `just check` green and one commit, and M3 lands as **one PR**.

**Prerequisites:**
- M2 is merged (PR #18).
- **For M3.8 only:** `docs/research/storage/traces-benchmark.md` exists on `main`. It comes from the branch `docs/s1-trace-bench-results`, which the user merges. M3.1–M3.7 don't need it. Its findings are summarized in `00-design.md` § S1 outcome. If the file is still missing when M3.8 starts, stop and ask.
- The A2 query layer (`src/views/`, `src/reads/`, `src/filter/`, `src/sql/`, `src/query_*.rs`, `src/test_support.rs`) is on `main` and must keep passing.

**Read first:**
- `AGENTS.md`, all of it.
- `docs/plans/a1-storage/00-design.md`: § Trace anchor, § Trace partials and the `traces` view, § Trace-list spike (S1) and its outcome, § Commit protocol, § Testing.
- `docs/spec/01-data-model.md` § Trace: the merge table and § Order.
- `docs/spec/03-storage.md`: § Layout, § Trace anchor, §2 Flush, §3, §4, §6.
- `00-design.md` § S1 outcome: why the merge is hand-written and how latest-N works. The full numbers are in `docs/research/storage/traces-benchmark.md` once it is merged.
- `docs/mvp-decisions.md`: D84, D86, D87, D88, D89, D91, D152, D153, D154.
- The M2 code: `crates/storage/src/{flush.rs,memtable.rs,parquet.rs,catalog/,query/,hooks.rs}` and `tests/crash.rs`.
- A2's reference trace derivation, `crates/storage/src/test_support.rs` (`trace_batch`, `TraceColumns::append`). It is the oracle for M3's merge tests. Read it; don't change it.

## What M3 delivers

After M3, `traces` is a real table:
- every flush writes, for each `spans` file, one `traces` file of trace partials derived from it, and records the pair in `file_sources` (D88);
- spans and partials are partitioned by the trace's anchor day (D86);
- `Storage::query` returns a `TracesProvider` that merges partials from files and from the memtable with a hand-written operator (D152);
- the trace list's "latest N" query runs in two phases over widening windows (D153);
- single-trace lookups use the `around` hint or bloom filters (D154);
- the S1 latency targets are measured on the real implementation.

**Still missing after M3:**
- Compaction and day close (M4). Partials accumulate one per `(trace, flush)` until then, and a span stored by two flushes is counted twice by the merge.
- Deletion, retention and media (M5).
- The process-wide memory pool, admission and the S3 read cache (M6).

## Decisions this plan relies on

| # | Summary |
|---|---|
| D86 | Spans and partials are partitioned by the trace's anchor day. Kept after S1: the rebuild costs 51 ms and 4 MB on the benchmark data. |
| D88 | Every `traces` file is derived from `spans` files, recorded in `file_sources`. |
| D89 / D91 | Providers expose stored schemas; A2 builds the session and the views. |
| **D152** | **Trace merge operator.** The merge rules of spec 01 are implemented once, in Rust (`partials.rs`), and used to build partials at flush, to build memtable partials at query time, and to merge partials in queries (and by M4's compaction). The `traces` provider accepts every filter as exact: `start_time` ranges and `trace_id` equality also prune the partial scan, and every filter is evaluated on merged rows. Only the columns a query needs are merged. |
| **D153** | **Trace list fast path.** A physical optimizer rule, handed to A2's session through `ProjectQuery`, recognises `ORDER BY start_time DESC, trace_id DESC LIMIT k` over the `traces` scan. The scan then reads disjoint slices back from the newest time (15 min, 1 h, 6 h, 24 h, the rest). In each slice it ranks on the columns the filter needs, and it fully merges only the top `k` traces. |
| **D154** | **Lookup hint.** `Storage::query(project, QueryOptions { around })`. With `around`, a `trace_id` lookup without a time range reads only files overlapping `around ± 1 day`, in `spans` and in `traces`. |

User answers behind this plan: keep the anchor; a Rust merge operator; the S1 targets checked by an ignored benchmark test in M3; one PR.

## Versions

No new dependency is expected.
- `proptest` and `tempfile` are already dev-dependencies of `llmobs-storage`.
- `parquet` is a normal dependency with the `async` and `object_store` features. Integration tests in `crates/storage/tests/` can use it directly.

If something else turns out to be needed, add it to the root `[workspace.dependencies]`, re-check it with `cargo info`, and report the exact version.

## Conventions

M1's and M2's conventions still apply:
- blocking I/O in `spawn_blocking`;
- one `thiserror` enum per operation;
- no row content in errors or logs (trace ids are fine; names, previews and attribute values are not);
- no panics on disk input;
- test hooks instead of sleeps;
- the lock order committer → memtable → `Published`;
- one function builds object paths;
- catalog writes go through the committer.

On top of that:
- **One merge implementation.** Every place that combines spans or partials into a trace row calls `partials.rs`. No SQL `GROUP BY trace_id` in storage code.
- **Two table enums.** The WAL `Table` (`Spans`, `Scores`) stays as it is: it names WALs, memtables and `flushed_seq`. Files get their own `FileTable` (`Spans`, `Traces`, `Scores`), because `traces` has files but no WAL.
- **A2's files.** `views/session.rs`, `views/mod.rs` (`QueryContext::new`) and `test_support.rs` (`example_query`) get the small changes M3.5 and M3.7 describe, and nothing else. Name them in the report. The same goes for every other file that only changes because `Storage::query`, `ProjectQuery` or `StorageConfig` changed shape.
- **Struct literals.** `StorageConfig`, `QueryConfig` and `ProjectQuery` are built as struct literals in tests: `src/` tests, `tests/crash.rs`, `tests/s3.rs`, and later `tests/trace_bench.rs`. A sub-phase that adds a field updates every literal in the same commit. Search for the type name to find them all.
- **Floating-point sums.** `cost_total` is summed in input order, so two orders can differ in the last bits. Every test that compares `cost_total` between two merges, or with the oracle, compares within a relative 1e-9. Every other column compares exactly.
- **No new hook points.** `HookPoint` keeps its current variants in M3. The object hooks (`ObjectWrite`, `ObjectSync`, `ObjectRename`, `ObjectDirSync`) already fire once per object, traces files included.

## Final layout after M3

```
crates/storage/
  migrations/catalog/0002_file_sources.sql   file_sources
  src/
    config.rs        + query.trace_lookback, anchor.window
    catalog/         + FileTable, file_sources rows, edit sources, open-time check
    parquet.rs       + traces writer settings, keyed by FileTable
    partials.rs      TraceAcc: spans → partial, partials → merged trace (spec 01)
    anchor.rs        anchor days near midnight, rebuild at open
    flush.rs         + anchor partitioning, traces files, sources in the commit
    memtable.rs      + cached partials per memtable batch
    query/
      mod.rs         + QueryOptions, day slack, around, OR bounds
      spans.rs       + day slack, around
      traces.rs      TracesProvider, TracesExec (range, lookup, list)
      trace_list.rs  TraceListRule
    lib.rs           query(project, options); ProjectQuery.optimizer_rules
  tests/
    crash.rs         + traces/source invariants, midnight traces
    trace_bench.rs   #[ignore] S1-shaped benchmark through the real engine
```

---

## M3.1 — Catalog and Parquet for `traces`

**Goal:** the catalog and the Parquet writer know the `traces` table and `file_sources`. Nothing writes traces yet.

### `FileTable`

In `catalog/mod.rs`:

```rust
pub(crate) enum FileTable { Spans, Traces, Scores }
```

- `as_str()` returns `spans`, `traces` or `scores`. It is the `data_files.table_name` value and the `tables/<table>/` path segment.
- `From<Table>` maps the two WAL tables.
- `FileMeta.table` becomes a `FileTable`. Update every caller:
  - catalog load (parse `table_name`; an unknown value is a catalog error, not a panic);
  - the committer's insert;
  - the providers' file selection;
  - GC;
  - the path builder in `objects.rs`;
  - `parquet.rs`;
  - `Catalog::files`, `Catalog::test_files`, and the test helpers that took a `Table`.
- **The in-memory catalog** (`Tables`) becomes an array of 3, indexed by `FileTable` (`spans` 0, `traces` 1, `scores` 2). `table_index(Table)` stays for the `flushed_seq` arrays in `committer.rs`, which have one slot per WAL (2). The two indexes must not be mixed: give each its own function, documented as such.

### Parquet settings

`parquet.rs` functions take a `FileTable`. `traces` settings, from spec 03 §3:
- rows sorted by `start_time`, then `trace_id`. The tie-break makes files byte-identical across runs. Edit spec 03 §3's sort-order row to say so.
- `write_file` sorts every table's batches, as it already does for spans. Partials arrive sorted, and re-sorting them costs little: no special case.
- sorting columns declared in the metadata;
- time column `start_time`, ingest column `last_ingest_time`. So a traces file's `min_time` / `max_time` are the smallest and largest partial `start_time`, and its ingest range is the partials' `last_ingest_time` range. Both differ from its spans file's ranges, which is expected.
- bloom filters on `trace_id`, `session_id` and `user_id`, fpp 0.01;
- dictionary columns: a hard-coded `TRACES_DICTIONARY_COLUMNS`, like the spans and scores lists. Use `status`, `environment`, `release` and `name`, the low-cardinality strings. Add a test that the list names real `traces_schema()` columns, like `dictionary_lists_match_the_schemas`.
- the same KV metadata keys, with `llmobs.table = traces`.

Add a round-trip test like `spans_round_trip_sorted_with_the_file_settings`, for `traces`.

### `file_sources`

`migrations/catalog/0002_file_sources.sql`:

```sql
CREATE TABLE file_sources (
    traces_file_id TEXT NOT NULL REFERENCES data_files(id) ON DELETE CASCADE,
    spans_file_id TEXT NOT NULL REFERENCES data_files(id) ON DELETE CASCADE,
    PRIMARY KEY (traces_file_id, spans_file_id)
);

CREATE INDEX file_sources_by_spans ON file_sources (spans_file_id);
```

The foreign keys are spec 03 §4. `ON DELETE CASCADE` drops source rows when M4's GC deletes an obsolete `data_files` row; a spans file and the traces files derived from it become obsolete in the same commit, so the cascade does not remove a source that is still active. The index covers that cascade and M4's lookup from a spans file to its traces file. `catalog/db.rs` already sets `foreign_keys`.

`CatalogEdit` gains `sources: Vec<(FileId, FileId)>`, as (traces file, spans file).
- **Precondition.** Each traces file is in `add` with table `Traces`. Each spans file is in `add` with table `Spans`, or is an active `spans` row. Otherwise the edit is a conflict and nothing is written.
- **Write.** The rows are inserted in the same transaction as `add`.
- **Memory.** No in-memory index of sources in M3: nothing reads them at query time. M4 adds one when compaction needs it.

**Check.** `check_file_sources` reads `catalog.db` and enforces spec 03 §3:
- every active `spans` file is the source of exactly one active `traces` file;
- every active `traces` file has at least one source, and all its sources are active `spans` files of the same `(project, day)`.

A violation is `CatalogError::SourcesInvariant { file }` (the offending file id only). There is no migration path for spans files without traces files (AGENTS.md § Project). M3.1 tests the function on catalogs built with direct SQL. `Storage::open` does not call it yet: flush still writes spans files with no traces files, and calling it would fail every reopen. M3.3 calls it from open.

### Tests

- The migration applies on a fresh catalog and on an M2 catalog. Test it through the function that opens `catalog.db` and runs migrations (`db.rs`), not through `Storage::open`.
- A commit with sources inserts its rows. A source pointing at an unknown file is a conflict, and the transaction leaves no row.
- Deleting a `data_files` row removes its `file_sources` rows.
- `FileTable` round-trips through `data_files.table_name`. An unknown name fails the load with an error.
- **The sources check** rejects:
  - a spans file without a traces file;
  - a spans file with two traces files;
  - a traces file whose source is in another day.

  Build those catalogs with direct SQL in the test.

**Commit:** `storage: add traces files and file_sources to the catalog`

---

## M3.2 — The merge operator (`partials.rs`)

**Goal:** spec 01's trace merge, implemented once (D152). Two entry points share the same accumulator.

```rust
/// One partial per trace of `spans` (spans schema, one project, any row order).
/// Rows: traces_schema(), sorted by (start_time, trace_id), partial_count = 1.
pub(crate) fn partials_from_spans(spans: &RecordBatch) -> Result<RecordBatch, PartialsError>;

/// Merges partial rows. Input batches carry `MergeColumns::input()` columns of traces_schema();
/// the output carries `MergeColumns::output()`, one row per trace, in no particular order.
pub(crate) struct TraceMerger { /* HashMap<TraceId, TraceAcc>, columns */ }
impl TraceMerger {
    pub(crate) fn new(columns: MergeColumns) -> Self;
    pub(crate) fn push(&mut self, partials: &RecordBatch) -> Result<(), PartialsError>;
    pub(crate) fn len(&self) -> usize;
    /// Heap bytes held, for the query memory reservation.
    pub(crate) fn size(&self) -> usize;
    pub(crate) fn finish(self) -> Result<RecordBatch, PartialsError>;
}
```

- **Columns.** `MergeColumns` is a set of `traces` column names, built from the columns a query asks for. It is closed under these rules:
  - `output()` is the requested set plus `trace_id` and `start_time`. Those two are always produced, because the window filter and the list mode need them.
  - `input()` is `output()`, plus the dependencies below, plus the order keys `trace_id`, `start_time`, `has_root` and `root_span_id`.
  - Dependencies: `status` needs `status` and `error_count`. `name`, the four context fields, the previews and `metadata` need only themselves, because the order keys are always there. Sums, unions, `end_time` and `last_ingest_time` need only themselves.
  - A column in `input()` that isn't in `output()` (for example `error_count` pulled in by `status`) is accumulated but not output.

  A merger never reads or builds any other column. That is what makes the list query's ranking phase cheap.
- **Input batches** are partial rows with any subset of `traces_schema()` columns, in any order, as long as `input()` is covered. The merger finds each column by name, once per batch, and downcasts it once per batch. A required column that is missing, or has the wrong type, returns `PartialsError::Column { name }`.
- **Output** is a batch with the `output()` columns in `traces_schema()` order.
- **Accumulator.** `TraceAcc` holds one `Option` per column. Strings are owned. Lists and maps are kept as `BTreeSet` and `BTreeMap` until `finish`.
- **`size()`** is an estimate for the memory reservation. Per trace: `size_of::<TraceAcc>()` + 16 (key) + 32 (hash-map overhead), plus the heap bytes of every owned string, set element and map entry. Keep it as a running total updated in `push`, not recomputed. It only grows.

### Rules

Span level is a partial built from spans. Merge level is a trace merged from partials. The span-level rules are A2's `test_support::TraceColumns::append`, which is the oracle.

| Column | Span level (spans → partial) | Merge level (partials → trace) |
|---|---|---|
| `project_id`, `trace_id` | key | key |
| `start_time` | min | min |
| `end_time` | max | max |
| `last_ingest_time` | max `ingest_time` | max |
| `has_root` | any span with null `parent_span_id` | or |
| `root_span_id` | the root span: null parent, smallest `(start_time, span_id)` | from the root partial (below) |
| `name` | root span's, else the earliest span's by `(start_time, span_id)` | root partial's, else the earliest partial's |
| `status` | `error` if any span has status `error`; else root's status; else `unset` | `error` if any `error_count > 0`; else root partial's; else `unset` |
| `session_id`, `user_id`, `environment`, `release` | root's value if non-null, else the first non-null by `(start_time, span_id)` | root partial's value if non-null, else the first non-null over the partials in merge order |
| `tags` | sorted distinct union | sorted distinct union |
| `models` | sorted distinct union of `response_model`, else `request_model`, per span | sorted distinct union |
| `metadata` | root's map; then the other spans in `(start_time, span_id)` order add keys that are absent | root partial's map; then the other partials in merge order add absent keys |
| `input_preview`, `output_preview` | root's value if non-null, else the first non-null among `llm` spans by `(start_time, span_id)` | root partial's value if non-null, else the first non-null over the partials in merge order |
| `span_count` | count | sum |
| `error_count` | spans with status `error` | sum |
| `llm_count` | spans with kind `llm` | sum |
| tokens (6 columns), `cost_total` | sum of non-null values; null if all are null | same |
| `partial_count` | 1 | sum |

**Merge order** (spec 01 § Order, as edited by this change):
1. The root partial is the partial with `has_root`. If several have it, it is the one with the smallest `(start_time, root_span_id)`.
2. The other partials follow in `start_time` order.
3. Non-root partials tied on `start_time` contribute the smallest value: the smallest string, the smallest name, and for `metadata`, per absent key, the smallest value. The root partial always comes first, so a tie between it and another partial doesn't matter.

That makes the result independent of input order, except for the last bits of `cost_total` (§ Conventions).

**What a merge can't rebuild.** A partial keeps its values but not the `(start_time, span_id)` of the span each value came from. So merging the partials of a split trace equals `partials_from_spans` of all its spans for:
- counts, tokens, `cost_total` (within float tolerance), `tags`, `models`, `start_time`, `end_time`, `last_ingest_time`, `has_root`;
- the context fields when each is set on every span of the trace or on none;
- `metadata` when every span that has a key has the same value for it.

It can differ for:
- `name`, when there is no root and spans in different partials tie on the earliest `start_time`;
- the previews, when the root's preview is null and `llm` spans are in different partials;
- `metadata` and the context fields, when values differ between spans;
- `root_span_id` and everything taken from the root, when a trace has several root spans.

Those results are still deterministic. Edit the last sentence of spec 01 § Order in this commit to say exactly this, replacing the current claim, which is too strong.

### Tests

1. **Oracle.** For the A2 dataset (`test_support::dataset_spans()`), `partials_from_spans` over all spans equals `test_support::trace_batch` row for row, after sorting both by `trace_id`.
2. **Split and merge** (proptest). The generator stays inside the cases that can be rebuilt (§ What a merge can't rebuild). Put a comment in the test that names the excluded cases. Generate traces of 1–12 spans with:
   - distinct `start_time`s within a trace;
   - at most one root span, or none;
   - random kinds, statuses, tokens and costs (some null);
   - each context field set to one value on every span, or null on every span;
   - `metadata` keys from a small pool, the same value per key within a trace;
   - previews null on every span except the root, which may have them;
   - tags and models random per span.

   Sort the trace's spans by `start_time` and cut that list into 1–4 contiguous groups at random points. Contiguous groups are what flushes produce. Run `partials_from_spans` per group and merge the partials in a random order. The result equals `partials_from_spans` of all the spans, except `partial_count`, which equals the number of groups. `cost_total` compares within tolerance.
3. **Order independence** (proptest, unrestricted). Values are random per span and may be inconsistent: ties, several roots, different metadata values. Merging the same partials in two random orders gives identical rows, except `cost_total` within tolerance.
4. **Column subsets** (proptest). For a random requested subset `S`:
   - the merger's output columns are exactly `S ∪ {trace_id, start_time}`;
   - each of those columns equals the same column of the full merge.
5. **No columns requested.** `MergeColumns` of the empty set outputs `trace_id` and `start_time` only, one row per trace.
6. **Edge cases:**
   - a trace whose only root partial has null `session_id` takes the earliest non-root partial's;
   - all costs null gives a null sum;
   - one span gives `span_count` 1;
   - a merge of zero batches gives zero rows.
7. **Malformed input.** A partial batch missing a required column, or with a wrong type, returns `PartialsError::Column`. It doesn't panic.

**Commit:** `storage: add the trace merge operator`

---

## M3.3 — Flush writes `traces` files

**Goal:** every spans flush also writes the partials of each spans file, and commits both together.

**Open.** After `load`, `Committer::open` calls `check_file_sources`. A violation fails `Storage::open`. M3.1 left that call out so spans-only catalogs still open.

In `flush.rs`, for `Table::Spans`, per prepared spans file (after dedup and sort):
1. compute `partials_from_spans` of exactly the rows written to that spans file;
2. prepare a `traces` file in the same `(project, day)` at level 0, with its own path from the path builder;
3. stage both paths in the existing single staging transaction;
4. write both, under the same `flush.upload_concurrency` limit;
5. commit both `FileMeta`s plus `sources = [(traces, spans)]` in the existing single commit.

**Order.** The list of prepared files is in partition order, and each spans file is immediately followed by its traces file. `put_all` starts uploads in list order. With `upload_concurrency = 1` the order is fully deterministic, which the fault test below relies on.

Retries are unchanged: a failed attempt retries the whole view under new names, both files included.

**Read state.** The published catalog holds the traces files. Queries ignore them until M3.6.

### Crash harness (`tests/crash.rs`)

After each reopen, check:
- `Storage::open` succeeded, which means the sources check passed;
- for every source pair in `catalog.db`: the traces file's `sum(span_count)` equals the spans file's row count, and both hold the same set of `trace_id`s.

**Reading the files.** The harness is an integration test with only the public API. It already reads `catalog.db` with `sqlx` (`CatalogView`). It reads each object with `parquet`'s sync reader (`ParquetRecordBatchReaderBuilder` over a `std::fs::File`), projected to the needed columns. The harness uses a local object store, at `<data_dir>/objects/<path>` with `path` from `data_files.path`.

**Crash points.** No new hook point. With several files per flush, the random kill points of the object hooks already fall between the spans write and the traces write in some iterations. Say so in the report.

### Tests

- A flush of 3 traces over 2 days writes 2 spans files, 2 traces files and 2 source rows. Each traces file's partials equal `partials_from_spans` of its spans file.
- **Ambiguous failure on the traces upload.**
  - Setup: `upload_concurrency = 1` and one partition, so the uploads are spans, then traces.
  - Fault: on the `FaultStore`, `enqueue` a `FaultKind::Delay(Duration::ZERO)` (consumed by the spans upload), then the ambiguous-failure kind the M2 tests use (consumed by the traces upload).
  - Expected: the flush retries the whole view under new names. The first attempt's names stay in `staged_objects` until a zero-age staged collection removes them and their objects.
- After a reopen the catalog loads, and the traces files are in the read state.

**Commit:** `storage: write trace partials at flush`

---

## M3.4 — Trace anchor (`anchor.rs`)

**Goal:** D86. The spans and partials of a trace that crosses midnight go to one partition day.

### Config

Two new fields, with `deny_unknown_fields` and humantime durations (D90). Update every struct literal (§ Conventions).
- **`QueryConfig.trace_lookback: Duration`**, TOML `[storage.query] trace_lookback`. Default `"1h"`, with `#[serde(default = …, with = "humantime_serde")]` like the existing durations. It must be > 0 and ≤ 24 h. Otherwise it is a config error.
- **`StorageConfig.anchor: AnchorConfig`**, TOML `[storage.anchor]`, `#[serde(default)]`. `AnchorConfig { window: Option<Duration> }`, where `None` means "same as `trace_lookback`". Resolve it once in `Storage::open`. The resolved value must be > 0 and ≤ 24 h.

### Definitions

All times are ns since the epoch, UTC. Days are UTC dates.
- **Nearest midnight** `B(t)`: the midnight closest to `t`. That is `t` rounded to the nearest multiple of one day; exactly noon rounds up.
- **Near midnight:** `|t − B(t)| < window`, strictly.
- **Own day** of a row: `day(start_time)`.

### State

```rust
pub(crate) struct Anchors {
    window: i64,                                   // ns
    clock: i64,                                    // ns: largest ingest_time committed by a flush
    entries: HashMap<(ProjectId, TraceId), Entry>, // Entry { day: NaiveDate, expires: i64 }
}
```

- **Expiry runs on ingest time.** An entry made by a flush expires at that flush's largest `ingest_time` + `2 × window`. That covers late rows arriving up to `2 × window` after the trace was last flushed. Because it counts ingest time, not the age of the event, backfilled data (old events ingested now) is anchored like live data.
- **Clock.** The largest `ingest_time` that any flush has committed. At open, it is the largest `max_ingest_time` among the active spans files, or `i64::MIN` without files.
- **Ownership.** `Anchors` lives in the spans flush task, behind its own mutex. Only the flush reads or writes it, so it is outside the lock order.

### Assignment, at flush

Flush step 2 starts with partitioning, per project, for the whole frozen view:
1. Concatenate the project's frozen spans batches from every day partition, and group the row indexes by `trace_id`.
2. For each trace, its **anchor** `A` is:
   - the entry's day, if an entry exists (expired entries were dropped at the last commit);
   - otherwise `day(min_start)`, where `min_start` is the trace's smallest `start_time` in this flush.
3. Each row goes to partition `A` if `|own day − A| ≤ 1` day, and to its own day otherwise. The rule is per row (spec 03 § Trace anchor). A row 2 days or more from its trace's anchor stays in its own day, where the one-day query slack finds it.
4. **New entries.** If any row of the trace is near midnight, the trace gets an entry `{ day: A, expires: max_ingest + 2 × window }`, where `max_ingest` is this flush's largest `ingest_time`. An existing entry gets its `expires` refreshed the same way.
5. Then dedup, sort and partials run per output partition, as before.

**Commit first, then update the state.** Steps 2–4 compute on the current state without changing it, and the planned entry changes travel with the prepared files. Only after the commit succeeds does the flush:
1. insert or refresh those entries;
2. set the clock to `max(clock, max_ingest)`;
3. drop entries with `expires < clock`.

A failed attempt changes nothing, so its retry computes the same days from the same state.

**A known limit, documented in spec 03.** A trace with no row near midnight before a flush, and rows past midnight later, gets an entry only from its later rows. Example: rows at 22:00 flushed, then rows at 00:30. Its earlier rows stay in the earlier day. The anchor guarantees one day only for traces whose rows reach the window before midnight.

The memtable keeps partitioning by each row's own day. Only files use anchor days.

### Rebuild at open

Run it after the catalog loads (and the sources check passes), before the flush task starts:
1. Set the clock as above. Let `since = clock − 2 × window`.
2. Select the active traces files with `max_ingest_time ≥ since`.
3. For each, read `trace_id`, `start_time` and `last_ingest_time`. Use `parquet`'s async reader: `ParquetRecordBatchStreamBuilder` over a `ParquetObjectReader` on the storage's object store (the workspace `parquet` features include `async` and `object_store`). Use a `ProjectionMask` of those three columns, and `with_row_groups` set to the row groups whose `last_ingest_time` statistics have a max ≥ `since`.
4. For each row with `last_ingest_time ≥ since` and `start_time` near midnight, insert `{ day: file's partition day, expires: last_ingest_time + 2 × window }`. If an entry exists, keep the earlier day and the later `expires`.
5. Log the files read, rows read, entries and duration. Ids and counts only.

WAL-replayed rows need no rebuild: they get their day when they flush.

**Row-group selection.** Put it in a pure function: (file metadata, `since`) → the row-group indexes to read. Unit-test it on a file the test writes itself with `parquet::arrow::ArrowWriter` and a small `max_row_group_size`, so it has 3 row groups. The storage writer's 100,000-row limit stays as it is, and production code gets no test-only parameter.

**Spec edit in this commit.** Rewrite spec 03 § Trace anchor's expiry and rebuild bullets, and the matching paragraph of `00-design.md` § Trace anchor, to match:
- expiry on ingest time;
- the rebuild by `last_ingest_time`;
- the per-row rule;
- the known limit above.

Spec 03 §2 step 2 already describes the regrouping.

### Day slack

`query/mod.rs` day pruning for the files of `spans` and `traces` widens by one day on each side. File pruning by `min_time` / `max_time` stays exact. Memtable pruning stays exact.

### Tests

Unit tests in `anchor.rs` and `flush.rs`, with explicit timestamps. Use day D = 2026-03-05, `window` 1 h, and `ingest_time` = `start_time` + 1 s unless stated.
- **Crossing midnight.** A trace with a span at D 23:59:50 is flushed, then a span at D+1 00:00:10. Both spans files are in day D.
- **One flush across midnight.** Spans at D 23:59:50 and D+1 00:00:10 in the same flush both go to D.
- **Late root.** The first flush has a child at D+1 00:00:20. A later flush has the root at D 23:59:40. Both are in D+1.
- **Far apart, same flush.** A trace with rows at D 12:00 and D+2 12:00 in one flush: anchor D, so the second row goes to its own day D+2.
- **Far apart, later flush.** A late span 2 days after the anchor goes to its own day.
- **Expiry.** A flush whose `ingest_time` is past the entry's `expires`. Then a late span of that trace at D+1 00:10 goes to its own day.
- **Backfill.** Events from 30 days ago, with `ingest_time` now, crossing that day's midnight, get one day.
- **Retry.** Make the commit fail once (`CatalogCommit` hook, `fail`). The retry assigns the same days, and the entries exist only after the successful commit.
- **Restart.** Same as "crossing midnight" with a reopen between the two flushes. The rebuild restores the entry, and the late span goes to D.
- **Row-group selection.** The pure-function test above.
- **Query slack.** A span whose own day is D+1, stored in D, is found by a spans query filtered to a range inside D+1.

### Crash harness

The child's stream changes so that traces cross midnight:
- **Ids.** Trace ids are derived from `(iteration, trace index)`: 16 bytes, deterministic, never all zero. Span ids come from a counter.
- **Times.** `start_time` falls in `[D 23:30, D+1 00:30)`, spread over the stream. `ingest_time` = `start_time` + 1 s. Each trace's spans are appended in 2–3 different batches, far enough apart in the stream that they usually land in different flushes.
- **Check after each reopen.** Every acknowledged span is in exactly one spans file, as before. Read `(trace_id, file day)` from each spans file with the sync reader. Every trace has all its spans in one partition day. All rows are near the same midnight, so the anchor must hold for all of them. The anchor clock advances with the stream, and entries live `2 × window` = 2 h of ingest time, longer than one iteration.

Keep the existing acknowledged-row checks working on the new stream.

**Commit:** `storage: anchor traces that cross midnight`

Continue with `06-m3b-traces-read.md`.
