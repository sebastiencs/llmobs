//! axum router, shared state, and the request log (D150).
//!
//! `llmobs serve` binds [`router`] with
//! `into_make_service_with_connect_info::<SocketAddr>()` so [`ClientIp`] can
//! read the socket peer. That binding is P5.
#![cfg_attr(not(test), allow(dead_code))] // `llmobs serve` binds this in P5

pub(crate) mod access;
pub(crate) mod client_ip;
mod csrf;
pub(crate) mod error;
mod health;

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::OnceCell;

use axum::Router;
use axum::extract::{DefaultBodyLimit, MatchedPath, Request};
use axum::middleware::{self, Next};
use axum::response::Response;
use llmobs_storage::Storage;
use tower::ServiceExt;

use crate::auth::Limits;
use crate::auth::last_used::LastUsed;
use crate::auth::principal::{ApiKeyCache, PatCache, authenticate};
use crate::clock::Clock;
use crate::config::ServerConfig;
use crate::meta::MetaDb;

/// Bodies on this router. Later tracks raise it on a single route.
const BODY_LIMIT: usize = 1024 * 1024;

/// Shared server state. Cloning copies the `Arc`.
#[derive(Clone)]
pub(crate) struct AppState(Arc<AppInner>);

pub(crate) struct AppInner {
    pub(crate) config: ServerConfig,
    pub(crate) meta: MetaDb,
    pub(crate) storage: Arc<Storage>,
    pub(crate) clock: Clock,
    pub(crate) limits: Limits,
    /// Successful API-key lookups, cached 60 s by hash.
    pub(crate) key_cache: ApiKeyCache,
    /// Successful personal-token lookups, cached 60 s by hash. The user is not cached.
    pub(crate) pat_cache: PatCache,
    /// Coalesced `api_keys.last_used_at` updates.
    pub(crate) last_used: LastUsed,
    /// Coalesced `personal_tokens.last_used_at` updates.
    pub(crate) token_last_used: LastUsed,
    pub(crate) setup: crate::bootstrap::SetupState,
    /// Built on the first password check, so tests that never log in skip argon2.
    pub(crate) password_hasher: OnceCell<crate::auth::password::Hasher>,
    /// `None` when `smtp_url` is unset. Shared so a send can outlive the request.
    pub(crate) mailer: Option<Arc<crate::email::Mailer>>,
    /// OAuth and OIDC providers from `[auth]`. Discovery has not run yet.
    pub(crate) providers: crate::auth::oidc::Providers,
    /// HMAC key for the OAuth state cookie. A new process mints a new key.
    pub(crate) oauth_key: [u8; 32],
    /// OTLP ingest core: budget, prices, `accept` and `commit` (D-G-1).
    pub(crate) ingest: crate::ingest::Ingest,
    /// Live bus and stream slots (D-G-12).
    pub(crate) live: crate::live::LiveBus,
    /// `GET /metrics` answers cached 10 s (spec 05).
    pub(crate) metrics_cache: crate::routes::metrics::MetricsCache,
    /// Usage counters, flushed every 10 s (D-G-11).
    pub(crate) usage: crate::usage::Usage,
    /// Deletion worker state: wake-up, guard, attempts (D-M-3).
    pub(crate) deletions: crate::deletion::Deletions,
    /// Seals and opens secret columns (D-I-2).
    pub(crate) secrets: crate::crypto::SecretBox,
    /// One egress client for judges, webhooks and connection tests (D-I-29).
    pub(crate) eval_runner: Arc<llmobs_eval::EvalRunner>,
    /// Decrypted connections, keyed by id and `updated_at`.
    pub(crate) connections: crate::eval::connection::ConnectionCache,
    /// Licence and (from M2) org entitlements (D179, D-P-10).
    pub(crate) entitlements: crate::entitlements::EntitlementsState,
}

impl AppState {
    pub(crate) fn new(inner: AppInner) -> Self {
        Self(Arc::new(inner))
    }

    /// The state when this is the last clone. The router holds the other one
    /// until its tasks finish.
    pub(crate) fn into_inner(self) -> Result<AppInner, Self> {
        match Arc::try_unwrap(self.0) {
            Ok(inner) => Ok(inner),
            Err(arc) => Err(Self(arc)),
        }
    }
}

