//! Authorization-code login (D148).
//!
//! A known `(provider, subject)` signs that user in. An email that already
//! belongs to an account is refused. Linking, domain checks, and
//! `auto_provision` are not applied here.

use std::io;

use uuid::Uuid;

use super::state_cookie::{self, OAuthState};
use super::{ExchangeError, LoginClaims, ProviderRef};
use crate::api::account::SignupPolicy;
use crate::auth::secret::new_opaque_token;
use crate::http::AppState;
use crate::meta::identities::{self, identity_user};
use crate::meta::users::{self, NewUser, normalize_email};
use crate::meta::{MetaError, invites};

/// Why `GET /authorize` did not redirect to the provider.
pub(crate) enum StartError {
    /// No provider has this id.
    NotFound,
    /// Metadata could not be loaded. The cookie is not set.
    Unavailable,
    /// The random source or the cookie failed. A static message.
    Internal(Box<dyn std::error::Error + Send + Sync>),
}

/// A completed login. The route starts the session.
pub(crate) struct LoggedIn {
    /// The user the code belongs to.
    pub(crate) user_id: Uuid,
    /// Sanitized relative path from the state cookie.
    pub(crate) redirect: String,
}

/// Why the callback redirects to `/login`. [`LoginError::Internal`] is HTTP 500.
pub(crate) enum LoginError {
    /// The state cookie is missing, stale, or does not match.
    InvalidState,
    /// The provider refused the code or the token did not verify.
    ProviderError,
    /// The provider could not be reached.
    ProviderUnavailable,
    /// No verified email. An account is not created.
    EmailNotVerified,
    /// The sign-up policy does not allow a new user.
    SignupClosed,
    /// The identity's user is disabled.
    AccountDisabled,
    /// The email belongs to an existing account that has no matching identity.
    LinkRefused,
    /// The users table is empty. The first account is created through setup.
    SetupRequired,
    /// The database or the random source failed.
    Internal(Box<dyn std::error::Error + Send + Sync>),
}

impl LoginError {
    /// The `error` query value, when the failure is a redirect.
    pub(crate) fn code(&self) -> Option<&'static str> {
        Some(match self {
            Self::InvalidState => "invalid_state",
            Self::ProviderError => "provider_error",
            Self::ProviderUnavailable => "provider_unavailable",
            Self::EmailNotVerified => "email_not_verified",
            Self::SignupClosed => "signup_closed",
            Self::AccountDisabled => "account_disabled",
            Self::LinkRefused => "link_refused",
            Self::SetupRequired => "setup_required",
            Self::Internal(_) => return None,
        })
    }
}

/// Builds the provider redirect and the state cookie.
///
/// # Errors
///
/// Returns [`StartError::NotFound`] for an unknown provider and
/// [`StartError::Unavailable`] when metadata cannot be loaded.
pub(crate) async fn start(
    state: &AppState,
    provider_id: &str,
    redirect: Option<&str>,
) -> Result<(String, axum::http::HeaderValue), StartError> {
    let Some(provider) = state.providers.get(provider_id) else {
        return Err(StartError::NotFound);
    };
    let redirect_uri = state
        .config
        .public_url
        .join(&format!("/api/auth/callback/{provider_id}"));
    let csrf = new_opaque_token().map_err(internal_start)?.plaintext;
    let nonce = new_opaque_token().map_err(internal_start)?.plaintext;
    let now = state.clock.now_ns();
    let (location, verifier) = match provider {
        ProviderRef::Oidc(provider) => provider
            .authorization_url(&redirect_uri, &csrf, &nonce, now)
            .await
            .map_err(start_exchange)?,
        ProviderRef::Github(provider) => provider
            .authorization_url(&redirect_uri, &csrf)
            .map_err(start_exchange)?,
    };
    let cookie = state_cookie::seal(
        &state.oauth_key,
        &OAuthState {
            provider: provider_id.to_owned(),
            state: csrf,
            nonce,
            pkce_verifier: verifier,
            redirect: state_cookie::safe_redirect(redirect),
            created_ns: now,
        },
        state.config.public_url.is_https(),
    )
    .map_err(|()| {
        internal_start(io::Error::new(
            io::ErrorKind::InvalidData,
            "oauth state cookie",
        ))
    })?;
    Ok((location, cookie))
}

