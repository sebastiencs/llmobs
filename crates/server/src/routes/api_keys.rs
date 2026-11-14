//! Create, list, and revoke project API keys (spec 08, D140).
#![cfg_attr(not(test), allow(dead_code))]

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::common::{FieldError, Page, PageParams};
use crate::api::keys::{ApiKey, ApiKeyKind, ApiKeyScope, CreateApiKeyRequest, CreatedApiKey};
use crate::auth::principal::{self, Principal};
use crate::auth::roles::{KeyScope, KeyScopes, ProjectPerm};
use crate::auth::secret::{self, KeyKind};
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::meta::api_keys::{self, ApiKeyRow, NewApiKey};
use crate::routes::time_cursor::{decode_time_cursor, encode_time_cursor};

/// Ten years of 365 days. An `expires_at` later than this is rejected.
const TEN_YEARS_NS: i64 = 10 * 365 * 24 * 60 * 60 * 1_000_000_000;

/// API key routes. All three are session-only.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/projects/{project}/api-keys",
            access: Access::Project {
                perm: ProjectPerm::ManageSettings,
                session_only: true,
            },
            documented: true,
            handler: get(list_api_keys),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/v1/projects/{project}/api-keys",
            access: Access::Project {
                perm: ProjectPerm::ManageSettings,
                session_only: true,
            },
            documented: true,
            handler: post(create_api_key),
        },
        RouteDef {
            method: axum::http::Method::DELETE,
            path: "/api/v1/projects/{project}/api-keys/{id}",
            access: Access::Project {
                perm: ProjectPerm::ManageSettings,
                session_only: true,
            },
            documented: true,
            handler: delete(revoke_api_key),
        },
    ]
}

#[derive(Debug, Deserialize)]
struct ApiKeyPath {
    id: Uuid,
}

async fn list_api_keys(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Query(params): error::Query<PageParams>,
) -> Result<error::Json<Page<ApiKey>>, ApiError> {
    let rows = api_keys::list_api_keys(state.meta.reader(), access.project.id).await?;
    Ok(error::Json(page_keys(
        rows,
        params.cursor.as_deref(),
        params.limit.get(),
    )?))
}

async fn create_api_key(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Json(body): error::Json<CreateApiKeyRequest>,
) -> Result<(StatusCode, error::Json<CreatedApiKey>), ApiError> {
    let user = principal::session_user(&principal)?;
    let name = optional_name(body.name)?;
    let (kind, scopes) = checked_scopes(body.kind, body.scopes)?;
    let now = state.clock.now_ns();
    let expires_at = checked_expiry(body.expires_at.map(|time| time.ns()), now)?;
    let minted = secret::new_key(kind).map_err(|error| ApiError::Internal(Box::new(error)))?;
    let project_id = access.project.id;
    let user_id = user.id;
    let prefix = minted.prefix.clone();
    let hash = minted.hash.clone();
    let row = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                api_keys::insert_api_key(
                    conn,
                    NewApiKey {
                        project_id,
                        kind: kind_name(kind),
                        scopes,
                        name,
                        prefix,
                        hash,
                        created_by: user_id,
                        expires_at,
                    },
                    now,
                )
                .await
            })
        })
        .await?;
    tracing::info!(
        target: "audit",
        key_id = %row.id,
        project_id = %project_id,
        user_id = %user_id,
        "api key created"
    );
    Ok((
        StatusCode::CREATED,
        error::Json(CreatedApiKey {
            key: api_key_json(&row)?,
            secret: minted.plaintext,
        }),
    ))
}

async fn revoke_api_key(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(ApiKeyPath { id }): error::Path<ApiKeyPath>,
) -> Result<StatusCode, ApiError> {
    let user = principal::session_user(&principal)?;
    let project_id = access.project.id;
    let user_id = user.id;
    let now = state.clock.now_ns();
    let hash = state
        .meta
        .write(move |conn| {
            Box::pin(async move { api_keys::revoke_api_key(conn, project_id, id, now).await })
        })
        .await?;
    let Some(hash) = hash else {
        return Err(ApiError::NotFound);
    };
    state.key_cache.evict(&hash);
    tracing::info!(
        target: "audit",
        key_id = %id,
        project_id = %project_id,
        user_id = %user_id,
        "api key revoked"
    );
    Ok(StatusCode::NO_CONTENT)
}

