//! Route access (D138). One table declares it and [`enforce`] applies it.
#![cfg_attr(not(test), allow(dead_code))]

use std::fmt;

use axum::extract::{FromRequestParts, RawPathParams, Request, State};
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{MethodRouter, get};
use axum::{Router, middleware};
use llmobs_core::ids::{OrgId, ProjectId};

use super::AppState;
use super::error::ApiError;
use super::health;
use crate::api::common::ProblemCode;
use crate::auth::principal::Principal;
use crate::auth::roles::{
    self, KeyScope, KeyScopes, OrgPerm, OrgRole, ProjectPerm, Role, org_rank,
};
use crate::meta::members::{org_role, project_override};
use crate::meta::orgs::{OrgRow, org_by_id, org_by_slug};
use crate::meta::projects::{ProjectRow, project_by_id, project_by_slugs};

/// Who may call a route.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Access {
    /// No credentials.
    Public,
    /// Any signed-in caller. `session_only` refuses a key or a PAT.
    User {
        /// Session cookie only.
        session_only: bool,
    },
    /// Instance admin, session only.
    InstanceAdmin,
    /// `{org}` in the path.
    Org {
        /// The permission.
        perm: OrgPerm,
        /// Session cookie only.
        session_only: bool,
    },
    /// `{project}` in the path.
    Project {
        /// The permission.
        perm: ProjectPerm,
        /// Session cookie only.
        session_only: bool,
    },
    /// A project API key with `scope`. Sessions and PATs are refused.
    Key {
        /// The scope the key must hold.
        scope: KeyScope,
    },
    /// A personal access token, read or read-write; sessions and API keys get 403.
    /// No `{org}` or `{project}` in the path: the handler resolves projects itself (D-H-3).
    Pat,
}

impl Access {
    /// Whether this request is a read made with POST: a read-only PAT and a `read` key may
    /// call it (D-H-4). Only POST counts; PUT, PATCH and DELETE on the same route stay writes.
    /// `Pat` is `/mcp`, where every tool reads.
    pub(crate) fn reads_by_post(self, method: &axum::http::Method) -> bool {
        *method == axum::http::Method::POST
            && matches!(
                self,
                Self::Project {
                    perm: ProjectPerm::Query,
                    ..
                } | Self::Pat
            )
    }
}

/// One row of the route table.
pub(crate) struct RouteDef {
    /// Method the OpenAPI check compares.
    pub(crate) method: axum::http::Method,
    /// Axum path, for example `/api/v1/orgs/{org}`.
    pub(crate) path: &'static str,
    /// Who may call it.
    pub(crate) access: Access,
    /// `false` for `/healthz` and `/readyz`, which are not in the OpenAPI document.
    pub(crate) documented: bool,
    /// The handler. [`build_router`] wraps it with [`enforce`].
    pub(crate) handler: MethodRouter<AppState>,
}

/// The org a handler may use, and the caller's role there.
#[derive(Debug, Clone)]
pub(crate) struct OrgAccess {
    /// The org. `state` is `active`.
    pub(crate) org: OrgRow,
    /// The caller's org role.
    pub(crate) role: OrgRole,
}

/// The project a handler may use, and the caller's effective role.
#[derive(Debug, Clone)]
pub(crate) struct ProjectAccess {
    /// The project. `state` is `active`, and so is its org.
    pub(crate) project: ProjectRow,
    /// Effective project role. Absent when the caller is an API key.
    pub(crate) role: Option<Role>,
}

/// Health probes. Later sub-phases append their handlers here.
pub(crate) fn routes() -> Vec<RouteDef> {
    let mut routes = vec![
        RouteDef {
            method: axum::http::Method::GET,
            path: "/healthz",
            access: Access::Public,
            documented: false,
            handler: get(health::healthz),
        },
        RouteDef {
            method: axum::http::Method::GET,
            path: "/readyz",
            access: Access::Public,
            documented: false,
            handler: get(health::readyz),
        },
    ];
    routes.extend(crate::routes::auth::routes());
    routes.extend(crate::routes::oauth::routes());
    routes.extend(crate::routes::admin::routes());
    routes.extend(crate::routes::invites::routes());
    routes.extend(crate::routes::me::routes());
    routes.extend(crate::routes::orgs::routes());
    routes.extend(crate::routes::projects::routes());
    routes.extend(crate::routes::members::routes());
    routes.extend(crate::routes::connections::routes());
    routes.extend(crate::routes::api_keys::routes());
    routes.extend(crate::routes::channels::routes());
    routes.extend(crate::routes::alert_rules::routes());
    routes.extend(crate::routes::tokens::routes());
    routes.extend(crate::routes::traces::routes());
    routes.extend(crate::routes::streams::routes());
    routes.extend(crate::routes::sessions::routes());
    routes.extend(crate::routes::users::routes());
    routes.extend(crate::routes::scores::routes());
    routes.extend(crate::routes::feedback::routes());
    routes.extend(crate::routes::saved_views::routes());
    routes.extend(crate::routes::sql::routes());
    routes.extend(crate::routes::schema::routes());
    routes.extend(crate::routes::metrics::routes());
    routes.extend(crate::routes::media::routes());
    routes.extend(crate::routes::otlp::routes());
    routes.extend(crate::mcp::routes());
    routes
}

/// Registers `routes`, with [`enforce`] on each method.
///
/// axum 0.8 merges a second [`Router::route`] on the same path into the existing
/// method router. The same method twice panics. Each row is wrapped before that
/// merge, so two methods on one path keep their own access checks.
pub(crate) fn build_router(state: &AppState, routes: Vec<RouteDef>) -> Router<AppState> {
    let mut router = Router::new();
    for route in routes {
        let access = route.access;
        let layered = route.handler.route_layer(middleware::from_fn_with_state(
            state.clone(),
            move |State(state): State<AppState>, request: Request, next: Next| {
                enforce(state, access, request, next)
            },
        ));
        router = router.route(route.path, layered);
    }
    router
}

