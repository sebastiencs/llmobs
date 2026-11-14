# K1 — `alert_value`: one storage read per evaluation

**Commit subject:** `storage: add the alert_value read`

Adds the read that the alert loop (K7) runs once per rule evaluation: one aggregate over a sliding window that returns the sample count and the measured value. It also adds `check_alert_filter`, which the rule routes (K6) use to reject a bad filter. No server code changes in this sub-phase.

## Read first

- README § Design → Evaluation, D-K-2, D-K-3, D-K-11.
- `crates/storage/src/reads/metrics.rs`: `metrics`, `trace_ids`, `span_frame`, `score_rows`, `kpi_range`, `kpi_values`, `require`, `i64_at`, `f64_at`, `internal`, `percentile`, `parsed`, and the test module's `context()`, `at()`.
- `crates/storage/src/reads/mod.rs`: `ListScope`, `present`, `scoped_traces`, the `pub use` list.
- `crates/storage/src/lib.rs`: the `pub use reads::{…}` block.
- `crates/storage/src/filter/mod.rs`: `FilterView`, `Filter`, `QueryContext::parse_filter`.
- `crates/storage/src/views/mod.rs`: `TimeWindow` (`new`, `from`, `to`, `predicate`), `QueryContext::fail`.
- `crates/storage/src/views/sessions.rs`: `exact_cost_sum`.
- `crates/storage/src/test_support.rs`: `span_row`, `score_row`, `spans_table`, `traces_table`, `live_scores_table`, `ProjectQuery` construction in `example_query`.

## Files

| File | Change |
|---|---|
| `crates/storage/src/reads/alerts.rs` | new |
| `crates/storage/src/reads/metrics.rs` | five helpers become `pub(super)` |
| `crates/storage/src/reads/mod.rs` | `mod alerts;` and its `pub use` |
| `crates/storage/src/lib.rs` | re-export the new items |

## 1. Helpers shared with `metrics.rs`

In `crates/storage/src/reads/metrics.rs`, change only the visibility of these private functions, from `fn` to `pub(super) fn`: `require`, `i64_at`, `f64_at`, `internal`, `percentile`. Do not change their bodies or doc comments. `alerts.rs` imports them with `use super::metrics::{f64_at, i64_at, internal, percentile, require};`.

## 2. `crates/storage/src/reads/alerts.rs`

Module doc:

```rust
//! The alert measure: one aggregate over a sliding window (spec 07, D-K-2, D-K-11).
//!
//! The filter is a `traces` expression. Trace measures filter the traces
//! directly; span and score measures keep the rows whose trace passes it,
//! through the same semi-join as the dashboard (spec 05). Experiment traces
//! are always excluded; evaluation traces are excluded unless the filter
//! names `environment` (spec 05 § Default exclusions).
```

### Public types (exactly these)

```rust
/// Which value an alert measures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlertMeasure {
    /// Traces with `error_count > 0` divided by traces.
    ErrorRate,
    /// `sum(cost_total)` on traces, USD.
    Cost,
    /// Trace `duration_ns` p95, in milliseconds.
    TraceLatencyP95,
    /// `duration_ns` p95 of `llm` spans, in milliseconds.
    LlmLatencyP95,
    /// `avg(value_num)` of the live trace and span scores named `name` whose
    /// `value_num` is not null: numeric, boolean (1/0), and categorical with a value.
    ScoreAvg {
        /// Score name.
        name: String,
    },
    /// `avg(value_num)` of the live boolean scores named `name`.
    ScorePassRate {
        /// Score name.
        name: String,
    },
}

/// One alert evaluation.
#[derive(Debug, Clone)]
pub struct AlertValueRequest<'a> {
    /// `start_time ∈ [from, to)` for traces and spans, `created_at` for scores.
    pub window: TimeWindow,
    /// Filter text over `traces`. `None`, empty or all-whitespace means no filter.
    pub filter: Option<&'a str>,
    /// What to measure.
    pub measure: AlertMeasure,
    /// Time allowed for planning and running.
    pub timeout: Duration,
}

/// The sample size and the value over the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlertValue {
    /// Rows the value was computed over: traces, `llm` spans, or scores.
    pub count: i64,
    /// `None` when `count` is 0, except for [`AlertMeasure::Cost`], which is always
    /// `Some` (0.0 when no trace or no cost).
    pub value: Option<f64>,
}
```

