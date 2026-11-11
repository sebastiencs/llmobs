# A1 · M3b — Traces: read path

Second half of milestone M3. Read `05-m3a-traces-write.md` first: its prerequisites, decisions (D152–D154), conventions and final layout apply here. Same branch, one commit per sub-phase, one PR for the whole of M3.

---

## M3.5 — `QueryOptions`, day slack for spans, `around`

**Goal:** the lookup hint of D154, and the `Storage::query` signature M3.6 and M3.7 build on.

```rust
#[derive(Debug, Clone, Copy, Default)]
pub struct QueryOptions {
    /// Time hint for a single-trace lookup (`?around=`), ns.
    pub around: Option<i64>,
}

pub fn query(&self, project: ProjectId, options: QueryOptions) -> Result<ProjectQuery, QueryError>;
```

- Update every caller of `query`: tests, `tests/crash.rs`, `tests/s3.rs`.
- **Recognising a lookup.** A scan is a lookup when its filters contain `trace_id = <literal>` or `trace_id IN (<literals>)`, with each literal 16 bytes. Accept the forms A2's hex rewrite produces: a `FixedSizeBinary(16)` literal, a `Binary` literal, or `arrow_cast(<binary>, 'FixedSizeBinary(16)')`. S1 found the last one. Write one function, `lookup_ids(filters) -> Option<Vec<TraceId>>`, in `query/mod.rs`. Both providers use it.
- **Effect of `around`.** When the scan is a lookup, has no `start_time` bound, and `around` is set, select only the files whose `[min_time, max_time]` overlaps `[around − 1 day, around + 1 day]`. The memtable is always read.
- **Without `around`.** The scan reads every file of the project. DataFusion's statistics and bloom pruning do the work (spec 03 §6).
- The day slack of M3.4 already applies to spans. Check it has a test here if M3.4 didn't add one.

### Tests

- `lookup_ids` accepts the three literal forms and `IN` lists. It rejects:
  - a 15-byte literal;
  - `trace_id = a column`;
  - `trace_id = x OR y = z`.
- File selection with `around`: of three files on days D−2, D and D+2, only D is scanned. Assert it from the plan's file groups, or with the scan metrics (`files_ranges_pruned_statistics`).
- A lookup without `around` finds a trace in each of two days.

**Commit:** `storage: add query options with a lookup hint`

---

## M3.6 — `TracesProvider` (range and lookup)

**Goal:** a correct `traces` table over files plus memtable, for every query shape. Speed for the trace list comes in M3.7.

### Provider

In `query/traces.rs`:

```rust
pub(crate) struct TracesProvider {
    project: ProjectId,
    catalog: Arc<Catalog>,
    spans_mem: Arc<MemSnapshot>,   // the spans memtable of the same read state
    options: QueryOptions,
    lookback: i64,                 // query.trace_lookback, ns
    partitions: usize,
}
```

- `schema()` is `traces_schema()`.
- `supports_filters_pushdown` answers `Exact` for every filter (D152). DataFusion then adds no `FilterExec`, so `TracesExec` must evaluate all of them. DataFusion never pushes volatile filters or subqueries into a scan.
- `Storage::query` builds it from the same `ReadState` as the other providers, replacing the empty `MemTable`. Remove the "empty until M3" comments in `lib.rs`, `query/mod.rs` and `00-design.md`'s API block.

### `scan(projection, filters, limit)`

1. **Bounds.**
   - `[a, b]` comes from `TimeBounds` over the filters on `start_time`.
   - Extend the extraction for `OR`: the bounds of `x OR y` are the union of each side's bounds.
   - A side that is `start_time IS NULL` is dropped, because `start_time` is non-null in `traces_schema()`.

   This makes A2's cursor predicate, `(st < x) OR (st = x AND ties) OR st IS NULL`, give `b = x`.
2. **Lookup ids:** `lookup_ids(filters)`.
3. **Padded window.**
   - With bounds, partials are scanned on `[a − L, b + L]`, where `L = lookback`. A missing end stays open.
   - Without bounds, nothing is padded.
4. **Files.** Select the project's active `traces` files:
   - day pruning with the one-day slack;
   - `[min_time, max_time]` against the padded window;
   - `around` for lookups (M3.5).
5. **Columns.** The merge columns are the projection ∪ the columns of every filter. Plus the order keys, which `MergeColumns::input()` adds.
6. **Plan:**

   ```
   TracesExec(mode = Range | Lookup)            P output partitions
     RepartitionExec(Hash(trace_id), P)
       UnionExec
         DataSourceExec(ParquetSource over the files,
             projected to MergeColumns::input(),
             predicate: start_time in the padded window, trace_id IN ids;
             statistics, page index and bloom pruning on, as SpansProvider)
         memtable partials (below), same projection and predicate
   ```

