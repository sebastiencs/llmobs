//! User list and user detail.
//!
//! The caller's filter runs on `traces` before the grouping, so it can name
//! trace columns such as `environment` (spec 09 `UserListParams`). Exclusions
//! see that reference and skip the environment clause when the filter already
//! names it.

use std::time::Duration;

use std::sync::Arc;

use arrow::compute::concat_batches;
use arrow::record_batch::RecordBatch;
use datafusion::error::DataFusionError;
use datafusion::execution::SessionState;
use datafusion::functions::expr_fn::coalesce;
use datafusion::functions_aggregate::expr_fn::{count, count_distinct, max, min, sum};
use datafusion::logical_expr::{LogicalPlan, col, in_subquery, lit};
use datafusion::prelude::DataFrame;
use llmobs_core::schema::traces;

use super::cursor::{after, assemble, decode, order_by};
use super::{ListScope, Page, SortOrder, present, scoped_traces};
use crate::filter::FilterView;
use crate::query_error::{RunError, classify};
use crate::views::{QueryContext, sessions_from_traces};

const LIST: &str = "users";
const FIRST_SEEN: &str = "first_seen";
const LAST_SEEN: &str = "last_seen";
const TRACE_COUNT: &str = "trace_count";
const SESSION_COUNT: &str = "session_count";
const TIES: &[&str] = &[traces::USER_ID];

/// Columns of a user list, in order.
pub const USER_COLUMNS: &[&str] = &[
    traces::USER_ID,
    FIRST_SEEN,
    LAST_SEEN,
    TRACE_COUNT,
    SESSION_COUNT,
    traces::ERROR_COUNT,
    traces::TOTAL_TOKENS,
    traces::COST_TOTAL,
];

/// Most sessions `user_detail` returns.
pub const MAX_USER_SESSIONS: usize = 100;

/// Column a user list sorts by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserSort {
    /// `last_seen`.
    LastSeen,
    /// `trace_count`.
    TraceCount,
    /// `cost_total`.
    CostTotal,
}

impl UserSort {
    fn column(self) -> &'static str {
        match self {
            Self::LastSeen => LAST_SEEN,
            Self::TraceCount => TRACE_COUNT,
            Self::CostTotal => traces::COST_TOTAL,
        }
    }
}

/// One page of users.
#[derive(Debug, Clone)]
pub struct UserListRequest<'a> {
    /// Filter text over `traces`, applied before grouping. Empty means no filter.
    pub filter: Option<&'a str>,
    /// Window and default exclusions, applied to the traces before grouping.
    pub scope: ListScope,
    /// Sort column.
    pub sort: UserSort,
    /// Sort direction. Nulls are last.
    pub order: SortOrder,
    /// Rows to return, from 1 to [`super::MAX_PAGE`].
    pub limit: usize,
    /// Cursor from the previous page.
    pub cursor: Option<&'a str>,
    /// Time allowed for planning and running.
    pub timeout: Duration,
}

/// One user and the whole sessions that contain the user's traces.
#[derive(Debug)]
pub struct UserDetailData {
    /// The aggregate row. Columns are [`USER_COLUMNS`].
    pub user: RecordBatch,
    /// Sessions of the user's traces, `start_time DESC, session_id DESC`.
    ///
    /// Each session covers every excluded trace in it, not only this user's.
    pub sessions: Vec<RecordBatch>,
}

