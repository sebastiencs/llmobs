//! Signed `state` cookie for the authorization-code flow (D148).
//!
//! The value is base64url JSON plus an HMAC-SHA256. It is not encrypted: the
//! redirect is visible to the browser. The signature is the CSRF defence.
//! Both routes are GET, so the origin check does not apply.

use std::fmt;

use axum::http::{HeaderMap, HeaderValue, header};
use axum_extra::extract::cookie::Cookie;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::auth::AuthError;
use crate::auth::secret::constant_time_eq;

/// `llmobs_oauth`.
pub(crate) const OAUTH_COOKIE: &str = llmobs_core::llmobs_name!("_oauth");

/// Cookie lifetime. Ten minutes.
const MAX_AGE_SECS: u64 = 600;

/// [`MAX_AGE_SECS`] in nanoseconds. Equal to this age is still valid.
pub(crate) const MAX_AGE_NS: i64 = 600 * 1_000_000_000;

const COOKIE_PATH: &str = "/api/auth/callback";

type StateMac = Hmac<Sha256>;

/// What the callback must see again. `pkce_verifier` is a secret.
#[derive(Serialize, Deserialize)]
pub(crate) struct OAuthState {
    /// `github`, `google`, or `oidc:<name>`.
    pub(crate) provider: String,
    /// Opaque `state` query value.
    pub(crate) state: String,
    /// OIDC nonce. GitHub stores one and does not send it.
    pub(crate) nonce: String,
    /// PKCE S256 verifier.
    pub(crate) pkce_verifier: String,
    /// Relative path to open after login. Already sanitized.
    pub(crate) redirect: String,
    /// First-run setup token from the authorize query. Absent otherwise.
    ///
    /// A secret. [`Debug`] omits it.
    pub(crate) setup_token: Option<String>,
    /// `Clock::now_ns` when the cookie was issued.
    pub(crate) created_ns: i64,
}

impl fmt::Debug for OAuthState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OAuthState")
            .field("provider", &self.provider)
            .field("redirect", &self.redirect)
            .field("created_ns", &self.created_ns)
            .finish_non_exhaustive()
    }
}

/// A fresh 32-byte key. A restart invalidates in-flight logins.
///
/// # Errors
///
/// Returns [`AuthError::Rng`] when the OS random source fails.
pub(crate) fn random_key() -> Result<[u8; 32], AuthError> {
    let bytes = crate::auth::secret::random_bytes(32)?;
    bytes.try_into().map_err(|_bytes: Vec<u8>| AuthError::Rng)
}

/// `Set-Cookie` for a new login. `Path=/api/auth/callback` is sent on
/// `/api/auth/callback/{provider}` (RFC 6265).
///
/// # Errors
///
/// Returns an error when the payload cannot be signed.
pub(crate) fn seal(key: &[u8; 32], state: &OAuthState, https: bool) -> Result<HeaderValue, ()> {
    let payload = serde_json::to_vec(state).map_err(|_| ())?;
    let body = URL_SAFE_NO_PAD.encode(payload);
    let signature = sign(key, &body)?;
    let value = format!("{body}.{}", URL_SAFE_NO_PAD.encode(signature));
    Ok(header_value(&value, MAX_AGE_SECS, https))
}

/// Checks the signature, the provider, the age, and `state`.
///
/// Every failure is the same result. The caller redirects `invalid_state`.
pub(crate) fn open(
    key: &[u8; 32],
    headers: &HeaderMap,
    provider: &str,
    query_state: &str,
    now: i64,
) -> Result<OAuthState, ()> {
    let Some(value) = cookie_value(headers) else {
        return Err(());
    };
    let Some((body, signature)) = value.split_once('.') else {
        return Err(());
    };
    let Ok(signature) = URL_SAFE_NO_PAD.decode(signature) else {
        return Err(());
    };
    if !verify(key, body, &signature) {
        return Err(());
    }
    let Ok(payload) = URL_SAFE_NO_PAD.decode(body) else {
        return Err(());
    };
    let Ok(state) = serde_json::from_slice::<OAuthState>(&payload) else {
        return Err(());
    };
    if state.provider != provider {
        return Err(());
    }
    let age = now.saturating_sub(state.created_ns);
    if state.created_ns > now || age > MAX_AGE_NS {
        return Err(());
    }
    if !constant_time_eq(&state.state, query_state) {
        return Err(());
    }
    Ok(state)
}

/// Clears the login cookie, including on failure.
pub(crate) fn clear(https: bool) -> HeaderValue {
    header_value("", 0, https)
}

/// Keeps `redirect` only when it is a same-origin path.
///
/// It must start with `/`, must not start with `//` or `/\`, must be at most
/// 1024 bytes, and must not contain an ASCII control character. Anything else,
/// including a missing value, becomes `/`.
pub(crate) fn safe_redirect(value: Option<&str>) -> String {
    let Some(value) = value else {
        return "/".to_owned();
    };
    if value.len() > 1024
        || !value.starts_with('/')
        || value.starts_with("//")
        || value.starts_with("/\\")
        || value.chars().any(|character| character.is_ascii_control())
    {
        return "/".to_owned();
    }
    value.to_owned()
}