impl std::ops::Deref for AppState {
    type Target = AppInner;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AppState {
    /// The process hasher, created on the first password check.
    ///
    /// # Errors
    ///
    /// Returns [`error::ApiError::Internal`] when the dummy hash cannot be computed.
    pub(crate) async fn passwords(
        &self,
    ) -> Result<&crate::auth::password::Hasher, error::ApiError> {
        self.password_hasher
            .get_or_try_init(crate::auth::password::Hasher::new)
            .await
            .map_err(|error| error::ApiError::Internal(Box::new(error)))
    }
}

/// Health probes and the shared layers. `ui` answers every path that is not an API path.
pub(crate) fn router(state: AppState, ui: Option<Router>) -> Router {
    let routed = access::build_router(&state, access::routes());
    finish(state, routed, ui)
}

/// [`router`] plus routes that tests add before the body limit and the log.
pub(crate) fn router_with(state: AppState, extra: Router<AppState>) -> Router {
    let routed = access::build_router(&state, access::routes()).merge(extra);
    finish(state, routed, None)
}

/// The route table, then the shared layers. Tests pass their own table.
pub(crate) fn serve_router(state: AppState, routes: Vec<access::RouteDef>) -> Router {
    let routed = access::build_router(&state, routes);
    finish(state, routed, None)
}

/// The OTLP/HTTP listener's router: `/v1/traces` and nothing else (D-G-21).
pub(crate) fn otlp_router(state: AppState) -> Router {
    serve_router(state, crate::routes::otlp::routes())
}

fn finish(state: AppState, routed: Router<AppState>, ui: Option<Router>) -> Router {
    // serde_json's default recursion limit is 128, which is spec 12's JSON depth limit.
    // Logging is the outer layer. Authentication runs next, then the origin check (D141).
    routed
        .fallback(move |request: Request| fallback(ui.clone(), request))
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .layer(middleware::from_fn_with_state(state.clone(), csrf::csrf))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .layer(middleware::from_fn(log_request))
        .with_state(state)
}

/// Unknown API paths are `404` problems; any other path goes to the UI when there is one.
async fn fallback(ui: Option<Router>, request: Request) -> Response {
    let path = request.uri().path();
    let api = matches!(path, "/api" | "/v1" | "/mcp" | "/healthz" | "/readyz")
        || path.starts_with("/api/")
        || path.starts_with("/v1/")
        || path.starts_with("/mcp/");
    match ui {
        Some(ui) if !api => match ui.oneshot(request).await {
            Ok(response) => response,
            Err(never) => match never {},
        },
        _ => not_found().await,
    }
}

async fn not_found() -> Response {
    error::problem_response(&error::not_found_problem())
}

async fn method_not_allowed() -> Response {
    error::problem_response(&error::method_not_allowed_problem())
}

/// One `info` event per request. The route is the matched template (D150).
async fn log_request(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("<unmatched>", MatchedPath::as_str)
        .to_owned();
    let started = Instant::now();
    let response = next.run(request).await;
    let status = response.status().as_u16();
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    tracing::info!(
        target: "http",
        %method,
        %route,
        status,
        duration_ms,
        "request"
    );
    response
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use serde::Deserialize;

    use super::client_ip::ClientIp;
    use super::error::{ApiError, Json, Path, Query};
    use crate::api::common::ProblemCode;
    use crate::test_support::{TestApp, assert_logs_clean, capture_logs};

    #[tokio::test(flavor = "current_thread")]
    async fn health_probes() {
        let app = TestApp::new().await;
        let live = app
            .request(axum::http::Method::GET, "/healthz")
            .send()
            .await;
        assert_eq!(live.status, 200);
        let body: serde_json::Value = live.json();
        assert_eq!(body["status"], "ok");

        let ready = app.request(axum::http::Method::GET, "/readyz").send().await;
        assert_eq!(ready.status, 200);
        let body: serde_json::Value = ready.json();
        assert_eq!(body["status"], "ready");

        app.state.meta.reader().close().await;
        let down = app.request(axum::http::Method::GET, "/readyz").send().await;
        assert_eq!(down.status, 503);
        assert_eq!(
            down.headers.get("content-type").unwrap(),
            "application/problem+json"
        );
        let problem = down.problem();
        assert_eq!(problem.code, ProblemCode::Busy);
        assert_eq!(problem.detail.as_deref(), Some("meta.db"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn unknown_path_and_method_are_problems() {
        let app = TestApp::new().await;
        let missing = app
            .request(axum::http::Method::GET, "/missing")
            .send()
            .await;
        assert_eq!(missing.status, 404);
        assert_eq!(
            missing.headers.get("content-type").unwrap(),
            "application/problem+json"
        );
        assert_eq!(missing.problem().code, ProblemCode::NotFound);

        let wrong = app
            .request(axum::http::Method::POST, "/healthz")
            .send()
            .await;
        assert_eq!(wrong.status, 405);
        assert_eq!(
            wrong.headers.get("content-type").unwrap(),
            "application/problem+json"
        );
        assert_eq!(wrong.problem().code, ProblemCode::MethodNotAllowed);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn json_rejections_hide_the_input() {
        let app = TestApp::with_routes(Router::new().route("/json", post(take_json))).await;

        let typed = app
            .request(axum::http::Method::POST, "/json")
            .header("content-type", "text/plain")
            .body("{\"secret\": tru")
            .send()
            .await;
        assert_eq!(typed.status, 415);
        assert_eq!(typed.problem().code, ProblemCode::UnsupportedMediaType);
        assert!(!typed_body_contains(&typed, "tru"));

        let invalid = app
            .request(axum::http::Method::POST, "/json")
            .header("content-type", "application/json")
            .body("{\"secret\": tru")
            .send()
            .await;
        assert_eq!(invalid.status, 400);
        assert_eq!(invalid.problem().code, ProblemCode::BadRequest);
        let detail = invalid.problem().detail.unwrap();
        assert!(detail.contains("invalid JSON body"), "{detail}");
        assert!(!detail.contains("tru"), "{detail}");

        let big = vec![b'{'; 1024 * 1024 + 1];
        let limited = app
            .request(axum::http::Method::POST, "/json")
            .header("content-type", "application/json")
            .body(big)
            .send()
            .await;
        assert_eq!(limited.status, 413);
        assert_eq!(limited.problem().code, ProblemCode::PayloadTooLarge);

        let accepted = app
            .request(axum::http::Method::POST, "/json")
            .json(&serde_json::json!({"ok": true}))
            .origin(None)
            .send()
            .await;
        assert_eq!(accepted.status, 200);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn path_and_query_details_omit_the_value() {
        let app = TestApp::with_routes(Router::new().route("/item/{id}", get(take_path))).await;
        let path = app
            .request(axum::http::Method::GET, "/item/secret-value")
            .send()
            .await;
        assert_eq!(path.status, 400);
        let detail = path.problem().detail.unwrap();
        assert!(!detail.contains("secret-value"), "{detail}");

        let query = app
            .request(axum::http::Method::GET, "/item/1?q=secret-value")
            .send()
            .await;
        assert_eq!(query.status, 400);
        let detail = query.problem().detail.unwrap();
        assert!(detail.contains("invalid query"), "{detail}");
        assert!(!detail.contains("secret-value"), "{detail}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn request_log_uses_the_route_template() {
        let app = TestApp::with_routes(Router::new().route("/echo/{id}", get(echo))).await;
        let response = assert_logs_clean(&["super-secret-token", "secret.example"], {
            app.request(axum::http::Method::GET, "/echo/super-secret-token")
                .origin(Some("https://secret.example"))
                .send()
        })
        .await;
        assert_eq!(response.status, 200);
        let (_body, logs) = capture_logs(
            app.request(axum::http::Method::GET, "/echo/super-secret-token")
                .send(),
        )
        .await;
        assert!(logs.contains("/echo/{id}"), "{logs}");
        assert!(!logs.contains("super-secret-token"), "{logs}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn client_ip_extractor_reads_the_peer() {
        let app = TestApp::with(
            |config| {
                config.trusted_proxies = vec!["10.0.0.0/8".parse().unwrap()];
            },
            Router::new().route("/who", get(who)),
        )
        .await;
        let response = app
            .request(axum::http::Method::GET, "/who")
            .peer("10.0.0.8".parse().unwrap())
            .header("x-forwarded-for", "203.0.113.9, 10.0.0.2")
            .send()
            .await;
        assert_eq!(response.status, 200);
        assert_eq!(response.body.as_ref(), b"203.0.113.9");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn manual_clock_starts_at_the_epoch() {
        let app = TestApp::with_config(|config| {
            assert!(config.trusted_proxies.is_empty());
        })
        .await;
        assert_eq!(app.state.clock.now_ns(), 1_767_225_600_000_000_000);
    }

    #[test]
    fn error_statuses() {
        assert_eq!(status_of(ApiError::Unauthorized), 401);
        assert_eq!(
            status_of(ApiError::Forbidden {
                code: ProblemCode::Forbidden,
            }),
            403
        );
        assert_eq!(
            status_of(ApiError::Forbidden {
                code: ProblemCode::SsoRequired,
            }),
            403
        );
        assert_eq!(
            status_of(ApiError::Forbidden {
                code: ProblemCode::EmailNotVerified,
            }),
            403
        );
        assert_eq!(status_of(ApiError::PlanRequired { detail: "x" }), 403);
        assert_eq!(status_of(ApiError::NotFound), 404);
        assert_eq!(
            status_of(ApiError::Conflict {
                detail: "already exists",
            }),
            409
        );
        assert_eq!(
            status_of(ApiError::PayloadTooLarge {
                detail: "request body is too large",
            }),
            413
        );
        assert_eq!(
            status_of(ApiError::UnsupportedMediaType {
                detail: "unsupported Content-Encoding",
            }),
            415
        );

        let unavailable = ApiError::Unavailable {
            detail: "server is busy, retry later",
        }
        .into_response();
        assert_eq!(unavailable.status(), 503);
        assert_eq!(unavailable.headers().get("retry-after").unwrap(), "1");
        assert!(unavailable.headers().get("ratelimit-limit").is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ingest_problems_carry_their_detail() {
        let cases = [
            (
                ApiError::PayloadTooLarge {
                    detail: "too many spans in one request",
                },
                "payload_too_large",
                "too many spans in one request",
            ),
            (
                ApiError::UnsupportedMediaType {
                    detail: "unsupported Content-Encoding",
                },
                "unsupported_media_type",
                "unsupported Content-Encoding",
            ),
            (
                ApiError::Unavailable {
                    detail: "server is busy, retry later",
                },
                "busy",
                "server is busy, retry later",
            ),
        ];
        for (error, code, detail) in cases {
            let response = error.into_response();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            let problem: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(problem["code"], code);
            assert_eq!(problem["detail"], detail);
        }
    }

    fn status_of(error: ApiError) -> u16 {
        error.into_response().status().as_u16()
    }

    #[test]
    fn rate_limited_sets_the_headers() {
        let response = ApiError::RateLimited { retry_after_s: 12 }.into_response();
        assert_eq!(response.status(), 429);
        let headers = response.headers();
        assert_eq!(headers.get("retry-after").unwrap(), "12");
        assert_eq!(headers.get("ratelimit-limit").unwrap(), "12");
        assert_eq!(headers.get("ratelimit-remaining").unwrap(), "0");
        assert_eq!(headers.get("ratelimit-reset").unwrap(), "12");
    }

    async fn take_json(Json(_value): Json<serde_json::Value>) -> Json<serde_json::Value> {
        Json(serde_json::Value::Null)
    }

    async fn take_path(Path(_id): Path<u32>, Query(_query): Query<ItemQuery>) -> &'static str {
        "ok"
    }

    async fn echo(Path(id): Path<String>) -> String {
        id
    }

    async fn who(ClientIp(ip): ClientIp) -> String {
        ip.to_string()
    }

    #[derive(Deserialize)]
    struct ItemQuery {
        #[allow(dead_code)]
        q: u32,
    }

    fn typed_body_contains(response: &crate::test_support::TestResponse, needle: &str) -> bool {
        String::from_utf8_lossy(&response.body).contains(needle)
    }
}
