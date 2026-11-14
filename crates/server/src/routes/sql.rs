//! SQL console (spec 05 § SQL console, spec 09, D-H-8).

use std::time::{Duration, Instant};

use axum::extract::{DefaultBodyLimit, Extension, State};
use axum::http::Method;
use axum::routing::post;
use llmobs_core::ids::ProjectId;
use llmobs_core::time_serde::parse_rfc3339;
use llmobs_storage::{SQL_MAX_ROWS, SqlRequest, TimeWindow};
use serde_json::{Map, Value};

use crate::api::sql::{SqlColumnInfo, SqlQueryRequest, SqlQueryResult};
use crate::auth::roles::ProjectPerm;
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError, field_error};
use crate::routes::read::open_query_windowed;

/// Request bodies are at most 256 KiB (the statement itself is capped at 64 KiB by A2).
const SQL_BODY_LIMIT: usize = 256 * 1024;
/// Default and largest time limit, ms.
const MAX_TIMEOUT_MS: i64 = 60_000;
/// Smallest time limit, ms.
const MIN_TIMEOUT_MS: i64 = 1_000;

pub(crate) fn routes() -> Vec<RouteDef> {
    vec![RouteDef {
        method: Method::POST,
        path: "/api/v1/projects/{project}/sql",
        access: Access::Project {
            perm: ProjectPerm::Query,
            session_only: false,
        },
        documented: true,
        handler: post(run_sql).layer(DefaultBodyLimit::max(SQL_BODY_LIMIT)),
    }]
}

/// A console request after its fields are checked.
#[derive(Debug)]
struct CheckedSql {
    query: String,
    params: Map<String, Value>,
    window: Option<TimeWindow>,
    limit: usize,
    timeout: Duration,
}

const BOUNDS_TOGETHER: &str = "from and to must be given together";
const RFC3339: &str = "expected an RFC 3339 timestamp";

/// D-H-8's field rules. The first failing rule wins, in this order: query, from/to, limit,
/// timeout_ms. Each failure is `400 bad_request` with one field error.
fn check_request(body: SqlQueryRequest) -> Result<CheckedSql, ApiError> {
    let SqlQueryRequest {
        query,
        params,
        from,
        to,
        limit,
        timeout_ms,
    } = body;
    if query.trim().is_empty() {
        return Err(field_error("query", "query must not be empty"));
    }
    let bounds = match (from.as_deref(), to.as_deref()) {
        (None, None) => None,
        (Some(_), None) => return Err(field_error("to", BOUNDS_TOGETHER)),
        (None, Some(_)) => return Err(field_error("from", BOUNDS_TOGETHER)),
        (Some(from), Some(to)) => {
            let from = parse_rfc3339(from).map_err(|_| field_error("from", RFC3339))?;
            let to = parse_rfc3339(to).map_err(|_| field_error("to", RFC3339))?;
            if from >= to {
                return Err(field_error("from", "from must be before to"));
            }
            Some((from, to))
        }
    };
    let limit = match limit {
        None => SQL_MAX_ROWS,
        Some(value) => usize::try_from(value)
            .ok()
            .filter(|limit| (1..=SQL_MAX_ROWS).contains(limit))
            .ok_or_else(|| field_error("limit", "limit must be between 1 and 10000"))?,
    };
    let timeout_ms = timeout_ms.unwrap_or(MAX_TIMEOUT_MS);
    if !(MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS).contains(&timeout_ms) {
        return Err(field_error(
            "timeout_ms",
            "timeout_ms must be between 1000 and 60000",
        ));
    }
    let timeout = Duration::from_millis(
        u64::try_from(timeout_ms)
            .map_err(|_| field_error("timeout_ms", "timeout_ms must be between 1000 and 60000"))?,
    );
    let window = bounds
        .map(|(from, to)| TimeWindow::new(from, to))
        .transpose()?;
    Ok(CheckedSql {
        query,
        params,
        window,
        limit,
        timeout,
    })
}

async fn run_sql(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Json(body): error::Json<SqlQueryRequest>,
) -> Result<error::Json<SqlQueryResult>, ApiError> {
    let started = Instant::now();
    let query_len = body.query.len();
    let result = run_json(&state, &access, body).await;
    log_end(access.project.id, "json", query_len, started, &result);
    result.map(error::Json)
}

