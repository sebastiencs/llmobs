//! Instance administration (D149).
//!
//! Every route is `Access::InstanceAdmin`: a session whose user is an instance
//! admin. Settings are read from config. They are not editable.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::admin::{
    AdminLicense, AdminResetLink, AdminSettings, AdminUser, CreateAdminUserRequest,
    CreatedAdminUser, LicenseStatus, UpdateAdminUserRequest,
};
use crate::api::common::{FieldError, Page, PageParams, Problem, ProblemCode};
use crate::auth::principal::{self, Principal};
use crate::auth::reset::{self, ResetError};
use crate::ee::license::LicenseState;
use crate::email::Email;
use crate::http::AppState;
use crate::http::access::{Access, RouteDef};
use crate::http::error::{self, ApiError, problem_response};
use crate::meta::MetaError;
use crate::meta::identities;
use crate::meta::password_resets::insert_reset;
use crate::meta::users::{self, NewUser, UserRow};
use crate::meta::web_sessions::delete_user_sessions;

/// Instance-admin routes. A personal access token or an API key is refused.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/admin/users",
            access: Access::InstanceAdmin,
            documented: true,
            handler: get(list_users),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/v1/admin/users",
            access: Access::InstanceAdmin,
            documented: true,
            handler: post(create_user),
        },
        RouteDef {
            method: axum::http::Method::PATCH,
            path: "/api/v1/admin/users/{id}",
            access: Access::InstanceAdmin,
            documented: true,
            handler: patch(update_user),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/v1/admin/users/{id}/reset-link",
            access: Access::InstanceAdmin,
            documented: true,
            handler: post(reset_link),
        },
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/admin/settings",
            access: Access::InstanceAdmin,
            documented: true,
            handler: get(get_settings),
        },
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/admin/license",
            access: Access::InstanceAdmin,
            documented: true,
            handler: get(get_license),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/v1/admin/settings/test-email",
            access: Access::InstanceAdmin,
            documented: true,
            handler: post(send_test_email),
        },
    ]
}

async fn list_users(
    axum::extract::State(state): axum::extract::State<AppState>,
    error::Query(params): error::Query<PageParams>,
) -> Result<error::Json<Page<AdminUser>>, ApiError> {
    let after = match params.cursor.as_deref() {
        Some(cursor) => Some(cursor_email(cursor)?),
        None => None,
    };
    let limit = usize::try_from(params.limit.get()).unwrap_or(usize::MAX);
    let reader = state.meta.reader();
    let mut rows = users::list_users(
        reader,
        after.as_deref(),
        params.limit.get().saturating_add(1),
    )
    .await?;
    let more = rows.len() > limit;
    if more {
        rows.truncate(limit);
    }
    let next_cursor = if more {
        let Some(last) = rows.last() else {
            return Err(ApiError::Internal(Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "admin user page was empty",
            ))));
        };
        Some(encode_cursor(&last.email)?)
    } else {
        None
    };
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let providers = users::providers_by_user(reader, &ids).await?;
    let data = rows
        .iter()
        .map(|row| view(row, providers.get(&row.id)))
        .collect();
    Ok(error::Json(Page { data, next_cursor }))
}

async fn create_user(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Extension(principal): axum::extract::Extension<Principal>,
    error::Json(body): error::Json<CreateAdminUserRequest>,
) -> Result<Response, ApiError> {
    let actor = principal::session_user(&principal)?;
    let email =
        users::normalize_email(&body.email).map_err(|_| field_error("email", "invalid email"))?;
    let name = optional_name(body.name)?;
    let now = state.clock.now_ns();
    let minted = reset::mint_reset(&state.config.public_url, now).map_err(auth_failure)?;
    let hash = minted.hash.clone();
    let expires_at = minted.expires_at;
    let admin = body.is_instance_admin.unwrap_or(false);
    let actor_id = actor.id;
    let user = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                let user = users::insert_user(
                    conn,
                    NewUser {
                        email,
                        name,
                        password_hash: None,
                        email_verified_at: Some(now),
                        is_instance_admin: admin,
                    },
                    now,
                )
                .await?;
                insert_reset(conn, &hash, user.id, now, expires_at).await?;
                Ok(user)
            })
        })
        .await?;
    tracing::info!(
        target: "audit",
        %actor_id,
        user_id = %user.id,
        "admin user created"
    );
    Ok((
        StatusCode::CREATED,
        error::Json(CreatedAdminUser {
            user: view(&user, None),
            reset_url: minted.url,
        }),
    )
        .into_response())
}

