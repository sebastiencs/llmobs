# M2 — Scope reads in storage

Read [README.md](README.md) first.

**Goal:** three reads in `llmobs-storage` that the deletion worker uses to resolve a request's scope (spec 03 § 7, D-M-2, D-M-6):
- `filter_trace_ids`: the trace ids that a traces-list filter, search and window match;
- `user_deletion_scope`: the trace ids and session ids of one end user's spans ingested up to a cut-off;
- `eval_trace_ids`: the evaluation (judge) traces whose target is in a set of traces.

They follow the existing reads in `crates/storage/src/reads/`. Each takes a `QueryContext`, runs under a timeout, and returns `RunError`. The server calls them in M3 and M4.

**Edit only:**
- `crates/storage/src/reads/deletions.rs` (new);
- `crates/storage/src/reads/mod.rs` (declare the module, re-export);
- `crates/storage/src/lib.rs` (add the new names to the existing `pub use reads::{…}` list).

**Commit:** `Add the deletion scope reads`

## Read first

- `crates/storage/src/reads/mod.rs`: `ListScope`, `present`, `scoped_traces` (the traces view with the window and the default exclusions).
- `crates/storage/src/reads/traces.rs`:
  - `trace_refresh` and `refresh`: the timeout pattern, `ctx.parse_filter`, `filter.references_environment`, `search(…)`, `filter.apply`, `ctx.fail`;
  - `list_frame`: the order in which a list applies window, exclusions, search and filter.
- `crates/storage/src/filter/scope.rs`:
  - `exclusions`, which builds `metadata['<key>']` with `ctx.session().state().create_logical_expr(…)`;
  - `search`.
- `crates/storage/src/views/mod.rs`: `QueryContext` (`session()`, `view_schema(ViewName)`, `fail`), `TimeWindow`.
- `crates/storage/src/test_support.rs`: `span_row`, `spans_table`, `traces_table`, `example_query`.
- `crates/storage/src/reads/users.rs` tests: `context_of(spans)`.
- `llmobs_core::schema::{spans, traces}` column names; `llmobs_core::names::{environment::EVAL, metadata_key::EVAL_TARGET_TRACE_ID}`; `llmobs_core::ids::TraceId` (derives `Copy, Eq, Hash, Ord`; `FromStr` accepts 32 hex digits of either case and rejects all zeros).

The `spans` and `traces` views expose `trace_id` as 32 lowercase hex `Utf8`, `ingest_time` (spans) as `Timestamp(Nanosecond, "UTC")`, and `metadata` as a `Map<Utf8, Utf8>` (spec 05 § Views).

## Code

`crates/storage/src/reads/deletions.rs`:

```rust
//! Scope resolution for deletion requests (spec 03 § 7, D-M-2, D-M-6).
//!
//! The server resolves a request to trace ids (and session ids for a user) before it
//! registers it with [`crate::Storage::register_deletion`].

use std::collections::{BTreeSet, HashSet};
use std::str::FromStr;
use std::time::Duration;

use arrow::array::{Array, ArrayRef, AsArray};
use arrow::compute::cast;
use arrow::datatypes::DataType;
use arrow::error::ArrowError;
use arrow::record_batch::RecordBatch;
use datafusion::common::ScalarValue;
use datafusion::error::DataFusionError;
use datafusion::logical_expr::{col, lit};
use futures::StreamExt;
use llmobs_core::ids::TraceId;
use llmobs_core::names::{environment, metadata_key};
use llmobs_core::schema::{spans, traces};

use super::{ListScope, present, scoped_traces};
use crate::filter::{FilterView, search};
use crate::query_error::{RunError, classify};
use crate::views::{QueryContext, TimeWindow, ViewName};

/// Most traces one filter deletion may cover (D-M-5).
pub const MAX_FILTER_DELETION_TRACES: usize = 100_000;

/// Traces a filter deletion covers: the traces list's filter, search, window and
/// default exclusions (D-M-2).
#[derive(Debug, Clone, Copy)]
pub struct FilterDeletionRequest<'a> {
    /// Filter text over `traces`. Empty or whitespace means no filter.
    pub filter: Option<&'a str>,
    /// Full-text search. Empty or whitespace means no search.
    pub search: Option<&'a str>,
    /// `start_time ∈ [from, to)`. Required.
    pub window: TimeWindow,
    /// Keep rows whose environment is the eval environment.
    pub include_eval: bool,
    /// Keep rows that belong to an experiment.
    pub include_experiments: bool,
    /// Most ids returned. The server passes `MAX_FILTER_DELETION_TRACES + 1` to detect overflow.
    pub limit: usize,
    /// Time allowed for planning and running.
    pub timeout: Duration,
}

/// One end user's scope (spec 03 § 7, D-A1-4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserDeletionScope {
    /// Traces with a span of the user, sorted by bytes, no duplicates.
    pub trace_ids: Vec<TraceId>,
    /// Non-null `session_id`s of those spans, sorted, no duplicates.
    pub session_ids: Vec<String>,
}
```

