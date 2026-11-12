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
    routes.extend(crate::routes::me::routes());
    routes.extend(crate::routes::orgs::routes());
    routes.extend(crate::routes::projects::routes());
    routes.extend(crate::routes::members::routes());
    routes.extend(crate::routes::api_keys::routes());
    routes.extend(crate::routes::tokens::routes());
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
    // is 403 even when the resource would be 404.
    if let Principal::Pat { write: false, .. } = &principal
        && !matches!(
            parts.method,
            axum::http::Method::GET | axum::http::Method::HEAD
        )
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

async fn grant_session_project(
    state: &AppState,
    parts: &mut Parts,
    user: &crate::auth::principal::AuthUser,
    key: &str,
    perm: ProjectPerm,
) -> Result<(), ApiError> {
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
    parts.extensions.insert(ProjectAccess {
        project,
        role: Some(role),
    });
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
    if scope == KeyScope::Read
        && !matches!(
            parts.method,
            axum::http::Method::GET | axum::http::Method::HEAD
        )
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
    use axum::http::Method;
    use axum::routing::{get, post};
    use serde_json::{Value, json};
    use utoipa::openapi::security::SecurityRequirement;

    use super::*;
    use crate::auth::roles::{ProjectOverride, org_role_str};
    use crate::auth::session::start_session;
    use crate::meta::members::{add_org_member, set_project_override};
    use crate::meta::orgs::insert_org;
    use crate::meta::projects::insert_project;
    use crate::meta::users::{NewUser, insert_user};
    use crate::test_support::TestApp;

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
                } => None,
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
