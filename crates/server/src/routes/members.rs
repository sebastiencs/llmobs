//! Org members and per-project overrides (spec 08, D139).
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashMap;

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, put};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::account::OrgRole;
use crate::api::common::{Page, PageParams, ProblemCode};
use crate::api::members::{
    MemberUser, OrgMember, ProjectMember, ProjectRoleOverride, SetProjectOverrideRequest,
    UpdateOrgMemberRequest,
};
use crate::auth::principal::Principal;
use crate::auth::roles::{self, OrgPerm, ProjectOverride, ProjectPerm, Role};
use crate::http::AppState;
use crate::http::access::{Access, OrgAccess, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::meta::MetaError;
use crate::meta::members::{self, MemberRow};

use super::orgs::page_named;

/// Member and override routes.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/orgs/{org}/members",
            access: Access::Org {
                perm: OrgPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(list_org_members),
        },
        RouteDef {
            method: axum::http::Method::PATCH,
            path: "/api/v1/orgs/{org}/members/{user}",
            access: Access::Org {
                perm: OrgPerm::ManageMembers,
                session_only: true,
            },
            documented: true,
            handler: patch(update_org_member),
        },
        RouteDef {
            method: axum::http::Method::DELETE,
            path: "/api/v1/orgs/{org}/members/{user}",
            access: Access::Org {
                perm: OrgPerm::View,
                session_only: true,
            },
            documented: true,
            handler: delete(remove_org_member),
        },
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/projects/{project}/members",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(list_project_members),
        },
        RouteDef {
            method: axum::http::Method::PUT,
            path: "/api/v1/projects/{project}/members/{user}",
            access: Access::Project {
                perm: ProjectPerm::ManageMembers,
                session_only: true,
            },
            documented: true,
            handler: put(set_project_override),
        },
        RouteDef {
            method: axum::http::Method::DELETE,
            path: "/api/v1/projects/{project}/members/{user}",
            access: Access::Project {
                perm: ProjectPerm::ManageMembers,
                session_only: true,
            },
            documented: true,
            handler: delete(remove_project_override),
        },
    ]
}

#[derive(Debug, Deserialize)]
struct UserPath {
    user: Uuid,
}

enum MemberChange {
    Done,
    Missing,
    Forbidden,
}

async fn list_org_members(
    State(state): State<AppState>,
    Extension(access): Extension<OrgAccess>,
    error::Query(params): error::Query<PageParams>,
) -> Result<error::Json<Page<OrgMember>>, ApiError> {
    let members = members::org_members(state.meta.reader(), access.org.id).await?;
    let rows = members
        .into_iter()
        .map(|member| {
            (
                member.user.email.clone(),
                member.user.id.to_string(),
                org_member(&member),
            )
        })
        .collect();
    Ok(error::Json(page_named(
        rows,
        params.cursor.as_deref(),
        params.limit.get(),
    )?))
}

async fn update_org_member(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<OrgAccess>,
    error::Path(UserPath { user: target }): error::Path<UserPath>,
    error::Json(body): error::Json<UpdateOrgMemberRequest>,
) -> Result<error::Json<OrgMember>, ApiError> {
    let Principal::Session { user, .. } = &principal;
    let actor = user.id;
    let org_id = access.org.id;
    let role = body.role;
    let outcome = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                let Some(actor_role) = members::org_role_tx(conn, org_id, actor).await? else {
                    return Ok(MemberChange::Forbidden);
                };
                if roles::org_rank(actor_role) < roles::org_rank(OrgRole::Admin) {
                    return Ok(MemberChange::Forbidden);
                }
                let Some(target_role) = members::org_role_tx(conn, org_id, target).await? else {
                    return Ok(MemberChange::Missing);
                };
                if !may_set_role(actor_role, target_role, role) {
                    return Ok(MemberChange::Forbidden);
                }
                members::set_org_role(conn, org_id, target, role).await?;
                if matches!(role, OrgRole::Owner | OrgRole::Admin) {
                    members::clear_project_overrides(conn, org_id, target).await?;
                }
                if members::count_owners(conn, org_id).await? == 0 {
                    return Err(MetaError::Conflict("an org needs an owner"));
                }
                Ok(MemberChange::Done)
            })
        })
        .await?;
    match outcome {
        MemberChange::Done => {}
        MemberChange::Missing => return Err(ApiError::NotFound),
        MemberChange::Forbidden => return Err(forbidden()),
    }
    tracing::info!(
        target: "audit",
        org_id = %org_id,
        actor_id = %actor,
        target_id = %target,
        "member role changed"
    );
    let member = find_member(&state, org_id, target).await?;
    Ok(error::Json(org_member(&member)))
}

