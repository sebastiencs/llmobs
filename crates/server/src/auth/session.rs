//! Web session cookie (spec 08).
//!
//! The database stores the SHA-256 hex. The cookie carries the plaintext.
#![cfg_attr(not(test), allow(dead_code))]

use std::net::IpAddr;
use std::time::Duration;

use axum::http::{HeaderMap, HeaderValue, header};
use axum_extra::extract::cookie::Cookie;

use super::secret::{hash_token, is_opaque_token, new_opaque_token};
use crate::http::AppState;
use crate::http::error::ApiError;
use crate::meta::web_sessions::{
    NewSession, SessionRow, insert_session, session_user, touch_session,
};

/// `llmobs_session`. Built from [`llmobs_core::llmobs_name`], the same constant as the key prefixes.
pub(crate) const SESSION_COOKIE: &str = llmobs_core::llmobs_name!("_session");

/// How long a session stays valid after it was last seen.
pub(crate) const LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// A session slides at most once per this interval.
pub(crate) const REFRESH_AFTER: Duration = Duration::from_secs(60 * 60);

/// Inserts a session and returns the `Set-Cookie` value.
///
/// `user_agent` is cut to 256 characters. `Secure` is set when `public_url` is https.
///
/// # Errors
///
/// Returns [`ApiError::Internal`] when the random source or the insert fails.
pub(crate) async fn start_session(
    state: &AppState,
    user_id: uuid::Uuid,
    user_agent: Option<&str>,
    ip: Option<IpAddr>,
) -> Result<HeaderValue, ApiError> {
    let token = new_opaque_token().map_err(|error| ApiError::Internal(Box::new(error)))?;
    let now = state.clock.now_ns();
    let expires_at = now.saturating_add(duration_ns(LIFETIME));
    let hash = token.hash.clone();
    let user_agent = user_agent.map(str::to_owned);
    let ip = ip.map(|addr| addr.to_string());
    state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                insert_session(
                    conn,
                    NewSession {
                        hash,
                        user_id,
                        user_agent,
                        ip,
                    },
                    now,
                    expires_at,
                )
                .await
            })
        })
        .await?;
    Ok(set_cookie(state, &token.plaintext, LIFETIME.as_secs()))
}

/// `Max-Age=0` with the same attributes as [`start_session`].
pub(crate) fn clear_cookie(state: &AppState) -> HeaderValue {
    set_cookie(state, "", 0)
}

/// Looks up one cookie value.
///
/// A value that is not an opaque token returns `Ok(None)` without a query.
/// When `now - last_seen_at >= REFRESH_AFTER`, the row slides and the result
/// carries a new `Set-Cookie` for the same token.
///
/// # Errors
///
/// Returns [`ApiError::Internal`] when the lookup or the slide fails.
pub(crate) async fn resolve_session(
    state: &AppState,
    cookie_value: &str,
    now: i64,
) -> Result<Option<(SessionRow, Option<HeaderValue>)>, ApiError> {
    if !is_opaque_token(cookie_value) {
        return Ok(None);
    }
    let hash = hash_token(cookie_value);
    let Some(mut row) = session_user(state.meta.reader(), &hash, now).await? else {
        return Ok(None);
    };
    if now.saturating_sub(row.last_seen_at) < duration_ns(REFRESH_AFTER) {
        return Ok(Some((row, None)));
    }
    let expires_at = now.saturating_add(duration_ns(LIFETIME));
    state
        .meta
        .write({
            let hash = hash.clone();
            move |conn| Box::pin(async move { touch_session(conn, &hash, now, expires_at).await })
        })
        .await?;
    row.last_seen_at = now;
    row.expires_at = expires_at;
    Ok(Some((
        row,
        Some(set_cookie(state, cookie_value, LIFETIME.as_secs())),
    )))
}

