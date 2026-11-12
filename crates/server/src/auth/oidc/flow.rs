//! Authorization-code login (D148).
//!
//! A known `(provider, subject)` signs that user in. An unknown identity links
//! onto an existing account only when the provider allows it. Domain rules run
//! before either lookup.

use std::io;

use uuid::Uuid;

use super::provider::{AutoProvision, ProvisionRole};
use super::state_cookie::{self, OAuthState};
use super::{ExchangeError, LoginClaims, ProviderRef};
use crate::api::account::SignupPolicy;
use crate::auth::OrgRole;
use crate::auth::secret::{constant_time_eq, new_opaque_token};
use crate::http::AppState;
use crate::meta::identities::{self, identity_user};
use crate::meta::users::{self, NewUser, normalize_email};
use crate::meta::{MetaError, invites, members, orgs, personal_tokens, web_sessions};

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
    /// The verified email or Google `hd` is outside `allowed_domains`.
    DomainNotAllowed,
    /// The sign-up policy does not allow a new user.
    SignupClosed,
    /// The identity's user is disabled.
    AccountDisabled,
    /// The email belongs to an existing account that this login cannot link.
    LinkRefused,
    /// The users table is empty and the setup token did not match.
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
            Self::DomainNotAllowed => "domain_not_allowed",
            Self::SignupClosed => "signup_closed",
            Self::AccountDisabled => "account_disabled",
            Self::LinkRefused => "link_refused",
            Self::SetupRequired => "setup_required",
            Self::Internal(_) => return None,
        })
    }
}

/// What this provider does with an email that is not yet an identity.
struct LoginRules {
    /// Empty means no domain check. Google also requires `hd`.
    domains: Vec<String>,
    /// `hd` is required when `domains` is non-empty.
    google: bool,
    /// Link a verified email onto a non-admin account.
    link_existing_by_email: bool,
    /// Counts as an invitation under `invite_only`, and joins the org after login.
    auto_provision: Option<AutoProvision>,
}

