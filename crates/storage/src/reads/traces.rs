//! Trace and span lists, plus trace and span detail.
//!
//! Without score functions, the default list is `Limit(n+1) ← Sort(start_time DESC NULLS LAST, trace_id DESC) ← Filter ← traces view`.

use std::future::Future;
use std::time::Duration;

use arrow::compute::concat_batches;
use arrow::record_batch::RecordBatch;
use datafusion::error::DataFusionError;
use datafusion::logical_expr::{Expr, col, lit};
use datafusion::prelude::DataFrame;
use llmobs_core::schema::{spans, traces};

use super::cursor::{after, assemble, decode, order_by};
use super::{ListScope, Page, SortOrder, present};
use crate::filter::{FilterView, exclusions, search, time_window};
use crate::query_error::{RunError, classify};
use crate::views::QueryContext;

const LIST: &str = "traces";
const SPANS: &str = "spans";
const DURATION_NS: &str = "duration_ns";
const DURATION_MS: &str = "duration_ms";
const TRACE_TIES: &[&str] = &[traces::TRACE_ID];
const SPAN_TIES: &[&str] = &[spans::SPAN_ID, spans::TRACE_ID];

/// Most spans `trace_detail` returns for one trace.
pub const MAX_TRACE_SPANS: usize = 20_000;

/// Span list columns: [`llmobs_core::schema::SPANS_LIGHT_COLUMNS`] without
/// `project_id`, with `duration_ms` after `duration_ns`.
pub const SPAN_LIST_COLUMNS: &[&str] = &[
    spans::TRACE_ID,
    spans::SPAN_ID,
    spans::PARENT_SPAN_ID,
    spans::NAME,
    spans::SPAN_KIND,
    spans::START_TIME,
    spans::END_TIME,
    spans::DURATION_NS,
    DURATION_MS,
    spans::STATUS,
    spans::STATUS_MESSAGE,
    spans::SERVICE_NAME,
    spans::SESSION_ID,
    spans::USER_ID,
    spans::ENVIRONMENT,
    spans::RELEASE,
    spans::TAGS,
    spans::METADATA,
    spans::PROVIDER,
    spans::REQUEST_MODEL,
    spans::RESPONSE_MODEL,
    spans::MODEL,
    spans::INPUT_TOKENS,
    spans::OUTPUT_TOKENS,
    spans::CACHE_READ_TOKENS,
    spans::CACHE_WRITE_TOKENS,
    spans::REASONING_TOKENS,
    spans::TOTAL_TOKENS,
    spans::COST_INPUT,
    spans::COST_OUTPUT,
    spans::COST_TOTAL,
    spans::TTFT_NS,
    spans::INPUT_PREVIEW,
    spans::OUTPUT_PREVIEW,
];

/// Column a trace list sorts by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceSort {
    /// `start_time`.
    StartTime,
    /// `duration_ns`.
    Duration,
    /// `cost_total`.
    CostTotal,
    /// `total_tokens`.
    TotalTokens,
}

impl TraceSort {
    fn column(self) -> &'static str {
        match self {
            Self::StartTime => traces::START_TIME,
            Self::Duration => DURATION_NS,
            Self::CostTotal => traces::COST_TOTAL,
            Self::TotalTokens => traces::TOTAL_TOKENS,
        }
    }
}

/// One page of the traces view.
#[derive(Debug, Clone)]
pub struct TraceListRequest<'a> {
    /// Filter text. Empty or whitespace means no filter.
    pub filter: Option<&'a str>,
    /// Search text. Empty or whitespace means no search.
    pub search: Option<&'a str>,
    /// Window and default exclusions.
    pub scope: ListScope,
    /// Sort column.
    pub sort: TraceSort,
    /// Sort direction. Nulls are last.
    pub order: SortOrder,
    /// Rows to return, from 1 to [`super::MAX_PAGE`].
    pub limit: usize,
    /// Cursor from the previous page.
    pub cursor: Option<&'a str>,
    /// Time allowed for planning and running.
    pub timeout: Duration,
}

/// Column a span list sorts by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanSort {
    /// `start_time`.
    StartTime,
    /// `duration_ns`.
    Duration,
    /// `cost_total`.
    CostTotal,
    /// `total_tokens`.
    TotalTokens,
}

impl SpanSort {
    fn column(self) -> &'static str {
        match self {
            Self::StartTime => spans::START_TIME,
            Self::Duration => DURATION_NS,
            Self::CostTotal => spans::COST_TOTAL,
            Self::TotalTokens => spans::TOTAL_TOKENS,
        }
    }
}

/// One page of the spans view, projected to [`SPAN_LIST_COLUMNS`].
#[derive(Debug, Clone)]
pub struct SpanListRequest<'a> {
    /// Filter text. Empty or whitespace means no filter.
    pub filter: Option<&'a str>,
    /// Search text. Empty or whitespace means no search.
    pub search: Option<&'a str>,
    /// Window and default exclusions.
    pub scope: ListScope,
    /// Sort column.
    pub sort: SpanSort,
    /// Sort direction. Nulls are last.
    pub order: SortOrder,
    /// Rows to return, from 1 to [`super::MAX_PAGE`].
    pub limit: usize,
    /// Cursor from the previous page.
    pub cursor: Option<&'a str>,
    /// Time allowed for planning and running.
    pub timeout: Duration,
}