`TimeWindow` is `crate::views::TimeWindow`; `Duration` is `std::time::Duration`.

### Public functions (exactly these)

```rust
/// Measures one alert over its window.
///
/// # Errors
///
/// [`RunError::Invalid`] for filter text, [`RunError::Timeout`] when the query
/// exceeds `timeout`, and the other [`RunError`] variants from planning and running.
pub async fn alert_value(
    ctx: &QueryContext,
    request: AlertValueRequest<'_>,
) -> Result<AlertValue, RunError> {
    let timeout = request.timeout;
    match tokio::time::timeout(timeout, compute(ctx, request)).await {
        Err(_) => Err(RunError::Timeout),
        Ok(result) => result,
    }
}

/// Parses `text` as a `traces` filter, as [`alert_value`] would, without running anything.
///
/// # Errors
///
/// [`RunError::Invalid`] when the text does not parse or names an unknown column.
/// Empty or all-whitespace text is accepted (it means no filter).
pub fn check_alert_filter(ctx: &QueryContext, text: &str) -> Result<(), RunError> {
    match present(Some(text)) {
        None => Ok(()),
        Some(text) => ctx
            .parse_filter(FilterView::Traces, text)
            .map(|_| ())
            .map_err(RunError::from),
    }
}
```

`RunError` has `#[from] InvalidQuery` on `RunError::Invalid`, so `RunError::from` works.

### Private implementation

Constants at the top of the file:

```rust
const COUNT: &str = "alert_count";
const ERRORS: &str = "alert_errors";
const VALUE: &str = "alert_value";
const DURATION_NS: &str = "duration_ns";
const NS_PER_MS: f64 = 1_000_000.0;
const DAY_NS: i64 = 86_400_000_000_000;
```

`compute(ctx, request) -> Result<AlertValue, RunError>` dispatches on `request.measure`:

**Filter parsing** (every measure): `let filter = match present(request.filter) { Some(text) => Some(ctx.parse_filter(FilterView::Traces, text)?), None => None };`. Then `let references_environment = filter.as_ref().is_some_and(|f| f.references_environment);`.

**Trace measures** (`ErrorRate`, `Cost`, `TraceLatencyP95`):

1. `let mut frame = scoped_traces(ctx, ListScope { window: Some(request.window), include_eval: false, include_experiments: false }, references_environment).await?;`
2. `if let Some(filter) = filter { frame = filter.apply(frame, ctx).await.map_err(classify)?; }`
3. Aggregate with no group keys (`frame.aggregate(vec![], aggregates).map_err(classify)?`), then `.collect().await.map_err(|error| ctx.fail(error))?`:
   - `ErrorRate`: `count(lit(1_i64)).alias(COUNT)` and `sum(when(col(traces::ERROR_COUNT).gt(lit(0_i64)), lit(1_i64)).otherwise(lit(0_i64)).map_err(classify)?).alias(ERRORS)`.
   - `Cost`: `count(lit(1_i64)).alias(COUNT)` and `exact_cost_sum().alias(VALUE)`.
   - `TraceLatencyP95`: `count(lit(1_i64)).alias(COUNT)` and `percentile(DURATION_NS, 0.95).alias(VALUE)`. (`traces` has a `duration_ns` column; `kpi_range` uses the same expression.)
4. Read the single row (see **Reading the row**). Then:
   - `ErrorRate`: `let errors = f64_at(require(batch, ERRORS)?, 0)?.unwrap_or(0.0);` (a `sum` over zero rows is null; `f64_at` reads the Int64 sum) and `value = (count > 0).then(|| errors / count as f64)`.
   - `Cost`: `value = Some(f64_at(require(batch, VALUE)?, 0)?.unwrap_or(0.0))` — always `Some`, 0.0 when no trace or no cost.
   - `TraceLatencyP95`: `value = f64_at(require(batch, VALUE)?, 0)?.map(|ns| ns / NS_PER_MS)`; when `count == 0`, `None`.

