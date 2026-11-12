//! In-process OIDC and GitHub identity provider for tests (D151).
//!
//! The signing key is generated when the mock starts. [`MockIdp::rotate`]
//! replaces it. Tokens are signed with `rsa` 0.10 and `rand::rng()`
//! (`rand_core` 0.10, the same trait the workspace `rand` 0.10 uses).

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use rand::TryRng;
use rsa::signature::{SignatureEncoding, Signer};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Client id the token endpoint accepts.
pub(crate) const CLIENT_ID: &str = "test-client";
/// Client secret the token endpoint accepts. Tests compare it; responses do not echo it.
pub(crate) const CLIENT_SECRET: &str = "test-secret";

/// Claims placed in the id_token. `None` means the mock fills the usual value.
#[derive(Debug, Clone)]
pub(crate) struct IdClaims {
    /// `sub`.
    pub sub: String,
    /// `aud`. Defaults to [`CLIENT_ID`].
    pub aud: Option<String>,
    /// `iss`. Defaults to the mock's base URL.
    pub iss: Option<String>,
    /// `exp`, seconds. Defaults to one hour from now.
    pub exp: Option<i64>,
    /// `iat`, seconds. Defaults to now.
    pub iat: Option<i64>,
    /// `nonce`. Omitted when `None`.
    pub nonce: Option<String>,
    /// `email`. Omitted when `None`.
    pub email: Option<String>,
    /// `email_verified`. Omitted when `None`.
    pub email_verified: Option<bool>,
    /// `hd`. Omitted when `None`.
    pub hd: Option<String>,
    /// `name`. Omitted when `None`.
    pub name: Option<String>,
}

/// A code the token endpoint can exchange.
#[derive(Debug, Clone)]
pub(crate) struct Grant {
    /// Claims signed into the id_token.
    pub claims: IdClaims,
    /// S256 challenge, base64url without padding.
    pub code_challenge: String,
    /// `redirect_uri` the token request must repeat.
    pub redirect_uri: String,
    /// Sign with a key that is not in the published JWKS.
    pub foreign_key: bool,
}

impl Grant {
    /// A grant with only `sub`, the challenge, and the redirect.
    pub(crate) fn new(
        sub: impl Into<String>,
        code_challenge: impl Into<String>,
        redirect_uri: impl Into<String>,
    ) -> Self {
        Self {
            claims: IdClaims {
                sub: sub.into(),
                aud: None,
                iss: None,
                exp: None,
                iat: None,
                nonce: None,
                email: None,
                email_verified: None,
                hd: None,
                name: None,
            },
            code_challenge: code_challenge.into(),
            redirect_uri: redirect_uri.into(),
            foreign_key: false,
        }
    }
}

/// One GitHub account the mock returns from `/user`.
#[derive(Debug, Clone)]
pub(crate) struct GithubUser {
    /// Numeric GitHub user id.
    pub id: u64,
    /// Login.
    pub login: String,
    /// Display name.
    pub name: Option<String>,
    /// Addresses from `/user/emails`.
    pub emails: Vec<GithubEmail>,
}

/// One address in GitHub's email list.
#[derive(Debug, Clone)]
pub(crate) struct GithubEmail {
    /// Address.
    pub email: String,
    /// Primary address.
    pub primary: bool,
    /// Verified by GitHub.
    pub verified: bool,
}

/// A mock IdP bound to `127.0.0.1:0`.
pub(crate) struct MockIdp {
    base: String,
    inner: Arc<Inner>,
    task: tokio::task::JoinHandle<()>,
}

impl fmt::Debug for MockIdp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MockIdp")
            .field("issuer", &self.base)
            .finish()
    }
}

