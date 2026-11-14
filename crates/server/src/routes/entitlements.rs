//! Org entitlements (D179, D-P-15).

use axum::extract::{Extension, State};
use axum::routing::get;

use crate::api::entitlements::OrgEntitlements;
use crate::auth::roles::OrgPerm;
use crate::entitlements;
use crate::http::AppState;
use crate::http::access::{Access, OrgAccess, RouteDef};
use crate::http::error::{self, ApiError};

/// `GET /api/v1/orgs/{org}/entitlements`.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![RouteDef {
        method: axum::http::Method::GET,
        path: "/api/v1/orgs/{org}/entitlements",
        access: Access::Org {
            perm: OrgPerm::View,
            session_only: false,
        },
        documented: true,
        handler: get(get_entitlements),
    }]
}

async fn get_entitlements(
    State(state): State<AppState>,
    Extension(access): Extension<OrgAccess>,
) -> Result<error::Json<OrgEntitlements>, ApiError> {
    let org = access.org.id;
    Ok(error::Json(OrgEntitlements {
        features: entitlements::of(&state, org).features(),
        upgrade_url: entitlements::upgrade_url(&state, org),
    }))
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, header};
    use serde_json::{Value, json};
    use uuid::Uuid;

    use crate::api::account::OrgRole;
    use crate::auth::session::start_session;
    use crate::config::ServerConfig;
    use crate::meta::members::add_org_member;
    use crate::meta::users::{NewUser, insert_user};
    use crate::test_support::TestApp;
    use llmobs_core::ids::OrgId;

    /// Signs in a new user and returns its id and session cookie.
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
        let set_cookie = start_session(&app.state, user.id, None, None)
            .await
            .unwrap();
        let pair = set_cookie
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        (user.id, pair)
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

    async fn get_json(app: &TestApp, cookie: &str, path: &str) -> (u16, Value) {
        let response = app
            .request(Method::GET, path)
            .header(header::COOKIE.as_str(), cookie)
            .send()
            .await;
        let status = response.status.as_u16();
        let body = if status == 200 {
            response.json()
        } else {
            Value::Null
        };
        (status, body)
    }

    #[tokio::test]
    async fn members_read_the_licence_features() {
        let app = TestApp::with_license(&["project_roles"]).await;
        let seeded = app.seed().await;
        let (viewer, cookie) = signed_in(&app, "viewer@example.com").await;
        add_member(&app, seeded.org_id, viewer, OrgRole::Viewer).await;

        let (status, body) = get_json(&app, &cookie, "/api/v1/orgs/acme/entitlements").await;
        assert_eq!(status, 200);
        assert_eq!(body, json!({ "features": ["project_roles"] }));
    }

    #[tokio::test]
    async fn no_license_no_features() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let (status, body) = get_json(&app, &seeded.cookie, "/api/v1/orgs/acme/entitlements").await;
        assert_eq!(status, 200);
        assert_eq!(body, json!({ "features": [] }));
    }

    #[tokio::test]
    async fn upgrade_url_names_the_org() {
        let app = TestApp::with_config(|config: &mut ServerConfig| {
            config.entitlements.upgrade_url = Some("https://up.example/{org}/billing".to_owned());
        })
        .await;
        let seeded = app.seed().await;
        let (status, body) = get_json(&app, &seeded.cookie, "/api/v1/orgs/acme/entitlements").await;
        assert_eq!(status, 200);
        assert_eq!(
            body["upgrade_url"],
            json!(format!("https://up.example/{}/billing", seeded.org_id))
        );
    }

    #[tokio::test]
    async fn outsiders_get_404() {
        let app = TestApp::with_license(&["project_roles"]).await;
        app.seed().await;
        let (_, cookie) = signed_in(&app, "outsider@example.com").await;
        let (status, _) = get_json(&app, &cookie, "/api/v1/orgs/acme/entitlements").await;
        assert_eq!(status, 404);
    }
}
