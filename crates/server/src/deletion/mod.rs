//! Deletion requests after creation: resolve, register, poll; project and org drops
//! (spec 03 § 7, D-M-3).

pub(crate) mod guard;
mod resolve;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use llmobs_core::ids::{OrgId, ProjectId};
use llmobs_storage::error::{DeletionError, QueryError};
use llmobs_storage::{
    Deletion, DeletionScope, DeletionStatus, MAX_FILTER_DELETION_TRACES, RunError,
};
use tokio::sync::Notify;
use uuid::Uuid;

use crate::http::AppState;
use crate::meta::MetaError;
use crate::meta::deletions::{
    DeletionKind, DeletionRow, DeletionState, FailReason, clear_handover, mark_applied,
    mark_dropped, mark_failed, mark_handed_over, open_deletions, scope_projects, scope_sessions,
    scope_traces,
};
use crate::meta::orgs::{delete_org, org_by_id};
use crate::meta::projects::{delete_project, project_by_id, project_ids_of_org};

/// Time between two runs when nothing wakes the worker.
pub(crate) const TICK: Duration = Duration::from_secs(2);
/// Time a failed row waits before its next attempt (60 s).
pub(crate) const RETRY_NS: i64 = 60_000_000_000;
/// Consecutive failures after which a `trace_ids`, `filter` or `user` row fails.
pub(crate) const MAX_ATTEMPTS: u32 = 5;
/// Time allowed to each resolution read.
pub(crate) const RESOLVE_TIMEOUT: Duration = Duration::from_secs(600);

/// Worker state shared through `AppInner.deletions`.
#[derive(Debug)]
pub(crate) struct Deletions {
    /// Notified by every route that creates a deletion. Producers call `notify_one()`
    /// (never `notify_waiters()`), so a wake sent while `run_once` runs is kept for the next wait.
    pub(crate) wake: Notify,
    pub(crate) guard: guard::DeletionGuard,
    attempts: Mutex<HashMap<Uuid, Attempt>>,
    /// `MAX_FILTER_DELETION_TRACES`; tests lower it.
    filter_cap: AtomicUsize,
}

#[derive(Debug, Clone, Copy)]
struct Attempt {
    failures: u32,
    retry_at: i64,
}

impl Deletions {
    pub(crate) fn new() -> Self {
        Self {
            wake: Notify::new(),
            guard: guard::DeletionGuard::default(),
            attempts: Mutex::new(HashMap::new()),
            filter_cap: AtomicUsize::new(MAX_FILTER_DELETION_TRACES),
        }
    }