impl Drop for MockIdp {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Inner {
    base: String,
    store: Mutex<Store>,
    discovery_hits: AtomicU64,
    landed_hits: AtomicU64,
    fail_discovery: AtomicBool,
}

struct Store {
    key: KeyMaterial,
    foreign: KeyMaterial,
    grants: HashMap<String, Grant>,
    github_codes: HashMap<String, GithubUser>,
    github_tokens: HashMap<String, GithubUser>,
}

struct KeyMaterial {
    signer: rsa::pkcs1v15::SigningKey<Sha256>,
    kid: String,
    jwk: Value,
}

impl MockIdp {
    /// Binds `127.0.0.1:0` and serves discovery, tokens, and the GitHub routes.
    pub(crate) async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock idp");
        let port = listener.local_addr().expect("mock address").port();
        let base = format!("http://127.0.0.1:{port}");
        let inner = Arc::new(Inner {
            base: base.clone(),
            store: Mutex::new(Store {
                key: KeyMaterial::generate(),
                foreign: KeyMaterial::generate(),
                grants: HashMap::new(),
                github_codes: HashMap::new(),
                github_tokens: HashMap::new(),
            }),
            discovery_hits: AtomicU64::new(0),
            landed_hits: AtomicU64::new(0),
            fail_discovery: AtomicBool::new(false),
        });
        let router = routes(Arc::clone(&inner));
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Self { base, inner, task }
    }

    /// Issuer and base URL, without a trailing slash.
    pub(crate) fn issuer(&self) -> &str {
        &self.base
    }

    /// How many times `/.well-known/openid-configuration` was requested.
    pub(crate) fn discovery_hits(&self) -> u64 {
        self.inner.discovery_hits.load(Ordering::Relaxed)
    }

    /// How many times `/landed` was requested. A client that does not follow
    /// the redirect from `/redirect` leaves this at zero.
    pub(crate) fn landed_hits(&self) -> u64 {
        self.inner.landed_hits.load(Ordering::Relaxed)
    }

    /// Later discovery requests answer 500.
    pub(crate) fn fail_discovery(&self) {
        self.inner.fail_discovery.store(true, Ordering::Relaxed);
    }

    /// `kid` of the key currently published at `/jwks`.
    pub(crate) fn signing_kid(&self) -> String {
        self.lock().key.kid.clone()
    }

    /// Replaces the published signing key. Already-issued tokens keep the old `kid`.
    pub(crate) fn rotate(&self) {
        self.lock().key = KeyMaterial::generate();
    }

    /// Registers `grant` and returns the authorization code.
    pub(crate) fn issue_code(&self, grant: Grant) -> String {
        let code = random_token();
        self.lock().grants.insert(code.clone(), grant);
        code
    }

    /// Registers a GitHub user and returns the authorization code.
    pub(crate) fn issue_github_code(&self, user: GithubUser) -> String {
        let code = random_token();
        self.lock().github_codes.insert(code.clone(), user);
        code
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Store> {
        self.inner.store.lock().expect("mock idp lock")
    }
}

fn routes(inner: Arc<Inner>) -> Router {
    Router::new()
        .route("/.well-known/openid-configuration", get(configuration))
        .route("/jwks", get(jwks))
        .route("/token", post(token))
        .route("/login/oauth/access_token", post(github_token))
        .route("/user", get(github_user))
        .route("/user/emails", get(github_emails))
        .route("/redirect", get(redirect_to_landed))
        .route("/landed", get(landed))
        .with_state(inner)
}

async fn configuration(State(inner): State<Arc<Inner>>) -> Response {
    inner.discovery_hits.fetch_add(1, Ordering::Relaxed);
    if inner.fail_discovery.load(Ordering::Relaxed) {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    let base = &inner.base;
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "jwks_uri": format!("{base}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
    }))
    .into_response()
}

async fn jwks(State(inner): State<Arc<Inner>>) -> Response {
    let store = inner.store.lock().expect("mock idp lock");
    Json(json!({ "keys": [store.key.jwk.clone()] })).into_response()
}

