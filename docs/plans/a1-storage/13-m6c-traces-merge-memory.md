# A1 · M6c — Bounded memory for range-mode `traces` merges

Follow-up to milestone M6, **Resources and hardening**. It needs M6a and M6b (merged in #44). It fixes finding (a) of `docs/research/storage/benchmarks.md`: a range-mode `traces` query needs memory proportional to the traces in its window, so it fails as too large above roughly 850,000 traces with the default `query.max_memory` (2 GiB).

It changes two things:
- the query pool gives each spilling operator a fair share of the query's memory, as DataFusion's `FairSpillPool` does (M6.7);
- `TracesExec` sorts each hash partition by `trace_id` (a `SortExec`, which spills) and merges one run of partials at a time, so the merge holds a bounded number of traces (M6.8).

M6.9 reruns the benchmark at the default memory and records the results.

Decision: D-A1-13 in `docs/mvp-decisions.md` § Track A1. The spec is `docs/spec/03-storage.md` §6 (`TracesProvider`, Resource limits), already updated by the plan PR. Read those sections before starting.

---

## Investigation

All numbers come from the M6.6 dataset: 1,000,000 traces, 10 spans each, 7 days, 31 spans files. They were measured on a 32-core, 62 GiB machine with a throwaway probe test and a prototype of the fix. The probe ran each query in its own process and read `VmHWM` from `/proc/self/status`.

### What holds the memory

`TracesExec` (`query/traces.rs`) hash-partitions the partials by `trace_id` into `target_partitions` streams. Each stream runs a `MergeStream` that pushes every input batch into one `TraceMerger` (`partials.rs`) and builds the output only when the input ends (`EmissionType::Final`). So each partition holds every trace of its share of the window until the end.

Per trace, `TraceMerger` keeps a `HashMap<TraceId, TraceAcc>` entry. `TraceAcc` is a `Vec<Option<Acc>>` with **one slot for each of the 29 `traces` columns**, whatever the query reads. `size_of::<Option<Acc>>()` is 72 bytes (`Ranked<String>` is 64: a 40-byte `Rank` with an `Option<Vec<u8>>` tie, plus a `String`). `TraceMerger::push` charges each new trace `size_of::<TraceAcc>() + 29 × 72 + 48` = **2,160 bytes** before any value, then the length of each string it reads.

Measured per `TracesExec` partition (8 partitions, ~125,000 traces each), current code:

| query | merged columns | reserved per partition | per trace | merged output per trace |
|---|---|---:|---:|---:|
| `SELECT count(*) FROM traces` | 4 (the order keys) | 274 MB | 2,190 B | 24 B |
| `SELECT trace_id FROM traces WHERE cost_total > 0.05` | 5 | 273 MB | 2,185 B | 32 B |
| A2 `session_list`, 7-day window | 13 | 280 MB | 2,245 B | 205 B |
| A2 `trace_list` without the list rule | 26 | ~296 MB (the M6.6 figure) | ~2,370 B | — |

So about 2.2 KB per trace whatever the query: the fixed 29-slot cost is 90–99% of it. The merged output is 24–205 B per trace. Over 8 partitions this is ~2.2–2.4 GB, above the 2 GiB cap.

Two more costs are not reserved at all:
- `finish()` builds the output batch while the accumulators are still alive (peak: both at once);
- the whole merged partition is kept in `outstanding` until it is read.

### Reproduction (default memory config, current code)

`trace_bench` with its 8 GiB override removed fails on the range-mode reference query:

```
thread 'trace_benchmark' panicked at crates/storage/tests/trace_bench.rs:1286:9:
called `Result::unwrap()` on an `Err` value: TooLarge
```

Every range-mode query of the probe fails at the default 2 GiB, the cheapest ones included:

| query | result | process RSS peak |
|---|---|---:|
| `SELECT count(*) FROM traces` | ResourcesExhausted | 2,226 MiB |
| `SELECT trace_id … WHERE cost_total > 0.05` | ResourcesExhausted | 2,250 MiB |
| `SELECT environment, count(*), sum(cost_total) … GROUP BY environment` | ResourcesExhausted | 2,282 MiB |
| A2 `session_list` (7 days) | TooLarge | 2,935 MiB |
| A2 `trace_list`, list rule removed | TooLarge | 3,418 MiB |

The list-mode queries (D153) and lookups pass. `metrics` passes because it reads `spans`.

### Options

**(1) Raise the default `query.max_memory`.** Moves the limit, doesn't remove it: ~2.4 GB per million traces, so 5M traces (the benchmark's default size) need ~12 GB, and `max_memory_total` defaults to at most 8 GiB. One query would also take most of the shared pool. Rejected as the fix.

