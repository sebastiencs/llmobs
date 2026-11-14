//! Create, list, and revoke personal access tokens (spec 08, D140).
#![cfg_attr(not(test), allow(dead_code))]

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::common::{FieldError, Page, PageParams};
use crate::api::keys::{
    CreatePersonalTokenRequest, CreatedPersonalToken, PersonalToken, PersonalTokenScope,
};
use crate::auth::principal::{self, Principal};
use crate::auth::roles::KeyScope;
use crate::auth::secret::{self, KeyKind};
use crate::http::AppState;
use crate::http::access::{Access, RouteDef};
use crate::http::error::{self, ApiError};
use crate::meta::personal_tokens::{self, NewToken, TokenRow};
use crate::routes::time_cursor::{decode_time_cursor, encode_time_cursor};

/// Ten years of 365 days. An `expires_at` later than this is rejected.
const TEN_YEARS_NS: i64 = 10 * 365 * 24 * 60 * 60 * 1_000_000_000;

/// Personal access token routes.
///
/// Listing, creating, and revoking by id are session-only. `DELETE .../current`
/// is the exception a token uses to revoke itself (D140). axum 0.8's matcher
/// ranks the static `current` segment above `{id}`.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/me/tokens",
            access: Access::User { session_only: true },
            documented: true,
            handler: get(list_tokens),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/v1/me/tokens",
            access: Access::User { session_only: true },
            documented: true,
            handler: post(create_token),
        },
        RouteDef {
            method: axum::http::Method::DELETE,
            path: "/api/v1/me/tokens/current",
            access: Access::User {
                session_only: false,
            },
            documented: true,
            handler: delete(revoke_current),
        },
        RouteDef {
            method: axum::http::Method::DELETE,
            path: "/api/v1/me/tokens/{id}",
            access: Access::User { session_only: true },
            documented: true,
            handler: delete(revoke_token),
        },
    ]
}

#[derive(Debug, Deserialize)]
struct TokenPath {
    id: Uuid,
}

async fn list_tokens(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    error::Query(params): error::Query<PageParams>,
) -> Result<error::Json<Page<PersonalToken>>, ApiError> {
    let user = principal::session_user(&principal)?;
    let rows = personal_tokens::list_user_tokens(state.meta.reader(), user.id).await?;
    Ok(error::Json(page_tokens(
        rows,
        params.cursor.as_deref(),
        params.limit.get(),
    )?))
}

async fn create_token(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    error::Json(body): error::Json<CreatePersonalTokenRequest>,
) -> Result<(StatusCode, error::Json<CreatedPersonalToken>), ApiError> {
    let user = principal::session_user(&principal)?;
    let name = checked_name(&body.name)?;
    let scopes = token_scopes(&body.scopes)?;
    let now = state.clock.now_ns();
    let expires_at = checked_expiry(body.expires_at.map(|time| time.ns()), now)?;
    let minted =
        secret::new_key(KeyKind::Personal).map_err(|error| ApiError::Internal(Box::new(error)))?;
    let user_id = user.id;
    let hash = minted.hash.clone();
    let prefix = minted.prefix.clone();
    let row = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                personal_tokens::insert_token(
                    conn,
                    NewToken {
                        user_id,
                        name,
                        prefix,
                        hash,
                        scopes,
                        expires_at,
                    },
                    now,
                )
                .await
            })
        })
        .await?;
    let token_id = row.id;
    tracing::info!(target: "audit", %token_id, %user_id, "personal token created");
    Ok((
        StatusCode::CREATED,
        error::Json(CreatedPersonalToken {
            token: token_json(&row)?,
            secret: minted.plaintext,
        }),
    ))
}

async fn revoke_token(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    error::Path(path): error::Path<TokenPath>,
) -> Result<StatusCode, ApiError> {
    let user = principal::session_user(&principal)?;
    revoke_owned(&state, user.id, path.id).await
}

async fn revoke_current(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
) -> Result<StatusCode, ApiError> {
    let Principal::Pat { user, token_id, .. } = principal else {
        return Err(ApiError::NotFound);
    };
    revoke_owned(&state, user.id, token_id).await
}