async fn enforce(state: AppState, access: Access, request: Request, next: Next) -> Response {
    match prepare(state, access, request).await {
        Ok(request) => {
            let mut response = next.run(request).await;
            // The handler's own 404 or 400 must not look like a denial.
            record_access(&mut response, "allowed");
            response
        }
        Err(error) => {
            let marker = denial_marker(&error);
            let mut response = error.into_response();
            record_access(&mut response, marker);
            response
        }
    }
}

/// `x-test-access` is how the authorization suite separates [`enforce`] from the handler.
fn record_access(response: &mut Response, marker: &'static str) {
    #[cfg(test)]
    {
        let _ = response.headers_mut().insert(
            axum::http::HeaderName::from_static("x-test-access"),
            axum::http::HeaderValue::from_static(marker),
        );
    }
    #[cfg(not(test))]
    {
        let _ = (response, marker);
    }
}

fn denial_marker(error: &ApiError) -> &'static str {
    match error {
        ApiError::Unauthorized => "denied-401",
        ApiError::Forbidden { .. } => "denied-403",
        ApiError::NotFound => "denied-404",
        _ => "denied-other",
    }
}

async fn prepare(state: AppState, access: Access, request: Request) -> Result<Request, ApiError> {
    if matches!(access, Access::Public) {
        return Ok(request);
    }
    let (mut parts, body) = request.into_parts();
    let principal = parts
        .extensions
        .get::<Option<Principal>>()
        .cloned()
        .flatten();
    let Some(principal) = principal else {
        return Err(ApiError::Unauthorized);
    };
    // A read PAT cannot call a write method. That is the token's scope, so it
    // is 403 even when the resource would be 404. A POST that reads
    // (Access::reads_by_post) is not a write.
    if let Principal::Pat { write: false, .. } = &principal
        && !matches!(
            parts.method,
            axum::http::Method::GET | axum::http::Method::HEAD
        )
        && !access.reads_by_post(&parts.method)
    {
        return Err(forbidden());
    }
    match access {
        Access::Public => {}
        Access::User { session_only } => {
            if session_only && !matches!(principal, Principal::Session { .. }) {
                return Err(forbidden());
            }
            if matches!(principal, Principal::ApiKey { .. }) {
                return Err(forbidden());
            }
        }
        Access::InstanceAdmin => match &principal {
            Principal::Session { user, .. } if user.is_instance_admin => {}
            _ => return Err(forbidden()),
        },
        Access::Org { perm, session_only } => {
            // A key is not a member of any org. 404 does not depend on whether
            // the route is session-only, so a foreign key cannot probe orgs.
            if matches!(principal, Principal::ApiKey { .. }) {
                return Err(ApiError::NotFound);
            }
            if session_only && !matches!(principal, Principal::Session { .. }) {
                return Err(forbidden());
            }
            let key = path_value(&mut parts, &state, "org").await?;
            grant_org(&state, &mut parts, &principal, &key, perm).await?;
        }
        Access::Project { perm, session_only } => {
            let key = path_value(&mut parts, &state, "project").await?;
            grant_project(&state, &mut parts, &principal, &key, perm, session_only).await?;
        }
        Access::Key { scope } => {
            let Principal::ApiKey {
                project_id, scopes, ..
            } = &principal
            else {
                return Err(forbidden());
            };
            if !scopes.contains(scope) {
                return Err(forbidden());
            }
            grant_key_only_project(&state, &mut parts, *project_id).await?;
        }
        Access::Pat => {
            if !matches!(principal, Principal::Pat { .. }) {
                return Err(forbidden());
            }
        }
    }
    parts.extensions.insert(principal);
    Ok(Request::from_parts(parts, body))
}

fn forbidden() -> ApiError {
    ApiError::Forbidden {
        code: ProblemCode::Forbidden,
    }
}

async fn path_value(parts: &mut Parts, state: &AppState, name: &str) -> Result<String, ApiError> {
    let params = RawPathParams::from_request_parts(parts, state)
        .await
        .map_err(|_| ApiError::BadRequest {
            detail: Some("invalid path parameter".to_owned()),
            errors: Vec::new(),
        })?;
    params
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.to_owned())
        .ok_or_else(|| ApiError::Internal(Box::new(MissingPathParam)))
}

async fn grant_org(
    state: &AppState,
    parts: &mut Parts,
    principal: &Principal,
    key: &str,
    perm: OrgPerm,
) -> Result<(), ApiError> {
    let user = match principal {
        Principal::Session { user, .. } | Principal::Pat { user, .. } => user,
        Principal::ApiKey { .. } => return Err(ApiError::NotFound),
    };
    let org = load_org(state, key).await?;
    let Some(org) = org else {
        return Err(ApiError::NotFound);
    };
    if org.state != "active" {
        return Err(ApiError::NotFound);
    }
    let Some(role) = org_role(state.meta.reader(), org.id, user.id).await? else {
        return Err(ApiError::NotFound);
    };
    if org_rank(role) < org_rank(perm.min_role()) {
        return Err(forbidden());
    }
    parts.extensions.insert(OrgAccess { org, role });
    Ok(())
}

async fn load_org(state: &AppState, key: &str) -> Result<Option<OrgRow>, ApiError> {
    let pool = state.meta.reader();
    if let Ok(id) = key.parse::<OrgId>() {
        return Ok(org_by_id(pool, id).await?);
    }
    Ok(org_by_slug(pool, key).await?)
}