async fn token(State(inner): State<Arc<Inner>>, headers: HeaderMap, body: Bytes) -> Response {
    let form = match serde_urlencoded::from_bytes::<TokenForm>(&body) {
        Ok(form) => form,
        Err(_) => return oauth_error("invalid_grant"),
    };
    if !client_ok(&headers, &form.client_id, &form.client_secret) {
        return oauth_error("invalid_client");
    }
    let mut store = inner.store.lock().expect("mock idp lock");
    let Some(grant) = store.grants.get(&form.code).cloned() else {
        return oauth_error("invalid_grant");
    };
    if form.redirect_uri != grant.redirect_uri || s256(&form.code_verifier) != grant.code_challenge
    {
        return oauth_error("invalid_grant");
    }
    store.grants.remove(&form.code);
    let key = if grant.foreign_key {
        &store.foreign
    } else {
        &store.key
    };
    let id_token = compact(key, &claims_json(&inner.base, &grant));
    drop(store);
    Json(json!({
        "access_token": random_token(),
        "token_type": "Bearer",
        "expires_in": 3600,
        "id_token": id_token,
    }))
    .into_response()
}

async fn github_token(
    State(inner): State<Arc<Inner>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let form = match serde_urlencoded::from_bytes::<TokenForm>(&body) {
        Ok(form) => form,
        Err(_) => return oauth_error("invalid_grant"),
    };
    if !client_ok(&headers, &form.client_id, &form.client_secret) {
        return oauth_error("invalid_client");
    }
    let mut store = inner.store.lock().expect("mock idp lock");
    let Some(user) = store.github_codes.remove(&form.code) else {
        return oauth_error("invalid_grant");
    };
    let access_token = random_token();
    store.github_tokens.insert(access_token.clone(), user);
    drop(store);
    Json(json!({
        "access_token": access_token,
        "token_type": "bearer",
    }))
    .into_response()
}

async fn github_user(State(inner): State<Arc<Inner>>, headers: HeaderMap) -> Response {
    let Some(user) = github_bearer(&inner, &headers) else {
        return oauth_error("invalid_grant");
    };
    Json(json!({
        "id": user.id,
        "login": user.login,
        "name": user.name,
    }))
    .into_response()
}

async fn github_emails(State(inner): State<Arc<Inner>>, headers: HeaderMap) -> Response {
    let Some(user) = github_bearer(&inner, &headers) else {
        return oauth_error("invalid_grant");
    };
    let emails = user
        .emails
        .iter()
        .map(|email| {
            json!({
                "email": email.email,
                "primary": email.primary,
                "verified": email.verified,
            })
        })
        .collect::<Vec<_>>();
    Json(emails).into_response()
}

async fn redirect_to_landed(State(inner): State<Arc<Inner>>) -> Response {
    let location = format!("{}/landed", inner.base);
    let location = axum::http::HeaderValue::from_str(&location).expect("location");
    (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
}

async fn landed(State(inner): State<Arc<Inner>>) -> StatusCode {
    inner.landed_hits.fetch_add(1, Ordering::Relaxed);
    StatusCode::NO_CONTENT
}

fn github_bearer(inner: &Inner, headers: &HeaderMap) -> Option<GithubUser> {
    let header = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = header.strip_prefix("Bearer ")?;
    inner
        .store
        .lock()
        .expect("mock idp lock")
        .github_tokens
        .get(token)
        .cloned()
}

fn client_ok(headers: &HeaderMap, client_id: &str, client_secret: &str) -> bool {
    if let Some(basic) = basic_credentials(headers) {
        return basic == format!("{CLIENT_ID}:{CLIENT_SECRET}");
    }
    client_id == CLIENT_ID && client_secret == CLIENT_SECRET
}

fn basic_credentials(headers: &HeaderMap) -> Option<String> {
    let header = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let encoded = header.strip_prefix("Basic ")?;
    let bytes = STANDARD.decode(encoded).ok()?;
    String::from_utf8(bytes).ok()
}

fn oauth_error(error: &'static str) -> Response {
    (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))).into_response()
}

