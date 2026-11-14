//! Deletion requests: create, preview, list and read (spec 09 § Pricing & GDPR, D-M-2 to
//! D-M-5), and project and org drops (D-M-4, D-M-8). The worker in `crate::deletion` does the
//! rest.

use std::collections::BTreeSet;
use std::str::FromStr;

use axum::extract::{Extension, State};
use axum::http::{Method, StatusCode};
use axum::routing::{delete, get, post};
use llmobs_core::ids::{ProjectId, TraceId};
use llmobs_storage::{FilterDeletionRequest, QueryContext, TimeWindow, filter_trace_ids};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::common::{FieldError, Page, PageParams, ProblemCode};
use crate::api::deletions::{
    CreateDeletion, Deletion, DeletionActor, DeletionActorKind, DeletionFailure, DeletionFilter,
    DeletionKind, DeletionPreview, DeletionPreviewRequest, DeletionStatus, DropParams,
};
use crate::auth::OrgRole;
use crate::auth::principal::{self, Principal};
use crate::auth::roles::{OrgPerm, ProjectPerm};
use crate::http::AppState;
use crate::http::access::{Access, OrgAccess, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::meta::MetaError;
use crate::meta::deletions as store;
use crate::meta::members::org_role;
use crate::meta::orgs::mark_org_deleting;
use crate::meta::projects::{mark_project_deleting, project_ids_of_org};
use crate::routes::read::{self, READ_TIMEOUT};
use crate::routes::time_cursor::{decode_time_cursor, encode_time_cursor};

/// Most trace ids one `trace_ids` deletion takes (D-M-5).
pub(crate) const MAX_TRACE_IDS: usize = 1_000;
/// Most open deletions per project (D-M-5).
pub(crate) const MAX_OPEN: u64 = 1_000;
/// `user_id` length bounds, bytes (D-M-5, D-G-28).
const MAX_USER_ID_BYTES: usize = 255;

/// Deletion routes. Access is checked for each method (D-M-4); the handlers refuse the
/// kinds an `sk` key may not use.
pub(crate) fn routes() -> Vec<RouteDef> {
    let access = Access::Project {
        perm: ProjectPerm::Delete,
        session_only: false,
    };
    vec![
        RouteDef {
            method: Method::DELETE,
            path: "/api/v1/projects/{project}",
            access: Access::Project {
                perm: ProjectPerm::Drop,
                session_only: false,
            },
            documented: true,
            handler: delete(drop_project),
        },
        RouteDef {
            method: Method::DELETE,
            path: "/api/v1/orgs/{org}",
            access: Access::Org {
                perm: OrgPerm::Delete,
                session_only: false,
            },
            documented: true,
            handler: delete(drop_org),
        },
        RouteDef {
            method: Method::GET,
            path: "/api/v1/orgs/{org}/deletions",
            access: Access::Org {
                perm: OrgPerm::ViewDeletions,
                session_only: false,
            },
            documented: true,
            handler: get(list_org_deletions),
        },
        RouteDef {
            method: Method::GET,
            path: "/api/v1/projects/{project}/deletions",
            access,
            documented: true,
            handler: get(list_deletions),
        },
        RouteDef {
            method: Method::POST,
            path: "/api/v1/projects/{project}/deletions",
            access,
            documented: true,
            handler: post(create_deletion),
        },
        RouteDef {
            method: Method::POST,
            path: "/api/v1/projects/{project}/deletions/preview",
            access,
            documented: true,
            handler: post(preview_deletion),
        },
        RouteDef {
            method: Method::GET,
            path: "/api/v1/projects/{project}/deletions/{id}",
            access,
            documented: true,
            handler: get(get_deletion),
        },
    ]
}

#[derive(Debug, Deserialize)]
struct DeletionPath {
    id: Uuid,
}

async fn list_deletions(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Query(params): error::Query<PageParams>,
) -> Result<error::Json<Page<Deletion>>, ApiError> {
    let after = after_of(&params)?;
    let limit = params.limit.get();
    let rows =
        store::page_of_project(state.meta.reader(), access.project.id, after, limit + 1).await?;
    Ok(error::Json(page(rows, limit)?))
}

/// `GET /api/v1/orgs/{org}/deletions`: every deletion of the org, dropped projects' included.
async fn list_org_deletions(
    State(state): State<AppState>,
    Extension(access): Extension<OrgAccess>,
    error::Query(params): error::Query<PageParams>,
) -> Result<error::Json<Page<Deletion>>, ApiError> {
    let after = after_of(&params)?;
    let limit = params.limit.get();
    let rows = store::page_of_org(state.meta.reader(), access.org.id, after, limit + 1).await?;
    Ok(error::Json(page(rows, limit)?))
}

/// The keyset position of the requested page, from its cursor.
fn after_of(params: &PageParams) -> Result<Option<(i64, Uuid)>, ApiError> {
    params.cursor.as_deref().map(decode_time_cursor).transpose()
}

/// A page of at most `limit` rows. `rows` holds one extra row when there is a next page.
fn page(mut rows: Vec<store::DeletionRow>, limit: u32) -> Result<Page<Deletion>, ApiError> {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if more {
        rows.last()
            .map(|row| encode_time_cursor(row.created_at, &row.id.to_string()))
            .transpose()?
    } else {
        None
    };
    Ok(Page {
        data: rows.iter().map(deletion_json).collect(),
        next_cursor,
    })
}

/// `DELETE /api/v1/projects/{project}?confirm=<slug>`. Org admins only. The project is
/// `deleting` from this request on, so every route answers 404 for it; the worker drops it.
async fn drop_project(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Query(params): error::Query<DropParams>,
) -> Result<(StatusCode, error::Json<Deletion>), ApiError> {
    let user_id = principal::session_user(&principal)?.id;
    let org = access.project.org_id;
    let role = org_role(state.meta.reader(), org, user_id).await?;
    if !matches!(role, Some(OrgRole::Owner | OrgRole::Admin)) {
        return Err(ApiError::Forbidden {
            code: ProblemCode::Forbidden,
        });
    }
    if params.confirm != access.project.slug {
        return Err(field_error("confirm", "type the project slug to confirm"));
    }
    let id = Uuid::now_v7();
    let now = state.clock.now_ns();
    let project = access.project.id;
    let new = store::NewDeletion {
        id,
        org_id: org,
        project_id: Some(project),
        kind: store::DeletionKind::Project,
        user_id: None,
        request: store::RequestDetails {
            name: Some(access.project.name.clone()),
            slug: Some(access.project.slug.clone()),
            ..Default::default()
        },
        actor_kind: store::ActorKind::User,
        actor_id: user_id,
        state: store::DeletionState::Pending,
        created_at: now,
    };
    let row = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                if !mark_project_deleting(conn, project, now).await? {
                    return Err(MetaError::Conflict("the project is already being deleted"));
                }
                store::insert_deletion(conn, &new).await
            })
        })
        .await?;
    state.deletions.wake.notify_one();
    tracing::info!(
        target: "audit",
        deletion_id = %id,
        kind = store::DeletionKind::Project.as_str(),
        org_id = %org,
        project_id = %project,
        actor_kind = store::ActorKind::User.as_str(),
        actor_id = %user_id,
        "deletion requested"
    );
    Ok((StatusCode::ACCEPTED, error::Json(deletion_json(&row))))
}

