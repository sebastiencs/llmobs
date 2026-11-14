//! SQL console (spec 05 § SQL console, spec 09, D-H-8).

use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Extension, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::{Stream, StreamExt};
use llmobs_core::ids::ProjectId;
use llmobs_core::time_serde::parse_rfc3339;
use llmobs_storage::{ExportFormat, RunError, SQL_MAX_ROWS, SqlRequest, TimeWindow};
use serde_json::{Map, Value};

use crate::api::common::ProblemCode;
use crate::api::sql::{SqlColumnInfo, SqlQueryRequest, SqlQueryResult};
use crate::auth::roles::{ProjectPerm, Role};
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

/// The answer format `Accept` asks for (D-H-8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SqlFormat {
    Json,
    Csv,
    Ndjson,
    Parquet,
}

/// Reads every `Accept` header value in order and splits it on `,`. For each media range,
/// drops the parameters (`;` and after), trims and lowercases it. The first one that is
/// `text/csv`, `application/x-ndjson`, `application/vnd.apache.parquet` or
/// `application/json` decides. Anything else (`*/*`, `text/*`, absent, a value that is not
/// visible ASCII) is skipped; when nothing decides, JSON. `q` values are ignored (D-H-8).
fn negotiate(headers: &HeaderMap) -> SqlFormat {
    for value in headers.get_all(header::ACCEPT) {
        let Ok(text) = value.to_str() else {
            continue;
        };
        for range in text.split(',') {
            let media = range
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            let format = match media.as_str() {
                "text/csv" => SqlFormat::Csv,
                "application/x-ndjson" => SqlFormat::Ndjson,
                "application/vnd.apache.parquet" => SqlFormat::Parquet,
                "application/json" => SqlFormat::Json,
                _ => continue,
            };
            return format;
        }
    }
    SqlFormat::Json
}

impl SqlFormat {
    /// A2's format, `None` for JSON.
    fn export(self) -> Option<ExportFormat> {
        match self {
            Self::Json => None,
            Self::Csv => Some(ExportFormat::Csv),
            Self::Ndjson => Some(ExportFormat::Ndjson),
            Self::Parquet => Some(ExportFormat::Parquet),
        }
    }

    /// `Content-Type` of the answer.
    fn content_type(self) -> &'static str {
        match self {
            Self::Json => "application/json",
            Self::Csv => "text/csv",
            Self::Ndjson => "application/x-ndjson",
            Self::Parquet => "application/vnd.apache.parquet",
        }
    }

    /// File name extension of an export.
    fn extension(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Csv => "csv",
            Self::Ndjson => "ndjson",
            Self::Parquet => "parquet",
        }
    }

    /// The `format` log field.
    fn label(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Csv => "csv",
            Self::Ndjson => "ndjson",
            Self::Parquet => "parquet",
        }
    }
}

async fn run_sql(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    headers: HeaderMap,
    error::Json(body): error::Json<SqlQueryRequest>,
) -> Result<Response, ApiError> {
    let started = Instant::now();
    let query_len = body.query.len();
    let format = negotiate(&headers);
    let Some(export) = format.export() else {
        let result = run_json(&state, &access, body).await;
        log_end(access.project.id, "json", query_len, started, &result);
        return result.map(|result| error::Json(result).into_response());
    };
    match start_export(&state, &access, body, format, export, started).await {
        Ok(response) => Ok(response),
        Err(error) => {
            log_failure(
                access.project.id,
                format.label(),
                query_len,
                started,
                &error,
            );
            Err(error)
        }
    }
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

/// Role, checks, admission, preparation and the first item; then the streaming response.
async fn start_export(
    state: &AppState,
    access: &ProjectAccess,
    body: SqlQueryRequest,
    format: SqlFormat,
    export: ExportFormat,
    started: Instant,
) -> Result<Response, ApiError> {
    // D-H-5: sessions and PATs need role member. An `sk` key has no role; the access layer
    // already required its `read` scope.
    if access.role.is_some_and(|role| role < Role::Member) {
        return Err(ApiError::Forbidden {
            code: ProblemCode::Forbidden,
        });
    }
    let checked = check_request(body)?;
    let ctx = open_query_windowed(state, access.project.id, checked.window).await?;
    let prepared = tokio::time::timeout(checked.timeout, async {
        let stream = ctx
            .sql_export(
                SqlRequest {
                    query: &checked.query,
                    params: &checked.params,
                    limit: checked.limit,
                    timeout: checked.timeout,
                },
                export,
            )
            .await?;
        first_item(stream, format).await
    })
    .await;
    let stream = match prepared {
        Err(_elapsed) => return Err(ApiError::Sql(RunError::Timeout)),
        Ok(Err(error)) => return Err(ApiError::Sql(error)),
        Ok(Ok(stream)) => stream,
    };
    // The stream owns the query's runtime and memory admission from here on.
    drop(ctx);
    export_response(
        stream,
        format,
        access.project.id,
        checked.query.len(),
        started,
        state.clock.now_ns(),
    )
}

/// Pulls the stream's first data item (for CSV, the header and the item after it) so an error
/// there becomes a problem document instead of a cut body. Returns a stream that yields the
/// pulled items first, then the rest.
async fn first_item(
    mut stream: BoxStream<'static, Result<Bytes, RunError>>,
    format: SqlFormat,
) -> Result<BoxStream<'static, Result<Bytes, RunError>>, RunError> {
    let wanted = if format == SqlFormat::Csv { 2 } else { 1 };
    let mut kept = Vec::with_capacity(wanted);
    while kept.len() < wanted {
        match stream.next().await {
            Some(Ok(bytes)) => kept.push(bytes),
            Some(Err(error)) => return Err(error),
            None => break,
        }
    }
    Ok(
        futures::stream::iter(kept.into_iter().map(Ok::<Bytes, RunError>))
            .chain(stream)
            .boxed(),
    )
}

