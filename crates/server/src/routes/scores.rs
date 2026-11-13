//! Score reads (spec 09 § Scores & annotations).

use axum::extract::{DefaultBodyLimit, Extension, State};
use axum::http::{Method, StatusCode};
use axum::routing::{delete, get, post};
use llmobs_core::score::Score;
use llmobs_storage as storage;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::common::{Page, ProblemCode};
use crate::api::convert;
use crate::api::scores::{ScoreBatch, ScoreListParams, ScoreWriteResult};
use crate::auth::principal::Principal;
use crate::auth::roles::{KeyScope, ProjectPerm};
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::routes::read::{READ_TIMEOUT, open_query, page_limit, window};
use crate::scores::{self as score_writes, Writer};

/// Score batches: up to 1000 items with reasons and metadata (D-G-28).
const SCORE_BODY_LIMIT: usize = 10 * 1024 * 1024;

/// Score routes. Reads need project view; writes need `Annotate` (member and above, D-G-16).
/// The key route is bearer-only (D-G-15). All are documented.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: Method::GET,
            path: "/api/v1/projects/{project}/scores",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(list_scores),
        },
        RouteDef {
            method: Method::POST,
            path: "/api/v1/projects/{project}/scores",
            access: Access::Project {
                perm: ProjectPerm::Annotate,
                session_only: false,
            },
            documented: true,
            handler: post(create_scores).layer(DefaultBodyLimit::max(SCORE_BODY_LIMIT)),
        },
        RouteDef {
            method: Method::DELETE,
            path: "/api/v1/projects/{project}/scores/{score_id}",
            access: Access::Project {
                perm: ProjectPerm::Annotate,
                session_only: false,
            },
            documented: true,
            handler: delete(delete_score),
        },
        RouteDef {
            method: Method::POST,
            path: "/api/v1/scores",
            access: Access::Key {
                scope: KeyScope::Ingest,
            },
            documented: true,
            handler: post(create_key_scores).layer(DefaultBodyLimit::max(SCORE_BODY_LIMIT)),
        },
    ]
}