/// `DELETE /api/v1/orgs/{org}?confirm=<slug>`. Owners only. The org is `deleting` from this
/// request on; its projects are not marked, they are hidden by the org-state check. The worker
/// drops them.
async fn drop_org(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<OrgAccess>,
    error::Query(params): error::Query<DropParams>,
) -> Result<(StatusCode, error::Json<Deletion>), ApiError> {
    let user_id = principal::session_user(&principal)?.id;
    if params.confirm != access.org.slug {
        return Err(field_error(
            "confirm",
            "type the organization slug to confirm",
        ));
    }
    let id = Uuid::now_v7();
    let now = state.clock.now_ns();
    let org = access.org.id;
    let new = store::NewDeletion {
        id,
        org_id: org,
        project_id: None,
        kind: store::DeletionKind::Org,
        user_id: None,
        request: store::RequestDetails {
            name: Some(access.org.name.clone()),
            slug: Some(access.org.slug.clone()),
            ..Default::default()
        },
        actor_kind: store::ActorKind::User,
        actor_id: user_id,
        state: store::DeletionState::Pending,
        created_at: now,
    };
    let row = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                if !mark_org_deleting(conn, org, now).await? {
                    return Err(MetaError::Conflict(
                        "the organization is already being deleted",
                    ));
                }
                let projects = project_ids_of_org(&mut *conn, org).await?;
                let row = store::insert_deletion(conn, &new).await?;
                store::insert_projects(conn, id, &projects).await?;
                Ok(row)
            })
        })
        .await?;
    state.deletions.wake.notify_one();
    tracing::info!(
        target: "audit",
        deletion_id = %id,
        kind = store::DeletionKind::Org.as_str(),
        org_id = %org,
        actor_kind = store::ActorKind::User.as_str(),
        actor_id = %user_id,
        "deletion requested"
    );
    Ok((StatusCode::ACCEPTED, error::Json(deletion_json(&row))))
}

async fn create_deletion(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Json(body): error::Json<CreateDeletion>,
) -> Result<(StatusCode, error::Json<Deletion>), ApiError> {
    let is_key = matches!(principal, Principal::ApiKey { .. });
    if is_key && !matches!(body, CreateDeletion::TraceIds { .. }) {
        return Err(ApiError::Forbidden {
            code: ProblemCode::Forbidden,
        });
    }
    let (actor_kind, actor_id) = actor(&principal);
    let project = access.project.id;
    let org = access.project.org_id;
    let now = state.clock.now_ns();
    let id = Uuid::now_v7();

    let (kind, user_id, request, traces) = match body {
        CreateDeletion::TraceIds { trace_ids } => {
            let traces = parse_trace_ids(&trace_ids)?;
            let request = store::RequestDetails {
                requested: Some(
                    u32::try_from(traces.len())
                        .map_err(|_| field_error("trace_ids", "send 1 to 1000 trace ids"))?,
                ),
                ..Default::default()
            };
            (store::DeletionKind::TraceIds, None, request, traces)
        }
        CreateDeletion::Filter {
            filter,
            search,
            from,
            to,
            include_eval,
            include_experiments,
        } => {
            let window = TimeWindow::new(from.ns(), to.ns())?;
            let selection = FilterSelection {
                filter: non_blank(filter),
                search: non_blank(search),
                window,
                include_eval,
                include_experiments,
            };
            // The count only checks the cap now; the worker resolves the traces again.
            let matched = matching(&state, project, &selection).await?;
            if matched.len() > state.deletions.filter_cap() {
                return Err(ApiError::TooManyTraces);
            }
            let request = store::RequestDetails {
                filter: selection.filter,
                search: selection.search,
                from: Some(from.ns()),
                to: Some(to.ns()),
                include_eval: Some(include_eval),
                include_experiments: Some(include_experiments),
                ..Default::default()
            };
            (store::DeletionKind::Filter, None, request, Vec::new())
        }
        CreateDeletion::User { user_id } => {
            if user_id.is_empty() || user_id.len() > MAX_USER_ID_BYTES {
                return Err(field_error("user_id", "user_id must be 1 to 255 bytes"));
            }
            (
                store::DeletionKind::User,
                Some(user_id),
                store::RequestDetails::default(),
                Vec::new(),
            )
        }
    };

    let new = store::NewDeletion {
        id,
        org_id: org,
        project_id: Some(project),
        kind,
        user_id: user_id.clone(),
        request,
        actor_kind,
        actor_id,
        state: store::DeletionState::Resolving,
        created_at: now,
    };
    let scope = traces.clone();
    let row = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                if store::count_open_in_project(conn, project).await? >= MAX_OPEN {
                    return Err(MetaError::Conflict("too many open deletions"));
                }
                let row = store::insert_deletion(conn, &new).await?;
                store::insert_traces(conn, id, &scope).await?;
                Ok(row)
            })
        })
        .await?;

    match kind {
        store::DeletionKind::TraceIds => state.deletions.guard.add_traces(project, &traces, now),
        store::DeletionKind::User => {
            if let Some(user) = &user_id {
                state.deletions.guard.add_user(project, user, now);
            }
        }
        store::DeletionKind::Filter | store::DeletionKind::Project | store::DeletionKind::Org => {}
    }
    state.deletions.wake.notify_one();
    tracing::info!(
        target: "audit",
        deletion_id = %id,
        kind = kind.as_str(),
        org_id = %org,
        project_id = %project,
        actor_kind = actor_kind.as_str(),
        actor_id = %actor_id,
        "deletion requested"
    );
    Ok((StatusCode::ACCEPTED, error::Json(deletion_json(&row))))
}

