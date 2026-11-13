//! Trace and span reads (spec 09 § Observability data).

use axum::extract::{Extension, State};
use axum::http::Method;
use axum::routing::get;
use llmobs_storage as storage;
use serde::Deserialize;

use crate::api::common::{Page, TimeParam};
use crate::api::convert;
use crate::api::traces::{
    SpanDetail, SpanListParams, SpanSort, SpanSummary, TraceDetail, TraceDetailParams,
    TraceListParams, TraceSort, TraceSummary, build_tree,
};
use crate::auth::roles::ProjectPerm;
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::routes::read::{
    READ_TIMEOUT, attach_trace_scores, list_scope, open_query, page_limit, sort_order,
};

/// Trace and span read routes. All are documented and need project view.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: Method::GET,
            path: "/api/v1/projects/{project}/traces",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(list_traces),
        },
        RouteDef {
            method: Method::GET,
            path: "/api/v1/projects/{project}/traces/{trace_id}",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(get_trace),
        },
        RouteDef {
            method: Method::GET,
            path: "/api/v1/projects/{project}/traces/{trace_id}/spans/{span_id}",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(get_span),
        },
        RouteDef {
            method: Method::GET,
            path: "/api/v1/projects/{project}/spans",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(list_spans),
        },
    ]
}

#[derive(Debug, Deserialize)]
struct TracePath {
    trace_id: String,
}

#[derive(Debug, Deserialize)]
struct SpanPath {
    trace_id: String,
    span_id: String,
}

async fn list_traces(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Query(params): error::Query<TraceListParams>,
) -> Result<error::Json<Page<TraceSummary>>, ApiError> {
    let scope = list_scope(
        params.from,
        params.to,
        params.include_eval,
        params.include_experiments,
    )?;
    let ctx = open_query(&state, access.project.id, None).await?;
    let page = storage::trace_list(
        &ctx,
        storage::TraceListRequest {
            filter: params.filter.as_deref(),
            search: params.search.as_deref(),
            scope,
            sort: trace_sort(params.sort),
            order: sort_order(params.order),
            limit: page_limit(params.limit),
            cursor: params.cursor.as_deref(),
            timeout: READ_TIMEOUT,
        },
    )
    .await?;
    let mut data = convert::trace_summaries(&page.batches)?;
    attach_trace_scores(&ctx, &mut data).await?;
    Ok(error::Json(Page {
        data,
        next_cursor: page.next_cursor,
    }))
}

async fn list_spans(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Query(params): error::Query<SpanListParams>,
) -> Result<error::Json<Page<SpanSummary>>, ApiError> {
    let scope = list_scope(
        params.from,
        params.to,
        params.include_eval,
        params.include_experiments,
    )?;
    let ctx = open_query(&state, access.project.id, None).await?;
    let page = storage::span_list(
        &ctx,
        storage::SpanListRequest {
            filter: params.filter.as_deref(),
            search: params.search.as_deref(),
            scope,
            sort: span_sort(params.sort),
            order: sort_order(params.order),
            limit: page_limit(params.limit),
            cursor: params.cursor.as_deref(),
            timeout: READ_TIMEOUT,
        },
    )
    .await?;
    Ok(error::Json(Page {
        data: convert::span_summaries(&page.batches)?,
        next_cursor: page.next_cursor,
    }))
}

async fn get_trace(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(path): error::Path<TracePath>,
    error::Query(params): error::Query<TraceDetailParams>,
) -> Result<error::Json<TraceDetail>, ApiError> {
    let around = params.around.map(TimeParam::ns);
    let ctx = open_query(&state, access.project.id, around).await?;
    let data = storage::trace_detail(&ctx, &path.trace_id, READ_TIMEOUT)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut trace = convert::trace_summaries(std::slice::from_ref(&data.trace))?
        .pop()
        .ok_or(ApiError::NotFound)?;
    attach_trace_scores(&ctx, std::slice::from_mut(&mut trace)).await?;
    let spans = build_tree(convert::span_summaries(&data.spans)?);
    Ok(error::Json(TraceDetail {
        trace,
        spans,
        truncated: data.truncated,
    }))
}