7. **Per output partition**, `TracesExec`:
   1. pushes every input batch into a `TraceMerger`;
   2. grows a `MemoryReservation` from a `MemoryConsumer` named `TracesExec[partition]`, by the change in `TraceMerger::size()`. A failed `try_grow` returns DataFusion's `ResourcesExhausted`, which A2 maps to `query_too_large`;
   3. at end of input, merges with `finish()`;
   4. keeps the rows whose merged `start_time` is in `[a, b]` and that pass every filter;
   5. projects to `projection`;
   6. streams the result in batches of the session's `batch_size`.

   The filters are converted once per scan with `create_physical_expr` against the merged schema (`MergeColumns::output()` in traces column order).
8. **Limit.** When DataFusion passes `limit` (no sort above the scan), use one output partition and stop emitting after `limit` rows. The merge itself still reads its whole input.

**Memtable partials.**
- Use the spans memtable batches of the project (active and frozen, every day) whose `[min_time, max_time]` overlaps the padded window. Use all of them when there are no bounds.
- Each batch's partials are `partials_from_spans(batch)`, computed at most once per memtable batch and cached. The cache must be shared by every read state that holds the batch: put an `Arc<OnceLock<Arc<RecordBatch>>>` next to the batch in `MemBatch`.
- Coalescing makes a new batch with an empty cache. That is fine.

**Displaying the plan.** `TracesExec` displays `TracesExec: mode=range|lookup|list, files=<n>, mem_batches=<m>` plus `fetch=<k>` in list mode. Tests use it.

### Tests

These are the golden tests of the M3 exit criterion. Write them against `Storage` with flushes driven by the test, not against hand-built partials.

1. **Oracle.**
   - Append the A2 dataset (`test_support::dataset_spans()`) in random-size batches, with shutdown-and-reopen flushes at 3 random points, leaving the last batch in the memtable.
   - `SELECT * FROM traces` over the raw provider must equal `test_support::trace_batch` of all spans, after sorting by `trace_id`.
   - Exclude `partial_count`, and check it separately: it equals the number of flushes, plus the memtable, that hold the trace.
2. **Late partials.** One trace's spans in 3 flushes plus the memtable merge into one row. The sums are right and `partial_count = 4`.
3. **Midnight.** A trace from 23:59:30 to 00:00:30 is returned by a window `[D 23:00, D+1 00:00)`, with its merged `start_time`. A window `[D+1 00:00, D+1 01:00)` doesn't return it.
4. **Duplicates.** The same span in two flushes gives `span_count = 2` until compaction. The test names the documented limit (spec 03 §6).
5. **Filters that pass only after the merge:**
   - `cost_total > 1.0`, with partials of 0.6 + 0.6, returns the trace with cost 1.2;
   - `session_id = 's'` with the session only on the root partial returns the whole trace, with every partial's counts;
   - `environment = 'prod'` with a null environment on the root partial and `prod` on a later partial returns it.
6. **Window.**
   - A partial 50 minutes after its root, with a window that starts between them: the trace is not returned, because its merged start is before the window.
   - A partial 70 minutes after its root (beyond `L`): the trace is returned with the late start. That is the documented window limit; the test names it.
7. **Projection.** `SELECT trace_id, cost_total` equals those columns of `SELECT *`. The plan's merge columns don't include `input_preview`; read them from the display, or with a test-only accessor.
8. **Lookups.**
   - `trace_id = X` with and without `around` returns the trace.
   - An unknown id returns no row.
   - `trace_id IN (X, Y)` returns both.
9. **Flush race.** With pause points: run a query while a flush is paused after the write and before the commit, and again after the commit. Each trace appears once, with the full `span_count`.
10. **Memory.** With a `RuntimeEnv` whose pool is 1 MiB, a full scan of 50,000 traces fails with `ResourcesExhausted`. It doesn't fail with an allocation abort.
11. **A2 over the real engine.** Build `QueryContext::new(storage.query(..), None)` over the A2 dataset ingested as in test 1. A2's `trace_list` (default request, then `include_eval`) and `trace_detail` (two ids) return the same rows as over `test_support::example_query()`. That proves A2 works unchanged over the real provider.

**Commit:** `storage: add the traces provider`

---

## M3.7 — Latest N (`TraceListRule`, list mode)