#[derive(Debug, Deserialize)]
struct TokenForm {
    #[serde(default)]
    code: String,
    #[serde(default)]
    code_verifier: String,
    #[serde(default)]
    redirect_uri: String,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    client_secret: String,
}

fn s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn claims_json(issuer: &str, grant: &Grant) -> Value {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(0))
        .unwrap_or(0);
    let claims = &grant.claims;
    let mut body = serde_json::Map::new();
    body.insert(
        "iss".to_owned(),
        json!(claims.iss.as_deref().unwrap_or(issuer)),
    );
    body.insert(
        "aud".to_owned(),
        json!(claims.aud.as_deref().unwrap_or(CLIENT_ID)),
    );
    body.insert("sub".to_owned(), json!(claims.sub));
    body.insert("exp".to_owned(), json!(claims.exp.unwrap_or(now + 3600)));
    body.insert("iat".to_owned(), json!(claims.iat.unwrap_or(now)));
    if let Some(nonce) = &claims.nonce {
        body.insert("nonce".to_owned(), json!(nonce));
    }
    if let Some(email) = &claims.email {
        body.insert("email".to_owned(), json!(email));
    }
    if let Some(verified) = claims.email_verified {
        body.insert("email_verified".to_owned(), json!(verified));
    }
    if let Some(hd) = &claims.hd {
        body.insert("hd".to_owned(), json!(hd));
    }
    if let Some(name) = &claims.name {
        body.insert("name".to_owned(), json!(name));
    }
    Value::Object(body)
}

fn compact(key: &KeyMaterial, claims: &Value) -> String {
    let header = json!({ "alg": "RS256", "typ": "JWT", "kid": key.kid });
    let signing_input = format!(
        "{}.{}",
        b64(&serde_json::to_vec(&header).expect("jwt header")),
        b64(&serde_json::to_vec(claims).expect("jwt claims"))
    );
    let signature = b64(&key.sign(signing_input.as_bytes()));
    format!("{signing_input}.{signature}")
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn random_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng()
        .try_fill_bytes(&mut bytes)
        .expect("mock idp random");
    URL_SAFE_NO_PAD.encode(bytes)
}

impl KeyMaterial {
    fn generate() -> Self {
        let mut rng = rand::rng();
        let private = rsa::PrivateKey::new(&mut rng, 2048).expect("rsa key");
        let public = private.to_public_key();
        let n = rsa::traits::PublicKeyParts::n_bytes(&public);
        let e = rsa::traits::PublicKeyParts::e_bytes(&public);
        let kid = kid_of(&n);
        let jwk = json!({
            "kty": "RSA",
            "use": "sig",
            "alg": "RS256",
            "kid": kid,
            "n": URL_SAFE_NO_PAD.encode(&n),
            "e": URL_SAFE_NO_PAD.encode(&e),
        });
        Self {
            signer: rsa::pkcs1v15::SigningKey::<Sha256>::new(private),
            kid,
            jwk,
        }
    }

    fn sign(&self, message: &[u8]) -> Vec<u8> {
        let signature = Signer::try_sign(&self.signer, message).expect("sign id token");
        signature.to_vec()
    }
}

