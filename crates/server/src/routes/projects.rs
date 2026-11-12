//! Project list, create, read, and update (spec 09).
#![cfg_attr(not(test), allow(dead_code))]

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};

use crate::api::account::{CreateProjectRequest, OrgRole, Project, UpdateProjectRequest};
use crate::api::common::{FieldError, Page, PageParams};
use crate::auth::principal::{self, Principal};
use crate::auth::roles::{self, OrgPerm, ProjectPerm, Role};
use crate::http::AppState;
use crate::http::access::{Access, OrgAccess, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::meta::members::project_override;
use crate::meta::orgs::{self, SlugError};
use crate::meta::projects::{self, ProjectPatch, ProjectRow};

use super::orgs::page_named;

/// Project routes. List and get were already in the OpenAPI contract.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/orgs/{org}/projects",
            access: Access::Org {
                perm: OrgPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(list_projects),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/v1/orgs/{org}/projects",
            access: Access::Org {
                perm: OrgPerm::CreateProject,
                session_only: false,
            },
            documented: true,
            handler: post(create_project),
        },
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/projects/{project}",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(get_project),
        },
        RouteDef {
            method: axum::http::Method::PATCH,
            path: "/api/v1/projects/{project}",
            access: Access::Project {
                perm: ProjectPerm::ManageSettings,
                session_only: false,
            },
            documented: true,
            handler: patch(update_project),
        },
    ]
}

async fn list_projects(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<OrgAccess>,
    error::Query(params): error::Query<PageParams>,
) -> Result<error::Json<Page<Project>>, ApiError> {
    let user = principal::session_user(&principal)?;
    let projects = projects::projects_of_org(state.meta.reader(), access.org.id).await?;
    let mut visible = Vec::new();
    for project in projects {
        if project.state != "active" {
            continue;
        }
        let ov = project_override(state.meta.reader(), project.id, user.id).await?;
        let Some(role) = roles::effective_project_role(access.role, ov) else {
            continue;
        };
        visible.push((
            project.name.clone(),
            project.id.to_string(),
            project_json(&project, Some(role)),
        ));
    }
    Ok(error::Json(page_named(
        visible,
        params.cursor.as_deref(),
        params.limit.get(),
    )?))
}

async fn create_project(
    State(state): State<AppState>,
    Extension(access): Extension<OrgAccess>,
    error::Json(body): error::Json<CreateProjectRequest>,
) -> Result<Response, ApiError> {
    let name = trimmed_name(&body.name)?;
    if let Some(slug) = &body.slug {
        orgs::validate_slug_shape(slug).map_err(slug_error)?;
    }
    let retention = body.retention_days.map(checked_retention).transpose()?;
    let org_id = access.org.id;
    let slug = body.slug;
    let now = state.clock.now_ns();
    let project = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                match &slug {
                    Some(slug) => {
                        projects::insert_project(conn, org_id, &name, slug, retention, now).await
                    }
                    None => {
                        projects::insert_project_with_derived_slug(
                            conn, org_id, &name, retention, now,
                        )
                        .await
                    }
                }
            })
        })
        .await?;
    // CreateProject requires an org admin, whose effective project role is admin.
    Ok((
        StatusCode::CREATED,
        error::Json(project_json(&project, Some(Role::Admin))),
    )
        .into_response())
}

async fn get_project(Extension(access): Extension<ProjectAccess>) -> error::Json<Project> {
    error::Json(project_json(&access.project, access.role))
}

async fn update_project(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Json(body): error::Json<UpdateProjectRequest>,
) -> Result<error::Json<Project>, ApiError> {
    let name = match body.name {
        None => None,
        Some(name) => Some(trimmed_name(&name)?),
    };
    let retention_days = match body.retention_days {
        None => None,
        Some(None) => Some(None),
        Some(Some(days)) => Some(Some(checked_retention(days)?)),
    };
    let id = access.project.id;
    let role = access.role;
    let now = state.clock.now_ns();
    state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                projects::update_project(
                    conn,
                    id,
                    ProjectPatch {
                        name,
                        retention_days,
                    },
                    now,
                )
                .await
            })
        })
        .await?;
    let Some(project) = projects::project_by_id(state.meta.reader(), id).await? else {
        return Err(ApiError::NotFound);
    };
    Ok(error::Json(project_json(&project, role)))
}