**Trace-id semi-join** (span and score measures), a private `async fn trace_id_plan(ctx, window, filter: Option<Filter>, references_environment) -> Result<LogicalPlan, RunError>`, the same as `metrics::trace_ids` but always built (the exclusions are always on here):
1. `let start = window.from().checked_sub(DAY_NS).ok_or_else(|| RunError::InvalidRequest("from is out of range".to_owned()))?;`
2. `let wide = TimeWindow::new(start, window.to())?;`
3. `let mut frame = scoped_traces(ctx, ListScope { window: Some(wide), include_eval: false, include_experiments: false }, references_environment).await?;`
4. `frame = frame.filter(col(traces::END_TIME).gt_eq(Expr::Literal(ScalarValue::TimestampNanosecond(Some(window.from()), Some("UTC".into())), None))).map_err(classify)?;`
5. Apply the filter as above, then `frame.select(vec![col(traces::TRACE_ID)]).map_err(classify)?` and return `frame.logical_plan().clone()`.

**`LlmLatencyP95`**:
1. `let ids = trace_id_plan(…).await?;`
2. `ctx.session().table("spans").await.map_err(classify)?`
   `.filter(window.predicate(spans::START_TIME).and(col(spans::SPAN_KIND).eq(lit(SpanKind::Llm.as_str()))).and(in_subquery(col(spans::TRACE_ID), Arc::new(ids))))`
   `.map_err(classify)?`
3. Aggregate: `count(lit(1_i64)).alias(COUNT)` and `percentile(spans::DURATION_NS, 0.95).alias(VALUE)`. Collect with `ctx.fail`.
4. `value = f64_at(…VALUE…)?.map(|ns| ns / NS_PER_MS)`, `None` when `count == 0`.

**`ScoreAvg { name }` and `ScorePassRate { name }`**:
1. `let ids = trace_id_plan(…).await?;`
2. Predicate: `window.predicate(scores::CREATED_AT)`
   `.and(col(scores::NAME).eq(lit(name.as_str())))`
   `.and(col(scores::TARGET).in_list(vec![lit(ScoreTargetKind::Trace.as_str()), lit(ScoreTargetKind::Span.as_str())], false))`
   `.and(col(scores::VALUE_NUM).is_not_null())`
   `.and(in_subquery(col(scores::TRACE_ID), Arc::new(ids)))`.
   For `ScorePassRate`, add `.and(col(scores::DATA_TYPE).eq(lit(ScoreDataType::Boolean.as_str())))`.
3. `ctx.session().table("scores")…filter(predicate)…aggregate(vec![], vec![count(lit(1_i64)).alias(COUNT), avg(col(scores::VALUE_NUM)).alias(VALUE)])`, collect with `ctx.fail`.
4. `value = f64_at(…VALUE…)?`, `None` when `count == 0`.

The `scores` view already holds only the latest live version of each score (spec 05), so no dedup is needed.

**Reading the row**: a private `fn single_row(batches: &[RecordBatch]) -> Result<&RecordBatch, RunError>` returns the one batch that has rows; every read uses row index 0. An aggregate with no group keys returns exactly one row, also over zero input rows. Skip batches with `num_rows() == 0`; zero rows in total, or more than one, is `internal("alert aggregate row count".to_owned())`. `count = i64_at(require(batch, COUNT)?, 0)?`.

`parsed`, `trace_ids` and `at_least` in `metrics.rs` are close to the code above but take a `MetricsRequest`; the small duplication is deliberate, so `metrics.rs` changes only by the five visibilities.

Imports you need (adjust to what the compiler asks): `std::sync::Arc`, `std::time::Duration`, `arrow::record_batch::RecordBatch`, `datafusion::common::ScalarValue`, `datafusion::functions_aggregate::expr_fn::{avg, count, sum}`, `datafusion::logical_expr::expr_fn::when`, `datafusion::logical_expr::{Expr, LogicalPlan, col, in_subquery, lit}`, `llmobs_core::enums::{ScoreDataType, ScoreTargetKind, SpanKind}`, `llmobs_core::schema::{scores, spans, traces}`, `super::{ListScope, present, scoped_traces}`, `super::metrics::{f64_at, i64_at, internal, percentile, require}`, `crate::filter::{Filter, FilterView}`, `crate::query_error::{RunError, classify}`, `crate::views::{QueryContext, TimeWindow, exact_cost_sum}`. `ScoreDataType::as_str()` and `SpanKind::as_str()` exist (`stored_enum!`); if `ScoreDataType::Boolean.as_str()` is not `"boolean"`, STOP and report.