/// One trace row and its spans.
#[derive(Debug)]
pub struct TraceDetailData {
    /// The `traces` view row, all columns.
    pub trace: RecordBatch,
    /// [`SPAN_LIST_COLUMNS`], ordered `start_time ASC, span_id ASC`.
    pub spans: Vec<RecordBatch>,
    /// True when more than [`MAX_TRACE_SPANS`] spans were stored.
    pub truncated: bool,
}

/// One page of traces, in traces-view column order.
///
/// # Errors
///
/// [`RunError::InvalidRequest`] for a bad limit, cursor, or search window.
/// [`RunError::Invalid`] for filter text. [`RunError::Timeout`] when the query
/// exceeds `timeout`. DataFusion failures go through [`classify`].
pub async fn trace_list(
    ctx: &QueryContext,
    request: TraceListRequest<'_>,
) -> Result<Page, RunError> {
    run_timed(request.limit, request.timeout, async {
        let frame = trace_frame(ctx, &request).await?;
        finish(frame, &ListPlan::traces(&request)).await
    })
    .await
}

/// One page of spans. Exclusions use the span's own environment and metadata.
///
/// # Errors
///
/// Same as [`trace_list`].
pub async fn span_list(ctx: &QueryContext, request: SpanListRequest<'_>) -> Result<Page, RunError> {
    run_list(ctx, ListPlan::spans(&request), request.timeout).await
}

/// The trace and its spans. Default exclusions do not apply.
///
/// `trace_id` is 32 hex digits, either case. An unknown id is `Ok(None)`.
///
/// # Errors
///
/// [`RunError::InvalidRequest`] when `trace_id` is not 32 hex digits.
/// [`RunError::Timeout`] when the query exceeds `timeout`.
pub async fn trace_detail(
    ctx: &QueryContext,
    trace_id: &str,
    timeout: Duration,
) -> Result<Option<TraceDetailData>, RunError> {
    trace_detail_limited(ctx, trace_id, timeout, MAX_TRACE_SPANS).await
}

/// `trace_detail` with a caller-chosen span cap. Tests pass a small cap.
async fn trace_detail_limited(
    ctx: &QueryContext,
    trace_id: &str,
    timeout: Duration,
    cap: usize,
) -> Result<Option<TraceDetailData>, RunError> {
    let trace_id = hex_id(trace_id, 32, "trace_id")?;
    match tokio::time::timeout(timeout, load_trace(ctx, &trace_id, cap)).await {
        Err(_) => Err(RunError::Timeout),
        Ok(result) => result,
    }
}

/// All columns of the span with this `(trace_id, span_id)`.
///
/// Duplicate rows keep the greatest `ingest_time`. An unknown pair is `Ok(None)`.
///
/// # Errors
///
/// [`RunError::InvalidRequest`] when an id has the wrong number of hex digits.
/// [`RunError::Timeout`] when the query exceeds `timeout`.
pub async fn span_detail(
    ctx: &QueryContext,
    trace_id: &str,
    span_id: &str,
    timeout: Duration,
) -> Result<Option<RecordBatch>, RunError> {
    let trace_id = hex_id(trace_id, 32, "trace_id")?;
    let span_id = hex_id(span_id, 16, "span_id")?;
    match tokio::time::timeout(timeout, load_span(ctx, &trace_id, &span_id)).await {
        Err(_) => Err(RunError::Timeout),
        Ok(result) => result,
    }
}

struct ListPlan<'a> {
    table: &'static str,
    view: FilterView,
    list: &'static str,
    sort: &'static str,
    ties: &'static [&'static str],
    filter: Option<&'a str>,
    search: Option<&'a str>,
    scope: ListScope,
    order: SortOrder,
    limit: usize,
    cursor: Option<&'a str>,
    columns: Option<&'static [&'static str]>,
}

impl<'a> ListPlan<'a> {
    fn traces(request: &TraceListRequest<'a>) -> Self {
        Self {
            table: LIST,
            view: FilterView::Traces,
            list: LIST,
            sort: request.sort.column(),
            ties: TRACE_TIES,
            filter: request.filter,
            search: request.search,
            scope: request.scope,
            order: request.order,
            limit: request.limit,
            cursor: request.cursor,
            columns: None,
        }
    }

    fn spans(request: &SpanListRequest<'a>) -> Self {
        Self {
            table: SPANS,
            view: FilterView::Spans,
            list: SPANS,
            sort: request.sort.column(),
            ties: SPAN_TIES,
            filter: request.filter,
            search: request.search,
            scope: request.scope,
            order: request.order,
            limit: request.limit,
            cursor: request.cursor,
            columns: Some(SPAN_LIST_COLUMNS),
        }
    }
}

async fn run_list(
    ctx: &QueryContext,
    plan: ListPlan<'_>,
    timeout: Duration,
) -> Result<Page, RunError> {
    run_timed(plan.limit, timeout, async {
        let frame = list_frame(ctx, &plan).await?;
        finish(frame, &plan).await
    })
    .await
}

async fn run_timed<F>(limit: usize, timeout: Duration, future: F) -> Result<Page, RunError>
where
    F: Future<Output = Result<Page, RunError>>,
{
    if !(1..=super::MAX_PAGE).contains(&limit) {
        return Err(RunError::InvalidRequest(
            "limit must be between 1 and 1000".to_owned(),
        ));
    }
    match tokio::time::timeout(timeout, future).await {
        Err(_) => Err(RunError::Timeout),
        Ok(result) => result,
    }
}