    /// The filter cap in force (M4's POST count uses it too).
    pub(crate) fn filter_cap(&self) -> usize {
        self.filter_cap.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn set_filter_cap(&self, cap: usize) {
        self.filter_cap.store(cap, Ordering::Relaxed);
    }

    fn attempts(&self) -> std::sync::MutexGuard<'_, HashMap<Uuid, Attempt>> {
        self.attempts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether `id` may be attempted at `now`.
    fn due(&self, id: Uuid, now: i64) -> bool {
        self.attempts()
            .get(&id)
            .is_none_or(|attempt| now >= attempt.retry_at)
    }

    /// Records a failure; returns the consecutive count.
    fn failed(&self, id: Uuid, now: i64) -> u32 {
        let mut attempts = self.attempts();
        let attempt = attempts.entry(id).or_insert(Attempt {
            failures: 0,
            retry_at: now,
        });
        attempt.failures = attempt.failures.saturating_add(1);
        attempt.retry_at = now.saturating_add(RETRY_NS);
        attempt.failures
    }

    /// Forgets `id`'s failures (after a success or a final state).
    fn settled(&self, id: Uuid) {
        self.attempts().remove(&id);
    }
}

impl Default for Deletions {
    fn default() -> Self {
        Self::new()
    }
}

/// Why a step stopped.
#[derive(Debug)]
enum Step {
    /// Try again later; a data row in `resolving` fails after `MAX_ATTEMPTS`.
    /// The label is a static error kind for the log.
    Retry(&'static str),
    /// Try again later, never fail: storage already holds the deletion (any error after
    /// `register_deletion` returned `Ok`), or the row is a drop.
    RetryForever(&'static str),
    /// Give up: the row becomes `failed` (data kinds only).
    Fail(FailReason),
}

fn from_meta(_error: MetaError) -> Step {
    Step::Retry("meta")
}

fn from_query(error: &QueryError) -> Step {
    match error {
        QueryError::Busy => Step::Retry("busy"),
        QueryError::DataFusion(_) => Step::Retry("query"),
    }
}

fn from_run(error: &RunError) -> Step {
    match error {
        RunError::Timeout => Step::Retry("timeout"),
        RunError::Internal(_) => Step::Retry("internal"),
        RunError::Invalid(_)
        | RunError::InvalidRequest(_)
        | RunError::Failed(_)
        | RunError::TooLarge => Step::Fail(FailReason::ResolutionFailed),
    }
}

fn from_deletion(error: &DeletionError) -> Step {
    match error {
        DeletionError::Invalid { .. } => Step::Fail(FailReason::ResolutionFailed),
        DeletionError::NotFound => Step::Retry("not_found"),
        DeletionError::Catalog(_) => Step::Retry("catalog"),
    }
}

/// A project or org whose data and `meta.db` rows are gone.
#[derive(Debug, Clone)]
pub(crate) struct Dropped {
    pub(crate) deletion_id: Uuid,
    pub(crate) org_id: OrgId,
    /// The dropped projects (one for a project drop; every project of the org for an org drop).
    pub(crate) projects: Vec<ProjectId>,
    /// The org itself is gone.
    pub(crate) whole_org: bool,
}

/// Called exactly once per drop, after its `meta.db` transaction committed (D-M-17).
/// Track Q adds the control-plane notification here (org → cell mapping, D170), through
/// its own outbox. Today it records the audit event.
pub(crate) fn on_dropped(_state: &AppState, dropped: &Dropped) {
    if dropped.whole_org {
        tracing::info!(target: "audit", deletion_id = %dropped.deletion_id, org_id = %dropped.org_id, projects = dropped.projects.len(), "org dropped");
    } else {
        for project in &dropped.projects {
            tracing::info!(target: "audit", deletion_id = %dropped.deletion_id, org_id = %dropped.org_id, project_id = %project, "project dropped");
        }
    }
}

/// Processes every open deletion once, oldest first. Errors on one row are logged and
/// counted; they do not stop the others. Returns `Err` only when the open rows cannot be listed.
///
/// # Errors
///
/// Returns [`MetaError`] when the open rows cannot be read.
pub(crate) async fn run_once(state: &AppState) -> Result<(), MetaError> {
    state.deletions.guard.prune(state.clock.now_ns());
    let rows = open_deletions(state.meta.reader()).await?;
    for row in rows {
        // Per row: a resolution read can take minutes.
        let now = state.clock.now_ns();
        if !state.deletions.due(row.id, now) {
            continue;
        }
        let result = match row.kind {
            DeletionKind::TraceIds | DeletionKind::Filter | DeletionKind::User => {
                data_step(state, &row, now).await
            }
            DeletionKind::Project => drop_project_step(state, &row, now).await,
            DeletionKind::Org => drop_org_step(state, &row, now).await,
        };
        match result {
            Ok(()) => state.deletions.settled(row.id),
            Err(Step::Retry(label)) => retry(state, &row, now, label, true).await,
            Err(Step::RetryForever(label)) => retry(state, &row, now, label, false).await,
            Err(Step::Fail(reason)) => fail(state, &row, now, reason).await,
        }
    }
    Ok(())
}

/// Records a failed step and, for a data row with no stored scope that has failed
/// `MAX_ATTEMPTS` times in a row, gives up on it.
async fn retry(state: &AppState, row: &DeletionRow, now: i64, label: &'static str, may_fail: bool) {
    let failures = state.deletions.failed(row.id, now);
    tracing::warn!(
        deletion_id = %row.id,
        kind = row.kind.as_str(),
        error = label,
        failures = failures,
        "deletion step failed"
    );
    let data_kind = matches!(
        row.kind,
        DeletionKind::TraceIds | DeletionKind::Filter | DeletionKind::User
    );
    // A row whose scope is stored keeps retrying: `failed` must mean nothing was hidden.
    if may_fail
        && data_kind
        && row.state == DeletionState::Resolving
        && row.trace_count.is_none()
        && failures >= MAX_ATTEMPTS
    {
        fail(state, row, now, FailReason::ResolutionFailed).await;
    }
}

/// Marks a data row `failed`. Logs the audit event when the row changed.
async fn fail(state: &AppState, row: &DeletionRow, now: i64, reason: FailReason) {
    let id = row.id;
    let written = state
        .meta
        .write(move |conn| Box::pin(async move { mark_failed(conn, id, reason).await }))
        .await;
    match written {
        Ok(true) => {
            tracing::info!(target: "audit", deletion_id = %row.id, error = reason.as_str(), "deletion failed");
            state.deletions.settled(row.id);
        }
        Ok(false) => {
            tracing::warn!(
                deletion_id = %row.id,
                error = reason.as_str(),
                "deletion step failed on a row that cannot fail"
            );
            state.deletions.failed(row.id, now);
        }
        Err(error) => {
            tracing::warn!(deletion_id = %row.id, error = %error, "deletion failure could not be recorded");
            state.deletions.failed(row.id, now);
        }
    }
}

/// Sets `row` applied at `at`. Logs the audit event when the row changed.
async fn applied_now(state: &AppState, row: &DeletionRow, at: i64) -> Result<(), MetaError> {
    let id = row.id;
    let changed = state
        .meta
        .write(move |conn| Box::pin(async move { mark_applied(conn, id, at).await }))
        .await?;
    if changed {
        tracing::info!(target: "audit", deletion_id = %row.id, "deletion applied");
    }
    Ok(())
}

/// One step of a `trace_ids`, `filter` or `user` row.
async fn data_step(state: &AppState, row: &DeletionRow, now: i64) -> Result<(), Step> {
    let Some(project) = row.project_id else {
        return Err(Step::Fail(FailReason::ResolutionFailed));
    };
    let reader = state.meta.reader();

    // Superseded: the project or its org is being dropped (or is gone). The drop erases
    // everything, so the deletion is done without a storage call.
    let project_row = project_by_id(reader, project).await.map_err(from_meta)?;
    let org_row = org_by_id(reader, row.org_id).await.map_err(from_meta)?;
    let live = project_row.is_some_and(|row| row.state == "active")
        && org_row.is_some_and(|row| row.state == "active");
    if !live {
        return applied_now(state, row, now).await.map_err(from_meta);
    }

    // The guard learns the request even when a previous process stored it.
    match row.kind {
        DeletionKind::User => {
            if let Some(user) = row.user_id.as_deref() {
                state
                    .deletions
                    .guard
                    .add_user(project, user, row.created_at);
            }
        }
        DeletionKind::TraceIds if row.trace_count.is_none() => {
            let requested = scope_traces(reader, row.id).await.map_err(from_meta)?;
            state
                .deletions
                .guard
                .add_traces(project, &requested, row.created_at);
        }
        _ => {}
    }

    match (row.state, row.trace_count, row.handed_over_at) {
        (DeletionState::Resolving, None, _) => {
            if !resolve::resolve(state, row, project).await? {
                return Ok(());
            }
            register(state, row, project, now).await
        }
        (DeletionState::Resolving, Some(_), _) | (DeletionState::Pending, _, None) => {
            register(state, row, project, now).await
        }
        (DeletionState::Pending, _, Some(_)) => poll(state, row).await,
        // Applied and failed rows are not listed by `open_deletions`.
        (DeletionState::Applied | DeletionState::Failed, _, _) => Ok(()),
    }
}

/// Hands the stored scope to storage and marks the row handed over.
async fn register(
    state: &AppState,
    row: &DeletionRow,
    project: ProjectId,
    now: i64,
) -> Result<(), Step> {
    let reader = state.meta.reader();
    let traces = scope_traces(reader, row.id).await.map_err(from_meta)?;
    let sessions = if row.kind == DeletionKind::User {
        scope_sessions(reader, row.id).await.map_err(from_meta)?
    } else {
        Vec::new()
    };
    if matches!(row.kind, DeletionKind::TraceIds | DeletionKind::Filter) && traces.is_empty() {
        // Storage refuses an empty `Traces` scope: there is nothing to hide.
        applied_now(state, row, now).await.map_err(from_meta)?;
        return Ok(());
    }

    let trace_count = traces.len();
    let session_count = sessions.len();
    state
        .deletions
        .guard
        .add_traces(project, &traces, row.created_at);
    let scope = match row.kind {
        DeletionKind::User => DeletionScope::User {
            user_id: row
                .user_id
                .clone()
                .ok_or(Step::Fail(FailReason::ResolutionFailed))?,
            trace_ids: traces,
            session_ids: sessions,
        },
        DeletionKind::TraceIds
        | DeletionKind::Filter
        | DeletionKind::Project
        | DeletionKind::Org => DeletionScope::Traces { trace_ids: traces },
    };
    let deletion = Deletion {
        id: row.id,
        project,
        cutoff: row.created_at,
        scope,
    };
    state
        .storage
        .register_deletion(deletion)
        .await
        .map_err(|error| from_deletion(&error))?;

    // Storage hides the rows from here on, so no error below may fail the row.
    let id = row.id;
    state
        .meta
        .write(move |conn| Box::pin(async move { mark_handed_over(conn, id, now).await }))
        .await
        .map_err(|_| Step::RetryForever("meta"))?;
    state.live.publish_resync(project);
    state.metrics_cache.clear_project(project);
    tracing::info!(
        target: "audit",
        deletion_id = %row.id,
        trace_count = trace_count,
        session_count = session_count,
        "deletion registered"
    );
    Ok(())
}

/// Asks storage whether a handed-over deletion is applied yet.
async fn poll(state: &AppState, row: &DeletionRow) -> Result<(), Step> {
    match state.storage.deletion_status(row.id).await {
        Ok(DeletionStatus::Pending) => Ok(()),
        Ok(DeletionStatus::Applied { applied_at }) => applied_now(state, row, applied_at)
            .await
            .map_err(|_| Step::RetryForever("meta")),
        Err(DeletionError::NotFound) => {
            let id = row.id;
            state
                .meta
                .write(move |conn| Box::pin(async move { clear_handover(conn, id).await }))
                .await
                .map_err(|_| Step::RetryForever("meta"))?;
            tracing::warn!(
                deletion_id = %row.id,
                "storage does not know a handed-over deletion; registering it again"
            );
            Ok(())
        }
        Err(error) => Err(from_deletion(&error)),
    }
}

/// Drops a project in storage, then deletes its `meta.db` row (and, by cascade, its keys
/// and settings) and marks the deletion applied.
async fn drop_project_step(state: &AppState, row: &DeletionRow, now: i64) -> Result<(), Step> {
    let Some(project) = row.project_id else {
        tracing::error!(deletion_id = %row.id, "a project deletion has no project");
        return Err(Step::RetryForever("corrupt"));
    };
    if row.state != DeletionState::Pending || row.handed_over_at.is_some() {
        return Ok(());
    }
    state
        .storage
        .drop_project(project)
        .await
        .map_err(|_| Step::RetryForever("storage"))?;
    let id = row.id;
    let dropped = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                delete_project(conn, project).await?;
                mark_dropped(conn, id, now).await
            })
        })
        .await
        .map_err(|_| Step::RetryForever("meta"))?;
    state.live.publish_resync(project);
    state.metrics_cache.clear_project(project);
    if dropped {
        tracing::info!(target: "audit", deletion_id = %row.id, "deletion applied");
        on_dropped(
            state,
            &Dropped {
                deletion_id: row.id,
                org_id: row.org_id,
                projects: vec![project],
                whole_org: false,
            },
        );
    }
    Ok(())
}

