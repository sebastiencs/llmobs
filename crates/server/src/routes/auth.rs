//! Auth routes: config, setup, login, logout, signup, and the current user (spec 08, D144).
#![cfg_attr(not(test), allow(dead_code))]

use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use uuid::Uuid;

use crate::api::account::{
    AuthConfig, LoginRequest, Me, OrgMembership, OrgRole, Project, ResetPasswordRequest,
    SetupRequest, SetupResponse, SignupPolicy, SignupRequest, SignupResponse, User,
};
use crate::api::common::{FieldError, Problem, ProblemCode};
use crate::auth::password::check_policy;
use crate::auth::principal::{self, Principal};
use crate::auth::secret::{constant_time_eq, hash_token, is_opaque_token};
use crate::auth::session::{clear_cookie, resolve_cookie_header, start_session};
use crate::bootstrap::{self, BootstrapError};
use crate::http::AppState;
use crate::http::access::{Access, RouteDef};
use crate::http::client_ip::ClientIp;
use crate::http::error::{self, ApiError};
use crate::meta::orgs::orgs_of_user;
use crate::meta::password_resets::{delete_user_resets, use_reset, valid_reset};
use crate::meta::users::{self, UserRow, normalize_email};
use crate::meta::web_sessions::{delete_session, delete_user_sessions};

/// Auth routes. CSRF still applies to every POST under `/api/auth/` (D141).
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/auth/config",
            access: Access::Public,
            documented: true,
            handler: get(config),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/auth/setup",
            access: Access::Public,
            documented: true,
            handler: post(setup),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/auth/login",
            access: Access::Public,
            documented: true,
            handler: post(login),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/auth/logout",
            access: Access::Public,
            documented: true,
            handler: post(logout),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/auth/signup",
            access: Access::Public,
            documented: true,
            handler: post(signup),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/auth/password/reset",
            access: Access::Public,
            documented: true,
            handler: post(reset_password),
        },
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/auth/me",
            access: Access::User {
                session_only: false,
            },
            documented: true,
            handler: get(me),
        },
    ]
}

async fn config(State(state): State<AppState>) -> Result<error::Json<AuthConfig>, ApiError> {
    let users = users::count_users(state.meta.reader()).await?;
    Ok(error::Json(AuthConfig {
        setup_required: users == 0,
        password_enabled: !state.config.auth.disable_password,
        signup: state.config.auth.signup,
        providers: Vec::new(),
        smtp_enabled: state.config.smtp_url.is_some(),
        email_verification_required: state.config.auth.require_email_verification,
    }))
}

async fn setup(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    error::Json(body): error::Json<SetupRequest>,
) -> Result<Response, ApiError> {
    state
        .limits
        .ip_forms
        .check(&ip, state.clock.now_ns())
        .map_err(|retry| ApiError::RateLimited {
            retry_after_s: retry.0.as_secs().max(1),
        })?;
    if state.config.auth.disable_password {
        return Ok(forbidden(Some(
            "password setup is disabled; use an identity provider",
        )));
    }
    let Some(expected) = state.setup.token().map_err(bootstrap_error)? else {
        return Err(ApiError::Conflict {
            detail: "already set up",
        });
    };
    if !constant_time_eq(&expected, &body.setup_token) {
        return Ok(forbidden(None));
    }
    let email = normalize_email(&body.email).map_err(|_| field_error("email", "invalid email"))?;
    check_policy(&body.password, &email)
        .map_err(|error| field_error("password", &error.to_string()))?;
    let name = optional_name(body.name)?;
    let org_name = trimmed_name("org_name", &body.org_name)?;
    let project_name = trimmed_name("project_name", &body.project_name)?;
    let created =
        bootstrap::create_setup(&state, email, name, &body.password, org_name, project_name)
            .await
            .map_err(bootstrap_error)?;
    let Some(created) = created else {
        return Err(ApiError::Conflict {
            detail: "already set up",
        });
    };
    state.setup.clear().map_err(bootstrap_error)?;
    let user_id = created.created.user.id;
    let org_id = created.created.org.id;
    let project_id = created.created.project.id;
    let key_id = created.created.key.as_ref().map(|key| key.id);
    if let Some(key_id) = key_id {
        tracing::info!(target: "audit", %user_id, %org_id, %project_id, %key_id, "setup completed");
    } else {
        tracing::info!(target: "audit", %user_id, %org_id, %project_id, "setup completed");
    }
    let cookie = start_session(
        &state,
        user_id,
        headers
            .get(axum::http::header::USER_AGENT)
            .and_then(|value| value.to_str().ok()),
        Some(ip),
    )
    .await?;
    let response = SetupResponse {
        user: user_view(&created.created.user),
        org: OrgMembership {
            id: created.created.org.id,
            slug: created.created.org.slug,
            name: created.created.org.name,
            role: OrgRole::Owner,
        },
        project: Project {
            id: created.created.project.id,
            org_id: created.created.project.org_id,
            slug: created.created.project.slug,
            name: created.created.project.name,
            retention_days: created.created.project.retention_days,
            role: Some(OrgRole::Owner),
            created_at: created.created.project.created_at,
        },
        secret_key: created.secret_key,
        secret_key_prefix: created.secret_key_prefix,
    };
    let mut http = (StatusCode::CREATED, error::Json(response)).into_response();
    http.headers_mut()
        .insert(axum::http::header::SET_COOKIE, cookie);
    Ok(http)
}