/// Checks the request and runs it as JSON.
async fn run_json(
    state: &AppState,
    access: &ProjectAccess,
    body: SqlQueryRequest,
) -> Result<SqlQueryResult, ApiError> {
    let checked = check_request(body)?;
    let ctx = open_query_windowed(state, access.project.id, checked.window).await?;
    let json = ctx
        .sql_json(SqlRequest {
            query: &checked.query,
            params: &checked.params,
            limit: checked.limit,
            timeout: checked.timeout,
        })
        .await
        .map_err(ApiError::Sql)?;
    Ok(SqlQueryResult {
        columns: json
            .columns
            .into_iter()
            .map(|column| SqlColumnInfo {
                name: column.name,
                data_type: column.data_type,
            })
            .collect(),
        rows: json.rows,
        truncated: json.truncated,
    })
}

/// One `info` line per request: ids, sizes, duration, and the failure kind. Never the query
/// text, parameters or values.
fn log_end(
    project_id: ProjectId,
    format: &'static str,
    query_len: usize,
    started: Instant,
    result: &Result<SqlQueryResult, ApiError>,
) {
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match result {
        Ok(result) => tracing::info!(
            project_id = %project_id,
            format,
            query_len,
            rows = result.rows.len(),
            truncated = result.truncated,
            duration_ms,
            "sql query"
        ),
        Err(error) => tracing::info!(
            project_id = %project_id,
            format,
            query_len,
            kind = failure_kind(error),
            duration_ms,
            "sql query"
        ),
    }
}