### `filter_trace_ids`

```rust
/// Trace ids of the traces-view rows that pass the window, the default exclusions, the
/// search and the filter, exactly as the trace list selects them. At most `limit`, sorted
/// by bytes, no duplicates. No order is applied before the limit: when more than `limit`
/// traces match, which ones come back is unspecified. The server only uses the result as
/// a set, or checks its length against the cap.
///
/// # Errors
///
/// [`RunError::Invalid`] for filter text; [`RunError::InvalidRequest`] for a bad search;
/// [`RunError::Timeout`]; the query's other errors as `ctx.fail` classifies them.
pub async fn filter_trace_ids(
    ctx: &QueryContext,
    request: FilterDeletionRequest<'_>,
) -> Result<Vec<TraceId>, RunError>
```

Body:
1. `let filter = match present(request.filter) { Some(text) => Some(ctx.parse_filter(FilterView::Traces, text)?), None => None };`. A parse error is returned before any timeout starts, as in `trace_refresh`.
2. `match tokio::time::timeout(request.timeout, matching(ctx, &request, filter)).await { Err(_) => Err(RunError::Timeout), Ok(result) => result }`.

`async fn matching(ctx, request: &FilterDeletionRequest<'_>, filter: Option<crate::filter::Filter>) -> Result<Vec<TraceId>, RunError>`:

```rust
let references_environment = filter.as_ref().is_some_and(|filter| filter.references_environment);
let scope = ListScope {
    window: Some(request.window),
    include_eval: request.include_eval,
    include_experiments: request.include_experiments,
};
let mut frame = scoped_traces(ctx, scope, references_environment).await?;
if let Some(text) = present(request.search)
    && let Some(expr) = search(FilterView::Traces, text, Some(request.window), ctx).await?
{
    frame = frame.filter(expr).map_err(classify)?;
}
if let Some(filter) = filter {
    frame = filter.apply(frame, ctx).await.map_err(classify)?;
}
let frame = frame
    .select(vec![col(traces::TRACE_ID)])
    .map_err(classify)?
    .limit(0, Some(request.limit))
    .map_err(classify)?;
let batches = frame.collect().await.map_err(|error| ctx.fail(error))?;
let mut ids = BTreeSet::new();
for batch in &batches {
    collect_ids(batch, 0, &mut ids)?;
}
Ok(ids.into_iter().collect())
```

Shared helpers (private):

```rust
fn arrow_failure(error: ArrowError) -> RunError {
    RunError::Internal(DataFusionError::ArrowError(Box::new(error), None))
}

/// The column as `Utf8`, whether the view yields `Utf8` or `Utf8View`.
fn utf8(column: &ArrayRef) -> Result<ArrayRef, RunError> {
    cast(column, &DataType::Utf8).map_err(arrow_failure)
}

/// Parses column `index` of `batch` (hex trace ids) into `out`.
fn collect_ids(batch: &RecordBatch, index: usize, out: &mut BTreeSet<TraceId>) -> Result<(), RunError> {
    let column = utf8(batch.column(index))?;
    let strings = column.as_string::<i32>();
    for row in 0..strings.len() {
        if strings.is_null(row) {
            continue;
        }
        let id = TraceId::from_str(strings.value(row)).map_err(|_| bad_id())?;
        out.insert(id);
    }
    Ok(())
}

fn bad_id() -> RunError {
    RunError::Internal(DataFusionError::Internal("a view returned an invalid trace id".to_owned()))
}
```

