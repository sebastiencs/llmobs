//! Scope resolution of `trace_ids`, `filter` and `user` deletions over the storage reads
//! (D-M-2, D-M-6). Called by the worker for a row in `resolving` with no stored scope.

use std::collections::HashSet;

use llmobs_core::ids::{ProjectId, TraceId};
use llmobs_storage::{
    FilterDeletionRequest, QueryContext, QueryOptions, TimeWindow, eval_trace_ids,
    filter_trace_ids, user_deletion_scope,
};

use super::{RESOLVE_TIMEOUT, Step};
use crate::http::AppState;
use crate::meta::deletions::{
    DeletionKind, DeletionRow, FailReason, insert_sessions, insert_traces, scope_traces,
    store_counts,
};

/// Resolves the scope of `row` and stores it with its counts in one transaction. Returns
/// `Ok(false)` when the row is no longer `resolving`, in which case nothing is handed over.
///
/// # Errors
///
/// Returns the [`Step`] to take when a read or the write fails.
pub(super) async fn resolve(
    state: &AppState,
    row: &DeletionRow,
    project: ProjectId,
) -> Result<bool, Step> {
    let source = state
        .storage
        .query(project, QueryOptions { around: None })
        .await
        .map_err(|error| super::from_query(&error))?;
    let ctx = QueryContext::new(source, None).map_err(|error| super::from_run(&error))?;

    let (targets, sessions) = match row.kind {
        DeletionKind::TraceIds => {
            let traces = scope_traces(state.meta.reader(), row.id)
                .await
                .map_err(super::from_meta)?;
            (traces, Vec::new())
        }
        DeletionKind::Filter => (filter_targets(state, row, &ctx).await?, Vec::new()),
        DeletionKind::User => {
            let Some(user_id) = row.user_id.as_deref() else {
                return Err(Step::Fail(FailReason::ResolutionFailed));
            };
            let scope = user_deletion_scope(&ctx, user_id, row.created_at, RESOLVE_TIMEOUT)
                .await
                .map_err(|error| super::from_run(&error))?;
            (scope.trace_ids, scope.session_ids)
        }
        DeletionKind::Project | DeletionKind::Org => {
            return Err(Step::Fail(FailReason::ResolutionFailed));
        }
    };

    let target_set: HashSet<TraceId> = targets.iter().copied().collect();
    let evaluations = eval_trace_ids(&ctx, &target_set, RESOLVE_TIMEOUT)
        .await
        .map_err(|error| super::from_run(&error))?;
    let mut scope = targets;
    scope.extend(evaluations);
    scope.sort();
    scope.dedup();

    let id = row.id;
    let session_count = (row.kind == DeletionKind::User).then(|| count(sessions.len()));
    let trace_count = count(scope.len());
    let stored = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                insert_traces(conn, id, &scope).await?;
                insert_sessions(conn, id, &sessions).await?;
                store_counts(conn, id, trace_count, session_count).await
            })
        })
        .await
        .map_err(super::from_meta)?;
    Ok(stored)
}

/// The traces matching a filter deletion's request, at most the cap.
async fn filter_targets(
    state: &AppState,
    row: &DeletionRow,
    ctx: &QueryContext,
) -> Result<Vec<TraceId>, Step> {
    let request = &row.request;
    let (Some(from), Some(to)) = (request.from, request.to) else {
        return Err(Step::Fail(FailReason::ResolutionFailed));
    };
    let window = TimeWindow::new(from, to).map_err(|error| super::from_run(&error))?;
    let cap = state.deletions.filter_cap();
    let ids = filter_trace_ids(
        ctx,
        FilterDeletionRequest {
            filter: request.filter.as_deref(),
            search: request.search.as_deref(),
            window,
            include_eval: request.include_eval.unwrap_or(false),
            include_experiments: request.include_experiments.unwrap_or(false),
            limit: cap.saturating_add(1),
            timeout: RESOLVE_TIMEOUT,
        },
    )
    .await
    .map_err(|error| super::from_run(&error))?;
    if ids.len() > cap {
        return Err(Step::Fail(FailReason::TooManyTraces));
    }
    Ok(ids)
}

/// A count as the `u64` the store takes. Saturates only where `usize` exceeds `u64`.
fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}