fn sign(key: &[u8; 32], body: &str) -> Result<Vec<u8>, ()> {
    let mut mac = StateMac::new_from_slice(key).map_err(|_| ())?;
    mac.update(body.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}

fn verify(key: &[u8; 32], body: &str, signature: &[u8]) -> bool {
    let Ok(mut mac) = StateMac::new_from_slice(key) else {
        return false;
    };
    mac.update(body.as_bytes());
    mac.verify_slice(signature).is_ok()
}

fn cookie_value(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|part| {
            let cookie = Cookie::parse_encoded(part.trim().to_owned()).ok()?;
            (cookie.name() == OAUTH_COOKIE).then(|| cookie.value().to_owned())
        })
}

fn header_value(value: &str, max_age: u64, https: bool) -> HeaderValue {
    let mut text = format!(
        "{OAUTH_COOKIE}={value}; HttpOnly; SameSite=Lax; Path={COOKIE_PATH}; Max-Age={max_age}"
    );
    if https {
        text.push_str("; Secure");
    }
    // The name, the base64url payload, and the attributes are ASCII.
    #[allow(clippy::expect_used)]
    HeaderValue::from_str(&text).expect("oauth cookie is ascii")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; 32] {
        [7; 32]
    }

    fn state(provider: &str, redirect: &str, created_ns: i64) -> OAuthState {
        OAuthState {
            provider: provider.to_owned(),
            state: "state-token".to_owned(),
            nonce: "nonce-token".to_owned(),
            pkce_verifier: "verifier-secret".to_owned(),
            redirect: redirect.to_owned(),
            setup_token: None,
            created_ns,
        }
    }

    fn headers(set_cookie: &HeaderValue) -> HeaderMap {
        let text = set_cookie.to_str().expect("cookie");
        let pair = text.split_once(';').expect("attributes").0;
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_str(pair).expect("pair"));
        headers
    }

    #[test]
    fn round_trip_checks_provider_state_and_age() {
        let sealed = seal(&key(), &state("github", "/traces", 1_000), false).expect("seal");
        let text = sealed.to_str().expect("text");
        assert!(text.contains("HttpOnly"));
        assert!(text.contains("SameSite=Lax"));
        assert!(text.contains("Path=/api/auth/callback"));
        assert!(text.contains("Max-Age=600"));
        assert!(!text.contains("Secure"));
        assert!(!format!("{sealed:?}").contains("verifier-secret"));

        let headers = headers(&sealed);
        let opened = open(
            &key(),
            &headers,
            "github",
            "state-token",
            1_000 + MAX_AGE_NS,
        )
        .expect("open");
        assert_eq!(opened.redirect, "/traces");
        assert_eq!(opened.pkce_verifier, "verifier-secret");
        assert_eq!(opened.nonce, "nonce-token");

        assert!(open(&key(), &headers, "google", "state-token", 1_000).is_err());
        assert!(open(&key(), &headers, "github", "other", 1_000).is_err());
        assert!(
            open(
                &key(),
                &headers,
                "github",
                "state-token",
                1_000 + MAX_AGE_NS + 1
            )
            .is_err()
        );
        assert!(open(&key(), &headers, "github", "state-token", 999).is_err());
        assert!(open(&key(), &HeaderMap::new(), "github", "state-token", 1_000).is_err());

        let mut tampered = sealed.to_str().expect("text").to_owned();
        let end = tampered.find(';').expect("attributes");
        let byte = tampered.as_bytes()[end - 1];
        tampered.replace_range(end - 1..end, if byte == b'A' { "B" } else { "A" });
        let mut bad = HeaderMap::new();
        bad.insert(
            header::COOKIE,
            HeaderValue::from_str(tampered.split_once(';').expect("pair").0).expect("pair"),
        );
        assert!(open(&key(), &bad, "github", "state-token", 1_000).is_err());

        let secure = seal(&key(), &state("github", "/", 0), true).expect("seal");
        assert!(secure.to_str().expect("text").contains("; Secure"));
        let cleared = clear(true);
        let cleared = cleared.to_str().expect("clear");
        assert!(cleared.contains("Max-Age=0"));
        assert!(cleared.contains("Path=/api/auth/callback"));
        assert!(cleared.contains("; Secure"));
    }

    #[test]
    fn redirect_must_be_a_relative_path() {
        assert_eq!(safe_redirect(None), "/");
        assert_eq!(safe_redirect(Some("/traces")), "/traces");
        assert_eq!(safe_redirect(Some("/traces?tab=1")), "/traces?tab=1");
        assert_eq!(safe_redirect(Some("//evil.example")), "/");
        assert_eq!(safe_redirect(Some("/\\evil")), "/");
        assert_eq!(safe_redirect(Some("https://evil.example")), "/");
        assert_eq!(safe_redirect(Some("")), "/");
        assert_eq!(safe_redirect(Some("/ok\nSet-Cookie")), "/");
        assert_eq!(safe_redirect(Some(&format!("/{}", "a".repeat(1024)))), "/");
        assert_eq!(
            safe_redirect(Some(&format!("/{}", "a".repeat(1023)))),
            format!("/{}", "a".repeat(1023))
        );
    }
}