/// Drops every project of the org (including any created after the request), its usage
/// and the org row.
async fn drop_org_step(state: &AppState, row: &DeletionRow, now: i64) -> Result<(), Step> {
    let reader = state.meta.reader();
    let mut projects = scope_projects(reader, row.id)
        .await
        .map_err(|_| Step::RetryForever("meta"))?;
    projects.extend(
        project_ids_of_org(reader, row.org_id)
            .await
            .map_err(|_| Step::RetryForever("meta"))?,
    );
    projects.sort();
    projects.dedup();

    for project in &projects {
        state
            .storage
            .drop_project(*project)
            .await
            .map_err(|_| Step::RetryForever("storage"))?;
    }
    // Usage counted in memory for these projects must not land after their rows are gone.
    state
        .usage
        .flush(&state.meta)
        .await
        .map_err(|_| Step::RetryForever("meta"))?;
    let id = row.id;
    let org = row.org_id;
    let dropped = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                crate::usage::delete_org_usage(conn, org).await?;
                delete_org(conn, org).await?;
                mark_dropped(conn, id, now).await
            })
        })
        .await
        .map_err(|_| Step::RetryForever("meta"))?;
    for project in &projects {
        state.live.publish_resync(*project);
        state.metrics_cache.clear_project(*project);
    }
    if dropped {
        tracing::info!(target: "audit", deletion_id = %row.id, "deletion applied");
        on_dropped(
            state,
            &Dropped {
                deletion_id: row.id,
                org_id: row.org_id,
                projects,
                whole_org: true,
            },
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use axum::http::{Method, StatusCode};
    use llmobs_core::batch::spans_to_record_batch;
    use llmobs_core::ids::{ProjectId, SpanId, TraceId};
    use llmobs_core::names::metadata_key;
    use llmobs_core::span::Span;
    use llmobs_storage::DeletionStatus;
    use llmobs_storage::error::{AppendError, DeletionError};

    use super::*;
    use crate::meta::deletions::{
        ActorKind, DeletionState, NewDeletion, RequestDetails, deletion_by_id, insert_deletion,
        insert_projects, insert_traces, mark_handed_over, scope_sessions, scope_traces,
        store_counts,
    };
    use crate::meta::orgs::{insert_org, org_by_id};
    use crate::meta::projects::{ProjectRow, insert_project, project_by_id};
    use crate::test_support::{Seeded, TestApp, capture_logs, dataset_span};

    /// A trace id of the tests: `a2`, then `n` in 30 hex digits.
    fn trace_id(n: u32) -> String {
        format!("a2{n:030x}")
    }

    /// A span id of the tests: `b1`, then `n` in 14 hex digits.
    fn span_id(n: u32) -> String {
        format!("b1{n:014x}")
    }

    fn trace(n: u32) -> TraceId {
        trace_id(n).parse().unwrap()
    }

    /// Commits one span of `trace` to `project`. The span is stamped with the clock's now as
    /// its ingest time, so a deletion created later covers it.
    async fn commit(
        app: &TestApp,
        project: &ProjectRow,
        trace: &str,
        span: &str,
        edit: impl FnOnce(&mut Span),
    ) {
        let mut row = dataset_span(0);
        row.trace_id = trace.parse::<TraceId>().unwrap();
        row.span_id = span.parse::<SpanId>().unwrap();
        row.parent_span_id = None;
        row.user_id = None;
        row.session_id = None;
        row.environment = None;
        row.metadata.clear();
        row.ingest_time = app.state.clock.now_ns();
        edit(&mut row);
        app.commit_spans(project, vec![row]).await;
    }

    /// `GET /api/v1/projects/{project}/traces/{trace}` with the seeded cookie; returns the status.
    async fn trace_status(
        app: &TestApp,
        seeded: &Seeded,
        project: ProjectId,
        trace: &str,
    ) -> StatusCode {
        app.request(
            Method::GET,
            &format!("/api/v1/projects/{project}/traces/{trace}"),
        )
        .header("cookie", &seeded.cookie)
        .send()
        .await
        .status
    }

    /// A deletion row of `kind` in `project`, created now, still open.
    fn new_row(app: &TestApp, project: &ProjectRow, kind: DeletionKind) -> NewDeletion {
        NewDeletion {
            id: Uuid::now_v7(),
            org_id: project.org_id,
            project_id: (kind != DeletionKind::Org).then_some(project.id),
            kind,
            user_id: None,
            request: RequestDetails::default(),
            actor_kind: ActorKind::User,
            actor_id: Uuid::now_v7(),
            state: match kind {
                DeletionKind::TraceIds | DeletionKind::Filter | DeletionKind::User => {
                    DeletionState::Resolving
                }
                DeletionKind::Project | DeletionKind::Org => DeletionState::Pending,
            },
            created_at: app.state.clock.now_ns(),
        }
    }

    /// Inserts `new` and its requested trace ids.
    async fn insert(app: &TestApp, new: NewDeletion, traces: &[TraceId]) -> Uuid {
        let id = new.id;
        let traces = traces.to_vec();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    insert_deletion(conn, &new).await?;
                    insert_traces(conn, id, &traces).await
                })
            })
            .await
            .unwrap();
        id
    }

    async fn deletion_row(app: &TestApp, id: Uuid) -> DeletionRow {
        deletion_by_id(app.state.meta.reader(), id)
            .await
            .unwrap()
            .unwrap()
    }

    async fn count_rows(app: &TestApp, sql: &'static str, bind: String) -> i64 {
        sqlx::query_scalar::<_, i64>(sql)
            .bind(bind)
            .fetch_one(app.state.meta.reader())
            .await
            .unwrap()
    }

    async fn worker(app: &TestApp) {
        run_once(&app.state).await.unwrap();
    }

    fn wall_now_ns() -> i64 {
        let since = SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        i64::try_from(since.as_nanos()).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn trace_ids_are_hidden_once_registered() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = &seeded.project;
        let (a, b) = (trace_id(1), trace_id(2));
        commit(&app, project, &a, &span_id(1), |_| {}).await;
        commit(&app, project, &b, &span_id(2), |_| {}).await;
        app.state.clock.advance(Duration::from_secs(1));

        let id = insert(
            &app,
            new_row(&app, project, DeletionKind::TraceIds),
            &[trace(1)],
        )
        .await;
        worker(&app).await;

        let now = app.state.clock.now_ns();
        let row = deletion_row(&app, id).await;
        assert_eq!(row.state, DeletionState::Pending);
        assert_eq!(row.trace_count, Some(1));
        assert_eq!(row.handed_over_at, Some(now));
        assert_eq!(
            trace_status(&app, &seeded, project.id, &a).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            trace_status(&app, &seeded, project.id, &b).await,
            StatusCode::OK
        );
        assert_eq!(
            app.state.storage.deletion_status(id).await.unwrap(),
            DeletionStatus::Pending
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn eval_traces_join_the_scope() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = &seeded.project;
        let (a, e) = (trace_id(1), trace_id(3));
        commit(&app, project, &a, &span_id(1), |_| {}).await;
        commit(&app, project, &e, &span_id(3), |row| {
            row.environment = Some("llmobs-eval".to_owned());
            row.metadata
                .insert(metadata_key::EVAL_TARGET_TRACE_ID.to_owned(), a.clone());
        })
        .await;
        assert_eq!(
            trace_status(&app, &seeded, project.id, &e).await,
            StatusCode::OK
        );
        app.state.clock.advance(Duration::from_secs(1));

        let id = insert(
            &app,
            new_row(&app, project, DeletionKind::TraceIds),
            &[trace(1)],
        )
        .await;
        worker(&app).await;

        let row = deletion_row(&app, id).await;
        assert_eq!(row.trace_count, Some(2));
        let mut expected = vec![trace(1), trace(3)];
        expected.sort();
        let scope = scope_traces(app.state.meta.reader(), id).await.unwrap();
        assert_eq!(scope, expected);
        assert_eq!(
            trace_status(&app, &seeded, project.id, &e).await,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn filter_resolves_matching_traces() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = &seeded.project;
        let (a, b) = (trace_id(1), trace_id(2));
        commit(&app, project, &a, &span_id(1), |row| {
            row.name = "keep".to_owned();
        })
        .await;
        commit(&app, project, &b, &span_id(2), |row| {
            row.name = "drop".to_owned();
        })
        .await;
        app.state.clock.advance(Duration::from_secs(1));

        let mut new = new_row(&app, project, DeletionKind::Filter);
        new.request = RequestDetails {
            filter: Some("name = 'drop'".to_owned()),
            from: Some(0),
            to: Some(i64::MAX),
            ..RequestDetails::default()
        };
        let id = insert(&app, new, &[]).await;
        worker(&app).await;

        let scope = scope_traces(app.state.meta.reader(), id).await.unwrap();
        assert_eq!(scope, vec![trace(2)]);
        assert_eq!(
            trace_status(&app, &seeded, project.id, &a).await,
            StatusCode::OK
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn filter_with_no_match_is_applied_at_once() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = &seeded.project;
        commit(&app, project, &trace_id(1), &span_id(1), |row| {
            row.name = "keep".to_owned();
        })
        .await;
        app.state.clock.advance(Duration::from_secs(1));

        let mut new = new_row(&app, project, DeletionKind::Filter);
        new.request = RequestDetails {
            filter: Some("name = 'nothing'".to_owned()),
            from: Some(0),
            to: Some(i64::MAX),
            ..RequestDetails::default()
        };
        let id = insert(&app, new, &[]).await;
        worker(&app).await;

        let row = deletion_row(&app, id).await;
        assert_eq!(row.state, DeletionState::Applied);
        assert_eq!(row.trace_count, Some(0));
        assert_eq!(row.handed_over_at, None);
        assert!(matches!(
            app.state.storage.deletion_status(id).await,
            Err(DeletionError::NotFound)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn filter_over_the_cap_fails() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = &seeded.project;
        let (a, b) = (trace_id(1), trace_id(2));
        commit(&app, project, &a, &span_id(1), |row| {
            row.name = "drop".to_owned();
        })
        .await;
        commit(&app, project, &b, &span_id(2), |row| {
            row.name = "drop".to_owned();
        })
        .await;
        app.state.clock.advance(Duration::from_secs(1));
        app.state.deletions.set_filter_cap(1);

        let mut new = new_row(&app, project, DeletionKind::Filter);
        new.request = RequestDetails {
            filter: Some("name = 'drop'".to_owned()),
            from: Some(0),
            to: Some(i64::MAX),
            ..RequestDetails::default()
        };
        let id = insert(&app, new, &[]).await;
        worker(&app).await;

        let row = deletion_row(&app, id).await;
        assert_eq!(row.state, DeletionState::Failed);
        assert_eq!(row.error, Some(FailReason::TooManyTraces));
        assert_eq!(
            trace_status(&app, &seeded, project.id, &a).await,
            StatusCode::OK
        );
        assert_eq!(
            trace_status(&app, &seeded, project.id, &b).await,
            StatusCode::OK
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_filter_fails() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = &seeded.project;
        commit(&app, project, &trace_id(1), &span_id(1), |_| {}).await;
        app.state.clock.advance(Duration::from_secs(1));

        let mut new = new_row(&app, project, DeletionKind::Filter);
        new.request = RequestDetails {
            filter: Some("name = ".to_owned()),
            from: Some(0),
            to: Some(i64::MAX),
            ..RequestDetails::default()
        };
        let id = insert(&app, new, &[]).await;
        worker(&app).await;

        let row = deletion_row(&app, id).await;
        assert_eq!(row.state, DeletionState::Failed);
        assert_eq!(row.error, Some(FailReason::ResolutionFailed));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn user_resolves_traces_and_sessions() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = &seeded.project;
        let (a, b, c) = (trace_id(1), trace_id(2), trace_id(3));
        commit(&app, project, &a, &span_id(1), |row| {
            row.user_id = Some("u1".to_owned());
            row.session_id = Some("s1".to_owned());
        })
        .await;
        commit(&app, project, &b, &span_id(2), |row| {
            row.user_id = Some("u1".to_owned());
        })
        .await;
        commit(&app, project, &c, &span_id(3), |row| {
            row.user_id = Some("u2".to_owned());
        })
        .await;
        app.state.clock.advance(Duration::from_secs(1));

        let mut new = new_row(&app, project, DeletionKind::User);
        new.user_id = Some("u1".to_owned());
        let id = insert(&app, new, &[]).await;
        worker(&app).await;

        let row = deletion_row(&app, id).await;
        assert_eq!(row.trace_count, Some(2));
        assert_eq!(row.session_count, Some(1));
        let sessions = scope_sessions(app.state.meta.reader(), id).await.unwrap();
        assert_eq!(sessions, vec!["s1".to_owned()]);
        assert_eq!(
            trace_status(&app, &seeded, project.id, &a).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            trace_status(&app, &seeded, project.id, &b).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            trace_status(&app, &seeded, project.id, &c).await,
            StatusCode::OK
        );
        let user = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}/users/u1", project.id),
            )
            .header("cookie", &seeded.cookie)
            .send()
            .await;
        assert_eq!(user.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn registered_scope_is_reused_after_a_restart() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = &seeded.project;
        commit(&app, project, &trace_id(1), &span_id(1), |_| {}).await;
        app.state.clock.advance(Duration::from_secs(1));

        // The scope of a previous process: trace X, which storage has never seen.
        let id = insert(
            &app,
            new_row(&app, project, DeletionKind::TraceIds),
            &[trace(9)],
        )
        .await;
        app.state
            .meta
            .write(move |conn| Box::pin(async move { store_counts(conn, id, 1, None).await }))
            .await
            .unwrap();

        worker(&app).await;

        let row = deletion_row(&app, id).await;
        assert_eq!(row.state, DeletionState::Pending);
        assert_eq!(
            app.state.storage.deletion_status(id).await.unwrap(),
            DeletionStatus::Pending
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lost_hand_over_registers_again() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = &seeded.project;
        commit(&app, project, &trace_id(1), &span_id(1), |_| {}).await;
        app.state.clock.advance(Duration::from_secs(1));

        let id = insert(
            &app,
            new_row(&app, project, DeletionKind::TraceIds),
            &[trace(1)],
        )
        .await;
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| Box::pin(async move { mark_handed_over(conn, id, now).await }))
            .await
            .unwrap();

        // Storage does not know the deletion: the hand-over is cleared.
        worker(&app).await;
        assert_eq!(deletion_row(&app, id).await.handed_over_at, None);

        worker(&app).await;
        let row = deletion_row(&app, id).await;
        assert_eq!(row.handed_over_at, Some(now));
        assert_eq!(
            app.state.storage.deletion_status(id).await.unwrap(),
            DeletionStatus::Pending
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reaches_applied_after_flush() {
        let app = TestApp::with_config(|config| {
            config.storage.flush.max_age = Duration::from_millis(100);
        })
        .await;
        let seeded = app.seed().await;
        let project = &seeded.project;
        commit(&app, project, &trace_id(1), &span_id(1), |_| {}).await;
        app.state.clock.advance(Duration::from_secs(1));

        let id = insert(
            &app,
            new_row(&app, project, DeletionKind::TraceIds),
            &[trace(1)],
        )
        .await;
        worker(&app).await;

        let mut applied_at = None;
        for _ in 0..100 {
            app.state
                .storage
                .apply_deletions(wall_now_ns())
                .await
                .unwrap();
            worker(&app).await;
            let row = deletion_row(&app, id).await;
            if row.state == DeletionState::Applied {
                applied_at = row.applied_at;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(applied_at.is_some(), "the deletion reaches applied");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn superseded_when_the_project_is_not_active() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.clone();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    sqlx::query("UPDATE projects SET state = 'deleting' WHERE id = ?")
                        .bind(project.id.to_string())
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();

        let mut new = new_row(&app, &seeded.project, DeletionKind::User);
        new.user_id = Some("u1".to_owned());
        let id = insert(&app, new, &[]).await;
        worker(&app).await;

        assert_eq!(deletion_row(&app, id).await.state, DeletionState::Applied);
        assert!(matches!(
            app.state.storage.deletion_status(id).await,
            Err(DeletionError::NotFound)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn project_drop_removes_the_project() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let (doomed, _key) = app.add_project(&seeded, "doomed", None).await;
        commit(&app, &doomed, &trace_id(1), &span_id(1), |_| {}).await;
        commit(&app, &seeded.project, &trace_id(2), &span_id(2), |_| {}).await;
        app.state.clock.advance(Duration::from_secs(1));

        let mut new = new_row(&app, &doomed, DeletionKind::Project);
        new.request = RequestDetails {
            name: Some("doomed".to_owned()),
            slug: Some("doomed".to_owned()),
            ..RequestDetails::default()
        };
        let id = insert(&app, new, &[]).await;
        worker(&app).await;

        let now = app.state.clock.now_ns();
        let row = deletion_row(&app, id).await;
        assert_eq!(row.state, DeletionState::Applied);
        assert_eq!(row.handed_over_at, Some(now));
        assert_eq!(row.applied_at, Some(now));
        assert!(
            project_by_id(app.state.meta.reader(), doomed.id)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            count_rows(
                &app,
                "SELECT COUNT(*) FROM api_keys WHERE project_id = ?",
                doomed.id.to_string()
            )
            .await,
            0
        );

        let mut span = dataset_span(0);
        span.project_id = doomed.id;
        let batch = spans_to_record_batch(&[span]).unwrap();
        let result = app.state.storage.append_spans(doomed.id, batch).await;
        assert!(
            matches!(result, Err(AppendError::ProjectDropped)),
            "appends to a dropped project are refused"
        );
        assert_eq!(
            trace_status(&app, &seeded, seeded.project.id, &trace_id(2)).await,
            StatusCode::OK
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn drop_hook_logs_once() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let (doomed, _key) = app.add_project(&seeded, "doomed", None).await;
        let mut new = new_row(&app, &doomed, DeletionKind::Project);
        new.request = RequestDetails {
            name: Some("doomed".to_owned()),
            slug: Some("doomed".to_owned()),
            ..RequestDetails::default()
        };
        insert(&app, new, &[]).await;

        let ((), logs) = capture_logs(async { worker(&app).await }).await;
        let lines = logs
            .lines()
            .filter(|line| line.contains("audit") && line.contains("project dropped"))
            .count();
        assert_eq!(lines, 1, "{logs}");

        let ((), again) = capture_logs(async { worker(&app).await }).await;
        assert!(!again.contains("project dropped"), "{again}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn org_drop_includes_projects_created_late() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let (p1, _) = app.add_project(&seeded, "p1", None).await;
        let mut new = new_row(&app, &p1, DeletionKind::Org);
        new.request = RequestDetails {
            name: Some("Acme".to_owned()),
            slug: Some("acme".to_owned()),
            ..RequestDetails::default()
        };
        let (id, p1_id) = (new.id, p1.id);
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    insert_deletion(conn, &new).await?;
                    insert_projects(conn, id, &[p1_id]).await
                })
            })
            .await
            .unwrap();
        // A project created after the request, as a racing `create_project` would.
        let (p2, _) = app.add_project(&seeded, "p2", None).await;

        worker(&app).await;

        assert_eq!(deletion_row(&app, id).await.state, DeletionState::Applied);
        for project in [p1.id, p2.id] {
            assert!(
                project_by_id(app.state.meta.reader(), project)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        let mut span = dataset_span(0);
        span.project_id = p2.id;
        let batch = spans_to_record_batch(&[span]).unwrap();
        let result = app.state.storage.append_spans(p2.id, batch).await;
        assert!(matches!(result, Err(AppendError::ProjectDropped)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn org_drop_removes_the_org_and_its_usage() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let now = app.state.clock.now_ns();
        let (globex, globex_p1, globex_p2) = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    let org = insert_org(conn, "Globex", "globex", now).await?;
                    let p1 = insert_project(conn, org.id, "One", "one", None, now).await?;
                    let p2 = insert_project(conn, org.id, "Two", "two", None, now).await?;
                    Ok((org.id, p1, p2))
                })
            })
            .await
            .unwrap();
        commit(&app, &globex_p1, &trace_id(1), &span_id(1), |_| {}).await;
        commit(&app, &globex_p2, &trace_id(2), &span_id(2), |_| {}).await;
        commit(&app, &seeded.project, &trace_id(3), &span_id(3), |_| {}).await;
        app.state.usage.flush(&app.state.meta).await.unwrap();
        assert!(
            count_rows(
                &app,
                "SELECT COUNT(*) FROM usage_daily WHERE org_id = ?",
                globex.to_string()
            )
            .await
                > 0
        );
        app.state.clock.advance(Duration::from_secs(1));

        let mut new = new_row(&app, &globex_p1, DeletionKind::Org);
        new.request = RequestDetails {
            name: Some("Globex".to_owned()),
            slug: Some("globex".to_owned()),
            ..RequestDetails::default()
        };
        let (id, projects) = (new.id, vec![globex_p1.id, globex_p2.id]);
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    insert_deletion(conn, &new).await?;
                    insert_projects(conn, id, &projects).await
                })
            })
            .await
            .unwrap();
        worker(&app).await;

        assert_eq!(deletion_row(&app, id).await.state, DeletionState::Applied);
        assert!(
            org_by_id(app.state.meta.reader(), globex)
                .await
                .unwrap()
                .is_none()
        );
        for project in [globex_p1.id, globex_p2.id] {
            assert!(
                project_by_id(app.state.meta.reader(), project)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(
            count_rows(
                &app,
                "SELECT COUNT(*) FROM usage_daily WHERE org_id = ?",
                globex.to_string()
            )
            .await,
            0
        );
        assert!(
            count_rows(
                &app,
                "SELECT COUNT(*) FROM usage_daily WHERE org_id = ?",
                seeded.org_id.to_string()
            )
            .await
                > 0
        );
    }

    #[test]
    fn failed_steps_wait_and_then_fail() {
        let deletions = Deletions::new();
        let id = Uuid::from_u128(1);
        assert!(deletions.due(id, 0));
        assert_eq!(deletions.failed(id, 0), 1);
        assert!(!deletions.due(id, RETRY_NS - 1));
        assert!(deletions.due(id, RETRY_NS));
        for expected in 2..=5 {
            assert_eq!(deletions.failed(id, 0), expected);
        }
        deletions.settled(id);
        assert!(deletions.due(id, 0));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn audit_logs_carry_no_scope() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = &seeded.project;
        let trace = trace_id(255);
        commit(&app, project, &trace, &span_id(255), |row| {
            row.user_id = Some("user-secret-7".to_owned());
            row.session_id = Some("session-secret-9".to_owned());
        })
        .await;
        app.state.clock.advance(Duration::from_secs(1));

        let mut new = new_row(&app, project, DeletionKind::User);
        new.user_id = Some("user-secret-7".to_owned());
        insert(&app, new, &[]).await;

        let ((), logs) = capture_logs(async { worker(&app).await }).await;
        assert!(
            logs.lines()
                .any(|line| line.contains("audit") && line.contains("deletion registered")),
            "{logs}"
        );
        for needle in ["user-secret-7", "session-secret-9", trace.as_str()] {
            assert!(!logs.contains(needle), "logs contain a scope value: {logs}");
        }
    }
}