/// Validates the callback and resolves the user. The session is the route's.
///
/// An existing session cookie is ignored. `provider_error_param` is the
/// provider's `error` query, which is only read after the state cookie checks.
///
/// # Errors
///
/// Returns a [`LoginError`]. The redirect codes are a closed set.
pub(crate) async fn finish(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    provider_id: &str,
    query_state: Option<&str>,
    code: Option<&str>,
    provider_error_param: bool,
) -> Result<LoggedIn, LoginError> {
    let saved = state_cookie::open(
        &state.oauth_key,
        headers,
        provider_id,
        query_state.unwrap_or(""),
        state.clock.now_ns(),
    )
    .map_err(|()| LoginError::InvalidState)?;
    if provider_error_param {
        return Err(LoginError::ProviderError);
    }
    let Some(code) = code else {
        return Err(LoginError::ProviderError);
    };
    let claims = exchange(state, provider_id, &saved, code).await?;
    let user_id = resolve(state, provider_id, &claims).await?;
    Ok(LoggedIn {
        user_id,
        redirect: saved.redirect,
    })
}

async fn exchange(
    state: &AppState,
    provider_id: &str,
    saved: &OAuthState,
    code: &str,
) -> Result<LoginClaims, LoginError> {
    let Some(provider) = state.providers.get(provider_id) else {
        return Err(LoginError::ProviderUnavailable);
    };
    let redirect_uri = state
        .config
        .public_url
        .join(&format!("/api/auth/callback/{provider_id}"));
    let now = state.clock.now_ns();
    let claims = match provider {
        ProviderRef::Oidc(provider) => provider
            .exchange_code(&redirect_uri, code, &saved.pkce_verifier, &saved.nonce, now)
            .await
            .map_err(login_exchange)?,
        ProviderRef::Github(provider) => {
            let profile = provider
                .identity(&redirect_uri, code, &saved.pkce_verifier)
                .await
                .map_err(login_exchange)?;
            LoginClaims {
                subject: profile.subject,
                email_verified: profile.email.is_some(),
                email: profile.email,
                hd: None,
                name: profile.name,
            }
        }
    };
    if claims.subject.is_empty() {
        return Err(LoginError::ProviderError);
    }
    Ok(claims)
}

async fn resolve(
    state: &AppState,
    provider: &str,
    claims: &LoginClaims,
) -> Result<Uuid, LoginError> {
    if let Some(user_id) = identity_user(state.meta.reader(), provider, &claims.subject)
        .await
        .map_err(internal)?
    {
        return active(state, user_id).await;
    }
    let Some(email) = verified_email(claims) else {
        return Err(LoginError::EmailNotVerified);
    };
    if users::user_by_email(state.meta.reader(), &email)
        .await
        .map_err(internal)?
        .is_some()
    {
        return Err(LoginError::LinkRefused);
    }
    let user_id = create_user(
        state,
        provider,
        &claims.subject,
        &email,
        display_name(claims.name.clone()),
    )
    .await?;
    active(state, user_id).await
}

fn verified_email(claims: &LoginClaims) -> Option<String> {
    if !claims.email_verified {
        return None;
    }
    let email = claims.email.as_deref()?;
    normalize_email(email).ok()
}

/// 1 to 100 characters after trimming. Anything else is stored as no name.
fn display_name(value: Option<String>) -> Option<String> {
    let name = value.unwrap_or_default();
    let name = name.trim();
    let length = name.chars().count();
    (1..=100).contains(&length).then(|| name.to_owned())
}

async fn active(state: &AppState, user_id: Uuid) -> Result<Uuid, LoginError> {
    let user = users::user_by_id(state.meta.reader(), user_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            internal(MetaError::Corrupt {
                table: "users",
                column: "id",
            })
        })?;
    if user.disabled_at.is_some() {
        return Err(LoginError::AccountDisabled);
    }
    Ok(user_id)
}