/// Builds the provider redirect and the state cookie.
///
/// `setup_token` is stored for the callback. It is used only when no user exists.
///
/// # Errors
///
/// Returns [`StartError::NotFound`] for an unknown provider and
/// [`StartError::Unavailable`] when metadata cannot be loaded.
pub(crate) async fn start(
    state: &AppState,
    provider_id: &str,
    redirect: Option<&str>,
    setup_token: Option<&str>,
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
    let setup_token = setup_token
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_owned);
    let cookie = state_cookie::seal(
        &state.oauth_key,
        &OAuthState {
            provider: provider_id.to_owned(),
            state: csrf,
            nonce,
            pkce_verifier: verifier,
            redirect: state_cookie::safe_redirect(redirect),
            setup_token,
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
    let user_id = resolve(state, provider_id, &claims, saved.setup_token.as_deref()).await?;
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
    setup_token: Option<&str>,
) -> Result<Uuid, LoginError> {
    let rules = rules(state, provider)?;
    check_domain(&rules, claims)?;
    if let Some(user_id) = identity_user(state.meta.reader(), provider, &claims.subject)
        .await
        .map_err(internal)?
    {
        let user_id = active(state, user_id).await?;
        provision_org(state, &rules, user_id).await?;
        return Ok(user_id);
    }
    let Some(email) = verified_email(claims) else {
        return Err(LoginError::EmailNotVerified);
    };
    let user_id = admit(
        state,
        &Admission {
            provider: provider.to_owned(),
            subject: claims.subject.clone(),
            email,
            name: display_name(claims.name.clone()),
            setup_ok: setup_matches(state, setup_token)?,
            rules: &rules,
        },
    )
    .await?;
    provision_org(state, &rules, user_id).await?;
    active(state, user_id).await
}

fn rules(state: &AppState, provider: &str) -> Result<LoginRules, LoginError> {
    match state.providers.get(provider) {
        Some(ProviderRef::Github(provider)) => Ok(LoginRules {
            domains: Vec::new(),
            google: false,
            link_existing_by_email: provider.link_existing_by_email(),
            auto_provision: None,
        }),
        Some(ProviderRef::Oidc(provider)) => Ok(LoginRules {
            domains: provider.allowed_domains().to_vec(),
            google: provider.is_google(),
            link_existing_by_email: provider.link_existing_by_email(),
            auto_provision: provider.auto_provision().cloned(),
        }),
        None => Err(LoginError::ProviderUnavailable),
    }
}

/// `allowed_domains` on a verified email, and Google's `hd`, before any lookup.
///
/// A provider with an empty list does not check. An unverified email is
/// [`LoginError::EmailNotVerified`]: the domain list applies to a verified address.
fn check_domain(rules: &LoginRules, claims: &LoginClaims) -> Result<(), LoginError> {
    if rules.domains.is_empty() {
        return Ok(());
    }
    let Some(email) = verified_email(claims) else {
        return Err(LoginError::EmailNotVerified);
    };
    if !domain_listed(&rules.domains, &email) {
        return Err(LoginError::DomainNotAllowed);
    }
    if rules.google {
        let hd = claims.hd.as_deref().map(str::trim).unwrap_or("");
        if hd.is_empty()
            || !rules
                .domains
                .iter()
                .any(|domain| domain == &hd.to_lowercase())
        {
            return Err(LoginError::DomainNotAllowed);
        }
    }
    Ok(())
}

fn verified_email(claims: &LoginClaims) -> Option<String> {
    if !claims.email_verified {
        return None;
    }
    let email = claims.email.as_deref()?;
    normalize_email(email).ok()
}

fn domain_listed(domains: &[String], email: &str) -> bool {
    let Some((_, domain)) = email.split_once('@') else {
        return false;
    };
    domains.iter().any(|item| item == domain)
}

fn setup_matches(state: &AppState, presented: Option<&str>) -> Result<bool, LoginError> {
    let Some(presented) = presented.map(str::trim).filter(|token| !token.is_empty()) else {
        return Ok(false);
    };
    match state.setup.token().map_err(internal)? {
        Some(expected) => Ok(constant_time_eq(&expected, presented)),
        None => Ok(false),
    }
}

/// 1 to 100 characters after trimming. Anything else is stored as no name.
fn display_name(value: Option<String>) -> Option<String> {
    let name = value.unwrap_or_default();
    let name = name.trim();
    let length = name.chars().count();
    (1..=100).contains(&length).then(|| name.to_owned())
}

struct Admission<'a> {
    provider: String,
    subject: String,
    email: String,
    name: Option<String>,
    setup_ok: bool,
    rules: &'a LoginRules,
}

enum Outcome {
    User(Uuid),
    Setup(Uuid),
    Linked { user_id: Uuid, revoked: Vec<String> },
    SetupRequired,
    SignupClosed,
    AccountDisabled,
    LinkRefused,
}

