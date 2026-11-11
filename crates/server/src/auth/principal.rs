//! The caller of one request (D140).
//!
//! P16 adds API keys and P17 adds personal access tokens. Until then an
//! `Authorization` header is refused and the session cookie is ignored
//! whenever that header is present.
#![cfg_attr(not(test), allow(dead_code))]

use std::fmt;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use uuid::Uuid;

use crate::auth::session::resolve_cookie_header;
use crate::http::AppState;
use crate::http::error::ApiError;

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
}

impl fmt::Debug for Principal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Session { user, .. } => formatter
                .debug_struct("Session")
                .field("user", user)
                .finish_non_exhaustive(),
        }
    }
}

/// Reads credentials and stores `Option<Principal>` on the request.
///
/// A resolved session may add a sliding `Set-Cookie` on the way back.
pub(crate) async fn authenticate(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    if request
        .headers()
        .contains_key(axum::http::header::AUTHORIZATION)
    {
        return ApiError::Unauthorized.into_response();
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

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{AuthUser, Principal};

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
        let Principal::Session { user, session_hash } = &principal;
        assert_eq!(user.email, "ada@example.com");
        assert_eq!(session_hash, "not-a-cookie");
        assert!(!text.contains(&user.email), "{text}");
        assert!(!text.contains(session_hash), "{text}");
    }
}
