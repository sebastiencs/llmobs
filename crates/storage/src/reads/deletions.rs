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
use crate::filter::{Filter, FilterView, search};
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
) -> Result<Vec<TraceId>, RunError> {
    let filter = match present(request.filter) {
        Some(text) => Some(ctx.parse_filter(FilterView::Traces, text)?),
        None => None,
    };
    match tokio::time::timeout(request.timeout, matching(ctx, &request, filter)).await {
        Err(_) => Err(RunError::Timeout),
        Ok(result) => result,
    }
}

async fn matching(
    ctx: &QueryContext,
    request: &FilterDeletionRequest<'_>,
    filter: Option<Filter>,
) -> Result<Vec<TraceId>, RunError> {
    let references_environment = filter
        .as_ref()
        .is_some_and(|filter| filter.references_environment);
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
}

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
) -> Result<UserDeletionScope, RunError> {
    match tokio::time::timeout(timeout, user_scope(ctx, user_id, cutoff)).await {
        Err(_) => Err(RunError::Timeout),
        Ok(result) => result,
    }
}

async fn user_scope(
    ctx: &QueryContext,
    user_id: &str,
    cutoff: i64,
) -> Result<UserDeletionScope, RunError> {
    let cutoff = lit(ScalarValue::TimestampNanosecond(
        Some(cutoff),
        Some("UTC".into()),
    ));
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
    let mut stream = frame
        .execute_stream()
        .await
        .map_err(|error| ctx.fail(error))?;
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
}

/// Evaluation traces (environment `llmobs-eval`) with a span whose
/// `metadata['llmobs.eval.target_trace_id']` is one of `targets`. Ids already in `targets`
/// are not returned. One level only, not transitive: an eval trace of an eval trace is
/// returned only when its own target is in `targets`. Sorted by bytes, no duplicates. One
/// scan of the project's eval spans; the match against `targets` is done in memory, batch
/// by batch, so a large `targets` does not grow the query plan (D-M-6). An empty `targets`
/// returns at once without a query. A target value that is not a valid trace id is skipped.
///
/// # Errors
///
/// [`RunError::Timeout`]; the query's other errors as `ctx.fail` classifies them.
pub async fn eval_trace_ids(
    ctx: &QueryContext,
    targets: &HashSet<TraceId>,
    timeout: Duration,
) -> Result<Vec<TraceId>, RunError> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }
    match tokio::time::timeout(timeout, eval_scan(ctx, targets)).await {
        Err(_) => Err(RunError::Timeout),
        Ok(result) => result,
    }
}

async fn eval_scan(
    ctx: &QueryContext,
    targets: &HashSet<TraceId>,
) -> Result<Vec<TraceId>, RunError> {
    let key = metadata_key::EVAL_TARGET_TRACE_ID.replace('\'', "''");
    let target = ctx
        .session()
        .state()
        .create_logical_expr(
            &format!("metadata['{key}']"),
            ctx.view_schema(ViewName::Spans),
        )
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
    let mut stream = frame
        .execute_stream()
        .await
        .map_err(|error| ctx.fail(error))?;
    let mut found = BTreeSet::new();
    while let Some(batch) = stream.next().await {
        let batch = batch.map_err(|error| ctx.fail(error))?;
        let ids = utf8(batch.column(0))?;
        let ids = ids.as_string::<i32>();
        let values = utf8(batch.column(1))?;
        let values = values.as_string::<i32>();
        for row in 0..batch.num_rows() {
            if values.is_null(row) {
                continue;
            }
            let Ok(target) = TraceId::from_str(values.value(row)) else {
                continue;
            };
            if !targets.contains(&target) {
                continue;
            }
            let id = TraceId::from_str(ids.value(row)).map_err(|_| bad_id())?;
            if !targets.contains(&id) {
                found.insert(id);
            }
        }
    }
    Ok(found.into_iter().collect())
}

