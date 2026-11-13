//! Session list and session detail.
//!
//! A session's aggregates cover the traces inside the caller's window. Traces
//! are scoped first (the `traces` view, the window on `start_time`, and the
//! default exclusions). `references_environment` stays false: the sessions
//! view has no environment column. Spec 05 says the same.

use std::time::Duration;

use arrow::compute::concat_batches;
use arrow::record_batch::RecordBatch;
use datafusion::error::DataFusionError;
use datafusion::logical_expr::{col, lit};
use datafusion::prelude::DataFrame;
use llmobs_core::schema::{spans, traces};

use super::cursor::{after, assemble, decode, order_by};
use super::{ListScope, Page, SortOrder, present, scoped_traces};
use crate::filter::FilterView;
use crate::query_error::{RunError, classify};
use crate::views::{QueryContext, sessions_from_traces};

const LIST: &str = "sessions";
const TRACE_COUNT: &str = "trace_count";
const TIES: &[&str] = &[traces::SESSION_ID];

/// Most turns `session_detail` returns.
pub const MAX_SESSION_TURNS: usize = 1000;

/// Column a session list sorts by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionSort {
    /// `start_time`.
    StartTime,
    /// `end_time`.
    EndTime,
    /// `trace_count`.
    TraceCount,
    /// `cost_total`.
    CostTotal,
}

impl SessionSort {
    fn column(self) -> &'static str {
        match self {
            Self::StartTime => traces::START_TIME,
            Self::EndTime => traces::END_TIME,
            Self::TraceCount => TRACE_COUNT,
            Self::CostTotal => traces::COST_TOTAL,
        }
    }
}

/// One page of the sessions view.
#[derive(Debug, Clone)]
pub struct SessionListRequest<'a> {
    /// Filter text over the sessions view. Empty or whitespace means no filter.
    pub filter: Option<&'a str>,
    /// Window and default exclusions, applied to the traces before aggregation.
    pub scope: ListScope,
    /// Sort column.
    pub sort: SessionSort,
    /// Sort direction. Nulls are last.
    pub order: SortOrder,
    /// Rows to return, from 1 to [`super::MAX_PAGE`].
    pub limit: usize,
    /// Cursor from the previous page.
    pub cursor: Option<&'a str>,
    /// Time allowed for planning and running.
    pub timeout: Duration,
}

/// One session, its turns, and those turns' root payloads.
#[derive(Debug)]
pub struct SessionDetailData {
    /// The sessions-view row.
    pub session: RecordBatch,
    /// Scoped traces, `start_time ASC, trace_id ASC`, at most [`MAX_SESSION_TURNS`].
    pub turns: Vec<RecordBatch>,
    /// Root span `trace_id`, `input`, `input_format`, `output`, `output_format`.
    ///
    /// Turns without a root have no row.
    pub roots: Vec<RecordBatch>,
    /// True when more than [`MAX_SESSION_TURNS`] traces were stored.
    pub truncated: bool,
}

/// One page of sessions.
///
/// Traces are scoped first: the `traces` view, [`ListScope::window`] on
/// `start_time`, and the default exclusions. The environment exclusion always
/// applies, because a session filter cannot name `environment`. Aggregates
/// then cover the traces inside that window.
///
/// # Errors
///
/// [`RunError::InvalidRequest`] for a bad limit or cursor. [`RunError::Invalid`]
/// for filter text. [`RunError::Timeout`] when the query exceeds `timeout`.
pub async fn session_list(
    ctx: &QueryContext,
    request: SessionListRequest<'_>,
) -> Result<Page, RunError> {
    if !(1..=super::MAX_PAGE).contains(&request.limit) {
        return Err(RunError::InvalidRequest(
            "limit must be between 1 and 1000".to_owned(),
        ));
    }
    match tokio::time::timeout(request.timeout, list(ctx, &request)).await {
        Err(_) => Err(RunError::Timeout),
        Ok(result) => result,
    }
}

/// The session, its turns, and their root payloads. No time window.
///
/// An unknown `session_id` is `Ok(None)`.
///
/// # Errors
///
/// [`RunError::Timeout`] when the query exceeds `timeout`.
pub async fn session_detail(
    ctx: &QueryContext,
    session_id: &str,
    include_eval: bool,
    include_experiments: bool,
    timeout: Duration,
) -> Result<Option<SessionDetailData>, RunError> {
    match tokio::time::timeout(
        timeout,
        load(ctx, session_id, include_eval, include_experiments),
    )
    .await
    {
        Err(_) => Err(RunError::Timeout),
        Ok(result) => result,
    }
}