async fn update_user(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Extension(principal): axum::extract::Extension<Principal>,
    error::Path(UserPath { id }): error::Path<UserPath>,
    error::Json(body): error::Json<UpdateAdminUserRequest>,
) -> Result<Response, ApiError> {
    let actor = principal::session_user(&principal)?;
    if actor.id == id {
        return Err(ApiError::BadRequest {
            detail: Some("cannot change your own account".to_owned()),
            errors: Vec::new(),
        });
    }
    let disabled = body.disabled;
    let instance_admin = body.is_instance_admin;
    let now = state.clock.now_ns();
    let user = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                let Some(user) = users::user_by_id_tx(conn, id).await? else {
                    return Ok(None);
                };
                if disabled == Some(true)
                    && user.disabled_at.is_none()
                    && users::is_last_active_owner(conn, id).await?
                {
                    return Err(MetaError::Conflict(users::LAST_ORG_OWNER));
                }
                users::ensure_active_admin(conn, &user, disabled, instance_admin).await?;
                if let Some(disabled) = disabled {
                    let at = disabled.then_some(now);
                    users::set_disabled(conn, id, at, now).await?;
                }
                if let Some(admin) = instance_admin {
                    users::set_instance_admin(conn, id, admin, now).await?;
                }
                if disabled == Some(true) && user.disabled_at.is_none() {
                    delete_user_sessions(conn, id, None).await?;
                }
                users::user_by_id_tx(conn, id).await
            })
        })
        .await?;
    let Some(user) = user else {
        return Err(ApiError::NotFound);
    };
    tracing::info!(
        target: "audit",
        actor_id = %actor.id,
        user_id = %user.id,
        "admin user updated"
    );
    let providers = identities::user_identities(state.meta.reader(), user.id).await?;
    Ok((StatusCode::OK, error::Json(view(&user, Some(&providers)))).into_response())
}

async fn reset_link(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Extension(principal): axum::extract::Extension<Principal>,
    error::Path(UserPath { id }): error::Path<UserPath>,
) -> Result<Response, ApiError> {
    let actor = principal::session_user(&principal)?;
    let Some(user) = users::user_by_id(state.meta.reader(), id).await? else {
        return Err(ApiError::NotFound);
    };
    if user.disabled_at.is_some() {
        return Err(ApiError::Conflict {
            detail: "the user is disabled",
        });
    }
    let url = reset::reset_link(
        &state.meta,
        &state.config.public_url,
        id,
        state.clock.now_ns(),
    )
    .await
    .map_err(reset_failure)?;
    tracing::info!(
        target: "audit",
        actor_id = %actor.id,
        user_id = %user.id,
        "admin reset link"
    );
    Ok((StatusCode::OK, error::Json(AdminResetLink { url })).into_response())
}