/// The streaming answer of an export: headers and a [`LoggedExport`] body. Separate from
/// `start_export` so tests can feed it a stream that fails after its first bytes.
fn export_response(
    stream: BoxStream<'static, Result<Bytes, RunError>>,
    format: SqlFormat,
    project_id: ProjectId,
    query_len: usize,
    started: Instant,
    now_ns: i64,
) -> Result<Response, ApiError> {
    let stamp = chrono::DateTime::from_timestamp_nanos(now_ns).format("%Y%m%dT%H%M%SZ");
    let disposition = format!(
        "attachment; filename=\"query-{stamp}.{}\"",
        format.extension()
    );
    let disposition =
        HeaderValue::from_str(&disposition).map_err(|error| ApiError::Internal(Box::new(error)))?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(format.content_type()),
    );
    headers.insert(header::CONTENT_DISPOSITION, disposition);
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let body = Body::from_stream(LoggedExport::new(
        stream,
        project_id,
        format.label(),
        query_len,
        started,
    ));
    Ok((StatusCode::OK, headers, body).into_response())
}

/// A2's export stream as an HTTP body: counts bytes, turns a `RunError` into a fixed
/// `io::Error`, and writes the request's log line once, when the stream ends, fails or is
/// dropped (the client went away).
struct LoggedExport {
    inner: BoxStream<'static, Result<Bytes, RunError>>,
    project_id: ProjectId,
    format: &'static str,
    query_len: usize,
    started: Instant,
    bytes: u64,
    outcome: ExportOutcome,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportOutcome {
    Running,
    Done,
    /// The failure kind (`RunError::kind()`).
    Failed(&'static str),
}

impl LoggedExport {
    fn new(
        inner: BoxStream<'static, Result<Bytes, RunError>>,
        project_id: ProjectId,
        format: &'static str,
        query_len: usize,
        started: Instant,
    ) -> Self {
        Self {
            inner,
            project_id,
            format,
            query_len,
            started,
            bytes: 0,
            outcome: ExportOutcome::Running,
        }
    }
}

impl std::fmt::Debug for LoggedExport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoggedExport")
            .field("project_id", &self.project_id)
            .field("format", &self.format)
            .field("bytes", &self.bytes)
            .field("outcome", &self.outcome)
            .finish_non_exhaustive()
    }
}

