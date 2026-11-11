//! `PATCH /api/v1/me` and `POST /api/v1/me/password` (spec 09).
//!
//! Both routes are session-only. A missing or null `name` clears the display name.
#![cfg_attr(not(test), allow(dead_code))]

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{patch, post};

use crate::api::account::{ChangePasswordRequest, UpdateMeRequest, User};
use crate::api::common::{FieldError, ProblemCode};
use crate::auth::password::check_policy;
use crate::auth::principal::{self, Principal};
use crate::http::AppState;
use crate::http::access::{Access, RouteDef};
use crate::http::client_ip::ClientIp;
use crate::http::error::{self, ApiError};
use crate::meta::personal_tokens::revoke_user_tokens;
use crate::meta::users::{self, UserRow};
use crate::meta::web_sessions::delete_user_sessions;

/// Account routes. CSRF applies because the principal is a session (D141).
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: axum::http::Method::PATCH,
            path: "/api/v1/me",
            access: Access::User { session_only: true },
            documented: true,
            handler: patch(update_me),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/v1/me/password",
            access: Access::User { session_only: true },
            documented: true,
            handler: post(change_password),
        },
    ]
}

async fn update_me(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    error::Json(body): error::Json<UpdateMeRequest>,
) -> Result<error::Json<User>, ApiError> {
    let user = principal::session_user(&principal)?;
    let name = match body.name {
        None => None,
        Some(name) => Some(trimmed_name(&name)?),
    };
    let user_id = user.id;
    let now = state.clock.now_ns();
    state
        .meta
        .write(move |conn| {
            Box::pin(async move { users::set_name(conn, user_id, name.as_deref(), now).await })
        })
        .await?;
    let Some(row) = users::user_by_id(state.meta.reader(), user_id).await? else {
        return Err(ApiError::Unauthorized);
    };
    Ok(error::Json(user_view(&row)))
}