## 3. Exports

- `crates/storage/src/reads/mod.rs`: add `mod alerts;` (alphabetically, before `mod cursor;`) and, just above `pub use metrics::{…};`, `pub use alerts::{AlertMeasure, AlertValue, AlertValueRequest, alert_value, check_alert_filter};`.
- `crates/storage/src/lib.rs`: add `AlertMeasure, AlertValue, AlertValueRequest, alert_value, check_alert_filter` to the `pub use reads::{…}` list, keeping it sorted as rustfmt leaves it (`just fmt`).

## 4. Tests (`#[cfg(test)] mod tests` in `alerts.rs`)

Build contexts from in-memory tables, like `metrics.rs`'s tests:

```rust
fn context(spans: &[Span], scores: &[Score]) -> QueryContext {
    let query = ProjectQuery {
        spans: spans_table(spans),
        traces: traces_table(spans),
        scores: live_scores_table(scores),
        runtime: Arc::new(RuntimeEnv::default()),
        target_partitions: NonZeroUsize::new(2).expect("two partitions"),
        optimizer_rules: Vec::new(),
        suspects: crate::verify::Suspects::detached(),
    };
    QueryContext::new(query, None).expect("context")
}
```

Test imports: `std::num::NonZeroUsize`, `std::sync::Arc`, `datafusion::execution::runtime_env::RuntimeEnv`, `llmobs_core::enums::{SpanKind, SpanStatus}`, `llmobs_core::names::{environment, metadata_key}`, `llmobs_core::score::{Score, ScoreTarget, ScoreValue}`, `llmobs_core::span::Span`, `crate::test_support::{live_scores_table, score_row, span_row, spans_table, traces_table}`, `crate::views::ProjectQuery`, plus `super::*`. Remove any the compiler reports unused.

