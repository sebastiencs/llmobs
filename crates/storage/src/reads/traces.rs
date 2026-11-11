//! Trace list.
//!
//! Without score functions, the default list is `Limit(n+1) ← Sort(start_time DESC NULLS LAST, trace_id DESC) ← Filter ← traces view`.

use std::time::Duration;

use datafusion::logical_expr::Expr;
use datafusion::prelude::DataFrame;
use llmobs_core::schema::traces;

use super::cursor::{after, assemble, decode, order_by};
use super::{ListScope, Page, SortOrder, present};
use crate::filter::{FilterView, exclusions, search, time_window};
use crate::query_error::{RunError, classify};
use crate::views::QueryContext;

const LIST: &str = "traces";
const DURATION_NS: &str = "duration_ns";

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
    if !(1..=super::MAX_PAGE).contains(&request.limit) {
        return Err(RunError::InvalidRequest(
            "limit must be between 1 and 1000".to_owned(),
        ));
    }
    match tokio::time::timeout(request.timeout, execute(ctx, &request)).await {
        Err(_) => Err(RunError::Timeout),
        Ok(result) => result,
    }
}

async fn execute(ctx: &QueryContext, request: &TraceListRequest<'_>) -> Result<Page, RunError> {
    let frame = trace_frame(ctx, request).await?;
    let batches = frame.collect().await.map_err(classify)?;
    assemble(
        batches,
        request.limit,
        LIST,
        request.sort.column(),
        request.order,
        &[traces::TRACE_ID],
    )
}

/// The planned frame, before it is collected. Tests read the plan from this.
async fn trace_frame(
    ctx: &QueryContext,
    request: &TraceListRequest<'_>,
) -> Result<DataFrame, RunError> {
    let filter = match present(request.filter) {
        Some(text) => Some(ctx.parse_filter(FilterView::Traces, text)?),
        None => None,
    };
    let references_environment = filter
        .as_ref()
        .is_some_and(|filter| filter.references_environment);
    let cursor = match request.cursor {
        Some(text) => {
            let cursor = decode(text, LIST, request.sort.column(), request.order)?;
            if cursor.tie_len() != 1 {
                return Err(RunError::InvalidRequest("invalid cursor".to_owned()));
            }
            Some(cursor)
        }
        None => None,
    };

    let mut frame = ctx.session().table(LIST).await.map_err(classify)?;
    let mut conjuncts = Vec::new();
    if let Some(window) = request.scope.window {
        conjuncts.push(time_window(window));
    }
    if let Some(expr) = exclusions(
        ctx,
        FilterView::Traces,
        request.scope.include_eval,
        request.scope.include_experiments,
        references_environment,
    )? {
        conjuncts.push(expr);
    }
    if let Some(text) = present(request.search)
        && let Some(expr) = search(FilterView::Traces, text, request.scope.window, ctx).await?
    {
        conjuncts.push(expr);
    }
    if let Some(cursor) = &cursor {
        conjuncts.push(after(cursor, request.sort.column(), &[traces::TRACE_ID]));
    }
    if let Some(predicate) = conjuncts.into_iter().reduce(Expr::and) {
        frame = frame.filter(predicate).map_err(classify)?;
    }
    if let Some(filter) = filter {
        frame = filter.apply(frame, ctx).await.map_err(classify)?;
    }
    frame
        .sort(order_by(
            request.sort.column(),
            &[traces::TRACE_ID],
            request.order,
        ))
        .map_err(classify)?
        .limit(0, Some(request.limit + 1))
        .map_err(classify)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use arrow::json::{ArrayWriter, WriterBuilder};
    use datafusion::physical_plan::displayable;
    use llmobs_core::span::Span;
    use llmobs_core::time_serde::parse_rfc3339;
    use serde_json::Value;

    use super::{TraceListRequest, TraceSort, trace_frame, trace_list};
    use crate::query_error::{Position, RunError};
    use crate::reads::{ListScope, Page, SortOrder};
    use crate::test_support::{dataset_spans, example_query};
    use crate::views::{QueryContext, TimeWindow};

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
            other => panic!("unknown fixture {other}"),
        };
        serde_json::from_str(text).expect(name)
    }

    fn rows(page: &Page) -> Vec<Value> {
        if page.batches.iter().all(|batch| batch.num_rows() == 0) {
            return Vec::new();
        }
        let mut buffer = Vec::new();
        let mut writer: ArrayWriter<&mut Vec<u8>> = WriterBuilder::new()
            .with_explicit_nulls(true)
            .build(&mut buffer);
        for batch in &page.batches {
            writer.write(batch).expect("encode");
        }
        writer.finish().expect("finish");
        drop(writer);
        serde_json::from_slice(&buffer).expect("rows")
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
}