async fn remove_org_member(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<OrgAccess>,
    error::Path(UserPath { user: target }): error::Path<UserPath>,
) -> Result<StatusCode, ApiError> {
    let Principal::Session { user, .. } = &principal;
    let actor = user.id;
    let org_id = access.org.id;
    let outcome = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                let Some(actor_role) = members::org_role_tx(conn, org_id, actor).await? else {
                    return Ok(MemberChange::Forbidden);
                };
                let Some(target_role) = members::org_role_tx(conn, org_id, target).await? else {
                    return Ok(MemberChange::Missing);
                };
                if actor != target && !may_remove_other(actor_role, target_role) {
                    return Ok(MemberChange::Forbidden);
                }
                if target_role == OrgRole::Owner && members::count_owners(conn, org_id).await? == 1
                {
                    return Err(MetaError::Conflict("an org needs an owner"));
                }
                members::remove_org_member(conn, org_id, target).await?;
                Ok(MemberChange::Done)
            })
        })
        .await?;
    match outcome {
        MemberChange::Done => {}
        MemberChange::Missing => return Err(ApiError::NotFound),
        MemberChange::Forbidden => return Err(forbidden()),
    }
    tracing::info!(
        target: "audit",
        org_id = %org_id,
        actor_id = %actor,
        target_id = %target,
        "member removed"
    );
    Ok(StatusCode::NO_CONTENT)
}

async fn list_project_members(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Query(params): error::Query<PageParams>,
) -> Result<error::Json<Page<ProjectMember>>, ApiError> {
    let org_id = access.project.org_id;
    let project_id = access.project.id;
    let members = members::org_members(state.meta.reader(), org_id).await?;
    let overrides = members::project_overrides(state.meta.reader(), project_id).await?;
    let overrides: HashMap<Uuid, ProjectOverride> = overrides.into_iter().collect();
    let rows = members
        .iter()
        .map(|member| {
            (
                member.user.email.clone(),
                member.user.id.to_string(),
                project_member(member, overrides.get(&member.user.id).copied()),
            )
        })
        .collect();
    Ok(error::Json(page_named(
        rows,
        params.cursor.as_deref(),
        params.limit.get(),
    )?))
}

async fn set_project_override(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(UserPath { user: target }): error::Path<UserPath>,
    error::Json(body): error::Json<SetProjectOverrideRequest>,
) -> Result<error::Json<ProjectMember>, ApiError> {
    let org_id = access.project.org_id;
    let project_id = access.project.id;
    let override_role = stored_override(body.role);
    let outcome = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                let Some(role) = members::org_role_tx(conn, org_id, target).await? else {
                    return Ok(MemberChange::Missing);
                };
                if matches!(role, OrgRole::Owner | OrgRole::Admin) {
                    return Ok(MemberChange::Forbidden);
                }
                members::set_project_override(conn, project_id, target, override_role).await?;
                Ok(MemberChange::Done)
            })
        })
        .await?;
    match outcome {
        MemberChange::Done => {}
        MemberChange::Missing => return Err(ApiError::NotFound),
        MemberChange::Forbidden => {
            return Err(ApiError::BadRequest {
                detail: Some("owners and admins cannot be overridden".to_owned()),
                errors: Vec::new(),
            });
        }
    }
    let stored = members::project_override(state.meta.reader(), project_id, target).await?;
    let members = members::org_members(state.meta.reader(), org_id).await?;
    let Some(member) = members.into_iter().find(|member| member.user.id == target) else {
        return Err(ApiError::NotFound);
    };
    Ok(error::Json(project_member(&member, stored)))
}

