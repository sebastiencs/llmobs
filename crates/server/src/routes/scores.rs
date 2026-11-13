//! Score reads (spec 09 § Scores & annotations).
#![cfg_attr(not(test), allow(dead_code))]

use axum::extract::{Extension, State};
use axum::http::Method;
use axum::routing::get;
use llmobs_core::score::Score;
use llmobs_storage as storage;

use crate::api::common::Page;
use crate::api::convert;
use crate::api::scores::ScoreListParams;
use crate::auth::roles::ProjectPerm;
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::routes::read::{READ_TIMEOUT, open_query, page_limit, window};

/// Score read routes. Documented and need project view.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![RouteDef {
        method: Method::GET,
        path: "/api/v1/projects/{project}/scores",
        access: Access::Project {
            perm: ProjectPerm::View,
            session_only: false,
        },
        documented: true,
        handler: get(list_scores),
    }]
}

async fn list_scores(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Query(params): error::Query<ScoreListParams>,
) -> Result<error::Json<Page<Score>>, ApiError> {
    let trace_id = params.trace_id.map(|id| id.to_string());
    let span_id = params.span_id.map(|id| id.to_string());
    let created = window(params.from, params.to)?;
    let ctx = open_query(&state, access.project.id, None).await?;
    let page = storage::score_list(
        &ctx,
        storage::ScoreListRequest {
            target: params.target,
            trace_id: trace_id.as_deref(),
            span_id: span_id.as_deref(),
            session_id: params.session_id.as_deref(),
            name: params.name.as_deref(),
            source: params.source,
            window: created,
            limit: page_limit(params.limit),
            cursor: params.cursor.as_deref(),
            timeout: READ_TIMEOUT,
        },
    )
    .await?;
    Ok(error::Json(Page {
        data: convert::scores(&page.batches, access.project.id)?,
        next_cursor: page.next_cursor,
    }))
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use axum::http::{Method, StatusCode};
    use llmobs_core::ids::{SpanId, TraceId};
    use llmobs_core::score::{ScoreTarget, ScoreValue};
    use serde_json::Value as Json;

    use super::*;
    use crate::test_support::{Seeded, TestApp};

    /// A seeded app with the example dataset loaded.
    async fn loaded() -> (TestApp, Seeded) {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        app.load_example_dataset(seeded.project.id).await;
        (app, seeded)
    }

    async fn get_json(app: &TestApp, seeded: &Seeded, path: &str) -> (StatusCode, Json) {
        let response = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}{path}", seeded.project.id),
            )
            .header("cookie", &seeded.cookie)
            .send()
            .await;
        let body = response.json::<Json>();
        (response.status, body)
    }

    /// `301` … `306` name the example dataset's `…0301` … `…0306`.
    fn score_id(number: u32) -> String {
        format!("01900000-0000-7000-8000-000000000{number}")
    }

    fn ids(page: &Page<Score>) -> Vec<String> {
        page.data.iter().map(|s| s.score_id.to_string()).collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_is_newest_first() {
        let (app, seeded) = loaded().await;
        let (status, body) = get_json(&app, &seeded, "/scores").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let page: Page<Score> = serde_json::from_value(body).unwrap();
        let expected: Vec<String> = [306, 305, 304, 303, 302, 301]
            .into_iter()
            .map(score_id)
            .collect();
        assert_eq!(ids(&page), expected);
        assert!(page.data.iter().all(|s| s.project_id == seeded.project.id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn targets_and_values_are_converted() {
        let (app, seeded) = loaded().await;
        let (_, body) = get_json(&app, &seeded, "/scores").await;
        let page: Page<Score> = serde_json::from_value(body).unwrap();
        let by_id = |number: u32| {
            page.data
                .iter()
                .find(|s| s.score_id.to_string() == score_id(number))
                .unwrap()
        };

        let session = by_id(305);
        assert_eq!(
            session.target,
            ScoreTarget::Session {
                session_id: "sess-001".to_owned(),
            }
        );
        assert_eq!(session.session_id, None);

        let span = by_id(303);
        assert_eq!(
            span.target,
            ScoreTarget::Span {
                trace_id: TraceId::from_str("a1000000000000000000000000000001").unwrap(),
                span_id: SpanId::from_str("b100000000000014").unwrap(),
            }
        );
        assert_eq!(
            span.value,
            ScoreValue::Categorical {
                label: "friendly".to_owned(),
                value: Some(1.0),
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn name_and_target_filters() {
        let (app, seeded) = loaded().await;
        let (_, body) = get_json(&app, &seeded, "/scores?name=helpfulness").await;
        let page: Page<Score> = serde_json::from_value(body).unwrap();
        assert_eq!(ids(&page), [score_id(302), score_id(301)]);

        let (_, body) = get_json(&app, &seeded, "/scores?target=session").await;
        let page: Page<Score> = serde_json::from_value(body).unwrap();
        assert_eq!(ids(&page), [score_id(305)]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pages_follow_the_cursor() {
        let (app, seeded) = loaded().await;
        let (status, body) = get_json(&app, &seeded, "/scores?limit=4").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let first: Page<Score> = serde_json::from_value(body).unwrap();
        assert_eq!(first.data.len(), 4);
        let cursor = first.next_cursor.unwrap();

        let query =
            serde_urlencoded::to_string([("limit", "4"), ("cursor", cursor.as_str())]).unwrap();
        let (status, body) = get_json(&app, &seeded, &format!("/scores?{query}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let second: Page<Score> = serde_json::from_value(body).unwrap();
        assert_eq!(ids(&second), [score_id(302), score_id(301)]);
        assert!(second.next_cursor.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_parameter_is_bad_request() {
        let (app, seeded) = loaded().await;
        let (status, body) = get_json(&app, &seeded, "/scores?target=bogus").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "bad_request");
    }
}
