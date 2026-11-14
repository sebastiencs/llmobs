//! `deletions` rows and their scope tables (spec 04 § Deletion requests, D-M-8).
//!
//! The rows outlive their org and project: there are no foreign keys to
//! `orgs` or `projects`, and nothing here deletes a row.

use llmobs_core::ids::{OrgId, ProjectId, TraceId};
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::{MetaError, unique_or};

/// Rows per `INSERT` in the scope helpers, to stay under SQLite's variable limit.
const CHUNK: usize = 500;

/// Columns of every `SELECT` over `deletions`, in [`DeletionRecord`] order.
const SELECT_COLUMNS: &str = "id, org_id, project_id, kind, user_id, request, requested_by_kind, \
     requested_by, status, trace_count, session_count, created_at, handed_over_at, applied_at, error";

/// What a deletion covers (D-M-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DeletionKind {
    /// Listed trace ids.
    TraceIds,
    /// Traces matching a filter, search and window.
    Filter,
    /// One end user's data.
    User,
    /// A whole project.
    Project,
    /// A whole org.
    Org,
}

impl DeletionKind {
    /// The stored text.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::TraceIds => "trace_ids",
            Self::Filter => "filter",
            Self::User => "user",
            Self::Project => "project",
            Self::Org => "org",
        }
    }

    /// Parses the stored text. Unknown text is `None`.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "trace_ids" => Self::TraceIds,
            "filter" => Self::Filter,
            "user" => Self::User,
            "project" => Self::Project,
            "org" => Self::Org,
            _ => return None,
        })
    }
}

/// Where a deletion is in its lifecycle (D-M-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeletionState {
    /// The scope is not handed to storage yet.
    Resolving,
    /// Storage hides the rows; or a project/org drop not run yet.
    Pending,
    /// Done.
    Applied,
    /// Resolution gave up; nothing was deleted.
    Failed,
}

impl DeletionState {
    /// The stored text.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Resolving => "resolving",
            Self::Pending => "pending",
            Self::Applied => "applied",
            Self::Failed => "failed",
        }
    }

    /// Parses the stored text. Unknown text is `None`.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "resolving" => Self::Resolving,
            "pending" => Self::Pending,
            "applied" => Self::Applied,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

/// Who asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActorKind {
    /// A session or personal access token user.
    User,
    /// A project API key.
    ApiKey,
}

impl ActorKind {
    /// The stored text.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::ApiKey => "api_key",
        }
    }

    /// Parses the stored text. Unknown text is `None`.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "user" => Self::User,
            "api_key" => Self::ApiKey,
            _ => return None,
        })
    }
}

/// Why a deletion failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailReason {
    /// A filter matched more than 100,000 traces at resolution.
    TooManyTraces,
    /// Resolution failed 5 times in a row, or could not succeed on retry.
    ResolutionFailed,
}

impl FailReason {
    /// The stored text.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::TooManyTraces => "too_many_traces",
            Self::ResolutionFailed => "resolution_failed",
        }
    }

    /// Parses the stored text. Unknown text is `None`.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "too_many_traces" => Self::TooManyTraces,
            "resolution_failed" => Self::ResolutionFailed,
            _ => return None,
        })
    }
}

/// The request details kept for audit, as the `request` column's JSON.
///
/// Every field is optional; each kind sets its own:
/// - `trace_ids`: `requested` (distinct ids in the request);
/// - `filter`: `filter`, `search` (each only when non-empty), `from`, `to`,
///   `include_eval`, `include_experiments`;
/// - `user`: nothing;
/// - `project`, `org`: `name`, `slug`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestDetails {
    /// Distinct trace ids in a `trace_ids` request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) requested: Option<u32>,
    /// The filter DSL text of a `filter` request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) filter: Option<String>,
    /// The search text of a `filter` request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) search: Option<String>,
    /// Window start, ns, inclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) from: Option<i64>,
    /// Window end, ns, exclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) to: Option<i64>,
    /// Whether evaluation traces were included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) include_eval: Option<bool>,
    /// Whether experiment traces were included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) include_experiments: Option<bool>,
    /// Dropped project or org name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    /// Dropped project or org slug.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) slug: Option<String>,
}

/// One `deletions` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeletionRow {
    /// UUIDv7.
    pub(crate) id: Uuid,
    /// Owning org.
    pub(crate) org_id: OrgId,
    /// `None` for `org`.
    pub(crate) project_id: Option<ProjectId>,
    /// What the deletion covers.
    pub(crate) kind: DeletionKind,
    /// `Some` for `user` only.
    pub(crate) user_id: Option<String>,
    /// Audit details of the request.
    pub(crate) request: RequestDetails,
    /// Kind of the actor who asked.
    pub(crate) actor_kind: ActorKind,
    /// `users.id` or `api_keys.id`, per `actor_kind`.
    pub(crate) actor_id: Uuid,
    /// Lifecycle state.
    pub(crate) state: DeletionState,
    /// Traces in the stored scope; `None` until resolution stored it.
    pub(crate) trace_count: Option<u64>,
    /// Sessions in the stored scope (`user` only); `None` until then.
    pub(crate) session_count: Option<u64>,
    /// Cut-off, ns.
    pub(crate) created_at: i64,
    /// When storage registered the deletion, ns.
    pub(crate) handed_over_at: Option<i64>,
    /// When storage reported it applied, ns.
    pub(crate) applied_at: Option<i64>,
    /// Why it failed, when `state` is `Failed`.
    pub(crate) error: Option<FailReason>,
}

/// Columns of [`insert_deletion`].
#[derive(Debug, Clone)]
pub(crate) struct NewDeletion {
    /// UUIDv7.
    pub(crate) id: Uuid,
    /// Owning org.
    pub(crate) org_id: OrgId,
    /// `None` for `org`.
    pub(crate) project_id: Option<ProjectId>,
    /// What the deletion covers.
    pub(crate) kind: DeletionKind,
    /// `Some` for `user` only.
    pub(crate) user_id: Option<String>,
    /// Audit details of the request.
    pub(crate) request: RequestDetails,
    /// Kind of the actor who asked.
    pub(crate) actor_kind: ActorKind,
    /// `users.id` or `api_keys.id`, per `actor_kind`.
    pub(crate) actor_id: Uuid,
    /// `Resolving` for `trace_ids`/`filter`/`user`, `Pending` for `project`/`org`.
    pub(crate) state: DeletionState,
    /// Cut-off, ns.
    pub(crate) created_at: i64,
}

