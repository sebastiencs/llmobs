# P7 — Order-independent cost sums (M-4)

Read [README.md](README.md) first. Independent of the other phases.

**Goal:** a trace's, session's or user's `cost_total` is the same bit for bit, whatever order its inputs are added in. A cursor on a cost sort then finds its row again on the next page. Today all three are `f64` sums whose last bits depend on arrival order. Floating-point addition is not associative: `(0.1 + 0.2) + 0.3 != 0.1 + (0.2 + 0.3)`. So a boundary row can repeat or vanish between pages.

**Edit only:**
- `crates/storage/src/partials.rs`;
- `crates/storage/src/views/sessions.rs`;
- `crates/storage/src/views/mod.rs` (one re-export);
- `crates/storage/src/reads/users.rs`;
- `docs/spec/01-data-model.md`;
- `docs/spec/05-query.md`;
- `docs/mvp-decisions.md`.

`partials.rs` is A1's file; this change is agreed for this plan.

**Commit:** `Sum costs in fixed point so pages are stable`

## Background

- **Traces.** `TraceMerger` (`partials.rs`) merges a trace's partials in a `HashMap`. Every rule there is order-independent (rank-based, min/max, set union, OR) except `cost_total`:
  - `TraceAcc::update` sends `Column::CostTotal` to `sum_f64`, which keeps `Acc::F64(f64)` and adds with `+=`;
  - `Acc::F64` is used for nothing else;
  - the output builder writes it with `Self::F64(builder) => match acc { Some(Acc::F64(value)) => builder.append_value(*value), _ => builder.append_null() }`.

  The partials reach the merger in no fixed order: `UnionExec`, then a hash `RepartitionExec`, then file grouping. A1's property test `merge_is_independent_of_input_order` already compares `cost_total` with a `1e-9` relative tolerance for this reason (`assert_batches_equal`).
- **The same accumulator builds partials at flush.** `partials_from_spans` goes through the same `acc.update`, with spans in `(start_time, span_id)` order, so the change applies there too.
- **Sessions.** `views/sessions.rs` `sessions_from_traces` uses `sum(col(traces::COST_TOTAL)).alias(traces::COST_TOTAL)`. A partitioned DataFusion `SUM` over `f64` adds in an order that depends on partitioning.
- **Users.** `reads/users.rs` `aggregate_users` has the same `sum`.
- Existing tests compare costs with an absolute tolerance of `1e-12` (`views/mod.rs`, `views/sessions.rs`, `reads/{traces,sessions,users,scores,metrics}.rs`), or a relative `1e-9` (`partials.rs`). Values that move by a few ulps keep passing them.

## The rule (D-A2-4)

Costs are added as integers of 1e-18 USD:
- `units = (value * 1e18).round() as i128`;
- add with `saturating_add`;
- convert back once at the end: `units as f64 / 1e18`.

Integer addition is associative, so the result does not depend on order. The range is about ±1.7e20 USD, far past any real total.

Sessions and users do the same in SQL with `Decimal128(38, 18)`: DataFusion sums decimals as `i128`.
- The range is ±1e20 USD.
- DataFusion's `CastExpr` uses `safe: false`, so a trace cost that is NaN, infinite or beyond 1e20 fails the cast. The whole session list, user list or detail then returns `query_failed`.
- DataFusion's decimal `SUM` wraps silently past `i128` (`add_wrapping`), which needs a total beyond 1e20 USD.
- Costs come from token counts and a price table (B1), so none of this is reachable in practice. D-A2-4 records it.

## Changes

### 1. `partials.rs`

1. Rename the variant `Acc::F64(f64)` to `Acc::Usd(i128)`, with the doc comment `/// USD in units of 1e-18, summed exactly so the merge order does not matter (D-A2-4).`

2. Add two helpers next to `sum_f64`:

   ```rust
   /// 1e-18 USD per unit (D-A2-4).
   const USD_UNITS: f64 = 1e18;

   /// `value` in 1e-18 USD units. `as` saturates out-of-range values and maps NaN to 0.
   fn usd_units(value: f64) -> i128 {
       (value * USD_UNITS).round() as i128
   }

   fn usd_from_units(units: i128) -> f64 {
       units as f64 / USD_UNITS
   }
   ```

   If clippy flags the `as` casts (`cast_possible_truncation`, `cast_precision_loss`), add `#[allow(…)]` on these two functions only, with a comment: "saturating by design; costs are far inside the range".

