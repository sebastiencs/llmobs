//! Shared plumbing of the read handlers (D-G-14).

use std::time::Duration;

use llmobs_core::ids::ProjectId;
use llmobs_storage::{ListScope, QueryContext, QueryOptions, ScoreTargets, Storage, TimeWindow};

use crate::api::common::{Limit, SortOrder, TimeParam};
use crate::api::convert;
use crate::api::sessions::SessionSummary;
use crate::api::traces::TraceSummary;
use crate::http::AppState;
use crate::http::error::ApiError;

/// Time allowed for one read: planning, running and score lookups each get this budget.
pub(crate) const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Admits a query on `project` and builds A2's context, without a window (D101).
pub(crate) async fn open_query(
    state: &AppState,
    project: ProjectId,
    around: Option<i64>,
) -> Result<QueryContext, ApiError> {
    open_query_on(&state.storage, project, around, None).await
}

/// [`open_query`] for a caller that holds the storage but not the whole state, such as a
/// stream task (the task must not keep an `AppState` alive). `window` is D101's.
pub(crate) async fn open_query_on(
    storage: &Storage,
    project: ProjectId,
    around: Option<i64>,
    window: Option<TimeWindow>,
) -> Result<QueryContext, ApiError> {
    let source = storage.query(project, QueryOptions { around }).await?;
    Ok(QueryContext::new(source, window)?)
}

/// [`open_query`] with D101's window written into the views. The SQL console uses it.
pub(crate) async fn open_query_windowed(
    state: &AppState,
    project: ProjectId,
    window: Option<TimeWindow>,
) -> Result<QueryContext, ApiError> {
    open_query_on(&state.storage, project, None, window).await
}

/// `[from, to)`. One bound alone leaves the other side open (`i64::MIN` / `i64::MAX`). Neither: `None`.
/// `from >= to` is A2's `InvalidRequest` (400). No 30-day cap: spec 05's search limit is the
/// UI's time picker; the server passes the window through and A2 only requires one for `search`.
pub(crate) fn window(
    from: Option<TimeParam>,
    to: Option<TimeParam>,
) -> Result<Option<TimeWindow>, ApiError> {
    if from.is_none() && to.is_none() {
        return Ok(None);
    }
    let from = from.map_or(i64::MIN, TimeParam::ns);
    let to = to.map_or(i64::MAX, TimeParam::ns);
    Ok(Some(TimeWindow::new(from, to)?))
}

/// The window plus the default-exclusion flags.
pub(crate) fn list_scope(
    from: Option<TimeParam>,
    to: Option<TimeParam>,
    include_eval: bool,
    include_experiments: bool,
) -> Result<ListScope, ApiError> {
    Ok(ListScope {
        window: window(from, to)?,
        include_eval,
        include_experiments,
    })
}

/// `Limit` as A2's `usize`. `Limit` is already 1–1000.
pub(crate) fn page_limit(limit: Limit) -> usize {
    usize::try_from(limit.get()).unwrap_or(llmobs_storage::MAX_PAGE)
}

pub(crate) fn sort_order(order: SortOrder) -> llmobs_storage::SortOrder {
    match order {
        SortOrder::Asc => llmobs_storage::SortOrder::Asc,
        SortOrder::Desc => llmobs_storage::SortOrder::Desc,
    }
}

/// Fills `scores` of each trace with its latest score per name (spec 09 § Response shapes).
///
/// Callers pass at most 1,000 traces.
pub(crate) async fn attach_trace_scores(
    ctx: &QueryContext,
    traces: &mut [TraceSummary],
) -> Result<(), ApiError> {
    if traces.is_empty() {
        return Ok(());
    }
    let ids: Vec<String> = traces
        .iter()
        .map(|trace| trace.trace_id.to_string())
        .collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let batches =
        llmobs_storage::latest_scores(ctx, ScoreTargets::Traces(&refs), READ_TIMEOUT).await?;
    let mut briefs = convert::score_briefs(&batches)?;
    for (trace, id) in traces.iter_mut().zip(&ids) {
        trace.scores = briefs.remove(id).unwrap_or_default();
    }
    Ok(())
}