/// The first `llmobs_session` cookie in `headers` that resolves.
///
/// A request can repeat the name. Values that are not opaque tokens are skipped
/// without a query. The first one that finds a session wins.
///
/// # Errors
///
/// Returns [`ApiError::Internal`] when a lookup fails.
pub(crate) async fn resolve_cookie_header(
    state: &AppState,
    headers: &HeaderMap,
    now: i64,
) -> Result<Option<(SessionRow, Option<HeaderValue>)>, ApiError> {
    for value in session_values(headers) {
        if let Some(found) = resolve_session(state, &value, now).await? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn session_values(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|part| {
            let cookie = Cookie::parse_encoded(part.trim().to_owned()).ok()?;
            (cookie.name() == SESSION_COOKIE).then(|| cookie.value().to_owned())
        })
        .collect()
}

fn set_cookie(state: &AppState, token: &str, max_age: u64) -> HeaderValue {
    let mut text =
        format!("{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}");
    if state.config.public_url.is_https() {
        text.push_str("; Secure");
    }
    header_value(&text)
}

fn header_value(text: &str) -> HeaderValue {
    // The name, the token alphabet, and the attributes are all ASCII.
    #[allow(clippy::expect_used)]
    HeaderValue::from_str(text).expect("session cookie is ascii")
}

fn duration_ns(duration: Duration) -> i64 {
    i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::time::Duration;

    use axum::http::{HeaderMap, HeaderValue};

    use super::{
        LIFETIME, REFRESH_AFTER, SESSION_COOKIE, clear_cookie, duration_ns, resolve_cookie_header,
        resolve_session, start_session,
    };
    use crate::auth::secret::{hash_token, new_opaque_token};
    use crate::meta::users::{NewUser, insert_user, set_disabled};
    use crate::test_support::TestApp;

    async fn user(app: &TestApp) -> uuid::Uuid {
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    let row = insert_user(
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
                    Ok(row.id)
                })
            })
            .await
            .unwrap()
    }

    fn token_of(header: &HeaderValue) -> String {
        let text = header.to_str().unwrap();
        let (pair, rest) = text.split_once(';').unwrap();
        let (name, value) = pair.split_once('=').unwrap();
        assert_eq!(name, SESSION_COOKIE);
        assert!(rest.contains("Path=/"));
        assert!(rest.contains("HttpOnly"));
        assert!(rest.contains("SameSite=Lax"));
        assert!(rest.contains(&format!("Max-Age={}", LIFETIME.as_secs())));
        value.to_owned()
    }

    #[tokio::test]
    async fn start_sets_the_cookie_and_stores_only_the_hash() {
        let app = TestApp::new().await;
        let user_id = user(&app).await;
        let ip: IpAddr = "203.0.113.8".parse().unwrap();
        let header = start_session(&app.state, user_id, Some(&"a".repeat(300)), Some(ip))
            .await
            .unwrap();
        let text = header.to_str().unwrap();
        assert!(!text.contains("Secure"), "{text}");
        let token = token_of(&header);
        assert_eq!(token.len(), 43);

        let now = app.state.clock.now_ns();
        let (row, refresh) = resolve_session(&app.state, &token, now)
            .await
            .unwrap()
            .unwrap();
        assert!(refresh.is_none());
        assert_eq!(row.user.id, user_id);
        assert_eq!(row.user.email, "ada@example.com");
        assert_eq!(row.expires_at, now + duration_ns(LIFETIME));

        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM web_sessions")
            .fetch_all(app.state.meta.reader())
            .await
            .unwrap();
        assert_eq!(ids, vec![hash_token(&token)]);
        let stored_agent: String = sqlx::query_scalar("SELECT user_agent FROM web_sessions")
            .fetch_one(app.state.meta.reader())
            .await
            .unwrap();
        assert_eq!(stored_agent.chars().count(), 256);

        let https = TestApp::with_config(|config| {
            config.public_url = serde_json::from_str("\"https://llmobs.example\"").unwrap();
        })
        .await;
        let secure = start_session(&https.state, user(&https).await, None, None)
            .await
            .unwrap();
        assert!(secure.to_str().unwrap().ends_with("; Secure"));
        let clearing = clear_cookie(&https.state);
        let clearing = clearing.to_str().unwrap();
        assert!(clearing.contains("Max-Age=0"));
        assert!(clearing.ends_with("; Secure"));
        assert!(
            !clear_cookie(&app.state)
                .to_str()
                .unwrap()
                .contains("Secure")
        );
    }

    #[tokio::test]
    async fn refresh_waits_an_hour_and_thirty_idle_days_expire() {
        let app = TestApp::new().await;
        let user_id = user(&app).await;
        let header = start_session(&app.state, user_id, None, None)
            .await
            .unwrap();
        let token = token_of(&header);
        let started = app.state.clock.now_ns();

        app.state.clock.advance(Duration::from_secs(59 * 60));
        let now = app.state.clock.now_ns();
        let (row, refresh) = resolve_session(&app.state, &token, now)
            .await
            .unwrap()
            .unwrap();
        assert!(refresh.is_none());
        assert_eq!(row.expires_at, started + duration_ns(LIFETIME));
        assert_eq!(row.last_seen_at, started);

        app.state.clock.advance(Duration::from_secs(2 * 60));
        let now = app.state.clock.now_ns();
        let (row, refresh) = resolve_session(&app.state, &token, now)
            .await
            .unwrap()
            .unwrap();
        let refresh = refresh.unwrap();
        assert!(refresh.to_str().unwrap().contains(&token));
        assert_eq!(row.last_seen_at, now);
        assert_eq!(row.expires_at, now + duration_ns(LIFETIME));
        let stored: i64 = sqlx::query_scalar("SELECT expires_at FROM web_sessions")
            .fetch_one(app.state.meta.reader())
            .await
            .unwrap();
        assert_eq!(stored, row.expires_at);

        app.state.clock.advance(LIFETIME);
        let now = app.state.clock.now_ns();
        assert!(
            resolve_session(&app.state, &token, now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(Duration::from_secs(59 * 60) < REFRESH_AFTER);
        assert!(Duration::from_secs(61 * 60) > REFRESH_AFTER);
    }

    #[tokio::test]
    async fn a_disabled_user_and_a_garbage_cookie_do_not_resolve() {
        let app = TestApp::new().await;
        let user_id = user(&app).await;
        let header = start_session(&app.state, user_id, None, None)
            .await
            .unwrap();
        let token = token_of(&header);
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move { set_disabled(conn, user_id, Some(now), now).await })
            })
            .await
            .unwrap();
        assert!(
            resolve_session(&app.state, &token, now)
                .await
                .unwrap()
                .is_none()
        );

        app.state.meta.reader().close().await;
        assert!(
            resolve_session(&app.state, "abc", now)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn the_first_cookie_that_resolves_wins() {
        let app = TestApp::new().await;
        let user_id = user(&app).await;
        let header = start_session(&app.state, user_id, None, None)
            .await
            .unwrap();
        let token = token_of(&header);
        let unknown = new_opaque_token().unwrap().plaintext;
        let now = app.state.clock.now_ns();
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            HeaderValue::from_str(&format!("{SESSION_COOKIE}={token}; {SESSION_COOKIE}=abc"))
                .unwrap(),
        );
        let (row, _) = resolve_cookie_header(&app.state, &headers, now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.hash, hash_token(&token));

        headers.insert(
            axum::http::header::COOKIE,
            HeaderValue::from_str(&format!(
                "{SESSION_COOKIE}={unknown}; {SESSION_COOKIE}={token}"
            ))
            .unwrap(),
        );
        let (row, _) = resolve_cookie_header(&app.state, &headers, now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.user.id, user_id);
    }
}