async fn login(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    error::Json(body): error::Json<LoginRequest>,
) -> Result<Response, ApiError> {
    if state.config.auth.disable_password {
        return Err(ApiError::Forbidden {
            code: ProblemCode::Forbidden,
        });
    }
    let Ok(email) = normalize_email(&body.email) else {
        return Err(ApiError::Unauthorized);
    };
    let now = state.clock.now_ns();
    state
        .limits
        .login
        .check(&(ip, email.clone()), now)
        .map_err(|retry| ApiError::RateLimited {
            retry_after_s: retry.0.as_secs().max(1),
        })?;
    if domain_listed(&state.config.auth.enforce_sso_domains, &email) {
        return Err(ApiError::Forbidden {
            code: ProblemCode::SsoRequired,
        });
    }
    let user = users::user_by_email(state.meta.reader(), &email).await?;
    let locked = state.limits.lockout.is_locked(&email, now);
    let disabled = user.as_ref().is_some_and(|user| user.disabled_at.is_some());
    let stored_hash = user.as_ref().and_then(|user| user.password_hash.clone());
    // Locked, unknown, password-less, and disabled accounts all verify once and
    // then answer the same 401. A wrong password of a real account is the only
    // failure that counts toward lockout (D145). The hasher borrow ends before
    // the lockout map is touched.
    let verified = {
        let hasher = state.passwords().await?;
        if locked || disabled || stored_hash.is_none() {
            hasher
                .verify(&body.password, None)
                .await
                .map_err(auth_error)?;
            None
        } else {
            let verified = hasher
                .verify(&body.password, stored_hash.as_deref())
                .await
                .map_err(auth_error)?;
            verified.ok.then_some(verified.needs_rehash)
        }
    };
    let accepted = if verified.is_none() && !locked && !disabled && stored_hash.is_some() {
        state.limits.lockout.record_failure(&email, now);
        None
    } else {
        verified
    };
    let Some(user) = user else {
        audit_login_failed(None);
        return Err(ApiError::Unauthorized);
    };
    let Some(needs_rehash) = accepted else {
        audit_login_failed(Some(user.id));
        return Err(ApiError::Unauthorized);
    };
    if state.config.auth.require_email_verification && user.email_verified_at.is_none() {
        return Err(ApiError::Forbidden {
            code: ProblemCode::EmailNotVerified,
        });
    }
    if needs_rehash {
        let password_hash = {
            let hasher = state.passwords().await?;
            hasher.hash(&body.password).await.map_err(auth_error)?
        };
        let user_id = user.id;
        state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    users::set_password_hash(conn, user_id, Some(&password_hash), now).await
                })
            })
            .await?;
    }
    state.limits.lockout.clear(&email);
    let me = me_from(&state, &user).await?;
    let cookie = session_cookie(&state, user.id, &headers, Some(ip)).await?;
    tracing::info!(target: "audit", user_id = %user.id, "login");
    Ok(json_cookie(StatusCode::OK, &me, cookie))
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    if let Some((row, _)) = resolve_cookie_header(&state, &headers, state.clock.now_ns()).await? {
        let hash = row.hash;
        state
            .meta
            .write(move |conn| Box::pin(async move { delete_session(conn, &hash).await }))
            .await?;
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(axum::http::header::SET_COOKIE, clear_cookie(&state));
    Ok(response)
}

async fn reset_password(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    error::Json(body): error::Json<ResetPasswordRequest>,
) -> Result<Response, ApiError> {
    state
        .limits
        .ip_forms
        .check(&ip, state.clock.now_ns())
        .map_err(|retry| ApiError::RateLimited {
            retry_after_s: retry.0.as_secs().max(1),
        })?;
    if state.config.auth.disable_password {
        return Err(ApiError::Forbidden {
            code: ProblemCode::Forbidden,
        });
    }
    if !is_opaque_token(&body.token) {
        return Err(invalid_link());
    }
    let token_hash = hash_token(&body.token);
    let now = state.clock.now_ns();
    let Some(user_id) = valid_reset(state.meta.reader(), &token_hash, now).await? else {
        return Err(invalid_link());
    };
    let Some(user) = users::user_by_id(state.meta.reader(), user_id).await? else {
        return Err(invalid_link());
    };
    check_policy(&body.password, &user.email)
        .map_err(|error| field_error("password", &error.to_string()))?;
    let password_hash = {
        let hasher = state.passwords().await?;
        hasher.hash(&body.password).await.map_err(auth_error)?
    };
    let email = user.email.clone();
    let consumed = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                let Some(id) = use_reset(conn, &token_hash, now).await? else {
                    return Ok(None);
                };
                users::set_password_hash(conn, id, Some(&password_hash), now).await?;
                delete_user_sessions(conn, id, None).await?;
                delete_user_resets(conn, id).await?;
                Ok(Some(id))
            })
        })
        .await?;
    let Some(user_id) = consumed else {
        return Err(invalid_link());
    };
    state.limits.lockout.clear(&email);
    tracing::info!(target: "audit", %user_id, "password reset");
    Ok(StatusCode::NO_CONTENT.into_response())
}

fn invalid_link() -> ApiError {
    ApiError::BadRequest {
        detail: Some("invalid or expired link".to_owned()),
        errors: Vec::new(),
    }
}