async fn revoke_owned(
    state: &AppState,
    user_id: Uuid,
    token_id: Uuid,
) -> Result<StatusCode, ApiError> {
    let now = state.clock.now_ns();
    let hash = state
        .meta
        .write(move |conn| {
            Box::pin(
                async move { personal_tokens::revoke_token(conn, user_id, token_id, now).await },
            )
        })
        .await?;
    let Some(hash) = hash else {
        return Err(ApiError::NotFound);
    };
    state.pat_cache.evict(&hash);
    tracing::info!(target: "audit", %token_id, %user_id, "personal token revoked");
    Ok(StatusCode::NO_CONTENT)
}

fn checked_name(name: &str) -> Result<String, ApiError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(field_error("name", "must be 1 to 100 characters"));
    }
    Ok(name.to_owned())
}

fn token_scopes(scopes: &[PersonalTokenScope]) -> Result<Vec<KeyScope>, ApiError> {
    let mut read = false;
    let mut write = false;
    for scope in scopes {
        let seen = match scope {
            PersonalTokenScope::Read => &mut read,
            PersonalTokenScope::Write => &mut write,
        };
        if *seen {
            return Err(field_error("scopes", "duplicate scope"));
        }
        *seen = true;
    }
    if !read {
        return Err(field_error("scopes", "must be read or read, write"));
    }
    let mut out = vec![KeyScope::Read];
    if write {
        out.push(KeyScope::Write);
    }
    Ok(out)
}

fn checked_expiry(expires_at: Option<i64>, now: i64) -> Result<Option<i64>, ApiError> {
    let Some(expires_at) = expires_at else {
        return Ok(None);
    };
    let limit = now.saturating_add(TEN_YEARS_NS);
    if expires_at <= now || expires_at > limit {
        return Err(field_error(
            "expires_at",
            "must be in the future and at most 10 years ahead",
        ));
    }
    Ok(Some(expires_at))
}

fn token_json(row: &TokenRow) -> Result<PersonalToken, ApiError> {
    let mut scopes = Vec::with_capacity(row.scopes.len());
    for scope in &row.scopes {
        scopes.push(match scope {
            KeyScope::Read => PersonalTokenScope::Read,
            KeyScope::Write => PersonalTokenScope::Write,
            KeyScope::Ingest | KeyScope::Feedback => {
                return Err(ApiError::Internal(Box::new(std::io::Error::other(
                    "corrupt personal_tokens.scopes",
                ))));
            }
        });
    }
    Ok(PersonalToken {
        id: row.id,
        name: row.name.clone(),
        prefix: row.prefix.clone(),
        scopes,
        created_at: row.created_at,
        expires_at: row.expires_at,
        last_used_at: row.last_used_at,
    })
}