**Goal:** D153. The trace list reads a few minutes of partials instead of the whole range, and merges in full only the rows it returns.

### Getting the rule into A2's session

- `ProjectQuery` gains `pub optimizer_rules: Vec<Arc<dyn PhysicalOptimizerRule + Send + Sync>>`. `Storage::query` puts `TraceListRule` in it.
- A2's `views/session.rs` `session_state` takes the rules and adds each with `SessionStateBuilder::with_physical_optimizer_rule`, which appends after DataFusion's defaults. `QueryContext::new` passes `source.optimizer_rules`.
- `test_support::example_query` sets an empty `Vec`.

Those are the only edits to A2's files. Update `00-design.md` § Public API and A2's README § Boundaries (done in this planning change; check they still match the code).

### `TraceListRule` (`query/trace_list.rs`)

Match, top-down, a `SortExec` with `fetch = Some(k)`, optionally under a `SortPreservingMergeExec`, whose sort expressions are exactly:
1. `start_time` descending (any nulls order: the column is non-null);
2. `trace_id` descending.

Walk from the `SortExec`'s input down through `ProjectionExec`, `CoalesceBatchesExec`, `CoalescePartitionsExec` and `RepartitionExec` only. Map each sort expression through every projection:
- `start_time` must be a plain column of the input;
- `trace_id` may be a plain column, or `encode(<trace_id column>, 'hex')`. A2's view projects it that way, and lowercase hex sorts like the bytes.

The walk must end at a `TracesExec` in `Range` mode. Then replace that `TracesExec` with the same scan in `List { fetch: k }` mode, and leave everything above it unchanged. The `SortExec` stays: it sorts at most `k` rows.

There is no rewrite when:
- any other operator is in the way (a `FilterExec`, a join from a score function, an aggregate, a window);
- the sort is different (another column, ascending, `cost_total`);
- the scan is a lookup.

Those queries stay in range mode, correct and slower.

### List mode (`TracesExec`, one output partition)

1. **Range.**
   - `b` is the scan's upper bound, or else the newest `max_time` among the selected files and memtable batches;
   - `a` is the lower bound, or else the oldest `min_time`.
2. **Slices**, newest first, clipped to `[a, b]`; stop at an empty one:
   - `[b − 15 min, b]`;
   - `[b − 1 h, b − 15 min)`;
   - `[b − 6 h, b − 1 h)`;
   - `[b − 24 h, b − 6 h)`;
   - `[a, b − 24 h)`.
3. **Phase 1, rank.** For each slice, run the range-mode plan of M3.6:
   - window = the slice, padded by `L`;
   - merge columns = the filters' columns only;
   - the same filters, plus merged `start_time` in the slice.

   Collect `(start_time, trace_id)` of the result. Drop a trace already found in a newer slice. Keep the best `k` by `(start_time DESC, trace_id DESC)`, and remember which slice each came from. Stop after the first slice that brings the total to at least `k`.
4. **Phase 2, fetch.** Group the kept ids by slice. For each such slice, run the range-mode plan:
   - the same padded window;
   - `trace_id IN (ids)` added to the partial-scan predicate;
   - merge columns = projection ∪ filter columns.

   Using the slice's own window makes every merged value identical to what phase 1 saw. Re-apply the filters: they hold, so this is a check. Count failures with a debug assertion in tests.
5. **Output.** The rows sorted by `(start_time DESC, trace_id DESC)`, at most `k`.

Both phases run their partitions concurrently, as range mode does, and share the scan's `MemoryReservation` accounting.

**Equivalence.** List mode returns exactly the first `k` rows that range mode returns over `[a, b]` with the same filters, except for traces with partials more than `L` apart. Those are the documented window limit.

### Tests

1. **Equivalence** (proptest, datasets where every partial is within `L/2` of its root). Compare list mode with range mode plus sort and truncate, for:
   - filters: none, `cost_total > x`, `environment = e`, `session_id = s`, `array_has(tags, t)`, `metadata['k'] = v`, and A2's cursor predicate after a random row;
   - `k` ∈ {1, 3, 50, more than the matching total}.

   Data comes from appends with flushes, plus the memtable.
2. **Plan shapes through A2.** Over a real `ProjectQuery`, A2's `trace_list` (default sort, limit 50) has `mode=list, fetch=51` in its physical plan. So do the same with a window, with a cursor, and with a filter. A score-function filter, `sort = cost_total`, and ascending order all give `mode=range`. Use `EXPLAIN` through `QueryContext::session()`.
3. **Paging.** A2's paging test (walk pages of 2) gives the same ids over the real engine as one page of 50.
4. **Slices.**
   - 60 traces spread over 3 days, `k = 50`: phase 1 stops at the slice that reaches 50.
   - A filter matching only traces older than 24 h runs every slice and returns them.