async fn remove_project_override(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(UserPath { user: target }): error::Path<UserPath>,
) -> Result<StatusCode, ApiError> {
    let org_id = access.project.org_id;
    let project_id = access.project.id;
    let belongs = members::org_role(state.meta.reader(), org_id, target).await?;
    if belongs.is_none() {
        return Err(ApiError::NotFound);
    }
    state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                members::remove_project_override(conn, project_id, target).await?;
                Ok(())
            })
        })
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Nobody sets a role above their own, and only an owner changes an owner or grants `owner`.
fn may_set_role(actor: OrgRole, target: OrgRole, new_role: OrgRole) -> bool {
    if target == OrgRole::Owner && actor != OrgRole::Owner {
        return false;
    }
    if new_role == OrgRole::Owner && actor != OrgRole::Owner {
        return false;
    }
    roles::org_rank(new_role) <= roles::org_rank(actor)
}

/// Removing someone else needs `ManageMembers`. Only an owner removes an owner.
fn may_remove_other(actor: OrgRole, target: OrgRole) -> bool {
    if roles::org_rank(actor) < roles::org_rank(OrgRole::Admin) {
        return false;
    }
    target != OrgRole::Owner || actor == OrgRole::Owner
}

async fn find_member(
    state: &AppState,
    org_id: llmobs_core::ids::OrgId,
    user: Uuid,
) -> Result<MemberRow, ApiError> {
    let members = members::org_members(state.meta.reader(), org_id).await?;
    members
        .into_iter()
        .find(|member| member.user.id == user)
        .ok_or(ApiError::NotFound)
}

fn org_member(member: &MemberRow) -> OrgMember {
    OrgMember {
        user: member_user(member),
        role: member.role,
        joined_at: member.joined_at,
    }
}

fn project_member(member: &MemberRow, stored: Option<ProjectOverride>) -> ProjectMember {
    let effective = roles::effective_project_role(member.role, stored).map(api_role);
    ProjectMember {
        user: member_user(member),
        org_role: member.role,
        role_override: stored.map(api_override),
        effective_role: effective,
    }
}

fn member_user(member: &MemberRow) -> MemberUser {
    MemberUser {
        id: member.user.id,
        email: member.user.email.clone(),
        name: member.user.name.clone(),
    }
}

fn api_role(role: Role) -> OrgRole {
    match role {
        Role::Admin => OrgRole::Admin,
        Role::Member => OrgRole::Member,
        Role::Viewer => OrgRole::Viewer,
    }
}

fn api_override(role: ProjectOverride) -> ProjectRoleOverride {
    match role {
        ProjectOverride::Admin => ProjectRoleOverride::Admin,
        ProjectOverride::Member => ProjectRoleOverride::Member,
        ProjectOverride::Viewer => ProjectRoleOverride::Viewer,
        ProjectOverride::None => ProjectRoleOverride::None,
    }
}

fn stored_override(role: ProjectRoleOverride) -> ProjectOverride {
    match role {
        ProjectRoleOverride::Admin => ProjectOverride::Admin,
        ProjectRoleOverride::Member => ProjectOverride::Member,
        ProjectRoleOverride::Viewer => ProjectOverride::Viewer,
        ProjectRoleOverride::None => ProjectOverride::None,
    }
}