async fn get_settings(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> error::Json<AdminSettings> {
    let auth = &state.config.auth;
    error::Json(AdminSettings {
        public_url: state.config.public_url.origin().to_owned(),
        signup: auth.signup,
        signup_domains: auth.signup_domains.clone(),
        password_enabled: !auth.disable_password,
        email_verification_required: auth.require_email_verification,
        enforce_sso_domains: auth.enforce_sso_domains.clone(),
        smtp_enabled: state.mailer.is_some(),
        email_from: state.config.email_from.clone(),
        providers: state.providers.list(),
        trusted_proxies: state
            .config
            .trusted_proxies
            .iter()
            .map(ToString::to_string)
            .collect(),
    })
}

async fn get_license(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> error::Json<AdminLicense> {
    let license = state.entitlements.license();
    let status = match license.status(state.clock.now_ns()) {
        crate::ee::license::LicenseStatus::Absent => LicenseStatus::None,
        crate::ee::license::LicenseStatus::Invalid => LicenseStatus::Invalid,
        crate::ee::license::LicenseStatus::Active => LicenseStatus::Active,
        crate::ee::license::LicenseStatus::Grace => LicenseStatus::Grace,
        crate::ee::license::LicenseStatus::Expired => LicenseStatus::Expired,
    };
    let none = AdminLicense {
        status,
        reason: None,
        id: None,
        customer: None,
        features: Vec::new(),
        expires_at: None,
        grace_ends_at: None,
    };
    error::Json(match license {
        LicenseState::Absent => none,
        LicenseState::Invalid(reason) => AdminLicense {
            reason: Some(reason.to_string()),
            ..none
        },
        LicenseState::Valid(valid) => AdminLicense {
            id: Some(valid.id),
            customer: Some(valid.customer.clone()),
            features: valid.features.to_vec(),
            expires_at: Some(valid.expires_at),
            grace_ends_at: Some(valid.grace_ends_at),
            ..none
        },
    })
}

async fn send_test_email(
    axum::extract::State(state): axum::extract::State<AppState>,
    axum::extract::Extension(principal): axum::extract::Extension<Principal>,
) -> Result<Response, ApiError> {
    let actor = principal::session_user(&principal)?;
    let Some(mailer) = state.mailer.as_ref() else {
        return Err(ApiError::BadRequest {
            detail: Some("email is not configured".to_owned()),
            errors: Vec::new(),
        });
    };
    if let Err(error) = mailer.send_now(&actor.email, &Email::Test).await {
        // `MailError`'s display is a fixed kind. It does not include the address.
        tracing::warn!(error = %error, "test email failed");
        return Ok(problem_response(&Problem::new(
            ProblemCode::Busy,
            Some("the SMTP server could not be reached or refused the message".to_owned()),
        )));
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// One user on a page. `providers` is absent when the user has no identity.
fn view(row: &UserRow, providers: Option<&Vec<String>>) -> AdminUser {
    AdminUser {
        id: row.id,
        email: row.email.clone(),
        name: row.name.clone(),
        is_instance_admin: row.is_instance_admin,
        disabled: row.disabled_at.is_some(),
        email_verified: row.email_verified_at.is_some(),
        has_password: row.password_hash.is_some(),
        providers: providers.cloned().unwrap_or_default(),
        created_at: row.created_at,
    }
}

fn cursor_email(cursor: &str) -> Result<String, ApiError> {
    let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| cursor_error())?;
    let stored: StoredCursor = serde_json::from_slice(&bytes).map_err(|_| cursor_error())?;
    if stored.email.is_empty() {
        return Err(cursor_error());
    }
    Ok(stored.email)
}

fn encode_cursor(email: &str) -> Result<String, ApiError> {
    let bytes = serde_json::to_vec(&StoredCursor {
        email: email.to_owned(),
    })
    .map_err(|error| ApiError::Internal(Box::new(error)))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn cursor_error() -> ApiError {
    field_error("cursor", "invalid cursor")
}

fn optional_name(name: Option<String>) -> Result<Option<String>, ApiError> {
    name.map(|name| {
        let name = name.trim().to_owned();
        if name.is_empty() || name.chars().count() > 100 {
            return Err(field_error("name", "name must be 1 to 100 characters"));
        }
        Ok(name)
    })
    .transpose()
}

fn field_error(field: &'static str, message: &'static str) -> ApiError {
    ApiError::BadRequest {
        detail: None,
        errors: vec![FieldError {
            field: field.to_owned(),
            message: message.to_owned(),
            position: None,
        }],
    }
}

fn auth_failure(error: crate::auth::AuthError) -> ApiError {
    ApiError::Internal(Box::new(error))
}

fn reset_failure(error: ResetError) -> ApiError {
    match error {
        ResetError::Auth(error) => auth_failure(error),
        ResetError::Meta(error) => error.into(),
    }
}

#[derive(Deserialize)]
struct UserPath {
    id: Uuid,
}

#[derive(Serialize, Deserialize)]
struct StoredCursor {
    email: String,
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, header};

    use crate::api::account::SetupResponse;
    use serde_json::json;

    use crate::api::admin::{AdminSettings, CreatedAdminUser};
    use crate::api::common::{Page, Problem};
    use crate::bootstrap;
    use crate::config::{Secret, SmtpUrl, Tls};
    use crate::test_support::{
        TestApp, TestResponse, assert_logs_clean, capture_logs, license_key, license_key_until,
    };

    const PASSWORD: &str = "llmobs-ok1";
    const REPLACEMENT: &str = "llmobs-ok2";

    async fn admin() -> (TestApp, String, SetupResponse) {
        admin_with(|_| {}).await
    }

    async fn admin_with(
        tweak: impl FnOnce(&mut crate::config::ServerConfig),
    ) -> (TestApp, String, SetupResponse) {
        let app = TestApp::with_config(tweak).await;
        bootstrap::prepare(&app.state).await.unwrap();
        let token = app.state.setup.token().unwrap().expect("setup token");
        let created = app
            .request(Method::POST, "/api/auth/setup")
            .json(&serde_json::json!({
                "setup_token": token,
                "email": "ada@example.com",
                "password": PASSWORD,
                "name": "Ada",
                "org_name": "Acme",
                "project_name": "Support bot",
            }))
            .send()
            .await;
        assert_eq!(created.status, 201);
        let cookie = session_cookie(&created);
        (app, cookie, created.json())
    }

    fn session_cookie(response: &TestResponse) -> String {
        response
            .header("set-cookie")
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    async fn create_account(
        app: &TestApp,
        cookie: &str,
        email: &str,
        instance_admin: bool,
    ) -> CreatedAdminUser {
        let (created, logs) = capture_logs(
            app.request(Method::POST, "/api/v1/admin/users")
                .header("cookie", cookie)
                .json(&serde_json::json!({
                    "email": email,
                    "name": " New ",
                    "is_instance_admin": instance_admin,
                }))
                .send(),
        )
        .await;
        assert_eq!(created.status, 201, "{logs}");
        let body: CreatedAdminUser = created.json();
        let token = body.reset_url.split("token=").nth(1).unwrap();
        assert!(!logs.contains(email), "{logs}");
        assert!(!logs.contains(token), "{logs}");
        assert!(!logs.contains(PASSWORD), "{logs}");
        body
    }

    async fn set_password(app: &TestApp, reset_url: &str, password: &str) {
        let token = reset_url.split("token=").nth(1).unwrap();
        let reset = app
            .request(Method::POST, "/api/auth/password/reset")
            .json(&serde_json::json!({ "token": token, "password": password }))
            .send()
            .await;
        assert_eq!(
            reset.status,
            204,
            "{}",
            reset.problem().detail.unwrap_or_default()
        );
    }

    async fn login(app: &TestApp, email: &str, password: &str) -> TestResponse {
        app.request(Method::POST, "/api/auth/login")
            .json(&serde_json::json!({ "email": email, "password": password }))
            .send()
            .await
    }

    #[tokio::test]
    async fn only_an_instance_admin_session_may_call_the_routes() {
        let (app, cookie, setup) = admin().await;
        let anonymous = app.request(Method::GET, "/api/v1/admin/users").send().await;
        assert_eq!(anonymous.status, 401);

        let token = app
            .request(Method::POST, "/api/v1/me/tokens")
            .header("cookie", &cookie)
            .json(&serde_json::json!({ "name": "read", "scopes": ["read"] }))
            .send()
            .await;
        assert_eq!(token.status, 201);
        let secret = token.json::<serde_json::Value>()["secret"]
            .as_str()
            .unwrap()
            .to_owned();
        let bearer = format!("Bearer {secret}");
        let pat = app
            .request(Method::GET, "/api/v1/admin/settings")
            .header(header::AUTHORIZATION.as_str(), &bearer)
            .send()
            .await;
        assert_eq!(pat.status, 403);
        let key = app
            .request(Method::GET, "/api/v1/admin/settings")
            .header(
                header::AUTHORIZATION.as_str(),
                &format!("Bearer {}", setup.secret_key),
            )
            .send()
            .await;
        assert_eq!(key.status, 403);

        let created = create_account(&app, &cookie, "bob@example.com", false).await;
        set_password(&app, &created.reset_url, REPLACEMENT).await;
        let bob = login(&app, "bob@example.com", REPLACEMENT).await;
        assert_eq!(bob.status, 200);
        let bob_cookie = session_cookie(&bob);
        let id = created.user.id;
        let calls = [
            (Method::GET, "/api/v1/admin/users".to_owned(), None),
            (
                Method::POST,
                "/api/v1/admin/users".to_owned(),
                Some(serde_json::json!({ "email": "cara@example.com" })),
            ),
            (
                Method::PATCH,
                format!("/api/v1/admin/users/{id}"),
                Some(serde_json::json!({ "disabled": true })),
            ),
            (
                Method::POST,
                format!("/api/v1/admin/users/{id}/reset-link"),
                None,
            ),
            (Method::GET, "/api/v1/admin/settings".to_owned(), None),
            (
                Method::POST,
                "/api/v1/admin/settings/test-email".to_owned(),
                None,
            ),
        ];
        for (method, path, body) in calls {
            let mut request = app
                .request(method.clone(), &path)
                .header("cookie", &bob_cookie);
            if let Some(body) = body {
                request = request.json(&body);
            }
            let response = request.send().await;
            assert_eq!(response.status, 403, "{method} {path}");
        }
    }

    #[tokio::test]
    async fn creating_a_user_returns_a_working_reset_link_and_pages_by_email() {
        let (app, cookie, _) = admin().await;
        let bob = create_account(&app, &cookie, "bob@example.com", false).await;
        assert!(!bob.user.has_password);
        assert!(bob.user.email_verified);
        assert_eq!(bob.user.name.as_deref(), Some("New"));
        assert!(bob.user.providers.is_empty());
        set_password(&app, &bob.reset_url, REPLACEMENT).await;
        let logged_in = login(&app, "bob@example.com", REPLACEMENT).await;
        assert_eq!(logged_in.status, 200);

        let duplicate = app
            .request(Method::POST, "/api/v1/admin/users")
            .header("cookie", &cookie)
            .json(&serde_json::json!({ "email": "Bob@Example.com" }))
            .send()
            .await;
        assert_eq!(duplicate.status, 409);
        assert_eq!(
            duplicate.problem().detail.as_deref(),
            Some("email already registered")
        );

        create_account(&app, &cookie, "zed@example.com", false).await;
        let first = app
            .request(Method::GET, "/api/v1/admin/users?limit=2")
            .header("cookie", &cookie)
            .send()
            .await;
        assert_eq!(first.status, 200);
        let first: Page<crate::api::admin::AdminUser> = first.json();
        let emails: Vec<&str> = first.data.iter().map(|user| user.email.as_str()).collect();
        assert_eq!(emails, ["ada@example.com", "bob@example.com"]);
        let cursor = first.next_cursor.unwrap();
        let second = app
            .request(
                Method::GET,
                &format!("/api/v1/admin/users?limit=2&cursor={cursor}"),
            )
            .header("cookie", &cookie)
            .send()
            .await;
        let second: Page<crate::api::admin::AdminUser> = second.json();
        assert_eq!(second.data[0].email, "zed@example.com");
        assert!(second.next_cursor.is_none());

        let bad = app
            .request(Method::GET, "/api/v1/admin/users?cursor=not-a-cursor")
            .header("cookie", &cookie)
            .send()
            .await;
        assert_eq!(bad.status, 400);
        assert_eq!(bad.problem().errors[0].field, "cursor");
    }

    #[tokio::test]
    async fn disabling_a_user_drops_the_session_and_the_token() {
        let (app, cookie, _) = admin().await;
        let created = create_account(&app, &cookie, "bob@example.com", false).await;
        set_password(&app, &created.reset_url, REPLACEMENT).await;
        let bob = login(&app, "bob@example.com", REPLACEMENT).await;
        let bob_cookie = session_cookie(&bob);
        let token = app
            .request(Method::POST, "/api/v1/me/tokens")
            .header("cookie", &bob_cookie)
            .json(&serde_json::json!({ "name": "read", "scopes": ["read"] }))
            .send()
            .await;
        assert_eq!(token.status, 201);
        let secret = token.json::<serde_json::Value>()["secret"]
            .as_str()
            .unwrap()
            .to_owned();

        let disabled = app
            .request(
                Method::PATCH,
                &format!("/api/v1/admin/users/{}", created.user.id),
            )
            .header("cookie", &cookie)
            .json(&serde_json::json!({ "disabled": true }))
            .send()
            .await;
        assert_eq!(disabled.status, 200);
        let user: crate::api::admin::AdminUser = disabled.json();
        assert!(user.disabled);

        let session = app
            .request(Method::GET, "/api/auth/me")
            .header("cookie", &bob_cookie)
            .send()
            .await;
        assert_eq!(session.status, 401);
        let pat = app
            .request(Method::GET, "/api/v1/orgs")
            .header(header::AUTHORIZATION.as_str(), &format!("Bearer {secret}"))
            .send()
            .await;
        assert_eq!(pat.status, 401);
        assert_eq!(
            login(&app, "bob@example.com", REPLACEMENT).await.status,
            401
        );

        let link = app
            .request(
                Method::POST,
                &format!("/api/v1/admin/users/{}/reset-link", created.user.id),
            )
            .header("cookie", &cookie)
            .send()
            .await;
        assert_eq!(link.status, 409);
        assert_eq!(
            link.problem().detail.as_deref(),
            Some("the user is disabled")
        );

        let enabled = app
            .request(
                Method::PATCH,
                &format!("/api/v1/admin/users/{}", created.user.id),
            )
            .header("cookie", &cookie)
            .json(&serde_json::json!({ "disabled": false }))
            .send()
            .await;
        assert_eq!(enabled.status, 200);
        assert_eq!(
            login(&app, "bob@example.com", REPLACEMENT).await.status,
            200
        );
    }

    #[tokio::test]
    async fn an_admin_cannot_change_themselves_and_can_demote_another() {
        let (app, cookie, setup) = admin().await;
        for body in [
            serde_json::json!({ "disabled": true }),
            serde_json::json!({ "is_instance_admin": false }),
            serde_json::json!({}),
        ] {
            let response = app
                .request(
                    Method::PATCH,
                    &format!("/api/v1/admin/users/{}", setup.user.id),
                )
                .header("cookie", &cookie)
                .json(&body)
                .send()
                .await;
            assert_eq!(response.status, 400, "{body}");
            assert_eq!(
                response.problem().detail.as_deref(),
                Some("cannot change your own account")
            );
        }

        let other = create_account(&app, &cookie, "bob@example.com", true).await;
        assert!(other.user.is_instance_admin);
        let demoted = app
            .request(
                Method::PATCH,
                &format!("/api/v1/admin/users/{}", other.user.id),
            )
            .header("cookie", &cookie)
            .json(&serde_json::json!({ "is_instance_admin": false }))
            .send()
            .await;
        assert_eq!(demoted.status, 200);
        let demoted: crate::api::admin::AdminUser = demoted.json();
        assert!(!demoted.is_instance_admin);
        let still = app
            .request(Method::GET, "/api/v1/admin/settings")
            .header("cookie", &cookie)
            .send()
            .await;
        assert_eq!(still.status, 200);
    }

    #[tokio::test]
    async fn the_last_owner_of_an_org_cannot_be_disabled() {
        let (app, cookie, _) = admin().await;
        let created = create_account(&app, &cookie, "bob@example.com", false).await;
        set_password(&app, &created.reset_url, REPLACEMENT).await;
        let bob = login(&app, "bob@example.com", REPLACEMENT).await;
        let org = app
            .request(Method::POST, "/api/v1/orgs")
            .header("cookie", &session_cookie(&bob))
            .json(&serde_json::json!({ "name": "Other" }))
            .send()
            .await;
        assert_eq!(org.status, 201);
        let denied = app
            .request(
                Method::PATCH,
                &format!("/api/v1/admin/users/{}", created.user.id),
            )
            .header("cookie", &cookie)
            .json(&serde_json::json!({ "disabled": true }))
            .send()
            .await;
        assert_eq!(denied.status, 409);
        assert_eq!(
            denied.problem().detail.as_deref(),
            Some("the user is the last owner of an org")
        );
        assert_eq!(
            login(&app, "bob@example.com", REPLACEMENT).await.status,
            200
        );
    }

    #[tokio::test]
    async fn settings_omit_the_smtp_secret() {
        const SMTP_PASSWORD: &str = "s3cret-smtp-password";
        let app = TestApp::with_mailer_config(|config| {
            let secret: Secret<String> =
                serde_json::from_str(&format!("\"{SMTP_PASSWORD}\"")).unwrap();
            config.smtp_url = Some(SmtpUrl {
                tls: Tls::Required,
                host: "smtp.example.com".to_owned(),
                port: 587,
                credentials: Some(("mailer".to_owned(), secret)),
            });
            config.email_from = Some("llmobs <noreply@example.com>".to_owned());
        })
        .await;
        bootstrap::prepare(&app.state).await.unwrap();
        let token = app.state.setup.token().unwrap().expect("setup token");
        let created = app
            .request(Method::POST, "/api/auth/setup")
            .json(&serde_json::json!({
                "setup_token": token,
                "email": "ada@example.com",
                "password": PASSWORD,
                "name": "Ada",
                "org_name": "Acme",
                "project_name": "Support bot",
            }))
            .send()
            .await;
        let cookie = session_cookie(&created);
        let (response, logs) = capture_logs(
            app.request(Method::GET, "/api/v1/admin/settings")
                .header("cookie", &cookie)
                .send(),
        )
        .await;
        assert_eq!(response.status, 200);
        let text = String::from_utf8_lossy(&response.body);
        assert!(!text.contains(SMTP_PASSWORD), "{text}");
        assert!(!text.contains("smtp://"), "{text}");
        assert!(!logs.contains(SMTP_PASSWORD), "{logs}");
        let settings: AdminSettings = response.json();
        assert_eq!(settings.public_url, "http://localhost:8080");
        assert!(settings.smtp_enabled);
        assert_eq!(
            settings.email_from.as_deref(),
            Some("llmobs <noreply@example.com>")
        );
        assert!(settings.providers.is_empty());
        assert!(settings.password_enabled);
        assert!(settings.trusted_proxies.is_empty());
    }

    #[tokio::test]
    async fn a_test_email_goes_to_the_caller() {
        let app = TestApp::with_mailer().await;
        bootstrap::prepare(&app.state).await.unwrap();
        let token = app.state.setup.token().unwrap().expect("setup token");
        let created = assert_logs_clean(&[PASSWORD], async {
            app.request(Method::POST, "/api/auth/setup")
                .json(&serde_json::json!({
                    "setup_token": token,
                    "email": "ada@example.com",
                    "password": PASSWORD,
                    "name": "Ada",
                    "org_name": "Acme",
                    "project_name": "Support bot",
                }))
                .send()
                .await
        })
        .await;
        let cookie = session_cookie(&created);
        let (sent, logs) = capture_logs(
            app.request(Method::POST, "/api/v1/admin/settings/test-email")
                .header("cookie", &cookie)
                .send(),
        )
        .await;
        assert_eq!(sent.status, 204);
        assert!(!logs.contains("ada@example.com"), "{logs}");
        let mail = app.sent();
        assert_eq!(mail.len(), 1);
        assert_eq!(mail[0].to, "ada@example.com");

        app.state.mailer.as_ref().unwrap().fail_next(1);
        let (failed, logs) = capture_logs(
            app.request(Method::POST, "/api/v1/admin/settings/test-email")
                .header("cookie", &cookie)
                .send(),
        )
        .await;
        assert_eq!(failed.status, 503);
        let problem: Problem = failed.json();
        assert_eq!(
            problem.detail.as_deref(),
            Some("the SMTP server could not be reached or refused the message")
        );
        assert!(!logs.contains("ada@example.com"), "{logs}");
        assert_eq!(app.sent().len(), 1);

        let plain = TestApp::new().await;
        bootstrap::prepare(&plain.state).await.unwrap();
        let token = plain.state.setup.token().unwrap().expect("setup token");
        let created = plain
            .request(Method::POST, "/api/auth/setup")
            .json(&serde_json::json!({
                "setup_token": token,
                "email": "ada@example.com",
                "password": PASSWORD,
                "name": "Ada",
                "org_name": "Acme",
                "project_name": "Support bot",
            }))
            .send()
            .await;
        let missing = plain
            .request(Method::POST, "/api/v1/admin/settings/test-email")
            .header("cookie", &session_cookie(&created))
            .send()
            .await;
        assert_eq!(missing.status, 400);
        assert_eq!(
            missing.problem().detail.as_deref(),
            Some("email is not configured")
        );
    }

    async fn license(app: &TestApp, cookie: &str) -> serde_json::Value {
        let response = app
            .request(Method::GET, "/api/v1/admin/license")
            .header("cookie", cookie)
            .send()
            .await;
        assert_eq!(response.status, 200);
        response.json()
    }

    #[tokio::test]
    async fn license_needs_an_instance_admin() {
        let (app, cookie, _) = admin().await;
        let anonymous = app
            .request(Method::GET, "/api/v1/admin/license")
            .send()
            .await;
        assert_eq!(anonymous.status, 401);

        let created = create_account(&app, &cookie, "bob@example.com", false).await;
        set_password(&app, &created.reset_url, REPLACEMENT).await;
        let bob = login(&app, "bob@example.com", REPLACEMENT).await;
        let denied = app
            .request(Method::GET, "/api/v1/admin/license")
            .header("cookie", &session_cookie(&bob))
            .send()
            .await;
        assert_eq!(denied.status, 403);
    }

    #[tokio::test]
    async fn license_none_without_a_key() {
        let (app, cookie, _) = admin().await;
        assert_eq!(
            license(&app, &cookie).await,
            json!({ "status": "none", "features": [] })
        );
    }

    #[tokio::test]
    async fn license_active_lists_the_claims() {
        let key = license_key(&["project_roles"]);
        let (app, cookie, _) = admin_with(move |config| {
            config.license_key = Some(Secret::new(key));
        })
        .await;
        assert_eq!(
            license(&app, &cookie).await,
            json!({
                "status": "active",
                "id": "01900000-0000-7000-8000-000000000510",
                "customer": "Test Customer",
                "features": ["project_roles"],
                "expires_at": "2099-01-01T00:00:00.000000000Z",
                "grace_ends_at": "2099-01-01T00:00:00.000000000Z",
            })
        );
    }

    #[tokio::test]
    async fn license_grace_and_expired_keep_the_listed_features() {
        let grace = license_key_until(
            &["project_roles"],
            "2025-12-01T00:00:00Z",
            "2026-02-01T00:00:00Z",
        );
        let (app, cookie, _) = admin_with(move |config| {
            config.license_key = Some(Secret::new(grace));
        })
        .await;
        let body = license(&app, &cookie).await;
        assert_eq!(body["status"], "grace");
        assert_eq!(body["features"], json!(["project_roles"]));

        let expired = license_key_until(
            &["project_roles"],
            "2025-11-01T00:00:00Z",
            "2025-12-01T00:00:00Z",
        );
        let (app, cookie, _) = admin_with(move |config| {
            config.license_key = Some(Secret::new(expired));
        })
        .await;
        let body = license(&app, &cookie).await;
        assert_eq!(body["status"], "expired");
        assert_eq!(body["features"], json!(["project_roles"]));
    }

    #[tokio::test]
    async fn license_invalid_never_echoes_the_key() {
        let (app, cookie, _) = admin_with(|config| {
            config.license_key = Some(Secret::new("garbage".to_owned()));
        })
        .await;
        let response = app
            .request(Method::GET, "/api/v1/admin/license")
            .header("cookie", &cookie)
            .send()
            .await;
        assert_eq!(response.status, 200);
        let text = String::from_utf8_lossy(&response.body).into_owned();
        assert!(!text.contains("garbage"), "{text}");
        assert_eq!(
            response.json::<serde_json::Value>(),
            json!({
                "status": "invalid",
                "reason": "the token is malformed",
                "features": [],
            })
        );
    }
}
