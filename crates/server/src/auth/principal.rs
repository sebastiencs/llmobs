//! The caller of one request (D140).
//!
//! A bearer token wins over a cookie. When both are present the cookie is
//! ignored and the session is not refreshed. A personal access token loads the
//! user on every request, so a disabled account fails at once (D149).
#![cfg_attr(not(test), allow(dead_code))]

use std::fmt;

use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use llmobs_core::ids::ProjectId;
use uuid::Uuid;

use crate::api::common::ProblemCode;
use crate::auth::cache::KeyCache;
use crate::auth::roles::{KeyScope, KeyScopes};
use crate::auth::secret::{self, KeyKind};
use crate::auth::session::resolve_cookie_header;
use crate::http::AppState;
use crate::http::error::ApiError;
use crate::meta::api_keys::{self, LoadedApiKey};
use crate::meta::personal_tokens::{self, TokenRow};
use crate::meta::users;

/// A signed-in user. `Debug` omits the email.
#[derive(Clone)]
pub(crate) struct AuthUser {
    /// `users.id`.
    pub(crate) id: Uuid,
    /// Instance admin.
    pub(crate) is_instance_admin: bool,
    /// Held for handlers that check the password. Never logged.
    pub(crate) email: String,
}

impl fmt::Debug for AuthUser {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthUser")
            .field("id", &self.id)
            .field("is_instance_admin", &self.is_instance_admin)
            .finish_non_exhaustive()
    }
}

/// Who is calling. The request extensions hold `Option<Principal>`.
#[derive(Clone)]
pub(crate) enum Principal {
    /// A web session.
    Session {
        /// The user.
        user: AuthUser,
        /// SHA-256 hex of the cookie. Not the cookie value.
        session_hash: String,
    },
    /// A project API key. `Debug` omits the hash.
    ApiKey {
        /// `api_keys.id`.
        key_id: Uuid,
        /// The key's project.
        project_id: ProjectId,
        /// `secret` or `public`.
        kind: KeyKind,
        /// Scopes stored on the key.
        scopes: KeyScopes,
    },
    /// A personal access token. `Debug` omits the email. `write` is the `write` scope.
    Pat {
        /// The token's user, loaded for this request.
        user: AuthUser,
        /// `personal_tokens.id`.
        token_id: Uuid,
        /// The token has the `write` scope.
        write: bool,
    },
}

/// A cached successful key lookup. `expires_at` is checked on every use.
#[derive(Clone, Copy)]
pub(crate) struct CachedKey {
    /// `api_keys.id`.
    pub(crate) id: Uuid,
    /// The key's project.
    pub(crate) project_id: ProjectId,
    /// `secret` or `public`.
    pub(crate) kind: KeyKind,
    /// Scopes stored on the key.
    pub(crate) scopes: KeyScopes,
    /// When the key stops working. Absent means it does not expire.
    pub(crate) expires_at: Option<i64>,
}

/// A cached successful personal-token lookup. The user is not cached.
#[derive(Clone, Copy)]
pub(crate) struct CachedPat {
    /// `personal_tokens.id`.
    pub(crate) id: Uuid,
    /// `personal_tokens.user_id`.
    pub(crate) user_id: Uuid,
    /// Scopes stored on the token.
    pub(crate) scopes: KeyScopes,
    /// When the token stops working. Absent means it does not expire.
    pub(crate) expires_at: Option<i64>,
}

impl fmt::Debug for Principal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session { user, .. } => formatter
                .debug_struct("Session")
                .field("user", user)
                .finish_non_exhaustive(),
            Self::ApiKey {
                key_id,
                project_id,
                kind,
                scopes,
            } => formatter
                .debug_struct("ApiKey")
                .field("key_id", key_id)
                .field("project_id", project_id)
                .field("kind", kind)
                .field("scopes", scopes)
                .finish(),
            Self::Pat {
                user,
                token_id,
                write,
            } => formatter
                .debug_struct("Pat")
                .field("user", user)
                .field("token_id", token_id)
                .field("write", write)
                .finish(),
        }
    }
}