**(2) Narrower accumulators.** Give `TraceAcc` one slot per *merged* column instead of 29 (or one column-wise vector per merged column). This removes most of the 2,160 fixed bytes: `count(*)` would drop to a few hundred bytes per trace, `session_list` to roughly 700. It moves the limit 3–8× for narrow queries, very little for wide ones, and memory still grows with the window. It also leaves the hash merge's speed as it is (below, the sorted merge is faster). Not chosen now; it stays a possible later optimization of `TraceMerger`, which flush and compaction share.

**(3) Finer hash partitioning.** All partitions run at once and each holds its traces until its input ends, so the total is the same at 8 or 64 partitions. To lower the total, the sub-partitions must run one after another, which means scanning the window once per sub-partition (K× the reads) or writing the sub-partitions to disk first (a hand-written Grace-style spill). Rejected.

**(4) Spill the hash merge.** Like DataFusion's hash aggregate: when the reservation is refused, sort the accumulated traces by `trace_id`, write them out, and merge the runs at the end. A merged trace is not a valid partial, though: its values are ranked by their own partial's `(has_root, start_time, root_span_id)`, while the merged row carries the trace's minimum `start_time`, so merging it again can pick different values (spec 01 § Trace, *Order*). Spilling would need a custom state format that keeps each ranked value's rank, plus its own spill files and k-way merge. Rejected: (5) gets the same bound from DataFusion's existing external sort.

**(5) Sort each partition by `trace_id`, then merge runs (chosen).** After the hash repartition, a `SortExec` (preserving partitioning) orders each partition by `trace_id`. The merge then sees each trace's partials next to each other: it merges a run, emits it, and drops it. The merge holds at most one chunk of traces. The sort holds the partials and spills to `tmp/query/` when its reservation is refused (DataFusion's external sort, with multi-level merge of the spill files). The merge rules are independent of input order (`merge_is_independent_of_input_order`), so results are unchanged.

**(6) Time-ordered merge with a watermark.** `traces` files are sorted by `start_time`, so the merge could stream in time order and emit a trace once the scan passes `start_time + trace_lookback`. Memory would then be traces per `trace_lookback` (1 h), not per window. But a trace whose partials are more than 1 h apart would come out as two rows with the same `trace_id` (the benchmark has 29,353 such traces). Today's documented limit is only a late `start_time`. Rejected.

### Prototype of (5)

The prototype added the `SortExec` and run-at-a-time merge, plus a fair-share rule in `QueryPool` (M6.7). Results are identical to the current merge: the same 50 list ids, count 1,000,000, the same 50 sessions, and the same `GROUP BY environment` rows (`sum(cost_total)` equal once rounded to 3 decimals; only the float summation order differs).

Memory and time, 8 partitions (p50 of two runs; RSS = process peak):

| query | hash merge, 8 GiB cap | sorted, 2 GiB cap (default) |
|---|---|---|
| `count(*)` | 1,200 ms, 2,276 MiB | 330 ms, 271 MiB |
| `cost_total > 0.05` | 1,260 ms, 2,280 MiB | 350 ms, 287 MiB |
| `GROUP BY environment` | 1,310 ms, 2,350 MiB | 400 ms, 307 MiB |
| A2 `session_list` | 2,430 ms, 3,060 MiB | 1,340 ms, 630 MiB |
| A2 `trace_list`, list rule removed | 3,130 ms, 4,930 MiB | 1,760 ms, 1,440 MiB |

The sorted merge is faster as well as smaller: a hash merge over 125,000 live traces allocates and probes 2 KB accumulators spread over the heap, while the sorted merge reuses a few thousand.

