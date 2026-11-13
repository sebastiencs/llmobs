//! Public end-user feedback (spec 06 § End-user feedback; D-F-16, D-G-15, D-G-16).
//!
//! `POST /api/public/feedback` takes a `pk` key with the `feedback` scope, from any origin.
//! Every answer of the handler carries `Access-Control-Allow-Origin: *`. Refusals of the
//! access layer (401, 403) and the 4 KiB body limit (413) carry no CORS header, so the browser
//! sees a network failure and `feedback()` resolves `{ ok: false }`, which is its contract (D-F-16).

use std::collections::BTreeMap;
use std::net::IpAddr;

use axum::extract::{DefaultBodyLimit, Extension, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{options, post};
use llmobs_core::enums::ScoreSource;
use llmobs_core::score::{ScoreTarget, ScoreValue};

use crate::api::common::ProblemCode;
use crate::api::scores::{FeedbackRequest, FeedbackResult, FeedbackValue, ScoreCreate};
use crate::auth::principal::Principal;
use crate::auth::roles::KeyScope;
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::client_ip::ClientIp;
use crate::http::error::{self, ApiError};
use crate::meta::projects::ProjectRow;
use crate::scores::{self, Writer};

/// The feedback body limit, 4 KiB (D-G-16).
const FEEDBACK_BODY_LIMIT: usize = 4096;
/// The name of feedback that gives none.
const DEFAULT_NAME: &str = "user_feedback";

/// Feedback routes. The `POST` takes a `pk` key with `feedback`; the `OPTIONS` is the CORS
/// preflight and is not documented.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: Method::POST,
            path: "/api/public/feedback",
            access: Access::Key {
                scope: KeyScope::Feedback,
            },
            documented: true,
            handler: post(post_feedback).layer(DefaultBodyLimit::max(FEEDBACK_BODY_LIMIT)),
        },
        RouteDef {
            method: Method::OPTIONS,
            path: "/api/public/feedback",
            access: Access::Public,
            documented: false,
            handler: options(preflight),
        },
    ]
}