DataFusion is 55.2.0, where the variant is `DataFusionError::ArrowError(Box<ArrowError>, Option<String>)`.

### `user_deletion_scope`

```rust
/// Trace ids and session ids of the `spans` rows with `user_id = user_id` and
/// `ingest_time ≤ cutoff`. No window and no default exclusions: eval and experiment spans
/// of the user count too. One scan, streamed.
///
/// # Errors
///
/// [`RunError::Timeout`]; the query's other errors as `ctx.fail` classifies them.
pub async fn user_deletion_scope(
    ctx: &QueryContext,
    user_id: &str,
    cutoff: i64,
    timeout: Duration,
) -> Result<UserDeletionScope, RunError>
```

Body: `tokio::time::timeout(timeout, user_scope(ctx, user_id, cutoff))`, mapped as above.

`user_scope`:

```rust
let cutoff = lit(ScalarValue::TimestampNanosecond(Some(cutoff), Some("UTC".into())));
let predicate = col(spans::USER_ID)
    .eq(lit(user_id))
    .and(col(spans::INGEST_TIME).lt_eq(cutoff));
let frame = ctx
    .session()
    .table("spans")
    .await
    .map_err(classify)?
    .filter(predicate)
    .map_err(classify)?
    .select(vec![col(spans::TRACE_ID), col(spans::SESSION_ID)])
    .map_err(classify)?;
let mut stream = frame.execute_stream().await.map_err(|error| ctx.fail(error))?;
let mut trace_ids = BTreeSet::new();
let mut session_ids = BTreeSet::new();
while let Some(batch) = stream.next().await {
    let batch = batch.map_err(|error| ctx.fail(error))?;
    collect_ids(&batch, 0, &mut trace_ids)?;
    let sessions = utf8(batch.column(1))?;
    let sessions = sessions.as_string::<i32>();
    for row in 0..sessions.len() {
        if !sessions.is_null(row) {
            session_ids.insert(sessions.value(row).to_owned());
        }
    }
}
Ok(UserDeletionScope {
    trace_ids: trace_ids.into_iter().collect(),
    session_ids: session_ids.into_iter().collect(),
})
```

`futures` is already a dependency of `llmobs-storage`.

### `eval_trace_ids`

```rust
/// Evaluation traces (environment `llmobs-eval`) with a span whose
/// `metadata['llmobs.eval.target_trace_id']` is one of `targets`. Ids already in `targets`
/// are not returned. One level only, not transitive: an eval trace of an eval trace is
/// returned only when its own target is in `targets`. Sorted by bytes, no duplicates. One scan of the project's eval spans;
/// the match against `targets` is done in memory, batch by batch, so a large `targets`
/// does not grow the query plan (D-M-6). An empty `targets` returns at once without a
/// query. A target value that is not a valid trace id is skipped.
///
/// # Errors
///
/// [`RunError::Timeout`]; the query's other errors as `ctx.fail` classifies them.
pub async fn eval_trace_ids(
    ctx: &QueryContext,
    targets: &HashSet<TraceId>,
    timeout: Duration,
) -> Result<Vec<TraceId>, RunError>
```

Body: `tokio::time::timeout(timeout, eval_scan(ctx, targets))`, mapped as in `filter_trace_ids`, after the empty-`targets` early return.

`eval_scan(ctx, targets) -> Result<Vec<TraceId>, RunError>`:

