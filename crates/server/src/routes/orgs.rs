//! `GET/POST /api/v1/orgs` and `GET/PATCH /api/v1/orgs/{org}` (spec 09).
#![cfg_attr(not(test), allow(dead_code))]

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};

use crate::api::account::{CreateOrgRequest, OrgMembership, OrgRole, UpdateOrgRequest};
use crate::api::common::{FieldError, Page, PageParams};
use crate::auth::principal::{self, Principal};
use crate::http::AppState;
use crate::http::access::{Access, OrgAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::meta::members::add_org_member;
use crate::meta::orgs::{self, OrgRow, SlugError};

/// Org routes. Any signed-in caller may list and create orgs (spec 08).
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/orgs",
            access: Access::User {
                session_only: false,
            },
            documented: true,
            handler: get(list_orgs),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/v1/orgs",
            access: Access::User {
                session_only: false,
            },
            documented: true,
            handler: post(create_org),
        },
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/orgs/{org}",
            access: Access::Org {
                perm: crate::auth::roles::OrgPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(get_org),
        },
        RouteDef {
            method: axum::http::Method::PATCH,
            path: "/api/v1/orgs/{org}",
            access: Access::Org {
                perm: crate::auth::roles::OrgPerm::Rename,
                session_only: false,
            },
            documented: true,
            handler: patch(update_org),
        },
    ]
}

async fn list_orgs(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    error::Query(params): error::Query<PageParams>,
) -> Result<error::Json<Page<OrgMembership>>, ApiError> {
    let user = principal::session_user(&principal)?;
    let rows = orgs::orgs_of_user(state.meta.reader(), user.id).await?;
    let items = rows
        .into_iter()
        .filter(|(org, _)| org.state == "active")
        .map(|(org, role)| (org.name.clone(), org.id.to_string(), membership(&org, role)))
        .collect();
    Ok(error::Json(page_named(
        items,
        params.cursor.as_deref(),
        params.limit.get(),
    )?))
}

async fn create_org(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    error::Json(body): error::Json<CreateOrgRequest>,
) -> Result<Response, ApiError> {
    let user = principal::session_user(&principal)?;
    let name = trimmed_name(&body.name)?;
    if let Some(slug) = &body.slug {
        orgs::validate_slug(slug).map_err(slug_error)?;
    }
    let user_id = user.id;
    let slug = body.slug;
    let now = state.clock.now_ns();
    let org = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                let org = match &slug {
                    Some(slug) => orgs::insert_org(conn, &name, slug, now).await?,
                    None => orgs::insert_org_with_derived_slug(conn, &name, now).await?,
                };
                add_org_member(conn, org.id, user_id, OrgRole::Owner, now).await?;
                Ok(org)
            })
        })
        .await?;
    tracing::info!(target: "audit", %user_id, org_id = %org.id, "org created");
    Ok((
        StatusCode::CREATED,
        error::Json(membership(&org, OrgRole::Owner)),
    )
        .into_response())
}

async fn get_org(Extension(access): Extension<OrgAccess>) -> error::Json<OrgMembership> {
    error::Json(membership(&access.org, access.role))
}

async fn update_org(
    State(state): State<AppState>,
    Extension(access): Extension<OrgAccess>,
    error::Json(body): error::Json<UpdateOrgRequest>,
) -> Result<error::Json<OrgMembership>, ApiError> {
    let name = trimmed_name(&body.name)?;
    let id = access.org.id;
    let stored = name.clone();
    let now = state.clock.now_ns();
    state
        .meta
        .write(move |conn| Box::pin(async move { orgs::rename_org(conn, id, &stored, now).await }))
        .await?;
    let mut org = access.org;
    org.name = name;
    Ok(error::Json(membership(&org, access.role)))
}

fn membership(org: &OrgRow, role: OrgRole) -> OrgMembership {
    OrgMembership {
        id: org.id,
        slug: org.slug.clone(),
        name: org.name.clone(),
        role,
    }
}