fn page_tokens(
    rows: Vec<TokenRow>,
    cursor: Option<&str>,
    limit: u32,
) -> Result<Page<PersonalToken>, ApiError> {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut rows = match cursor {
        None => rows,
        Some(cursor) => {
            let (created_at, id) = decode_time_cursor(cursor)?;
            let id = id.to_string();
            rows.into_iter()
                .filter(|row| {
                    let row_id = row.id.to_string();
                    (row.created_at, row_id.as_str()) < (created_at, id.as_str())
                })
                .collect()
        }
    };
    let more = rows.len() > limit;
    rows.truncate(limit);
    let next_cursor = if more {
        let last = rows.last().ok_or_else(|| {
            ApiError::Internal(Box::new(std::io::Error::other("empty token page")))
        })?;
        Some(encode_time_cursor(last.created_at, &last.id.to_string())?)
    } else {
        None
    };
    let mut data = Vec::with_capacity(rows.len());
    for row in &rows {
        data.push(token_json(row)?);
    }
    Ok(Page { data, next_cursor })
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
    use std::time::Duration;

    use axum::http::{Method, header};
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::*;
    use crate::auth::roles::OrgRole;
    use crate::auth::secret::hash_token;
    use crate::auth::session::start_session;
    use crate::http::access::Access;
    use crate::meta::members::{add_org_member, remove_org_member};
    use crate::meta::orgs::insert_org;
    use crate::meta::projects::insert_project;
    use crate::meta::users::{self, NewUser, insert_user};
    use crate::test_support::{TestApp, capture_logs};

    struct Account {
        cookie: String,
        user_id: Uuid,
        project_id: String,
    }

    async fn account(app: &TestApp) -> Account {
        let now = app.state.clock.now_ns();
        let (user_id, project_id) = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    let user = insert_user(
                        conn,
                        NewUser {
                            email: "ada@example.com".to_owned(),
                            name: None,
                            password_hash: None,
                            email_verified_at: None,
                            is_instance_admin: false,
                        },
                        now,
                    )
                    .await?;
                    let org = insert_org(conn, "Acme", "acme", now).await?;
                    add_org_member(conn, org.id, user.id, OrgRole::Owner, now).await?;
                    let project =
                        insert_project(conn, org.id, "Support", "support-bot", None, now).await?;
                    Ok((user.id, project.id.to_string()))
                })
            })
            .await
            .unwrap();
        let header = start_session(&app.state, user_id, None, None)
            .await
            .unwrap();
        let cookie = header
            .to_str()
            .unwrap()
            .split_once(';')
            .unwrap()
            .0
            .to_owned();
        Account {
            cookie,
            user_id,
            project_id,
        }
    }

    async fn create(app: &TestApp, account: &Account, body: Value) -> Value {
        let response = app
            .request(Method::POST, "/api/v1/me/tokens")
            .header(header::COOKIE.as_str(), &account.cookie)
            .json(&body)
            .send()
            .await;
        assert_eq!(
            response.status,
            201,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        response.json()
    }

    fn bearer(secret: &str) -> String {
        format!("Bearer {secret}")
    }

    #[tokio::test]
    async fn an_unknown_token_is_unauthorized() {
        let app = TestApp::new().await;
        let auth = format!("Bearer pat-llmobs-{}", "A".repeat(32));
        let response = app
            .request(Method::GET, "/api/auth/me")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(response.status, 401);
    }

    #[test]
    fn token_routes_are_session_only_except_current() {
        let routes = routes();
        assert!(routes.iter().any(|route| {
            route.path == "/api/v1/me/tokens"
                && matches!(route.access, Access::User { session_only: true })
        }));
        assert!(routes.iter().any(|route| {
            route.path == "/api/v1/me/tokens/current"
                && matches!(
                    route.access,
                    Access::User {
                        session_only: false
                    }
                )
        }));
    }

    #[tokio::test]
    async fn a_read_token_can_read_and_cannot_write() {
        let app = TestApp::new().await;
        let account = account(&app).await;
        let created = create(&app, &account, json!({"name": "read", "scopes": ["read"]})).await;
        let secret = created["secret"].as_str().unwrap().to_owned();
        let auth = bearer(&secret);
        let orgs = app
            .request(Method::GET, "/api/v1/orgs")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(orgs.status, 200);
        let org = app
            .request(Method::GET, "/api/v1/orgs/acme")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(org.status, 200);
        let me = app
            .request(Method::GET, "/api/auth/me")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(me.status, 200);
        let create_org = app
            .request(Method::POST, "/api/v1/orgs")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .json(&json!({"name": "Other"}))
            .send()
            .await;
        assert_eq!(create_org.status, 403);
        let patch = app
            .request(Method::PATCH, "/api/v1/orgs/acme")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .json(&json!({"name": "Renamed"}))
            .send()
            .await;
        assert_eq!(patch.status, 403);
    }

    #[tokio::test]
    async fn a_write_token_can_create_an_org_and_cannot_touch_session_routes() {
        let app = TestApp::new().await;
        let account = account(&app).await;
        let created = create(
            &app,
            &account,
            json!({"name": "write", "scopes": ["read", "write"]}),
        )
        .await;
        let secret = created["secret"].as_str().unwrap().to_owned();
        let auth = bearer(&secret);
        let created_org = app
            .request(Method::POST, "/api/v1/orgs")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .json(&json!({"name": "Other"}))
            .send()
            .await;
        assert_eq!(created_org.status, 201);
        let tokens = app
            .request(Method::POST, "/api/v1/me/tokens")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .json(&json!({"name": "again", "scopes": ["read"]}))
            .send()
            .await;
        assert_eq!(tokens.status, 403);
        let keys = app
            .request(
                Method::POST,
                &format!("/api/v1/projects/{}/api-keys", account.project_id),
            )
            .header(header::AUTHORIZATION.as_str(), &auth)
            .json(&json!({"kind": "secret", "scopes": ["read"]}))
            .send()
            .await;
        assert_eq!(keys.status, 403);
        let members = app
            .request(
                Method::PATCH,
                &format!("/api/v1/orgs/acme/members/{}", account.user_id),
            )
            .header(header::AUTHORIZATION.as_str(), &auth)
            .json(&json!({"role": "member"}))
            .send()
            .await;
        assert_eq!(members.status, 403);
    }

    #[tokio::test]
    async fn a_token_wins_over_a_session_cookie() {
        let app = TestApp::new().await;
        let account = account(&app).await;
        let created = create(
            &app,
            &account,
            json!({"name": "both", "scopes": ["read", "write"]}),
        )
        .await;
        let auth = bearer(created["secret"].as_str().unwrap());
        let response = app
            .request(Method::POST, "/api/v1/me/tokens")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .header(header::COOKIE.as_str(), &account.cookie)
            .json(&json!({"name": "again", "scopes": ["read"]}))
            .send()
            .await;
        assert_eq!(response.status, 403);
    }

    #[tokio::test]
    async fn current_revokes_the_token_and_a_session_gets_404() {
        let app = TestApp::new().await;
        let account = account(&app).await;
        let created = create(
            &app,
            &account,
            json!({"name": "logout", "scopes": ["read", "write"]}),
        )
        .await;
        let secret = created["secret"].as_str().unwrap().to_owned();
        let auth = bearer(&secret);
        let session = app
            .request(Method::DELETE, "/api/v1/me/tokens/current")
            .header(header::COOKIE.as_str(), &account.cookie)
            .send()
            .await;
        assert_eq!(session.status, 404);
        let revoked = app
            .request(Method::DELETE, "/api/v1/me/tokens/current")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(revoked.status, 204);
        let again = app
            .request(Method::GET, "/api/auth/me")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(again.status, 401);
    }

    #[tokio::test]
    async fn disabling_the_user_rejects_a_cached_token() {
        let app = TestApp::new().await;
        let account = account(&app).await;
        let created = create(&app, &account, json!({"name": "live", "scopes": ["read"]})).await;
        let auth = bearer(created["secret"].as_str().unwrap());
        let ok = app
            .request(Method::GET, "/api/v1/orgs")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(ok.status, 200);
        let user_id = account.user_id;
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move { users::set_disabled(conn, user_id, Some(now), now).await })
            })
            .await
            .unwrap();
        let denied = app
            .request(Method::GET, "/api/v1/orgs")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(denied.status, 401);
    }

    #[tokio::test]
    async fn leaving_the_org_is_a_404_at_once() {
        let app = TestApp::new().await;
        let account = account(&app).await;
        let created = create(
            &app,
            &account,
            json!({"name": "member", "scopes": ["read"]}),
        )
        .await;
        let auth = bearer(created["secret"].as_str().unwrap());
        let ok = app
            .request(Method::GET, "/api/v1/orgs/acme")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(ok.status, 200);
        let user_id = account.user_id;
        let org = crate::meta::orgs::org_by_slug(app.state.meta.reader(), "acme")
            .await
            .unwrap()
            .unwrap();
        let org_id = org.id;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move { remove_org_member(conn, org_id, user_id).await })
            })
            .await
            .unwrap();
        let gone = app
            .request(Method::GET, "/api/v1/orgs/acme")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(gone.status, 404);
    }

    #[tokio::test]
    async fn changing_the_password_revokes_tokens() {
        let app = TestApp::new().await;
        let account = account(&app).await;
        let created = create(&app, &account, json!({"name": "old", "scopes": ["read"]})).await;
        let auth = bearer(created["secret"].as_str().unwrap());
        let ok = app
            .request(Method::GET, "/api/auth/me")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(ok.status, 200);
        let changed = app
            .request(Method::POST, "/api/v1/me/password")
            .header(header::COOKIE.as_str(), &account.cookie)
            .json(&json!({"new_password": "replacement-password-1"}))
            .send()
            .await;
        assert_eq!(
            changed.status,
            204,
            "{}",
            String::from_utf8_lossy(&changed.body)
        );
        let denied = app
            .request(Method::GET, "/api/auth/me")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(denied.status, 401);
    }

    #[tokio::test]
    async fn create_lists_the_hash_and_not_the_secret() {
        let app = TestApp::new().await;
        let account = account(&app).await;
        let (created, logs) = capture_logs(create(
            &app,
            &account,
            json!({"name": " CLI ", "scopes": ["write", "read"]}),
        ))
        .await;
        let secret = created["secret"].as_str().unwrap().to_owned();
        assert!(!logs.contains(&secret), "{logs}");
        assert_eq!(created["token"]["name"], "CLI");
        assert_eq!(created["token"]["scopes"], json!(["read", "write"]));
        assert!(!created["token"].get("secret").is_some());
        let listed = app
            .request(Method::GET, "/api/v1/me/tokens")
            .header(header::COOKIE.as_str(), &account.cookie)
            .send()
            .await;
        assert_eq!(listed.status, 200);
        let page: Value = listed.json();
        let text = page.to_string();
        assert!(!text.contains(&secret));
        let hash = hash_token(&secret);
        let stored: String =
            sqlx::query_scalar("SELECT token_hash FROM personal_tokens WHERE id = ?")
                .bind(created["token"]["id"].as_str().unwrap())
                .fetch_one(app.state.meta.reader())
                .await
                .unwrap();
        assert_eq!(stored, hash);
    }

    #[tokio::test]
    async fn an_expired_cached_token_is_rejected() {
        let app = TestApp::new().await;
        let account = account(&app).await;
        let expires_ms = (app.state.clock.now_ns() / 1_000_000) + 2_000;
        let created = create(
            &app,
            &account,
            json!({"name": "soon", "scopes": ["read"], "expires_at": expires_ms.to_string()}),
        )
        .await;
        let auth = bearer(created["secret"].as_str().unwrap());
        let ok = app
            .request(Method::GET, "/api/v1/orgs")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(ok.status, 200);
        app.state.clock.advance(Duration::from_secs(2));
        let denied = app
            .request(Method::GET, "/api/v1/orgs")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(denied.status, 401);
    }

    #[tokio::test]
    async fn last_used_is_written_on_flush() {
        let app = TestApp::new().await;
        let account = account(&app).await;
        let created = create(&app, &account, json!({"name": "used", "scopes": ["read"]})).await;
        let auth = bearer(created["secret"].as_str().unwrap());
        let ok = app
            .request(Method::GET, "/api/auth/me")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(ok.status, 200);
        let before = app
            .request(Method::GET, "/api/v1/me/tokens")
            .header(header::COOKIE.as_str(), &account.cookie)
            .send()
            .await;
        let before: Value = before.json();
        assert!(before["data"][0].get("last_used_at").is_none());
        app.state
            .token_last_used
            .flush(&app.state.meta)
            .await
            .unwrap();
        let after = app
            .request(Method::GET, "/api/v1/me/tokens")
            .header(header::COOKIE.as_str(), &account.cookie)
            .send()
            .await;
        let after: Value = after.json();
        assert!(after["data"][0].get("last_used_at").is_some());
    }

    #[tokio::test]
    async fn bad_input_does_not_echo_the_value() {
        let app = TestApp::new().await;
        let account = account(&app).await;
        let cursor = app
            .request(Method::GET, "/api/v1/me/tokens?cursor=!!!")
            .header(header::COOKIE.as_str(), &account.cookie)
            .send()
            .await;
        assert_eq!(cursor.status, 400);
        let text = String::from_utf8_lossy(&cursor.body);
        assert!(!text.contains("!!!"));
        let long = "n".repeat(101);
        let name = app
            .request(Method::POST, "/api/v1/me/tokens")
            .header(header::COOKIE.as_str(), &account.cookie)
            .json(&json!({"name": long, "scopes": ["read"]}))
            .send()
            .await;
        assert_eq!(name.status, 400);
        let scopes = app
            .request(Method::POST, "/api/v1/me/tokens")
            .header(header::COOKIE.as_str(), &account.cookie)
            .json(&json!({"name": "nope", "scopes": ["write"]}))
            .send()
            .await;
        assert_eq!(scopes.status, 400);
        let past = app
            .request(Method::POST, "/api/v1/me/tokens")
            .header(header::COOKIE.as_str(), &account.cookie)
            .json(&json!({"name": "nope", "scopes": ["read"], "expires_at": "1"}))
            .send()
            .await;
        assert_eq!(past.status, 400);
        let text = String::from_utf8_lossy(&past.body);
        assert!(!text.contains("\"1\""));
    }
}