async fn grant_project(
    state: &AppState,
    parts: &mut Parts,
    principal: &Principal,
    key: &str,
    perm: ProjectPerm,
    session_only: bool,
) -> Result<(), ApiError> {
    match principal {
        Principal::Session { user, .. } | Principal::Pat { user, .. } => {
            if session_only && !matches!(principal, Principal::Session { .. }) {
                return Err(forbidden());
            }
            grant_session_project(state, parts, user, key, perm).await
        }
        Principal::ApiKey {
            project_id, scopes, ..
        } => {
            // A project this key does not own is outside its reach (D138).
            // Scope and session-only stay 403, and only on its own project.
            if !key_owns_project(state, key, *project_id).await? {
                return Err(ApiError::NotFound);
            }
            if session_only {
                return Err(forbidden());
            }
            grant_key_project(state, parts, *project_id, *scopes, key, perm).await
        }
    }
}

/// Whether `key` is this API key's active project. `current` means that project.
async fn key_owns_project(
    state: &AppState,
    key: &str,
    project_id: ProjectId,
) -> Result<bool, ApiError> {
    let project = if key == "current" {
        project_by_id(state.meta.reader(), project_id).await?
    } else {
        load_project(state, key).await?
    };
    let Some(project) = project else {
        return Ok(false);
    };
    if project.id != project_id || project.state != "active" {
        return Ok(false);
    }
    match org_by_id(state.meta.reader(), project.org_id).await? {
        Some(org) if org.state == "active" => Ok(true),
        _ => Ok(false),
    }
}

/// The project `key` names, as `user` reaches it with `perm` (D138). `key` is a project UUID
/// or `org-slug/project-slug`. 404 when the project or its org is missing or not active, when
/// the user is not in the org, when an override hides it, and for `current` (a key's project);
/// 403 when the effective role is below `perm.min_role()`.
///
/// # Errors
///
/// [`ApiError::NotFound`], [`ApiError::Forbidden`], or a `meta.db` failure.
pub(crate) async fn user_project(
    state: &AppState,
    user: &crate::auth::principal::AuthUser,
    key: &str,
    perm: ProjectPerm,
) -> Result<ProjectAccess, ApiError> {
    if key == "current" {
        return Err(ApiError::NotFound);
    }
    let project = load_project(state, key).await?;
    let Some(project) = project else {
        return Err(ApiError::NotFound);
    };
    let Some(org) = org_by_id(state.meta.reader(), project.org_id).await? else {
        return Err(ApiError::NotFound);
    };
    if project.state != "active" || org.state != "active" {
        return Err(ApiError::NotFound);
    }
    let Some(org_role) = org_role(state.meta.reader(), org.id, user.id).await? else {
        return Err(ApiError::NotFound);
    };
    let ov = project_override(state.meta.reader(), project.id, user.id).await?;
    let Some(role) = roles::effective_project_role(org_role, ov) else {
        return Err(ApiError::NotFound);
    };
    if role < perm.min_role() {
        return Err(forbidden());
    }
    Ok(ProjectAccess {
        project,
        role: Some(role),
    })
}

async fn grant_session_project(
    state: &AppState,
    parts: &mut Parts,
    user: &crate::auth::principal::AuthUser,
    key: &str,
    perm: ProjectPerm,
) -> Result<(), ApiError> {
    let access = user_project(state, user, key, perm).await?;
    parts.extensions.insert(access);
    Ok(())
}

async fn grant_key_project(
    state: &AppState,
    parts: &mut Parts,
    project_id: ProjectId,
    scopes: KeyScopes,
    key: &str,
    perm: ProjectPerm,
) -> Result<(), ApiError> {
    let Some(scope) = perm.key_scope() else {
        return Err(forbidden());
    };
    if !scopes.contains(scope) {
        return Err(forbidden());
    }
    let reads_by_post = perm == ProjectPerm::Query && parts.method == axum::http::Method::POST;
    if scope == KeyScope::Read
        && !matches!(
            parts.method,
            axum::http::Method::GET | axum::http::Method::HEAD
        )
        && !reads_by_post
    {
        return Err(forbidden());
    }
    let project = if key == "current" {
        project_by_id(state.meta.reader(), project_id).await?
    } else {
        load_project(state, key).await?
    };
    let Some(project) = project else {
        return Err(ApiError::NotFound);
    };
    if project.id != project_id || project.state != "active" {
        return Err(ApiError::NotFound);
    }
    let Some(org) = org_by_id(state.meta.reader(), project.org_id).await? else {
        return Err(ApiError::NotFound);
    };
    if org.state != "active" {
        return Err(ApiError::NotFound);
    }
    parts.extensions.insert(ProjectAccess {
        project,
        role: None,
    });
    Ok(())
}

/// The key's own project, for an [`Access::Key`] route. The route has no
/// `{project}` in its path, so the project comes from the key. A project or org
/// that is not `active` is 404, as for the other project routes.
async fn grant_key_only_project(
    state: &AppState,
    parts: &mut Parts,
    project_id: ProjectId,
) -> Result<(), ApiError> {
    let project = key_only_project(state, project_id).await?;
    parts.extensions.insert(ProjectAccess {
        project,
        role: None,
    });
    Ok(())
}

/// The active project `project_id` in an active org, shared by HTTP and gRPC ingest.
pub(crate) async fn key_only_project(
    state: &AppState,
    project_id: ProjectId,
) -> Result<ProjectRow, ApiError> {
    let Some(project) = project_by_id(state.meta.reader(), project_id).await? else {
        return Err(ApiError::NotFound);
    };
    if project.state != "active" {
        return Err(ApiError::NotFound);
    }
    let Some(org) = org_by_id(state.meta.reader(), project.org_id).await? else {
        return Err(ApiError::NotFound);
    };
    if org.state != "active" {
        return Err(ApiError::NotFound);
    }
    Ok(project)
}

async fn load_project(state: &AppState, key: &str) -> Result<Option<ProjectRow>, ApiError> {
    let pool = state.meta.reader();
    if let Ok(id) = key.parse::<ProjectId>() {
        return Ok(project_by_id(pool, id).await?);
    }
    let Some((org_slug, project_slug)) = key.split_once('/') else {
        return Ok(None);
    };
    if org_slug.is_empty() || project_slug.is_empty() {
        return Ok(None);
    }
    Ok(project_by_slugs(pool, org_slug, project_slug).await?)
}