fn kid_of(modulus: &[u8]) -> String {
    let digest = Sha256::digest(modulus);
    digest
        .iter()
        .take(8)
        .fold(String::with_capacity(16), |mut out, byte| {
            out.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
            out.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
            out
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http() -> reqwest::Client {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("client")
    }

    fn challenge(verifier: &str) -> String {
        s256(verifier)
    }

    fn payload(token: &str) -> Value {
        let segment = token.split('.').nth(1).expect("payload");
        let bytes = URL_SAFE_NO_PAD.decode(segment).expect("payload bytes");
        serde_json::from_slice(&bytes).expect("payload json")
    }

    fn header(token: &str) -> Value {
        let segment = token.split('.').next().expect("header");
        let bytes = URL_SAFE_NO_PAD.decode(segment).expect("header bytes");
        serde_json::from_slice(&bytes).expect("header json")
    }

    async fn exchange(idp: &MockIdp, code: &str, verifier: &str, redirect: &str) -> Value {
        let response = http()
            .post(format!("{}/token", idp.issuer()))
            .form(&[
                ("code", code),
                ("code_verifier", verifier),
                ("redirect_uri", redirect),
                ("client_id", CLIENT_ID),
                ("client_secret", CLIENT_SECRET),
            ])
            .send()
            .await
            .expect("token");
        let status = response.status();
        let body: Value = response.json().await.expect("token json");
        assert_eq!(status, StatusCode::OK, "{body}");
        body
    }

    #[tokio::test]
    async fn token_signs_the_claims_the_test_chose() {
        let idp = MockIdp::start().await;
        let verifier = "verifier-value";
        let redirect = "http://127.0.0.1/callback";
        let mut grant = Grant::new("user-1", challenge(verifier), redirect);
        grant.claims.nonce = Some("nonce-1".to_owned());
        grant.claims.email = Some("ada@example.com".to_owned());
        grant.claims.email_verified = Some(true);
        grant.claims.hd = Some("example.com".to_owned());
        grant.claims.name = Some("Ada".to_owned());
        let code = idp.issue_code(grant);
        let body = exchange(&idp, &code, verifier, redirect).await;
        let token = body["id_token"].as_str().expect("id_token");
        let claims = payload(token);
        assert_eq!(claims["sub"], "user-1");
        assert_eq!(claims["aud"], CLIENT_ID);
        assert_eq!(claims["iss"], idp.issuer());
        assert_eq!(claims["nonce"], "nonce-1");
        assert_eq!(claims["email"], "ada@example.com");
        assert_eq!(claims["email_verified"], true);
        assert_eq!(claims["hd"], "example.com");
        assert_eq!(claims["name"], "Ada");
        assert_eq!(header(token)["kid"], idp.signing_kid());
        assert_eq!(header(token)["alg"], "RS256");
        let signature = URL_SAFE_NO_PAD
            .decode(token.split('.').nth(2).expect("signature"))
            .expect("signature bytes");
        assert_eq!(signature.len(), 256);
        assert!(!body.to_string().contains(CLIENT_SECRET));
    }

    #[tokio::test]
    async fn knobs_change_the_token_and_rotation_changes_the_kid() {
        let idp = MockIdp::start().await;
        let verifier = "verifier-value";
        let redirect = "http://127.0.0.1/callback";
        let mut foreign = Grant::new("user-1", challenge(verifier), redirect);
        foreign.foreign_key = true;
        let code = idp.issue_code(foreign);
        let token = exchange(&idp, &code, verifier, redirect).await;
        let token = token["id_token"].as_str().expect("id_token");
        assert_ne!(header(token)["kid"], idp.signing_kid());

        let mut wrong = Grant::new("user-1", challenge(verifier), redirect);
        wrong.claims.aud = Some("other-client".to_owned());
        wrong.claims.iss = Some("https://evil.example".to_owned());
        wrong.claims.exp = Some(1);
        let code = idp.issue_code(wrong);
        let token = exchange(&idp, &code, verifier, redirect).await;
        let claims = payload(token["id_token"].as_str().expect("id_token"));
        assert_eq!(claims["aud"], "other-client");
        assert_eq!(claims["iss"], "https://evil.example");
        assert_eq!(claims["exp"], 1);

        let before = idp.signing_kid();
        idp.rotate();
        let after = idp.signing_kid();
        assert_ne!(before, after);
        let code = idp.issue_code(Grant::new("user-2", challenge(verifier), redirect));
        let token = exchange(&idp, &code, verifier, redirect).await;
        assert_eq!(
            header(token["id_token"].as_str().expect("id_token"))["kid"],
            after
        );
    }

    #[tokio::test]
    async fn bad_client_or_verifier_is_a_static_error() {
        let idp = MockIdp::start().await;
        let verifier = "verifier-value";
        let redirect = "http://127.0.0.1/callback";
        let code = idp.issue_code(Grant::new("user-1", challenge(verifier), redirect));
        let response = http()
            .post(format!("{}/token", idp.issuer()))
            .form(&[
                ("code", code.as_str()),
                ("code_verifier", verifier),
                ("redirect_uri", redirect),
                ("client_id", CLIENT_ID),
                ("client_secret", "not-the-secret"),
            ])
            .send()
            .await
            .expect("token");
        let status = response.status();
        let text = response.text().await.expect("body");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(text.contains("invalid_client"), "{text}");
        assert!(!text.contains(CLIENT_SECRET), "{text}");
        assert!(!text.contains("not-the-secret"), "{text}");

        let response = http()
            .post(format!("{}/token", idp.issuer()))
            .form(&[
                ("code", code.as_str()),
                ("code_verifier", "wrong-verifier"),
                ("redirect_uri", redirect),
                ("client_id", CLIENT_ID),
                ("client_secret", CLIENT_SECRET),
            ])
            .send()
            .await
            .expect("token");
        let text = response.text().await.expect("body");
        assert!(text.contains("invalid_grant"), "{text}");
        assert!(!text.contains("wrong-verifier"), "{text}");
        assert!(!text.contains(CLIENT_SECRET), "{text}");
    }

    #[tokio::test]
    async fn basic_auth_is_accepted() {
        let idp = MockIdp::start().await;
        let verifier = "verifier-value";
        let redirect = "http://127.0.0.1/callback";
        let code = idp.issue_code(Grant::new("user-1", challenge(verifier), redirect));
        let basic = STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"));
        let response = http()
            .post(format!("{}/token", idp.issuer()))
            .header(header::AUTHORIZATION, format!("Basic {basic}"))
            .form(&[
                ("code", code.as_str()),
                ("code_verifier", verifier),
                ("redirect_uri", redirect),
            ])
            .send()
            .await
            .expect("token");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn github_code_returns_the_user_and_emails() {
        let idp = MockIdp::start().await;
        let code = idp.issue_github_code(GithubUser {
            id: 7,
            login: "ada".to_owned(),
            name: Some("Ada".to_owned()),
            emails: vec![GithubEmail {
                email: "ada@example.com".to_owned(),
                primary: true,
                verified: true,
            }],
        });
        let response = http()
            .post(format!("{}/login/oauth/access_token", idp.issuer()))
            .form(&[
                ("code", code.as_str()),
                ("client_id", CLIENT_ID),
                ("client_secret", CLIENT_SECRET),
            ])
            .send()
            .await
            .expect("github token");
        let body: Value = response.json().await.expect("json");
        let token = body["access_token"].as_str().expect("access token");
        assert!(!token.is_empty());
        let user: Value = http()
            .get(format!("{}/user", idp.issuer()))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("user")
            .json()
            .await
            .expect("user json");
        assert_eq!(user["id"], 7);
        assert_eq!(user["login"], "ada");
        assert_eq!(user["name"], "Ada");
        let emails: Value = http()
            .get(format!("{}/user/emails", idp.issuer()))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .expect("emails")
            .json()
            .await
            .expect("emails json");
        assert_eq!(emails[0]["email"], "ada@example.com");
        assert_eq!(emails[0]["primary"], true);
        assert_eq!(emails[0]["verified"], true);
    }

    #[tokio::test]
    async fn discovery_failure_is_an_empty_500() {
        let idp = MockIdp::start().await;
        idp.fail_discovery();
        let response = http()
            .get(format!("{}/.well-known/openid-configuration", idp.issuer()))
            .send()
            .await
            .expect("discovery");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let text = response.text().await.expect("body");
        assert!(text.is_empty(), "{text}");
        assert_eq!(idp.discovery_hits(), 1);
    }
}