/// One page of rows already ordered by `(name, id)`.
///
/// The cursor is the last row of the previous page. A cursor that does not decode is 400.
pub(crate) fn page_named<T>(
    rows: Vec<(String, String, T)>,
    cursor: Option<&str>,
    limit: u32,
) -> Result<Page<T>, ApiError> {
    let total = rows.len();
    let start = match cursor {
        None => 0,
        Some(cursor) => {
            let key = decode_cursor(cursor)?;
            rows.iter()
                .position(|(name, id, _)| (name, id) > (&key.name, &key.id))
                .unwrap_or(total)
        }
    };
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut page: Vec<_> = rows.into_iter().skip(start).take(limit).collect();
    let more = start + page.len() < total;
    let next_cursor = if more {
        match page.last() {
            Some((name, id, _)) => Some(encode_cursor(name, id)?),
            None => None,
        }
    } else {
        None
    };
    Ok(Page {
        data: page.drain(..).map(|(_, _, item)| item).collect(),
        next_cursor,
    })
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CursorKey {
    name: String,
    id: String,
}

fn decode_cursor(cursor: &str) -> Result<CursorKey, ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(cursor)
        .map_err(|_| invalid_cursor())?;
    serde_json::from_slice(&bytes).map_err(|_| invalid_cursor())
}