fn optional_name(name: Option<String>) -> Result<Option<String>, ApiError> {
    let Some(name) = name else {
        return Ok(None);
    };
    let name = name.trim();
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().count() > 100 {
        return Err(field_error("name", "must be at most 100 characters"));
    }
    Ok(Some(name.to_owned()))
}

fn checked_scopes(
    kind: ApiKeyKind,
    scopes: Option<Vec<ApiKeyScope>>,
) -> Result<(KeyKind, Vec<KeyScope>), ApiError> {
    match kind {
        ApiKeyKind::Secret => Ok((KeyKind::Secret, secret_scopes(scopes)?)),
        ApiKeyKind::Public => Ok((KeyKind::Public, public_scopes(scopes)?)),
    }
}

fn secret_scopes(scopes: Option<Vec<ApiKeyScope>>) -> Result<Vec<KeyScope>, ApiError> {
    let Some(scopes) = scopes else {
        return Ok(vec![KeyScope::Ingest, KeyScope::Read, KeyScope::Write]);
    };
    if scopes.is_empty() {
        return Err(field_error(
            "scopes",
            "must be a non-empty subset of ingest, read, and write",
        ));
    }
    let mut out = Vec::new();
    for scope in scopes {
        let scope = match scope {
            ApiKeyScope::Ingest => KeyScope::Ingest,
            ApiKeyScope::Read => KeyScope::Read,
            ApiKeyScope::Write => KeyScope::Write,
            ApiKeyScope::Feedback => {
                return Err(field_error(
                    "scopes",
                    "must be a non-empty subset of ingest, read, and write",
                ));
            }
        };
        if out.contains(&scope) {
            return Err(field_error("scopes", "duplicate scope"));
        }
        out.push(scope);
    }
    Ok(KeyScopes::from_scopes(&out).iter().collect())
}

fn public_scopes(scopes: Option<Vec<ApiKeyScope>>) -> Result<Vec<KeyScope>, ApiError> {
    match scopes {
        None => Ok(vec![KeyScope::Feedback]),
        Some(scopes) if scopes == [ApiKeyScope::Feedback] => Ok(vec![KeyScope::Feedback]),
        Some(_) => Err(field_error("scopes", "must be feedback")),
    }
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

fn kind_name(kind: KeyKind) -> &'static str {
    match kind {
        KeyKind::Secret => "secret",
        KeyKind::Public => "public",
        KeyKind::Personal => "personal",
    }
}

fn api_key_json(row: &ApiKeyRow) -> Result<ApiKey, ApiError> {
    let kind = match api_keys::stored_kind(&row.kind)? {
        KeyKind::Secret => ApiKeyKind::Secret,
        KeyKind::Public => ApiKeyKind::Public,
        KeyKind::Personal => {
            return Err(ApiError::Internal(Box::new(std::io::Error::other(
                "corrupt api_keys.kind",
            ))));
        }
    };
    Ok(ApiKey {
        id: row.id,
        kind,
        name: row.name.clone(),
        prefix: row.prefix.clone(),
        scopes: KeyScopes::from_scopes(&row.scopes)
            .iter()
            .map(api_scope)
            .collect(),
        created_by: row.created_by,
        created_at: row.created_at,
        expires_at: row.expires_at,
        last_used_at: row.last_used_at,
    })
}

fn api_scope(scope: KeyScope) -> ApiKeyScope {
    match scope {
        KeyScope::Ingest => ApiKeyScope::Ingest,
        KeyScope::Read => ApiKeyScope::Read,
        KeyScope::Write => ApiKeyScope::Write,
        KeyScope::Feedback => ApiKeyScope::Feedback,
    }
}