async fn list(ctx: &QueryContext, request: &SessionListRequest<'_>) -> Result<Page, RunError> {
    let filter = match present(request.filter) {
        Some(text) => Some(ctx.parse_filter(FilterView::Sessions, text)?),
        None => None,
    };
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
    let scoped = scoped_traces(ctx, request.scope, false).await?;
    let plan = sessions_from_traces(scoped.into_unoptimized_plan()).map_err(RunError::Internal)?;
    let mut frame = DataFrame::new(ctx.session().state(), plan);
    if let Some(filter) = filter {
        frame = filter.apply(frame, ctx).await.map_err(classify)?;
    }
    if let Some(cursor) = &cursor {
        frame = frame
            .filter(after(cursor, request.sort.column(), TIES))
            .map_err(classify)?;
    }
    let frame = frame
        .sort(order_by(request.sort.column(), TIES, request.order))
        .map_err(classify)?
        .limit(0, Some(request.limit + 1))
        .map_err(classify)?;
    let batches = frame.collect().await.map_err(|error| ctx.fail(error))?;
    assemble(
        batches,
        request.limit,
        LIST,
        request.sort.column(),
        request.order,
        TIES,
    )
}

async fn load(
    ctx: &QueryContext,
    session_id: &str,
    include_eval: bool,
    include_experiments: bool,
) -> Result<Option<SessionDetailData>, RunError> {
    let scope = ListScope {
        window: None,
        include_eval,
        include_experiments,
    };
    let scoped = scoped_traces(ctx, scope, false)
        .await?
        .filter(col(traces::SESSION_ID).eq(lit(session_id)))
        .map_err(classify)?;
    let plan = scoped.logical_plan().clone();
    let state = ctx.session().state();
    let existing = DataFrame::new(state.clone(), plan.clone())
        .collect()
        .await
        .map_err(|error| ctx.fail(error))?;
    if row_count(&existing) == 0 {
        return Ok(None);
    }
    let session_plan = sessions_from_traces(plan.clone()).map_err(RunError::Internal)?;
    let session = DataFrame::new(state.clone(), session_plan)
        .collect()
        .await
        .map_err(|error| ctx.fail(error))?;
    let Some(session) = single_row(&session)? else {
        return Ok(None);
    };
    let turns = DataFrame::new(state, plan)
        .sort(order_by(
            traces::START_TIME,
            &[traces::TRACE_ID],
            SortOrder::Asc,
        ))
        .map_err(classify)?
        .limit(0, Some(MAX_SESSION_TURNS + 1))
        .map_err(classify)?
        .collect()
        .await
        .map_err(|error| ctx.fail(error))?;
    let (turns, truncated) = cap_rows(turns, MAX_SESSION_TURNS);
    let roots = root_payloads(ctx, &turns).await?;
    Ok(Some(SessionDetailData {
        session,
        turns,
        roots,
        truncated,
    }))
}

async fn root_payloads(
    ctx: &QueryContext,
    turns: &[RecordBatch],
) -> Result<Vec<RecordBatch>, RunError> {
    let Some(schema) = turns
        .iter()
        .find(|batch| batch.num_rows() > 0)
        .map(RecordBatch::schema)
    else {
        return Ok(Vec::new());
    };
    let turns = concat_batches(&schema, turns).map_err(arrow_failure)?;
    let spans = ctx
        .session()
        .table("spans")
        .await
        .map_err(classify)?
        .select(vec![
            col(spans::TRACE_ID).alias("__root_trace"),
            col(spans::SPAN_ID).alias("__root_span"),
            col(spans::INPUT),
            col(spans::INPUT_FORMAT),
            col(spans::OUTPUT),
            col(spans::OUTPUT_FORMAT),
        ])
        .map_err(classify)?;
    let joined = ctx
        .session()
        .read_batch(turns)
        .map_err(RunError::Internal)?
        .join(
            spans,
            datafusion::logical_expr::JoinType::Inner,
            &[traces::TRACE_ID, traces::ROOT_SPAN_ID],
            &["__root_trace", "__root_span"],
            None,
        )
        .map_err(classify)?
        .select(vec![
            col(traces::TRACE_ID),
            col(spans::INPUT),
            col(spans::INPUT_FORMAT),
            col(spans::OUTPUT),
            col(spans::OUTPUT_FORMAT),
        ])
        .map_err(classify)?;
    joined.collect().await.map_err(|error| ctx.fail(error))
}