/// One page of users.
///
/// Traces are scoped by the window and the default exclusions, then by the
/// filter, and only then grouped. `user_id IS NOT NULL` drops anonymous traces.
///
/// # Errors
///
/// [`RunError::InvalidRequest`] for a bad limit or cursor. [`RunError::Invalid`]
/// for filter text. [`RunError::Timeout`] when the query exceeds `timeout`.
pub async fn user_list(ctx: &QueryContext, request: UserListRequest<'_>) -> Result<Page, RunError> {
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

/// The user and the sessions of their traces. No window and no filter.
///
/// An unknown `user_id` is `Ok(None)`.
///
/// # Errors
///
/// [`RunError::Timeout`] when the query exceeds `timeout`.
pub async fn user_detail(
    ctx: &QueryContext,
    user_id: &str,
    include_eval: bool,
    include_experiments: bool,
    timeout: Duration,
) -> Result<Option<UserDetailData>, RunError> {
    match tokio::time::timeout(
        timeout,
        load(ctx, user_id, include_eval, include_experiments),
    )
    .await
    {
        Err(_) => Err(RunError::Timeout),
        Ok(result) => result,
    }
}

async fn list(ctx: &QueryContext, request: &UserListRequest<'_>) -> Result<Page, RunError> {
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
    let mut frame = user_frame(ctx, request).await?;
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

/// The grouped frame, before the cursor, sort, and limit. Tests read its plan.
async fn user_frame(
    ctx: &QueryContext,
    request: &UserListRequest<'_>,
) -> Result<DataFrame, RunError> {
    let filter = match present(request.filter) {
        Some(text) => Some(ctx.parse_filter(FilterView::Traces, text)?),
        None => None,
    };
    let references_environment = filter
        .as_ref()
        .is_some_and(|filter| filter.references_environment);
    let mut frame = scoped_traces(ctx, request.scope, references_environment).await?;
    if let Some(filter) = filter {
        frame = filter.apply(frame, ctx).await.map_err(classify)?;
    }
    frame = frame
        .filter(col(traces::USER_ID).is_not_null())
        .map_err(classify)?;
    aggregate_users(frame)
}

fn aggregate_users(frame: DataFrame) -> Result<DataFrame, RunError> {
    let grouped = frame
        .aggregate(
            vec![col(traces::USER_ID)],
            vec![
                min(col(traces::START_TIME)).alias(FIRST_SEEN),
                max(col(traces::END_TIME)).alias(LAST_SEEN),
                count(lit(1_i64)).alias(TRACE_COUNT),
                count_distinct(col(traces::SESSION_ID)).alias(SESSION_COUNT),
                sum(col(traces::ERROR_COUNT)).alias(traces::ERROR_COUNT),
                sum(col(traces::TOTAL_TOKENS)).alias(traces::TOTAL_TOKENS),
                sum(col(traces::COST_TOTAL)).alias(traces::COST_TOTAL),
            ],
        )
        .map_err(classify)?;
    grouped
        .select(vec![
            col(traces::USER_ID),
            col(FIRST_SEEN),
            col(LAST_SEEN),
            col(TRACE_COUNT),
            col(SESSION_COUNT),
            coalesce(vec![col(traces::ERROR_COUNT), lit(0_i64)]).alias(traces::ERROR_COUNT),
            col(traces::TOTAL_TOKENS),
            col(traces::COST_TOTAL),
        ])
        .map_err(classify)
}

async fn load(
    ctx: &QueryContext,
    user_id: &str,
    include_eval: bool,
    include_experiments: bool,
) -> Result<Option<UserDetailData>, RunError> {
    let scope = ListScope {
        window: None,
        include_eval,
        include_experiments,
    };
    let excluded = scoped_traces(ctx, scope, false).await?;
    let excluded_plan = excluded.logical_plan().clone();
    let state = ctx.session().state();
    let user_plan = DataFrame::new(state.clone(), excluded_plan.clone())
        .filter(col(traces::USER_ID).eq(lit(user_id)))
        .map_err(classify)?
        .logical_plan()
        .clone();
    let user = aggregate_users(DataFrame::new(state.clone(), user_plan.clone()))?
        .collect()
        .await
        .map_err(|error| ctx.fail(error))?;
    let Some(user) = single_row(&user)? else {
        return Ok(None);
    };
    let sessions = user_sessions_plan(&state, excluded_plan, user_plan)?;
    let sessions = DataFrame::new(state, sessions)
        .sort(order_by(
            traces::START_TIME,
            &[traces::SESSION_ID],
            SortOrder::Desc,
        ))
        .map_err(classify)?
        .limit(0, Some(MAX_USER_SESSIONS))
        .map_err(classify)?
        .collect()
        .await
        .map_err(|error| ctx.fail(error))?;
    let sessions: Vec<RecordBatch> = sessions
        .into_iter()
        .filter(|batch| batch.num_rows() > 0)
        .collect();
    Ok(Some(UserDetailData { user, sessions }))
}

/// Sessions of the user whose traces `user_plan` selects, over `excluded_plan`
/// (all scoped traces), through an `IN (subquery)` that plans as a semi join.
fn user_sessions_plan(
    state: &SessionState,
    excluded_plan: LogicalPlan,
    user_plan: LogicalPlan,
) -> Result<LogicalPlan, RunError> {
    let session_ids = DataFrame::new(state.clone(), user_plan)
        .filter(col(traces::SESSION_ID).is_not_null())
        .map_err(classify)?
        .select(vec![col(traces::SESSION_ID)])
        .map_err(classify)?
        .distinct()
        .map_err(classify)?
        .into_unoptimized_plan();
    let session_traces = DataFrame::new(state.clone(), excluded_plan)
        .filter(in_subquery(col(traces::SESSION_ID), Arc::new(session_ids)))
        .map_err(classify)?;
    sessions_from_traces(session_traces.into_unoptimized_plan()).map_err(RunError::Internal)
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use arrow::json::{ArrayWriter, WriterBuilder};
    use arrow::record_batch::RecordBatch;
    use datafusion::logical_expr::{col, lit};
    use datafusion::prelude::DataFrame;
    use llmobs_core::names::environment;
    use llmobs_core::schema::traces;
    use llmobs_core::span::Span;
    use serde_json::Value;

    use super::{
        UserListRequest, UserSort, user_detail, user_frame, user_list, user_sessions_plan,
    };
    use crate::reads::{ListScope, Page, SortOrder, scoped_traces};
    use crate::test_support::{example_query, span_row, spans_table, traces_table};
    use crate::views::QueryContext;

    const TIMEOUT: Duration = Duration::from_secs(30);

    fn context() -> QueryContext {
        QueryContext::new(example_query(), None).expect("context")
    }

    fn context_of(spans: &[Span]) -> QueryContext {
        let mut query = example_query();
        query.spans = spans_table(spans);
        query.traces = traces_table(spans);
        QueryContext::new(query, None).expect("context")
    }

    fn scope(include_eval: bool) -> ListScope {
        ListScope {
            window: None,
            include_eval,
            include_experiments: false,
        }
    }

    fn request<'a>(include_eval: bool, sort: UserSort, order: SortOrder) -> UserListRequest<'a> {
        UserListRequest {
            filter: None,
            scope: scope(include_eval),
            sort,
            order,
            limit: 50,
            cursor: None,
            timeout: TIMEOUT,
        }
    }

    fn fixture(name: &str) -> Value {
        let text = match name {
            "users.json" => include_str!("../../../../fixtures/api/responses/users.json"),
            "users-include-eval.json" => {
                include_str!("../../../../fixtures/api/responses/users-include-eval.json")
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

    fn ids(rows: &[Value], column: &str) -> Vec<String> {
        rows.iter()
            .map(|row| row[column].as_str().expect(column).to_owned())
            .collect()
    }

    #[tokio::test]
    async fn user_lists_match_the_fixtures() {
        let ctx = context();
        let page = user_list(&ctx, request(false, UserSort::LastSeen, SortOrder::Desc))
            .await
            .expect("list");
        let listed = fixture("users.json");
        assert_rows(&rows(&page), listed["data"].as_array().expect("data"));
        assert!(page.next_cursor.is_none());

        let page = user_list(&ctx, request(true, UserSort::LastSeen, SortOrder::Desc))
            .await
            .expect("eval");
        let listed = fixture("users-include-eval.json");
        assert_rows(&rows(&page), listed["data"].as_array().expect("data"));
    }

    #[tokio::test]
    async fn user_detail_matches_the_fixtures() {
        let ctx = context();
        for (user_id, include_eval, file) in [
            (
                "user-42",
                false,
                "fixtures/api/responses/users/user-42.json",
            ),
            (
                "user-42",
                true,
                "fixtures/api/responses/users/user-42-include-eval.json",
            ),
            ("user-7", false, "fixtures/api/responses/users/user-7.json"),
        ] {
            let detail = user_detail(&ctx, user_id, include_eval, false, TIMEOUT)
                .await
                .expect("detail")
                .unwrap_or_else(|| panic!("missing {user_id}"));
            let fixture = fixture_file(file);
            assert_rows(
                &json_rows(std::slice::from_ref(&detail.user)),
                std::slice::from_ref(&fixture["user"]),
            );
            let sessions = json_rows(&detail.sessions);
            let expected = fixture["sessions"].as_array().expect("sessions");
            assert_eq!(
                ids(&sessions, "session_id"),
                ids(expected, "session_id"),
                "{file}"
            );
            assert_rows(&sessions, expected);
        }
        let missing = user_detail(&ctx, "nobody", false, false, TIMEOUT)
            .await
            .expect("missing");
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn user_pages_cover_every_sort() {
        let ctx = context();
        for sort in [
            UserSort::LastSeen,
            UserSort::TraceCount,
            UserSort::CostTotal,
        ] {
            for order in [SortOrder::Asc, SortOrder::Desc] {
                let full = user_list(&ctx, request(true, sort, order))
                    .await
                    .expect("full");
                let expected = ids(&rows(&full), "user_id");
                assert!(full.next_cursor.is_none());
                let mut walked = Vec::new();
                let mut cursor = None;
                loop {
                    let mut page_request = request(true, sort, order);
                    page_request.limit = 1;
                    page_request.cursor = cursor.as_deref();
                    let page = user_list(&ctx, page_request).await.expect("page");
                    let page_ids = ids(&rows(&page), "user_id");
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
    }

    #[tokio::test]
    async fn an_environment_filter_is_visible_to_the_exclusions() {
        let ctx = context();
        let mut production = request(false, UserSort::LastSeen, SortOrder::Desc);
        production.filter = Some("environment = 'production'");
        let page = user_list(&ctx, production).await.expect("production");
        let production_rows = rows(&page);
        // user-42's 10:20 trace is staging, and the anonymous 10:30 trace has
        // no user. last_seen desc is user-7 (10:10) then user-42 (two
        // production traces). The eval trace is not one of them.
        assert_eq!(ids(&production_rows, "user_id"), ["user-7", "user-42"]);
        assert_eq!(production_rows[0]["trace_count"], 1);
        assert_eq!(production_rows[1]["trace_count"], 2);

        let mut eval_only = request(false, UserSort::LastSeen, SortOrder::Desc);
        let eval_filter = format!("environment = '{}'", environment::EVAL);
        eval_only.filter = Some(&eval_filter);
        let page = user_list(&ctx, eval_only).await.expect("eval");
        let eval_rows = rows(&page);
        assert_eq!(ids(&eval_rows, "user_id"), ["user-42"]);
        assert_eq!(eval_rows[0]["trace_count"], 1);
    }

    #[tokio::test]
    async fn count_distinct_session_id_plans_as_an_aggregate() {
        let request = request(false, UserSort::LastSeen, SortOrder::Desc);
        let frame = user_frame(&context(), &request).await.expect("frame");
        let logical = format!("{}", frame.logical_plan().display_indent());
        assert!(
            logical.contains("count") && logical.to_lowercase().contains("distinct"),
            "{logical}"
        );
    }

    #[tokio::test]
    async fn a_user_with_only_excluded_traces_is_none() {
        let mut root = span_row(7, 0, 1_000, 1_000);
        root.user_id = Some("eval-user".to_owned());
        root.environment = Some(environment::EVAL.to_owned());
        let spans = [root];
        let ctx = context_of(&spans);
        let hidden = user_detail(&ctx, "eval-user", false, false, TIMEOUT)
            .await
            .expect("hidden");
        assert!(hidden.is_none());
        let shown = user_detail(&ctx, "eval-user", true, false, TIMEOUT)
            .await
            .expect("eval");
        assert!(shown.is_some());
    }

    #[tokio::test]
    async fn a_user_without_sessions_has_an_empty_session_list() {
        let mut root = span_row(7, 0, 1_000, 1_000);
        root.user_id = Some("lonely".to_owned());
        let spans = [root];
        let detail = user_detail(&context_of(&spans), "lonely", false, false, TIMEOUT)
            .await
            .expect("detail")
            .expect("user");
        assert!(detail.sessions.is_empty());
    }

    #[tokio::test]
    async fn user_detail_plans_a_semi_join() {
        let ctx = context();
        let state = ctx.session().state();
        let excluded = scoped_traces(&ctx, scope(false), false)
            .await
            .expect("scoped");
        let excluded_plan = excluded.logical_plan().clone();
        let user_plan = DataFrame::new(state.clone(), excluded_plan.clone())
            .filter(col(traces::USER_ID).eq(lit("user-42")))
            .expect("user filter")
            .logical_plan()
            .clone();
        let plan = user_sessions_plan(&state, excluded_plan, user_plan).expect("plan");
        let optimized = state.optimize(&plan).expect("optimize");
        let text = format!("{}", optimized.display_indent());
        assert!(text.contains("LeftSemi"), "{text}");
    }
}