fn page_keys(
    rows: Vec<ApiKeyRow>,
    cursor: Option<&str>,
    limit: u32,
) -> Result<Page<ApiKey>, ApiError> {
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
        let last = rows
            .last()
            .ok_or_else(|| ApiError::Internal(Box::new(std::io::Error::other("empty key page"))))?;
        Some(encode_time_cursor(last.created_at, &last.id.to_string())?)
    } else {
        None
    };
    let mut data = Vec::with_capacity(rows.len());
    for row in &rows {
        data.push(api_key_json(row)?);
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

    use super::*;
    use crate::api::common::Page;
    use crate::auth::roles::OrgRole;
    use crate::auth::secret::hash_token;
    use crate::auth::session::start_session;
    use crate::http::access::{self, Access};
    use crate::meta::members::add_org_member;
    use crate::meta::orgs::insert_org;
    use crate::meta::projects::insert_project;
    use crate::meta::users::{NewUser, insert_user};
    use crate::test_support::{TestApp, capture_logs};

    struct Owner {
        cookie: String,
        project_id: String,
    }

    async fn owner(app: &TestApp) -> Owner {
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
                            is_instance_admin: true,
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
        Owner { cookie, project_id }
    }

    async fn create(app: &TestApp, owner: &Owner, body: Value) -> Value {
        let response = app
            .request(
                Method::POST,
                &format!("/api/v1/projects/{}/api-keys", owner.project_id),
            )
            .header(header::COOKIE.as_str(), &owner.cookie)
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
    async fn a_read_key_can_read_its_project_and_nothing_else() {
        let app = TestApp::new().await;
        let owner = owner(&app).await;
        let created = create(&app, &owner, json!({"kind": "secret", "scopes": ["read"]})).await;
        let secret = created["secret"].as_str().unwrap().to_owned();
        let auth = bearer(&secret);
        let current = app
            .request(Method::GET, "/api/v1/projects/current")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(current.status, 200);
        let project: Value = current.json();
        assert_eq!(project["id"], owner.project_id);
        assert!(project.get("role").is_none());
        let by_id = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}", owner.project_id),
            )
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(by_id.status, 200);

        let other = app
            .request(Method::POST, "/api/v1/orgs/acme/projects")
            .header(header::COOKIE.as_str(), &owner.cookie)
            .json(&json!({"name": "Other"}))
            .send()
            .await;
        assert_eq!(other.status, 201);
        let other: Value = other.json();
        let foreign = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}", other["id"].as_str().unwrap()),
            )
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(foreign.status, 404);
        let patch = app
            .request(
                Method::PATCH,
                &format!("/api/v1/projects/{}", owner.project_id),
            )
            .header(header::AUTHORIZATION.as_str(), &auth)
            .json(&json!({"name": "Nope"}))
            .send()
            .await;
        assert_eq!(patch.status, 403);
        let me = app
            .request(Method::GET, "/api/auth/me")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(me.status, 403);
        let orgs = app
            .request(Method::GET, "/api/v1/orgs")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(orgs.status, 403);
    }

    #[tokio::test]
    async fn an_ingest_key_and_a_public_key_cannot_read_the_project() {
        let app = TestApp::new().await;
        let owner = owner(&app).await;
        let ingest = create(
            &app,
            &owner,
            json!({"kind": "secret", "scopes": ["ingest"]}),
        )
        .await;
        let denied = app
            .request(Method::GET, "/api/v1/projects/current")
            .header(
                header::AUTHORIZATION.as_str(),
                &bearer(ingest["secret"].as_str().unwrap()),
            )
            .send()
            .await;
        assert_eq!(denied.status, 403);
        let public = create(&app, &owner, json!({"kind": "public"})).await;
        assert_eq!(public["key"]["scopes"], json!(["feedback"]));
        let auth = bearer(public["secret"].as_str().unwrap());
        let read = app
            .request(Method::GET, "/api/v1/projects/current")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(read.status, 403);
        let session_only = app
            .request(Method::POST, "/api/v1/projects/current/api-keys")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .json(&json!({"kind": "public"}))
            .send()
            .await;
        assert_eq!(session_only.status, 403);
    }

    #[tokio::test]
    async fn revoke_evicts_the_cache_and_a_database_revoke_waits_for_the_ttl() {
        let app = TestApp::new().await;
        let owner = owner(&app).await;
        let created = create(&app, &owner, json!({"kind": "secret", "scopes": ["read"]})).await;
        let secret = created["secret"].as_str().unwrap().to_owned();
        let id = created["key"]["id"].as_str().unwrap().to_owned();
        let auth = bearer(&secret);
        let ok = app
            .request(Method::GET, "/api/v1/projects/current")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(ok.status, 200);
        let revoked = app
            .request(
                Method::DELETE,
                &format!("/api/v1/projects/{}/api-keys/{id}", owner.project_id),
            )
            .header(header::COOKIE.as_str(), &owner.cookie)
            .send()
            .await;
        assert_eq!(revoked.status, 204);
        let again = app
            .request(Method::GET, "/api/v1/projects/current")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(again.status, 401);

        let cached = create(&app, &owner, json!({"kind": "secret", "scopes": ["read"]})).await;
        let cached_secret = cached["secret"].as_str().unwrap().to_owned();
        let cached_id = cached["key"]["id"].as_str().unwrap().to_owned();
        let cached_auth = bearer(&cached_secret);
        let warm = app
            .request(Method::GET, "/api/v1/projects/current")
            .header(header::AUTHORIZATION.as_str(), &cached_auth)
            .send()
            .await;
        assert_eq!(warm.status, 200);
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    sqlx::query("UPDATE api_keys SET revoked_at = ? WHERE id = ?")
                        .bind(now)
                        .bind(cached_id)
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        let still = app
            .request(Method::GET, "/api/v1/projects/current")
            .header(header::AUTHORIZATION.as_str(), &cached_auth)
            .send()
            .await;
        assert_eq!(still.status, 200);
        app.state.clock.advance(Duration::from_secs(60));
        let expired = app
            .request(Method::GET, "/api/v1/projects/current")
            .header(header::AUTHORIZATION.as_str(), &cached_auth)
            .send()
            .await;
        assert_eq!(expired.status, 401);
    }

    #[tokio::test]
    async fn an_expired_key_is_refused_while_its_cache_entry_is_fresh() {
        let app = TestApp::new().await;
        let owner = owner(&app).await;
        let expires_ms = app.state.clock.now_ns() / 1_000_000 + 2_000;
        let created = create(
            &app,
            &owner,
            json!({
                "kind": "secret",
                "scopes": ["read"],
                "expires_at": expires_ms.to_string(),
            }),
        )
        .await;
        let auth = bearer(created["secret"].as_str().unwrap());
        let ok = app
            .request(Method::GET, "/api/v1/projects/current")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(ok.status, 200);
        app.state.clock.advance(Duration::from_secs(2));
        let expired = app
            .request(Method::GET, "/api/v1/projects/current")
            .header(header::AUTHORIZATION.as_str(), &auth)
            .send()
            .await;
        assert_eq!(expired.status, 401);
    }

    #[tokio::test]
    async fn last_used_at_is_written_on_flush_and_the_secret_is_not_stored() {
        let app = TestApp::new().await;
        let owner = owner(&app).await;
        let (created, logs) = capture_logs(create(
            &app,
            &owner,
            json!({"kind": "secret", "name": "CI"}),
        ))
        .await;
        let secret = created["secret"].as_str().unwrap().to_owned();
        assert!(!logs.contains(&secret));
        assert_eq!(created["key"]["scopes"], json!(["ingest", "read", "write"]));
        assert_eq!(created["key"]["prefix"], secret::display_prefix(&secret));
        let id = created["key"]["id"].as_str().unwrap();
        let loaded = api_keys::api_key_by_id(
            app.state.meta.reader(),
            owner.project_id.parse().unwrap(),
            id.parse().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(loaded.hash, hash_token(&secret));
        let list = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}/api-keys", owner.project_id),
            )
            .header(header::COOKIE.as_str(), &owner.cookie)
            .send()
            .await;
        assert_eq!(list.status, 200);
        let text = String::from_utf8_lossy(&list.body);
        assert!(!text.contains(&secret));
        let page: Page<ApiKey> = list.json();
        assert!(page.data[0].last_used_at.is_none());
        let used = app
            .request(Method::GET, "/api/v1/projects/current")
            .header(header::AUTHORIZATION.as_str(), &bearer(&secret))
            .send()
            .await;
        assert_eq!(used.status, 200);
        let before = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}/api-keys", owner.project_id),
            )
            .header(header::COOKIE.as_str(), &owner.cookie)
            .send()
            .await;
        let before: Page<ApiKey> = before.json();
        assert!(before.data[0].last_used_at.is_none());
        app.state.last_used.flush(&app.state.meta).await.unwrap();
        let after = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}/api-keys", owner.project_id),
            )
            .header(header::COOKIE.as_str(), &owner.cookie)
            .send()
            .await;
        let after: Page<ApiKey> = after.json();
        assert!(after.data[0].last_used_at.is_some());
    }

    #[tokio::test]
    async fn api_key_routes_are_session_only() {
        let routes = access::routes();
        let keys: Vec<_> = routes
            .iter()
            .filter(|route| route.path.contains("api-keys"))
            .collect();
        assert_eq!(keys.len(), 3);
        for route in keys {
            assert!(matches!(
                route.access,
                Access::Project {
                    perm: ProjectPerm::ManageSettings,
                    session_only: true,
                }
            ));
        }
        let app = TestApp::new().await;
        let owner = owner(&app).await;
        let created = create(&app, &owner, json!({"kind": "secret", "scopes": ["read"]})).await;
        let denied = app
            .request(Method::POST, "/api/v1/projects/current/api-keys")
            .header(
                header::AUTHORIZATION.as_str(),
                &bearer(created["secret"].as_str().unwrap()),
            )
            .json(&json!({"kind": "secret"}))
            .send()
            .await;
        assert_eq!(denied.status, 403);
    }

    #[tokio::test]
    async fn a_malformed_bearer_is_refused_without_a_database_query() {
        let app = TestApp::new().await;
        app.state.meta.reader().close().await;
        let short = app
            .request(Method::GET, "/api/v1/projects/current")
            .header(header::AUTHORIZATION.as_str(), "Bearer nope")
            .send()
            .await;
        assert_eq!(short.status, 401);
        let bad = format!("Bearer sk-llmobs-{}", "!".repeat(32));
        let alphabet = app
            .request(Method::GET, "/api/v1/projects/current")
            .header(header::AUTHORIZATION.as_str(), &bad)
            .send()
            .await;
        assert_eq!(alphabet.status, 401);
        let personal = format!("Bearer pat-llmobs-{}", "A".repeat(31));
        let pat = app
            .request(Method::GET, "/api/v1/projects/current")
            .header(header::AUTHORIZATION.as_str(), &personal)
            .send()
            .await;
        assert_eq!(pat.status, 401);
    }

    #[tokio::test]
    async fn a_bad_cursor_is_a_field_error_and_does_not_echo_the_value() {
        let app = TestApp::new().await;
        let owner = owner(&app).await;
        let response = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}/api-keys?cursor=!!!", owner.project_id),
            )
            .header(header::COOKIE.as_str(), &owner.cookie)
            .send()
            .await;
        assert_eq!(response.status, 400);
        let text = String::from_utf8_lossy(&response.body);
        assert!(!text.contains("!!!"));
        let long = "n".repeat(101);
        let name = app
            .request(
                Method::POST,
                &format!("/api/v1/projects/{}/api-keys", owner.project_id),
            )
            .header(header::COOKIE.as_str(), &owner.cookie)
            .json(&json!({"kind": "secret", "name": long}))
            .send()
            .await;
        assert_eq!(name.status, 400);
        let past = app
            .request(
                Method::POST,
                &format!("/api/v1/projects/{}/api-keys", owner.project_id),
            )
            .header(header::COOKIE.as_str(), &owner.cookie)
            .json(&json!({"kind": "secret", "expires_at": "1"}))
            .send()
            .await;
        assert_eq!(past.status, 400);
        let text = String::from_utf8_lossy(&past.body);
        assert!(!text.contains("\"1\""));
    }
}