impl Stream for LoggedExport {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = &mut *self;
        match this.inner.poll_next_unpin(cx) {
            Poll::Ready(Some(Ok(bytes))) => {
                let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                this.bytes = this.bytes.saturating_add(size);
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(error))) => {
                this.outcome = ExportOutcome::Failed(error.kind());
                // The message may quote the query, so the body gets a fixed text.
                Poll::Ready(Some(Err(std::io::Error::other("sql export failed"))))
            }
            Poll::Ready(None) => {
                if this.outcome == ExportOutcome::Running {
                    this.outcome = ExportOutcome::Done;
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for LoggedExport {
    fn drop(&mut self) {
        let duration_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let kind = match self.outcome {
            ExportOutcome::Running => Some("cancelled"),
            ExportOutcome::Done => None,
            ExportOutcome::Failed(kind) => Some(kind),
        };
        if kind == Some("internal") {
            tracing::error!(
                project_id = %self.project_id,
                format = self.format,
                query_len = self.query_len,
                bytes = self.bytes,
                kind,
                duration_ms,
                "sql query"
            );
        } else {
            tracing::info!(
                project_id = %self.project_id,
                format = self.format,
                query_len = self.query_len,
                bytes = self.bytes,
                kind,
                duration_ms,
                "sql query"
            );
        }
    }
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
        Err(error) => log_failure(project_id, format, query_len, started, error),
    }
}

/// The request's line for a failure, any format.
fn log_failure(
    project_id: ProjectId,
    format: &'static str,
    query_len: usize,
    started: Instant,
    error: &ApiError,
) {
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    tracing::info!(
        project_id = %project_id,
        format,
        query_len,
        kind = failure_kind(error),
        duration_ms,
        "sql query"
    );
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
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;
    use bytesize::ByteSize;
    use http_body_util::BodyExt;
    use serde_json::json;
    use uuid::Uuid;

    use super::*;
    use crate::auth::roles::{OrgRole, ProjectOverride};
    use crate::meta::members::set_project_override;
    use crate::test_support::{Seeded, TestApp, TestResponse};

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
        assert_eq!(failure_kind(&ApiError::Sql(RunError::Timeout)), "timeout");
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

    /// The example request with `limit` set.
    fn example_with_limit(limit: u64) -> Value {
        let mut body: Value = serde_json::from_str(EXAMPLE_REQUEST).unwrap();
        body["limit"] = json!(limit);
        body
    }

    fn example() -> Value {
        serde_json::from_str(EXAMPLE_REQUEST).unwrap()
    }

    /// POSTs `body` to the console of `project` with one credential header and an optional
    /// `Accept`.
    async fn post_as(
        app: &TestApp,
        project: ProjectId,
        credential: (&str, &str),
        accept: Option<&str>,
        body: &Value,
    ) -> TestResponse {
        let mut request = app
            .request(Method::POST, &format!("/api/v1/projects/{project}/sql"))
            .header(credential.0, credential.1)
            .json(body);
        if let Some(accept) = accept {
            request = request.header("accept", accept);
        }
        request.send().await
    }

    /// Two data frames, then a failure: a stream that fails after its first bytes.
    fn failing_stream() -> BoxStream<'static, Result<Bytes, RunError>> {
        futures::stream::iter(vec![
            Ok(Bytes::from_static(b"n\r\n")),
            Ok(Bytes::from_static(b"1\r\n")),
            Err(RunError::Failed("boom".to_owned())),
        ])
        .boxed()
    }

    /// [`failing_stream`] with a pause before the failure. Without it, hyper drops the
    /// connection before it flushes the status line, and the client sees no response at all.
    fn paused_failing_stream() -> BoxStream<'static, Result<Bytes, RunError>> {
        futures::stream::iter(vec![
            Ok::<Bytes, RunError>(Bytes::from_static(b"n\r\n")),
            Ok(Bytes::from_static(b"1\r\n")),
        ])
        .chain(futures::stream::once(async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            Err(RunError::Failed("boom".to_owned()))
        }))
        .boxed()
    }

    /// A handler answering [`paused_failing_stream`] as a CSV export.
    async fn cut_export() -> Result<Response, ApiError> {
        export_response(
            paused_failing_stream(),
            SqlFormat::Csv,
            ProjectId::from_uuid(Uuid::now_v7()),
            10,
            Instant::now(),
            0,
        )
    }

    /// The row count and column names of a Parquet file.
    fn read_parquet(bytes: Bytes) -> (Vec<String>, usize) {
        use datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        let builder = ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
        let names = builder
            .schema()
            .fields()
            .iter()
            .map(|field| field.name().clone())
            .collect();
        let rows = builder
            .build()
            .unwrap()
            .map(|batch| batch.unwrap().num_rows())
            .sum();
        (names, rows)
    }

    #[test]
    fn negotiate_reads_accept_in_order() {
        let cases: Vec<(Vec<&str>, SqlFormat)> = vec![
            (vec![], SqlFormat::Json),
            (vec!["*/*"], SqlFormat::Json),
            (vec!["text/csv"], SqlFormat::Csv),
            (vec!["TEXT/CSV"], SqlFormat::Csv),
            (vec!["application/json, text/csv"], SqlFormat::Json),
            (vec!["text/csv;q=0.1, application/json"], SqlFormat::Csv),
            (vec!["text/html, application/x-ndjson"], SqlFormat::Ndjson),
            (vec!["application/vnd.apache.parquet"], SqlFormat::Parquet),
            (vec!["text/html", "text/csv"], SqlFormat::Csv),
        ];
        for (values, expected) in cases {
            let mut headers = HeaderMap::new();
            for value in &values {
                headers.append(header::ACCEPT, HeaderValue::from_str(value).unwrap());
            }
            assert_eq!(negotiate(&headers), expected, "{values:?}");
        }

        // Not visible ASCII: skipped, so the next value decides.
        let mut headers = HeaderMap::new();
        headers.append(
            header::ACCEPT,
            HeaderValue::from_bytes(&[0xC3, 0xA9]).unwrap(),
        );
        headers.append(header::ACCEPT, HeaderValue::from_static("text/csv"));
        assert_eq!(negotiate(&headers), SqlFormat::Csv);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn csv_export_streams_every_row() {
        let (app, seeded) = loaded().await;
        let response = post_as(
            &app,
            seeded.project.id,
            ("cookie", &seeded.cookie),
            Some("text/csv"),
            &example_with_limit(1),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.header("content-type"), Some("text/csv"));
        let stamp = chrono::DateTime::from_timestamp_nanos(app.state.clock.now_ns())
            .format("%Y%m%dT%H%M%SZ")
            .to_string();
        assert_eq!(
            response.header("content-disposition"),
            Some(format!("attachment; filename=\"query-{stamp}.csv\"").as_str())
        );
        assert_eq!(response.header("cache-control"), Some("no-store"));
        assert_eq!(
            response.body.as_ref(),
            b"span_kind,spans,first_start\r\nagent,2,2026-01-01T10:00:00.000000000Z\r\nchain,1,2026-01-01T10:10:00.000000000Z\r\nllm,5,2026-01-01T10:00:00.100000000Z\r\ntool,1,2026-01-01T10:00:01.950000000Z\r\n"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ndjson_export_matches_the_json_rows() {
        let (app, seeded) = loaded().await;
        let response = post_as(
            &app,
            seeded.project.id,
            ("cookie", &seeded.cookie),
            Some("application/x-ndjson"),
            &example(),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(
            response.header("content-type"),
            Some("application/x-ndjson")
        );
        assert!(
            response
                .header("content-disposition")
                .is_some_and(|value| value.ends_with(".ndjson\"")),
            "{:?}",
            response.header("content-disposition")
        );
        let text = std::str::from_utf8(&response.body).unwrap();
        let rows: Vec<Value> = text
            .split('\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let expected: Value = serde_json::from_str(EXAMPLE_RESULT).unwrap();
        assert_eq!(Value::Array(rows), expected["rows"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn parquet_export_reads_back() {
        let (app, seeded) = loaded().await;
        let response = post_as(
            &app,
            seeded.project.id,
            ("cookie", &seeded.cookie),
            Some("application/vnd.apache.parquet"),
            &example(),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(
            response.header("content-type"),
            Some("application/vnd.apache.parquet")
        );
        assert!(response.body.starts_with(b"PAR1"));
        assert!(response.body.ends_with(b"PAR1"));
        let (names, rows) = read_parquet(response.body);
        assert_eq!(names, ["span_kind", "spans", "first_start"]);
        assert_eq!(rows, 4);

        let response = post_as(
            &app,
            seeded.project.id,
            ("cookie", &seeded.cookie),
            Some("application/vnd.apache.parquet"),
            &json!({"query": "SELECT span_id FROM spans WHERE span_id = 'none'"}),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        let (names, rows) = read_parquet(response.body);
        assert_eq!(names, ["span_id"]);
        assert_eq!(rows, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn exports_need_member() {
        let (app, seeded) = loaded().await;
        let project = seeded.project.id;
        let csv = Some("text/csv");
        let body = json!({"query": "SELECT 1 AS one"});

        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let cookie = ("cookie", viewer.cookie.as_str());
        let response = post_as(&app, project, cookie, csv, &body).await;
        assert_eq!(response.status, StatusCode::FORBIDDEN);
        assert_eq!(response.problem().code, ProblemCode::Forbidden);

        // The role is checked before the fields.
        let response = post_as(
            &app,
            project,
            cookie,
            csv,
            &json!({"query": "SELECT 1", "limit": 0}),
        )
        .await;
        assert_eq!(response.status, StatusCode::FORBIDDEN);
        // The body is extracted before the role is checked.
        let response = post_as(
            &app,
            project,
            cookie,
            csv,
            &json!({"query": "SELECT 1", "foo": 1}),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        // Without `Accept`, a viewer may still read JSON.
        let response = post_as(&app, project, cookie, None, &body).await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.header("content-type"), Some("application/json"));

        let token = app.personal_token(viewer.id, false).await;
        let bearer = format!("Bearer {token}");
        let response = post_as(&app, project, ("authorization", &bearer), csv, &body).await;
        assert_eq!(response.status, StatusCode::FORBIDDEN);

        let member = app
            .add_member(&seeded, "member@example.com", OrgRole::Member)
            .await;
        let response = post_as(&app, project, ("cookie", &member.cookie), csv, &body).await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.header("content-type"), Some("text/csv"));
        assert_eq!(response.body.as_ref(), b"one\r\n1\r\n");
        let token = app.personal_token(member.id, false).await;
        let bearer = format!("Bearer {token}");
        let response = post_as(&app, project, ("authorization", &bearer), csv, &body).await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.header("content-type"), Some("text/csv"));

        // Org member, but a project override makes them a viewer here.
        let member2 = app
            .add_member(&seeded, "member2@example.com", OrgRole::Member)
            .await;
        let member2_id = member2.id;
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    set_project_override(conn, project, member2_id, ProjectOverride::Viewer).await
                })
            })
            .await
            .unwrap();
        let response = post_as(&app, project, ("cookie", &member2.cookie), csv, &body).await;
        assert_eq!(response.status, StatusCode::FORBIDDEN);

        // An `sk` key with `read` may export.
        let bearer = format!("Bearer {}", seeded.read_key);
        let response = post_as(&app, project, ("authorization", &bearer), csv, &body).await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.header("content-type"), Some("text/csv"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn preparation_errors_are_problems() {
        let (app, seeded) = loaded().await;
        let csv = Some("text/csv");

        let response = post_as(
            &app,
            seeded.project.id,
            ("cookie", &seeded.cookie),
            csv,
            &json!({"query": "SELEC 1"}),
        )
        .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            response.header("content-type"),
            Some("application/problem+json")
        );
        let body: Value = response.json();
        assert_eq!(body["code"], "invalid_filter", "{body}");
        assert_eq!(body["errors"][0]["field"], "query", "{body}");

        let response = post_as(
            &app,
            seeded.project.id,
            ("cookie", &seeded.cookie),
            csv,
            &json!({"query": "SELECT 1", "limit": 0}),
        )
        .await;
        let body: Value = response.json();
        assert_field_error(
            response.status,
            &body,
            "limit",
            "limit must be between 1 and 10000",
        );

        let response = post_as(
            &app,
            seeded.project.id,
            ("cookie", &seeded.cookie),
            csv,
            &json!({"query": "SELECT 1", "params": {"x": 1}}),
        )
        .await;
        let body: Value = response.json();
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "bad_request", "{body}");
        assert_eq!(body["detail"], "unknown parameter x", "{body}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failing_first_batch_is_a_problem() {
        let (app, seeded) = loaded().await;
        let body = json!({"query": "SELECT CAST(name AS INT) AS n FROM spans"});
        for accept in ["text/csv", "application/x-ndjson"] {
            let response = post_as(
                &app,
                seeded.project.id,
                ("cookie", &seeded.cookie),
                Some(accept),
                &body,
            )
            .await;
            assert_eq!(response.status, StatusCode::BAD_REQUEST, "{accept}");
            assert_eq!(
                response.header("content-type"),
                Some("application/problem+json"),
                "{accept}"
            );
            let problem: Value = response.json();
            assert_eq!(problem["code"], "query_failed", "{accept}: {problem}");
        }
    }

    #[tokio::test]
    async fn a_failure_after_the_first_bytes_ends_the_body_with_an_error() {
        let ((), logs) = crate::test_support::capture_logs(async {
            let response = export_response(
                failing_stream(),
                SqlFormat::Csv,
                ProjectId::from_uuid(Uuid::now_v7()),
                10,
                Instant::now(),
                0,
            )
            .unwrap();
            let mut body = response.into_body();
            let first = body.frame().await.expect("a frame").expect("no error yet");
            assert_eq!(first.into_data().unwrap(), Bytes::from_static(b"n\r\n"));
            let second = body.frame().await.expect("a frame").expect("no error yet");
            assert_eq!(second.into_data().unwrap(), Bytes::from_static(b"1\r\n"));
            let third = body.frame().await;
            assert!(matches!(third, Some(Err(_))), "{third:?}");
            drop(body);
        })
        .await;
        assert!(logs.contains("sql query"), "{logs}");
        assert!(logs.contains("bytes=6"), "{logs}");
        assert!(logs.contains("kind=\"failed\""), "{logs}");
        assert!(!logs.contains("boom"), "{logs}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cut_export_ends_the_body_with_its_frames() {
        let app = TestApp::with_routes(Router::new().route("/test/export", get(cut_export))).await;
        let mut stream = app.request(Method::GET, "/test/export").send_stream().await;
        assert_eq!(stream.status, StatusCode::OK);
        let (bytes, failed) = stream.read_to_end(Duration::from_secs(10)).await;
        assert_eq!(bytes, b"n\r\n1\r\n");
        assert!(failed);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_real_client_sees_a_cut_transfer() {
        let app = TestApp::with_routes(Router::new().route("/test/export", get(cut_export))).await;
        let addr = app.serve_loopback().await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let response = client
            .get(format!("http://{addr}/test/export"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.bytes().await.is_err());
    }

    #[tokio::test]
    async fn a_client_that_goes_away_releases_the_query() {
        let app = TestApp::with_config(|config| {
            config.storage.query.max_memory_total = Some(ByteSize::mib(64));
            config.storage.query.max_memory = ByteSize::mib(64);
            config.storage.query.initial_memory = ByteSize::mib(64);
            config.storage.query.admission_wait = Duration::from_millis(200);
        })
        .await;
        let seeded = app.seed().await;
        app.load_example_dataset(seeded.project.id).await;
        let project = seeded.project.id;
        let ((), logs) = crate::test_support::capture_logs(async {
            let mut stream = app
                .request(Method::POST, &format!("/api/v1/projects/{project}/sql"))
                .header("cookie", &seeded.cookie)
                .header("accept", "text/csv")
                .json(&example())
                .send_stream()
                .await;
            assert_eq!(stream.status, StatusCode::OK);
            assert!(stream.next_frame(Duration::from_secs(10)).await.is_some());

            let busy = post_as(
                &app,
                project,
                ("cookie", &seeded.cookie),
                None,
                &json!({"query": "SELECT 1 AS one"}),
            )
            .await;
            assert_eq!(busy.status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(busy.problem().code, ProblemCode::Busy);

            drop(stream);
            let free = post_as(
                &app,
                project,
                ("cookie", &seeded.cookie),
                None,
                &json!({"query": "SELECT 1 AS one"}),
            )
            .await;
            assert_eq!(free.status, StatusCode::OK);
        })
        .await;
        assert!(logs.contains("cancelled"), "{logs}");
    }

    #[tokio::test]
    async fn export_lines_carry_sizes_not_text() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let project = seeded.project.id;
        let csv = Some("text/csv");
        let ((), logs) = crate::test_support::capture_logs(async {
            let ok = post_as(
                &app,
                project,
                ("cookie", &seeded.cookie),
                csv,
                &json!({"query": "SELECT 'secret_marker_text' AS s"}),
            )
            .await;
            assert_eq!(ok.status, StatusCode::OK);
            assert_eq!(ok.body.as_ref(), b"s\r\nsecret_marker_text\r\n");

            let bad = post_as(
                &app,
                project,
                ("cookie", &seeded.cookie),
                csv,
                &json!({"query": "SELECT secret_marker_text FROM spans"}),
            )
            .await;
            assert_eq!(bad.status, StatusCode::BAD_REQUEST);

            let denied = post_as(
                &app,
                project,
                ("cookie", &viewer.cookie),
                csv,
                &json!({"query": "SELECT 1"}),
            )
            .await;
            assert_eq!(denied.status, StatusCode::FORBIDDEN);
        })
        .await;
        assert!(logs.matches("sql query").count() >= 3, "{logs}");
        assert!(logs.contains("bytes=23"), "{logs}");
        assert!(logs.contains("format=") && logs.contains("csv"), "{logs}");
        assert!(logs.contains("kind="), "{logs}");
        assert!(logs.contains("kind=\"forbidden\""), "{logs}");
        let success = logs
            .lines()
            .find(|line| line.contains("bytes=23"))
            .expect("the success line");
        assert!(!success.contains("kind="), "{success}");
        assert!(!logs.contains("secret_marker_text"), "{logs}");
        assert!(!logs.contains("CAST("), "{logs}");
    }
}
