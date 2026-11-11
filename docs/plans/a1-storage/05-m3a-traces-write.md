# A1 · M3a — Traces: write path

First half of milestone M3 (traces) of the storage track. The second half is `06-m3b-traces-read.md`. One agent does the sub-phases of both files in order, on one branch (`a1/m3-traces`). Each sub-phase ends with `just check` green and one commit, and M3 lands as **one PR**.

**Prerequisites:**
- M2 is merged (PR #18).
- The S1 results branch `docs/s1-trace-bench-results` is merged, so `docs/research/storage/traces-benchmark.md` exists on `main`. If it isn't, stop and ask: M3.8 adds a section to it.
- The A2 query layer (`src/views/`, `src/reads/`, `src/filter/`, `src/sql/`, `src/query_*.rs`, `src/test_support.rs`) is on `main` and must keep passing.

**Read first:**
- `AGENTS.md`, all of it.
- `docs/plans/a1-storage/00-design.md`: § Trace anchor, § Trace partials and the `traces` view, § Trace-list spike (S1) and its outcome, § Commit protocol, § Testing.
- `docs/spec/01-data-model.md` § Trace: the merge table and § Order.
- `docs/spec/03-storage.md`: § Layout, § Trace anchor, §2 Flush, §3, §4, §6.
- `docs/research/storage/traces-benchmark.md`: why the merge is hand-written and how latest-N works.
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

No new dependency is expected. `proptest` is already a workspace dependency (D112): add it to `crates/storage` `[dev-dependencies]` with `workspace = true`. If something else turns out to be needed, add it to the root `[workspace.dependencies]`, re-check it with `cargo info`, and report the exact version.

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
- **A2's files.** `views/session.rs`, `views/mod.rs` (`QueryContext::new`) and `test_support.rs` (`example_query`) get the small changes M3.5 and M3.7 describe, and nothing else. Name them in the report.

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
  - `parquet.rs`.

### Parquet settings

`parquet.rs` functions take a `FileTable`. `traces` settings, from spec 03 §3:
- rows sorted by `start_time`, then `trace_id` (the tie-break makes files byte-identical across runs);
- sorting columns declared in the metadata;
- time column `start_time`, ingest column `last_ingest_time`;
- bloom filters on `trace_id`, `session_id` and `user_id`, fpp 0.01;
- dictionary columns as the existing rule computes them from `traces_schema()`;
- the same KV metadata keys, with `llmobs.table = traces`.

Add a round-trip test like `spans_round_trip_sorted_with_the_file_settings`, for `traces`.

### `file_sources`

`migrations/catalog/0002_file_sources.sql`:

```sql
CREATE TABLE file_sources (
    traces_file_id TEXT NOT NULL,
    spans_file_id TEXT NOT NULL,
    PRIMARY KEY (traces_file_id, spans_file_id)
);

CREATE INDEX file_sources_by_spans ON file_sources (spans_file_id);
```

`CatalogEdit` gains `sources: Vec<(FileId, FileId)>`, as (traces file, spans file).
- **Precondition.** Each traces file is in `add` with table `Traces`. Each spans file is in `add` with table `Spans`, or is an active `spans` row. Otherwise the edit is a conflict and nothing is written.
- **Write.** The rows are inserted in the same transaction as `add`.
- **Memory.** No in-memory index of sources in M3: nothing reads them at query time. M4 adds one when compaction needs it.

**Check at open.** After loading, `catalog.db` must satisfy spec 03 §3's invariant:
- every active `spans` file is the source of exactly one active `traces` file;
- every active `traces` file has at least one source, and all its sources are active `spans` files of the same `(project, day)`.

A violation is a new `CatalogError` variant (`SourcesInvariant { file }`, ids only), and `Storage::open` fails with it. Data from before M3 does not exist (AGENTS.md § Project), so there is no migration path for spans files without traces files.

### Tests

- The migration applies on a fresh catalog and on an M2 catalog.
- A commit with sources inserts its rows. A source pointing at an unknown file is a conflict, and the transaction leaves no row.
- `FileTable` round-trips through `data_files.table_name`. An unknown name fails the load with an error.
- **The open check** rejects:
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

- **Columns.** `MergeColumns` is the set of `traces` columns to produce. `input()` adds the order keys `trace_id`, `start_time`, `has_root` and `root_span_id` to it. A merger never reads or builds a column it wasn't asked for. That is what makes the list query's ranking phase cheap.
- **Accumulator.** `TraceAcc` holds one `Option` per column. Strings are owned. Lists and maps are kept as sorted sets and maps until `finish`.
- **No Arrow kernels per row.** The merger downcasts each input column once per batch.

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
3. Partials tied on `start_time` contribute the smallest value: the smallest string, the smallest name, and for `metadata`, per absent key, the smallest value.

That makes the result independent of input order.

### Tests

1. **Oracle.** For the A2 dataset (`test_support::dataset_spans()`), `partials_from_spans` over all spans equals `test_support::trace_batch` row for row, after sorting both by `trace_id`.
2. **Split and merge** (proptest). Generate traces of 1–12 spans:
   - random kinds, statuses and roots (including traces with no root);
   - context fields set on every span or on none. That is the consistency that spec 02 line 168 promises.

   Split each trace's spans into 1–4 random groups, run `partials_from_spans` per group, and merge the partials in a random order. The result equals `partials_from_spans` of all the spans, except for `partial_count`.
3. **Order independence** (proptest, context fields random per span, inconsistent allowed). Merging the same partials in two random orders gives identical rows.
4. **Column subsets.** For a random subset of columns, merging with `MergeColumns(subset)` equals the full merge projected to the subset.
5. **Edge cases:**
   - a trace whose only root partial has null `session_id` takes the earliest non-root partial's;
   - all costs null gives a null sum;
   - one span gives `span_count` 1;
   - a merge of zero batches gives zero rows.
6. **Malformed input.** A partial batch missing a required column returns `PartialsError`. It doesn't panic.

**Commit:** `storage: add the trace merge operator`

---

## M3.3 — Flush writes `traces` files

**Goal:** every spans flush also writes the partials of each spans file, and commits both together.

In `flush.rs`, for `Table::Spans`, per prepared spans file (after dedup and sort):
1. compute `partials_from_spans` of exactly the rows written to that spans file;
2. prepare a `traces` file in the same `(project, day)` at level 0, with its own path from the path builder;
3. stage both paths in the existing single staging transaction;
4. write both, under the same `flush.upload_concurrency` limit;
5. commit both `FileMeta`s plus `sources = [(traces, spans)]` in the existing single commit.

Retries are unchanged: a failed attempt retries the whole view under new names, both files included.

**Read state.** The published catalog holds the traces files. Queries ignore them until M3.6.

**Crash harness** (`tests/crash.rs`). After each reopen, check:
- the open-time sources invariant holds (open would fail otherwise; assert that it opened);
- for every pair: the traces file's `sum(span_count)` equals the spans file's row count, and both hold the same set of `trace_id`s.

The existing per-step aborts already cover the new writes, because the traces file is written in the same steps. Add one deterministic abort between the spans write and the traces write, if the hooks allow it without a new step name. Otherwise, explain in the report why the existing hooks cover it.

### Tests

- A flush of 3 traces over 2 days writes 2 spans files, 2 traces files and 2 source rows. Each traces file's partials equal `partials_from_spans` of its spans file.
- A `FaultStore` ambiguous failure on the traces upload retries the whole view under new names. The first names stay staged, and staged GC removes them.
- After a reopen the catalog loads, and the traces files are in the read state.

**Commit:** `storage: write trace partials at flush`

---

## M3.4 — Trace anchor (`anchor.rs`)

**Goal:** D86. Spans and partials of a trace that crosses midnight go to one partition day.

### Config

Two fields, with `deny_unknown_fields` and humantime durations (D90):
- `[storage.query] trace_lookback`, default `"1h"`. It must be > 0 and ≤ 24 h. Otherwise it is a config error.
- `[storage.anchor] window`, default absent, meaning `trace_lookback`. Same bounds.

### State

```rust
pub(crate) struct Anchors {
    window: i64,                                          // ns
    clock: i64,                                           // ns, data clock
    entries: HashMap<(ProjectId, TraceId), Entry>,        // Entry { day: NaiveDate, expires: i64 }
}
```

- **Clock.** The data clock is the largest `ingest_time` written by any flush so far. At open, it is the largest `max_ingest_time` of the active spans files. Using data time, not wall time, keeps tests and the benchmark deterministic. It also expires entries correctly after a long outage.
- **Ownership.** `Anchors` lives in the spans flush task, behind its own mutex. Only the flush reads or writes it, so it is outside the lock order.

### Assignment, at flush

Flush step 2 starts with partitioning:
1. Take all frozen spans batches of one project, from every day partition, and group the rows by `trace_id`. Find each trace's `min_start` in this flush, and its own day `own = day(min_start)`.
2. Pick the trace's day:
   - if an entry exists and `|entry.day − own| ≤ 1` day, use `entry.day`;
   - otherwise use `own`.
3. If `min_start` is within `window` of a UTC midnight `B` (on either side), insert an entry with the chosen day and `expires = B + 2 × window`, unless one already exists.
4. Every row of the trace goes to the chosen day's spans file. Then dedup, sort and partials run per output partition, as before.
5. Advance the clock to the flush's largest `ingest_time`, and drop entries with `expires < clock`.

The memtable keeps partitioning by each row's own day. Only files use anchor days.

### Rebuild at open

Run it after the catalog loads and before the flush task starts:
1. Let `B` be the midnight nearest to the clock.
2. If `|clock − B| ≤ 2 × window`, read `trace_id` and `start_time` from the active traces files whose `[min_time, max_time]` overlaps `[B − window, B + window]`.
3. Skip row groups whose `start_time` statistics are outside that range. Files are sorted by `start_time`.
4. Insert an entry for each row inside the range, with the file's partition day and `expires = B + 2 × window`. When a trace appears in two days, keep the earlier day.

WAL-replayed rows need no rebuild: they get their day when they flush. Log the rebuild's file count, row count and duration.

### Day slack

`query/mod.rs` day pruning for files of `spans` and `traces` widens by one day on each side. File pruning by `min_time` / `max_time` stays exact. Memtable pruning stays exact.

### Tests

- **Crossing midnight.** A trace with a span at 23:59:50 on day D, flushed, then a span at 00:00:10 on D+1, flushed. Both spans files are in day D.
- **Late root.** The first flush has a child at 00:00:20 on D+1. A later flush has the root at 23:59:40 on D. Both are in D+1.
- **Far apart.** A span 2 days after the anchor goes to its own day.
- **Expiry.** After the clock passes `B + 2 × window`, a late span near `B` goes to its own day.
- **Restart.** Same as "crossing midnight" with a reopen between the two flushes. Use a traces file with several row groups: write it in the test with a small row-group limit through a test-only writer parameter, or skip the row-group assertion and say so. The rebuild reads only the overlapping row groups and assigns the late span to D.
- **Query slack.** A span whose own day is D+1 but which is stored in D is found by a spans query filtered to a range inside D+1.
- **Crash harness.** The child's stream includes traces that cross midnight. After each reopen, check that all spans of each such trace, if within `window` of the same midnight, sit in one partition day.

**Commit:** `storage: anchor traces that cross midnight`

Continue with `06-m3b-traces-read.md`.