```rust
let key = metadata_key::EVAL_TARGET_TRACE_ID.replace('\'', "''");
let target = ctx
    .session()
    .state()
    .create_logical_expr(&format!("metadata['{key}']"), ctx.view_schema(ViewName::Spans))
    .map_err(RunError::Internal)?;
let predicate = col(spans::ENVIRONMENT)
    .eq(lit(environment::EVAL))
    .and(target.clone().is_not_null());
let frame = ctx
    .session()
    .table("spans")
    .await
    .map_err(classify)?
    .filter(predicate)
    .map_err(classify)?
    .select(vec![col(spans::TRACE_ID), target.alias("target")])
    .map_err(classify)?;
let mut stream = frame.execute_stream().await.map_err(|error| ctx.fail(error))?;
let mut found = BTreeSet::new();
while let Some(batch) = stream.next().await {
    let batch = batch.map_err(|error| ctx.fail(error))?;
    let ids = utf8(batch.column(0))?;
    let ids = ids.as_string::<i32>();
    let values = utf8(batch.column(1))?;
    let values = values.as_string::<i32>();
    for row in 0..batch.num_rows() {
        if values.is_null(row) { continue; }
        let Ok(target) = TraceId::from_str(values.value(row)) else { continue };
        if !targets.contains(&target) { continue; }
        let id = TraceId::from_str(ids.value(row)).map_err(|_| bad_id())?;
        if !targets.contains(&id) {
            found.insert(id);
        }
    }
}
Ok(found.into_iter().collect())
```

`ViewName` is public in `crate::views`; `ctx.view_schema(ViewName::Spans)` works as written.

### Exports

- `reads/mod.rs`: add `mod deletions;` (alphabetical, after `cursor`) and `pub use deletions::{FilterDeletionRequest, MAX_FILTER_DELETION_TRACES, UserDeletionScope, eval_trace_ids, filter_trace_ids, user_deletion_scope};`.
- `lib.rs`: add the same six names to the `pub use reads::{…}` list, keeping it sorted as `rustfmt` sorts it.

## Tests

In `reads/deletions.rs`, `#[cfg(test)] mod tests`. Use the pattern of `reads/users.rs`:

```rust
fn context_of(spans: &[Span]) -> QueryContext {
    let mut query = example_query();
    query.spans = spans_table(spans);
    query.traces = traces_table(spans);
    QueryContext::new(query, None).expect("context")
}
const TIMEOUT: Duration = Duration::from_secs(30);
```

Copy the imports of the `reads/users.rs` tests that you need (`Span`, `QueryContext`, `example_query`, `spans_table`, `traces_table`, `Duration`). Times are small ns values. `span_row(trace, span, start, ingest)` builds a root span of trace `trace` (its `trace_id` has the big-endian bytes of `trace + 1` repeated, so byte order is trace-number order: "sorted by bytes" means ascending trace number). A second span in the same trace uses another `span` index and sets `parent_span_id` to the first span's id. Set extra fields on the returned `Span` (`name`, `user_id`, `session_id`, `environment`, `metadata`, `input`) as each test needs. `test_support::trace_id_of` is private: do not use it. Get expected ids from `span_row(n, 0, 0, 0).trace_id`, and hex for metadata targets from `.trace_id.to_hex()` (and `.to_hex().to_uppercase()`). Unless a test says otherwise, the window is `[0, 10_000)` and trace `n` starts at `n * 1_000`.

1. `filter_matches_window_and_filter`:
   - Spans of traces 1–4 at `start` 1_000, 2_000, 3_000, 4_000. Names: `chat`, `chat`, `tool`, `chat`.
   - The request has window `[1_500, 4_500)`, filter `name = 'chat'` and limit 10.
   - It returns traces 2 and 4, sorted by bytes.