fn project_json(project: &ProjectRow, role: Option<Role>) -> Project {
    Project {
        id: project.id,
        org_id: project.org_id,
        slug: project.slug.clone(),
        name: project.name.clone(),
        retention_days: project.retention_days,
        role: role.map(api_role),
        created_at: project.created_at,
    }
}

fn api_role(role: Role) -> OrgRole {
    match role {
        Role::Admin => OrgRole::Admin,
        Role::Member => OrgRole::Member,
        Role::Viewer => OrgRole::Viewer,
    }
}

fn checked_retention(days: u32) -> Result<u32, ApiError> {
    if (1..=36_500).contains(&days) {
        Ok(days)
    } else {
        Err(field_error("retention_days", "must be 1 to 36500"))
    }
}

fn slug_error(error: SlugError) -> ApiError {
    field_error("slug", &error.to_string())
}

fn trimmed_name(value: &str) -> Result<String, ApiError> {
    let name = value.trim();
    let length = name.chars().count();
    if !(1..=100).contains(&length) {
        return Err(field_error("name", "must be 1 to 100 characters"));
    }
    Ok(name.to_owned())
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

#[cfg(test)]
mod tests {
    use axum::http::{Method, header};
    use llmobs_core::ids::OrgId;
    use uuid::Uuid;

    use super::*;
    use crate::api::account::OrgMembership;
    use crate::api::common::ProblemCode;
    use crate::auth::roles::ProjectOverride;
    use crate::auth::session::start_session;
    use crate::meta::members::{add_org_member, set_project_override};
    use crate::meta::users::{NewUser, insert_user};
    use crate::test_support::TestApp;

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
                            is_instance_admin: false,
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
                Box::pin(async move { add_org_member(conn, org, user, role, now).await })
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn projects_hide_none_and_report_effective_roles() {
        let app = TestApp::new().await;
        let (_, ada) = signed_in(&app, "ada@example.com").await;
        let (admin_id, admin) = signed_in(&app, "admin@example.com").await;
        let (member_id, member) = signed_in(&app, "member@example.com").await;
        let (viewer_id, viewer) = signed_in(&app, "viewer@example.com").await;
        let (_, bob) = signed_in(&app, "bob@example.com").await;

        let org = app
            .request(Method::POST, "/api/v1/orgs")
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "name": "Acme", "slug": "acme" }))
            .send()
            .await;
        assert_eq!(org.status, 201);
        let org: OrgMembership = org.json();
        add_member(&app, org.id, admin_id, OrgRole::Admin).await;
        add_member(&app, org.id, member_id, OrgRole::Member).await;
        add_member(&app, org.id, viewer_id, OrgRole::Viewer).await;

        let denied = app
            .request(Method::POST, "/api/v1/orgs/acme/projects")
            .header(header::COOKIE.as_str(), &viewer)
            .json(&serde_json::json!({ "name": "Nope" }))
            .send()
            .await;
        assert_eq!(denied.status, 403);
        assert_eq!(denied.problem().code, ProblemCode::Forbidden);
        assert!(denied.problem().detail.is_none());

        let created = app
            .request(Method::POST, "/api/v1/orgs/acme/projects")
            .header(header::COOKIE.as_str(), &admin)
            .json(&serde_json::json!({ "name": "Support bot", "slug": "support-bot" }))
            .send()
            .await;
        assert_eq!(created.status, 201);
        let created: Project = created.json();
        assert_eq!(created.slug, "support-bot");
        assert_eq!(created.role, Some(OrgRole::Admin));
        assert!(created.retention_days.is_none());

        let hidden = app
            .request(Method::POST, "/api/v1/orgs/acme/projects")
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "name": "Hidden" }))
            .send()
            .await;
        let hidden: Project = hidden.json();
        let hidden_id = hidden.id;
        let member_id_override = member_id;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    set_project_override(conn, hidden_id, member_id_override, ProjectOverride::None)
                        .await
                })
            })
            .await
            .unwrap();

        let members = app
            .request(Method::GET, "/api/v1/orgs/acme/projects")
            .header(header::COOKIE.as_str(), &member)
            .send()
            .await;
        let members: Page<Project> = members.json();
        assert_eq!(members.data.len(), 1);
        assert_eq!(members.data[0].slug, "support-bot");
        assert_eq!(members.data[0].role, Some(OrgRole::Member));

        let viewers = app
            .request(Method::GET, "/api/v1/orgs/acme/projects")
            .header(header::COOKIE.as_str(), &viewer)
            .send()
            .await;
        let viewers: Page<Project> = viewers.json();
        assert_eq!(viewers.data.len(), 2);
        assert!(
            viewers
                .data
                .iter()
                .all(|project| project.role == Some(OrgRole::Viewer))
        );

        let owners = app
            .request(Method::GET, "/api/v1/orgs/acme/projects")
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        let owners: Page<Project> = owners.json();
        assert_eq!(owners.data.len(), 2);
        assert!(
            owners
                .data
                .iter()
                .all(|project| project.role == Some(OrgRole::Admin))
        );

        let by_id = app
            .request(Method::GET, &format!("/api/v1/projects/{}", created.id))
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        assert_eq!(by_id.status, 200);
        let by_id: Project = by_id.json();
        assert_eq!(by_id.slug, "support-bot");

        let by_slug = app
            .request(Method::GET, "/api/v1/projects/acme%2Fsupport-bot")
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        assert_eq!(by_slug.status, 200);
        let by_slug: Project = by_slug.json();
        assert_eq!(by_slug.id, created.id);

        let hidden_get = app
            .request(Method::GET, &format!("/api/v1/projects/{hidden_id}"))
            .header(header::COOKIE.as_str(), &member)
            .send()
            .await;
        assert_eq!(hidden_get.status, 404);

        let other = app
            .request(Method::POST, "/api/v1/orgs")
            .header(header::COOKIE.as_str(), &bob)
            .json(&serde_json::json!({ "name": "Other" }))
            .send()
            .await;
        let other: OrgMembership = other.json();
        let foreign = app
            .request(
                Method::POST,
                &format!("/api/v1/orgs/{}/projects", other.slug),
            )
            .header(header::COOKIE.as_str(), &bob)
            .json(&serde_json::json!({ "name": "Foreign" }))
            .send()
            .await;
        let foreign: Project = foreign.json();
        let missing = app
            .request(Method::GET, &format!("/api/v1/projects/{}", foreign.id))
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        assert_eq!(missing.status, 404);
    }

    #[tokio::test]
    async fn patch_retention_sets_clears_and_rejects_the_range() {
        let app = TestApp::new().await;
        let (_, ada) = signed_in(&app, "ada@example.com").await;
        let org = app
            .request(Method::POST, "/api/v1/orgs")
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "name": "Acme", "slug": "acme" }))
            .send()
            .await;
        assert_eq!(org.status, 201);
        let created = app
            .request(Method::POST, "/api/v1/orgs/acme/projects")
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "name": "Support bot", "slug": "support-bot" }))
            .send()
            .await;
        let created: Project = created.json();
        assert!(created.retention_days.is_none());
        let path = format!("/api/v1/projects/{}", created.id);

        let set = app
            .request(Method::PATCH, &path)
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "retention_days": 30 }))
            .send()
            .await;
        assert_eq!(set.status, 200);
        let set: Project = set.json();
        assert_eq!(set.retention_days, Some(30));

        let absent = app
            .request(Method::PATCH, &path)
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({}))
            .send()
            .await;
        assert_eq!(absent.status, 200);
        let absent: Project = absent.json();
        assert_eq!(absent.retention_days, Some(30));

        let cleared = app
            .request(Method::PATCH, &path)
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "retention_days": null }))
            .send()
            .await;
        assert_eq!(cleared.status, 200);
        let cleared: Project = cleared.json();
        assert!(cleared.retention_days.is_none());

        for days in [0, 36_501] {
            let rejected = app
                .request(Method::PATCH, &path)
                .header(header::COOKIE.as_str(), &ada)
                .json(&serde_json::json!({ "retention_days": days }))
                .send()
                .await;
            assert_eq!(rejected.status, 400);
            assert_eq!(rejected.problem().errors[0].field, "retention_days");
            assert_eq!(rejected.problem().errors[0].message, "must be 1 to 36500");
        }
        let still = app
            .request(Method::GET, &path)
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        let still: Project = still.json();
        assert!(still.retention_days.is_none());
    }
}