async fn admit(state: &AppState, login: &Admission<'_>) -> Result<Uuid, LoginError> {
    let provider = login.provider.clone();
    let subject = login.subject.clone();
    let email = login.email.clone();
    let name = login.name.clone();
    let setup_ok = login.setup_ok;
    let link = login.rules.link_existing_by_email;
    let invited = login.rules.auto_provision.is_some();
    let signup = state.config.auth.signup;
    let signup_domains = state.config.auth.signup_domains.clone();
    let now = state.clock.now_ns();
    let written = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                decide(
                    conn,
                    &provider,
                    &subject,
                    &email,
                    name,
                    setup_ok,
                    link,
                    invited,
                    signup,
                    &signup_domains,
                    now,
                )
                .await
            })
        })
        .await;
    match written {
        Ok(Outcome::User(id)) => Ok(id),
        Ok(Outcome::Setup(id)) => {
            state.setup.clear().map_err(internal)?;
            tracing::info!(target: "audit", user_id = %id, "setup completed");
            Ok(id)
        }
        Ok(Outcome::Linked { user_id, revoked }) => {
            for hash in revoked {
                state.pat_cache.evict(&hash);
            }
            tracing::info!(
                target: "audit",
                user_id = %user_id,
                provider = %login.provider,
                "identity linked"
            );
            Ok(user_id)
        }
        Ok(Outcome::SetupRequired) => Err(LoginError::SetupRequired),
        Ok(Outcome::SignupClosed) => Err(LoginError::SignupClosed),
        Ok(Outcome::AccountDisabled) => Err(LoginError::AccountDisabled),
        Ok(Outcome::LinkRefused) => Err(LoginError::LinkRefused),
        Err(MetaError::Conflict(_)) => {
            match identity_user(state.meta.reader(), &login.provider, &login.subject)
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

#[allow(clippy::too_many_arguments)]
async fn decide(
    conn: &mut sqlx::SqliteConnection,
    provider: &str,
    subject: &str,
    email: &str,
    name: Option<String>,
    setup_ok: bool,
    link: bool,
    invited: bool,
    signup: SignupPolicy,
    signup_domains: &[String],
    now: i64,
) -> Result<Outcome, MetaError> {
    if let Some(id) = identities::identity_user_tx(conn, provider, subject).await? {
        return Ok(Outcome::User(id));
    }
    if users::count_users_tx(conn).await? == 0 {
        if !setup_ok {
            return Ok(Outcome::SetupRequired);
        }
        let user = users::insert_user(
            conn,
            NewUser {
                email: email.to_owned(),
                name,
                password_hash: None,
                email_verified_at: Some(now),
                is_instance_admin: true,
            },
            now,
        )
        .await?;
        identities::insert_identity(conn, user.id, provider, subject, email, now).await?;
        return Ok(Outcome::Setup(user.id));
    }
    if let Some(user) = users::user_by_email_tx(conn, email).await? {
        if user.disabled_at.is_some() {
            return Ok(Outcome::AccountDisabled);
        }
        if link && !user.is_instance_admin {
            identities::insert_identity(conn, user.id, provider, subject, email, now).await?;
            web_sessions::delete_user_sessions(conn, user.id, None).await?;
            let revoked = personal_tokens::revoke_user_tokens(conn, user.id, now).await?;
            if user.email_verified_at.is_none() {
                users::set_password_hash(conn, user.id, None, now).await?;
                users::set_email_verified(conn, user.id, now).await?;
            }
            return Ok(Outcome::Linked {
                user_id: user.id,
                revoked,
            });
        }
        return Ok(Outcome::LinkRefused);
    }
    match signup {
        SignupPolicy::Disabled => return Ok(Outcome::SignupClosed),
        SignupPolicy::InviteOnly => {
            if !invited && !invites::has_pending_invite(conn, email, now).await? {
                return Ok(Outcome::SignupClosed);
            }
        }
        SignupPolicy::Open => {
            if !signup_domains.is_empty() && !domain_listed(signup_domains, email) {
                return Ok(Outcome::SignupClosed);
            }
        }
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
        Err(MetaError::Conflict(_)) => return Ok(Outcome::LinkRefused),
        Err(error) => return Err(error),
    };
    identities::insert_identity(conn, user.id, provider, subject, email, now).await?;
    Ok(Outcome::User(user.id))
}

async fn provision_org(
    state: &AppState,
    rules: &LoginRules,
    user_id: Uuid,
) -> Result<(), LoginError> {
    let Some(provision) = &rules.auto_provision else {
        return Ok(());
    };
    let slug = provision.org.clone();
    let role = match provision.role {
        ProvisionRole::Admin => OrgRole::Admin,
        ProvisionRole::Member => OrgRole::Member,
        ProvisionRole::Viewer => OrgRole::Viewer,
    };
    let Some(org) = orgs::org_by_slug(state.meta.reader(), &slug)
        .await
        .map_err(internal)?
    else {
        tracing::warn!(org = %slug, "auto-provision org is missing");
        return Ok(());
    };
    let org_id = org.id;
    let now = state.clock.now_ns();
    state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                if members::org_role_tx(conn, org_id, user_id).await?.is_some() {
                    return Ok(());
                }
                match members::add_org_member(conn, org_id, user_id, role, now).await {
                    Ok(()) | Err(MetaError::Conflict(_)) => Ok(()),
                    Err(error) => Err(error),
                }
            })
        })
        .await
        .map_err(internal)
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