async fn signup(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    error::Json(body): error::Json<SignupRequest>,
) -> Result<Response, ApiError> {
    state
        .limits
        .ip_forms
        .check(&ip, state.clock.now_ns())
        .map_err(|retry| ApiError::RateLimited {
            retry_after_s: retry.0.as_secs().max(1),
        })?;
    if state.config.auth.disable_password {
        return Err(ApiError::Forbidden {
            code: ProblemCode::Forbidden,
        });
    }
    match state.config.auth.signup {
        SignupPolicy::Disabled => {
            return Ok(forbidden(Some("sign-up is disabled")));
        }
        SignupPolicy::InviteOnly => {
            return Ok(forbidden(Some("sign-up requires an invitation")));
        }
        SignupPolicy::Open => {}
    }
    let email = normalize_email(&body.email).map_err(|_| field_error("email", "invalid email"))?;
    if !state.config.auth.signup_domains.is_empty()
        && !domain_listed(&state.config.auth.signup_domains, &email)
    {
        return Err(ApiError::Forbidden {
            code: ProblemCode::Forbidden,
        });
    }
    if domain_listed(&state.config.auth.enforce_sso_domains, &email) {
        return Err(ApiError::Forbidden {
            code: ProblemCode::SsoRequired,
        });
    }
    check_policy(&body.password, &email)
        .map_err(|error| field_error("password", &error.to_string()))?;
    let name = optional_name(body.name)?;
    let password_hash = {
        let hasher = state.passwords().await?;
        hasher.hash(&body.password).await.map_err(auth_error)?
    };
    let now = state.clock.now_ns();
    let email_for_insert = email.clone();
    let name_for_insert = name.clone();
    let user = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                users::insert_user(
                    conn,
                    users::NewUser {
                        email: email_for_insert,
                        name: name_for_insert,
                        password_hash: Some(password_hash),
                        email_verified_at: None,
                        is_instance_admin: false,
                    },
                    now,
                )
                .await
            })
        })
        .await?;
    if state.config.auth.require_email_verification {
        let response = SignupResponse {
            me: None,
            verification_required: true,
        };
        return Ok((StatusCode::CREATED, error::Json(response)).into_response());
    }
    let me = me_from(&state, &user).await?;
    let cookie = session_cookie(&state, user.id, &headers, Some(ip)).await?;
    let response = SignupResponse {
        me: Some(me),
        verification_required: false,
    };
    Ok(json_cookie(StatusCode::CREATED, &response, cookie))
}