/// Fills `scores` of each session with its latest score per name.
///
/// Callers pass at most 1,000 sessions.
pub(crate) async fn attach_session_scores(
    ctx: &QueryContext,
    sessions: &mut [SessionSummary],
) -> Result<(), ApiError> {
    if sessions.is_empty() {
        return Ok(());
    }
    let ids: Vec<String> = sessions
        .iter()
        .map(|session| session.session_id.clone())
        .collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let batches =
        llmobs_storage::latest_scores(ctx, ScoreTargets::Sessions(&refs), READ_TIMEOUT).await?;
    let mut briefs = convert::score_briefs(&batches)?;
    for (session, id) in sessions.iter_mut().zip(&ids) {
        session.scores = briefs.remove(id).unwrap_or_default();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use llmobs_storage::{RunError, SortOrder as StorageSortOrder, TraceListRequest, TraceSort};

    use super::*;
    use crate::test_support::TestApp;

    fn time(text: &str) -> TimeParam {
        serde_json::from_value(serde_json::Value::String(text.to_owned())).unwrap()
    }

    #[test]
    fn sort_order_and_page_limit_map_to_storage() {
        assert_eq!(sort_order(SortOrder::Asc), StorageSortOrder::Asc);
        assert_eq!(sort_order(SortOrder::Desc), StorageSortOrder::Desc);
        let limit: Limit = serde_json::from_value(serde_json::json!(1000)).unwrap();
        assert_eq!(page_limit(limit), 1000);
        let limit: Limit = serde_json::from_value(serde_json::json!(50)).unwrap();
        assert_eq!(page_limit(limit), 50);
    }

    #[test]
    fn no_bounds_is_no_window() {
        assert!(window(None, None).unwrap().is_none());
    }

    #[test]
    fn one_bound_leaves_the_other_side_open() {
        let from = time("1767225600000");
        let to = time("1767229200000");
        let only_from = window(Some(from), None).unwrap().unwrap();
        assert_eq!(only_from.from(), from.ns());
        assert_eq!(only_from.to(), i64::MAX);
        let only_to = window(None, Some(to)).unwrap().unwrap();
        assert_eq!(only_to.from(), i64::MIN);
        assert_eq!(only_to.to(), to.ns());
    }

    #[test]
    fn empty_window_is_bad_request() {
        let at = time("1767225600000");
        let error = window(Some(at), Some(at)).unwrap_err();
        assert!(matches!(
            error,
            ApiError::Query(RunError::InvalidRequest(_))
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn example_dataset_lists_five_traces_newest_first() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        app.load_example_dataset(seeded.project.id).await;
        let ctx = open_query(&app.state, seeded.project.id, None)
            .await
            .unwrap();
        let scope = list_scope(None, None, false, false).unwrap();
        let page = llmobs_storage::trace_list(
            &ctx,
            TraceListRequest {
                filter: None,
                search: None,
                scope,
                sort: TraceSort::StartTime,
                order: StorageSortOrder::Desc,
                limit: 50,
                cursor: None,
                timeout: READ_TIMEOUT,
            },
        )
        .await
        .unwrap();
        let mut ids = Vec::new();
        for batch in &page.batches {
            let column = batch.column_by_name("trace_id").unwrap();
            let strings = arrow::array::AsArray::as_string::<i32>(column.as_ref());
            ids.extend(strings.iter().map(|id| id.unwrap().to_owned()));
        }
        let expected = [
            "a1000000000000000000000000000006",
            "a1000000000000000000000000000004",
            "a1000000000000000000000000000003",
            "a1000000000000000000000000000002",
            "a1000000000000000000000000000001",
        ];
        assert_eq!(ids, expected);
    }
}