3. Replace `sum_f64` with `sum_usd`:

   ```rust
   fn sum_usd(slot: &mut Option<Acc>, val: &Val) -> usize {
       if let Val::F64(value) = val {
           let units = usd_units(*value);
           match slot {
               Some(Acc::Usd(kept)) => *kept = kept.saturating_add(units),
               _ => *slot = Some(Acc::Usd(units)),
           }
       }
       0
   }
   ```

   In `TraceAcc::update`, change `Column::CostTotal => sum_f64(self.slot(column), &val)` to `sum_usd`.

4. In the output builder, change the `Self::F64(builder)` arm to:

   ```rust
           Self::F64(builder) => match acc {
               Some(Acc::Usd(units)) => builder.append_value(usd_from_units(*units)),
               _ => builder.append_null(),
           },
   ```

5. Change nothing else. In particular, `Val::F64` (the cell read from a batch) stays as it is.

### 2. `views/sessions.rs`, `sessions_from_traces`

Replace `sum(col(traces::COST_TOTAL)).alias(traces::COST_TOTAL)` with:

```rust
                exact_cost_sum().alias(traces::COST_TOTAL),
```

and add, in the same file:

```rust
/// `sum(cost_total)` in `Decimal128(38, 18)`, back to `Float64`: an exact sum
/// whose result does not depend on the partitioning (D-A2-4).
pub(crate) fn exact_cost_sum() -> Expr {
    cast(
        sum(cast(col(traces::COST_TOTAL), DataType::Decimal128(38, 18))),
        DataType::Float64,
    )
}
```

- `cast` is `datafusion::logical_expr::cast`: add it to the file's existing `datafusion::logical_expr` import. `DataType` is already imported (`use arrow::datatypes::DataType;`); do not import it again.
- The column keeps its name (the alias), its type (`Float64`) and its nullability (nullable).

### 3. `reads/users.rs`, `aggregate_users`

- In `views/mod.rs`, change `pub(crate) use sessions::sessions_from_traces;` to `pub(crate) use sessions::{exact_cost_sum, sessions_from_traces};`. `mod sessions;` is private.
- In `aggregate_users`, replace `sum(col(traces::COST_TOTAL)).alias(traces::COST_TOTAL)` with `exact_cost_sum().alias(traces::COST_TOTAL)`, importing `crate::views::exact_cost_sum`.
- If `sum` then has no other use in `users.rs`, drop it from the imports.

The dashboard metrics (`reads/metrics.rs`) keep plain `sum`: they have no cursor, and an ulp of difference in a chart is invisible.

## Docs

### `docs/spec/01-data-model.md`, the partial merge table

Replace the row `| `cost_total` | Float64 | sum |` with:

```
| `cost_total` | Float64 | sum, in integer units of 1e-18 USD so the result does not depend on merge order (D-A2-4) |
```

In the paragraph after it that says `cost_total` (within floating-point tolerance), replace `cost_total` (within floating-point tolerance) with `cost_total` (within one rounding to 1e-18 USD per value).

### `docs/spec/05-query.md`

In § Logical views exposed to users, the `sessions` row (the one with `sum(cost_total) AS cost_total`), append after that row's description, inside the same cell:

```
 Cost totals of `sessions` and of the user list are summed as `Decimal128(38, 18)`, so they do not depend on the partitioning (D-A2-4).
```

If the cell's layout makes that awkward, add it as a sentence right after the table instead.

### `docs/mvp-decisions.md`, section `## Track A2 — Query layer`

Add, in ID order (create the section as P1 describes if it is missing):