**Test data recipe.**
- A trace `t` (0, 1, 2, …) is a root span `let mut root = span_row(t, 0, T0 + MIN, 1);` (its `end_time` is `start + 100`). Set fields on it: `end_time`, `status = SpanStatus::Error` (the trace's `error_count` becomes 1), `cost_total = Some(x)`, `environment = Some("prod".to_owned())`, or for an evaluation trace `environment = Some(environment::EVAL.to_owned())`. An experiment trace: `root.metadata.insert(metadata_key::EXPERIMENT_ID.to_owned(), "e1".to_owned());` (`Span::metadata` is `BTreeMap<String, String>`; use `metadata_key::EXPERIMENT_ID`, not `attr::EXPERIMENT_ID`, because the exclusion reads the metadata key).
- An `llm` child of trace `t`: `let mut child = span_row(t, 1, T0 + MIN, 1); child.span_kind = SpanKind::Llm; child.parent_span_id = Some(root.span_id); child.end_time = child.start_time + 5_000_000;`.
- A score: `let mut score = score_row(id, t, T0 + MIN, T0 + MIN, T0 + MIN, false, name);` with a distinct `id` (1, 2, 3, …) in each test (`live_scores` keeps one row per id), then `score.value = ScoreValue::Numeric(x)`, `ScoreValue::Boolean(b)` or `ScoreValue::Text(..)`. A session score: `score.target = ScoreTarget::Session { session_id: "s1".to_owned() }` .
- Every score sits on a trace that exists in the same test.
- Compare floats with a tolerance of `1e-9` unless a wider one is stated.
- Empty tables: `context(&[], &[])` should work; if `traces_table(&[])` panics, use a single trace at `T0 - 2 * DAY` (outside every window) instead, with `const DAY: i64 = 86_400_000_000_000;`.

Let `T0 = 1_700_000_000_000_000_000` and `MIN = 60_000_000_000`. Every window below is `TimeWindow::new(T0, T0 + 10 * MIN)`, timeout 30 s.

Tests (names exact):
1. `error_rate_counts_traces_with_errors`: 4 traces starting in the window, 1 with an error span → `count 4`, `value Some(0.25)`.
2. `error_rate_without_traces_is_none`: no spans → `AlertValue { count: 0, value: None }`.
3. `traces_outside_the_window_are_ignored`: one trace at `T0 − 1` and one at `T0 + 10 * MIN` (the end is exclusive), one inside → `count 1`.
4. `cost_sums_and_is_zero_without_traces`: traces with costs 1.5 and 2.25 and one without → `count 3`, `value Some(3.75)`; with no traces → `count 0`, `value Some(0.0)`.
5. `trace_latency_is_milliseconds`: 20 traces whose root span lasts `k * 1_000_000` ns for k = 1..=20 → `count 20`, value within `[18.0, 20.0]` (approximate percentile).
6. `llm_latency_reads_llm_spans_only`: three traces each with a root span and one `llm` child lasting 5 ms; a fourth trace with only a root span lasting 50 ms (`end_time = start + 50_000_000`) → `count 3`, value `Some(5.0)` within `±0.5`.
7. `score_avg_and_pass_rate`: numeric scores 0.2 and 0.8 named `"helpfulness"` on two traces in the window → `ScoreAvg` `count 2`, `Some(0.5)` (within 1e-9); boolean scores true, true, false named `"ok"` → `ScorePassRate` `count 3`, value `2/3` within 1e-9; a text score named `"ok"` is not counted; a score named `"other"` is not counted.
8. `score_on_a_filtered_out_trace_is_ignored`: two traces, environment `"prod"` and `"dev"`, one score each; filter `environment = 'prod'` → `count 1`.
9. `evaluation_and_experiment_traces_are_excluded`: one normal trace, one evaluation trace, one experiment trace; `ErrorRate`, no filter → `count 1`. With filter `format!("environment = '{}'", environment::EVAL)` (the filter names `environment`, so the eval exclusion is off; the experiment exclusion stays) → `count 1` (the eval trace).
10. `scores_ignore_eval_and_experiment_traces`: one numeric score on each of the three traces of test 9 → `ScoreAvg` without filter → `count 1`.
11. `a_trace_started_before_the_window_still_counts_its_spans`: a root at `T0 - 2 * MIN` with `end_time = T0 + 2 * MIN` and an `llm` child starting at `T0 + MIN` lasting 5 ms → `LlmLatencyP95` `count 1`. A second trace whose root starts at `T0 - 3 * MIN` and ends at `T0 - MIN`, with an llm child starting at `T0 - 2 * MIN`, adds nothing.
12. `session_scores_are_not_counted`: a session score named `"helpfulness"` → `ScoreAvg` `count 0`, `value None`.
13. `blank_filter_is_no_filter`: `alert_value` with `filter: Some("  ")` gives the same result as `None` on the data of test 1.
14. `filter_errors_are_invalid`: filter `"nonsense_column = 1"` → `Err(RunError::Invalid(_))`.
15. `check_alert_filter_accepts_empty_and_rejects_bad_text`: `""` and `"  "` → `Ok(())`; `"error_count >"` → `Err(RunError::Invalid(_))`; `"error_count > 0"` → `Ok(())`.

Write a small helper `async fn measure(ctx: &QueryContext, measure: AlertMeasure, filter: Option<&str>) -> Result<AlertValue, RunError>` for the window above.

If one of these expectations disagrees with how the existing views behave (e.g. the trace's `environment` comes from another span), fix the test data, not the read; if the read itself cannot meet the behaviour above, STOP and report.

## Commands

```bash
cargo test -p llmobs-storage alerts
```

```bash
just check
```

## Done when

- `alert_value` and `check_alert_filter` are exported from `llmobs_storage` with exactly the signatures above.
- The 15 tests pass; `just check` is green. (The timeout path is not tested: it is the same `tokio::time::timeout` wrapper as `metrics`.)
- `metrics.rs` changed only in the five visibilities.