2. `no_filter_and_no_search_take_the_whole_window`: same data, filter `None`, window `[0, 10_000)`. All 4 traces.
3. `default_exclusions_apply`:
   - Traces 5 and 6 are single root spans at 5_000 and 6_000. Trace 5 has `environment = Some("llmobs-eval")`. Trace 6 has `metadata["llmobs.experiment_id"] = "e1"`. A trace row takes its environment and metadata from its spans (`traces_table`), so these fields carry over.
   - With both flags false, neither is returned.
   - With `include_eval: true`: trace 5, not 6. With `include_experiments: true`: trace 6, not 5.
   - Filter `environment = 'llmobs-eval'` with both flags false returns trace 5: a filter that names `environment` lifts the eval exclusion.
4. `search_narrows_the_traces`: trace 3's span has `input` containing `refund please`. Search `refund`, window `[0, 10_000)`. Only trace 3.
5. `limit_caps_the_ids`: traces 1–5, limit 3. Exactly 3 ids, all from the 5.
6. `invalid_filter_is_invalid`: filter `name = ` gives `Err(RunError::Invalid(_))` (the parse error comes before the query; the window does not matter).
6a. `blank_filter_and_search_are_ignored`: `filter: Some("  ")` and `search: Some(" ")` over traces 1–4 return all 4.
6b. `search_and_filter_combine`: trace 3 (name `tool`) has `input` `refund please`. Search `refund` with filter `name = 'tool'` returns `[3]`; with filter `name = 'chat'` it returns `[]`.
6c. `search_counts_only_spans_in_the_window`: trace 3's root at 3_000 has no `refund`; a child span of trace 3 at 20_000 has `input` `refund`; trace 4's root at 4_000 has a child span at 4_500 with `input` `refund`. Window `[0, 10_000)`, search `refund`: returns `[4]` (trace 3's matching span is outside the window).
7. `user_scope_keeps_the_users_spans_up_to_the_cutoff`:
   - Trace 1 has a span of `u1` with session `s1` and ingest 100, and a second span (`span` index 1) of `u1`, session `s1`, ingest 200 (equal to the cut-off: included, `<=`).
   - Trace 2 has a span (`span` 0) of `u1` with no session and ingest 100, plus a span (`span` 1) of `u2` with session `s2`.
   - Trace 5 has a span of `U1` (upper case), session `s5`, ingest 100: ids match exactly, so it is not in the scope.
   - Trace 3 has a span of `u1` with session `s3` and ingest 500.
   - With cut-off 200, the result is `trace_ids` [1, 2] and `session_ids` [`s1`] (each once): trace 3 and `s3` are after the cut-off, `s2` belongs to `u2`, `U1` is another user.
8. `user_scope_ignores_exclusions`: trace 4 is an eval-environment span of `u1` ingested at 100. With cut-off 200, trace 4 is in the scope.
9. `user_scope_of_an_unknown_user_is_empty`: `user_id` `nobody`. Both lists are empty.
10. `eval_traces_follow_their_targets`:
    - Trace 10 has environment `llmobs-eval` and `metadata[EVAL_TARGET_TRACE_ID]` = trace 1's id in lowercase hex.
    - Trace 11 is the same with trace 2's id in **uppercase**.
    - Trace 12 has environment `production` and a target of trace 1.
    - Trace 13 has environment `llmobs-eval` and target `zz`.
    - Trace 14 has environment `llmobs-eval` and a target of trace 10.
    - Targets `{1}` return `[10]`.
    - Targets `{1, 2}` return `[10, 11]`.
    - Targets `{1, 10}` return `[14]`: 10 is already a target, and 14 targets 10.
    - Trace 15 has two spans with environment `llmobs-eval`: one with the target metadata (trace 3), one without. Targets `{3}` return `[15]` once.
11. `eval_traces_of_no_targets_is_empty`: an empty set returns `[]`.

The timeout branches are not unit-tested: a zero timeout against an in-memory table is not deterministic. They are the same `tokio::time::timeout` mapping as `trace_refresh`.

## Done when

- [ ] The three functions, the two request/scope types and the constant are public and re-exported from `llmobs_storage`.
- [ ] The 14 tests pass.
- [ ] `just check` is green; one commit `Add the deletion scope reads`.
- [ ] Report any deviation from this file.