async fn finish(frame: DataFrame, plan: &ListPlan<'_>) -> Result<Page, RunError> {
    let batches = frame.collect().await.map_err(classify)?;
    assemble(
        batches, plan.limit, plan.list, plan.sort, plan.order, plan.ties,
    )
}

/// The planned frame, before it is collected. Tests read the plan from this.
async fn trace_frame(
    ctx: &QueryContext,
    request: &TraceListRequest<'_>,
) -> Result<DataFrame, RunError> {
    list_frame(ctx, &ListPlan::traces(request)).await
}

async fn list_frame(ctx: &QueryContext, plan: &ListPlan<'_>) -> Result<DataFrame, RunError> {
    let filter = match present(plan.filter) {
        Some(text) => Some(ctx.parse_filter(plan.view, text)?),
        None => None,
    };
    let references_environment = filter
        .as_ref()
        .is_some_and(|filter| filter.references_environment);
    let cursor = match plan.cursor {
        Some(text) => {
            let cursor = decode(text, plan.list, plan.sort, plan.order)?;
            if cursor.tie_len() != plan.ties.len() {
                return Err(RunError::InvalidRequest("invalid cursor".to_owned()));
            }
            Some(cursor)
        }
        None => None,
    };

    let mut frame = ctx.session().table(plan.table).await.map_err(classify)?;
    let mut conjuncts = Vec::new();
    if let Some(window) = plan.scope.window {
        conjuncts.push(time_window(window));
    }
    if let Some(expr) = exclusions(
        ctx,
        plan.view,
        plan.scope.include_eval,
        plan.scope.include_experiments,
        references_environment,
    )? {
        conjuncts.push(expr);
    }
    if let Some(text) = present(plan.search)
        && let Some(expr) = search(plan.view, text, plan.scope.window, ctx).await?
    {
        conjuncts.push(expr);
    }
    if let Some(cursor) = &cursor {
        conjuncts.push(after(cursor, plan.sort, plan.ties));
    }
    if let Some(predicate) = conjuncts.into_iter().reduce(Expr::and) {
        frame = frame.filter(predicate).map_err(classify)?;
    }
    if let Some(filter) = filter {
        frame = filter.apply(frame, ctx).await.map_err(classify)?;
    }
    frame = frame
        .sort(order_by(plan.sort, plan.ties, plan.order))
        .map_err(classify)?
        .limit(0, Some(plan.limit + 1))
        .map_err(classify)?;
    if let Some(columns) = plan.columns {
        frame = frame
            .select(columns.iter().copied().map(col))
            .map_err(classify)?;
    }
    Ok(frame)
}

async fn load_trace(
    ctx: &QueryContext,
    trace_id: &str,
    cap: usize,
) -> Result<Option<TraceDetailData>, RunError> {
    let trace = ctx
        .session()
        .table(LIST)
        .await
        .map_err(classify)?
        .filter(col(traces::TRACE_ID).eq(lit(trace_id)))
        .map_err(classify)?;
    let trace = trace.collect().await.map_err(classify)?;
    let Some(trace) = single_row(&trace)? else {
        return Ok(None);
    };
    let spans = ctx
        .session()
        .table(SPANS)
        .await
        .map_err(classify)?
        .filter(col(spans::TRACE_ID).eq(lit(trace_id)))
        .map_err(classify)?
        .select(SPAN_LIST_COLUMNS.iter().copied().map(col))
        .map_err(classify)?
        .sort(order_by(
            spans::START_TIME,
            &[spans::SPAN_ID],
            SortOrder::Asc,
        ))
        .map_err(classify)?
        .limit(0, Some(cap + 1))
        .map_err(classify)?;
    let (spans, truncated) = cap_rows(spans.collect().await.map_err(classify)?, cap);
    Ok(Some(TraceDetailData {
        trace,
        spans,
        truncated,
    }))
}

async fn load_span(
    ctx: &QueryContext,
    trace_id: &str,
    span_id: &str,
) -> Result<Option<RecordBatch>, RunError> {
    let frame = ctx
        .session()
        .table(SPANS)
        .await
        .map_err(classify)?
        .filter(
            col(spans::TRACE_ID)
                .eq(lit(trace_id))
                .and(col(spans::SPAN_ID).eq(lit(span_id))),
        )
        .map_err(classify)?
        .sort(order_by(spans::INGEST_TIME, &[], SortOrder::Desc))
        .map_err(classify)?
        .limit(0, Some(1))
        .map_err(classify)?;
    let batches = frame.collect().await.map_err(classify)?;
    single_row(&batches)
}

fn hex_id(text: &str, width: usize, name: &str) -> Result<String, RunError> {
    if text.len() == width && text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Ok(text.to_ascii_lowercase());
    }
    Err(RunError::InvalidRequest(format!(
        "{name} must be {width} hex digits"
    )))
}