async fn me(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<error::Json<Me>, ApiError> {
    let user = principal::session_user(&principal)?;
    let Some(row) = users::user_by_id(state.meta.reader(), user.id).await? else {
        return Err(ApiError::Unauthorized);
    };
    Ok(error::Json(me_from(&state, &row).await?))
}

async fn me_from(state: &AppState, user: &UserRow) -> Result<Me, ApiError> {
    let orgs = orgs_of_user(state.meta.reader(), user.id).await?;
    Ok(Me {
        user: user_view(user),
        orgs: orgs
            .into_iter()
            .map(|(org, role)| OrgMembership {
                id: org.id,
                slug: org.slug,
                name: org.name,
                role,
            })
            .collect(),
    })
}

async fn session_cookie(
    state: &AppState,
    user_id: Uuid,
    headers: &HeaderMap,
    ip: Option<std::net::IpAddr>,
) -> Result<axum::http::HeaderValue, ApiError> {
    start_session(
        state,
        user_id,
        headers
            .get(axum::http::header::USER_AGENT)
            .and_then(|value| value.to_str().ok()),
        ip,
    )
    .await
}

fn json_cookie(
    status: StatusCode,
    body: &impl serde::Serialize,
    cookie: axum::http::HeaderValue,
) -> Response {
    let mut response = (status, error::Json(body)).into_response();
    response
        .headers_mut()
        .insert(axum::http::header::SET_COOKIE, cookie);
    response
}

fn domain_listed(domains: &[String], email: &str) -> bool {
    let Some(domain) = email.split_once('@').map(|(_, domain)| domain) else {
        return false;
    };
    domains.iter().any(|item| item == domain)
}

fn audit_login_failed(user_id: Option<Uuid>) {
    if let Some(user_id) = user_id {
        tracing::info!(target: "audit", %user_id, "login failed");
    } else {
        tracing::info!(target: "audit", account = "unknown", "login failed");
    }
}

fn auth_error(error: crate::auth::AuthError) -> ApiError {
    ApiError::Internal(Box::new(error))
}

fn user_view(user: &UserRow) -> User {
    User {
        id: user.id,
        email: user.email.clone(),
        name: user.name.clone(),
        is_instance_admin: user.is_instance_admin,
    }
}

fn optional_name(value: Option<String>) -> Result<Option<String>, ApiError> {
    match value {
        None => Ok(None),
        Some(name) => Ok(Some(trimmed_name("name", &name)?)),
    }
}

fn trimmed_name(field: &str, value: &str) -> Result<String, ApiError> {
    let name = value.trim();
    let length = name.chars().count();
    if !(1..=100).contains(&length) {
        return Err(field_error(field, "must be 1 to 100 characters"));
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

fn forbidden(detail: Option<&str>) -> Response {
    error::problem_response(&Problem::new(
        ProblemCode::Forbidden,
        detail.map(str::to_owned),
    ))
}

fn bootstrap_error(error: BootstrapError) -> ApiError {
    match error {
        BootstrapError::Meta(error) => ApiError::from(error),
        other => ApiError::Internal(Box::new(other)),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Method;

    use super::*;
    use crate::api::account::{OrgRole, SignupPolicy};
    use crate::api::common::ProblemCode;
    use crate::auth::secret::hash_token;
    use crate::meta::members::org_role;
    use crate::meta::projects::project_by_slugs;
    use crate::meta::users::{count_users, user_by_email};
    use crate::test_support::{TestApp, assert_logs_clean, capture_logs};

    const PASSWORD: &str = "llmobs-ok1";

    fn body(token: &str, password: &str) -> serde_json::Value {
        serde_json::json!({
            "setup_token": token,
            "email": "ada@example.com",
            "password": password,
            "name": " Ada ",
            "org_name": " Acme ",
            "project_name": " Support bot ",
        })
    }

    async fn prepared() -> (TestApp, String) {
        let app = TestApp::new().await;
        let (prepared, logs) = capture_logs(bootstrap::prepare(&app.state)).await;
        prepared.unwrap();
        let token = app.state.setup.token().unwrap().expect("setup token");
        assert_eq!(logs.matches(&token).count(), 1, "{logs}");
        assert!(
            logs.contains("setup required: open http://localhost:8080/setup?token="),
            "{logs}"
        );
        (app, token)
    }

    #[tokio::test]
    async fn setup_creates_the_admin_and_a_second_call_conflicts() {
        let (app, token) = prepared().await;
        let config: AuthConfig = app
            .request(Method::GET, "/api/auth/config")
            .send()
            .await
            .json();
        assert!(config.setup_required);
        assert!(config.password_enabled);
        assert_eq!(config.signup, SignupPolicy::InviteOnly);
        assert!(config.providers.is_empty());
        assert!(!config.smtp_enabled);
        assert!(!config.email_verification_required);

        let rejected = app
            .request(Method::POST, "/api/auth/setup")
            .json(&body("not-the-token", PASSWORD))
            .send()
            .await;
        assert_eq!(rejected.status, 403);

        let created = app
            .request(Method::POST, "/api/auth/setup")
            .json(&body(&token, PASSWORD))
            .send()
            .await;
        assert_eq!(created.status, 201);
        let cookie = created.headers.get(axum::http::header::SET_COOKIE).unwrap();
        let cookie = cookie.to_str().unwrap();
        assert!(
            cookie.contains(&format!("{}=", llmobs_core::llmobs_name!("_session"))),
            "{cookie}"
        );
        let response: SetupResponse = created.json();
        assert!(response.user.is_instance_admin);
        assert_eq!(response.user.email, "ada@example.com");
        assert_eq!(response.user.name.as_deref(), Some("Ada"));
        assert_eq!(response.org.role, OrgRole::Owner);
        assert_eq!(response.org.slug, "acme");
        assert_eq!(response.project.slug, "support-bot");
        assert_eq!(response.project.role, Some(OrgRole::Owner));
        assert!(response.secret_key_prefix.len() < response.secret_key.len());
        assert!(response.secret_key.starts_with(&response.secret_key_prefix));

        let user = user_by_email(app.state.meta.reader(), "ada@example.com")
            .await
            .unwrap()
            .unwrap();
        assert!(user.is_instance_admin);
        assert_eq!(user.email_verified_at, Some(app.state.clock.now_ns()));
        assert!(user.password_hash.unwrap().starts_with("$argon2id$"));
        assert_eq!(
            org_role(app.state.meta.reader(), response.org.id, user.id)
                .await
                .unwrap(),
            Some(OrgRole::Owner)
        );
        assert!(
            project_by_slugs(app.state.meta.reader(), "acme", "support-bot")
                .await
                .unwrap()
                .is_some()
        );
        let (hash, prefix, name, scopes): (String, String, String, String) =
            sqlx::query_as("SELECT key_hash, prefix, name, scopes FROM api_keys")
                .fetch_one(app.state.meta.reader())
                .await
                .unwrap();
        assert_eq!(hash, hash_token(&response.secret_key));
        assert_eq!(prefix, response.secret_key_prefix);
        assert_eq!(name, "setup");
        assert_eq!(scopes, r#"["ingest","read","write"]"#);
        let stored = format!("{hash}{prefix}{name}{scopes}");
        assert!(!stored.contains(&response.secret_key));
        let sessions: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM web_sessions WHERE user_id = ?")
                .bind(user.id.to_string())
                .fetch_one(app.state.meta.reader())
                .await
                .unwrap();
        assert_eq!(sessions, 1);

        let again: AuthConfig = app
            .request(Method::GET, "/api/auth/config")
            .send()
            .await
            .json();
        assert!(!again.setup_required);
        let second = app
            .request(Method::POST, "/api/auth/setup")
            .json(&body(&token, PASSWORD))
            .send()
            .await;
        assert_eq!(second.status, 409);
        assert_eq!(second.problem().detail.as_deref(), Some("already set up"));
    }

    #[tokio::test]
    async fn two_setups_at_once_create_one_admin() {
        let (app, token) = prepared().await;
        let payload = body(&token, PASSWORD);
        let (left, right) = tokio::join!(
            app.request(Method::POST, "/api/auth/setup")
                .json(&payload)
                .send(),
            app.request(Method::POST, "/api/auth/setup")
                .json(&payload)
                .send(),
        );
        let mut statuses = [left.status.as_u16(), right.status.as_u16()];
        statuses.sort_unstable();
        assert_eq!(statuses, [201, 409]);
        assert_eq!(count_users(app.state.meta.reader()).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn a_weak_password_is_a_field_error_and_is_not_logged() {
        let (app, token) = prepared().await;
        let password = "nope-nope";
        let response = assert_logs_clean(&[password, "ada@example.com"], async {
            app.request(Method::POST, "/api/auth/setup")
                .json(&body(&token, password))
                .send()
                .await
        })
        .await;
        assert_eq!(response.status, 400);
        let problem = response.problem();
        assert_eq!(problem.errors.len(), 1);
        assert_eq!(problem.errors[0].field, "password");
        assert_eq!(problem.errors[0].message, "password is too short");
        let text = String::from_utf8_lossy(&response.body);
        assert!(!text.contains(password), "{text}");
        assert_eq!(count_users(app.state.meta.reader()).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn password_setup_can_be_disabled() {
        let app = TestApp::with_config(|config| config.auth.disable_password = true).await;
        bootstrap::prepare(&app.state).await.unwrap();
        let response = app
            .request(Method::POST, "/api/auth/setup")
            .json(&body("token", PASSWORD))
            .send()
            .await;
        assert_eq!(response.status, 403);
        let problem = response.problem();
        assert_eq!(problem.code, ProblemCode::Forbidden);
        assert_eq!(
            problem.detail.as_deref(),
            Some("password setup is disabled; use an identity provider")
        );
        let config: AuthConfig = app
            .request(Method::GET, "/api/auth/config")
            .send()
            .await
            .json();
        assert!(!config.password_enabled);
    }

    use std::net::IpAddr;
    use std::time::Duration;

    use crate::api::account::{LoginRequest, Me, SignupRequest, SignupResponse};
    use crate::auth::password::Hasher;
    use crate::meta::members::add_org_member;
    use crate::meta::orgs::insert_org_with_derived_slug;
    use crate::meta::users::{NewUser, insert_user, set_disabled};

    fn login_body(email: &str, password: &str) -> LoginRequest {
        LoginRequest {
            email: email.to_owned(),
            password: password.to_owned(),
        }
    }

    fn signup_body(email: &str, password: &str, name: Option<&str>) -> SignupRequest {
        SignupRequest {
            email: email.to_owned(),
            password: password.to_owned(),
            name: name.map(str::to_owned),
        }
    }

    fn session_cookie(response: &crate::test_support::TestResponse) -> String {
        let set = response
            .headers
            .get(axum::http::header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        set.split(';').next().unwrap().to_owned()
    }

    fn verifies(app: &TestApp) -> u64 {
        app.state
            .password_hasher
            .get()
            .map(Hasher::verify_count)
            .unwrap_or(0)
    }

    async fn insert_account(
        app: &TestApp,
        email: &str,
        password: Option<&str>,
        verified: bool,
    ) -> Uuid {
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
        let verified_at = verified.then_some(now);
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
                            email_verified_at: verified_at,
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

    #[tokio::test]
    async fn login_sets_a_cookie_and_me_returns_the_user() {
        let app = TestApp::new().await;
        let user_id = insert_account(&app, "ada@example.com", Some(PASSWORD), true).await;
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    let org = insert_org_with_derived_slug(conn, "Acme", now).await?;
                    add_org_member(conn, org.id, user_id, OrgRole::Owner, now).await?;
                    Ok(org)
                })
            })
            .await
            .unwrap();
        let logged_in = app
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("Ada@Example.com", PASSWORD))
            .send()
            .await;
        assert_eq!(logged_in.status, 200);
        let cookie = session_cookie(&logged_in);
        let me: Me = logged_in.json();
        assert_eq!(me.user.email, "ada@example.com");
        assert_eq!(me.user.name.as_deref(), Some("Ada"));
        assert_eq!(me.orgs.len(), 1);
        assert_eq!(me.orgs[0].slug, "acme");
        assert_eq!(me.orgs[0].role, OrgRole::Owner);
        let again: Me = app
            .request(Method::GET, "/api/auth/me")
            .header("cookie", &cookie)
            .send()
            .await
            .json();
        assert_eq!(again, me);
    }

    #[tokio::test]
    async fn failed_logins_answer_the_same_and_each_verifies_once() {
        let app = TestApp::new().await;
        insert_account(&app, "ada@example.com", Some(PASSWORD), true).await;
        let disabled = insert_account(&app, "off@example.com", Some(PASSWORD), true).await;
        insert_account(&app, "sso@example.com", None, true).await;
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move { set_disabled(conn, disabled, Some(now), now).await })
            })
            .await
            .unwrap();
        for _ in 0..20 {
            app.state
                .limits
                .lockout
                .record_failure("locked@example.com", now);
        }
        insert_account(&app, "locked@example.com", Some(PASSWORD), true).await;

        let cases = [
            login_body("missing@example.com", PASSWORD),
            login_body("ada@example.com", "not-the-password"),
            login_body("off@example.com", PASSWORD),
            login_body("sso@example.com", PASSWORD),
            login_body("locked@example.com", PASSWORD),
        ];
        let mut bodies = Vec::new();
        for case in cases {
            let before = verifies(&app);
            let response = app
                .request(Method::POST, "/api/auth/login")
                .json(&case)
                .send()
                .await;
            assert_eq!(
                response.status,
                401,
                "{}",
                String::from_utf8_lossy(&response.body)
            );
            assert_eq!(verifies(&app) - before, 1);
            bodies.push(response.body);
        }
        for body in &bodies[1..] {
            assert_eq!(body, &bodies[0]);
        }
        let before = verifies(&app);
        let invalid = app
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("not-an-email", PASSWORD))
            .send()
            .await;
        assert_eq!(invalid.status, 401);
        assert_eq!(invalid.body, bodies[0]);
        assert_eq!(verifies(&app), before);
    }

    #[tokio::test]
    async fn lockout_and_the_login_rate_limit() {
        let app = TestApp::new().await;
        insert_account(&app, "ada@example.com", Some(PASSWORD), true).await;
        let mut failures = 0;
        while failures < 20 {
            let response = app
                .request(Method::POST, "/api/auth/login")
                .json(&login_body("ada@example.com", "not-the-password"))
                .send()
                .await;
            if response.status == 429 {
                app.state.clock.advance(Duration::from_secs(60));
                continue;
            }
            assert_eq!(response.status, 401);
            failures += 1;
        }
        app.state.clock.advance(Duration::from_secs(60));
        let other: IpAddr = "127.0.0.2".parse().unwrap();
        let locked = app
            .request(Method::POST, "/api/auth/login")
            .peer(other)
            .json(&login_body("ada@example.com", PASSWORD))
            .send()
            .await;
        assert_eq!(locked.status, 401);
        app.state.clock.advance(Duration::from_secs(15 * 60));
        let opened = app
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("ada@example.com", PASSWORD))
            .send()
            .await;
        assert_eq!(opened.status, 200);

        let limited = TestApp::new().await;
        insert_account(&limited, "ada@example.com", Some(PASSWORD), true).await;
        for _ in 0..10 {
            let response = limited
                .request(Method::POST, "/api/auth/login")
                .json(&login_body("ada@example.com", "not-the-password"))
                .send()
                .await;
            assert_eq!(response.status, 401);
        }
        let blocked = limited
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("ada@example.com", "not-the-password"))
            .send()
            .await;
        assert_eq!(blocked.status, 429);
        assert_eq!(
            blocked
                .headers
                .get(axum::http::header::RETRY_AFTER)
                .unwrap()
                .to_str()
                .unwrap(),
            "6"
        );
        let elsewhere = limited
            .request(Method::POST, "/api/auth/login")
            .peer(other)
            .json(&login_body("ada@example.com", "not-the-password"))
            .send()
            .await;
        assert_eq!(elsewhere.status, 401);
    }

    #[tokio::test]
    async fn an_sso_domain_refuses_known_and_unknown_accounts() {
        let app = TestApp::with_config(|config| {
            config.auth.enforce_sso_domains = vec!["example.com".to_owned()];
        })
        .await;
        insert_account(&app, "ada@example.com", Some(PASSWORD), true).await;
        let before = verifies(&app);
        for email in ["ada@example.com", "missing@example.com"] {
            let response = app
                .request(Method::POST, "/api/auth/login")
                .json(&login_body(email, PASSWORD))
                .send()
                .await;
            assert_eq!(response.status, 403);
            assert_eq!(response.problem().code, ProblemCode::SsoRequired);
        }
        assert_eq!(verifies(&app), before);
    }

    #[tokio::test]
    async fn email_not_verified_requires_the_right_password() {
        let app = TestApp::with_config(|config| {
            config.auth.require_email_verification = true;
            config.auth.signup = SignupPolicy::Open;
        })
        .await;
        let created = app
            .request(Method::POST, "/api/auth/signup")
            .json(&signup_body("ada@example.com", PASSWORD, None))
            .send()
            .await;
        assert_eq!(created.status, 201);
        assert!(
            created
                .headers
                .get(axum::http::header::SET_COOKIE)
                .is_none()
        );
        let body: SignupResponse = created.json();
        assert!(body.me.is_none());
        assert!(body.verification_required);
        let wrong = app
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("ada@example.com", "not-the-password"))
            .send()
            .await;
        assert_eq!(wrong.status, 401);
        assert_eq!(wrong.problem().code, ProblemCode::Unauthorized);
        let right = app
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("ada@example.com", PASSWORD))
            .send()
            .await;
        assert_eq!(right.status, 403);
        assert_eq!(right.problem().code, ProblemCode::EmailNotVerified);
    }

    #[tokio::test]
    async fn a_weaker_hash_is_replaced_on_login() {
        let app = TestApp::new().await;
        let hash = weak_hash(PASSWORD);
        assert!(hash.contains(",t=1,"));
        let email = "ada@example.com".to_owned();
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    insert_user(
                        conn,
                        NewUser {
                            email,
                            name: None,
                            password_hash: Some(hash),
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
        let response = app
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("ada@example.com", PASSWORD))
            .send()
            .await;
        assert_eq!(response.status, 200);
        let stored: String = sqlx::query_scalar("SELECT password_hash FROM users")
            .fetch_one(app.state.meta.reader())
            .await
            .unwrap();
        assert!(stored.contains(",t=2,"), "{stored}");
        assert!(!stored.contains(",t=1,"), "{stored}");
    }

    #[tokio::test]
    async fn logout_deletes_the_session() {
        let app = TestApp::new().await;
        insert_account(&app, "ada@example.com", Some(PASSWORD), true).await;
        let logged_in = app
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("ada@example.com", PASSWORD))
            .send()
            .await;
        let cookie = session_cookie(&logged_in);
        let gone = app
            .request(Method::POST, "/api/auth/logout")
            .header("cookie", &cookie)
            .send()
            .await;
        assert_eq!(gone.status, 204);
        let cleared = gone
            .headers
            .get(axum::http::header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(cleared.contains("Max-Age=0"), "{cleared}");
        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM web_sessions")
            .fetch_one(app.state.meta.reader())
            .await
            .unwrap();
        assert_eq!(sessions, 0);
        let me = app
            .request(Method::GET, "/api/auth/me")
            .header("cookie", &cookie)
            .send()
            .await;
        assert_eq!(me.status, 401);
    }

    #[tokio::test]
    async fn signup_follows_the_policy_and_rejects_a_duplicate() {
        let disabled =
            TestApp::with_config(|config| config.auth.signup = SignupPolicy::Disabled).await;
        let closed = disabled
            .request(Method::POST, "/api/auth/signup")
            .json(&signup_body("ada@example.com", PASSWORD, None))
            .send()
            .await;
        assert_eq!(closed.status, 403);
        assert_eq!(
            closed.problem().detail.as_deref(),
            Some("sign-up is disabled")
        );

        let invited = TestApp::new().await;
        let needs_invite = invited
            .request(Method::POST, "/api/auth/signup")
            .json(&signup_body("ada@example.com", PASSWORD, None))
            .send()
            .await;
        assert_eq!(needs_invite.status, 403);
        assert_eq!(
            needs_invite.problem().detail.as_deref(),
            Some("sign-up requires an invitation")
        );

        let passwords_off =
            TestApp::with_config(|config| config.auth.disable_password = true).await;
        let refused = passwords_off
            .request(Method::POST, "/api/auth/signup")
            .json(&signup_body("ada@example.com", PASSWORD, None))
            .send()
            .await;
        assert_eq!(refused.status, 403);
        assert_eq!(refused.problem().code, ProblemCode::Forbidden);
        assert!(refused.problem().detail.is_none());
        let login_refused = passwords_off
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("ada@example.com", PASSWORD))
            .send()
            .await;
        assert_eq!(login_refused.status, 403);
        assert_eq!(login_refused.problem().code, ProblemCode::Forbidden);

        let open = TestApp::with_config(|config| {
            config.auth.signup = SignupPolicy::Open;
            config.auth.signup_domains = vec!["example.com".to_owned(), "sso.example".to_owned()];
            config.auth.enforce_sso_domains = vec!["sso.example".to_owned()];
        })
        .await;
        let outside = open
            .request(Method::POST, "/api/auth/signup")
            .json(&signup_body("ada@other.test", PASSWORD, None))
            .send()
            .await;
        assert_eq!(outside.status, 403);
        assert_eq!(outside.problem().code, ProblemCode::Forbidden);
        let sso = open
            .request(Method::POST, "/api/auth/signup")
            .json(&signup_body("ada@sso.example", PASSWORD, Some("Ada")))
            .send()
            .await;
        assert_eq!(sso.status, 403);
        assert_eq!(sso.problem().code, ProblemCode::SsoRequired);
        let created = open
            .request(Method::POST, "/api/auth/signup")
            .json(&signup_body("Ada@Example.com", PASSWORD, Some(" Ada ")))
            .send()
            .await;
        assert_eq!(created.status, 201);
        let cookie = session_cookie(&created);
        let response: SignupResponse = created.json();
        assert!(!response.verification_required);
        let me = response.me.unwrap();
        assert_eq!(me.user.email, "ada@example.com");
        assert_eq!(me.user.name.as_deref(), Some("Ada"));
        assert!(!me.user.is_instance_admin);
        assert!(me.orgs.is_empty());
        let me_again: Me = open
            .request(Method::GET, "/api/auth/me")
            .header("cookie", &cookie)
            .send()
            .await
            .json();
        assert_eq!(me_again.user.id, me.user.id);
        let duplicate = open
            .request(Method::POST, "/api/auth/signup")
            .json(&signup_body("ADA@example.com", PASSWORD, None))
            .send()
            .await;
        assert_eq!(duplicate.status, 409);
        assert_eq!(
            duplicate.problem().detail.as_deref(),
            Some("email already registered")
        );
        let long_name = "n".repeat(101);
        let named = open
            .request(Method::POST, "/api/auth/signup")
            .json(&signup_body("bea@example.com", PASSWORD, Some(&long_name)))
            .send()
            .await;
        assert_eq!(named.status, 400);
        assert_eq!(named.problem().errors[0].field, "name");
    }

    #[tokio::test]
    async fn a_weak_signup_password_is_a_field_error() {
        let app = TestApp::with_config(|config| config.auth.signup = SignupPolicy::Open).await;
        let password = "nope-nope";
        let response = assert_logs_clean(&[password, "ada@example.com"], async {
            app.request(Method::POST, "/api/auth/signup")
                .json(&signup_body("ada@example.com", password, None))
                .send()
                .await
        })
        .await;
        assert_eq!(response.status, 400);
        assert_eq!(response.problem().errors[0].field, "password");
        assert_eq!(
            response.problem().errors[0].message,
            "password is too short"
        );
        assert!(!String::from_utf8_lossy(&response.body).contains(password));
        assert_eq!(count_users(app.state.meta.reader()).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn login_without_origin_is_forbidden_and_passwords_are_not_logged() {
        let app = TestApp::new().await;
        insert_account(&app, "ada@example.com", Some(PASSWORD), true).await;
        let missing_origin = app
            .request(Method::POST, "/api/auth/login")
            .origin(None)
            .json(&login_body("ada@example.com", PASSWORD))
            .send()
            .await;
        assert_eq!(missing_origin.status, 403);
        assert_eq!(
            missing_origin.problem().detail.as_deref(),
            Some("origin check failed")
        );
        let failed = assert_logs_clean(&[PASSWORD, "ada@example.com"], async {
            app.request(Method::POST, "/api/auth/login")
                .json(&login_body("ada@example.com", "not-the-password"))
                .send()
                .await
        })
        .await;
        assert_eq!(failed.status, 401);
        let (_, unknown_logs) = capture_logs(async {
            app.request(Method::POST, "/api/auth/login")
                .json(&login_body("missing@example.com", PASSWORD))
                .send()
                .await
        })
        .await;
        assert!(unknown_logs.contains("login failed"), "{unknown_logs}");
        assert!(unknown_logs.contains("unknown"), "{unknown_logs}");
        assert!(
            !unknown_logs.contains("missing@example.com"),
            "{unknown_logs}"
        );
        assert!(!unknown_logs.contains(PASSWORD), "{unknown_logs}");
        let (logged_in, logs) = capture_logs(async {
            app.request(Method::POST, "/api/auth/login")
                .json(&login_body("ada@example.com", PASSWORD))
                .send()
                .await
        })
        .await;
        assert_eq!(logged_in.status, 200, "{logs}");
        assert!(!logs.contains(PASSWORD), "{logs}");
        assert!(!logs.contains("ada@example.com"), "{logs}");
        assert!(logs.contains("login"), "{logs}");
    }

    fn weak_hash(password: &str) -> String {
        use argon2::password_hash::PasswordHasher as _;
        use argon2::{Algorithm, Argon2, Params, Version};

        let params = Params::new(19_456, 1, 1, None).unwrap();
        let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
        let salt = crate::auth::secret::random_bytes(16).unwrap();
        argon2
            .hash_password_with_salt(password.as_bytes(), &salt)
            .unwrap()
            .to_string()
    }

    fn sync_clock(app: &TestApp) {
        let now = crate::clock::Clock::System.now_ns();
        let crate::clock::Clock::Manual(nanos) = &app.state.clock else {
            panic!("test clock");
        };
        nanos.store(now, std::sync::atomic::Ordering::Relaxed);
    }

    fn reset_token(url: &str) -> &str {
        url.split("token=").nth(1).expect("reset url")
    }

    #[tokio::test]
    async fn password_reset_replaces_the_password_and_drops_sessions() {
        let app = TestApp::new().await;
        sync_clock(&app);
        insert_account(&app, "ada@example.com", Some(PASSWORD), true).await;
        let logged_in = app
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("ada@example.com", PASSWORD))
            .send()
            .await;
        assert_eq!(logged_in.status, 200);
        let cookie = session_cookie(&logged_in);
        let url = crate::admin::reset_password(&app.state.config, "ada@example.com")
            .await
            .unwrap();
        let token = reset_token(&url).to_owned();
        let reset = assert_logs_clean(
            &[&token, PASSWORD, "llmobs-ok2", "ada@example.com"],
            async {
                app.request(Method::POST, "/api/auth/password/reset")
                    .json(&serde_json::json!({ "token": &token, "password": "llmobs-ok2" }))
                    .send()
                    .await
            },
        )
        .await;
        assert_eq!(
            reset.status,
            204,
            "{}",
            String::from_utf8_lossy(&reset.body)
        );
        let me = app
            .request(Method::GET, "/api/auth/me")
            .header("cookie", &cookie)
            .send()
            .await;
        assert_eq!(me.status, 401);
        let old = app
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("ada@example.com", PASSWORD))
            .send()
            .await;
        assert_eq!(old.status, 401);
        let fresh = app
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("ada@example.com", "llmobs-ok2"))
            .send()
            .await;
        assert_eq!(fresh.status, 200);
        let again = app
            .request(Method::POST, "/api/auth/password/reset")
            .json(&serde_json::json!({ "token": token, "password": "llmobs-ok2" }))
            .send()
            .await;
        assert_eq!(again.status, 400);
        assert_eq!(
            again.problem().detail.as_deref(),
            Some("invalid or expired link")
        );
    }

    #[tokio::test]
    async fn expired_reset_link_is_rejected_and_a_weak_password_is_not_consumed() {
        let app = TestApp::new().await;
        sync_clock(&app);
        insert_account(&app, "ada@example.com", Some(PASSWORD), true).await;
        let url = crate::admin::reset_password(&app.state.config, "ada@example.com")
            .await
            .unwrap();
        app.state.clock.advance(Duration::from_secs(3600 + 5));
        let expired = app
            .request(Method::POST, "/api/auth/password/reset")
            .json(&serde_json::json!({
                "token": reset_token(&url),
                "password": "llmobs-ok2",
            }))
            .send()
            .await;
        assert_eq!(expired.status, 400);
        assert_eq!(
            expired.problem().detail.as_deref(),
            Some("invalid or expired link")
        );
        let still = app
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("ada@example.com", PASSWORD))
            .send()
            .await;
        assert_eq!(still.status, 200);

        let fresh = TestApp::new().await;
        sync_clock(&fresh);
        insert_account(&fresh, "ada@example.com", Some(PASSWORD), true).await;
        let shaped = fresh
            .request(Method::POST, "/api/auth/password/reset")
            .json(&serde_json::json!({ "token": "nope", "password": "llmobs-ok2" }))
            .send()
            .await;
        assert_eq!(shaped.status, 400);
        let url = crate::admin::reset_password(&fresh.state.config, "ada@example.com")
            .await
            .unwrap();
        let token = reset_token(&url);
        let weak = fresh
            .request(Method::POST, "/api/auth/password/reset")
            .json(&serde_json::json!({ "token": token, "password": "nope-nope" }))
            .send()
            .await;
        assert_eq!(weak.status, 400);
        assert_eq!(weak.problem().errors[0].field, "password");
        let accepted = fresh
            .request(Method::POST, "/api/auth/password/reset")
            .json(&serde_json::json!({ "token": token, "password": "llmobs-ok2" }))
            .send()
            .await;
        assert_eq!(
            accepted.status,
            204,
            "{}",
            String::from_utf8_lossy(&accepted.body)
        );
        let logged_in = fresh
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("ada@example.com", "llmobs-ok2"))
            .send()
            .await;
        assert_eq!(logged_in.status, 200);
    }

    #[tokio::test]
    async fn reset_unlocks_a_locked_email() {
        let app = TestApp::new().await;
        sync_clock(&app);
        insert_account(&app, "ada@example.com", Some(PASSWORD), true).await;
        let now = app.state.clock.now_ns();
        for _ in 0..20 {
            app.state
                .limits
                .lockout
                .record_failure("ada@example.com", now);
        }
        assert!(app.state.limits.lockout.is_locked("ada@example.com", now));
        let url = crate::admin::reset_password(&app.state.config, "ada@example.com")
            .await
            .unwrap();
        let reset = app
            .request(Method::POST, "/api/auth/password/reset")
            .json(&serde_json::json!({
                "token": reset_token(&url),
                "password": "llmobs-ok2",
            }))
            .send()
            .await;
        assert_eq!(reset.status, 204);
        assert!(!app.state.limits.lockout.is_locked("ada@example.com", now));
        let logged_in = app
            .request(Method::POST, "/api/auth/login")
            .json(&login_body("ada@example.com", "llmobs-ok2"))
            .send()
            .await;
        assert_eq!(logged_in.status, 200);
    }

    #[tokio::test]
    async fn reset_is_forbidden_when_password_login_is_disabled() {
        let app = TestApp::with_config(|config| config.auth.disable_password = true).await;
        let response = app
            .request(Method::POST, "/api/auth/password/reset")
            .json(&serde_json::json!({ "token": "nope", "password": "llmobs-ok2" }))
            .send()
            .await;
        assert_eq!(response.status, 403);
        assert_eq!(response.problem().code, ProblemCode::Forbidden);
        assert!(response.problem().detail.is_none());
    }
}