/// The deletion guard's view of the recent deletions (M3, D-M-6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GuardEntries {
    /// `(project, trace, created_at)` of every stored scope trace.
    pub(crate) traces: Vec<(ProjectId, TraceId, i64)>,
    /// `(project, user_id, created_at)` of every `user` deletion.
    pub(crate) users: Vec<(ProjectId, String, i64)>,
}

/// The private record that `sqlx` decodes. Field names are the column names.
#[derive(Debug, sqlx::FromRow)]
struct DeletionRecord {
    id: String,
    org_id: String,
    project_id: Option<String>,
    kind: String,
    user_id: Option<String>,
    request: String,
    requested_by_kind: String,
    requested_by: String,
    status: String,
    trace_count: Option<i64>,
    session_count: Option<i64>,
    created_at: i64,
    handed_over_at: Option<i64>,
    applied_at: Option<i64>,
    error: Option<String>,
}

fn corrupt(column: &'static str) -> MetaError {
    MetaError::Corrupt {
        table: "deletions",
        column,
    }
}

/// Logs a stored row that does not decode: its id when it parses, and the error. Only the
/// id and the error kind are logged, never the row's values.
pub(crate) fn log_undecodable(id: Option<Uuid>, error: &MetaError) {
    let id = id.map_or_else(|| "unknown".to_owned(), |id| id.to_string());
    tracing::error!(deletion_id = %id, error = %error, "deletion row does not decode");
}

fn row_from(record: DeletionRecord) -> Result<DeletionRow, MetaError> {
    let id = Uuid::parse_str(&record.id).map_err(|_| corrupt("id"))?;
    let org_id = record
        .org_id
        .parse::<OrgId>()
        .map_err(|_| corrupt("org_id"))?;
    let kind = DeletionKind::parse(&record.kind).ok_or_else(|| corrupt("kind"))?;
    let project_id = record
        .project_id
        .as_deref()
        .map(str::parse::<ProjectId>)
        .transpose()
        .map_err(|_| corrupt("project_id"))?;
    // A project id is present for every kind but `org`.
    if project_id.is_some() == (kind == DeletionKind::Org) {
        return Err(corrupt("project_id"));
    }
    // A user id is present for `user` only.
    if record.user_id.is_some() != (kind == DeletionKind::User) {
        return Err(corrupt("user_id"));
    }
    let request =
        serde_json::from_str::<RequestDetails>(&record.request).map_err(|_| corrupt("request"))?;
    let actor_kind =
        ActorKind::parse(&record.requested_by_kind).ok_or_else(|| corrupt("requested_by_kind"))?;
    let actor_id = Uuid::parse_str(&record.requested_by).map_err(|_| corrupt("requested_by"))?;
    let state = DeletionState::parse(&record.status).ok_or_else(|| corrupt("status"))?;
    let trace_count = record
        .trace_count
        .map(u64::try_from)
        .transpose()
        .map_err(|_| corrupt("trace_count"))?;
    let session_count = record
        .session_count
        .map(u64::try_from)
        .transpose()
        .map_err(|_| corrupt("session_count"))?;
    let error = record
        .error
        .as_deref()
        .map(FailReason::parse)
        .map(|reason| reason.ok_or_else(|| corrupt("error")))
        .transpose()?;
    Ok(DeletionRow {
        id,
        org_id,
        project_id,
        kind,
        user_id: record.user_id,
        request,
        actor_kind,
        actor_id,
        state,
        trace_count,
        session_count,
        created_at: record.created_at,
        handed_over_at: record.handed_over_at,
        applied_at: record.applied_at,
        error,
    })
}

/// Inserts a deletion. A duplicate id is `MetaError::Conflict("duplicate deletion")`.
///
/// # Errors
///
/// Returns [`MetaError`] when the request does not encode or the insert fails.
pub(crate) async fn insert_deletion(
    conn: &mut SqliteConnection,
    new: &NewDeletion,
) -> Result<DeletionRow, MetaError> {
    let request = serde_json::to_string(&new.request).map_err(|_| corrupt("request"))?;
    sqlx::query(
        "INSERT INTO deletions \
         (id, org_id, project_id, kind, user_id, request, requested_by_kind, requested_by, \
         status, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(new.id.to_string())
    .bind(new.org_id.to_string())
    .bind(new.project_id.map(|project| project.to_string()))
    .bind(new.kind.as_str())
    .bind(&new.user_id)
    .bind(&request)
    .bind(new.actor_kind.as_str())
    .bind(new.actor_id.to_string())
    .bind(new.state.as_str())
    .bind(new.created_at)
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, "duplicate deletion"))?;
    Ok(DeletionRow {
        id: new.id,
        org_id: new.org_id,
        project_id: new.project_id,
        kind: new.kind,
        user_id: new.user_id.clone(),
        request: new.request.clone(),
        actor_kind: new.actor_kind,
        actor_id: new.actor_id,
        state: new.state,
        trace_count: None,
        session_count: None,
        created_at: new.created_at,
        handed_over_at: None,
        applied_at: None,
        error: None,
    })
}

/// Adds trace ids to the scope. Ids already present are ignored.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the insert fails, including an unknown deletion id.
pub(crate) async fn insert_traces(
    conn: &mut SqliteConnection,
    id: Uuid,
    traces: &[TraceId],
) -> Result<(), MetaError> {
    for chunk in traces.chunks(CHUNK) {
        let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
            "INSERT OR IGNORE INTO deletion_traces (deletion_id, trace_id) ",
        );
        builder.push_values(chunk, |mut row, trace| {
            row.push_bind(id.to_string())
                .push_bind(trace.as_bytes().to_vec());
        });
        builder.build().execute(&mut *conn).await?;
    }
    Ok(())
}

/// Adds session ids to the scope. Ids already present are ignored.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the insert fails, including an unknown deletion id.
pub(crate) async fn insert_sessions(
    conn: &mut SqliteConnection,
    id: Uuid,
    sessions: &[String],
) -> Result<(), MetaError> {
    for chunk in sessions.chunks(CHUNK) {
        let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
            "INSERT OR IGNORE INTO deletion_sessions (deletion_id, session_id) ",
        );
        builder.push_values(chunk, |mut row, session| {
            row.push_bind(id.to_string()).push_bind(session.as_str());
        });
        builder.build().execute(&mut *conn).await?;
    }
    Ok(())
}