/// The user behind a session or a personal access token. An API key is refused.
///
/// # Errors
///
/// Returns [`ApiError::Forbidden`] when `principal` is an API key.
pub(crate) fn session_user(principal: &Principal) -> Result<&AuthUser, ApiError> {
    match principal {
        Principal::Session { user, .. } | Principal::Pat { user, .. } => Ok(user),
        Principal::ApiKey { .. } => Err(ApiError::Forbidden {
            code: ProblemCode::Forbidden,
        }),
    }
}

/// Reads credentials and stores `Option<Principal>` on the request.
///
/// A resolved session may add a sliding `Set-Cookie` on the way back. A bearer
/// does not.
pub(crate) async fn authenticate(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    if request
        .headers()
        .contains_key(axum::http::header::AUTHORIZATION)
    {
        let principal = match bearer_principal(&state, request.headers()).await {
            Ok(principal) => principal,
            Err(error) => return error.into_response(),
        };
        request.extensions_mut().insert(Some(principal));
        return next.run(request).await;
    }
    let resolved =
        match resolve_cookie_header(&state, request.headers(), state.clock.now_ns()).await {
            Ok(resolved) => resolved,
            Err(error) => return error.into_response(),
        };
    let (principal, refresh) = match resolved {
        Some((row, refresh)) => (
            Some(Principal::Session {
                user: AuthUser {
                    id: row.user.id,
                    is_instance_admin: row.user.is_instance_admin,
                    email: row.user.email,
                },
                session_hash: row.hash,
            }),
            refresh,
        ),
        None => (None, None),
    };
    request.extensions_mut().insert(principal);
    let mut response = next.run(request).await;
    if let Some(cookie) = refresh {
        response
            .headers_mut()
            .append(axum::http::header::SET_COOKIE, cookie);
    }
    response
}

async fn bearer_principal(state: &AppState, headers: &HeaderMap) -> Result<Principal, ApiError> {
    let token = bearer_token(headers)?;
    let Some(kind) = secret::parse_key(token) else {
        return Err(ApiError::Unauthorized);
    };
    let now = state.clock.now_ns();
    let hash = secret::hash_token(token);
    if kind == KeyKind::Personal {
        return personal_principal(state, &hash, now).await;
    }
    if let Some(cached) = state.key_cache.get(&hash, now) {
        if expired(cached.expires_at, now) {
            state.key_cache.evict(&hash);
            return Err(ApiError::Unauthorized);
        }
        state.last_used.record(cached.id, now);
        return Ok(principal_from_cached(cached));
    }
    let loaded = api_keys::api_key_by_hash(state.meta.reader(), &hash).await?;
    let Some(loaded) = loaded else {
        return Err(ApiError::Unauthorized);
    };
    if loaded.key.revoked_at.is_some() || !loaded.active || expired(loaded.key.expires_at, now) {
        return Err(ApiError::Unauthorized);
    }
    let cached = cached_from(&loaded)?;
    state.key_cache.put(hash, cached, now);
    state.last_used.record(cached.id, now);
    Ok(principal_from_cached(cached))
}

fn principal_from_cached(cached: CachedKey) -> Principal {
    Principal::ApiKey {
        key_id: cached.id,
        project_id: cached.project_id,
        kind: cached.kind,
        scopes: cached.scopes,
    }
}

fn cached_from(loaded: &LoadedApiKey) -> Result<CachedKey, ApiError> {
    Ok(CachedKey {
        id: loaded.key.id,
        project_id: loaded.key.project_id,
        kind: api_keys::stored_kind(&loaded.key.kind)?,
        scopes: KeyScopes::from_scopes(&loaded.key.scopes),
        expires_at: loaded.key.expires_at,
    })
}