fn arrow_failure(error: ArrowError) -> RunError {
    RunError::Internal(DataFusionError::ArrowError(Box::new(error), None))
}

/// The column as `Utf8`, whether the view yields `Utf8` or `Utf8View`.
fn utf8(column: &ArrayRef) -> Result<ArrayRef, RunError> {
    cast(column, &DataType::Utf8).map_err(arrow_failure)
}

/// Parses column `index` of `batch` (hex trace ids) into `out`.
fn collect_ids(
    batch: &RecordBatch,
    index: usize,
    out: &mut BTreeSet<TraceId>,
) -> Result<(), RunError> {
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
    RunError::Internal(DataFusionError::Internal(
        "a view returned an invalid trace id".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Duration;

    use llmobs_core::ids::TraceId;
    use llmobs_core::span::Span;

    use super::{
        FilterDeletionRequest, RunError, UserDeletionScope, eval_trace_ids, filter_trace_ids,
        user_deletion_scope,
    };
    use crate::test_support::{example_query, span_row, spans_table, traces_table};
    use crate::views::{QueryContext, TimeWindow};

    const TIMEOUT: Duration = Duration::from_secs(30);

    fn context_of(spans: &[Span]) -> QueryContext {
        let mut query = example_query();
        query.spans = spans_table(spans);
        query.traces = traces_table(spans);
        QueryContext::new(query, None).expect("context")
    }

    fn window() -> TimeWindow {
        TimeWindow::new(0, 10_000).expect("window")
    }

    fn id(trace: u16) -> TraceId {
        span_row(trace, 0, 0, 0).trace_id
    }

    fn request<'a>(filter: Option<&'a str>, search: Option<&'a str>) -> FilterDeletionRequest<'a> {
        FilterDeletionRequest {
            filter,
            search,
            window: window(),
            include_eval: false,
            include_experiments: false,
            limit: 10,
            timeout: TIMEOUT,
        }
    }

    /// Traces 1–4 with one root span each, starting at `n * 1_000`.
    fn four_traces() -> Vec<Span> {
        let names = ["chat", "chat", "tool", "chat"];
        (1..=4)
            .zip(names)
            .map(|(trace, name)| {
                let mut span = span_row(trace, 0, i64::from(trace) * 1_000, 0);
                span.name = name.to_owned();
                span
            })
            .collect()
    }

    /// A child span of `trace` (index 1) whose parent is the root (index 0).
    fn child(trace: u16, start: i64) -> Span {
        let root = span_row(trace, 0, 0, 0);
        let mut span = span_row(trace, 1, start, 0);
        span.parent_span_id = Some(root.span_id);
        span
    }

    #[tokio::test]
    async fn filter_matches_window_and_filter() {
        let ctx = context_of(&four_traces());
        let mut req = request(Some("name = 'chat'"), None);
        req.window = TimeWindow::new(1_500, 4_500).expect("window");
        let ids = filter_trace_ids(&ctx, req).await.expect("filter");
        assert_eq!(ids, [id(2), id(4)]);
    }

    #[tokio::test]
    async fn no_filter_and_no_search_take_the_whole_window() {
        let ctx = context_of(&four_traces());
        let ids = filter_trace_ids(&ctx, request(None, None))
            .await
            .expect("window");
        assert_eq!(ids, [id(1), id(2), id(3), id(4)]);
    }

    #[tokio::test]
    async fn default_exclusions_apply() {
        let mut eval = span_row(5, 0, 5_000, 0);
        eval.environment = Some("llmobs-eval".to_owned());
        let mut experiment = span_row(6, 0, 6_000, 0);
        experiment
            .metadata
            .insert("llmobs.experiment_id".to_owned(), "e1".to_owned());
        let ctx = context_of(&[eval, experiment]);

        let ids = filter_trace_ids(&ctx, request(None, None))
            .await
            .expect("default");
        assert_eq!(ids, []);

        let mut req = request(None, None);
        req.include_eval = true;
        let ids = filter_trace_ids(&ctx, req).await.expect("eval");
        assert_eq!(ids, [id(5)]);

        let mut req = request(None, None);
        req.include_experiments = true;
        let ids = filter_trace_ids(&ctx, req).await.expect("experiments");
        assert_eq!(ids, [id(6)]);

        let ids = filter_trace_ids(&ctx, request(Some("environment = 'llmobs-eval'"), None))
            .await
            .expect("environment filter");
        assert_eq!(ids, [id(5)]);
    }

    #[tokio::test]
    async fn search_narrows_the_traces() {
        let mut spans = four_traces();
        spans[2].input = Some("refund please".to_owned());
        let ctx = context_of(&spans);
        let ids = filter_trace_ids(&ctx, request(None, Some("refund")))
            .await
            .expect("search");
        assert_eq!(ids, [id(3)]);
    }

    #[tokio::test]
    async fn limit_caps_the_ids() {
        let spans: Vec<Span> = (1..=5).map(|trace| span_row(trace, 0, 1_000, 0)).collect();
        let ctx = context_of(&spans);
        let mut req = request(None, None);
        req.limit = 3;
        let ids = filter_trace_ids(&ctx, req).await.expect("limit");
        assert_eq!(ids.len(), 3);
        let all: Vec<TraceId> = (1..=5).map(id).collect();
        assert!(ids.iter().all(|id| all.contains(id)));
    }

    #[tokio::test]
    async fn invalid_filter_is_invalid() {
        let ctx = context_of(&four_traces());
        let result = filter_trace_ids(&ctx, request(Some("name = "), None)).await;
        assert!(matches!(result, Err(RunError::Invalid(_))), "{result:?}");
    }

    #[tokio::test]
    async fn blank_filter_and_search_are_ignored() {
        let ctx = context_of(&four_traces());
        let ids = filter_trace_ids(&ctx, request(Some("  "), Some(" ")))
            .await
            .expect("blank");
        assert_eq!(ids, [id(1), id(2), id(3), id(4)]);
    }

    #[tokio::test]
    async fn search_and_filter_combine() {
        let mut spans = four_traces();
        spans[2].input = Some("refund please".to_owned());
        let ctx = context_of(&spans);
        let ids = filter_trace_ids(&ctx, request(Some("name = 'tool'"), Some("refund")))
            .await
            .expect("tool");
        assert_eq!(ids, [id(3)]);
        let ids = filter_trace_ids(&ctx, request(Some("name = 'chat'"), Some("refund")))
            .await
            .expect("chat");
        assert_eq!(ids, []);
    }

    #[tokio::test]
    async fn search_counts_only_spans_in_the_window() {
        let mut spans = four_traces();
        let mut late = child(3, 20_000);
        late.input = Some("refund".to_owned());
        let mut inside = child(4, 4_500);
        inside.input = Some("refund".to_owned());
        spans.push(late);
        spans.push(inside);
        let ctx = context_of(&spans);
        let ids = filter_trace_ids(&ctx, request(None, Some("refund")))
            .await
            .expect("window search");
        assert_eq!(ids, [id(4)]);
    }

    #[tokio::test]
    async fn user_scope_keeps_the_users_spans_up_to_the_cutoff() {
        let mut t1a = span_row(1, 0, 1_000, 100);
        t1a.user_id = Some("u1".to_owned());
        t1a.session_id = Some("s1".to_owned());
        let mut t1b = span_row(1, 1, 1_100, 200);
        t1b.user_id = Some("u1".to_owned());
        t1b.session_id = Some("s1".to_owned());
        let mut t2a = span_row(2, 0, 2_000, 100);
        t2a.user_id = Some("u1".to_owned());
        let mut t2b = span_row(2, 1, 2_100, 100);
        t2b.user_id = Some("u2".to_owned());
        t2b.session_id = Some("s2".to_owned());
        let mut t5 = span_row(5, 0, 5_000, 100);
        t5.user_id = Some("U1".to_owned());
        t5.session_id = Some("s5".to_owned());
        let mut t3 = span_row(3, 0, 3_000, 500);
        t3.user_id = Some("u1".to_owned());
        t3.session_id = Some("s3".to_owned());
        let ctx = context_of(&[t1a, t1b, t2a, t2b, t5, t3]);

        let scope = user_deletion_scope(&ctx, "u1", 200, TIMEOUT)
            .await
            .expect("scope");
        assert_eq!(
            scope,
            UserDeletionScope {
                trace_ids: vec![id(1), id(2)],
                session_ids: vec!["s1".to_owned()],
            }
        );
    }

    #[tokio::test]
    async fn user_scope_ignores_exclusions() {
        let mut eval = span_row(4, 0, 4_000, 100);
        eval.user_id = Some("u1".to_owned());
        eval.environment = Some("llmobs-eval".to_owned());
        let ctx = context_of(&[eval]);
        let scope = user_deletion_scope(&ctx, "u1", 200, TIMEOUT)
            .await
            .expect("scope");
        assert_eq!(scope.trace_ids, [id(4)]);
    }

    #[tokio::test]
    async fn user_scope_of_an_unknown_user_is_empty() {
        let mut span = span_row(1, 0, 1_000, 100);
        span.user_id = Some("u1".to_owned());
        let ctx = context_of(&[span]);
        let scope = user_deletion_scope(&ctx, "nobody", 200, TIMEOUT)
            .await
            .expect("scope");
        assert_eq!(scope, UserDeletionScope::default());
    }

    /// An eval trace (`trace`) whose one span targets `target` (hex text as written).
    fn eval_span(trace: u16, target: &str) -> Span {
        let mut span = span_row(trace, 0, i64::from(trace) * 1_000, 0);
        span.environment = Some("llmobs-eval".to_owned());
        span.metadata
            .insert("llmobs.eval.target_trace_id".to_owned(), target.to_owned());
        span
    }

    #[tokio::test]
    async fn eval_traces_follow_their_targets() {
        let target = |trace: u16| span_row(trace, 0, 0, 0).trace_id.to_hex();
        let mut spans = vec![
            eval_span(10, &target(1)),
            eval_span(11, &target(2).to_uppercase()),
            eval_span(13, "zz"),
            eval_span(14, &target(10)),
        ];
        let mut production = eval_span(12, &target(1));
        production.environment = Some("production".to_owned());
        spans.push(production);
        let mut plain = span_row(15, 1, 15_000, 0);
        plain.environment = Some("llmobs-eval".to_owned());
        spans.push(eval_span(15, &target(3)));
        spans.push(plain);
        let ctx = context_of(&spans);

        let targets = |ids: &[u16]| -> HashSet<TraceId> { ids.iter().map(|&t| id(t)).collect() };
        let found = eval_trace_ids(&ctx, &targets(&[1]), TIMEOUT)
            .await
            .expect("one target");
        assert_eq!(found, [id(10)]);

        let found = eval_trace_ids(&ctx, &targets(&[1, 2]), TIMEOUT)
            .await
            .expect("two targets");
        assert_eq!(found, [id(10), id(11)]);

        let found = eval_trace_ids(&ctx, &targets(&[1, 10]), TIMEOUT)
            .await
            .expect("chained");
        assert_eq!(found, [id(14)]);

        let found = eval_trace_ids(&ctx, &targets(&[3]), TIMEOUT)
            .await
            .expect("once");
        assert_eq!(found, [id(15)]);
    }

    #[tokio::test]
    async fn eval_traces_of_no_targets_is_empty() {
        let ctx = context_of(&[eval_span(10, "00000000000000000000000000000001")]);
        let found = eval_trace_ids(&ctx, &HashSet::new(), TIMEOUT)
            .await
            .expect("empty");
        assert_eq!(found, []);
    }
}