#[derive(Debug)]
struct MissingPathParam;

impl fmt::Display for MissingPathParam {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("path parameter is missing")
    }
}

impl std::error::Error for MissingPathParam {}

#[cfg(test)]
#[path = "access_suite.rs"]
mod access_suite;

#[cfg(test)]
mod tests {
    use axum::Json;
    use axum::extract::Extension;
    use axum::http::{Method, StatusCode};
    use axum::routing::{delete, get, patch, post, put};
    use serde_json::{Value, json};
    use utoipa::openapi::security::SecurityRequirement;

    use super::*;
    use crate::auth::roles::{ProjectOverride, org_role_str};
    use crate::auth::secret::{self, KeyKind};
    use crate::auth::session::start_session;
    use crate::meta::api_keys::{self, NewApiKey};
    use crate::meta::members::{add_org_member, set_project_override};
    use crate::meta::orgs::insert_org;
    use crate::meta::personal_tokens::{self, NewToken};
    use crate::meta::projects::insert_project;
    use crate::meta::users::{self, NewUser, insert_user};
    use crate::test_support::{TestApp, TestResponse};

    struct People {
        ada: HeaderValueCookie,
        admin: HeaderValueCookie,
        member: HeaderValueCookie,
        viewer: HeaderValueCookie,
        bob: HeaderValueCookie,
        org_id: String,
        project_id: String,
    }

    struct HeaderValueCookie(String);

    fn table() -> Vec<RouteDef> {
        vec![
            route(Method::GET, "/t/public", Access::Public, get(echo_empty)),
            route(
                Method::GET,
                "/t/user",
                Access::User {
                    session_only: false,
                },
                get(echo_user),
            ),
            route(
                Method::POST,
                "/t/user",
                Access::User {
                    session_only: false,
                },
                post(echo_user),
            ),
            route(
                Method::GET,
                "/t/session",
                Access::User { session_only: true },
                get(echo_user),
            ),
            route(
                Method::GET,
                "/t/admin",
                Access::InstanceAdmin,
                get(echo_user),
            ),
            route(
                Method::GET,
                "/t/orgs/{org}",
                Access::Org {
                    perm: OrgPerm::View,
                    session_only: false,
                },
                get(echo_org),
            ),
            route(
                Method::GET,
                "/t/orgs/{org}/members",
                Access::Org {
                    perm: OrgPerm::ManageMembers,
                    session_only: false,
                },
                get(echo_org),
            ),
            route(
                Method::GET,
                "/t/projects/{project}",
                Access::Project {
                    perm: ProjectPerm::View,
                    session_only: false,
                },
                get(echo_project),
            ),
            route(
                Method::GET,
                "/t/projects/{project}/settings",
                Access::Project {
                    perm: ProjectPerm::ManageSettings,
                    session_only: false,
                },
                get(echo_project),
            ),
            route(
                Method::POST,
                "/api/auth/probe",
                Access::Public,
                post(echo_empty),
            ),
        ]
    }

    fn route(
        method: Method,
        path: &'static str,
        access: Access,
        handler: MethodRouter<AppState>,
    ) -> RouteDef {
        RouteDef {
            method,
            path,
            access,
            documented: false,
            handler,
        }
    }

    async fn echo_empty() -> Json<Value> {
        Json(json!({"org": null, "project": null, "role": null}))
    }

    async fn echo_user(Extension(principal): Extension<Principal>) -> Json<Value> {
        let user = match principal {
            Principal::Session { user, .. } | Principal::Pat { user, .. } => user,
            Principal::ApiKey { .. } => {
                return Json(json!({
                    "org": null,
                    "project": null,
                    "role": null,
                    "admin": false,
                }));
            }
        };
        Json(json!({
            "org": null,
            "project": null,
            "role": null,
            "admin": user.is_instance_admin,
        }))
    }

    async fn echo_org(Extension(access): Extension<OrgAccess>) -> Json<Value> {
        Json(json!({
            "org": access.org.slug,
            "project": null,
            "role": org_role_str(access.role),
        }))
    }

    async fn echo_project(Extension(access): Extension<ProjectAccess>) -> Json<Value> {
        Json(json!({
            "org": null,
            "project": access.project.slug,
            "role": access.role.map(role_name),
        }))
    }