async fn create_user(
    state: &AppState,
    provider: &str,
    subject: &str,
    email: &str,
    name: Option<String>,
) -> Result<Uuid, LoginError> {
    let provider_key = provider.to_owned();
    let subject_key = subject.to_owned();
    let provider = provider_key.clone();
    let subject = subject_key.clone();
    let email = email.to_owned();
    let now = state.clock.now_ns();
    let signup = state.config.auth.signup;
    let written = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                provision(conn, &provider, &subject, &email, name, signup, now).await
            })
        })
        .await;
    match written {
        Ok(Provision::User(id)) => Ok(id),
        Ok(Provision::SetupRequired) => Err(LoginError::SetupRequired),
        Ok(Provision::SignupClosed) => Err(LoginError::SignupClosed),
        Ok(Provision::LinkRefused) => Err(LoginError::LinkRefused),
        Err(MetaError::Conflict(_)) => {
            match identity_user(state.meta.reader(), &provider_key, &subject_key)
                .await
                .map_err(internal)?
            {
                Some(id) => Ok(id),
                None => Err(LoginError::LinkRefused),
            }
        }
        Err(error) => Err(internal(error)),
    }
}

enum Provision {
    User(Uuid),
    SetupRequired,
    SignupClosed,
    LinkRefused,
}

async fn provision(
    conn: &mut sqlx::SqliteConnection,
    provider: &str,
    subject: &str,
    email: &str,
    name: Option<String>,
    signup: SignupPolicy,
    now: i64,
) -> Result<Provision, MetaError> {
    if users::count_users_tx(conn).await? == 0 {
        return Ok(Provision::SetupRequired);
    }
    match signup {
        SignupPolicy::Disabled => return Ok(Provision::SignupClosed),
        SignupPolicy::InviteOnly => {
            if !invites::has_pending_invite(conn, email, now).await? {
                return Ok(Provision::SignupClosed);
            }
        }
        SignupPolicy::Open => {}
    }
    if let Some(id) = identities::identity_user_tx(conn, provider, subject).await? {
        return Ok(Provision::User(id));
    }
    let user = match users::insert_user(
        conn,
        NewUser {
            email: email.to_owned(),
            name,
            password_hash: None,
            email_verified_at: Some(now),
            is_instance_admin: false,
        },
        now,
    )
    .await
    {
        Ok(user) => user,
        Err(MetaError::Conflict(_)) => return Ok(Provision::LinkRefused),
        Err(error) => return Err(error),
    };
    identities::insert_identity(conn, user.id, provider, subject, email, now).await?;
    Ok(Provision::User(user.id))
}

fn start_exchange(error: ExchangeError) -> StartError {
    match error {
        ExchangeError::Unavailable => StartError::Unavailable,
        ExchangeError::Rejected => internal_start(io::Error::new(
            io::ErrorKind::InvalidData,
            "oauth authorization url",
        )),
    }
}

fn login_exchange(error: ExchangeError) -> LoginError {
    match error {
        ExchangeError::Unavailable => LoginError::ProviderUnavailable,
        ExchangeError::Rejected => LoginError::ProviderError,
    }
}

fn internal(error: impl std::error::Error + Send + Sync + 'static) -> LoginError {
    LoginError::Internal(Box::new(error))
}

fn internal_start(error: impl std::error::Error + Send + Sync + 'static) -> StartError {
    StartError::Internal(Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::display_name;

    #[test]
    fn name_is_trimmed_or_dropped() {
        assert_eq!(
            display_name(Some(" Grace ".to_owned())),
            Some("Grace".to_owned())
        );
        assert_eq!(display_name(Some("  ".to_owned())), None);
        assert_eq!(display_name(None), None);
        assert_eq!(display_name(Some("a".repeat(101))), None);
        assert_eq!(display_name(Some("a".repeat(100))), Some("a".repeat(100)));
    }
}