fn encode_cursor(name: &str, id: &str) -> Result<String, ApiError> {
    let bytes = serde_json::to_vec(&CursorKey {
        name: name.to_owned(),
        id: id.to_owned(),
    })
    .map_err(|error| ApiError::Internal(Box::new(error)))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn invalid_cursor() -> ApiError {
    field_error("cursor", "invalid cursor")
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
    use uuid::Uuid;

    use super::*;
    use crate::api::common::ProblemCode;
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

    async fn add_member(app: &TestApp, org: llmobs_core::ids::OrgId, user: Uuid, role: OrgRole) {
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
    async fn ada_creates_orgs_and_lists_only_hers() {
        let app = TestApp::new().await;
        let (_, ada) = signed_in(&app, "ada@example.com").await;
        let (_, bob) = signed_in(&app, "bob@example.com").await;

        let (created, logs) = capture_logs(async {
            app.request(Method::POST, "/api/v1/orgs")
                .header(header::COOKIE.as_str(), &ada)
                .json(&serde_json::json!({ "name": "  Widgets  " }))
                .send()
                .await
        })
        .await;
        assert_eq!(created.status, 201);
        let org: OrgMembership = created.json();
        assert_eq!(org.role, OrgRole::Owner);
        assert_eq!(org.name, "Widgets");
        assert_eq!(org.slug, "widgets");
        assert!(logs.contains("org created"));
        assert!(!logs.contains("Widgets"));
        assert!(!logs.contains("widgets"));
        assert!(!logs.contains("ada@example.com"));

        let second = app
            .request(Method::POST, "/api/v1/orgs")
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "name": "Widgets" }))
            .send()
            .await;
        assert_eq!(second.status, 201);
        let second: OrgMembership = second.json();
        assert_eq!(second.slug, "widgets-2");
        assert_eq!(second.role, OrgRole::Owner);

        let taken = app
            .request(Method::POST, "/api/v1/orgs")
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "name": "Other", "slug": "widgets" }))
            .send()
            .await;
        assert_eq!(taken.status, 409);
        assert_eq!(taken.problem().detail.as_deref(), Some("slug taken"));

        let reserved = app
            .request(Method::POST, "/api/v1/orgs")
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "name": "Admin", "slug": "admin" }))
            .send()
            .await;
        assert_eq!(reserved.status, 400);
        assert_eq!(reserved.problem().errors[0].field, "slug");
        assert_eq!(reserved.problem().errors[0].message, "reserved slug");

        let bobs = app
            .request(Method::POST, "/api/v1/orgs")
            .header(header::COOKIE.as_str(), &bob)
            .json(&serde_json::json!({ "name": "Bobs Org" }))
            .send()
            .await;
        assert_eq!(bobs.status, 201);

        let listed = app
            .request(Method::GET, "/api/v1/orgs")
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        assert_eq!(listed.status, 200);
        let page: Page<OrgMembership> = listed.json();
        let slugs: Vec<_> = page.data.iter().map(|org| org.slug.as_str()).collect();
        assert_eq!(slugs, ["widgets", "widgets-2"]);
        assert!(page.next_cursor.is_none());

        let fetched = app
            .request(Method::GET, "/api/v1/orgs/widgets")
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        assert_eq!(fetched.status, 200);
        let fetched: OrgMembership = fetched.json();
        assert_eq!(fetched.role, OrgRole::Owner);
        assert_eq!(fetched.id, org.id);
    }

    #[tokio::test]
    async fn viewer_cannot_rename_and_admin_can() {
        let app = TestApp::new().await;
        let (_, ada) = signed_in(&app, "ada@example.com").await;
        let (admin_id, admin) = signed_in(&app, "admin@example.com").await;
        let (viewer_id, viewer) = signed_in(&app, "viewer@example.com").await;
        let created = app
            .request(Method::POST, "/api/v1/orgs")
            .header(header::COOKIE.as_str(), &ada)
            .json(&serde_json::json!({ "name": "Acme" }))
            .send()
            .await;
        let org: OrgMembership = created.json();
        add_member(&app, org.id, admin_id, OrgRole::Admin).await;
        add_member(&app, org.id, viewer_id, OrgRole::Viewer).await;

        let denied = app
            .request(Method::PATCH, "/api/v1/orgs/acme")
            .header(header::COOKIE.as_str(), &viewer)
            .json(&serde_json::json!({ "name": "Renamed" }))
            .send()
            .await;
        assert_eq!(denied.status, 403);
        assert_eq!(denied.problem().code, ProblemCode::Forbidden);
        assert!(denied.problem().detail.is_none());

        let renamed = app
            .request(Method::PATCH, "/api/v1/orgs/acme")
            .header(header::COOKIE.as_str(), &admin)
            .json(&serde_json::json!({ "name": "  Renamed  " }))
            .send()
            .await;
        assert_eq!(renamed.status, 200);
        let renamed: OrgMembership = renamed.json();
        assert_eq!(renamed.name, "Renamed");
        assert_eq!(renamed.slug, "acme");
        assert_eq!(renamed.role, OrgRole::Admin);
    }

    #[tokio::test]
    async fn org_list_pages_and_rejects_a_bad_cursor() {
        let app = TestApp::new().await;
        let (_, ada) = signed_in(&app, "ada@example.com").await;
        for name in ["Alpha", "Beta", "Gamma"] {
            let response = app
                .request(Method::POST, "/api/v1/orgs")
                .header(header::COOKIE.as_str(), &ada)
                .json(&serde_json::json!({ "name": name }))
                .send()
                .await;
            assert_eq!(response.status, 201);
        }

        let first = app
            .request(Method::GET, "/api/v1/orgs?limit=2")
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        assert_eq!(first.status, 200);
        let first: Page<OrgMembership> = first.json();
        let names: Vec<_> = first.data.iter().map(|org| org.name.as_str()).collect();
        assert_eq!(names, ["Alpha", "Beta"]);
        let cursor = first.next_cursor.expect("another page");

        let second = app
            .request(
                Method::GET,
                &format!("/api/v1/orgs?limit=2&cursor={cursor}"),
            )
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        assert_eq!(second.status, 200);
        let second: Page<OrgMembership> = second.json();
        let names: Vec<_> = second.data.iter().map(|org| org.name.as_str()).collect();
        assert_eq!(names, ["Gamma"]);
        assert!(second.next_cursor.is_none());

        let bad = app
            .request(Method::GET, "/api/v1/orgs?cursor=!!!")
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        assert_eq!(bad.status, 400);
        assert_eq!(bad.problem().errors[0].field, "cursor");
        assert!(!String::from_utf8_lossy(&bad.body).contains("!!!"));
    }
}