async fn get_span(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(path): error::Path<SpanPath>,
) -> Result<error::Json<SpanDetail>, ApiError> {
    let ctx = open_query(&state, access.project.id, None).await?;
    let batch = storage::span_detail(&ctx, &path.trace_id, &path.span_id, READ_TIMEOUT)
        .await?
        .ok_or(ApiError::NotFound)?;
    let span = convert::span(&batch, access.project.id)?;
    Ok(error::Json(SpanDetail::from(span)))
}

fn trace_sort(sort: TraceSort) -> storage::TraceSort {
    match sort {
        TraceSort::StartTime => storage::TraceSort::StartTime,
        TraceSort::Duration => storage::TraceSort::Duration,
        TraceSort::CostTotal => storage::TraceSort::CostTotal,
        TraceSort::TotalTokens => storage::TraceSort::TotalTokens,
    }
}

fn span_sort(sort: SpanSort) -> storage::SpanSort {
    match sort {
        SpanSort::StartTime => storage::SpanSort::StartTime,
        SpanSort::Duration => storage::SpanSort::Duration,
        SpanSort::CostTotal => storage::SpanSort::CostTotal,
        SpanSort::TotalTokens => storage::SpanSort::TotalTokens,
    }
}

#[cfg(test)]
mod tests {
    use axum::http::{Method, StatusCode};
    use llmobs_core::message::Part;
    use llmobs_core::score::ScoreValue;
    use serde_json::Value as Json;

    use super::*;
    use crate::api::common::Problem;
    use crate::api::payload::Payload;
    use crate::test_support::{Seeded, TestApp, assert_logs_clean};

    const TRACE_1: &str = "a1000000000000000000000000000001";
    const TRACE_2: &str = "a1000000000000000000000000000002";
    const TRACE_3: &str = "a1000000000000000000000000000003";
    const TRACE_4: &str = "a1000000000000000000000000000004";
    const TRACE_6: &str = "a1000000000000000000000000000006";

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