/// The `kind` field of a failed request.
fn failure_kind(error: &ApiError) -> &'static str {
    match error {
        ApiError::Sql(error) | ApiError::Query(error) => error.kind(),
        ApiError::BadRequest { .. } => "bad_request",
        ApiError::Unavailable { .. } => "busy",
        ApiError::Forbidden { .. } => "forbidden",
        _ => "error",
    }
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;
    use serde_json::json;

    use super::*;
    use crate::api::common::ProblemCode;
    use crate::auth::roles::OrgRole;
    use crate::test_support::{Seeded, TestApp};

    /// The example request and its answer, checked against the real handler.
    const EXAMPLE_REQUEST: &str = include_str!("../../../../fixtures/api/requests/sql-query.json");
    const EXAMPLE_RESULT: &str = include_str!("../../../../fixtures/api/static/sql-result.json");

    /// A seeded app with the example dataset loaded.
    async fn loaded() -> (TestApp, Seeded) {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        app.load_example_dataset(seeded.project.id).await;
        (app, seeded)
    }

    /// POSTs `body` to the console of `seeded`'s project as `cookie`.
    async fn post_sql(app: &TestApp, seeded: &Seeded, body: &Value) -> (StatusCode, Value) {
        let response = app
            .request(
                Method::POST,
                &format!("/api/v1/projects/{}/sql", seeded.project.id),
            )
            .header("cookie", &seeded.cookie)
            .json(body)
            .send()
            .await;
        (response.status, response.json())
    }

    /// Asserts a `400 bad_request` field error on `field` with `message`, and no `detail`.
    fn assert_field_error(status: StatusCode, body: &Value, field: &str, message: &str) {
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "bad_request", "{body}");
        assert!(body.get("detail").is_none(), "{body}");
        assert_eq!(body["errors"][0]["field"], field, "{body}");
        assert_eq!(body["errors"][0]["message"], message, "{body}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn example_request_returns_the_example_result() {
        let (app, seeded) = loaded().await;
        let request: Value = serde_json::from_str(EXAMPLE_REQUEST).unwrap();
        let (status, body) = post_sql(&app, &seeded, &request).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        let result: SqlQueryResult = serde_json::from_value(body.clone()).unwrap();
        let expected: SqlQueryResult = serde_json::from_str(EXAMPLE_RESULT).unwrap();
        assert_eq!(result, expected);
        assert_eq!(
            result.columns[1],
            SqlColumnInfo {
                name: "spans".to_owned(),
                data_type: "Int64".to_owned(),
            }
        );
        assert!(!result.truncated);
        assert_eq!(
            body["rows"],
            json!([
                {"span_kind": "agent", "spans": 2, "first_start": "2026-01-01T10:00:00.000000000Z"},
                {"span_kind": "chain", "spans": 1, "first_start": "2026-01-01T10:10:00.000000000Z"},
                {"span_kind": "llm", "spans": 5, "first_start": "2026-01-01T10:00:00.100000000Z"},
                {"span_kind": "tool", "spans": 1, "first_start": "2026-01-01T10:00:01.950000000Z"}
            ])
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn limit_truncates() {
        let (app, seeded) = loaded().await;
        let query = "SELECT span_id FROM spans ORDER BY span_id";

        let (_, body) = post_sql(&app, &seeded, &json!({"query": query, "limit": 3})).await;
        assert_eq!(body["rows"].as_array().unwrap().len(), 3);
        assert_eq!(body["truncated"], true);
        assert_eq!(body["rows"][0], json!({"span_id": "b100000000000011"}));

        let (_, body) = post_sql(&app, &seeded, &json!({"query": query, "limit": 1})).await;
        assert_eq!(body["rows"].as_array().unwrap().len(), 1);
        assert_eq!(body["truncated"], true);

        let (_, body) = post_sql(&app, &seeded, &json!({"query": query, "limit": 13})).await;
        assert_eq!(body["rows"].as_array().unwrap().len(), 13);
        assert_eq!(body["truncated"], true);

        let (_, body) = post_sql(&app, &seeded, &json!({"query": query, "limit": 14})).await;
        assert_eq!(body["rows"].as_array().unwrap().len(), 14);
        assert_eq!(body["truncated"], false);

        let (_, body) = post_sql(&app, &seeded, &json!({"query": query, "limit": 10000})).await;
        assert_eq!(body["rows"].as_array().unwrap().len(), 14);
        assert_eq!(body["truncated"], false);

        let (_, body) = post_sql(&app, &seeded, &json!({"query": query})).await;
        assert_eq!(body["rows"].as_array().unwrap().len(), 14);
        assert_eq!(body["truncated"], false);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn window_filters_rows() {
        let (app, seeded) = loaded().await;
        let count = |extra: Value| {
            let mut body = json!({"query": "SELECT count(*) AS n FROM spans"});
            body.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            body
        };
        let window = json!({"from": "2026-01-01T10:00:00Z", "to": "2026-01-01T10:05:00Z"});

        let (_, body) = post_sql(&app, &seeded, &count(window.clone())).await;
        assert_eq!(body["rows"], json!([{"n": 6}]));

        let (_, body) = post_sql(
            &app,
            &seeded,
            &count(json!({"from": "2026-01-01T11:00:00+01:00", "to": "2026-01-01T11:05:00+01:00"})),
        )
        .await;
        assert_eq!(body["rows"], json!([{"n": 6}]));

        let (_, body) = post_sql(&app, &seeded, &count(json!({}))).await;
        assert_eq!(body["rows"], json!([{"n": 14}]));

        let traces = json!({"query": "SELECT count(*) AS n FROM traces", "from": "2026-01-01T10:00:00Z", "to": "2026-01-01T10:05:00Z"});
        let (_, body) = post_sql(&app, &seeded, &traces).await;
        assert_eq!(body["rows"], json!([{"n": 2}]));

        let scores = json!({"query": "SELECT count(*) AS n FROM scores", "from": "2026-01-01T10:00:00Z", "to": "2026-01-01T10:05:00Z"});
        let (_, body) = post_sql(&app, &seeded, &scores).await;
        assert_eq!(body["rows"], json!([{"n": 3}]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn params_bind_and_bad_params_fail() {
        let (app, seeded) = loaded().await;
        let query = "SELECT count(*) AS n FROM spans WHERE environment = {env:String}";

        let (status, body) = post_sql(
            &app,
            &seeded,
            &json!({"query": query, "params": {"env": "staging"}}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["rows"], json!([{"n": 3}]));

        let (status, body) = post_sql(&app, &seeded, &json!({"query": query, "params": {}})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "bad_request");
        assert_eq!(body["detail"], "missing value for parameter env");

        let (status, body) = post_sql(
            &app,
            &seeded,
            &json!({"query": query, "params": {"env": "staging", "x": 1}}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "bad_request");
        assert_eq!(body["detail"], "unknown parameter x");

        let (status, body) = post_sql(&app, &seeded, &json!({"query": query, "params": []})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "bad_request");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn invalid_sql_names_the_query_field() {
        let (app, seeded) = loaded().await;

        let (status, body) = post_sql(&app, &seeded, &json!({"query": "SELEC 1"})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "invalid_filter");
        assert_eq!(body["detail"], "invalid query");
        assert_eq!(body["errors"][0]["field"], "query");
        assert_eq!(body["errors"][0]["position"]["line"], 1);

        let (status, body) = post_sql(&app, &seeded, &json!({"query": "DROP TABLE spans"})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "invalid_filter");
        assert_eq!(body["errors"][0]["field"], "query");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn execution_errors_are_query_failed() {
        let (app, seeded) = loaded().await;
        let (status, body) = post_sql(&app, &seeded, &json!({"query": "SELECT 1 / 0 AS n"})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "query_failed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn request_fields_are_checked() {
        let (app, seeded) = loaded().await;
        let q = "SELECT 1";
        let t10 = "2026-01-01T10:00:00Z";
        let cases: Vec<(Value, &str, &str)> = vec![
            (json!({"query": "  "}), "query", "query must not be empty"),
            (
                json!({"query": q, "from": t10, "to": "soon"}),
                "to",
                RFC3339,
            ),
            (
                json!({"query": q, "from": "9999-12-31T00:00:00Z", "to": t10}),
                "from",
                RFC3339,
            ),
            (
                json!({"query": q, "from": t10, "to": "2026-01-01T11:00:00+01:00"}),
                "from",
                "from must be before to",
            ),
            (
                json!({"query": " ", "limit": 0}),
                "query",
                "query must not be empty",
            ),
            (
                json!({"query": q, "from": "x", "to": t10, "limit": 0}),
                "from",
                RFC3339,
            ),
            (json!({"query": q, "from": t10}), "to", BOUNDS_TOGETHER),
            (json!({"query": q, "to": t10}), "from", BOUNDS_TOGETHER),
            (
                json!({"query": q, "from": "yesterday", "to": t10}),
                "from",
                RFC3339,
            ),
            (
                json!({"query": q, "from": t10, "to": t10}),
                "from",
                "from must be before to",
            ),
            (
                json!({"query": q, "limit": 0}),
                "limit",
                "limit must be between 1 and 10000",
            ),
            (
                json!({"query": q, "limit": -1}),
                "limit",
                "limit must be between 1 and 10000",
            ),
            (
                json!({"query": q, "limit": 10001}),
                "limit",
                "limit must be between 1 and 10000",
            ),
            (
                json!({"query": q, "timeout_ms": 999}),
                "timeout_ms",
                "timeout_ms must be between 1000 and 60000",
            ),
            (
                json!({"query": q, "timeout_ms": 60001}),
                "timeout_ms",
                "timeout_ms must be between 1000 and 60000",
            ),
        ];
        for (body, field, message) in cases {
            let (status, response) = post_sql(&app, &seeded, &body).await;
            assert_field_error(status, &response, field, message);
        }

        for (limit, expected) in [(1, 1), (10000, 14)] {
            let (status, body) = post_sql(
                &app,
                &seeded,
                &json!({"query": "SELECT span_id FROM spans", "limit": limit}),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["rows"].as_array().unwrap().len(), expected);
        }
        let (status, body) =
            post_sql(&app, &seeded, &json!({"query": q, "timeout_ms": 60000})).await;
        assert_eq!(status, StatusCode::OK, "{body}");

        for extra in [
            json!({"query": q, "foo": 1}),
            json!({"query": 1}),
            json!({"query": q, "from": 123, "to": t10}),
        ] {
            let (status, body) = post_sql(&app, &seeded, &extra).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert_eq!(body["code"], "bad_request");
        }

        let response = app
            .request(
                Method::POST,
                &format!("/api/v1/projects/{}/sql", seeded.project.id),
            )
            .header("cookie", &seeded.cookie)
            .body(r#"{"query":"SELECT 1"}"#)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);

        let huge = format!("{{\"query\": \"SELECT '{}'\"}}", "x".repeat(270_000));
        let response = app
            .request(
                Method::POST,
                &format!("/api/v1/projects/{}/sql", seeded.project.id),
            )
            .header("cookie", &seeded.cookie)
            .header("content-type", "application/json")
            .body(huge)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(response.problem().code, ProblemCode::PayloadTooLarge);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_credentials_can_query() {
        let (app, seeded) = loaded().await;
        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let token = app.personal_token(viewer.id, false).await;
        let path = format!("/api/v1/projects/{}/sql", seeded.project.id);
        let body = json!({"query": "SELECT 1 AS one"});

        let response = app
            .request(Method::POST, &path)
            .header("cookie", &viewer.cookie)
            .json(&body)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.json::<Value>()["rows"], json!([{"one": 1}]));

        let response = app
            .request(Method::POST, &path)
            .header("authorization", &format!("Bearer {token}"))
            .json(&body)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.json::<Value>()["rows"], json!([{"one": 1}]));

        let response = app
            .request(Method::POST, &path)
            .header("authorization", &format!("Bearer {}", seeded.read_key))
            .json(&body)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.json::<Value>()["rows"], json!([{"one": 1}]));

        let response = app
            .request(Method::POST, &path)
            .header("authorization", &format!("Bearer {}", seeded.ingest_key))
            .json(&body)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::FORBIDDEN);
    }

    #[test]
    fn check_request_fills_defaults_and_the_window() {
        let checked = check_request(serde_json::from_value(json!({"query": "SELECT 1"})).unwrap())
            .unwrap_or_else(|_| panic!("defaults are valid"));
        assert_eq!(checked.query, "SELECT 1");
        assert!(checked.params.is_empty());
        assert!(checked.window.is_none());
        assert_eq!(checked.limit, SQL_MAX_ROWS);
        assert_eq!(checked.timeout, Duration::from_secs(60));

        let checked = check_request(
            serde_json::from_value(json!({
                "query": "SELECT 1",
                "from": "2026-01-01T10:00:00Z",
                "to": "2026-01-01T12:00:00+01:00",
                "limit": 5,
                "timeout_ms": 1000
            }))
            .unwrap(),
        )
        .unwrap_or_else(|_| panic!("explicit values are valid"));
        assert_eq!(checked.limit, 5);
        assert_eq!(checked.timeout, Duration::from_millis(1000));
        let window = checked.window.expect("both bounds give a window");
        assert_eq!(window.from(), 1_767_261_600_000_000_000);
        assert_eq!(window.to(), 1_767_265_200_000_000_000);

        let body = serde_json::from_value(json!({"query": "SELECT 1", "timeout_ms": 999})).unwrap();
        assert!(check_request(body).is_err());
    }

    #[test]
    fn request_schema_requires_only_query() {
        let document = serde_json::to_value(crate::api::openapi::openapi()).unwrap();
        let schema = &document["components"]["schemas"]["SqlQueryRequest"];
        assert_eq!(schema["required"], json!(["query"]));
        for field in ["from", "to"] {
            let property = &schema["properties"][field];
            assert_eq!(property["type"], "string", "{field}");
            assert_eq!(property["format"], "date-time", "{field}");
        }
    }

    #[test]
    fn failure_kind_names_each_error() {
        assert_eq!(
            failure_kind(&ApiError::Sql(llmobs_storage::RunError::Timeout)),
            "timeout"
        );
        assert_eq!(
            failure_kind(&ApiError::BadRequest {
                detail: None,
                errors: Vec::new(),
            }),
            "bad_request"
        );
        assert_eq!(failure_kind(&ApiError::Unavailable { detail: "x" }), "busy");
        assert_eq!(
            failure_kind(&ApiError::Forbidden {
                code: ProblemCode::Forbidden,
            }),
            "forbidden"
        );
        assert_eq!(failure_kind(&ApiError::NotFound), "error");
    }

    #[tokio::test]
    async fn logs_carry_no_query_text() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let path = format!("/api/v1/projects/{}/sql", seeded.project.id);
        let (_, logs) = crate::test_support::capture_logs(async {
            for body in [
                json!({"query": "SELECT 'secret_marker_text' AS s"}),
                json!({"query": "SELECT secret_marker_text FROM spans"}),
                json!({"query": "  "}),
            ] {
                app.request(Method::POST, &path)
                    .header("cookie", &seeded.cookie)
                    .json(&body)
                    .send()
                    .await;
            }
        })
        .await;
        assert!(logs.matches("sql query").count() >= 3, "{logs}");
        assert!(logs.contains("rows=1"), "{logs}");
        assert!(logs.contains("kind="), "{logs}");
        assert!(!logs.contains("secret_marker_text"), "{logs}");

        let (_, logs) = crate::test_support::capture_logs(async {
            app.request(Method::POST, &path)
                .header("cookie", &seeded.cookie)
                .json(&json!({"query": 1}))
                .send()
                .await;
        })
        .await;
        assert!(!logs.contains("sql query"), "{logs}");
    }
}