fn single_row(batches: &[RecordBatch]) -> Result<Option<RecordBatch>, RunError> {
    let Some(schema) = batches
        .iter()
        .find(|batch| batch.num_rows() > 0)
        .map(RecordBatch::schema)
    else {
        return Ok(None);
    };
    let batch = concat_batches(&schema, batches)
        .map_err(|error| RunError::Internal(DataFusionError::ArrowError(Box::new(error), None)))?;
    if batch.num_rows() == 0 {
        return Ok(None);
    }
    if batch.num_rows() == 1 {
        return Ok(Some(batch));
    }
    Ok(Some(batch.slice(0, 1)))
}

fn cap_rows(batches: Vec<RecordBatch>, limit: usize) -> (Vec<RecordBatch>, bool) {
    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    if total <= limit {
        return (batches, false);
    }
    let mut kept = Vec::new();
    let mut remaining = limit;
    for batch in batches {
        if remaining == 0 {
            break;
        }
        if batch.num_rows() <= remaining {
            remaining -= batch.num_rows();
            kept.push(batch);
        } else {
            kept.push(batch.slice(0, remaining));
            remaining = 0;
        }
    }
    (kept, true)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::path::PathBuf;
    use std::str::FromStr;
    use std::sync::Arc;
    use std::time::Duration;

    use arrow::json::{ArrayWriter, WriterBuilder};
    use arrow::record_batch::RecordBatch;
    use datafusion::execution::runtime_env::RuntimeEnv;
    use datafusion::physical_plan::displayable;
    use llmobs_core::ids::{SpanId, TraceId};
    use llmobs_core::schema::{SPANS_LIGHT_COLUMNS, spans};
    use llmobs_core::span::Span;
    use llmobs_core::time_serde::parse_rfc3339;
    use serde_json::Value;

    use super::{
        SPAN_LIST_COLUMNS, SpanListRequest, SpanSort, TraceListRequest, TraceSort, span_detail,
        span_list, trace_detail, trace_detail_limited, trace_frame, trace_list,
    };
    use crate::query_error::{Position, RunError};
    use crate::reads::{ListScope, Page, SortOrder};
    use crate::test_support::{
        dataset_scores, dataset_spans, example_query, live_scores_table, spans_table, traces_table,
    };
    use crate::views::{ProjectQuery, QueryContext, TimeWindow};

    const TIMEOUT: Duration = Duration::from_secs(30);

    fn context() -> QueryContext {
        QueryContext::new(example_query(), None).expect("context")
    }

    fn scope(window: Option<TimeWindow>, include_eval: bool) -> ListScope {
        ListScope {
            window,
            include_eval,
            include_experiments: false,
        }
    }

    fn listed<'a>(
        scope: ListScope,
        sort: TraceSort,
        order: SortOrder,
        limit: usize,
    ) -> TraceListRequest<'a> {
        TraceListRequest {
            filter: None,
            search: None,
            scope,
            sort,
            order,
            limit,
            cursor: None,
            timeout: TIMEOUT,
        }
    }

    fn fixture(name: &str) -> Value {
        let text = match name {
            "traces.json" => include_str!("../../../../fixtures/api/responses/traces.json"),
            "traces-include-eval.json" => {
                include_str!("../../../../fixtures/api/responses/traces-include-eval.json")
            }
            "spans.json" => include_str!("../../../../fixtures/api/responses/spans.json"),
            other => panic!("unknown fixture {other}"),
        };
        serde_json::from_str(text).expect(name)
    }

    fn json_rows(batches: &[RecordBatch]) -> Vec<Value> {
        if batches.iter().all(|batch| batch.num_rows() == 0) {
            return Vec::new();
        }
        let mut buffer = Vec::new();
        let mut writer: ArrayWriter<&mut Vec<u8>> = WriterBuilder::new()
            .with_explicit_nulls(true)
            .with_timestamp_tz_format("%Y-%m-%dT%H:%M:%S%.9fZ".to_owned())
            .with_timestamp_format("%Y-%m-%dT%H:%M:%S%.9f".to_owned())
            .build(&mut buffer);
        for batch in batches {
            writer.write(batch).expect("encode");
        }
        writer.finish().expect("finish");
        drop(writer);
        serde_json::from_slice(&buffer).expect("rows")
    }

    fn rows(page: &Page) -> Vec<Value> {
        json_rows(&page.batches)
    }

    fn ids(page: &Page) -> Vec<String> {
        rows(page)
            .iter()
            .map(|row| row["trace_id"].as_str().expect("trace_id").to_owned())
            .collect()
    }

    fn values_match(actual: &Value, expected: &Value) -> bool {
        match (actual, expected) {
            (Value::Number(left), Value::Number(right)) => match (left.as_i64(), right.as_i64()) {
                (Some(left), Some(right)) => left == right,
                _ => {
                    let left = left.as_f64().expect("left");
                    let right = right.as_f64().expect("right");
                    (left - right).abs() < 1e-12
                }
            },
            (Value::Array(left), Value::Array(right)) => {
                left.len() == right.len()
                    && left
                        .iter()
                        .zip(right)
                        .all(|(left, right)| values_match(left, right))
            }
            (Value::Object(left), Value::Object(right)) => {
                left.len() == right.len()
                    && left.iter().all(|(key, value)| {
                        right
                            .get(key)
                            .is_some_and(|expected| values_match(value, expected))
                    })
            }
            _ => actual == expected,
        }
    }

    fn assert_scalars(page: &Page, fixture: &Value) {
        let expected = fixture["data"].as_array().expect("data");
        let actual = rows(page);
        assert_eq!(actual.len(), expected.len());
        let fields = [
            "name",
            "status",
            "span_count",
            "error_count",
            "cost_total",
            "total_tokens",
            "tags",
            "models",
            "metadata",
            "session_id",
            "user_id",
            "environment",
        ];
        for (row, trace) in actual.iter().zip(expected) {
            assert_eq!(row["trace_id"], trace["trace_id"]);
            for field in fields {
                let expected = trace.get(field).unwrap_or(&Value::Null);
                assert!(
                    values_match(&row[field], expected),
                    "{field}: {} vs {expected}",
                    row[field]
                );
            }
        }
    }

    fn nanos(text: &str) -> i64 {
        parse_rfc3339(text).expect(text)
    }

    fn day() -> TimeWindow {
        TimeWindow::new(nanos("2026-01-01T00:00:00Z"), nanos("2026-01-02T00:00:00Z")).expect("day")
    }

    fn text_has(span: &Span, needle: &str) -> bool {
        let needle = needle.to_lowercase();
        [
            span.input.as_deref(),
            span.output.as_deref(),
            Some(span.name.as_str()),
            span.system_instructions.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|text| text.to_lowercase().contains(&needle))
    }

    #[tokio::test]
    async fn default_list_matches_the_trace_fixture() {
        let page = trace_list(
            &context(),
            listed(
                scope(None, false),
                TraceSort::StartTime,
                SortOrder::Desc,
                50,
            ),
        )
        .await
        .expect("list");
        let fixture = fixture("traces.json");
        let expected: Vec<_> = fixture["data"]
            .as_array()
            .expect("data")
            .iter()
            .map(|row| row["trace_id"].as_str().expect("id").to_owned())
            .collect();
        assert_eq!(ids(&page), expected);
        assert_scalars(&page, &fixture);
        assert!(page.next_cursor.is_none());
    }

    #[tokio::test]
    async fn include_eval_matches_the_fixture_order() {
        let page = trace_list(
            &context(),
            listed(scope(None, true), TraceSort::StartTime, SortOrder::Desc, 50),
        )
        .await
        .expect("list");
        let fixture = fixture("traces-include-eval.json");
        let expected: Vec<_> = fixture["data"]
            .as_array()
            .expect("data")
            .iter()
            .map(|row| row["trace_id"].as_str().expect("id").to_owned())
            .collect();
        assert_eq!(ids(&page), expected);
    }

    #[tokio::test]
    async fn pages_cover_every_sort_including_nulls() {
        let ctx = context();
        let sorts = [
            TraceSort::StartTime,
            TraceSort::Duration,
            TraceSort::CostTotal,
            TraceSort::TotalTokens,
        ];
        let orders = [SortOrder::Asc, SortOrder::Desc];
        for sort in sorts {
            for order in orders {
                let full = trace_list(&ctx, listed(scope(None, true), sort, order, 50))
                    .await
                    .expect("full");
                let expected = ids(&full);
                assert!(full.next_cursor.is_none());
                let mut walked = Vec::new();
                let mut cursor = None;
                loop {
                    let mut request = listed(scope(None, true), sort, order, 2);
                    request.cursor = cursor.as_deref();
                    let page = trace_list(&ctx, request).await.expect("page");
                    let page_ids = ids(&page);
                    assert!(page_ids.len() <= 2);
                    walked.extend(page_ids);
                    match page.next_cursor {
                        Some(next) => cursor = Some(next),
                        None => break,
                    }
                }
                assert_eq!(walked, expected, "{sort:?} {order:?}");
            }
        }
        let costs = trace_list(
            &ctx,
            listed(scope(None, true), TraceSort::CostTotal, SortOrder::Desc, 50),
        )
        .await
        .expect("costs");
        assert!(
            rows(&costs).iter().any(|row| row["cost_total"].is_null()),
            "cost_total has a null"
        );
        let tokens = trace_list(
            &ctx,
            listed(
                scope(None, true),
                TraceSort::TotalTokens,
                SortOrder::Asc,
                50,
            ),
        )
        .await
        .expect("tokens");
        assert!(
            rows(&tokens)
                .iter()
                .any(|row| row["total_tokens"].is_null()),
            "total_tokens has a null"
        );
    }

    #[tokio::test]
    async fn a_cursor_for_another_sort_is_rejected() {
        let ctx = context();
        let mut first = listed(scope(None, true), TraceSort::CostTotal, SortOrder::Desc, 2);
        let page = trace_list(&ctx, first.clone()).await.expect("page");
        let cursor = page.next_cursor.expect("next");
        first.sort = TraceSort::StartTime;
        first.cursor = Some(&cursor);
        let error = trace_list(&ctx, first).await.expect_err("sort");
        assert_eq!(
            error.to_string(),
            "the cursor belongs to another list or sort order"
        );
        for text in ["zz", hex::encode(b"{}").as_str()] {
            let mut request = listed(scope(None, false), TraceSort::StartTime, SortOrder::Desc, 2);
            request.cursor = Some(text);
            let error = trace_list(&ctx, request).await.expect_err(text);
            assert_eq!(error.to_string(), "invalid cursor");
        }
    }

    #[tokio::test]
    async fn a_filter_error_keeps_its_position_and_a_score_filter_matches() {
        let ctx = context();
        let mut request = listed(
            scope(None, false),
            TraceSort::StartTime,
            SortOrder::Desc,
            50,
        );
        request.filter = Some("nme = 'a'");
        match trace_list(&ctx, request).await {
            Err(RunError::Invalid(error)) => {
                assert_eq!(error.message, "unknown column `nme`");
                assert_eq!(error.position, Some(Position { line: 1, column: 1 }));
            }
            other => panic!("expected an invalid filter, got {other:?}"),
        }

        let mut request = listed(
            scope(None, false),
            TraceSort::StartTime,
            SortOrder::Desc,
            50,
        );
        request.filter = Some("score('helpfulness') >= 0.5");
        let page = trace_list(&ctx, request).await.expect("score");
        let fixture = fixture("traces.json");
        let expected: Vec<_> = fixture["data"]
            .as_array()
            .expect("data")
            .iter()
            .filter(|row| {
                row["scores"].as_array().is_some_and(|scores| {
                    scores.iter().any(|score| {
                        score["name"] == "helpfulness"
                            && score["value"]["value"]
                                .as_f64()
                                .is_some_and(|value| value >= 0.5)
                    })
                })
            })
            .map(|row| row["trace_id"].as_str().expect("id").to_owned())
            .collect();
        assert!(!expected.is_empty());
        assert_eq!(ids(&page), expected);
    }

    #[tokio::test]
    async fn search_uses_the_window_and_rejects_a_missing_one() {
        let ctx = context();
        let window = day();
        let mut request = listed(
            scope(Some(window), false),
            TraceSort::StartTime,
            SortOrder::Desc,
            50,
        );
        request.search = Some("order");
        let page = trace_list(&ctx, request).await.expect("search");
        let matching: std::collections::BTreeSet<String> = dataset_spans()
            .iter()
            .filter(|span| {
                text_has(span, "order") && (window.from()..window.to()).contains(&span.start_time)
            })
            .map(|span| span.trace_id.to_hex())
            .collect();
        let fixture = fixture("traces.json");
        let expected: Vec<_> = fixture["data"]
            .as_array()
            .expect("data")
            .iter()
            .filter(|row| {
                let id = row["trace_id"].as_str().expect("id");
                let start = nanos(row["start_time"].as_str().expect("start"));
                matching.contains(id) && (window.from()..window.to()).contains(&start)
            })
            .map(|row| row["trace_id"].as_str().expect("id").to_owned())
            .collect();
        assert!(!expected.is_empty());
        assert_eq!(ids(&page), expected);

        let mut request = listed(
            scope(None, false),
            TraceSort::StartTime,
            SortOrder::Desc,
            50,
        );
        request.search = Some("order");
        match trace_list(&ctx, request).await {
            Err(RunError::InvalidRequest(message)) => {
                assert_eq!(message, "search needs a time range");
            }
            other => panic!("expected a missing window, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_window_keeps_traces_starting_inside_it() {
        let from = nanos("2026-01-01T10:05:00Z");
        let to = nanos("2026-01-01T10:21:00Z");
        let window = TimeWindow::new(from, to).expect("window");
        let page = trace_list(
            &context(),
            listed(
                scope(Some(window), false),
                TraceSort::StartTime,
                SortOrder::Desc,
                50,
            ),
        )
        .await
        .expect("window");
        let fixture = fixture("traces.json");
        let expected: Vec<_> = fixture["data"]
            .as_array()
            .expect("data")
            .iter()
            .filter(|row| {
                let start = nanos(row["start_time"].as_str().expect("start"));
                (from..to).contains(&start)
            })
            .map(|row| row["trace_id"].as_str().expect("id").to_owned())
            .collect();
        assert_eq!(
            expected,
            [
                "a1000000000000000000000000000004",
                "a1000000000000000000000000000003",
                "a1000000000000000000000000000002",
            ]
        );
        assert_eq!(ids(&page), expected);
    }

    #[tokio::test]
    async fn limit_outside_the_page_is_rejected() {
        let ctx = context();
        for limit in [0, 1001] {
            let error = trace_list(
                &ctx,
                listed(
                    scope(None, false),
                    TraceSort::StartTime,
                    SortOrder::Desc,
                    limit,
                ),
            )
            .await
            .expect_err("limit");
            assert_eq!(
                error.to_string(),
                "limit must be between 1 and 1000",
                "{limit}"
            );
        }
    }

    #[tokio::test]
    async fn the_default_plan_sorts_with_a_fetch() {
        let ctx = context();
        let request = listed(
            scope(None, false),
            TraceSort::StartTime,
            SortOrder::Desc,
            50,
        );
        let frame = trace_frame(&ctx, &request).await.expect("frame");
        let physical = frame.create_physical_plan().await.expect("physical");
        let physical = format!("{}", displayable(physical.as_ref()).indent(true));
        assert!(
            physical.contains("SortExec") && physical.contains("fetch="),
            "{physical}"
        );
    }

    fn context_for(spans: &[Span]) -> QueryContext {
        QueryContext::new(
            ProjectQuery {
                spans: spans_table(spans),
                traces: traces_table(spans),
                scores: live_scores_table(&dataset_scores()),
                runtime: Arc::new(RuntimeEnv::default()),
                target_partitions: NonZeroUsize::new(1).expect("one partition"),
                optimizer_rules: Vec::new(),
            },
            None,
        )
        .expect("context")
    }

    fn fixture_file(path: &str) -> Value {
        let full = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path);
        let text = std::fs::read_to_string(&full)
            .unwrap_or_else(|error| panic!("{}: {error}", full.display()));
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{path}: {error}"))
    }

    fn span_request<'a>(
        scope: ListScope,
        sort: SpanSort,
        order: SortOrder,
        limit: usize,
    ) -> SpanListRequest<'a> {
        SpanListRequest {
            filter: None,
            search: None,
            scope,
            sort,
            order,
            limit,
            cursor: None,
            timeout: TIMEOUT,
        }
    }

    fn pairs(rows: &[Value], left: &str, right: &str) -> Vec<(String, String)> {
        rows.iter()
            .map(|row| {
                (
                    row[left].as_str().expect(left).to_owned(),
                    row[right].as_str().expect(right).to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn span_list_columns_follow_the_light_projection() {
        let mut expected = Vec::new();
        for name in SPANS_LIGHT_COLUMNS {
            if *name == spans::PROJECT_ID {
                continue;
            }
            expected.push(*name);
            if *name == spans::DURATION_NS {
                expected.push("duration_ms");
            }
        }
        assert_eq!(SPAN_LIST_COLUMNS, expected.as_slice());
    }

    #[tokio::test]
    async fn default_span_list_matches_the_fixture() {
        let page = span_list(
            &context(),
            span_request(scope(None, false), SpanSort::StartTime, SortOrder::Desc, 50),
        )
        .await
        .expect("list");
        let fixture = fixture("spans.json");
        let expected = fixture["data"].as_array().expect("data");
        let actual = rows(&page);
        assert_eq!(
            pairs(&actual, "trace_id", "span_id"),
            pairs(expected, "trace_id", "span_id")
        );
        assert_eq!(actual.len(), expected.len());
        for (row, span) in actual.iter().zip(expected) {
            let object = span.as_object().expect("span");
            for (key, value) in object {
                assert!(
                    values_match(&row[key], value),
                    "{key}: {} vs {value}",
                    row[key]
                );
            }
        }
        let schema = page.batches[0].schema();
        let names: Vec<_> = schema
            .fields()
            .iter()
            .map(|field| field.name().as_str())
            .collect();
        assert_eq!(names, SPAN_LIST_COLUMNS);
        assert!(page.next_cursor.is_none());
    }

    #[tokio::test]
    async fn span_pages_cover_every_sort_including_nulls() {
        let ctx = context();
        let sorts = [
            SpanSort::StartTime,
            SpanSort::Duration,
            SpanSort::CostTotal,
            SpanSort::TotalTokens,
        ];
        for sort in sorts {
            for order in [SortOrder::Asc, SortOrder::Desc] {
                let full = span_list(&ctx, span_request(scope(None, true), sort, order, 50))
                    .await
                    .expect("full");
                let expected = pairs(&rows(&full), "trace_id", "span_id");
                assert!(full.next_cursor.is_none());
                let mut walked = Vec::new();
                let mut cursor = None;
                loop {
                    let mut request = span_request(scope(None, true), sort, order, 2);
                    request.cursor = cursor.as_deref();
                    let page = span_list(&ctx, request).await.expect("page");
                    let page_ids = pairs(&rows(&page), "trace_id", "span_id");
                    assert!(page_ids.len() <= 2);
                    walked.extend(page_ids);
                    match page.next_cursor {
                        Some(next) => cursor = Some(next),
                        None => break,
                    }
                }
                assert_eq!(walked, expected, "{sort:?} {order:?}");
            }
        }
        let costs = span_list(
            &ctx,
            span_request(scope(None, true), SpanSort::CostTotal, SortOrder::Desc, 50),
        )
        .await
        .expect("costs");
        assert!(
            rows(&costs).iter().any(|row| row["cost_total"].is_null()),
            "cost_total has a null"
        );
        let tokens = span_list(
            &ctx,
            span_request(scope(None, true), SpanSort::TotalTokens, SortOrder::Asc, 50),
        )
        .await
        .expect("tokens");
        assert!(
            rows(&tokens)
                .iter()
                .any(|row| row["total_tokens"].is_null()),
            "total_tokens has a null"
        );
    }

    #[tokio::test]
    async fn trace_detail_matches_each_dataset_trace() {
        let ctx = context();
        let listed = fixture("traces-include-eval.json");
        for trace in listed["data"].as_array().expect("data") {
            let id = trace["trace_id"].as_str().expect("id");
            let detail = trace_detail(&ctx, id, TIMEOUT)
                .await
                .expect("detail")
                .unwrap_or_else(|| panic!("missing {id}"));
            let row = &json_rows(std::slice::from_ref(&detail.trace))[0];
            for (key, expected) in trace.as_object().expect("trace") {
                if key == "scores" {
                    continue;
                }
                assert!(
                    values_match(&row[key], expected),
                    "{id} {key}: {} vs {expected}",
                    row[key]
                );
            }
            let file = fixture_file(&format!("fixtures/api/responses/traces/{id}.json"));
            let mut expected_spans: Vec<(String, String)> = file["spans"]
                .as_array()
                .expect("spans")
                .iter()
                .map(|node| {
                    let span = &node["span"];
                    (
                        span["start_time"].as_str().expect("start").to_owned(),
                        span["span_id"].as_str().expect("span").to_owned(),
                    )
                })
                .collect();
            expected_spans.sort();
            let actual = pairs(&json_rows(&detail.spans), "start_time", "span_id");
            assert_eq!(actual, expected_spans, "{id}");
            assert!(!detail.truncated, "{id}");
        }
    }

    #[tokio::test]
    async fn trace_detail_accepts_uppercase_and_rejects_a_short_id() {
        let ctx = context();
        let id = "a1000000000000000000000000000001";
        let lower = trace_detail(&ctx, id, TIMEOUT)
            .await
            .expect("lower")
            .expect("row");
        let upper = trace_detail(&ctx, &id.to_ascii_uppercase(), TIMEOUT)
            .await
            .expect("upper")
            .expect("row");
        assert_eq!(
            pairs(&json_rows(&lower.spans), "start_time", "span_id"),
            pairs(&json_rows(&upper.spans), "start_time", "span_id")
        );
        match trace_detail(&ctx, "abc", TIMEOUT).await {
            Err(RunError::InvalidRequest(message)) => {
                assert_eq!(message, "trace_id must be 32 hex digits");
            }
            other => panic!("expected an invalid id, got {other:?}"),
        }
        let unknown = trace_detail(&ctx, "0123456789abcdef0123456789abcdef", TIMEOUT)
            .await
            .expect("unknown");
        assert!(unknown.is_none());
    }

    #[tokio::test]
    async fn trace_detail_truncates_at_the_span_cap() {
        let base = dataset_spans().into_iter().next().expect("span");
        let trace_id = TraceId::from_str("a10000000000000000000000000000aa").expect("trace");
        let mut spans = Vec::new();
        for index in 0..5 {
            let mut span = base.clone();
            span.trace_id = trace_id;
            span.span_id = SpanId::from_str(&format!("b1000000000000a{index}")).expect("span");
            span.parent_span_id = None;
            span.start_time = base.start_time + i64::from(index);
            span.name = format!("span-{index}");
            spans.push(span);
        }
        let detail = trace_detail_limited(
            &context_for(&spans),
            "a10000000000000000000000000000aa",
            TIMEOUT,
            3,
        )
        .await
        .expect("detail")
        .expect("trace");
        assert!(detail.truncated);
        assert_eq!(
            pairs(&json_rows(&detail.spans), "span_id", "name"),
            [
                ("b1000000000000a0".to_owned(), "span-0".to_owned()),
                ("b1000000000000a1".to_owned(), "span-1".to_owned()),
                ("b1000000000000a2".to_owned(), "span-2".to_owned()),
            ]
        );
    }

    #[tokio::test]
    async fn span_detail_matches_the_fixture_and_keeps_the_latest_duplicate() {
        let ctx = context();
        let batch = span_detail(
            &ctx,
            "a1000000000000000000000000000001",
            "b100000000000012",
            TIMEOUT,
        )
        .await
        .expect("detail")
        .expect("span");
        let row = &json_rows(std::slice::from_ref(&batch))[0];
        let fixture = fixture_file(
            "fixtures/api/responses/spans/a1000000000000000000000000000001/b100000000000012.json",
        );
        for field in [
            "name",
            "span_kind",
            "start_time",
            "end_time",
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_write_tokens",
            "reasoning_tokens",
            "total_tokens",
            "cost_input",
            "cost_output",
            "cost_total",
            "status_message",
        ] {
            let expected = fixture.get(field).unwrap_or(&Value::Null);
            assert!(
                values_match(&row[field], expected),
                "{field}: {} vs {expected}",
                row[field]
            );
        }
        let stored: Value =
            serde_json::from_str(row["input"].as_str().expect("input text")).expect("input json");
        assert_eq!(stored, fixture["input"]["value"]);
        assert_eq!(row["input_format"], fixture["input"]["format"]);

        let base = dataset_spans().into_iter().next().expect("span");
        let trace_id = TraceId::from_str("a2000000000000000000000000000001").expect("trace");
        let span_id = SpanId::from_str("b200000000000001").expect("span");
        let mut older = base.clone();
        let mut newer = base.clone();
        older.trace_id = trace_id;
        newer.trace_id = trace_id;
        older.span_id = span_id;
        newer.span_id = span_id;
        older.parent_span_id = None;
        newer.parent_span_id = None;
        older.ingest_time = 1_000;
        newer.ingest_time = 2_000;
        older.name = "older".to_owned();
        newer.name = "newer".to_owned();
        let winner = span_detail(
            &context_for(&[older, newer]),
            "a2000000000000000000000000000001",
            "b200000000000001",
            TIMEOUT,
        )
        .await
        .expect("duplicate")
        .expect("row");
        let winner = &json_rows(std::slice::from_ref(&winner))[0];
        assert_eq!(winner["name"], "newer");

        match span_detail(&ctx, "a1000000000000000000000000000001", "abc", TIMEOUT).await {
            Err(RunError::InvalidRequest(message)) => {
                assert_eq!(message, "span_id must be 16 hex digits");
            }
            other => panic!("expected an invalid span id, got {other:?}"),
        }
        let missing = span_detail(
            &ctx,
            "a1000000000000000000000000000001",
            "b1000000000000ff",
            TIMEOUT,
        )
        .await
        .expect("missing");
        assert!(missing.is_none());
    }
}