async fn preview_deletion(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Json(body): error::Json<DeletionPreviewRequest>,
) -> Result<error::Json<DeletionPreview>, ApiError> {
    if matches!(principal, Principal::ApiKey { .. }) {
        return Err(ApiError::Forbidden {
            code: ProblemCode::Forbidden,
        });
    }
    let selection = FilterSelection {
        filter: non_blank(body.filter),
        search: non_blank(body.search),
        window: TimeWindow::new(body.from.ns(), body.to.ns())?,
        include_eval: body.include_eval,
        include_experiments: body.include_experiments,
    };
    let matched = matching(&state, access.project.id, &selection).await?.len();
    let cap = state.deletions.filter_cap();
    Ok(error::Json(DeletionPreview {
        matched_traces: u64::try_from(matched.min(cap)).unwrap_or(u64::MAX),
        capped: matched > cap,
    }))
}

async fn get_deletion(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(DeletionPath { id }): error::Path<DeletionPath>,
) -> Result<error::Json<Deletion>, ApiError> {
    let row = store::deletion_in_project(state.meta.reader(), access.project.id, id)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(error::Json(deletion_json(&row)))
}

/// What a filter deletion selects, after the blank-text rule.
struct FilterSelection {
    filter: Option<String>,
    search: Option<String>,
    window: TimeWindow,
    include_eval: bool,
    include_experiments: bool,
}

/// The traces the selection matches, up to the cap plus one (so the caller can tell
/// "over the cap" from "at the cap"). Runs under the read timeout (D-M-5).
async fn matching(
    state: &AppState,
    project: ProjectId,
    selection: &FilterSelection,
) -> Result<Vec<TraceId>, ApiError> {
    let ctx: QueryContext = read::open_query(state, project, None).await?;
    let ids = filter_trace_ids(
        &ctx,
        FilterDeletionRequest {
            filter: selection.filter.as_deref(),
            search: selection.search.as_deref(),
            window: selection.window,
            include_eval: selection.include_eval,
            include_experiments: selection.include_experiments,
            limit: state.deletions.filter_cap().saturating_add(1),
            timeout: READ_TIMEOUT,
        },
    )
    .await?;
    Ok(ids)
}

/// Blank filter or search text means "no filter". The stored text is not trimmed.
fn non_blank(text: Option<String>) -> Option<String> {
    text.filter(|text| !text.trim().is_empty())
}

/// Distinct trace ids, 1 to [`MAX_TRACE_IDS`] of them. The length is checked before parsing.
fn parse_trace_ids(raw: &[String]) -> Result<Vec<TraceId>, ApiError> {
    let count_error = || field_error("trace_ids", "send 1 to 1000 trace ids");
    if raw.len() > MAX_TRACE_IDS {
        return Err(count_error());
    }
    let mut ids = BTreeSet::new();
    for text in raw {
        let id = TraceId::from_str(text).map_err(|_| {
            field_error(
                "trace_ids",
                "each trace id must be 32 hex digits, not all zero",
            )
        })?;
        ids.insert(id);
    }
    if ids.is_empty() || ids.len() > MAX_TRACE_IDS {
        return Err(count_error());
    }
    Ok(ids.into_iter().collect())
}

fn actor(principal: &Principal) -> (store::ActorKind, Uuid) {
    match principal {
        Principal::Session { user, .. } | Principal::Pat { user, .. } => {
            (store::ActorKind::User, user.id)
        }
        Principal::ApiKey { key_id, .. } => (store::ActorKind::ApiKey, *key_id),
    }
}

fn field_error(field: &str, message: &str) -> ApiError {
    ApiError::BadRequest {
        detail: None,
        errors: vec![FieldError {
            field: field.to_owned(),
            message: message.to_owned(),
            position: None,
        }],
    }
}

/// The API view of a stored deletion (M5 reuses it).
pub(crate) fn deletion_json(row: &store::DeletionRow) -> Deletion {
    Deletion {
        id: row.id,
        kind: kind_json(row.kind),
        status: status_json(row.state),
        org_id: row.org_id,
        project_id: row.project_id,
        user_id: row.user_id.clone(),
        requested_traces: row.request.requested,
        filter: filter_json(row),
        name: row.request.name.clone(),
        slug: row.request.slug.clone(),
        trace_count: row.trace_count,
        session_count: row.session_count,
        requested_by: DeletionActor {
            kind: actor_kind_json(row.actor_kind),
            id: row.actor_id,
        },
        created_at: row.created_at,
        handed_over_at: row.handed_over_at,
        applied_at: row.applied_at,
        error: row.error.map(failure_json),
    }
}

fn filter_json(row: &store::DeletionRow) -> Option<DeletionFilter> {
    if row.kind != store::DeletionKind::Filter {
        return None;
    }
    Some(DeletionFilter {
        filter: row.request.filter.clone(),
        search: row.request.search.clone(),
        from: row.request.from.unwrap_or(0),
        to: row.request.to.unwrap_or(0),
        include_eval: row.request.include_eval.unwrap_or(false),
        include_experiments: row.request.include_experiments.unwrap_or(false),
    })
}

fn kind_json(kind: store::DeletionKind) -> DeletionKind {
    match kind {
        store::DeletionKind::TraceIds => DeletionKind::TraceIds,
        store::DeletionKind::Filter => DeletionKind::Filter,
        store::DeletionKind::User => DeletionKind::User,
        store::DeletionKind::Project => DeletionKind::Project,
        store::DeletionKind::Org => DeletionKind::Org,
    }
}

fn status_json(state: store::DeletionState) -> DeletionStatus {
    match state {
        store::DeletionState::Resolving => DeletionStatus::Resolving,
        store::DeletionState::Pending => DeletionStatus::Pending,
        store::DeletionState::Applied => DeletionStatus::Applied,
        store::DeletionState::Failed => DeletionStatus::Failed,
    }
}

fn failure_json(reason: store::FailReason) -> DeletionFailure {
    match reason {
        store::FailReason::TooManyTraces => DeletionFailure::TooManyTraces,
        store::FailReason::ResolutionFailed => DeletionFailure::ResolutionFailed,
    }
}

