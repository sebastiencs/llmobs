// SPDX-License-Identifier: Elastic-2.0
//! Per-project role overrides (D139): the write routes (D-P-4, D-P-8).
//!
//! Setting an override needs `Feature::ProjectRoles`. Removing one never does, and stored
//! overrides keep applying through the core read path whatever the licence, so a lapse
//! never widens access.

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::routing::{delete, put};

use crate::api::account::OrgRole;
use crate::api::entitlements::Feature;
use crate::api::members::{ProjectMember, ProjectRoleOverride, SetProjectOverrideRequest};
use crate::auth::roles::{ProjectOverride, ProjectPerm};
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::meta::members;
use crate::routes::members::{MemberChange, UserPath, project_member};

/// The project member write routes. Both are session-only.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
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

async fn set_project_override(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(UserPath { user: target }): error::Path<UserPath>,
    error::Json(body): error::Json<SetProjectOverrideRequest>,
) -> Result<error::Json<ProjectMember>, ApiError> {
    crate::entitlements::require(&state, access.project.org_id, Feature::ProjectRoles)?;
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

fn stored_override(role: ProjectRoleOverride) -> ProjectOverride {
    match role {
        ProjectRoleOverride::Admin => ProjectOverride::Admin,
        ProjectRoleOverride::Member => ProjectOverride::Member,
        ProjectRoleOverride::Viewer => ProjectOverride::Viewer,
        ProjectRoleOverride::None => ProjectOverride::None,
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, header};
    use llmobs_core::ids::OrgId;
    use uuid::Uuid;

    use super::*;
    use crate::api::account::{OrgMembership, Project};
    use crate::api::common::{Page, ProblemCode};
    use crate::auth::session::start_session;
    use crate::config::Secret;
    use crate::meta::users::{NewUser, insert_user};
    use crate::test_support::{TestApp, license_key_until};

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

    #[tokio::test]
    async fn setting_an_override_needs_project_roles() {
        let app = TestApp::new().await;
        let (_, ada) = signed_in(&app, "ada@example.com").await;
        let (member_id, _) = signed_in(&app, "member@example.com").await;
        let org = create_org(&app, &ada, "Acme").await;
        add_member(&app, org.id, member_id, OrgRole::Member).await;
        let project = create_project(&app, &ada, "acme", "Support").await;

        let denied = app
            .request(
                Method::PUT,
                &format!("/api/v1/projects/{}/members/{member_id}", project.id),
            )
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "role": "viewer" }))
            .send()
            .await;
        assert_eq!(denied.status, 403);
        let problem = denied.problem();
        assert_eq!(problem.code, ProblemCode::PlanRequired);
        assert_eq!(
            problem.detail.as_deref(),
            Some("Per-project roles need the Teams add-on or an enterprise licence.")
        );
        let stored = members::project_override(app.state.meta.reader(), project.id, member_id)
            .await
            .unwrap();
        assert!(stored.is_none());
    }

    #[tokio::test]
    async fn removing_an_override_needs_nothing() {
        let app = TestApp::new().await;
        let (_, ada) = signed_in(&app, "ada@example.com").await;
        let (member_id, _) = signed_in(&app, "member@example.com").await;
        let org = create_org(&app, &ada, "Acme").await;
        add_member(&app, org.id, member_id, OrgRole::Member).await;
        let project = create_project(&app, &ada, "acme", "Support").await;
        let project_id = project.id;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    members::set_project_override(
                        conn,
                        project_id,
                        member_id,
                        ProjectOverride::Viewer,
                    )
                    .await
                })
            })
            .await
            .unwrap();

        let removed = app
            .request(
                Method::DELETE,
                &format!("/api/v1/projects/{}/members/{member_id}", project.id),
            )
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        assert_eq!(removed.status, 204);
        let stored = members::project_override(app.state.meta.reader(), project.id, member_id)
            .await
            .unwrap();
        assert!(stored.is_none());
    }

    #[tokio::test]
    async fn stored_overrides_keep_applying_without_a_license() {
        let app = TestApp::new().await;
        let (_, ada) = signed_in(&app, "ada@example.com").await;
        let (member_id, member) = signed_in(&app, "member@example.com").await;
        let org = create_org(&app, &ada, "Acme").await;
        add_member(&app, org.id, member_id, OrgRole::Member).await;
        let hidden = create_project(&app, &ada, "acme", "Alpha").await;
        let visible = create_project(&app, &ada, "acme", "Beta").await;
        let hidden_id = hidden.id;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    members::set_project_override(conn, hidden_id, member_id, ProjectOverride::None)
                        .await
                })
            })
            .await
            .unwrap();

        let get_hidden = app
            .request(Method::GET, &format!("/api/v1/projects/{}", hidden.id))
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
        assert!(listed.data.iter().all(|project| project.id != hidden.id));
        assert!(listed.data.iter().any(|project| project.id == visible.id));
    }

    #[tokio::test]
    async fn expired_license_locks_setting() {
        let key = license_key_until(
            &["project_roles"],
            "2025-12-01T00:00:00Z",
            "2025-12-31T00:00:00Z",
        );
        let app = TestApp::with_config(move |config| {
            config.license_key = Some(Secret::new(key));
        })
        .await;
        let (_, ada) = signed_in(&app, "ada@example.com").await;
        let (member_id, _) = signed_in(&app, "member@example.com").await;
        let org = create_org(&app, &ada, "Acme").await;
        add_member(&app, org.id, member_id, OrgRole::Member).await;
        let project = create_project(&app, &ada, "acme", "Support").await;

        let locked = app
            .request(
                Method::PUT,
                &format!("/api/v1/projects/{}/members/{member_id}", project.id),
            )
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "role": "viewer" }))
            .send()
            .await;
        assert_eq!(locked.status, 403);
        assert_eq!(locked.problem().code, ProblemCode::PlanRequired);
    }

    #[test]
    fn override_writes_are_session_only() {
        let table = routes();
        assert_eq!(table.len(), 2);
        for route in &table {
            assert!(
                matches!(route.method, Method::PUT | Method::DELETE),
                "{} {}",
                route.method,
                route.path
            );
            assert!(
                matches!(
                    route.access,
                    Access::Project {
                        perm: ProjectPerm::ManageMembers,
                        session_only: true,
                    }
                ),
                "{} {}",
                route.method,
                route.path
            );
        }
    }

    #[tokio::test]
    async fn overrides_hide_projects_and_respect_the_ceiling() {
        let app = TestApp::with_license(&["project_roles"]).await;
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