async fn personal_principal(state: &AppState, hash: &str, now: i64) -> Result<Principal, ApiError> {
    if let Some(cached) = state.pat_cache.get(hash, now) {
        if expired(cached.expires_at, now) {
            state.pat_cache.evict(hash);
            return Err(ApiError::Unauthorized);
        }
        return pat_principal(state, cached, now).await;
    }
    let Some(row) = personal_tokens::token_by_hash(state.meta.reader(), hash).await? else {
        return Err(ApiError::Unauthorized);
    };
    if row.revoked_at.is_some() || expired(row.expires_at, now) {
        return Err(ApiError::Unauthorized);
    }
    let cached = cached_pat(&row);
    state.pat_cache.put(hash.to_owned(), cached, now);
    pat_principal(state, cached, now).await
}

async fn pat_principal(
    state: &AppState,
    cached: CachedPat,
    now: i64,
) -> Result<Principal, ApiError> {
    let Some(row) = users::user_by_id(state.meta.reader(), cached.user_id).await? else {
        return Err(ApiError::Unauthorized);
    };
    if row.disabled_at.is_some() {
        return Err(ApiError::Unauthorized);
    }
    state.token_last_used.record(cached.id, now);
    Ok(Principal::Pat {
        user: AuthUser {
            id: row.id,
            is_instance_admin: row.is_instance_admin,
            email: row.email,
        },
        token_id: cached.id,
        write: cached.scopes.contains(KeyScope::Write),
    })
}

fn cached_pat(row: &TokenRow) -> CachedPat {
    CachedPat {
        id: row.id,
        user_id: row.user_id,
        scopes: KeyScopes::from_scopes(&row.scopes),
        expires_at: row.expires_at,
    }
}

fn expired(expires_at: Option<i64>, now: i64) -> bool {
    matches!(expires_at, Some(at) if at <= now)
}

fn bearer_token(headers: &HeaderMap) -> Result<&str, ApiError> {
    let Some(value) = headers.get(axum::http::header::AUTHORIZATION) else {
        return Err(ApiError::Unauthorized);
    };
    let Ok(text) = value.to_str() else {
        return Err(ApiError::Unauthorized);
    };
    let mut parts = text.splitn(2, char::is_whitespace);
    let scheme = parts.next().unwrap_or("");
    let token = parts.next().unwrap_or("").trim();
    if !scheme.eq_ignore_ascii_case("bearer")
        || token.is_empty()
        || token.contains(char::is_whitespace)
    {
        return Err(ApiError::Unauthorized);
    }
    Ok(token)
}

/// The cache used by [`authenticate`]. Declared here so `AppState` can name it.
pub(crate) type ApiKeyCache = KeyCache<CachedKey>;

/// The personal-token cache used by [`authenticate`].
pub(crate) type PatCache = KeyCache<CachedPat>;

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{AuthUser, Principal};
    use crate::auth::roles::{KeyScope, KeyScopes};
    use crate::auth::secret::KeyKind;
    use llmobs_core::ids::ProjectId;

    #[test]
    fn debug_omits_the_email_and_the_session_hash() {
        let principal = Principal::Session {
            user: AuthUser {
                id: Uuid::nil(),
                is_instance_admin: false,
                email: "ada@example.com".to_owned(),
            },
            session_hash: "not-a-cookie".to_owned(),
        };
        let text = format!("{principal:?}");
        let Principal::Session { user, session_hash } = &principal else {
            panic!("expected a session");
        };
        assert_eq!(user.email, "ada@example.com");
        assert_eq!(session_hash, "not-a-cookie");
        assert!(!text.contains(&user.email), "{text}");
        assert!(!text.contains(session_hash), "{text}");
    }

    #[test]
    fn debug_of_a_key_omits_the_secret() {
        let principal = Principal::ApiKey {
            key_id: Uuid::nil(),
            project_id: ProjectId::from_uuid(Uuid::nil()),
            kind: KeyKind::Secret,
            scopes: KeyScopes::from_scopes(&[KeyScope::Read]),
        };
        let text = format!("{principal:?}");
        assert!(!text.contains("sk-llmobs-"), "{text}");
    }
}