5. **Window limit.** A trace whose late partial starts 2 h after its root and inside the newest slice appears in list mode with its late start. The test names the documented limit.

**Commit:** `storage: add the trace list fast path`

---

## M3.8 — Benchmark (`tests/trace_bench.rs`, ignored)

**Goal:** the M3 exit criterion. The S1 latency targets, measured through the real engine and A2's reads.

`#[ignore]` integration test, run by hand on the same server as S1b:

```bash
LLMOBS_TRACE_BENCH_DIR=/path/to/data cargo test --release -p llmobs-storage --test trace_bench -- --ignored --nocapture
```

### Data

- If `LLMOBS_TRACE_BENCH_DIR` holds no catalog, generate into it. Otherwise reuse it.
- Generate S1b's shape:
  - `LLMOBS_TRACE_BENCH_TRACES` traces (default 5,000,000), 10 spans each, over 7 days from 2026-03-02 00:00 UTC;
  - twice the rate between 08:00 and 20:00;
  - 40% of traces split into 2–3 partials, with S1b's delays (80% under 1 min, 15% 1–30 min, 5% 1–3 h) as span start offsets from the root;
  - environment 80 / 15 / 5;
  - sessions and users at S1's rates;
  - log-normal cost (median $0.002, p99 $0.20) and tokens;
  - previews of 100–400 characters;
  - 1–2 models of 6, and tags.
- Reimplementing the spike's model is fine (it is our code, on `spike/s1-trace-bench`). Drop late spans past the range end, as S1b does.
- **Append order.** Append through `Storage::append_spans` in write-time order: a heap of pending late partials, released when their write time comes. Set `ingest_time` to the write time.
- **Config:** `wal.sync = "100ms"`, `flush.max_age = "1h"`, and `flush.max_bytes` sized so the run produces 900–1,100 spans files. Report the count.
- **Ending.** The last 2 minutes are appended after the last explicit flush trigger, and the generator then drops `Storage` without `shutdown`. The tail stays in the WAL, and every later open replays it into the memtable, as S1's memtable did.

### Queries

Each runs through `QueryContext::new(storage.query(project, options), None)` and A2's reads, at `target_partitions = 8`: one cold run, then 20 warm runs, giving p50, p95 and max.

| # | Read | Target (warm p95) |
|---|---|---|
| Q1 | `trace_list`, default request, limit 50 | 200 ms |
| Q2 | `trace_list`, filter `cost_total > 0.05` | 2 s |
| Q2r-2h, Q2r-15h | `trace_list`, filter `cost_total > t`. Choose `t` as S1b did, with an untimed range-mode query, so the 50th match is about 2 h and about 15 h back. | 2 s |
| Q3 | `trace_list`, filter `environment = 'production'` | 500 ms |
| Q4a | `trace_detail` of 99 ids (half split), `around` set | 100 ms |
| Q4b | the same ids without `around` | 1 s |
| Q5 | `metrics`: hourly trace count and cost over the 7 days | 2 s |
| Q6 | `trace_list` with the window of day 4 | 200 ms |

- Print the plan mode (`list`/`range`) of each list query.
- **Correctness.** Compare each list result with the same request over a `QueryContext` built without the rule (`optimizer_rules` emptied). Report the differing ids per query: only window-limit traces may differ.
- Print one markdown table (query, mode, cold, p50, p95, max, target, pass, rows, differing ids), plus the machine (CPU model, cores, RAM, disk, kernel, commit) and the dataset (traces, spans files, bytes, generation time).

### Results

Add a section **M3 implementation** to `docs/research/storage/traces-benchmark.md`: the table, the machine, and one paragraph comparing it with S1b.
- **A target that misses** is reported as missed, with the operator that took the time (from `EXPLAIN ANALYZE`). Don't tune past this sub-phase: report it, and the coordinating session decides.
- **Don't commit the data directory.**

**Commit:** `storage: benchmark the traces view`

---

## Report

When M3 is done, report:
- the PR, and the benchmark table with the pass or miss of each target;
- every deviation from these two plans, and why;
- the A2 files you edited and what changed in each;
- anything left unverified. In particular:
  - whether the S3 test (`tests/s3.rs`, D119) was run against Versity Gateway after M3.3. It writes traces files now, so run it;
  - the crash harness iteration count you ran.