/// Adds project ids to the scope. Ids already present are ignored.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the insert fails, including an unknown deletion id.
pub(crate) async fn insert_projects(
    conn: &mut SqliteConnection,
    id: Uuid,
    projects: &[ProjectId],
) -> Result<(), MetaError> {
    for chunk in projects.chunks(CHUNK) {
        let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
            "INSERT OR IGNORE INTO deletion_projects (deletion_id, project_id) ",
        );
        builder.push_values(chunk, |mut row, project| {
            row.push_bind(id.to_string()).push_bind(project.to_string());
        });
        builder.build().execute(&mut *conn).await?;
    }
    Ok(())
}

/// Stores the resolved counts. Only a `resolving` row changes; returns whether one did.
///
/// # Errors
///
/// Returns [`MetaError`] when a count does not fit `i64` or the update fails.
pub(crate) async fn store_counts(
    conn: &mut SqliteConnection,
    id: Uuid,
    trace_count: u64,
    session_count: Option<u64>,
) -> Result<bool, MetaError> {
    let traces = i64::try_from(trace_count).map_err(|_| corrupt("trace_count"))?;
    let sessions = session_count
        .map(i64::try_from)
        .transpose()
        .map_err(|_| corrupt("session_count"))?;
    let result = sqlx::query(
        "UPDATE deletions SET trace_count = ?, session_count = ? \
         WHERE id = ? AND status = 'resolving'",
    )
    .bind(traces)
    .bind(sessions)
    .bind(id.to_string())
    .execute(&mut *conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// `resolving` → `pending`, or a `pending` row whose hand-over was cleared; sets `handed_over_at`.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn mark_handed_over(
    conn: &mut SqliteConnection,
    id: Uuid,
    now: i64,
) -> Result<bool, MetaError> {
    let result = sqlx::query(
        "UPDATE deletions SET status = 'pending', handed_over_at = ? \
         WHERE id = ? AND (status = 'resolving' OR (status = 'pending' AND handed_over_at IS NULL))",
    )
    .bind(now)
    .bind(id.to_string())
    .execute(&mut *conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// `resolving` or `pending` → `applied` at `applied_at`. `handed_over_at` is kept as is.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn mark_applied(
    conn: &mut SqliteConnection,
    id: Uuid,
    applied_at: i64,
) -> Result<bool, MetaError> {
    let result = sqlx::query(
        "UPDATE deletions SET status = 'applied', applied_at = ? \
         WHERE id = ? AND status IN ('resolving', 'pending')",
    )
    .bind(applied_at)
    .bind(id.to_string())
    .execute(&mut *conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// A `pending` project or org deletion that ran: `applied`, `handed_over_at = applied_at = now`.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn mark_dropped(
    conn: &mut SqliteConnection,
    id: Uuid,
    now: i64,
) -> Result<bool, MetaError> {
    let result = sqlx::query(
        "UPDATE deletions SET status = 'applied', handed_over_at = ?, applied_at = ? \
         WHERE id = ? AND status = 'pending' AND kind IN ('project', 'org')",
    )
    .bind(now)
    .bind(now)
    .bind(id.to_string())
    .execute(&mut *conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// `resolving` → `failed` with `reason`.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn mark_failed(
    conn: &mut SqliteConnection,
    id: Uuid,
    reason: FailReason,
) -> Result<bool, MetaError> {
    let result = sqlx::query(
        "UPDATE deletions SET status = 'failed', error = ? WHERE id = ? AND status = 'resolving'",
    )
    .bind(reason.as_str())
    .bind(id.to_string())
    .execute(&mut *conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// A `pending` row's `handed_over_at` back to NULL (storage lost it; M3 registers it again).
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn clear_handover(
    conn: &mut SqliteConnection,
    id: Uuid,
) -> Result<bool, MetaError> {
    let result = sqlx::query(
        "UPDATE deletions SET handed_over_at = NULL WHERE id = ? AND status = 'pending'",
    )
    .bind(id.to_string())
    .execute(&mut *conn)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Open deletions of `project`, read inside a write transaction: rows in `resolving` or
/// `pending`. `org` rows have no `project_id` and never count (the cap is per project, D-M-5).
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or the count does not fit `u64`.
pub(crate) async fn count_open_in_project(
    conn: &mut SqliteConnection,
    project: ProjectId,
) -> Result<u64, MetaError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM deletions WHERE project_id = ? AND status IN ('resolving', 'pending')",
    )
    .bind(project.to_string())
    .fetch_one(&mut *conn)
    .await?;
    u64::try_from(count).map_err(|_| corrupt("status"))
}

/// One row by id, in any project.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or the stored row does not decode.
pub(crate) async fn deletion_by_id(
    pool: &SqlitePool,
    id: Uuid,
) -> Result<Option<DeletionRow>, MetaError> {
    // The SQL is built only from constants.
    let sql = format!("SELECT {SELECT_COLUMNS} FROM deletions WHERE id = ?");
    let record: Option<DeletionRecord> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(id.to_string())
        .fetch_optional(pool)
        .await?;
    record.map(row_from).transpose()
}

/// One row by id when it belongs to `project`; `None` otherwise (the route answers 404).
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or the stored row does not decode.
pub(crate) async fn deletion_in_project(
    pool: &SqlitePool,
    project: ProjectId,
    id: Uuid,
) -> Result<Option<DeletionRow>, MetaError> {
    // The SQL is built only from constants.
    let sql = format!("SELECT {SELECT_COLUMNS} FROM deletions WHERE id = ? AND project_id = ?");
    let record: Option<DeletionRecord> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(id.to_string())
        .bind(project.to_string())
        .fetch_optional(pool)
        .await?;
    record.map(row_from).transpose()
}

/// Newest first, `ORDER BY created_at DESC, id DESC`. `after` is the last row of the previous
/// page as `(created_at, id)`; rows strictly after it in that order come next. At most `limit`.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored row does not decode.
pub(crate) async fn page_of_project(
    pool: &SqlitePool,
    project: ProjectId,
    after: Option<(i64, Uuid)>,
    limit: u32,
) -> Result<Vec<DeletionRow>, MetaError> {
    page(pool, "project_id", project.to_string(), after, limit).await
}

/// Same over every row of `org` (all kinds, including dropped projects' rows).
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored row does not decode.
pub(crate) async fn page_of_org(
    pool: &SqlitePool,
    org: OrgId,
    after: Option<(i64, Uuid)>,
    limit: u32,
) -> Result<Vec<DeletionRow>, MetaError> {
    page(pool, "org_id", org.to_string(), after, limit).await
}

async fn page(
    pool: &SqlitePool,
    column: &'static str,
    scope: String,
    after: Option<(i64, Uuid)>,
    limit: u32,
) -> Result<Vec<DeletionRow>, MetaError> {
    let cursor = if after.is_some() {
        " AND (created_at < ? OR (created_at = ? AND id < ?))"
    } else {
        ""
    };
    // The SQL is built only from constants (`column` is one of two literals, `cursor` is constant).
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM deletions WHERE {column} = ?{cursor} \
         ORDER BY created_at DESC, id DESC LIMIT ?"
    );
    let mut query = sqlx::query_as::<_, DeletionRecord>(sqlx::AssertSqlSafe(sql)).bind(scope);
    if let Some((created_at, id)) = after {
        query = query.bind(created_at).bind(created_at).bind(id.to_string());
    }
    let records = query.bind(i64::from(limit)).fetch_all(pool).await?;
    records.into_iter().map(row_from).collect()
}

/// Every `resolving` or `pending` row, oldest first (`created_at, id`). The worker's input.
/// Each row decodes on its own: a row that does not decode is `Err((its id, the error))` and
/// the others are still listed, so one corrupt row cannot stop the worker.
///
/// # Errors
///
/// Returns [`MetaError`] when the query itself fails.
pub(crate) async fn open_deletions(
    pool: &SqlitePool,
) -> Result<Vec<Result<DeletionRow, (Option<Uuid>, MetaError)>>, MetaError> {
    // The SQL is built only from constants.
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM deletions WHERE status IN ('resolving', 'pending') \
         ORDER BY created_at, id"
    );
    let records: Vec<DeletionRecord> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .fetch_all(pool)
        .await?;
    Ok(records
        .into_iter()
        .map(|record| {
            let id = Uuid::parse_str(&record.id).ok();
            row_from(record).map_err(|error| (id, error))
        })
        .collect())
}

/// The stored trace scope, ordered by trace id.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored id is not a trace id.
pub(crate) async fn scope_traces(pool: &SqlitePool, id: Uuid) -> Result<Vec<TraceId>, MetaError> {
    let blobs: Vec<Vec<u8>> = sqlx::query_scalar(
        "SELECT trace_id FROM deletion_traces WHERE deletion_id = ? ORDER BY trace_id",
    )
    .bind(id.to_string())
    .fetch_all(pool)
    .await?;
    blobs
        .iter()
        .map(|blob| {
            TraceId::from_slice(blob).map_err(|_| MetaError::Corrupt {
                table: "deletion_traces",
                column: "trace_id",
            })
        })
        .collect()
}

/// The stored session scope, ordered by session id.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the query fails.
pub(crate) async fn scope_sessions(pool: &SqlitePool, id: Uuid) -> Result<Vec<String>, MetaError> {
    let sessions: Vec<String> = sqlx::query_scalar(
        "SELECT session_id FROM deletion_sessions WHERE deletion_id = ? ORDER BY session_id",
    )
    .bind(id.to_string())
    .fetch_all(pool)
    .await?;
    Ok(sessions)
}

/// The stored project scope, ordered by project id.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored id is not a project id.
pub(crate) async fn scope_projects(
    pool: &SqlitePool,
    id: Uuid,
) -> Result<Vec<ProjectId>, MetaError> {
    let texts: Vec<String> = sqlx::query_scalar(
        "SELECT project_id FROM deletion_projects WHERE deletion_id = ? ORDER BY project_id",
    )
    .bind(id.to_string())
    .fetch_all(pool)
    .await?;
    texts
        .iter()
        .map(|text| {
            text.parse::<ProjectId>().map_err(|_| MetaError::Corrupt {
                table: "deletion_projects",
                column: "project_id",
            })
        })
        .collect()
}

/// Rows the deletion guard needs: deletions created at or after `since`.
/// - traces: the `deletion_traces` of rows of kind `trace_ids`, `filter` or `user`;
/// - users: `user_id` of rows of kind `user`.
///
/// Rows in any status, `failed` included: their entries do no harm and expire.
/// Each row decodes on its own: a value that does not decode is logged (see
/// [`log_undecodable`]) and its entry is skipped; the other entries are kept.
///
/// # Errors
///
/// Returns [`MetaError`] when the query itself fails.
pub(crate) async fn guard_entries(
    pool: &SqlitePool,
    since: i64,
) -> Result<GuardEntries, MetaError> {
    let trace_rows: Vec<(String, Option<String>, Vec<u8>, i64)> = sqlx::query_as(
        "SELECT d.id, d.project_id, t.trace_id, d.created_at \
         FROM deletions d JOIN deletion_traces t ON t.deletion_id = d.id \
         WHERE d.created_at >= ? AND d.kind IN ('trace_ids', 'filter', 'user') \
         ORDER BY d.created_at, d.id, t.trace_id",
    )
    .bind(since)
    .fetch_all(pool)
    .await?;
    let mut traces = Vec::with_capacity(trace_rows.len());
    for (id, project, blob, created_at) in trace_rows {
        match guard_trace(project, &blob) {
            Ok((project, trace)) => traces.push((project, trace, created_at)),
            Err(error) => log_undecodable(Uuid::parse_str(&id).ok(), &error),
        }
    }

    let user_rows: Vec<(String, Option<String>, Option<String>, i64)> = sqlx::query_as(
        "SELECT id, project_id, user_id, created_at FROM deletions \
         WHERE created_at >= ? AND kind = 'user' ORDER BY created_at, id",
    )
    .bind(since)
    .fetch_all(pool)
    .await?;
    let mut users = Vec::with_capacity(user_rows.len());
    for (id, project, user, created_at) in user_rows {
        match guard_user(project, user) {
            Ok((project, user)) => users.push((project, user, created_at)),
            Err(error) => log_undecodable(Uuid::parse_str(&id).ok(), &error),
        }
    }
    Ok(GuardEntries { traces, users })
}

/// The project and trace of one guard trace entry.
fn guard_trace(project: Option<String>, blob: &[u8]) -> Result<(ProjectId, TraceId), MetaError> {
    let project = guard_project(project)?;
    let trace = TraceId::from_slice(blob).map_err(|_| MetaError::Corrupt {
        table: "deletion_traces",
        column: "trace_id",
    })?;
    Ok((project, trace))
}

/// The project and end user of one guard user entry.
fn guard_user(
    project: Option<String>,
    user: Option<String>,
) -> Result<(ProjectId, String), MetaError> {
    let project = guard_project(project)?;
    let user = user.ok_or_else(|| corrupt("user_id"))?;
    Ok((project, user))
}

fn guard_project(text: Option<String>) -> Result<ProjectId, MetaError> {
    text.ok_or_else(|| corrupt("project_id"))?
        .parse::<ProjectId>()
        .map_err(|_| corrupt("project_id"))
}

#[cfg(test)]
mod tests {
    use llmobs_core::ids::{OrgId, ProjectId, TraceId};
    use uuid::Uuid;

    use super::*;
    use crate::meta::{MetaDb, orgs, projects, test_db};

    const ORG: &str = "01900000-0000-7000-8000-000000000010";
    const PROJECT_A: &str = "01900000-0000-7000-8000-000000000001";
    const PROJECT_B: &str = "01900000-0000-7000-8000-000000000002";
    /// Text of `Uuid::from_u128(1)`, the row the corruption tests change.
    const ROW1: &str = "00000000-0000-0000-0000-000000000001";

    fn org() -> OrgId {
        ORG.parse().unwrap()
    }

    fn project_a() -> ProjectId {
        PROJECT_A.parse().unwrap()
    }

    fn project_b() -> ProjectId {
        PROJECT_B.parse().unwrap()
    }

    fn trace(n: u128) -> TraceId {
        TraceId::from_bytes((n + 1).to_be_bytes()).unwrap()
    }

    fn new(n: u128, kind: DeletionKind, project: ProjectId, created_at: i64) -> NewDeletion {
        NewDeletion {
            id: Uuid::from_u128(n),
            org_id: org(),
            project_id: (kind != DeletionKind::Org).then_some(project),
            kind,
            user_id: (kind == DeletionKind::User).then(|| "user-1".to_owned()),
            request: RequestDetails::default(),
            actor_kind: ActorKind::User,
            actor_id: Uuid::from_u128(1000 + n),
            state: match kind {
                DeletionKind::TraceIds | DeletionKind::Filter | DeletionKind::User => {
                    DeletionState::Resolving
                }
                DeletionKind::Project | DeletionKind::Org => DeletionState::Pending,
            },
            created_at,
        }
    }

    async fn insert(db: &MetaDb, new: NewDeletion) -> DeletionRow {
        db.write(move |conn| Box::pin(async move { insert_deletion(conn, &new).await }))
            .await
            .unwrap()
    }

    async fn add_traces(db: &MetaDb, id: Uuid, traces: Vec<TraceId>) {
        db.write(move |conn| Box::pin(async move { insert_traces(conn, id, &traces).await }))
            .await
            .unwrap();
    }

    async fn handover(db: &MetaDb, id: Uuid, now: i64) -> bool {
        db.write(move |conn| Box::pin(async move { mark_handed_over(conn, id, now).await }))
            .await
            .unwrap()
    }

    async fn clear(db: &MetaDb, id: Uuid) -> bool {
        db.write(move |conn| Box::pin(async move { clear_handover(conn, id).await }))
            .await
            .unwrap()
    }

    async fn applied(db: &MetaDb, id: Uuid, at: i64) -> bool {
        db.write(move |conn| Box::pin(async move { mark_applied(conn, id, at).await }))
            .await
            .unwrap()
    }

    async fn failed(db: &MetaDb, id: Uuid, reason: FailReason) -> bool {
        db.write(move |conn| Box::pin(async move { mark_failed(conn, id, reason).await }))
            .await
            .unwrap()
    }

    async fn counts(
        db: &MetaDb,
        id: Uuid,
        traces: u64,
        sessions: Option<u64>,
    ) -> Result<bool, MetaError> {
        db.write(move |conn| {
            Box::pin(async move { store_counts(conn, id, traces, sessions).await })
        })
        .await
    }

    async fn state(db: &MetaDb, id: Uuid) -> DeletionRow {
        deletion_by_id(db.reader(), id).await.unwrap().unwrap()
    }

    /// Runs one statement on the writer, with literal values.
    async fn raw(db: &MetaDb, sql: String) {
        db.write(move |conn| {
            Box::pin(async move {
                sqlx::query(sqlx::AssertSqlSafe(sql))
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
    }

    async fn count(db: &MetaDb, sql: String) -> i64 {
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
            .fetch_one(db.reader())
            .await
            .unwrap()
    }

    fn corrupt_of<T: std::fmt::Debug>(
        result: Result<T, MetaError>,
    ) -> (&'static str, &'static str) {
        match result {
            Err(MetaError::Corrupt { table, column }) => (table, column),
            other => panic!("expected corrupt, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn insert_and_read_back_every_kind() {
        let (_dir, db) = test_db().await;
        let mut trace_ids = new(1, DeletionKind::TraceIds, project_a(), 10);
        trace_ids.request.requested = Some(3);
        let mut filter = new(2, DeletionKind::Filter, project_a(), 20);
        filter.request = RequestDetails {
            filter: Some("status = 'error'".to_owned()),
            search: Some("refund".to_owned()),
            from: Some(1),
            to: Some(2),
            include_eval: Some(true),
            include_experiments: Some(false),
            ..RequestDetails::default()
        };
        let user = new(3, DeletionKind::User, project_a(), 30);
        let mut project = new(4, DeletionKind::Project, project_a(), 40);
        project.request = RequestDetails {
            name: Some("Bot".to_owned()),
            slug: Some("bot".to_owned()),
            ..RequestDetails::default()
        };
        let mut org_row = new(5, DeletionKind::Org, project_a(), 50);
        org_row.request = RequestDetails {
            name: Some("Acme".to_owned()),
            slug: Some("acme".to_owned()),
            ..RequestDetails::default()
        };

        for new in [trace_ids, filter, user, project, org_row] {
            let inserted = insert(&db, new.clone()).await;
            let read = deletion_by_id(db.reader(), new.id).await.unwrap().unwrap();
            assert_eq!(read, inserted);
            assert_eq!(read.kind, new.kind);
            assert_eq!(read.request, new.request);
            assert_eq!(read.trace_count, None);
            assert_eq!(read.session_count, None);
            assert_eq!(read.handed_over_at, None);
            assert_eq!(read.applied_at, None);
            assert_eq!(read.error, None);
        }
    }

    #[tokio::test]
    async fn duplicate_id_is_a_conflict() {
        let (_dir, db) = test_db().await;
        let row = new(1, DeletionKind::TraceIds, project_a(), 10);
        insert(&db, row.clone()).await;
        let error = db
            .write(move |conn| Box::pin(async move { insert_deletion(conn, &row).await }))
            .await
            .unwrap_err();
        assert!(
            matches!(error, MetaError::Conflict("duplicate deletion")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn scope_inserts_ignore_duplicates() {
        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::TraceIds, project_a(), 10)).await;
        let id = Uuid::from_u128(1);

        add_traces(&db, id, vec![trace(0), trace(1), trace(0)]).await;
        add_traces(&db, id, vec![trace(1), trace(2)]).await;
        let traces = scope_traces(db.reader(), id).await.unwrap();
        assert_eq!(traces, vec![trace(0), trace(1), trace(2)]);

        db.write(|conn| {
            Box::pin(async move {
                insert_sessions(conn, id, &["b".to_owned(), "a".to_owned(), "b".to_owned()])
                    .await?;
                insert_sessions(conn, id, &["c".to_owned()]).await?;
                insert_sessions(conn, id, &[]).await?;
                insert_traces(conn, id, &[]).await?;
                insert_projects(conn, id, &[project_b(), project_a()]).await?;
                insert_projects(conn, id, &[]).await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        let sessions = scope_sessions(db.reader(), id).await.unwrap();
        assert_eq!(sessions, vec!["a", "b", "c"]);
        let projects = scope_projects(db.reader(), id).await.unwrap();
        assert_eq!(projects, vec![project_a(), project_b()]);
    }

    #[tokio::test]
    async fn scope_inserts_chunk() {
        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::TraceIds, project_a(), 10)).await;
        let mut traces: Vec<TraceId> = (0..1234).map(trace).collect();
        traces.push(trace(0));
        traces.push(trace(600));
        add_traces(&db, Uuid::from_u128(1), traces).await;
        let scope = scope_traces(db.reader(), Uuid::from_u128(1)).await.unwrap();
        assert_eq!(scope.len(), 1234);
    }

    #[tokio::test]
    async fn scope_insert_for_an_unknown_deletion_fails() {
        let (_dir, db) = test_db().await;
        let error = db
            .write(|conn| {
                Box::pin(async move { insert_traces(conn, Uuid::from_u128(99), &[trace(0)]).await })
            })
            .await
            .unwrap_err();
        assert!(matches!(error, MetaError::Db(_)), "{error}");
    }

    #[tokio::test]
    async fn resolving_path() {
        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::TraceIds, project_a(), 10)).await;
        let id = Uuid::from_u128(1);

        assert!(!clear(&db, id).await);
        assert!(counts(&db, id, 3, None).await.unwrap());
        let row = state(&db, id).await;
        assert_eq!(row.trace_count, Some(3));
        assert_eq!(row.session_count, None);

        assert!(handover(&db, id, 50).await);
        let row = state(&db, id).await;
        assert_eq!(row.state, DeletionState::Pending);
        assert_eq!(row.handed_over_at, Some(50));

        assert!(!counts(&db, id, 4, None).await.unwrap());
        assert!(!handover(&db, id, 60).await);
        assert!(!failed(&db, id, FailReason::ResolutionFailed).await);
        assert_eq!(state(&db, id).await.handed_over_at, Some(50));

        assert!(clear(&db, id).await);
        assert_eq!(state(&db, id).await.handed_over_at, None);
        assert!(handover(&db, id, 70).await);

        assert!(applied(&db, id, 80).await);
        let row = state(&db, id).await;
        assert_eq!(row.state, DeletionState::Applied);
        assert_eq!(row.applied_at, Some(80));
        assert_eq!(row.handed_over_at, Some(70));

        assert!(!applied(&db, id, 90).await);
        assert!(!handover(&db, id, 100).await);
        assert!(!clear(&db, id).await);
    }

    #[tokio::test]
    async fn other_paths() {
        let (_dir, db) = test_db().await;

        // A filter goes straight from resolving to applied.
        insert(&db, new(1, DeletionKind::Filter, project_a(), 10)).await;
        let filter = Uuid::from_u128(1);
        assert!(counts(&db, filter, 0, None).await.unwrap());
        assert!(applied(&db, filter, 5).await);
        let row = state(&db, filter).await;
        assert_eq!(row.state, DeletionState::Applied);
        assert_eq!(row.handed_over_at, None);

        // A user row keeps its session count and can fail.
        insert(&db, new(2, DeletionKind::User, project_a(), 20)).await;
        let user = Uuid::from_u128(2);
        assert!(counts(&db, user, 2, Some(1)).await.unwrap());
        assert_eq!(state(&db, user).await.session_count, Some(1));
        assert!(failed(&db, user, FailReason::TooManyTraces).await);
        let row = state(&db, user).await;
        assert_eq!(row.error, Some(FailReason::TooManyTraces));
        assert!(!handover(&db, user, 30).await);

        // A project row starts pending and cannot fail.
        insert(&db, new(3, DeletionKind::Project, project_a(), 30)).await;
        let project = Uuid::from_u128(3);
        assert!(!failed(&db, project, FailReason::ResolutionFailed).await);
        assert!(handover(&db, project, 9).await);

        // A trace count that does not fit i64 is corrupt, not stored.
        insert(&db, new(4, DeletionKind::TraceIds, project_a(), 40)).await;
        let error = counts(&db, Uuid::from_u128(4), u64::MAX, None)
            .await
            .unwrap_err();
        assert_eq!(
            corrupt_of(Err::<(), _>(error)),
            ("deletions", "trace_count")
        );
    }

    #[tokio::test]
    async fn count_open_counts_resolving_and_pending_of_one_project() {
        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::Filter, project_a(), 10)).await;
        insert(&db, new(2, DeletionKind::Project, project_a(), 20)).await;
        insert(&db, new(3, DeletionKind::Filter, project_a(), 30)).await;
        insert(&db, new(4, DeletionKind::Filter, project_a(), 40)).await;
        assert!(applied(&db, Uuid::from_u128(3), 35).await);
        assert!(failed(&db, Uuid::from_u128(4), FailReason::ResolutionFailed).await);
        insert(&db, new(5, DeletionKind::Filter, project_b(), 50)).await;
        insert(&db, new(6, DeletionKind::Org, project_a(), 60)).await;

        let open_a = db
            .write(|conn| Box::pin(async move { count_open_in_project(conn, project_a()).await }))
            .await
            .unwrap();
        assert_eq!(open_a, 2);
        let open_b = db
            .write(|conn| Box::pin(async move { count_open_in_project(conn, project_b()).await }))
            .await
            .unwrap();
        assert_eq!(open_b, 1);
    }

    #[tokio::test]
    async fn pages_are_newest_first_with_a_cursor() {
        let (_dir, db) = test_db().await;
        let created = [10, 20, 20, 30, 40];
        for (index, at) in created.into_iter().enumerate() {
            insert(
                &db,
                new(index as u128 + 1, DeletionKind::TraceIds, project_a(), at),
            )
            .await;
        }
        insert(&db, new(6, DeletionKind::Org, project_a(), 50)).await;

        let ids = |rows: Vec<DeletionRow>| rows.into_iter().map(|row| row.id).collect::<Vec<_>>();
        let n = Uuid::from_u128;

        let first = page_of_project(db.reader(), project_a(), None, 2)
            .await
            .unwrap();
        assert_eq!(ids(first), vec![n(5), n(4)]);

        let second = page_of_project(db.reader(), project_a(), Some((30, n(4))), 2)
            .await
            .unwrap();
        assert_eq!(ids(second), vec![n(3), n(2)]);

        let third = page_of_project(db.reader(), project_a(), Some((20, n(2))), 2)
            .await
            .unwrap();
        assert_eq!(ids(third), vec![n(1)]);

        let past = page_of_project(db.reader(), project_a(), Some((10, n(1))), 2)
            .await
            .unwrap();
        assert!(past.is_empty());

        let all = page_of_org(db.reader(), org(), None, 10).await.unwrap();
        assert_eq!(ids(all), vec![n(6), n(5), n(4), n(3), n(2), n(1)]);
    }

    #[tokio::test]
    async fn open_deletions_are_oldest_first() {
        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::Filter, project_a(), 30)).await;
        insert(&db, new(2, DeletionKind::Project, project_a(), 10)).await;
        insert(&db, new(3, DeletionKind::Filter, project_a(), 20)).await;
        assert!(counts(&db, Uuid::from_u128(3), 0, None).await.unwrap());
        assert!(applied(&db, Uuid::from_u128(3), 25).await);
        insert(&db, new(4, DeletionKind::Filter, project_a(), 40)).await;
        assert!(failed(&db, Uuid::from_u128(4), FailReason::ResolutionFailed).await);

        let open = open_deletions(db.reader()).await.unwrap();
        let ids: Vec<Uuid> = open.iter().map(|row| row.as_ref().unwrap().id).collect();
        assert_eq!(ids, vec![Uuid::from_u128(2), Uuid::from_u128(1)]);
    }

    #[tokio::test]
    async fn open_deletions_keep_going_past_a_corrupt_row() {
        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::TraceIds, project_a(), 10)).await;
        insert(&db, new(2, DeletionKind::TraceIds, project_a(), 20)).await;
        raw(
            &db,
            format!("UPDATE deletions SET request = '{{\"bogus\":1}}' WHERE id = '{ROW1}'"),
        )
        .await;

        let mut open = open_deletions(db.reader()).await.unwrap();
        assert_eq!(open.len(), 2);
        let (id, error) = open.remove(0).unwrap_err();
        assert_eq!(id, Some(Uuid::from_u128(1)));
        assert_eq!(corrupt_of(Err::<(), _>(error)), ("deletions", "request"));
        assert_eq!(open[0].as_ref().unwrap().id, Uuid::from_u128(2));
    }

    #[tokio::test]
    async fn deletion_in_project_checks_the_project() {
        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::Filter, project_a(), 10)).await;
        let id = Uuid::from_u128(1);
        assert!(
            deletion_in_project(db.reader(), project_a(), id)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            deletion_in_project(db.reader(), project_b(), id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn guard_entries_since() {
        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::TraceIds, project_a(), 100)).await;
        add_traces(&db, Uuid::from_u128(1), vec![trace(0), trace(1)]).await;
        insert(&db, new(2, DeletionKind::User, project_a(), 200)).await;
        add_traces(&db, Uuid::from_u128(2), vec![trace(2)]).await;
        insert(&db, new(3, DeletionKind::Filter, project_a(), 50)).await;
        add_traces(&db, Uuid::from_u128(3), vec![trace(3)]).await;
        insert(&db, new(4, DeletionKind::Project, project_a(), 300)).await;
        add_traces(&db, Uuid::from_u128(4), vec![trace(4)]).await;

        let entries = guard_entries(db.reader(), 100).await.unwrap();
        assert_eq!(
            entries.traces,
            vec![
                (project_a(), trace(0), 100),
                (project_a(), trace(1), 100),
                (project_a(), trace(2), 200),
            ]
        );
        assert_eq!(entries.users, vec![(project_a(), "user-1".to_owned(), 200)]);
    }

    #[tokio::test]
    async fn rows_outlive_their_org_and_project() {
        let (_dir, db) = test_db().await;
        let (org_id, project_id) = db
            .write(|conn| {
                Box::pin(async move {
                    let org = orgs::insert_org(conn, "Acme", "acme", 1).await?;
                    let project =
                        projects::insert_project(conn, org.id, "Bot", "bot", None, 1).await?;
                    Ok((org.id, project.id))
                })
            })
            .await
            .unwrap();

        let mut trace_row = new(1, DeletionKind::TraceIds, project_id, 10);
        trace_row.org_id = org_id;
        insert(&db, trace_row).await;
        add_traces(&db, Uuid::from_u128(1), vec![trace(0)]).await;
        let mut org_row = new(2, DeletionKind::Org, project_id, 20);
        org_row.org_id = org_id;
        insert(&db, org_row).await;

        raw(
            &db,
            format!("DELETE FROM projects WHERE id = '{project_id}'"),
        )
        .await;
        raw(&db, format!("DELETE FROM orgs WHERE id = '{org_id}'")).await;

        assert!(
            deletion_by_id(db.reader(), Uuid::from_u128(1))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            deletion_by_id(db.reader(), Uuid::from_u128(2))
                .await
                .unwrap()
                .is_some()
        );
        let scope = scope_traces(db.reader(), Uuid::from_u128(1)).await.unwrap();
        assert_eq!(scope, vec![trace(0)]);
    }

    #[tokio::test]
    async fn scope_rows_go_with_their_deletion() {
        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::TraceIds, project_a(), 10)).await;
        let id = Uuid::from_u128(1);
        add_traces(&db, id, vec![trace(0)]).await;
        db.write(move |conn| {
            Box::pin(async move {
                insert_sessions(conn, id, &["s".to_owned()]).await?;
                insert_projects(conn, id, &[project_a()]).await?;
                Ok(())
            })
        })
        .await
        .unwrap();

        raw(&db, format!("DELETE FROM deletions WHERE id = '{ROW1}'")).await;
        for table in ["deletion_traces", "deletion_sessions", "deletion_projects"] {
            let left = count(&db, format!("SELECT COUNT(*) FROM {table}")).await;
            assert_eq!(left, 0, "{table}");
        }
    }

    #[tokio::test]
    async fn corrupt_values_are_reported() {
        // Each case changes one value of a fresh row with raw SQL.
        let cases: [(DeletionKind, &str, &str, &str); 6] = [
            (DeletionKind::TraceIds, "kind = 'nope'", "deletions", "kind"),
            (
                DeletionKind::TraceIds,
                "status = 'nope'",
                "deletions",
                "status",
            ),
            (
                DeletionKind::TraceIds,
                "requested_by_kind = 'robot'",
                "deletions",
                "requested_by_kind",
            ),
            (
                DeletionKind::TraceIds,
                "request = '{\"bogus\":1}'",
                "deletions",
                "request",
            ),
            (
                DeletionKind::TraceIds,
                "trace_count = -1",
                "deletions",
                "trace_count",
            ),
            (
                DeletionKind::TraceIds,
                "user_id = 'u'",
                "deletions",
                "user_id",
            ),
        ];
        for (kind, change, table, column) in cases {
            let (_dir, db) = test_db().await;
            insert(&db, new(1, kind, project_a(), 10)).await;
            raw(
                &db,
                format!("UPDATE deletions SET {change} WHERE id = '{ROW1}'"),
            )
            .await;
            let result = deletion_by_id(db.reader(), Uuid::from_u128(1)).await;
            assert_eq!(corrupt_of(result), (table, column), "{change}");
        }

        // Identity and nullability rules of kind, project and user.
        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::User, project_a(), 10)).await;
        raw(
            &db,
            format!("UPDATE deletions SET user_id = NULL WHERE id = '{ROW1}'"),
        )
        .await;
        let result = deletion_by_id(db.reader(), Uuid::from_u128(1)).await;
        assert_eq!(corrupt_of(result), ("deletions", "user_id"));

        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::Project, project_a(), 10)).await;
        raw(
            &db,
            format!("UPDATE deletions SET project_id = NULL WHERE id = '{ROW1}'"),
        )
        .await;
        let result = deletion_by_id(db.reader(), Uuid::from_u128(1)).await;
        assert_eq!(corrupt_of(result), ("deletions", "project_id"));

        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::Org, project_a(), 10)).await;
        raw(
            &db,
            format!("UPDATE deletions SET project_id = '{PROJECT_A}' WHERE id = '{ROW1}'"),
        )
        .await;
        let result = deletion_by_id(db.reader(), Uuid::from_u128(1)).await;
        assert_eq!(corrupt_of(result), ("deletions", "project_id"));

        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::TraceIds, project_a(), 10)).await;
        raw(
            &db,
            format!("UPDATE deletions SET id = 'x' WHERE id = '{ROW1}'"),
        )
        .await;
        let result = page_of_project(db.reader(), project_a(), None, 10).await;
        assert_eq!(corrupt_of(result), ("deletions", "id"));

        // Scope tables.
        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::TraceIds, project_a(), 10)).await;
        raw(
            &db,
            format!(
                "INSERT INTO deletion_traces (deletion_id, trace_id) VALUES ('{ROW1}', X'{}')",
                "01".repeat(15)
            ),
        )
        .await;
        let result = scope_traces(db.reader(), Uuid::from_u128(1)).await;
        assert_eq!(corrupt_of(result), ("deletion_traces", "trace_id"));

        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::TraceIds, project_a(), 10)).await;
        raw(
            &db,
            format!(
                "INSERT INTO deletion_traces (deletion_id, trace_id) VALUES ('{ROW1}', X'{}')",
                "00".repeat(16)
            ),
        )
        .await;
        let result = scope_traces(db.reader(), Uuid::from_u128(1)).await;
        assert_eq!(corrupt_of(result), ("deletion_traces", "trace_id"));

        let (_dir, db) = test_db().await;
        insert(&db, new(1, DeletionKind::Project, project_a(), 10)).await;
        raw(
            &db,
            format!(
                "INSERT INTO deletion_projects (deletion_id, project_id) VALUES ('{ROW1}', 'x')"
            ),
        )
        .await;
        let result = scope_projects(db.reader(), Uuid::from_u128(1)).await;
        assert_eq!(corrupt_of(result), ("deletion_projects", "project_id"));
    }

    #[tokio::test]
    async fn guard_entries_skip_corrupt_rows() {
        let (_dir, db) = test_db().await;
        // A user row with no user id, and a trace row whose project is gone.
        insert(&db, new(1, DeletionKind::User, project_a(), 10)).await;
        raw(
            &db,
            format!("UPDATE deletions SET user_id = NULL WHERE id = '{ROW1}'"),
        )
        .await;
        insert(&db, new(2, DeletionKind::TraceIds, project_a(), 20)).await;
        add_traces(&db, Uuid::from_u128(2), vec![trace(5)]).await;
        raw(
            &db,
            format!(
                "UPDATE deletions SET project_id = NULL WHERE id = '{}'",
                Uuid::from_u128(2)
            ),
        )
        .await;
        // The good rows, which the guard keeps.
        insert(&db, new(3, DeletionKind::User, project_a(), 30)).await;
        insert(&db, new(4, DeletionKind::TraceIds, project_b(), 40)).await;
        add_traces(&db, Uuid::from_u128(4), vec![trace(6)]).await;

        let entries = guard_entries(db.reader(), 0).await.unwrap();
        assert_eq!(entries.traces, vec![(project_b(), trace(6), 40)]);
        assert_eq!(entries.users, vec![(project_a(), "user-1".to_owned(), 30)]);
    }
}