fn actor_kind_json(kind: store::ActorKind) -> DeletionActorKind {
    match kind {
        store::ActorKind::User => DeletionActorKind::User,
        store::ActorKind::ApiKey => DeletionActorKind::ApiKey,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::http::header;
    use llmobs_core::ids::{SpanId, TraceId};
    use llmobs_storage::error::DeletionError;
    use serde_json::{Value, json};

    use super::*;
    use crate::auth::roles::{KeyScope, OrgRole, ProjectOverride};
    use crate::auth::secret::{self, KeyKind};
    use crate::deletion::run_once;
    use crate::meta::api_keys::{self, NewApiKey};
    use crate::meta::members::{add_org_member, set_project_override};
    use crate::meta::orgs::insert_org;
    use crate::meta::projects::{ProjectRow, insert_project, project_by_id};
    use crate::test_support::{TestApp, TestRequest, TestResponse, capture_logs, dataset_span};

    const A: &str = "a1000000000000000000000000000001";
    const B: &str = "a1000000000000000000000000000002";
    const C: &str = "a1000000000000000000000000000003";
    const FROM: &str = "2025-12-31T00:00:00Z";
    const TO: &str = "2026-01-02T00:00:00Z";

    /// How a request authenticates.
    enum Auth {
        Cookie(String),
        Bearer(String),
    }

    fn with_auth(request: TestRequest, auth: &Auth) -> TestRequest {
        match auth {
            Auth::Cookie(cookie) => request.header(header::COOKIE.as_str(), cookie),
            Auth::Bearer(token) => {
                request.header(header::AUTHORIZATION.as_str(), &format!("Bearer {token}"))
            }
        }
    }

    /// One request to any full path, with an optional JSON body.
    async fn send(
        app: &TestApp,
        method: Method,
        path: &str,
        auth: &Auth,
        body: Option<Value>,
    ) -> TestResponse {
        let request = app.request(method, path);
        let request = match body {
            Some(body) => request.json(&body),
            None => request,
        };
        with_auth(request, auth).send().await
    }

    async fn post(
        app: &TestApp,
        project: ProjectId,
        suffix: &str,
        body: Value,
        auth: &Auth,
    ) -> TestResponse {
        send(
            app,
            Method::POST,
            &format!("/api/v1/projects/{project}{suffix}"),
            auth,
            Some(body),
        )
        .await
    }

    async fn get(app: &TestApp, project: ProjectId, suffix: &str, auth: &Auth) -> TestResponse {
        send(
            app,
            Method::GET,
            &format!("/api/v1/projects/{project}{suffix}"),
            auth,
            None,
        )
        .await
    }

    /// The drop request of `project`: by id, confirmed with its slug.
    fn drop_path(project: &ProjectRow) -> String {
        format!("/api/v1/projects/{}?confirm={}", project.id, project.slug)
    }

    /// An OTLP export with `key`, as the ingest path sees it.
    async fn otlp(app: &TestApp, key: &str) -> TestResponse {
        app.request(Method::POST, "/v1/traces")
            .header("authorization", &format!("Bearer {key}"))
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
    }

    /// Inserts a `sk` key of `project` with `scopes`, created by `created_by`. Returns the key id
    /// and its plaintext.
    async fn key_with(
        app: &TestApp,
        project: ProjectId,
        created_by: Uuid,
        scopes: Vec<KeyScope>,
    ) -> (Uuid, String) {
        let minted = secret::new_key(KeyKind::Secret).unwrap();
        let plaintext = minted.plaintext.clone();
        let now = app.state.clock.now_ns();
        let id = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    let row = api_keys::insert_api_key(
                        conn,
                        NewApiKey {
                            project_id: project,
                            kind: "secret",
                            scopes,
                            name: None,
                            prefix: minted.prefix,
                            hash: minted.hash,
                            created_by,
                            expires_at: None,
                        },
                        now,
                    )
                    .await?;
                    Ok(row.id)
                })
            })
            .await
            .unwrap();
        (id, plaintext)
    }

    /// One span of `trace` named `name`, committed to `project`.
    async fn commit(app: &TestApp, project: &ProjectRow, trace: &str, span: u8, name: &str) {
        let mut row = dataset_span(0);
        row.trace_id = trace.parse::<TraceId>().unwrap();
        row.span_id = format!("b2000000000000{span:02x}")
            .parse::<SpanId>()
            .unwrap();
        row.parent_span_id = None;
        row.name = name.to_owned();
        row.environment = None;
        app.commit_spans(project, vec![row]).await;
    }

    fn window() -> Value {
        json!({"from": FROM, "to": TO})
    }

    fn user_body(user_id: &str) -> Value {
        json!({"kind": "user", "user_id": user_id})
    }

    fn id_of(created: &Value) -> String {
        created["id"].as_str().unwrap().to_owned()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn trace_ids_deletion_is_accepted_and_runs() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let auth = Auth::Cookie(seeded.cookie.clone());
        let body = json!({"kind": "trace_ids", "trace_ids": [A.to_uppercase(), A, B]});

        let response = post(&app, project, "/deletions", body, &auth).await;
        assert_eq!(response.status, StatusCode::ACCEPTED);
        let created: Value = response.json();
        assert_eq!(created["kind"], "trace_ids");
        assert_eq!(created["status"], "resolving");
        assert_eq!(created["requested_traces"], 2);
        assert_eq!(created["requested_by"]["kind"], "user");
        assert!(created.get("trace_count").is_none());
        let id = id_of(&created);

        run_once(&app.state).await.unwrap();
        let got: Value = get(&app, project, &format!("/deletions/{id}"), &auth)
            .await
            .json();
        assert_eq!(got["status"], "pending");
        assert_eq!(got["trace_count"], 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn trace_ids_validation() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let auth = Auth::Cookie(seeded.cookie.clone());
        let too_many: Vec<String> = (0..1_001).map(|n| format!("{n:032x}")).collect();
        let bodies = [
            json!({"kind": "trace_ids", "trace_ids": []}),
            json!({"kind": "trace_ids", "trace_ids": too_many}),
            json!({"kind": "trace_ids", "trace_ids": ["xyz"]}),
            json!({"kind": "trace_ids", "trace_ids": ["0".repeat(32)]}),
        ];
        for body in bodies {
            let response = post(&app, seeded.project.id, "/deletions", body, &auth).await;
            assert_eq!(response.status, StatusCode::BAD_REQUEST);
            let problem: Value = response.json();
            assert_eq!(problem["errors"][0]["field"], "trace_ids");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_kind_and_fields_are_400() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let auth = Auth::Cookie(seeded.cookie.clone());
        let project = seeded.project.id;
        let unknown_kind = post(&app, project, "/deletions", json!({"kind": "nope"}), &auth).await;
        assert_eq!(unknown_kind.status, StatusCode::BAD_REQUEST);
        let extra = post(
            &app,
            project,
            "/deletions",
            json!({"kind": "user", "user_id": "u", "extra": 1}),
            &auth,
        )
        .await;
        assert_eq!(extra.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn filter_deletion_counts_first() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let auth = Auth::Cookie(seeded.cookie.clone());
        commit(&app, &seeded.project, A, 1, "x").await;
        commit(&app, &seeded.project, B, 2, "y").await;
        app.state.clock.advance(Duration::from_secs(1));

        let body = json!({"kind": "filter", "filter": "name = 'x'", "from": FROM, "to": TO});
        let response = post(&app, project, "/deletions", body, &auth).await;
        assert_eq!(response.status, StatusCode::ACCEPTED);
        let created: Value = response.json();
        assert_eq!(created["filter"]["filter"], "name = 'x'");
        assert!(
            created["filter"]["from"]
                .as_str()
                .unwrap()
                .starts_with("2025-12-31")
        );
        assert!(
            created["filter"]["to"]
                .as_str()
                .unwrap()
                .starts_with("2026-01-02")
        );
        assert_eq!(created["filter"]["include_eval"], false);
        let id = id_of(&created);

        run_once(&app.state).await.unwrap();
        let got: Value = get(&app, project, &format!("/deletions/{id}"), &auth)
            .await
            .json();
        assert_eq!(got["trace_count"], 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn filter_over_the_cap_is_422() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let auth = Auth::Cookie(seeded.cookie.clone());
        commit(&app, &seeded.project, A, 1, "x").await;
        commit(&app, &seeded.project, B, 2, "x").await;
        app.state.clock.advance(Duration::from_secs(1));
        app.state.deletions.set_filter_cap(1);

        let response = post(
            &app,
            project,
            "/deletions",
            json!({"kind": "filter", "from": FROM, "to": TO}),
            &auth,
        )
        .await;
        assert_eq!(response.status, StatusCode::UNPROCESSABLE_ENTITY);
        let problem: Value = response.json();
        assert_eq!(problem["code"], "too_many_traces");

        let list: Value = get(&app, project, "/deletions", &auth).await.json();
        assert_eq!(list["data"].as_array().unwrap().len(), 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn filter_errors() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let auth = Auth::Cookie(seeded.cookie.clone());
        let bad_filter = post(
            &app,
            project,
            "/deletions",
            json!({"kind": "filter", "filter": "name =", "from": FROM, "to": TO}),
            &auth,
        )
        .await;
        assert_eq!(bad_filter.status, StatusCode::BAD_REQUEST);
        let problem: Value = bad_filter.json();
        assert_eq!(problem["code"], "invalid_filter");

        let wrong_type = post(
            &app,
            project,
            "/deletions",
            json!({"kind": "filter", "include_eval": "yes", "from": FROM, "to": TO}),
            &auth,
        )
        .await;
        assert_eq!(wrong_type.status, StatusCode::BAD_REQUEST);

        let reversed = post(
            &app,
            project,
            "/deletions",
            json!({"kind": "filter", "from": TO, "to": FROM}),
            &auth,
        )
        .await;
        assert_eq!(reversed.status, StatusCode::BAD_REQUEST);
        let problem: Value = reversed.json();
        assert_eq!(problem["code"], "bad_request");

        let missing_to = post(
            &app,
            project,
            "/deletions",
            json!({"kind": "filter", "from": FROM}),
            &auth,
        )
        .await;
        assert_eq!(missing_to.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn blank_filter_text_is_not_stored() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let auth = Auth::Cookie(seeded.cookie.clone());
        let response = post(
            &app,
            seeded.project.id,
            "/deletions",
            json!({"kind": "filter", "filter": "   ", "search": "", "from": FROM, "to": TO}),
            &auth,
        )
        .await;
        assert_eq!(response.status, StatusCode::ACCEPTED);
        let created: Value = response.json();
        assert!(created["filter"].get("filter").is_none());
        assert!(created["filter"].get("search").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn user_deletion_validation() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let auth = Auth::Cookie(seeded.cookie.clone());
        for user_id in [String::new(), "u".repeat(256)] {
            let response = post(&app, project, "/deletions", user_body(&user_id), &auth).await;
            assert_eq!(response.status, StatusCode::BAD_REQUEST);
            let problem: Value = response.json();
            assert_eq!(problem["errors"][0]["field"], "user_id");
        }
        let longest = post(
            &app,
            project,
            "/deletions",
            user_body(&"u".repeat(255)),
            &auth,
        )
        .await;
        assert_eq!(longest.status, StatusCode::ACCEPTED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn preview_counts_and_caps() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let auth = Auth::Cookie(seeded.cookie.clone());
        commit(&app, &seeded.project, A, 1, "x").await;
        commit(&app, &seeded.project, B, 2, "x").await;
        commit(&app, &seeded.project, C, 3, "x").await;
        app.state.clock.advance(Duration::from_secs(1));

        let counted = post(&app, project, "/deletions/preview", window(), &auth).await;
        assert_eq!(counted.status, StatusCode::OK);
        let counted: Value = counted.json();
        assert_eq!(counted, json!({"matched_traces": 3, "capped": false}));

        app.state.deletions.set_filter_cap(2);
        let capped = post(&app, project, "/deletions/preview", window(), &auth).await;
        assert_eq!(capped.status, StatusCode::OK);
        let capped: Value = capped.json();
        assert_eq!(capped, json!({"matched_traces": 2, "capped": true}));

        let invalid = post(
            &app,
            project,
            "/deletions/preview",
            json!({"filter": "name =", "from": FROM, "to": TO}),
            &auth,
        )
        .await;
        assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
        let problem: Value = invalid.json();
        assert_eq!(problem["code"], "invalid_filter");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn access_rules() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let owner = Auth::Cookie(seeded.cookie.clone());
        let owner_read_pat = Auth::Bearer(app.personal_token(seeded.user_id, false).await);
        let member = app
            .add_member(&seeded, "bob@example.com", OrgRole::Member)
            .await;
        let member_auth = Auth::Cookie(member.cookie.clone());
        let (_, write_key) = key_with(&app, project, seeded.user_id, vec![KeyScope::Write]).await;
        let (_, read_key) = key_with(&app, project, seeded.user_id, vec![KeyScope::Read]).await;
        let write_key = Auth::Bearer(write_key);
        let read_key = Auth::Bearer(read_key);

        let trace_ids = json!({"kind": "trace_ids", "trace_ids": [A]});
        let filter = json!({"kind": "filter", "from": FROM, "to": TO});
        let user = user_body("user-access");
        let preview = window();

        // Each row: credential, then the status of trace_ids, filter, user, preview and the list.
        let owner_cases = [
            (&owner, [202, 202, 202, 200, 200]),
            (&member_auth, [403, 403, 403, 403, 403]),
            (&write_key, [202, 403, 403, 403, 200]),
            (&read_key, [403, 403, 403, 403, 403]),
            (&owner_read_pat, [403, 403, 403, 403, 200]),
        ];
        let bodies = [
            ("/deletions", trace_ids.clone()),
            ("/deletions", filter.clone()),
            ("/deletions", user.clone()),
            ("/deletions/preview", preview.clone()),
        ];
        for (auth, statuses) in owner_cases {
            for (index, (suffix, body)) in bodies.iter().enumerate() {
                let response = post(&app, project, suffix, body.clone(), auth).await;
                assert_eq!(
                    response.status.as_u16(),
                    statuses[index],
                    "row {index} of the table"
                );
            }
            let list = get(&app, project, "/deletions", auth).await;
            assert_eq!(list.status.as_u16(), statuses[4], "list");
        }

        // A project override of admin lets a plain member delete listed traces.
        let member_id = member.id;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    set_project_override(conn, project, member_id, ProjectOverride::Admin).await
                })
            })
            .await
            .unwrap();
        let response = post(&app, project, "/deletions", trace_ids, &member_auth).await;
        assert_eq!(response.status, StatusCode::ACCEPTED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn key_actor_is_recorded() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let (key_id, plaintext) =
            key_with(&app, project, seeded.user_id, vec![KeyScope::Write]).await;
        let response = post(
            &app,
            project,
            "/deletions",
            json!({"kind": "trace_ids", "trace_ids": [A]}),
            &Auth::Bearer(plaintext),
        )
        .await;
        assert_eq!(response.status, StatusCode::ACCEPTED);
        let created: Value = response.json();
        assert_eq!(
            created["requested_by"],
            json!({"kind": "api_key", "id": key_id.to_string()})
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn too_many_open_deletions_is_409() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let org_id = seeded.org_id;
        let user_id = seeded.user_id;
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    for _ in 0..MAX_OPEN {
                        let new = store::NewDeletion {
                            id: Uuid::now_v7(),
                            org_id,
                            project_id: Some(project),
                            kind: store::DeletionKind::Filter,
                            user_id: None,
                            request: store::RequestDetails::default(),
                            actor_kind: store::ActorKind::User,
                            actor_id: user_id,
                            state: store::DeletionState::Resolving,
                            created_at: now,
                        };
                        store::insert_deletion(conn, &new).await?;
                    }
                    Ok(())
                })
            })
            .await
            .unwrap();

        let auth = Auth::Cookie(seeded.cookie.clone());
        let response = post(&app, project, "/deletions", user_body("u-open"), &auth).await;
        assert_eq!(response.status, StatusCode::CONFLICT);
        let problem: Value = response.json();
        assert_eq!(problem["detail"], "too many open deletions");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_pages_newest_first() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let auth = Auth::Cookie(seeded.cookie.clone());
        for user in ["u-1", "u-2", "u-3"] {
            let response = post(&app, project, "/deletions", user_body(user), &auth).await;
            assert_eq!(response.status, StatusCode::ACCEPTED);
            app.state.clock.advance(Duration::from_secs(1));
        }

        let first: Value = get(&app, project, "/deletions?limit=2", &auth).await.json();
        let data = first["data"].as_array().unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data[0]["user_id"], "u-3");
        assert_eq!(data[1]["user_id"], "u-2");
        let cursor = first["next_cursor"].as_str().unwrap().to_owned();

        let second: Value = get(
            &app,
            project,
            &format!("/deletions?limit=2&cursor={cursor}"),
            &auth,
        )
        .await
        .json();
        let data = second["data"].as_array().unwrap();
        assert_eq!(data.len(), 1);
        assert_eq!(data[0]["user_id"], "u-1");
        assert!(second["next_cursor"].is_null());

        let bad = get(&app, project, "/deletions?cursor=bad", &auth).await;
        assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_checks_the_project() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let auth = Auth::Cookie(seeded.cookie.clone());
        let (other, _) = app.add_project(&seeded, "other", None).await;
        let response = post(&app, other.id, "/deletions", user_body("u-other"), &auth).await;
        assert_eq!(response.status, StatusCode::ACCEPTED);
        let id = id_of(&response.json());

        let through_this_project = get(&app, project, &format!("/deletions/{id}"), &auth).await;
        assert_eq!(through_this_project.status, StatusCode::NOT_FOUND);
        let unknown = get(
            &app,
            project,
            &format!("/deletions/{}", Uuid::now_v7()),
            &auth,
        )
        .await;
        assert_eq!(unknown.status, StatusCode::NOT_FOUND);
        let malformed = get(&app, project, "/deletions/not-a-uuid", &auth).await;
        assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
        let own = get(&app, other.id, &format!("/deletions/{id}"), &auth).await;
        assert_eq!(own.status, StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn guard_is_filled_at_creation() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let auth = Auth::Cookie(seeded.cookie.clone());
        let response = post(&app, project, "/deletions", user_body("u9"), &auth).await;
        assert_eq!(response.status, StatusCode::ACCEPTED);
        let now = app.state.clock.now_ns();
        let trace = TraceId::from_bytes([1; 16]).unwrap();
        assert!(
            app.state
                .deletions
                .guard
                .blocks(project, trace, &["u9"], now, now)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn request_audit_log_has_no_scope() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = seeded.project.id;
        let auth = Auth::Cookie(seeded.cookie.clone());
        let (response, logs) = capture_logs(post(
            &app,
            project,
            "/deletions",
            user_body("secret-user-1"),
            &auth,
        ))
        .await;
        assert_eq!(response.status, StatusCode::ACCEPTED);
        assert!(logs.contains("deletion requested"));
        assert!(!logs.contains("secret-user-1"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_pat_of_an_admin_lists() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let token = Auth::Bearer(app.personal_token(seeded.user_id, false).await);
        let response = get(&app, seeded.project.id, "/deletions", &token).await;
        assert_eq!(response.status, StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn project_drop_hides_and_drops() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let owner = Auth::Cookie(seeded.cookie.clone());
        let (doomed, used_key) = app.add_project(&seeded, "doomed", None).await;
        let (_, unused_key) =
            key_with(&app, doomed.id, seeded.user_id, vec![KeyScope::Ingest]).await;
        commit(&app, &doomed, A, 1, "x").await;
        // The export caches the used key, so the drop is seen at the project check.
        assert_eq!(otlp(&app, &used_key).await.status, StatusCode::OK);

        let response = send(&app, Method::DELETE, &drop_path(&doomed), &owner, None).await;
        assert_eq!(response.status, StatusCode::ACCEPTED);
        let created: Value = response.json();
        assert_eq!(created["kind"], "project");
        assert_eq!(created["status"], "pending");
        assert_eq!(created["slug"], "doomed");
        let id = id_of(&created);

        let project_path = format!("/api/v1/projects/{}", doomed.id);
        let gone = send(&app, Method::GET, &project_path, &owner, None).await;
        assert_eq!(gone.status, StatusCode::NOT_FOUND);
        let projects: Value = send(
            &app,
            Method::GET,
            "/api/v1/orgs/acme/projects",
            &owner,
            None,
        )
        .await
        .json();
        let listed = projects["data"].as_array().unwrap();
        assert!(listed.iter().all(|row| row["id"] != doomed.id.to_string()));
        assert_eq!(otlp(&app, &used_key).await.status, StatusCode::NOT_FOUND);
        assert_eq!(
            otlp(&app, &unused_key).await.status,
            StatusCode::UNAUTHORIZED
        );

        run_once(&app.state).await.unwrap();
        let history: Value = send(
            &app,
            Method::GET,
            "/api/v1/orgs/acme/deletions",
            &owner,
            None,
        )
        .await
        .json();
        let row = history["data"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .expect("the drop is listed");
        assert_eq!(row["status"], "applied");
        assert!(
            project_by_id(app.state.meta.reader(), doomed.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn project_drop_confirmation() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let owner = Auth::Cookie(seeded.cookie.clone());
        let (doomed, _) = app.add_project(&seeded, "doomed", None).await;
        let path = format!("/api/v1/projects/{}", doomed.id);

        let wrong = send(
            &app,
            Method::DELETE,
            &format!("{path}?confirm=wrong"),
            &owner,
            None,
        )
        .await;
        assert_eq!(wrong.status, StatusCode::BAD_REQUEST);
        let problem: Value = wrong.json();
        assert_eq!(problem["errors"][0]["field"], "confirm");

        let missing = send(&app, Method::DELETE, &path, &owner, None).await;
        assert_eq!(missing.status, StatusCode::BAD_REQUEST);

        let still = send(&app, Method::GET, &path, &owner, None).await;
        assert_eq!(still.status, StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn project_drop_needs_org_admin() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let owner = Auth::Cookie(seeded.cookie.clone());
        let member = app
            .add_member(&seeded, "bob@example.com", OrgRole::Member)
            .await;
        let admin = app
            .add_member(&seeded, "carol@example.com", OrgRole::Admin)
            .await;
        let member_auth = Auth::Cookie(member.cookie.clone());
        let admin_auth = Auth::Cookie(admin.cookie.clone());

        // A member with a project override of admin is still not an org admin.
        let (overridden, _) = app.add_project(&seeded, "p-override", None).await;
        let (by_admin, _) = app.add_project(&seeded, "p-admin", None).await;
        let (by_key, _) = app.add_project(&seeded, "p-key", None).await;
        let (by_read_pat, _) = app.add_project(&seeded, "p-read", None).await;
        let (by_write_pat, _) = app.add_project(&seeded, "p-write", None).await;
        let member_id = member.id;
        let override_project = overridden.id;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    set_project_override(conn, override_project, member_id, ProjectOverride::Admin)
                        .await
                })
            })
            .await
            .unwrap();

        let member_try = send(
            &app,
            Method::DELETE,
            &drop_path(&overridden),
            &member_auth,
            None,
        )
        .await;
        assert_eq!(member_try.status, StatusCode::FORBIDDEN);
        let admin_try = send(
            &app,
            Method::DELETE,
            &drop_path(&by_admin),
            &admin_auth,
            None,
        )
        .await;
        assert_eq!(admin_try.status, StatusCode::ACCEPTED);
        let (_, all_scopes) = key_with(
            &app,
            by_key.id,
            seeded.user_id,
            vec![
                KeyScope::Ingest,
                KeyScope::Read,
                KeyScope::Write,
                KeyScope::Feedback,
            ],
        )
        .await;
        let key_try = send(
            &app,
            Method::DELETE,
            &drop_path(&by_key),
            &Auth::Bearer(all_scopes),
            None,
        )
        .await;
        assert_eq!(key_try.status, StatusCode::FORBIDDEN);
        let read_only = Auth::Bearer(app.personal_token(seeded.user_id, false).await);
        let read_try = send(
            &app,
            Method::DELETE,
            &drop_path(&by_read_pat),
            &read_only,
            None,
        )
        .await;
        assert_eq!(read_try.status, StatusCode::FORBIDDEN);
        let read_write = Auth::Bearer(app.personal_token(seeded.user_id, true).await);
        let write_try = send(
            &app,
            Method::DELETE,
            &drop_path(&by_write_pat),
            &read_write,
            None,
        )
        .await;
        assert_eq!(write_try.status, StatusCode::ACCEPTED);
        // The refused project is still active, so the owner may drop it.
        let owner_try = send(&app, Method::DELETE, &drop_path(&overridden), &owner, None).await;
        assert_eq!(owner_try.status, StatusCode::ACCEPTED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn second_drop_is_404() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let owner = Auth::Cookie(seeded.cookie.clone());
        let (doomed, _) = app.add_project(&seeded, "doomed", None).await;
        let first = send(&app, Method::DELETE, &drop_path(&doomed), &owner, None).await;
        assert_eq!(first.status, StatusCode::ACCEPTED);
        let second = send(&app, Method::DELETE, &drop_path(&doomed), &owner, None).await;
        assert_eq!(second.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn org_drop_hides_the_org_and_its_projects() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let owner = Auth::Cookie(seeded.cookie.clone());
        let user_id = seeded.user_id;
        let now = app.state.clock.now_ns();
        let (b1, b2) = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    let beta = insert_org(conn, "Beta", "beta", now).await?;
                    add_org_member(conn, beta.id, user_id, OrgRole::Owner, now).await?;
                    let b1 = insert_project(conn, beta.id, "b1", "b1", None, now).await?;
                    let b2 = insert_project(conn, beta.id, "b2", "b2", None, now).await?;
                    Ok((b1, b2))
                })
            })
            .await
            .unwrap();
        commit(&app, &b1, A, 1, "x").await;

        let response = send(
            &app,
            Method::DELETE,
            "/api/v1/orgs/beta?confirm=beta",
            &owner,
            None,
        )
        .await;
        assert_eq!(response.status, StatusCode::ACCEPTED);
        let created: Value = response.json();
        assert_eq!(created["kind"], "org");
        let id = Uuid::parse_str(&id_of(&created)).unwrap();

        let gone = send(&app, Method::GET, "/api/v1/orgs/beta", &owner, None).await;
        assert_eq!(gone.status, StatusCode::NOT_FOUND);
        let orgs: Value = send(&app, Method::GET, "/api/v1/orgs", &owner, None)
            .await
            .json();
        let slugs: Vec<&str> = orgs["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|org| org["slug"].as_str().unwrap())
            .collect();
        assert_eq!(slugs, vec!["acme"]);
        let me: Value = send(&app, Method::GET, "/api/auth/me", &owner, None)
            .await
            .json();
        let me_slugs: Vec<&str> = me["orgs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|org| org["slug"].as_str().unwrap())
            .collect();
        assert_eq!(me_slugs, vec!["acme"]);
        for project in [&b1, &b2] {
            let path = format!("/api/v1/projects/{}", project.id);
            let status = send(&app, Method::GET, &path, &owner, None).await.status;
            assert_eq!(status, StatusCode::NOT_FOUND);
        }

        run_once(&app.state).await.unwrap();
        let row = store::deletion_by_id(app.state.meta.reader(), id)
            .await
            .unwrap()
            .expect("the drop record is kept");
        assert_eq!(row.state, store::DeletionState::Applied);
        let projects = store::scope_projects(app.state.meta.reader(), id)
            .await
            .unwrap();
        assert_eq!(
            projects.into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([b1.id, b2.id])
        );

        // A dropped project's own drop, and then its org's, both apply.
        let g1 = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    let gamma = insert_org(conn, "Gamma", "gamma", now).await?;
                    add_org_member(conn, gamma.id, user_id, OrgRole::Owner, now).await?;
                    insert_project(conn, gamma.id, "g1", "g1", None, now).await
                })
            })
            .await
            .unwrap();
        let project_drop = send(&app, Method::DELETE, &drop_path(&g1), &owner, None).await;
        assert_eq!(project_drop.status, StatusCode::ACCEPTED);
        let project_drop_id = Uuid::parse_str(&id_of(&project_drop.json())).unwrap();
        let org_drop = send(
            &app,
            Method::DELETE,
            "/api/v1/orgs/gamma?confirm=gamma",
            &owner,
            None,
        )
        .await;
        assert_eq!(org_drop.status, StatusCode::ACCEPTED);
        let org_drop_id = Uuid::parse_str(&id_of(&org_drop.json())).unwrap();
        run_once(&app.state).await.unwrap();
        for deletion in [project_drop_id, org_drop_id] {
            let row = store::deletion_by_id(app.state.meta.reader(), deletion)
                .await
                .unwrap()
                .expect("the record is kept");
            assert_eq!(row.state, store::DeletionState::Applied);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn org_drop_is_owner_only() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let owner = Auth::Cookie(seeded.cookie.clone());
        let admin = app
            .add_member(&seeded, "dan@example.com", OrgRole::Admin)
            .await;
        let admin_auth = Auth::Cookie(admin.cookie.clone());
        let (_, key) = key_with(
            &app,
            seeded.project.id,
            seeded.user_id,
            vec![KeyScope::Write],
        )
        .await;

        let path = "/api/v1/orgs/acme?confirm=acme";
        let by_admin = send(&app, Method::DELETE, path, &admin_auth, None).await;
        assert_eq!(by_admin.status, StatusCode::FORBIDDEN);
        let by_key = send(&app, Method::DELETE, path, &Auth::Bearer(key), None).await;
        assert_eq!(by_key.status, StatusCode::NOT_FOUND);
        let typo = send(
            &app,
            Method::DELETE,
            "/api/v1/orgs/acme?confirm=acme-typo",
            &owner,
            None,
        )
        .await;
        assert_eq!(typo.status, StatusCode::BAD_REQUEST);
        let problem: Value = typo.json();
        assert_eq!(problem["errors"][0]["field"], "confirm");
        let still = send(&app, Method::GET, "/api/v1/orgs/acme", &owner, None).await;
        assert_eq!(still.status, StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_data_deletions_of_a_dropped_project_are_superseded() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let owner = Auth::Cookie(seeded.cookie.clone());
        let (sup, _) = app.add_project(&seeded, "sup", None).await;
        let created = post(&app, sup.id, "/deletions", user_body("sup-user"), &owner).await;
        assert_eq!(created.status, StatusCode::ACCEPTED);
        let user_row = Uuid::parse_str(&id_of(&created.json())).unwrap();

        let dropped = send(&app, Method::DELETE, &drop_path(&sup), &owner, None).await;
        assert_eq!(dropped.status, StatusCode::ACCEPTED);
        run_once(&app.state).await.unwrap();

        let row = store::deletion_by_id(app.state.meta.reader(), user_row)
            .await
            .unwrap()
            .expect("the user row is kept");
        assert_eq!(row.state, store::DeletionState::Applied);
        assert!(row.trace_count.is_none());
        assert!(matches!(
            app.state.storage.deletion_status(user_row).await,
            Err(DeletionError::NotFound)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn org_history_pages() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let owner = Auth::Cookie(seeded.cookie.clone());
        let (first_project, _) = app.add_project(&seeded, "hist-1", None).await;
        let (second_project, _) = app.add_project(&seeded, "hist-2", None).await;
        let first = send(
            &app,
            Method::DELETE,
            &drop_path(&first_project),
            &owner,
            None,
        )
        .await;
        let first_id = id_of(&first.json());
        app.state.clock.advance(Duration::from_secs(1));
        let second = send(
            &app,
            Method::DELETE,
            &drop_path(&second_project),
            &owner,
            None,
        )
        .await;
        let second_id = id_of(&second.json());

        let page: Value = send(
            &app,
            Method::GET,
            "/api/v1/orgs/acme/deletions?limit=1",
            &owner,
            None,
        )
        .await
        .json();
        assert_eq!(page["data"][0]["id"], second_id);
        let cursor = page["next_cursor"].as_str().unwrap().to_owned();
        let next: Value = send(
            &app,
            Method::GET,
            &format!("/api/v1/orgs/acme/deletions?limit=1&cursor={cursor}"),
            &owner,
            None,
        )
        .await
        .json();
        assert_eq!(next["data"].as_array().unwrap().len(), 1);
        assert_eq!(next["data"][0]["id"], first_id);
        assert!(next["next_cursor"].is_null());

        let member = app
            .add_member(&seeded, "eve@example.com", OrgRole::Member)
            .await;
        let denied = send(
            &app,
            Method::GET,
            "/api/v1/orgs/acme/deletions",
            &Auth::Cookie(member.cookie.clone()),
            None,
        )
        .await;
        assert_eq!(denied.status, StatusCode::FORBIDDEN);
        let admin = app
            .add_member(&seeded, "fay@example.com", OrgRole::Admin)
            .await;
        let allowed = send(
            &app,
            Method::GET,
            "/api/v1/orgs/acme/deletions",
            &Auth::Cookie(admin.cookie.clone()),
            None,
        )
        .await;
        assert_eq!(allowed.status, StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn drop_audit_carries_no_slug() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let owner = Auth::Cookie(seeded.cookie.clone());
        let (secret, _) = app.add_project(&seeded, "secret-slug-1", None).await;
        let path = format!("/api/v1/projects/{}?confirm=secret-slug-1", secret.id);
        let (response, logs) = capture_logs(send(&app, Method::DELETE, &path, &owner, None)).await;
        assert_eq!(response.status, StatusCode::ACCEPTED);
        assert!(logs.contains("deletion requested"));
        assert!(!logs.contains("secret-slug-1"));
    }
}