**Spilling and the pool.** With the per-query pool as M6.1 built it (greedy: the first to ask gets the memory), the sorts of several partitions starve each other. One sorter holds most of the pool, another cannot reserve even one batch, and a sorter can only spill its own memory. Result: `SELECT * … ORDER BY start_time DESC, trace_id DESC LIMIT 50` failed at a 1 GiB cap even though it had spilled 216 MiB ("Not enough memory to continue external sort"). With fair shares (each spilling consumer gets at most `(cap − unspillable) / spillable consumers`, as DataFusion's `FairSpillPool`), the same query passes at 1 GiB (760 MiB spilled, 2.0 s). Every query passes at the default 2 GiB with 8 and with 32 partitions:

| query, 2 GiB, fair shares | 8 partitions | 32 partitions |
|---|---|---|
| `SELECT * … LIMIT 50` | 2,110 ms, 773 MiB spilled | 2,070 ms, 708–738 MiB spilled |
| A2 `trace_list`, list rule removed | — | 2,120 ms, 505–655 MiB spilled |
| A2 `session_list` | — | 1,140 ms, 10–13 MiB spilled |
| `count(*)` | — | 314 ms, no spill |

**Floor.** A sorter needs about twice its largest input batch plus `sort_spill_reservation_bytes` (DataFusion default 10 MiB). All-column partial batches of 8,192 rows were 6.7 MB, and 13-column ones 1.4 MB. So a query needs roughly `partitions × (2 × batch + 10 MiB)` plus what can't spill: ~770 MB for all columns at 32 partitions. Below that it fails as too large, whatever the number of traces. At 8 partitions and 512 MiB the all-column query failed this way; at 384 MiB `session_list` passed with 42 MiB spilled.

**Full benchmark** (`trace_bench`, 1M traces, local disk, same machine, p50 ms):

| query | hash merge, 8 GiB override | sorted + fair, default config |
|---|---:|---:|
| Q1 | 12.4 | 15.1 |
| Q2 | 31.5 | 36.2 |
| Q2r-2h | 123.3 | 110.5 |
| Q2r-15h | 525.1 | 327.3 |
| Q3 | 12.5 | 13.0 |
| Q6 | 22.1 | 23.6 |
| Q4a / Q4b | 18.3 / 18.9 | 19.0 / 19.0 |
| Q5 | 478.5 | 329.7 |
| metrics (informational) | 1,890 | 1,494 |

Every target passes in both runs, with 0 differing ids. Small list slices pay 1–5 ms for the sort; large merges are 20–40% faster.

### Recommendation

Do (5) with the fair-share pool, and drop the hash merge (no second path, AGENTS.md). Keep `query.max_memory` at 2 GiB. A range-mode query's memory no longer depends on the traces in its window; its time and spill volume still do (the all-column query spills ~0.75 GB per million traces in the window).

Not in this plan, to decide separately:
- **Spill disk.** The spill directory has DataFusion's default limit (`max_temp_directory_size`, 100 GiB per process), and spill files are not checked against `storage.min_free_bytes` (D-A1-9). A large range query can now write gigabytes under `<data_dir>/tmp/query/`. A cap tied to `min_free_bytes` would be a follow-up.
- **List mode for other sorts.** A2 `trace_list` sorted by cost or duration runs the full merge (above: ~2 s and ~0.5–0.75 GB spilled per million traces). A D153-style two-phase plan for those sorts would make them cheap. That is a query-layer change.
- **Option (2)**, as an optimization of `TraceMerger`.

---

## Conventions for every sub-phase

- **`just check-rust` passes before each commit.** Node is not always installed; when it is, run `just check`. Commit subjects are given per sub-phase. One commit per sub-phase.
- **Scope.** Edit only `crates/storage/` and `docs/`. No dependency changes: everything used is in the pinned `datafusion` 55.2 / `arrow` 59. The DataFusion items this plan names were checked in `~/.cargo/registry/src/index.crates.io-*/datafusion-*-55.2.0/`: `MemoryPool::register`/`unregister`, `MemoryReservation::consumer`, `size`, `try_resize`, `free`, `MemoryConsumer::can_spill`, `with_can_spill`, `SessionConfig::with_batch_size`, `with_sort_spill_reservation_bytes`, `MetricsSet::spill_count`, `SortExec::new(LexOrdering, input).with_preserve_partitioning(bool)`, `OrderingRequirements::new(LexRequirement)` (a hard requirement), `LexRequirement::new`, `PhysicalSortRequirement::new`, `EmissionType::Incremental`. If one doesn't exist or has another signature, stop and report; don't substitute.
- **Logs and errors.** Error texts name byte counts only, never a row value.
- **Errors.** No `unwrap`/`expect` outside tests unless a comment states the invariant.
- **What not to build here.** The three items listed under Recommendation. No change to `TraceMerger`, `partials_from_spans` or the merge rules. No new config key.
- **Report at the end of each sub-phase** (in your final message): what was done; any deviation from this plan, and why; anything left undone or unverified.

---

## M6.7 — Fair shares for spilling operators

Commit: `storage: give spilling operators fair shares of a query's memory (M6.7)`

### What it does (spec 03 §6 Resource limits, D-A1-13)

Inside one query's pool, each reservation whose consumer can spill (`MemoryConsumer::can_spill()`: sorts, spillable aggregates, spilling repartitions) may hold at most

```
share = (cap − unspillable) / spillable_consumers
```

where `unspillable` is the bytes held by reservations that can't spill and `spillable_consumers` is the number of registered consumers that can spill (at least 1 in the division). Past its share, `try_grow` fails with `ResourcesExhausted` and the operator spills. Reservations that can't spill are unchanged: first come, first served, up to the cap. The shared pool and admission (D-A1-8) are unchanged; every `try_grow` still checks the cap and takes from the shared pool as today.

### `QueryPool` (`src/memory.rs`)

`PoolState` gains three fields, documented:

```rust
/// Bytes held by reservations whose consumer cannot spill.
unspillable: usize,
/// Bytes held by reservations whose consumer can spill.
spillable: usize,
/// Registered consumers that can spill.
spillable_consumers: usize,
```

`used` stays (it equals `unspillable + spillable`; keep it, as `held` is computed from it).

Add to `impl MemoryPool for QueryPool`:
- `fn register(&self, consumer: &MemoryConsumer)`: if `consumer.can_spill()`, `spillable_consumers += 1`.
- `fn unregister(&self, consumer: &MemoryConsumer)`: if `consumer.can_spill()`, `spillable_consumers = spillable_consumers.saturating_sub(1)`.

Change:
- `grow(reservation, additional)`: add `additional` to `spillable` or `unspillable` according to `reservation.consumer().can_spill()`, then the existing `used`/`held` logic.
- `try_grow(reservation, additional)`: first, if the consumer can spill, compute `share` as above (`self.cap.saturating_sub(state.unspillable) / state.spillable_consumers.max(1)`); if `reservation.size() + additional > share`, return

  ```rust
  Err(DataFusionError::ResourcesExhausted(format!(
      "a spilling operator needs more than its {share} bytes of the query's {} bytes",
      self.cap
  )))
  ```

  Then the existing cap check and shared-pool take, unchanged. On success, add `additional` to `spillable` or `unspillable` as in `grow`.
- `shrink(reservation, shrink)`: subtract `shrink` (saturating) from `spillable` or `unspillable` according to the consumer, then the existing logic.

Update the module doc and `QueryPool`'s doc comment with one sentence on fair shares (D-A1-13).

### Tests (`memory.rs`, `#[tokio::test]`, same setup as the M6.1 tests)

Register spilling reservations with `MemoryConsumer::new("s1").with_can_spill(true).register(&as_dyn)`.
- `a_spilling_reservation_is_held_to_its_share`: shared 100 MiB, initial 1 MiB, cap 10 MiB. Two spilling reservations A and B. A `try_grow(5 MiB)` ok; A `try_grow(1 byte)` fails with `ResourcesExhausted`; B `try_grow(5 MiB)` ok. `reserved()` is 10 MiB.
- `unspillable_memory_shrinks_the_shares`: cap 10 MiB. One unspillable reservation U grows 4 MiB; spilling A and B: each can take 3 MiB, not 3 MiB + 1 byte.
- `a_lone_spiller_gets_everything_left`: cap 10 MiB, one spilling reservation: `try_grow(10 MiB)` ok.
- `dropping_a_spiller_widens_the_others_share`: cap 10 MiB, A and B spilling; A holds 5 MiB. Drop B (unregisters it): A `try_grow(5 MiB)` ok.
- `unspillable_reservations_are_first_come_first_served`: cap 10 MiB; spilling A holds 5 MiB; unspillable U `try_grow(5 MiB)` ok, `try_grow(1 byte)` fails (cap).
- Run the existing M6.1 pool tests unchanged.

In `lib.rs` tests, `a_query_that_cannot_spill_fails_as_too_large` must still pass unchanged.

---

## M6.8 — Sorted, run-at-a-time `traces` merge

Commit: `storage: sort traces partials by trace id and merge one run at a time (M6.8)`

### What it does (spec 03 §6 `TracesProvider`, D-A1-13)

`build_range_plan` puts a `SortExec` on `trace_id` (ascending), with `preserve_partitioning = true`, between the hash `RepartitionExec` and `TracesExec`. `TracesExec` declares that ordering as a hard requirement of its input, so the optimizer can't drop the sort. `MergeStream` merges each run of one trace's partials and emits merged rows as it goes: it never holds more than one chunk of traces. This applies to range and lookup modes, and to list mode's phase-1 and phase-2 inner plans, since they are built by `build_range_plan` too. The merge rules (`TraceMerger`, `partials.rs`) do not change.

### Plan building (`query/traces.rs`, `build_range_plan`)

After `repartition` is built, wrap it:

```rust
// Partials of one trace end up next to each other, so the merge holds one
// chunk of traces at a time; the sort spills when its share is refused (D-A1-13).
let ordering = LexOrdering::new([PhysicalSortExpr::new(
    Arc::clone(&key),
    SortOptions::default(),
)])
.ok_or_else(|| DataFusionError::Internal("empty trace_id ordering".into()))?;
let sorted: Arc<dyn ExecutionPlan> =
    Arc::new(SortExec::new(ordering, repartition).with_preserve_partitioning(true));
```

and pass `sorted` as `TracesExec.input`. Keep the `RepartitionExec` for every partition count, 1 included: list mode executes its inner plans directly (`collect_plan`, no optimizer), and a `SortExec` that does not preserve partitioning would read only its first input partition there.

The `display` string is unchanged.

### `TracesExec` (`query/traces.rs`)

- `fn required_input_ordering(&self) -> Vec<Option<OrderingRequirements>>`: for `Mode::Range` and `Mode::Lookup`, `vec![Some(OrderingRequirements::new(req))]` with `req = LexRequirement::new([PhysicalSortRequirement::new(Arc::clone(&self.key[0]), Some(SortOptions::default()))])` (it returns `Option`; `None` → `vec![None]`, which cannot happen with one element; say so in a comment rather than `expect`). For `Mode::List { .. }`, `vec![None]`: list mode never executes its `input`.
- `properties_for(schema, partitions)` gains an `emission: EmissionType` parameter. `build_range_plan` and `with_new_children` (for range and lookup) pass `EmissionType::Incremental`; `rewrite_list` and `with_new_children` for list mode pass `EmissionType::Final`.
- `maintains_input_order` stays the default (`false`): a run's merged rows come out in `HashMap` order.
- `execute` (range and lookup): build the new `MergeStream` below. The reservation keeps its name `TracesExec[{partition}]` and stays unspillable.

### `MergeStream` (`query/traces.rs`)

Fields:

```rust
struct MergeStream {
    input: SendableRecordBatchStream,
    merge_columns: MergeColumns,
    /// Partials of traces not yet known to be complete.
    merger: TraceMerger,
    /// Holds `merger.size()`.
    reservation: MemoryReservation,
    predicate: Option<Arc<dyn PhysicalExpr>>,
    bounds: TimeBounds,
    projection: Projection,
    output_schema: SchemaRef,
    /// Rows `limit` still allows; `None` without a limit.
    remaining: Option<usize>,
    outstanding: VecDeque<RecordBatch>,
    done: bool,
}
```

`batch_size` and `accounted` go away (output batches are at most `MERGE_CHUNK_ROWS` rows; the reservation is resized to the merger's size).

A module constant:

```rust
/// Most input rows merged before the complete traces are emitted (D-A1-13).
/// It bounds the merge's own reservation, which can't spill: about 2 KB per
/// trace, so ~2 MB per partition.
const MERGE_CHUNK_ROWS: usize = 1024;
```

Methods:
- `fn push_batch(&mut self, batch: &RecordBatch) -> DfResult<()>`: for each chunk `batch.slice(offset, len)` of at most `MERGE_CHUNK_ROWS` rows, call `push_chunk`; stop at the first error.
- `fn push_chunk(&mut self, chunk: &RecordBatch) -> DfResult<()>`:
  1. If `chunk.num_rows() == 0`, return.
  2. `cut = last_run_start(chunk)?`: the first row of the chunk whose `trace_id` equals the chunk's last row's `trace_id`, scanning back from the end. Read the column with `column_by_name(traces::TRACE_ID)` and downcast to `FixedSizeBinaryArray`, with `DataFusionError::Internal` on a missing column or wrong type. Compare with `value(row)`.
  3. If `cut > 0`: `self.merge(&chunk.slice(0, cut))?`, then `self.emit()?`. Every trace in the merger is now complete: the input is sorted, so no later row has an id below the chunk's last id, and the trace carried over from the previous chunk, if it continues here, starts this chunk and is inside `[0, cut)`.
  4. `self.merge(&chunk.slice(cut, len - cut))`: the last trace is carried to the next chunk.
- `fn merge(&mut self, partials: &RecordBatch) -> DfResult<()>`: `self.merger.push(partials).map_err(partial_err)?`, then `self.reservation.try_resize(self.merger.size())?` (a refusal is `ResourcesExhausted`, which A2 maps to `query_too_large`).
- `fn emit(&mut self) -> DfResult<()>`: replace `self.merger` with `TraceMerger::new(self.merge_columns.clone())`, `finish()` the old one (`partial_err`), `self.reservation.free()`. Then, as `finish` does today: `keep_mask`, `filter_record_batch`; if `remaining` is `Some(n)`, keep the first `min(rows, n)` rows and lower `n`; `project`; push the batch onto `outstanding` only when it has rows. If `remaining == Some(0)`, set `done = true`.
- `keep_mask` and `project` are unchanged. The old `finish` method is removed.

`poll_next`:
- pop `outstanding` first, as today;
- if `done`, `Ready(None)`;
- input batch → `push_batch`; an error sets `done` and is returned;
- input error → set `done`, return it;
- input end → `emit()` (the carried trace and any others left), set `done`; an error is returned.

The `limit` case now stops reading as soon as `limit` merged rows passed the filters, instead of after the whole input.

### Tests (`query/traces.rs` tests)

- **Replace** `a_small_memory_pool_fails_with_resources_exhausted` (Group 10). The merge no longer fails on a small pool; it spills. Two tests, same 50,000-trace setup (append, `reopen` so the partials are in a `traces` file):
  - `a_small_pool_spills_the_merge_and_returns_every_trace`: open with `config_in` plus `query.max_memory = 8MiB`, `query.initial_memory = 1MiB`, `query.max_memory_total = Some(8MiB)`. Take `storage.query(..)` (its runtime has the query pool and the spill directory) and build the session as `session` does, with `SessionConfig::new().with_target_partitions(2).with_batch_size(1024).with_sort_spill_reservation_bytes(1024 * 1024)` (the default 10 MiB merge reservation per sorter would not fit the small pool). Register `traces`. Plan `SELECT trace_id, span_count FROM traces` with `ctx.sql(..).await?.create_physical_plan().await?`, run it with `collect_plan(Arc::clone(&plan), ctx.task_ctx())`, then walk the plan for the `SortExec` nodes and sum `metrics().and_then(|m| m.spill_count())`: it is greater than 0. The rows equal those of the same query on a storage opened with `config_in` (default memory), compared sorted by `trace_id`; 50,000 rows. If `spill_count` is 0 at 8 MiB, halve the cap until it is above 0 while the query still succeeds, and give the value in the report.
  - `a_pool_below_the_sort_floor_fails_as_too_large`: same data, `max_memory = initial_memory = max_memory_total = 1MiB`; `QueryContext::new(storage.query(..).await?, None)` and `sql_json` with `SELECT * FROM traces` (as `lib.rs`'s `a_query_that_cannot_spill_fails_as_too_large` does) is `Err(RunError::TooLarge)`: the sort cannot reserve its 10 MiB merge memory.
- **Runs split across batches.** Extract the body of `oracle_full_scan_matches_the_fixture` (Group 1, several partials per trace across three flushes and the memtable) into `async fn oracle_full_scan(batch_size: Option<usize>)`. With `Some(n)`, the session is built with `.with_batch_size(n)`. Keep `oracle_full_scan_matches_the_fixture` as `oracle_full_scan(None)` and add `oracle_full_scan_with_tiny_batches_matches_the_fixture` as `oracle_full_scan(Some(2))`: the sort's output batches then end in the middle of most runs.
- `runs_longer_than_a_chunk_merge_into_one_row`: one partition (`target_partitions = 1`), 3,000 traces with one span each plus one trace whose id sorts in the middle (`trace_id(..)` ordering) with one span in each of three flushes (`reopen` between). `SELECT trace_id, span_count FROM traces`: 3,001 rows, ids distinct, the three-flush trace has `span_count = 3`. This crosses `MERGE_CHUNK_ROWS` within one input batch.
- `the_merge_input_is_sorted_by_trace_id`: `EXPLAIN SELECT * FROM traces WHERE start_time >= <ts>` (range mode, no limit) has a line containing `SortExec: expr=[trace_id@` right after the `TracesExec:` line. Don't hard-code the column index: it depends on the merge input.
- `a2_reads_unchanged_over_the_real_provider` (Group 11): the assertion `!plan.contains("RepartitionExec")` was checking that the limit scan is not repartitioned to `target_partitions`. Replace it with: every line containing `RepartitionExec` contains both `partitioning=Hash([trace_id@` and `], 1)`. Keep the other assertions.
- Every existing test in `query/traces.rs`, `reads/`, `views/` and the list-mode property tests must pass unchanged. They compare range mode with list mode, lookups and the A2 oracle, so they cover result equality.

---

## M6.9 — Benchmark at the default memory

Commit: `storage: benchmark range-mode traces at the default query memory (M6.9)`

### Harness (`crates/storage/tests/trace_bench.rs`)

In `config`, remove `max_memory: ByteSize::gib(8)` and `max_memory_total: Some(ByteSize::gib(16))` and their comment: the range-mode reference query must pass with `QueryConfig::default()` memory. Remove imports this leaves unused.

### Run

Run 3 of `docs/research/storage/benchmarks.md` again (local disk, `LLMOBS_TRACE_BENCH_TRACES=1000000`, same command), on a freshly generated dataset or a copy of an existing one (a run replays the WAL tail and may compact; don't reuse a directory another session is using). Also run, by hand from a scratch test or `just run`, the range-mode queries of the Investigation table (`count(*)`, `session_list` over 7 days, A2 `trace_list` with the list rule removed) with the default config and `target_partitions` 8. Record time, process peak RSS (`VmHWM`) and the peak size of `<data_dir>/tmp/query`.

Pass criteria:
- the benchmark completes with the default memory config;
- all nine targets pass and `differing ids` is 0 except Q6's window-limit traces (as before);
- no list-mode p50 is more than 10 ms or 20% (whichever is larger) above Run 3's.

If a criterion fails, stop and report the numbers; don't tune.

### Results (`docs/research/storage/benchmarks.md`)

- Add "Run 7 — trace_bench, local disk, 1,000,000 traces, default query memory (M6c)", with the command, the table, and the range-query table.
- Rewrite Findings (a): what it was, that M6c (D-A1-13) fixed it, and the new numbers. Keep the spill-disk note from this plan's Recommendation as an open item.

### Docs

- `docs/roadmap.md`, Track A1 row: replace the sentence starting "Open finding: range-mode `traces` merges need ~2.4 GB" (it names this plan) with "Range-mode `traces` merges are bounded (M6c, D-A1-13)." and add this PR's number.
