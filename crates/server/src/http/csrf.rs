//! Origin check for cookie-authenticated writes (D141).
//!
//! Bearer requests skip the check. They have no `Authorization` header in P10,
//! because that header is refused before this layer runs.

use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::Next;
use axum::response::Response;

use crate::api::common::{Problem, ProblemCode};
use crate::auth::principal::Principal;
use crate::http::AppState;
use crate::http::error;

/// Requires `Origin` to equal `public_url`'s origin on state-changing requests
/// that use a session, and on every state-changing `/api/auth/` request.
pub(crate) async fn csrf(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if skips_origin(request.method()) || !must_check(&request) {
        return next.run(request).await;
    }
    let origin = request
        .headers()
        .get(axum::http::header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    if origin == Some(state.config.public_url.origin()) {
        return next.run(request).await;
    }
    error::problem_response(&Problem::new(
        ProblemCode::Forbidden,
        Some("origin check failed".to_owned()),
    ))
}

fn skips_origin(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

fn must_check(request: &Request) -> bool {
    let session = request
        .extensions()
        .get::<Option<Principal>>()
        .and_then(Option::as_ref)
        .is_some_and(|principal| matches!(principal, Principal::Session { .. }));
    session || request.uri().path().starts_with("/api/auth/")
}