fn forbidden() -> ApiError {
    ApiError::Forbidden {
        code: ProblemCode::Forbidden,
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, header};
    use llmobs_core::ids::OrgId;
    use serde_json::Value;

    use super::*;
    use crate::api::account::{OrgMembership, Project};
    use crate::auth::session::start_session;
    use crate::meta::users::{NewUser, insert_user};
    use crate::test_support::{TestApp, capture_logs};

    fn cookie_pair(set_cookie: &axum::http::HeaderValue) -> String {
        set_cookie
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    async fn signed_in(app: &TestApp, email: &str) -> (Uuid, String) {
        let email = email.to_owned();
        let now = app.state.clock.now_ns();
        let user = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    insert_user(
                        conn,
                        NewUser {
                            email,
                            name: None,
                            password_hash: None,
                            email_verified_at: Some(now),
                            is_instance_admin: true,
                        },
                        now,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        let cookie = start_session(&app.state, user.id, None, None)
            .await
            .unwrap();
        (user.id, cookie_pair(&cookie))
    }

    async fn add_member(app: &TestApp, org: OrgId, user: Uuid, role: OrgRole) {
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move { members::add_org_member(conn, org, user, role, now).await })
            })
            .await
            .unwrap();
    }

    async fn create_org(app: &TestApp, cookie: &str, name: &str) -> OrgMembership {
        let response = app
            .request(Method::POST, "/api/v1/orgs")
            .header(header::COOKIE.as_str(), cookie)
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await;
        assert_eq!(response.status, 201);
        response.json()
    }

    async fn create_project(app: &TestApp, cookie: &str, org: &str, name: &str) -> Project {
        let response = app
            .request(Method::POST, &format!("/api/v1/orgs/{org}/projects"))
            .header(header::COOKIE.as_str(), cookie)
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await;
        assert_eq!(response.status, 201);
        response.json()
    }

    #[test]
    fn member_writes_are_session_only() {
        for route in routes() {
            let session_only = match route.access {
                Access::Org { session_only, .. } | Access::Project { session_only, .. } => {
                    session_only
                }
                other => panic!("unexpected access {other:?} on {}", route.path),
            };
            let writes = matches!(route.method, Method::PATCH | Method::PUT | Method::DELETE);
            assert_eq!(session_only, writes, "{} {}", route.method, route.path);
        }
        assert!(routes().iter().any(|route| {
            route.path == "/api/v1/orgs/{org}/members/{user}"
                && route.method == Method::PATCH
                && matches!(
                    route.access,
                    Access::Org {
                        perm: OrgPerm::ManageMembers,
                        session_only: true,
                    }
                )
        }));
        assert!(routes().iter().any(|route| {
            route.path == "/api/v1/orgs/{org}/members/{user}"
                && route.method == Method::DELETE
                && matches!(
                    route.access,
                    Access::Org {
                        perm: OrgPerm::View,
                        session_only: true,
                    }
                )
        }));
    }

    #[tokio::test]
    async fn roles_follow_the_ceiling_and_keep_an_owner() {
        let app = TestApp::new().await;
        let (ada_id, ada) = signed_in(&app, "ada@example.com").await;
        let (admin_id, admin) = signed_in(&app, "admin@example.com").await;
        let (member_id, _) = signed_in(&app, "member@example.com").await;
        let (second_id, _) = signed_in(&app, "second@example.com").await;
        let org = create_org(&app, &ada, "Acme").await;
        add_member(&app, org.id, admin_id, OrgRole::Admin).await;
        add_member(&app, org.id, member_id, OrgRole::Member).await;
        add_member(&app, org.id, second_id, OrgRole::Member).await;

        let (promoted, logs) = capture_logs(async {
            app.request(
                Method::PATCH,
                &format!("/api/v1/orgs/acme/members/{member_id}"),
            )
            .header(header::COOKIE.as_str(), &admin)
            .json(&serde_json::json!({ "role": "admin" }))
            .send()
            .await
        })
        .await;
        assert_eq!(promoted.status, 200);
        let promoted: OrgMember = promoted.json();
        assert_eq!(promoted.role, OrgRole::Admin);
        assert_eq!(promoted.user.email, "member@example.com");
        assert!(logs.contains("member role changed"));
        assert!(!logs.contains("member@example.com"));
        assert!(!logs.contains("admin@example.com"));
        let body: Value = serde_json::to_value(&promoted).unwrap();
        assert!(body["user"].get("is_instance_admin").is_none());

        let too_high = app
            .request(
                Method::PATCH,
                &format!("/api/v1/orgs/acme/members/{second_id}"),
            )
            .header(header::COOKIE.as_str(), &admin)
            .json(&serde_json::json!({ "role": "owner" }))
            .send()
            .await;
        assert_eq!(too_high.status, 403);
        assert!(too_high.problem().detail.is_none());

        let demote_owner = app
            .request(
                Method::PATCH,
                &format!("/api/v1/orgs/acme/members/{ada_id}"),
            )
            .header(header::COOKIE.as_str(), &admin)
            .json(&serde_json::json!({ "role": "member" }))
            .send()
            .await;
        assert_eq!(demote_owner.status, 403);

        let sole = app
            .request(
                Method::PATCH,
                &format!("/api/v1/orgs/acme/members/{ada_id}"),
            )
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "role": "admin" }))
            .send()
            .await;
        assert_eq!(sole.status, 409);
        assert_eq!(
            sole.problem().detail.as_deref(),
            Some("an org needs an owner")
        );

        let granted = app
            .request(
                Method::PATCH,
                &format!("/api/v1/orgs/acme/members/{second_id}"),
            )
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "role": "owner" }))
            .send()
            .await;
        assert_eq!(granted.status, 200);
        let granted: OrgMember = granted.json();
        assert_eq!(granted.role, OrgRole::Owner);

        let stepped_down = app
            .request(
                Method::PATCH,
                &format!("/api/v1/orgs/acme/members/{ada_id}"),
            )
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "role": "admin" }))
            .send()
            .await;
        assert_eq!(stepped_down.status, 200);
        let stepped_down: OrgMember = stepped_down.json();
        assert_eq!(stepped_down.role, OrgRole::Admin);
    }

    #[tokio::test]
    async fn removal_allows_self_and_keeps_the_last_owner() {
        let app = TestApp::new().await;
        let (ada_id, ada) = signed_in(&app, "ada@example.com").await;
        let (member_id, member) = signed_in(&app, "member@example.com").await;
        let (viewer_id, viewer) = signed_in(&app, "viewer@example.com").await;
        let (other_id, other) = signed_in(&app, "other@example.com").await;
        let org = create_org(&app, &ada, "Acme").await;
        add_member(&app, org.id, member_id, OrgRole::Member).await;
        add_member(&app, org.id, viewer_id, OrgRole::Viewer).await;
        add_member(&app, org.id, other_id, OrgRole::Owner).await;

        let gone = app
            .request(
                Method::DELETE,
                &format!("/api/v1/orgs/acme/members/{viewer_id}"),
            )
            .header(header::COOKIE.as_str(), &viewer)
            .send()
            .await;
        assert_eq!(gone.status, 204);

        let denied = app
            .request(
                Method::DELETE,
                &format!("/api/v1/orgs/acme/members/{ada_id}"),
            )
            .header(header::COOKIE.as_str(), &member)
            .send()
            .await;
        assert_eq!(denied.status, 403);

        let last = app
            .request(
                Method::DELETE,
                &format!("/api/v1/orgs/acme/members/{ada_id}"),
            )
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        // Another owner remains, so this succeeds. Then the remaining owner cannot leave.
        assert_eq!(last.status, 204);
        let stuck = app
            .request(
                Method::DELETE,
                &format!("/api/v1/orgs/acme/members/{other_id}"),
            )
            .header(header::COOKIE.as_str(), &other)
            .send()
            .await;
        assert_eq!(stuck.status, 409);
        assert_eq!(
            stuck.problem().detail.as_deref(),
            Some("an org needs an owner")
        );
    }

    #[tokio::test]
    async fn promotion_to_admin_clears_overrides() {
        let app = TestApp::new().await;
        let (_, ada) = signed_in(&app, "ada@example.com").await;
        let (member_id, _) = signed_in(&app, "member@example.com").await;
        let org = create_org(&app, &ada, "Acme").await;
        add_member(&app, org.id, member_id, OrgRole::Member).await;
        let project = create_project(&app, &ada, "acme", "Support").await;
        let project_id = project.id;
        let user_id = member_id;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    members::set_project_override(conn, project_id, user_id, ProjectOverride::None)
                        .await
                })
            })
            .await
            .unwrap();

        let promoted = app
            .request(
                Method::PATCH,
                &format!("/api/v1/orgs/acme/members/{member_id}"),
            )
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "role": "admin" }))
            .send()
            .await;
        assert_eq!(promoted.status, 200);
        let stored = members::project_override(app.state.meta.reader(), project.id, member_id)
            .await
            .unwrap();
        assert!(stored.is_none());
    }

    #[tokio::test]
    async fn overrides_hide_projects_and_respect_the_ceiling() {
        let app = TestApp::new().await;
        let (_, ada) = signed_in(&app, "ada@example.com").await;
        let (admin_id, _) = signed_in(&app, "admin@example.com").await;
        let (member_id, member) = signed_in(&app, "member@example.com").await;
        let (viewer_id, viewer) = signed_in(&app, "viewer@example.com").await;
        let (outsider_id, _) = signed_in(&app, "outsider@example.com").await;
        let org = create_org(&app, &ada, "Acme").await;
        add_member(&app, org.id, admin_id, OrgRole::Admin).await;
        add_member(&app, org.id, member_id, OrgRole::Member).await;
        add_member(&app, org.id, viewer_id, OrgRole::Viewer).await;
        let first = create_project(&app, &ada, "acme", "Alpha").await;
        let second = create_project(&app, &ada, "acme", "Beta").await;

        let not_admin = app
            .request(
                Method::PUT,
                &format!("/api/v1/projects/{}/members/{viewer_id}", first.id),
            )
            .header(header::COOKIE.as_str(), &member)
            .json(&serde_json::json!({ "role": "none" }))
            .send()
            .await;
        assert_eq!(not_admin.status, 403);

        let on_admin = app
            .request(
                Method::PUT,
                &format!("/api/v1/projects/{}/members/{admin_id}", first.id),
            )
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "role": "viewer" }))
            .send()
            .await;
        assert_eq!(on_admin.status, 400);
        assert_eq!(
            on_admin.problem().detail.as_deref(),
            Some("owners and admins cannot be overridden")
        );

        let missing = app
            .request(
                Method::PUT,
                &format!("/api/v1/projects/{}/members/{outsider_id}", first.id),
            )
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "role": "none" }))
            .send()
            .await;
        assert_eq!(missing.status, 404);

        let hidden = app
            .request(
                Method::PUT,
                &format!("/api/v1/projects/{}/members/{member_id}", first.id),
            )
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "role": "none" }))
            .send()
            .await;
        assert_eq!(hidden.status, 200);
        let hidden: ProjectMember = hidden.json();
        assert_eq!(hidden.role_override, Some(ProjectRoleOverride::None));
        assert!(hidden.effective_role.is_none());

        let get_hidden = app
            .request(Method::GET, &format!("/api/v1/projects/{}", first.id))
            .header(header::COOKIE.as_str(), &member)
            .send()
            .await;
        assert_eq!(get_hidden.status, 404);
        let listed: Page<Project> = app
            .request(Method::GET, "/api/v1/orgs/acme/projects")
            .header(header::COOKIE.as_str(), &member)
            .send()
            .await
            .json();
        assert!(listed.data.iter().all(|project| project.id != first.id));
        assert!(listed.data.iter().any(|project| project.id == second.id));

        let raised = app
            .request(
                Method::PUT,
                &format!("/api/v1/projects/{}/members/{viewer_id}", first.id),
            )
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "role": "admin" }))
            .send()
            .await;
        assert_eq!(raised.status, 200);

        let managed = app
            .request(
                Method::PUT,
                &format!("/api/v1/projects/{}/members/{member_id}", first.id),
            )
            .header(header::COOKIE.as_str(), &viewer)
            .json(&serde_json::json!({ "role": "viewer" }))
            .send()
            .await;
        assert_eq!(managed.status, 200);

        let other_project = app
            .request(
                Method::PUT,
                &format!("/api/v1/projects/{}/members/{member_id}", second.id),
            )
            .header(header::COOKIE.as_str(), &viewer)
            .json(&serde_json::json!({ "role": "viewer" }))
            .send()
            .await;
        assert_eq!(other_project.status, 403);

        let cleared = app
            .request(
                Method::DELETE,
                &format!("/api/v1/projects/{}/members/{member_id}", first.id),
            )
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        assert_eq!(cleared.status, 204);
        let again = app
            .request(
                Method::DELETE,
                &format!("/api/v1/projects/{}/members/{member_id}", first.id),
            )
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        assert_eq!(again.status, 204);

        let lowered = app
            .request(
                Method::PUT,
                &format!("/api/v1/projects/{}/members/{viewer_id}", first.id),
            )
            .header(header::COOKIE.as_str(), &viewer)
            .json(&serde_json::json!({ "role": "member" }))
            .send()
            .await;
        assert_eq!(lowered.status, 200);
        let lowered: ProjectMember = lowered.json();
        assert_eq!(lowered.user.id, viewer_id);
        assert_eq!(lowered.role_override, Some(ProjectRoleOverride::Member));
        let lost = app
            .request(
                Method::PUT,
                &format!("/api/v1/projects/{}/members/{member_id}", first.id),
            )
            .header(header::COOKIE.as_str(), &viewer)
            .json(&serde_json::json!({ "role": "viewer" }))
            .send()
            .await;
        assert_eq!(lost.status, 403);
    }
}