    fn role_name(role: Role) -> &'static str {
        match role {
            Role::Viewer => "viewer",
            Role::Member => "member",
            Role::Admin => "admin",
        }
    }

    async fn people(app: &TestApp) -> People {
        let now = app.state.clock.now_ns();
        let (ada, admin, member, viewer, bob, org_id, project_id) = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    let ada = insert_user(conn, user("ada@example.com", true), now).await?;
                    let admin = insert_user(conn, user("admin@example.com", false), now).await?;
                    let member = insert_user(conn, user("member@example.com", false), now).await?;
                    let viewer = insert_user(conn, user("viewer@example.com", false), now).await?;
                    let bob = insert_user(conn, user("bob@example.com", false), now).await?;
                    let acme = insert_org(conn, "Acme", "acme", now).await?;
                    let other = insert_org(conn, "Other", "other", now).await?;
                    add_org_member(conn, acme.id, ada.id, OrgRole::Owner, now).await?;
                    add_org_member(conn, acme.id, admin.id, OrgRole::Admin, now).await?;
                    add_org_member(conn, acme.id, member.id, OrgRole::Member, now).await?;
                    add_org_member(conn, acme.id, viewer.id, OrgRole::Viewer, now).await?;
                    add_org_member(conn, other.id, bob.id, OrgRole::Owner, now).await?;
                    let project =
                        insert_project(conn, acme.id, "Support", "support-bot", None, now).await?;
                    set_project_override(conn, project.id, admin.id, ProjectOverride::Viewer)
                        .await?;
                    set_project_override(conn, project.id, member.id, ProjectOverride::None)
                        .await?;
                    set_project_override(conn, project.id, viewer.id, ProjectOverride::Admin)
                        .await?;
                    Ok((
                        ada.id,
                        admin.id,
                        member.id,
                        viewer.id,
                        bob.id,
                        acme.id.to_string(),
                        project.id.to_string(),
                    ))
                })
            })
            .await
            .unwrap();
        People {
            ada: cookie(app, ada).await,
            admin: cookie(app, admin).await,
            member: cookie(app, member).await,
            viewer: cookie(app, viewer).await,
            bob: cookie(app, bob).await,
            org_id,
            project_id,
        }
    }

    fn user(email: &str, admin: bool) -> NewUser {
        NewUser {
            email: email.to_owned(),
            name: None,
            password_hash: None,
            email_verified_at: None,
            is_instance_admin: admin,
        }
    }

    async fn cookie(app: &TestApp, user_id: uuid::Uuid) -> HeaderValueCookie {
        let header = start_session(&app.state, user_id, None, None)
            .await
            .unwrap();
        let pair = header.to_str().unwrap().split_once(';').unwrap().0;
        HeaderValueCookie(pair.to_owned())
    }

    #[tokio::test]
    async fn access_and_csrf_follow_the_route_table() {
        let app = TestApp::with_route_table(table()).await;
        let people = people(&app).await;

        let public = app.request(Method::GET, "/t/public").send().await;
        assert_eq!(public.status, 200);

        let anonymous = app.request(Method::GET, "/t/user").send().await;
        assert_eq!(anonymous.status, 401);

        let signed_in = app
            .request(Method::GET, "/t/user")
            .header("cookie", &people.ada.0)
            .send()
            .await;
        assert_eq!(signed_in.status, 200);
        assert!(signed_in.json::<Value>()["admin"].as_bool().unwrap());

        let session_only = app
            .request(Method::GET, "/t/session")
            .header("cookie", &people.ada.0)
            .send()
            .await;
        assert_eq!(session_only.status, 200);

        let admin = app
            .request(Method::GET, "/t/admin")
            .header("cookie", &people.ada.0)
            .send()
            .await;
        assert_eq!(admin.status, 200);
        let not_admin = app
            .request(Method::GET, "/t/admin")
            .header("cookie", &people.bob.0)
            .send()
            .await;
        assert_eq!(not_admin.status, 403);

        let by_slug = app
            .request(Method::GET, "/t/orgs/acme")
            .header("cookie", &people.ada.0)
            .send()
            .await;
        assert_eq!(by_slug.status, 200);
        assert_eq!(by_slug.json::<Value>()["role"], "owner");
        let by_id = app
            .request(Method::GET, &format!("/t/orgs/{}", people.org_id))
            .header("cookie", &people.ada.0)
            .send()
            .await;
        assert_eq!(by_id.status, 200);
        let outsider = app
            .request(Method::GET, "/t/orgs/acme")
            .header("cookie", &people.bob.0)
            .send()
            .await;
        assert_eq!(outsider.status, 404);
        let viewer_members = app
            .request(Method::GET, "/t/orgs/acme/members")
            .header("cookie", &people.viewer.0)
            .send()
            .await;
        assert_eq!(viewer_members.status, 403);
        let unknown = app
            .request(Method::GET, "/t/orgs/missing")
            .header("cookie", &people.ada.0)
            .send()
            .await;
        assert_eq!(unknown.status, 404);

        let by_project_id = app
            .request(Method::GET, &format!("/t/projects/{}", people.project_id))
            .header("cookie", &people.ada.0)
            .send()
            .await;
        assert_eq!(by_project_id.status, 200);
        assert_eq!(by_project_id.json::<Value>()["project"], "support-bot");
        let by_slugs = app
            .request(Method::GET, "/t/projects/acme%2Fsupport-bot")
            .header("cookie", &people.ada.0)
            .send()
            .await;
        assert_eq!(by_slugs.status, 200);
        assert_eq!(by_slugs.json::<Value>()["role"], "admin");
        let bob_project = app
            .request(Method::GET, "/t/projects/acme%2Fsupport-bot")
            .header("cookie", &people.bob.0)
            .send()
            .await;
        assert_eq!(bob_project.status, 404);
        let hidden = app
            .request(Method::GET, "/t/projects/acme%2Fsupport-bot")
            .header("cookie", &people.member.0)
            .send()
            .await;
        assert_eq!(hidden.status, 404);
        let raised = app
            .request(Method::GET, "/t/projects/acme%2Fsupport-bot/settings")
            .header("cookie", &people.viewer.0)
            .send()
            .await;
        assert_eq!(raised.status, 200);
        assert_eq!(raised.json::<Value>()["role"], "admin");
        let not_lowered = app
            .request(Method::GET, "/t/projects/acme%2Fsupport-bot/settings")
            .header("cookie", &people.admin.0)
            .send()
            .await;
        assert_eq!(not_lowered.status, 200);
        assert_eq!(not_lowered.json::<Value>()["role"], "admin");
        let current = app
            .request(Method::GET, "/t/projects/current")
            .header("cookie", &people.ada.0)
            .send()
            .await;
        assert_eq!(current.status, 404);

        let evil = app
            .request(Method::POST, "/t/user")
            .header("cookie", &people.ada.0)
            .origin(Some("http://evil.example"))
            .send()
            .await;
        assert_eq!(evil.status, 403);
        assert_eq!(
            evil.problem().detail.as_deref(),
            Some("origin check failed")
        );
        let missing_origin = app
            .request(Method::POST, "/t/user")
            .header("cookie", &people.ada.0)
            .origin(None)
            .send()
            .await;
        assert_eq!(missing_origin.status, 403);
        let same_origin = app
            .request(Method::POST, "/t/user")
            .header("cookie", &people.ada.0)
            .send()
            .await;
        assert_eq!(same_origin.status, 200);
        let auth_post = app
            .request(Method::POST, "/api/auth/probe")
            .origin(None)
            .send()
            .await;
        assert_eq!(auth_post.status, 403);
        let get_without_origin = app
            .request(Method::GET, "/t/public")
            .origin(None)
            .send()
            .await;
        assert_eq!(get_without_origin.status, 200);

        let basic = app
            .request(Method::GET, "/t/user")
            .header("authorization", "Basic eHg=")
            .header("cookie", &people.ada.0)
            .send()
            .await;
        assert_eq!(basic.status, 401);
        let bearer = app
            .request(Method::GET, "/t/user")
            .header("authorization", "Bearer x")
            .header("cookie", &people.ada.0)
            .send()
            .await;
        assert_eq!(bearer.status, 401);
    }

    #[tokio::test]
    async fn a_deleting_org_or_project_is_not_found() {
        let app = TestApp::with_route_table(table()).await;
        let people = people(&app).await;
        app.state
            .meta
            .write(|conn| {
                Box::pin(async move {
                    sqlx::query(
                        "UPDATE projects SET state = 'deleting' WHERE slug = 'support-bot'",
                    )
                    .execute(&mut *conn)
                    .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        let project = app
            .request(Method::GET, "/t/projects/acme%2Fsupport-bot")
            .header("cookie", &people.ada.0)
            .send()
            .await;
        assert_eq!(project.status, 404);
        app.state
            .meta
            .write(|conn| {
                Box::pin(async move {
                    sqlx::query("UPDATE orgs SET state = 'deleting' WHERE slug = 'acme'")
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        let org = app
            .request(Method::GET, "/t/orgs/acme")
            .header("cookie", &people.ada.0)
            .send()
            .await;
        assert_eq!(org.status, 404);
    }

    async fn key_route(Extension(_access): Extension<ProjectAccess>) -> StatusCode {
        StatusCode::NO_CONTENT
    }

    async fn post_key(app: &TestApp, cookie: Option<&str>, bearer: Option<&str>) -> TestResponse {
        let mut request = app.request(Method::POST, "/test/key");
        if let Some(cookie) = cookie {
            request = request.header("cookie", cookie);
        }
        if let Some(bearer) = bearer {
            request = request.header("authorization", &format!("Bearer {bearer}"));
        }
        request.send().await
    }

    fn assert_outcome(response: &TestResponse, status: StatusCode, marker: &str, case: &str) {
        assert_eq!(response.status, status, "{case}");
        assert_eq!(response.header("x-test-access"), Some(marker), "{case}");
    }

    async fn personal_token(app: &TestApp, email: &str, scopes: Vec<KeyScope>) -> String {
        let user = users::user_by_email(app.state.meta.reader(), email)
            .await
            .unwrap()
            .expect("user exists");
        let minted = secret::new_key(KeyKind::Personal).unwrap();
        let plaintext = minted.plaintext.clone();
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    personal_tokens::insert_token(
                        conn,
                        NewToken {
                            user_id: user.id,
                            name: "test".to_owned(),
                            prefix: minted.prefix,
                            hash: minted.hash,
                            scopes,
                            expires_at: None,
                        },
                        now,
                    )
                    .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        plaintext
    }

    #[tokio::test]
    async fn a_key_route_admits_only_an_ingest_key_of_an_active_project() {
        let app = TestApp::with_route_table(vec![route(
            Method::POST,
            "/test/key",
            Access::Key {
                scope: KeyScope::Ingest,
            },
            post(key_route),
        )])
        .await;
        let seeded = app.seed().await;
        assert_eq!(seeded.project.slug, "support-bot");
        // Never used before the project is deleted.
        let (_, unused_key) = app.add_project(&seeded, "other-bot", None).await;
        let read_pat = personal_token(&app, "ada@example.com", vec![KeyScope::Read]).await;
        let write_pat = personal_token(
            &app,
            "ada@example.com",
            vec![KeyScope::Read, KeyScope::Write],
        )
        .await;

        let anonymous = post_key(&app, None, None).await;
        assert_outcome(
            &anonymous,
            StatusCode::UNAUTHORIZED,
            "denied-401",
            "anonymous",
        );
        let session = post_key(&app, Some(&seeded.cookie), None).await;
        assert_outcome(&session, StatusCode::FORBIDDEN, "denied-403", "session");
        let read_token = post_key(&app, None, Some(&read_pat)).await;
        assert_outcome(&read_token, StatusCode::FORBIDDEN, "denied-403", "read PAT");
        let write_token = post_key(&app, None, Some(&write_pat)).await;
        assert_outcome(
            &write_token,
            StatusCode::FORBIDDEN,
            "denied-403",
            "write PAT",
        );
        let read_key = post_key(&app, None, Some(&seeded.read_key)).await;
        assert_outcome(&read_key, StatusCode::FORBIDDEN, "denied-403", "read key");
        let public_key = post_key(&app, None, Some(&seeded.public_key)).await;
        assert_outcome(&public_key, StatusCode::FORBIDDEN, "denied-403", "pk key");
        let ingest_key = post_key(&app, None, Some(&seeded.ingest_key)).await;
        assert_outcome(&ingest_key, StatusCode::NO_CONTENT, "allowed", "ingest key");

        app.state
            .meta
            .write(|conn| {
                Box::pin(async move {
                    sqlx::query("UPDATE projects SET state = 'deleting'")
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        // The key is not cached, and its project is not active, so the
        // authenticator refuses it before the route is reached. That refusal
        // happens outside `enforce`, so there is no access marker.
        let unused = post_key(&app, None, Some(&unused_key)).await;
        assert_eq!(unused.status, StatusCode::UNAUTHORIZED, "unused key");
        // The key was cached by its successful use, so the route's own project
        // check answers 404.
        let used = post_key(&app, None, Some(&seeded.ingest_key)).await;
        assert_outcome(&used, StatusCode::NOT_FOUND, "denied-404", "used key");
    }

    #[tokio::test]
    async fn a_query_route_admits_read_credentials_on_post() {
        let app = TestApp::with_route_table(vec![
            route(
                Method::POST,
                "/t/projects/{project}/query",
                Access::Project {
                    perm: ProjectPerm::Query,
                    session_only: false,
                },
                post(echo_project),
            ),
            route(
                Method::PATCH,
                "/t/projects/{project}/query",
                Access::Project {
                    perm: ProjectPerm::Query,
                    session_only: false,
                },
                patch(echo_project),
            ),
            route(
                Method::PUT,
                "/t/projects/{project}/query",
                Access::Project {
                    perm: ProjectPerm::Query,
                    session_only: false,
                },
                put(echo_project),
            ),
            route(
                Method::DELETE,
                "/t/projects/{project}/query",
                Access::Project {
                    perm: ProjectPerm::Query,
                    session_only: false,
                },
                delete(echo_project),
            ),
            route(
                Method::POST,
                "/t/projects/{project}/view",
                Access::Project {
                    perm: ProjectPerm::View,
                    session_only: false,
                },
                post(echo_project),
            ),
        ])
        .await;
        let seeded = app.seed().await;
        let read_pat = personal_token(&app, "ada@example.com", vec![KeyScope::Read]).await;
        let write_pat = personal_token(
            &app,
            "ada@example.com",
            vec![KeyScope::Read, KeyScope::Write],
        )
        .await;
        let (_other, other_key) = app.add_project(&seeded, "other-bot", None).await;
        let now = app.state.clock.now_ns();
        let ada_id = users::user_by_email(app.state.meta.reader(), "ada@example.com")
            .await
            .unwrap()
            .expect("seeded user exists")
            .id;
        let minted = secret::new_key(KeyKind::Secret).unwrap();
        let write_only = minted.plaintext.clone();
        let (prefix, hash) = (minted.prefix, minted.hash);
        let project_id = seeded.project.id;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    api_keys::insert_api_key(
                        conn,
                        NewApiKey {
                            project_id,
                            kind: "secret",
                            scopes: vec![KeyScope::Write],
                            name: None,
                            prefix,
                            hash,
                            created_by: ada_id,
                            expires_at: None,
                        },
                        now,
                    )
                    .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        let org_id = seeded.org_id;
        let viewer_id = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    let viewer = insert_user(conn, user("viewer@example.com", false), now).await?;
                    add_org_member(conn, org_id, viewer.id, OrgRole::Viewer, now).await?;
                    Ok(viewer.id)
                })
            })
            .await
            .unwrap();
        let viewer = cookie(&app, viewer_id).await.0;

        let query = format!("/t/projects/{}/query", seeded.project.id);
        let view = format!("/t/projects/{}/view", seeded.project.id);
        let missing = "/t/projects/01900000-0000-7000-8000-000000009999/query";
        let current = "/t/projects/current/query";
        let ada = seeded.cookie.as_str();
        let read_pat = read_pat.as_str();
        let write_pat = write_pat.as_str();
        let read_key = seeded.read_key.as_str();
        let ingest_key = seeded.ingest_key.as_str();
        let write_only = write_only.as_str();
        let other_key = other_key.as_str();
        let as_ada = json!({"org": null, "project": "support-bot", "role": "admin"});
        let as_viewer = json!({"org": null, "project": "support-bot", "role": "viewer"});
        let as_key = json!({"org": null, "project": "support-bot", "role": null});
        let none = Value::Null;

        // (case, method, path, cookie, bearer, status, marker, expected body or null).
        type Case<'a> = (
            &'a str,
            Method,
            &'a str,
            Option<&'a str>,
            Option<&'a str>,
            StatusCode,
            &'a str,
            Value,
        );
        let cases: Vec<Case<'_>> = vec![
            (
                "anonymous query",
                Method::POST,
                query.as_str(),
                None,
                None,
                StatusCode::UNAUTHORIZED,
                "denied-401",
                none.clone(),
            ),
            (
                "session query",
                Method::POST,
                query.as_str(),
                Some(ada),
                None,
                StatusCode::OK,
                "allowed",
                as_ada.clone(),
            ),
            (
                "viewer session query",
                Method::POST,
                query.as_str(),
                Some(viewer.as_str()),
                None,
                StatusCode::OK,
                "allowed",
                as_viewer,
            ),
            (
                "read pat query",
                Method::POST,
                query.as_str(),
                None,
                Some(read_pat),
                StatusCode::OK,
                "allowed",
                as_ada.clone(),
            ),
            (
                "write pat query",
                Method::POST,
                query.as_str(),
                None,
                Some(write_pat),
                StatusCode::OK,
                "allowed",
                as_ada.clone(),
            ),
            (
                "read pat patch",
                Method::PATCH,
                query.as_str(),
                None,
                Some(read_pat),
                StatusCode::FORBIDDEN,
                "denied-403",
                none.clone(),
            ),
            (
                "read pat put",
                Method::PUT,
                query.as_str(),
                None,
                Some(read_pat),
                StatusCode::FORBIDDEN,
                "denied-403",
                none.clone(),
            ),
            (
                "read pat delete",
                Method::DELETE,
                query.as_str(),
                None,
                Some(read_pat),
                StatusCode::FORBIDDEN,
                "denied-403",
                none.clone(),
            ),
            (
                "write pat patch",
                Method::PATCH,
                query.as_str(),
                None,
                Some(write_pat),
                StatusCode::OK,
                "allowed",
                as_ada.clone(),
            ),
            (
                "read pat view",
                Method::POST,
                view.as_str(),
                None,
                Some(read_pat),
                StatusCode::FORBIDDEN,
                "denied-403",
                none.clone(),
            ),
            (
                "read pat missing project",
                Method::POST,
                missing,
                None,
                Some(read_pat),
                StatusCode::NOT_FOUND,
                "denied-404",
                none.clone(),
            ),
            (
                "session current",
                Method::POST,
                current,
                Some(ada),
                None,
                StatusCode::NOT_FOUND,
                "denied-404",
                none.clone(),
            ),
            (
                "pat current",
                Method::POST,
                current,
                None,
                Some(read_pat),
                StatusCode::NOT_FOUND,
                "denied-404",
                none.clone(),
            ),
            (
                "read key query",
                Method::POST,
                query.as_str(),
                None,
                Some(read_key),
                StatusCode::OK,
                "allowed",
                as_key.clone(),
            ),
            (
                "read key current",
                Method::POST,
                current,
                None,
                Some(read_key),
                StatusCode::OK,
                "allowed",
                as_key.clone(),
            ),
            (
                "read key patch",
                Method::PATCH,
                query.as_str(),
                None,
                Some(read_key),
                StatusCode::FORBIDDEN,
                "denied-403",
                none.clone(),
            ),
            (
                "read key put",
                Method::PUT,
                query.as_str(),
                None,
                Some(read_key),
                StatusCode::FORBIDDEN,
                "denied-403",
                none.clone(),
            ),
            (
                "read key delete",
                Method::DELETE,
                query.as_str(),
                None,
                Some(read_key),
                StatusCode::FORBIDDEN,
                "denied-403",
                none.clone(),
            ),
            (
                "read key view",
                Method::POST,
                view.as_str(),
                None,
                Some(read_key),
                StatusCode::FORBIDDEN,
                "denied-403",
                none.clone(),
            ),
            (
                "ingest key query",
                Method::POST,
                query.as_str(),
                None,
                Some(ingest_key),
                StatusCode::FORBIDDEN,
                "denied-403",
                none.clone(),
            ),
            (
                "write-only key query",
                Method::POST,
                query.as_str(),
                None,
                Some(write_only),
                StatusCode::FORBIDDEN,
                "denied-403",
                none.clone(),
            ),
            (
                "other project key",
                Method::POST,
                query.as_str(),
                None,
                Some(other_key),
                StatusCode::NOT_FOUND,
                "denied-404",
                none.clone(),
            ),
        ];
        for (case, method, path, cookie, bearer, status, marker, body) in cases {
            let mut request = app.request(method, path).json(&json!({}));
            if let Some(cookie) = cookie {
                request = request.header("cookie", cookie);
            }
            if let Some(bearer) = bearer {
                request = request.header("authorization", &format!("Bearer {bearer}"));
            }
            let response = request.send().await;
            assert_outcome(&response, status, marker, case);
            if body != Value::Null {
                assert_eq!(response.json::<Value>(), body, "{case}");
            }
        }
    }

    #[test]
    fn reads_by_post_needs_post_and_query() {
        let query = Access::Project {
            perm: ProjectPerm::Query,
            session_only: false,
        };
        assert!(query.reads_by_post(&Method::POST));
        for method in [Method::PATCH, Method::PUT, Method::DELETE] {
            assert!(!query.reads_by_post(&method), "{method}");
        }
        let view = Access::Project {
            perm: ProjectPerm::View,
            session_only: false,
        };
        assert!(!view.reads_by_post(&Method::POST));
        assert!(
            !Access::User {
                session_only: false
            }
            .reads_by_post(&Method::POST)
        );
    }

    #[test]
    fn route_table_matches_parameters_and_openapi_security() {
        let document = crate::api::openapi::openapi();
        let both = vec![
            SecurityRequirement::new("session", [] as [&str; 0]),
            SecurityRequirement::new("bearer", [] as [&str; 0]),
        ];
        assert!(document.security.as_ref() == Some(&both));
        for route in routes() {
            let has_org = route.path.split('/').any(|segment| segment == "{org}");
            let has_project = route.path.split('/').any(|segment| segment == "{project}");
            match route.access {
                Access::Org { .. } => assert!(has_org, "{}", route.path),
                Access::Project { .. } => assert!(has_project, "{}", route.path),
                _ => {
                    assert!(!has_org, "{}", route.path);
                    assert!(!has_project, "{}", route.path);
                }
            }
            if !route.documented {
                continue;
            }
            let item = document
                .paths
                .paths
                .get(route.path)
                .unwrap_or_else(|| panic!("missing {}", route.path));
            let operation = match route.method {
                Method::GET => item.get.as_ref(),
                Method::POST => item.post.as_ref(),
                Method::PUT => item.put.as_ref(),
                Method::PATCH => item.patch.as_ref(),
                Method::DELETE => item.delete.as_ref(),
                Method::HEAD => item.head.as_ref(),
                _ => None,
            }
            .unwrap_or_else(|| panic!("missing {} {}", route.method, route.path));
            let expected = match route.access {
                // Key routes are bearer-only (D-G-15).
                Access::Key { .. } => {
                    Some(vec![SecurityRequirement::new("bearer", [] as [&str; 0])])
                }
                Access::Public => Some(Vec::new()),
                Access::InstanceAdmin
                | Access::User { session_only: true }
                | Access::Org {
                    session_only: true, ..
                }
                | Access::Project {
                    session_only: true, ..
                } => Some(vec![SecurityRequirement::new("session", [] as [&str; 0])]),
                Access::User {
                    session_only: false,
                }
                | Access::Org {
                    session_only: false,
                    ..
                }
                | Access::Project {
                    session_only: false,
                    ..
                }
                | Access::Pat => None,
            };
            assert!(
                operation.security == expected,
                "{} {}",
                route.method,
                route.path
            );
        }
    }
}