fn row_count(batches: &[RecordBatch]) -> usize {
    batches.iter().map(RecordBatch::num_rows).sum()
}

fn single_row(batches: &[RecordBatch]) -> Result<Option<RecordBatch>, RunError> {
    let Some(schema) = batches
        .iter()
        .find(|batch| batch.num_rows() > 0)
        .map(RecordBatch::schema)
    else {
        return Ok(None);
    };
    let batch = concat_batches(&schema, batches).map_err(arrow_failure)?;
    if batch.num_rows() == 0 {
        return Ok(None);
    }
    if batch.num_rows() == 1 {
        return Ok(Some(batch));
    }
    Ok(Some(batch.slice(0, 1)))
}

fn cap_rows(batches: Vec<RecordBatch>, limit: usize) -> (Vec<RecordBatch>, bool) {
    let total = row_count(&batches);
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

fn arrow_failure(error: arrow::error::ArrowError) -> RunError {
    RunError::Internal(DataFusionError::ArrowError(Box::new(error), None))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use arrow::json::{ArrayWriter, WriterBuilder};
    use arrow::record_batch::RecordBatch;
    use llmobs_core::time_serde::parse_rfc3339;
    use serde_json::Value;

    use super::{SessionListRequest, SessionSort, session_detail, session_list};
    use crate::reads::{ListScope, Page, SortOrder};
    use crate::test_support::example_query;
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

    fn request<'a>(
        scope: ListScope,
        sort: SessionSort,
        order: SortOrder,
    ) -> SessionListRequest<'a> {
        SessionListRequest {
            filter: None,
            scope,
            sort,
            order,
            limit: 50,
            cursor: None,
            timeout: TIMEOUT,
        }
    }

    fn fixture(name: &str) -> Value {
        let text = match name {
            "sessions.json" => include_str!("../../../../fixtures/api/responses/sessions.json"),
            "sessions-include-eval.json" => {
                include_str!("../../../../fixtures/api/responses/sessions-include-eval.json")
            }
            other => panic!("unknown fixture {other}"),
        };
        serde_json::from_str(text).expect(name)
    }

    fn fixture_file(path: &str) -> Value {
        let full = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(path);
        let text = std::fs::read_to_string(&full)
            .unwrap_or_else(|error| panic!("{}: {error}", full.display()));
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{path}: {error}"))
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

    fn assert_rows(actual: &[Value], expected: &[Value]) {
        assert_eq!(actual.len(), expected.len());
        for (row, fixture) in actual.iter().zip(expected) {
            for (key, value) in fixture.as_object().expect("object") {
                if key == "scores" {
                    continue;
                }
                assert!(
                    values_match(&row[key], value),
                    "{key}: {} vs {value}",
                    row[key]
                );
            }
        }
    }

    fn ids(rows: &[Value]) -> Vec<String> {
        rows.iter()
            .map(|row| row["session_id"].as_str().expect("id").to_owned())
            .collect()
    }

    #[tokio::test]
    async fn session_lists_match_the_fixtures() {
        let ctx = context();
        let page = session_list(
            &ctx,
            request(scope(None, false), SessionSort::StartTime, SortOrder::Desc),
        )
        .await
        .expect("list");
        let listed = fixture("sessions.json");
        assert_rows(&rows(&page), listed["data"].as_array().expect("data"));
        assert!(page.next_cursor.is_none());

        let page = session_list(
            &ctx,
            request(scope(None, true), SessionSort::StartTime, SortOrder::Desc),
        )
        .await
        .expect("eval");
        let listed = fixture("sessions-include-eval.json");
        assert_rows(&rows(&page), listed["data"].as_array().expect("data"));
    }

    #[tokio::test]
    async fn session_detail_matches_the_fixture() {
        let ctx = context();
        for (include_eval, file) in [
            (false, "fixtures/api/responses/sessions/sess-001.json"),
            (
                true,
                "fixtures/api/responses/sessions/sess-001-include-eval.json",
            ),
        ] {
            let detail = session_detail(&ctx, "sess-001", include_eval, false, TIMEOUT)
                .await
                .expect("detail")
                .expect("session");
            let fixture = fixture_file(file);
            assert_rows(
                &json_rows(std::slice::from_ref(&detail.session)),
                std::slice::from_ref(&fixture["session"]),
            );
            let turns = json_rows(&detail.turns);
            let expected = fixture["turns"].as_array().expect("turns");
            let actual_ids: Vec<_> = turns
                .iter()
                .map(|row| row["trace_id"].as_str().expect("id").to_owned())
                .collect();
            let expected_ids: Vec<_> = expected
                .iter()
                .map(|turn| turn["trace"]["trace_id"].as_str().expect("id").to_owned())
                .collect();
            assert_eq!(actual_ids, expected_ids, "{file}");
            let roots = json_rows(&detail.roots);
            for turn in expected {
                if turn.get("input").is_none() {
                    continue;
                }
                let id = turn["trace"]["trace_id"].as_str().expect("id");
                let root = roots
                    .iter()
                    .find(|row| row["trace_id"] == id)
                    .unwrap_or_else(|| panic!("missing root {id}"));
                assert!(!root["input"].is_null(), "{id}");
                if turn["input"]["format"] == "text" {
                    assert_eq!(root["input"], turn["input"]["value"], "{id}");
                }
            }
            assert!(!detail.truncated, "{file}");
        }
        let missing = session_detail(&ctx, "missing", false, false, TIMEOUT)
            .await
            .expect("missing");
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn session_pages_cover_every_sort() {
        let ctx = context();
        let sorts = [
            SessionSort::StartTime,
            SessionSort::EndTime,
            SessionSort::TraceCount,
            SessionSort::CostTotal,
        ];
        for sort in sorts {
            for order in [SortOrder::Asc, SortOrder::Desc] {
                let mut full = request(scope(None, true), sort, order);
                full.limit = 50;
                let full = session_list(&ctx, full).await.expect("full");
                let expected = ids(&rows(&full));
                assert!(full.next_cursor.is_none());
                let mut walked = Vec::new();
                let mut cursor = None;
                loop {
                    let mut page_request = request(scope(None, true), sort, order);
                    page_request.limit = 1;
                    page_request.cursor = cursor.as_deref();
                    let page = session_list(&ctx, page_request).await.expect("page");
                    let page_ids = ids(&rows(&page));
                    assert!(page_ids.len() <= 1);
                    walked.extend(page_ids);
                    match page.next_cursor {
                        Some(next) => cursor = Some(next),
                        None => break,
                    }
                }
                assert_eq!(walked, expected, "{sort:?} {order:?}");
            }
        }
        let costs = session_list(
            &ctx,
            request(scope(None, true), SessionSort::CostTotal, SortOrder::Desc),
        )
        .await
        .expect("costs");
        assert!(
            rows(&costs).iter().any(|row| row["cost_total"].is_null()),
            "cost_total has a null"
        );
    }

    #[tokio::test]
    async fn session_filters_keep_the_matching_sessions() {
        let ctx = context();
        for text in ["trace_count >= 2", "has_score('resolved')"] {
            let mut request = request(scope(None, false), SessionSort::StartTime, SortOrder::Desc);
            request.filter = Some(text);
            let page = session_list(&ctx, request).await.expect(text);
            assert_eq!(ids(&rows(&page)), ["sess-001"], "{text}");
        }
    }

    #[tokio::test]
    async fn a_window_counts_only_the_traces_inside_it() {
        let from = parse_rfc3339("2026-01-01T10:05:00Z").expect("from");
        let to = parse_rfc3339("2026-01-01T10:06:00Z").expect("to");
        let window = TimeWindow::new(from, to).expect("window");
        let page = session_list(
            &context(),
            request(
                scope(Some(window), false),
                SessionSort::StartTime,
                SortOrder::Desc,
            ),
        )
        .await
        .expect("window");
        let rows = rows(&page);
        assert_eq!(ids(&rows), ["sess-001"]);
        assert_eq!(rows[0]["trace_count"], 1);
    }
}