async fn change_password(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Extension(principal): Extension<Principal>,
    error::Json(body): error::Json<ChangePasswordRequest>,
) -> Result<Response, ApiError> {
    let (user, session_hash) = match &principal {
        Principal::Session { user, session_hash } => (user, session_hash),
        Principal::ApiKey { .. } | Principal::Pat { .. } => {
            return Err(ApiError::Forbidden {
                code: ProblemCode::Forbidden,
            });
        }
    };
    let Some(row) = users::user_by_id(state.meta.reader(), user.id).await? else {
        return Err(ApiError::Unauthorized);
    };
    let now = state.clock.now_ns();
    state
        .limits
        .login
        .check(&(ip, row.email.clone()), now)
        .map_err(|retry| ApiError::RateLimited {
            retry_after_s: retry.0.as_secs().max(1),
        })?;
    if state.config.auth.disable_password {
        return Err(ApiError::Forbidden {
            code: ProblemCode::Forbidden,
        });
    }
    if row.password_hash.is_some() {
        let Some(current) = body.current_password.as_deref() else {
            return Err(field_error(
                "current_password",
                "current password is required",
            ));
        };
        let ok = {
            let hasher = state.passwords().await?;
            hasher
                .verify(current, row.password_hash.as_deref())
                .await
                .map_err(auth_error)?
                .ok
        };
        if !ok {
            state.limits.lockout.record_failure(&row.email, now);
            return Err(ApiError::Forbidden {
                code: ProblemCode::Forbidden,
            });
        }
    } else if body.current_password.is_some() {
        return Err(field_error(
            "current_password",
            "current password must be absent",
        ));
    }
    check_policy(&body.new_password, &row.email)
        .map_err(|error| field_error("new_password", &error.to_string()))?;
    let password_hash = {
        let hasher = state.passwords().await?;
        hasher.hash(&body.new_password).await.map_err(auth_error)?
    };
    let user_id = row.id;
    let email = row.email.clone();
    let keep = session_hash.clone();
    let hashes = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                users::set_password_hash(conn, user_id, Some(&password_hash), now).await?;
                delete_user_sessions(conn, user_id, Some(&keep)).await?;
                revoke_user_tokens(conn, user_id, now).await
            })
        })
        .await?;
    for hash in hashes {
        state.pat_cache.evict(&hash);
    }
    state.limits.lockout.clear(&email);
    tracing::info!(target: "audit", %user_id, "password changed");
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn user_view(user: &UserRow) -> User {
    User {
        id: user.id,
        email: user.email.clone(),
        name: user.name.clone(),
        is_instance_admin: user.is_instance_admin,
    }
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

fn auth_error(error: crate::auth::AuthError) -> ApiError {
    ApiError::Internal(Box::new(error))
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, header};

    use super::*;
    use crate::api::common::ProblemCode;
    use crate::auth::password::Hasher;
    use crate::auth::session::start_session;
    use crate::meta::users::{NewUser, insert_user, user_by_email};
    use crate::test_support::{TestApp, assert_logs_clean};
    use uuid::Uuid;

    const PASSWORD: &str = "llmobs-ok1";
    const NEW_PASSWORD: &str = "llmobs-ok2";

    fn cookie_pair(set_cookie: &axum::http::HeaderValue) -> String {
        set_cookie
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    fn session_cookie(response: &crate::test_support::TestResponse) -> String {
        cookie_pair(response.headers.get(header::SET_COOKIE).unwrap())
    }

    async fn insert_account(app: &TestApp, email: &str, password: Option<&str>) -> Uuid {
        let hash = match password {
            Some(password) => Some(
                app.state
                    .passwords()
                    .await
                    .unwrap()
                    .hash(password)
                    .await
                    .unwrap(),
            ),
            None => None,
        };
        let email = email.to_owned();
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    insert_user(
                        conn,
                        NewUser {
                            email,
                            name: Some("Ada".to_owned()),
                            password_hash: hash,
                            email_verified_at: Some(now),
                            is_instance_admin: false,
                        },
                        now,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .id
    }

    async fn login(
        app: &TestApp,
        email: &str,
        password: &str,
    ) -> crate::test_support::TestResponse {
        app.request(Method::POST, "/api/auth/login")
            .json(&serde_json::json!({ "email": email, "password": password }))
            .send()
            .await
    }

    #[test]
    fn account_routes_are_session_only() {
        for route in routes() {
            assert!(
                matches!(route.access, Access::User { session_only: true }),
                "{}",
                route.path
            );
        }
    }

    #[tokio::test]
    async fn patch_me_sets_and_clears_the_name() {
        let app = TestApp::new().await;
        insert_account(&app, "ada@example.com", Some(PASSWORD)).await;
        let logged_in = login(&app, "ada@example.com", PASSWORD).await;
        let cookie = session_cookie(&logged_in);
        let renamed = app
            .request(Method::PATCH, "/api/v1/me")
            .header(header::COOKIE.as_str(), &cookie)
            .json(&serde_json::json!({ "name": "  Grace Hopper  " }))
            .send()
            .await;
        assert_eq!(renamed.status, 200);
        let user: User = renamed.json();
        assert_eq!(user.name.as_deref(), Some("Grace Hopper"));
        assert_eq!(user.email, "ada@example.com");
        let cleared = app
            .request(Method::PATCH, "/api/v1/me")
            .header(header::COOKIE.as_str(), &cookie)
            .json(&serde_json::json!({ "name": null }))
            .send()
            .await;
        assert_eq!(cleared.status, 200);
        let user: User = cleared.json();
        assert!(user.name.is_none());
        let blank = app
            .request(Method::PATCH, "/api/v1/me")
            .header(header::COOKIE.as_str(), &cookie)
            .json(&serde_json::json!({ "name": "   " }))
            .send()
            .await;
        assert_eq!(blank.status, 400);
        assert_eq!(blank.problem().errors[0].field, "name");
        assert_eq!(
            blank.problem().errors[0].message,
            "must be 1 to 100 characters"
        );
    }

    #[tokio::test]
    async fn password_change_keeps_only_the_current_session() {
        let app = TestApp::new().await;
        let user_id = insert_account(&app, "ada@example.com", Some(PASSWORD)).await;
        let logged_in = login(&app, "ada@example.com", PASSWORD).await;
        let cookie = session_cookie(&logged_in);
        let other = start_session(&app.state, user_id, None, None)
            .await
            .unwrap();
        let other_cookie = cookie_pair(&other);
        let now = app.state.clock.now_ns();
        for _ in 0..19 {
            app.state
                .limits
                .lockout
                .record_failure("ada@example.com", now);
        }
        let wrong = app
            .request(Method::POST, "/api/v1/me/password")
            .header(header::COOKIE.as_str(), &cookie)
            .json(&serde_json::json!({
                "current_password": "not-the-password",
                "new_password": NEW_PASSWORD,
            }))
            .send()
            .await;
        assert_eq!(wrong.status, 403);
        assert_eq!(wrong.problem().code, ProblemCode::Forbidden);
        assert!(wrong.problem().detail.is_none());
        assert!(app.state.limits.lockout.is_locked("ada@example.com", now));
        let changed = assert_logs_clean(&[PASSWORD, NEW_PASSWORD, "ada@example.com"], async {
            app.request(Method::POST, "/api/v1/me/password")
                .header(header::COOKIE.as_str(), &cookie)
                .json(&serde_json::json!({
                    "current_password": PASSWORD,
                    "new_password": NEW_PASSWORD,
                }))
                .send()
                .await
        })
        .await;
        assert_eq!(
            changed.status,
            204,
            "{}",
            String::from_utf8_lossy(&changed.body)
        );
        assert!(!app.state.limits.lockout.is_locked("ada@example.com", now));
        let current = app
            .request(Method::GET, "/api/auth/me")
            .header(header::COOKIE.as_str(), &cookie)
            .send()
            .await;
        assert_eq!(current.status, 200);
        let dropped = app
            .request(Method::GET, "/api/auth/me")
            .header(header::COOKIE.as_str(), &other_cookie)
            .send()
            .await;
        assert_eq!(dropped.status, 401);
        let old = login(&app, "ada@example.com", PASSWORD).await;
        assert_eq!(old.status, 401);
        let fresh = login(&app, "ada@example.com", NEW_PASSWORD).await;
        assert_eq!(fresh.status, 200);
    }

    #[tokio::test]
    async fn sso_user_sets_a_password_without_the_current_one() {
        let app = TestApp::new().await;
        let user_id = insert_account(&app, "ada@example.com", None).await;
        let cookie = cookie_pair(
            &start_session(&app.state, user_id, None, None)
                .await
                .unwrap(),
        );
        let rejected = app
            .request(Method::POST, "/api/v1/me/password")
            .header(header::COOKIE.as_str(), &cookie)
            .json(&serde_json::json!({
                "current_password": PASSWORD,
                "new_password": NEW_PASSWORD,
            }))
            .send()
            .await;
        assert_eq!(rejected.status, 400);
        assert_eq!(rejected.problem().errors[0].field, "current_password");
        assert_eq!(
            rejected.problem().errors[0].message,
            "current password must be absent"
        );
        let set = app
            .request(Method::POST, "/api/v1/me/password")
            .header(header::COOKIE.as_str(), &cookie)
            .json(&serde_json::json!({ "new_password": NEW_PASSWORD }))
            .send()
            .await;
        assert_eq!(set.status, 204);
        let row = user_by_email(app.state.meta.reader(), "ada@example.com")
            .await
            .unwrap()
            .unwrap();
        let hasher = app.state.password_hasher.get().unwrap();
        let verified = hasher
            .verify(NEW_PASSWORD, row.password_hash.as_deref())
            .await
            .unwrap();
        assert!(verified.ok);
        let logged_in = login(&app, "ada@example.com", NEW_PASSWORD).await;
        assert_eq!(logged_in.status, 200);
    }

    #[tokio::test]
    async fn password_change_rejects_a_weak_password_and_a_disabled_login() {
        let app = TestApp::new().await;
        insert_account(&app, "ada@example.com", Some(PASSWORD)).await;
        let cookie = session_cookie(&login(&app, "ada@example.com", PASSWORD).await);
        let missing = app
            .request(Method::POST, "/api/v1/me/password")
            .header(header::COOKIE.as_str(), &cookie)
            .json(&serde_json::json!({ "new_password": NEW_PASSWORD }))
            .send()
            .await;
        assert_eq!(missing.status, 400);
        assert_eq!(missing.problem().errors[0].field, "current_password");
        assert_eq!(
            missing.problem().errors[0].message,
            "current password is required"
        );
        let weak = app
            .request(Method::POST, "/api/v1/me/password")
            .header(header::COOKIE.as_str(), &cookie)
            .json(&serde_json::json!({
                "current_password": PASSWORD,
                "new_password": "nope-nope",
            }))
            .send()
            .await;
        assert_eq!(weak.status, 400);
        assert_eq!(weak.problem().errors[0].field, "new_password");
        let still = login(&app, "ada@example.com", PASSWORD).await;
        assert_eq!(still.status, 200);

        let disabled = TestApp::with_config(|config| config.auth.disable_password = true).await;
        let user_id = insert_account(&disabled, "ada@example.com", Some(PASSWORD)).await;
        let cookie = cookie_pair(
            &start_session(&disabled.state, user_id, None, None)
                .await
                .unwrap(),
        );
        let before = disabled
            .state
            .password_hasher
            .get()
            .map(Hasher::verify_count)
            .unwrap_or(0);
        let blocked = disabled
            .request(Method::POST, "/api/v1/me/password")
            .header(header::COOKIE.as_str(), &cookie)
            .json(&serde_json::json!({
                "current_password": "not-the-password",
                "new_password": NEW_PASSWORD,
            }))
            .send()
            .await;
        assert_eq!(blocked.status, 403);
        assert!(blocked.problem().detail.is_none());
        let after = disabled
            .state
            .password_hasher
            .get()
            .map(Hasher::verify_count)
            .unwrap_or(0);
        assert_eq!(after, before);
    }
}