/// `POST /api/public/feedback`. The access layer admits only a `pk` key with `feedback`.
async fn post_feedback(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    ClientIp(ip): ClientIp,
    error::Json(body): error::Json<FeedbackRequest>,
) -> Response {
    let answer = feedback(&state, &principal, &access.project, ip, body).await;
    let mut response = match answer {
        Ok(result) => error::Json(result).into_response(),
        Err(error) => error.into_response(),
    };
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

/// Rate-limits per (key, client IP), then writes the feedback as one `end_user` score.
async fn feedback(
    state: &AppState,
    principal: &Principal,
    project: &ProjectRow,
    ip: IpAddr,
    body: FeedbackRequest,
) -> Result<FeedbackResult, ApiError> {
    let Principal::ApiKey { key_id, .. } = principal else {
        return Err(ApiError::Forbidden {
            code: ProblemCode::Forbidden,
        });
    };
    let now = state.clock.now_ns();
    state
        .limits
        .feedback
        .check(&(*key_id, ip), now)
        .map_err(|retry| ApiError::RateLimited {
            retry_after_s: retry.0.as_secs().max(1),
        })?;
    let ids = scores::write(
        state,
        project,
        Writer::EndUser { key_id: *key_id },
        vec![score_of(body)],
    )
    .await
    .map_err(rewrite_fields)?;
    let score_id = ids.first().copied().ok_or_else(|| {
        ApiError::Internal(Box::new(std::io::Error::other(
            "score write answered no id",
        )))
    })?;
    Ok(FeedbackResult { score_id })
}

/// The one score a feedback body stands for (spec 06 § End-user feedback). The id is derived.
fn score_of(body: FeedbackRequest) -> ScoreCreate {
    let target = match body.span_id {
        Some(span_id) => ScoreTarget::Span {
            trace_id: body.trace_id,
            span_id,
        },
        None => ScoreTarget::Trace {
            trace_id: body.trace_id,
        },
    };
    let value = match body.value {
        FeedbackValue::Boolean(value) => ScoreValue::Boolean(value),
        FeedbackValue::Number(value) => ScoreValue::Numeric(value),
        FeedbackValue::Text(label) => ScoreValue::Categorical { label, value: None },
    };
    ScoreCreate {
        score_id: None,
        target,
        session_id: None,
        user_id: None,
        name: body.name.unwrap_or_else(|| DEFAULT_NAME.to_owned()),
        value,
        reason: body.comment,
        source: Some(ScoreSource::EndUser),
        metadata: BTreeMap::new(),
    }
}

/// The write names its fields `scores[0].<field>`; the feedback body has no `scores`, so the
/// prefix is dropped.
fn rewrite_fields(error: ApiError) -> ApiError {
    match error {
        ApiError::BadRequest { detail, errors } => ApiError::BadRequest {
            detail,
            errors: errors
                .into_iter()
                .map(|mut error| {
                    let rewritten = error.field.strip_prefix("scores[0].").map(str::to_owned);
                    if let Some(field) = rewritten {
                        error.field = field;
                    }
                    error
                })
                .collect(),
        },
        other => other,
    }
}

/// `OPTIONS /api/public/feedback`: the CORS preflight. It needs no credentials.
async fn preflight() -> Response {
    (
        StatusCode::NO_CONTENT,
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::ACCESS_CONTROL_ALLOW_METHODS, "POST, OPTIONS"),
            (
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                "Authorization, Content-Type",
            ),
            (header::ACCESS_CONTROL_MAX_AGE, "600"),
        ],
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;
    use std::str::FromStr;

    use axum::http::{Method, StatusCode};
    use llmobs_core::ids::TraceId;
    use llmobs_core::score::Score;

    use super::*;
    use crate::api::common::Page;
    use crate::test_support::{Seeded, TestApp, TestResponse, assert_logs_clean};

    /// The example of `fixtures/api/requests/feedback.json`.
    const FEEDBACK: &[u8] = include_bytes!("../../../../fixtures/api/requests/feedback.json");
    const TRACE: &str = "a1000000000000000000000000000001";
    const SPAN: &str = "b100000000000012";

    fn trace_target() -> ScoreTarget {
        ScoreTarget::Trace {
            trace_id: TraceId::from_str(TRACE).unwrap(),
        }
    }

    async fn post_feedback_as(app: &TestApp, bearer: &str, body: Vec<u8>) -> TestResponse {
        app.request(Method::POST, "/api/public/feedback")
            .header("authorization", &format!("Bearer {bearer}"))
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
    }

    /// Every live score of the seeded project, through the M2 list.
    async fn stored(app: &TestApp, seeded: &Seeded) -> Vec<Score> {
        let response = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}/scores", seeded.project.id),
            )
            .header("cookie", &seeded.cookie)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        response.json::<Page<Score>>().data
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_example_feedback_is_stored_with_cors() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let response = post_feedback_as(&app, &seeded.public_key, FEEDBACK.to_vec()).await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.header("access-control-allow-origin"), Some("*"));
        let answer: FeedbackResult = response.json();

        let rows = stored(&app, &seeded).await;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.score_id, answer.score_id);
        assert_eq!(row.target, trace_target());
        assert_eq!(row.name, "user_feedback");
        assert_eq!(row.value, ScoreValue::Boolean(true));
        assert_eq!(row.reason.as_deref(), Some("Helpful."));
        assert_eq!(row.source, ScoreSource::EndUser);
        assert_eq!(row.author_user_id, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_same_body_twice_is_one_live_score() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let first: FeedbackResult = post_feedback_as(&app, &seeded.public_key, FEEDBACK.to_vec())
            .await
            .json();
        let second: FeedbackResult = post_feedback_as(&app, &seeded.public_key, FEEDBACK.to_vec())
            .await
            .json();
        assert_eq!(first.score_id, second.score_id);
        assert_eq!(first.score_id.get_version_num(), 8);
        assert_eq!(stored(&app, &seeded).await.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn text_on_a_span_is_a_categorical_score() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let body = format!(
            r#"{{"value": "great", "trace_id": "{TRACE}", "span_id": "{SPAN}", "name": "rating"}}"#
        );
        let response = post_feedback_as(&app, &seeded.public_key, body.into_bytes()).await;
        assert_eq!(response.status, StatusCode::OK);

        let rows = stored(&app, &seeded).await;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.name, "rating");
        assert_eq!(
            row.target,
            ScoreTarget::Span {
                trace_id: TraceId::from_str(TRACE).unwrap(),
                span_id: SPAN.parse().unwrap(),
            }
        );
        assert_eq!(
            row.value,
            ScoreValue::Categorical {
                label: "great".to_owned(),
                value: None,
            }
        );
        assert_eq!(row.reason, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn preflight_needs_no_credentials() {
        let app = TestApp::new().await;
        let response = app
            .request(Method::OPTIONS, "/api/public/feedback")
            .send()
            .await;
        assert_eq!(response.status, StatusCode::NO_CONTENT);
        assert_eq!(response.header("access-control-allow-origin"), Some("*"));
        assert_eq!(
            response.header("access-control-allow-methods"),
            Some("POST, OPTIONS")
        );
        assert_eq!(
            response.header("access-control-allow-headers"),
            Some("Authorization, Content-Type")
        );
        assert_eq!(response.header("access-control-max-age"), Some("600"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ingest_key_and_anonymous_are_refused_without_cors() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let ingest = post_feedback_as(&app, &seeded.ingest_key, FEEDBACK.to_vec()).await;
        assert_eq!(ingest.status, StatusCode::FORBIDDEN);
        assert_eq!(ingest.header("access-control-allow-origin"), None);

        let anonymous = app
            .request(Method::POST, "/api/public/feedback")
            .header("content-type", "application/json")
            .body(FEEDBACK.to_vec())
            .send()
            .await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
        assert_eq!(anonymous.header("access-control-allow-origin"), None);
        assert!(stored(&app, &seeded).await.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_body_over_four_kib_is_too_large_without_cors() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let body = format!(
            r#"{{"trace_id": "{TRACE}", "value": true, "comment": "{}"}}"#,
            "c".repeat(5000)
        );
        let response = post_feedback_as(&app, &seeded.public_key, body.into_bytes()).await;
        assert_eq!(response.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(response.header("access-control-allow-origin"), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_body_keeps_cors_and_names_the_feedback_field() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let body = format!(r#"{{"trace_id": "{TRACE}", "value": true, "name": ""}}"#);
        let response = post_feedback_as(&app, &seeded.public_key, body.into_bytes()).await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert_eq!(response.header("access-control-allow-origin"), Some("*"));
        let problem: serde_json::Value = response.json();
        assert_eq!(problem["errors"][0]["field"], "name");
        assert!(stored(&app, &seeded).await.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn eleventh_request_in_a_second_is_limited_per_client_ip() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        for _ in 0..10 {
            let response = post_feedback_as(&app, &seeded.public_key, FEEDBACK.to_vec()).await;
            assert_eq!(response.status, StatusCode::OK);
        }
        let limited = post_feedback_as(&app, &seeded.public_key, FEEDBACK.to_vec()).await;
        assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(limited.header("retry-after"), Some("1"));
        assert_eq!(limited.header("access-control-allow-origin"), Some("*"));

        let other_ip = IpAddr::from_str("203.0.113.7").unwrap();
        let other = app
            .request(Method::POST, "/api/public/feedback")
            .header("authorization", &format!("Bearer {}", seeded.public_key))
            .header("content-type", "application/json")
            .peer(other_ip)
            .body(FEEDBACK.to_vec())
            .send()
            .await;
        assert_eq!(other.status, StatusCode::OK);
    }

    /// Log capture is per thread, so this test runs on the current-thread runtime.
    #[tokio::test]
    async fn the_feedback_comment_is_not_logged() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let response = assert_logs_clean(&["Helpful."], async {
            post_feedback_as(&app, &seeded.public_key, FEEDBACK.to_vec()).await
        })
        .await;
        assert_eq!(response.status, StatusCode::OK);
    }
}