#[derive(Debug, Deserialize)]
struct ScorePath {
    score_id: Uuid,
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

/// `POST …/scores` with a session or a PAT or project key. The source and author follow the
/// caller (D-G-27).
async fn create_scores(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Json(batch): error::Json<ScoreBatch>,
) -> Result<error::Json<ScoreWriteResult>, ApiError> {
    let (writer, kind) = match principal {
        Principal::Session { user, .. } => (Writer::Human { user_id: user.id }, "human"),
        Principal::Pat { token_id, .. } => (Writer::Api { author: token_id }, "api"),
        Principal::ApiKey { key_id, .. } => (Writer::Api { author: key_id }, "api"),
    };
    write_batch(&state, &access, writer, kind, batch).await
}

/// `DELETE …/scores/{score_id}`: appends a tombstone of the live score (spec 03 §5).
async fn delete_score(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(path): error::Path<ScorePath>,
) -> Result<StatusCode, ApiError> {
    score_writes::delete(&state, &access.project, path.score_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/v1/scores` with an `sk` key. The access layer admits only an ingest key, so the
/// principal is always an `ApiKey`.
async fn create_key_scores(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Json(batch): error::Json<ScoreBatch>,
) -> Result<error::Json<ScoreWriteResult>, ApiError> {
    let Principal::ApiKey { key_id, .. } = principal else {
        return Err(ApiError::Forbidden {
            code: ProblemCode::Forbidden,
        });
    };
    write_batch(&state, &access, Writer::Sdk { key_id }, "sdk", batch).await
}

async fn write_batch(
    state: &AppState,
    access: &ProjectAccess,
    writer: Writer,
    kind: &'static str,
    batch: ScoreBatch,
) -> Result<error::Json<ScoreWriteResult>, ApiError> {
    let count = batch.scores.len();
    let score_ids = score_writes::write(state, &access.project, writer, batch.scores).await?;
    tracing::info!(project_id = %access.project.id, count, writer = kind, "scores written");
    Ok(error::Json(ScoreWriteResult { score_ids }))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::str::FromStr;

    use axum::http::{Method, StatusCode};
    use llmobs_core::enums::ScoreSource;
    use llmobs_core::ids::{SpanId, TraceId};
    use llmobs_core::score::{ScoreTarget, ScoreValue};
    use serde_json::Value as Json;

    use super::*;
    use crate::api::scores::ScoreCreate;
    use crate::meta::users;
    use crate::test_support::{Seeded, TestApp};

    /// The SDK example of `fixtures/api/requests/score-batch.json`.
    const SCORE_BATCH: &[u8] = include_bytes!("../../../../fixtures/api/requests/score-batch.json");

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

    /// A span score on the example trace, with no `score_id` (derived).
    fn span_score(name: &str, value: f64) -> ScoreCreate {
        ScoreCreate {
            score_id: None,
            target: ScoreTarget::Span {
                trace_id: TraceId::from_str("a1000000000000000000000000000001").unwrap(),
                span_id: SpanId::from_str("b100000000000012").unwrap(),
            },
            session_id: None,
            user_id: None,
            name: name.to_owned(),
            value: ScoreValue::Numeric(value),
            reason: None,
            source: None,
            metadata: BTreeMap::new(),
        }
    }

    /// The live scores of a project without loaded examples, newest first.
    async fn listed(app: &TestApp, seeded: &Seeded) -> Vec<Score> {
        let (status, body) = get_json(app, seeded, "/scores").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let page: Page<Score> = serde_json::from_value(body).unwrap();
        page.data
    }

    fn scores_path(seeded: &Seeded) -> String {
        format!("/api/v1/projects/{}/scores", seeded.project.id)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sdk_batch_is_stored_with_the_sdk_source() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let response = app
            .request(Method::POST, "/api/v1/scores")
            .header("authorization", &format!("Bearer {}", seeded.ingest_key))
            .header("content-type", "application/json")
            .body(SCORE_BATCH)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        let answer: ScoreWriteResult = response.json();
        assert_eq!(answer.score_ids.len(), 1);
        assert_eq!(answer.score_ids[0].to_string(), score_id(401));

        let rows = listed(&app, &seeded).await;
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.score_id.to_string(), score_id(401));
        assert_eq!(row.source, ScoreSource::Sdk);
        assert_eq!(row.value, ScoreValue::Numeric(0.8));
        assert_eq!(row.reason.as_deref(), Some("Matches the reference answer."));
        assert_eq!(row.user_id.as_deref(), Some("user-42"));
        assert_eq!(
            row.metadata,
            BTreeMap::from([("k".to_owned(), "v".to_owned())])
        );
        assert_eq!(row.author_user_id, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn key_route_refuses_other_credentials() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let batch = || {
            app.request(Method::POST, "/api/v1/scores")
                .header("content-type", "application/json")
                .body(SCORE_BATCH)
        };
        let anonymous = batch().send().await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
        let session = batch().header("cookie", &seeded.cookie).send().await;
        assert_eq!(session.status, StatusCode::FORBIDDEN);
        let read = batch()
            .header("authorization", &format!("Bearer {}", seeded.read_key))
            .send()
            .await;
        assert_eq!(read.status, StatusCode::FORBIDDEN);
        let public = batch()
            .header("authorization", &format!("Bearer {}", seeded.public_key))
            .send()
            .await;
        assert_eq!(public.status, StatusCode::FORBIDDEN);
        assert!(listed(&app, &seeded).await.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn session_write_is_stored_as_human_and_read_key_is_refused() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let ada = users::user_by_email(app.state.meta.reader(), "ada@example.com")
            .await
            .unwrap()
            .expect("user exists");
        let batch = ScoreBatch {
            scores: vec![span_score("tone", 1.0)],
        };
        let response = app
            .request(Method::POST, &scores_path(&seeded))
            .header("cookie", &seeded.cookie)
            .json(&batch)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        let answer: ScoreWriteResult = response.json();
        assert_eq!(answer.score_ids.len(), 1);

        let rows = listed(&app, &seeded).await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].source, ScoreSource::Human);
        assert_eq!(rows[0].author_user_id, Some(ada.id));

        let read = app
            .request(Method::POST, &scores_path(&seeded))
            .header("authorization", &format!("Bearer {}", seeded.read_key))
            .json(&batch)
            .send()
            .await;
        assert_eq!(read.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_hides_the_score_and_refuses_unknown_ids() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let id = "01900000-0000-7000-8000-000000000402";
        let batch = ScoreBatch {
            scores: vec![ScoreCreate {
                score_id: Some(id.parse().unwrap()),
                ..span_score("tone", 1.0)
            }],
        };
        let written = app
            .request(Method::POST, &scores_path(&seeded))
            .header("cookie", &seeded.cookie)
            .json(&batch)
            .send()
            .await;
        assert_eq!(written.status, StatusCode::OK);
        assert_eq!(listed(&app, &seeded).await.len(), 1);

        let delete_path = format!("{}/{id}", scores_path(&seeded));
        let deleted = app
            .request(Method::DELETE, &delete_path)
            .header("cookie", &seeded.cookie)
            .send()
            .await;
        assert_eq!(deleted.status, StatusCode::NO_CONTENT);
        assert!(listed(&app, &seeded).await.is_empty());

        let again = app
            .request(Method::DELETE, &delete_path)
            .header("cookie", &seeded.cookie)
            .send()
            .await;
        assert_eq!(again.status, StatusCode::NOT_FOUND);
        let malformed = app
            .request(Method::DELETE, &format!("{}/nope", scores_path(&seeded)))
            .header("cookie", &seeded.cookie)
            .send()
            .await;
        assert_eq!(malformed.status, StatusCode::BAD_REQUEST);
        let read = app
            .request(Method::DELETE, &delete_path)
            .header("authorization", &format!("Bearer {}", seeded.read_key))
            .send()
            .await;
        assert_eq!(read.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn score_batches_have_their_own_body_limit() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        // About 2 MiB: over the router's 1 MiB default, under this route's 10 MiB.
        let scores: Vec<ScoreCreate> = (0..1000)
            .map(|index| ScoreCreate {
                reason: Some("r".repeat(2048)),
                ..span_score(&format!("score-{index}"), 0.5)
            })
            .collect();
        let accepted = app
            .request(Method::POST, &scores_path(&seeded))
            .header("cookie", &seeded.cookie)
            .json(&ScoreBatch { scores })
            .send()
            .await;
        assert_eq!(accepted.status, StatusCode::OK);
        let answer: ScoreWriteResult = accepted.json();
        assert_eq!(answer.score_ids.len(), 1000);

        let oversized = ScoreBatch {
            scores: vec![ScoreCreate {
                reason: Some("r".repeat(11 * 1024 * 1024)),
                ..span_score("tone", 0.5)
            }],
        };
        let refused = app
            .request(Method::POST, &scores_path(&seeded))
            .header("cookie", &seeded.cookie)
            .json(&oversized)
            .send()
            .await;
        assert_eq!(refused.status, StatusCode::PAYLOAD_TOO_LARGE);
    }
}