```
| D-A2-4 | Exact cost sums | **A trace's `cost_total` is merged in integer units of 1e-18 USD, and session and user totals are summed as `Decimal128(38, 18)`.** Floating-point addition is not associative, so an `f64` sum depended on the order partials or rows arrived in (hash repartitioning, file grouping, partitioned aggregation). A cursor on a cost sort stores the exact value, and the next page then repeated or skipped the boundary row when the recomputed sum moved by one ulp. Integer and decimal sums are exact, so the order no longer matters. The range (±1e20 USD) is far past any real total; the per-value rounding (5e-19 USD) is invisible. A non-finite or out-of-range trace cost, which B1 never produces, fails session and user queries (`query_failed`) instead of being skipped (spec 01 § Traces, spec 05). |
```

## Tests

**T1 (`partials.rs` `mod tests`) `cost_merge_does_not_depend_on_order`.**
1. Build three single-row partial batches for one trace, with the helpers `all_costs_null_give_a_null_sum` uses:
   - spans `base_span(trace_id(1), span_id(k), 10 * k)` for `k` in 1, 2, 3;
   - set `cost_total` to `Some(0.1)`, `Some(0.2)` and `Some(0.3)`;
   - give spans 2 and 3 `parent_span_id = Some(span_id(9))`;
   - turn them into partials with `partial_batches(&[vec![s1], vec![s2], vec![s3]])`.
2. For each of the 6 orders of those three batches, call `merge_all(&ordered)`. It uses `TraceMerger::new(MergeColumns::all())`.
3. Read `cost_total` (`Float64Type`, row 0) from each result. Assert all 6 values have the same `to_bits()`, and that the value is within `1e-15` of `0.6`.

Check that this test **fails before the change**: run it once on the unchanged code and say so in the report. The plain `f64` sum gives `0.6000000000000001` for some orders and `0.6` for others.

**T2 (`partials.rs` `mod tests`) `usd_units_round_trip`.**
- `usd_from_units(usd_units(0.1)) == 0.1`;
- `usd_units(0.0) == 0`;
- `usd_units(-0.25) == -250_000_000_000_000_000`;
- `usd_units(f64::NAN) == 0`;
- `usd_units(f64::INFINITY) == i128::MAX`.

**T3 (`views/sessions.rs` `mod tests`) `session_cost_does_not_depend_on_partitioning`.**
1. Build traces for one session with costs 0.1, 0.2 and 0.3: three traces built with `span_row`, each a single root span with `session_id = Some("s")` and `cost_total = Some(c)`.
2. Make one `RecordBatch` per trace with `test_support::trace_batch(&[span])`: `t1`, `t2`, `t3`.
3. Build three `MemTable`s with `MemTable::try_new(traces_schema(), partitions)`, one per partition layout (each inner `Vec` is one partition):
   - `vec![vec![t1, t2, t3]]`;
   - `vec![vec![t3], vec![t2], vec![t1]]`;
   - `vec![vec![t2], vec![t3, t1]]`.
4. For each layout, take `example_query()` and set:
   - `source.traces = Arc::new(table)`;
   - `source.target_partitions = NonZeroUsize::new(3).expect("three")`.

   Build a `QueryContext` from it, and run `SELECT cost_total FROM sessions` through `test_support::rows`.
5. Assert the three results have identical `to_bits()`.

Unlike T1, this test is not guaranteed to fail on today's code, because DataFusion may happen to add in the same order. It pins the new behaviour; say in the report whether it failed before the change.

**T4 (`reads/users.rs` `mod tests`) `user_cost_does_not_depend_on_partitioning`.**
- The same three layouts as T3, with `user_id = Some("u")` on the three traces.
- Read the cost with `user_list` and the file's `request(...)` helper for `UserSort::CostTotal`, `SortOrder::Desc` (see `user_pages_cover_every_sort` for the call).
- Assert the `cost_total` of user `u` has identical `to_bits()` across the layouts.

**Existing tests** must pass unchanged. In particular:
- `merge_is_independent_of_input_order`;
- `partials_from_spans_matches_the_a2_oracle`;
- `all_costs_null_give_a_null_sum`: a null-only cost still gives `NULL`, since `sum_usd` ignores `Val::Null` as `sum_f64` did;
- every `reads::` fixture test.

## Done when

- `cargo test -p llmobs-storage --lib` passes.
- `just check` passes.
