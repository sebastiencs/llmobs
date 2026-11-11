//! `GET /api/auth/config` and `POST /api/auth/setup` (spec 08, D144).
#![cfg_attr(not(test), allow(dead_code))]

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

use crate::api::account::{
    AuthConfig, OrgMembership, OrgRole, Project, SetupRequest, SetupResponse, User,
};
use crate::api::common::{FieldError, Problem, ProblemCode};
use crate::auth::password::check_policy;
use crate::auth::secret::constant_time_eq;
use crate::auth::session::start_session;
use crate::bootstrap::{self, BootstrapError};
use crate::http::AppState;
use crate::http::access::{Access, RouteDef};
use crate::http::client_ip::ClientIp;
use crate::http::error::{self, ApiError};
use crate::meta::users::normalize_email;

/// Auth routes. Both are public. CSRF still applies to the POST (D141).
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
    ]
}

async fn config(State(state): State<AppState>) -> Result<error::Json<AuthConfig>, ApiError> {
    let users = crate::meta::users::count_users(state.meta.reader()).await?;
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
            role: OrgRole::Owner,
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

fn user_view(user: &crate::meta::users::UserRow) -> User {
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
        assert_eq!(response.project.role, OrgRole::Owner);
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
}