    async fn trace_ids(app: &TestApp, seeded: &Seeded, query: &str) -> Vec<String> {
        let (status, body) = get_json(app, seeded, &format!("/traces{query}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|trace| trace["trace_id"].as_str().unwrap().to_owned())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn default_list_is_newest_first_with_latest_scores() {
        let (app, seeded) = loaded().await;
        let response = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}/traces", seeded.project.id),
            )
            .header("cookie", &seeded.cookie)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        let page: Page<TraceSummary> = response.json();
        assert_eq!(page.next_cursor, None);
        let ids: Vec<String> = page.data.iter().map(|t| t.trace_id.to_string()).collect();
        assert_eq!(ids, [TRACE_6, TRACE_4, TRACE_3, TRACE_2, TRACE_1]);
        let trace_1 = &page.data[4];
        assert_eq!(trace_1.scores.len(), 1);
        assert_eq!(trace_1.scores[0].name, "helpfulness");
        assert_eq!(trace_1.scores[0].value, ScoreValue::Numeric(0.9));
        let trace_2 = &page.data[3];
        assert_eq!(trace_2.scores[0].name, "thumbs");
        assert_eq!(trace_2.scores[0].value, ScoreValue::Boolean(true));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn include_eval_adds_the_evaluation_trace() {
        let (app, seeded) = loaded().await;
        let ids = trace_ids(&app, &seeded, "?include_eval=true").await;
        assert_eq!(
            ids,
            [
                TRACE_6,
                TRACE_4,
                TRACE_3,
                TRACE_2,
                "a1000000000000000000000000000005",
                TRACE_1
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cursors_walk_every_trace_once() {
        let (app, seeded) = loaded().await;
        let (_, first) = get_json(&app, &seeded, "/traces?limit=2").await;
        assert!(first["next_cursor"].is_string());
        let mut ids = Vec::new();
        let mut cursor = Some(first["next_cursor"].as_str().unwrap().to_owned());
        ids.extend(
            first["data"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["trace_id"].as_str().unwrap().to_owned()),
        );
        while let Some(current) = cursor.take() {
            let (status, body) =
                get_json(&app, &seeded, &format!("/traces?limit=2&cursor={current}")).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            ids.extend(
                body["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|t| t["trace_id"].as_str().unwrap().to_owned()),
            );
            cursor = body["next_cursor"].as_str().map(str::to_owned);
        }
        assert_eq!(ids, [TRACE_6, TRACE_4, TRACE_3, TRACE_2, TRACE_1]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn filter_selects_matching_traces() {
        let (app, seeded) = loaded().await;
        let ids = trace_ids(&app, &seeded, "?filter=name%20%3D%20'answer-question'").await;
        assert_eq!(ids, [TRACE_3]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_filter_is_a_bad_request_naming_the_filter_and_is_not_logged() {
        let (app, seeded) = loaded().await;
        let body = assert_logs_clean(&["needle-7f3a"], async {
            app.request(
                Method::GET,
                &format!(
                    "/api/v1/projects/{}/traces?filter=name%20%3D%20'needle-7f3a'%20)",
                    seeded.project.id
                ),
            )
            .header("cookie", &seeded.cookie)
            .send()
            .await
        })
        .await;
        assert_eq!(body.status, StatusCode::BAD_REQUEST);
        let problem: Problem = body.problem();
        let json = body.json::<Json>();
        assert_eq!(json["code"], "invalid_filter");
        assert_eq!(problem.errors[0].field, "filter");
        assert!(!problem.errors[0].message.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn window_bounds_the_list_and_reversed_window_is_rejected() {
        let (app, seeded) = loaded().await;
        let ids = trace_ids(
            &app,
            &seeded,
            "?from=2026-01-01T10:15:00Z&to=2026-01-01T11:00:00Z",
        )
        .await;
        assert_eq!(ids, [TRACE_6, TRACE_4]);
        let (status, body) = get_json(
            &app,
            &seeded,
            "/traces?from=2026-01-01T11:00:00Z&to=2026-01-01T10:00:00Z",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "bad_request");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sort_by_cost_puts_null_last() {
        let (app, seeded) = loaded().await;
        let ids = trace_ids(&app, &seeded, "?sort=cost_total&order=asc").await;
        assert_eq!(ids, [TRACE_4, TRACE_2, TRACE_6, TRACE_1, TRACE_3]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn trace_detail_builds_the_tree() {
        let (app, seeded) = loaded().await;
        for id in [TRACE_1.to_owned(), TRACE_1.to_uppercase()] {
            let (status, body) = get_json(&app, &seeded, &format!("/traces/{id}")).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let detail: TraceDetail = serde_json::from_value(body).unwrap();
            assert_eq!(detail.trace.trace_id.to_string(), TRACE_1);
            let ids: Vec<String> = detail
                .spans
                .iter()
                .map(|s| s.span.span_id.to_string())
                .collect();
            assert_eq!(
                ids,
                [
                    "b100000000000011",
                    "b100000000000012",
                    "b100000000000013",
                    "b100000000000014"
                ]
            );
            let depths: Vec<u32> = detail.spans.iter().map(|s| s.depth).collect();
            assert_eq!(depths, [0, 1, 1, 1]);
            assert!(detail.spans.iter().all(|s| !s.detached));
            assert!(!detail.truncated);
            assert_eq!(detail.trace.scores.len(), 1);
            assert_eq!(detail.trace.scores[0].value, ScoreValue::Numeric(0.9));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn detached_span_is_its_own_root() {
        let (app, seeded) = loaded().await;
        let (status, body) = get_json(&app, &seeded, &format!("/traces/{TRACE_6}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let detail: TraceDetail = serde_json::from_value(body).unwrap();
        assert_eq!(detail.spans.len(), 1);
        assert!(detail.spans[0].detached);
        assert!(!detail.trace.has_root);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_trace_is_not_found_and_malformed_id_is_bad_request() {
        let (app, seeded) = loaded().await;
        let (status, body) =
            get_json(&app, &seeded, "/traces/a1000000000000000000000000000009").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["code"], "not_found");
        let (status, body) = get_json(&app, &seeded, "/traces/zz").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "bad_request");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn span_list_excludes_evaluation_spans_by_default() {
        let (app, seeded) = loaded().await;
        let (_, body) = get_json(&app, &seeded, "/spans").await;
        assert_eq!(body["data"].as_array().unwrap().len(), 12);
        assert_eq!(body["data"][0]["span_id"], "b100000000000061");
        let (_, body) = get_json(&app, &seeded, "/spans?include_eval=true").await;
        assert_eq!(body["data"].as_array().unwrap().len(), 14);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn span_detail_carries_events_links_and_maps() {
        let (app, seeded) = loaded().await;
        let mut span: llmobs_core::span::Span = serde_json::from_value(serde_json::json!({
            "project_id": seeded.project.id.to_string(),
            "trace_id": "a1000000000000000000000000000008",
            "span_id": "b100000000000081",
            "name": "with-events",
            "span_kind": "span",
            "start_time": "2026-01-01T10:50:00.000000000Z",
            "end_time": "2026-01-01T10:50:01.000000000Z",
            "status": "ok",
            "ingest_time": "2026-01-01T10:50:01.000000000Z",
            "schema_version": 1
        }))
        .unwrap();
        span.events = vec![
            llmobs_core::span::SpanEvent {
                time: span.start_time,
                name: "gen_ai.choice".to_owned(),
                attributes: Some(r#"{"index":0}"#.to_owned()),
            },
            llmobs_core::span::SpanEvent {
                time: span.end_time,
                name: "retry".to_owned(),
                attributes: None,
            },
        ];
        span.links = vec![llmobs_core::span::SpanLink {
            trace_id: TRACE_1.parse().unwrap(),
            span_id: "b100000000000011".parse().unwrap(),
            attributes: Some(r#"{"kind":"follows"}"#.to_owned()),
        }];
        span.attributes
            .insert("http.method".to_owned(), "GET".to_owned());
        span.resource_attributes
            .insert("host.name".to_owned(), "box".to_owned());
        span.conventions = vec![llmobs_core::enums::Convention::Genai];
        let batch = llmobs_core::batch::spans_to_record_batch(&[span]).unwrap();
        app.state
            .storage
            .append_spans(seeded.project.id, batch)
            .await
            .unwrap();

        let (status, body) = get_json(
            &app,
            &seeded,
            "/traces/a1000000000000000000000000000008/spans/b100000000000081",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["events"],
            serde_json::json!([
                {"time": "2026-01-01T10:50:00.000000000Z", "name": "gen_ai.choice", "attributes": {"index": 0}},
                {"time": "2026-01-01T10:50:01.000000000Z", "name": "retry", "attributes": {}}
            ])
        );
        assert_eq!(
            body["links"],
            serde_json::json!([
                {"trace_id": TRACE_1, "span_id": "b100000000000011", "attributes": {"kind": "follows"}}
            ])
        );
        assert_eq!(
            body["attributes"],
            serde_json::json!({"http.method": "GET"})
        );
        assert_eq!(
            body["resource_attributes"],
            serde_json::json!({"host.name": "box"})
        );
        assert_eq!(body["conventions"], serde_json::json!(["genai"]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn span_detail_carries_media_and_model() {
        let (app, seeded) = loaded().await;
        let (status, body) = get_json(
            &app,
            &seeded,
            &format!("/traces/{TRACE_4}/spans/b100000000000043"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let span: SpanDetail = serde_json::from_value(body).unwrap();
        assert_eq!(
            span.media_refs,
            ["497790947d4666760ce38f3c00e852c71fdb66cae849bae8e9ede352719e1581"]
        );
        assert_eq!(span.model.as_deref(), Some("gpt-4o-mini"));
        let Some(Payload::Messages(messages)) = &span.input else {
            panic!("input is not messages: {:?}", span.input);
        };
        assert!(messages.iter().any(|message| {
            message
                .parts
                .iter()
                .any(|part| matches!(part, Part::Uri { .. }))
        }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_span_is_not_found() {
        let (app, seeded) = loaded().await;
        let (status, body) = get_json(
            &app,
            &seeded,
            &format!("/traces/{TRACE_4}/spans/b100000000000099"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn other_projects_see_no_rows() {
        let (app, seeded) = loaded().await;
        let (other, _key) = app.add_project(&seeded, "other", None).await;
        let response = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}/traces", other.id),
            )
            .header("cookie", &seeded.cookie)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        let page: Json = response.json();
        assert_eq!(page["data"], serde_json::json!([]));
    }
}
