# H4 — SQL console endpoint (JSON)

**Commit subject:** `server: add the SQL console endpoint`

Adds `POST /api/v1/projects/{project}/sql` with JSON results (spec 05 § SQL console, spec 09, D98, D101, D-H-8), over A2's `QueryContext::sql_json`. H5 adds the CSV, NDJSON and Parquet exports on the same route.

## Read first

- README § Design → Access and § SQL console endpoint, § Contracts between sub-phases (operation `run_sql`, `SqlQueryRequest`, `SqlQueryResult`, `SqlColumnInfo`, the two fixture files).
- `docs/spec/05-query.md` § SQL console; `docs/spec/09-api.md` § Conventions (errors) and the `POST /sql` row.
- `crates/storage/src/sql/run.rs` (`SqlRequest`, `SqlJson`, `SqlColumn`, `SQL_MAX_ROWS`, `QueryContext::sql_json`), `crates/storage/src/views/mod.rs` (`TimeWindow::new`, `QueryContext::new`), `crates/storage/src/query_error.rs` (`RunError`, `InvalidQuery`, `Position`, `RunError::kind`), `crates/storage/src/lib.rs` (`Storage::query`, `QueryOptions`).
- `crates/server/src/http/error.rs`: `ApiError`, `query_problem`, `IntoResponse for ApiError`, `From<QueryError>`.
- `crates/server/src/routes/scores.rs` and `routes/read.rs`: handler style, `open_query`.
- `crates/server/src/api/openapi.rs`: `collect_schemas`, `paths`, `score_paths` (an operation with a body), `problem`.
- `crates/server/src/api_examples.rs` and `crates/server/tests/api_examples.rs`: how `fixtures/api` is generated and checked.
- `crates/server/src/test_support/mod.rs`: `TestApp`, `seed`, `load_example_dataset`, `add_member`, `personal_token` (H3), `assert_logs_clean`.

## Files

| File | Change |
|---|---|
| `crates/server/src/api/sql.rs` | new: `SqlQueryRequest`, `SqlQueryResult`, `SqlColumnInfo` |
| `crates/server/src/api/mod.rs` | `pub mod sql;` (alphabetical, after `sessions`) |
| `crates/server/src/http/error.rs` | `ApiError::Sql`; `query_problem` takes the field name |
| `crates/server/src/routes/sql.rs` | new: route, request checks, handler, tests |
| `crates/server/src/routes/mod.rs` | `pub(crate) mod sql;` (after `sessions`) |
| `crates/server/src/routes/read.rs` | `open_query_on` takes a window; `open_query_windowed` |
| `crates/server/tests/openapi.rs` | tag, operation and expected errors of `run_sql` |
| `crates/server/src/http/access.rs` | `routes.extend(crate::routes::sql::routes());` after the `saved_views` line |
| `crates/server/src/api/openapi.rs` | tag `sql`, three schemas, `sql_paths` |
| `crates/server/src/api_examples.rs` | generate `static/sql-result.json` and its manifest row |
| `crates/server/tests/api_examples.rs` | check the two new fixture files |
| `fixtures/api/requests/sql-query.json` | new (hand-written, below) |
| `fixtures/api/static/sql-result.json`, `fixtures/api/manifest.json` | written by `just api-examples` |
| `fixtures/api/README.md` | Layout table: new row `` `static/sql-result.json` `` written by `just api-examples` (the real `run_sql` handler's answer to `requests/sql-query.json`); the `static/*.json` row becomes "hand, except `static/sql-result.json`"; the sentence "Do not edit `responses/` or `manifest.json` by hand" becomes "Do not edit `responses/`, `manifest.json` or `static/sql-result.json` by hand". |
| `crates/server/openapi.json`, `web/src/api/schema.gen.ts` | written by `just openapi` |

## 1. API types (`api/sql.rs`)

Module doc: `//! SQL console request and JSON result (spec 05 § SQL console, spec 09, D98).`

```rust
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::ToSchema;

/// Body of `POST /api/v1/projects/{project}/sql`.
///
/// Spec 09 § Observability data, D-H-8.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SqlQueryRequest {
    /// One statement, with `{name:Type}` placeholders (spec 05 § Parameters).
    pub query: String,
    /// Values for the placeholders, by name. Default empty.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub params: Map<String, Value>,
    /// Start of the time window, RFC 3339, inclusive. Give both `from` and `to`, or neither.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub from: Option<String>,
    /// End of the time window, RFC 3339, exclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub to: Option<String>,
    /// Most rows to return as JSON, 1–10,000. Default 10,000. Checked on every request; exports do not use it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 1, maximum = 10000)]
    pub limit: Option<i64>,
    /// Time limit in milliseconds, 1,000–60,000. Default 60,000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 1000, maximum = 60000)]
    pub timeout_ms: Option<i64>,
}

/// One output column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SqlColumnInfo {
    /// Output column name.
    pub name: String,
    /// Arrow type as DataFusion prints it, for example `Int64` or `Utf8`.
    #[serde(rename = "type")]
    pub data_type: String,
}

/// JSON answer of `POST …/sql` (D98).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SqlQueryResult {
    /// Output columns, in order.
    pub columns: Vec<SqlColumnInfo>,
    /// One object per row, keyed by column name. Timestamps are RFC 3339 with 9 fractional
    /// digits, binary is lowercase hex, maps are objects, NaN and infinities are null.
    #[schema(value_type = Vec<Object>)]
    pub rows: Vec<Map<String, Value>>,
    /// More rows matched than `limit`; `rows` holds the first `limit`.
    pub truncated: bool,
}
```

`limit` and `timeout_ms` are `i64` so that `0` or a negative number reaches the handler's field check (a `u32` would fail in serde with a less precise message). A JSON value of the wrong type (a number for `from`, a string for `limit`, a non-string `query`) fails in serde and answers `400 bad_request` with detail `invalid JSON body at <field>` from the existing `error::Json` extractor, not a field error.

In the OpenAPI document, `required` of `SqlQueryRequest` is `["query"]`, and `from`/`to` are `type: string, format: date-time`, not nullable (`drop_null_from_omitted_optionals` removes the null, as for `CreateApiKeyRequest.expires_at`). A unit test checks this (§ 6).

Imports of `api/sql.rs`: `serde::{Deserialize, Serialize}`, `serde_json::{Map, Value}`, `utoipa::ToSchema`.

Unit test in this file (`#[cfg(test)] mod tests`): `request_rejects_unknown_fields_and_defaults_params` — `{"query":"SELECT 1","foo":1}` fails to deserialize; `{"query":"SELECT 1"}` gives empty `params`, `from == None`, `limit == None`.

## 2. `ApiError::Sql` (`http/error.rs`)

The existing `ApiError::Query` maps `RunError::Invalid` to a field error named `filter`. A SQL text error must name `query`. A separate variant is simpler than a field argument on `Query`: about ten read handlers build `Query` through `From<RunError>` and `?`, and none of them would change.

1. Add the variant after `Query`:

```rust
    /// A2 refused or failed a SQL console statement (D94). Like [`Self::Query`], but an
    /// invalid statement is reported on the `query` field.
    #[error("sql error")]
    Sql(#[source] RunError),
```

2. Change `fn query_problem(error: &RunError) -> Problem` to `fn query_problem(error: &RunError, field: &str, invalid_detail: &str) -> Problem`. In the `RunError::Invalid` arm use `invalid_detail` as the detail and `field` as `FieldError.field`. Nothing else in it changes.
3. In `ApiError::problem`: `Self::Query(error) => query_problem(error, "filter", "invalid filter"),` and `Self::Sql(error) => query_problem(error, "query", "invalid query"),`.
4. In `IntoResponse for ApiError`, the logging block `if let Self::Query(error) = &self` becomes `if let Self::Query(error) | Self::Sql(error) = &self`.
5. `denial_marker` in `http/access.rs` matches by variant with a `_` arm: no change.

Add `#[tokio::test]` unit tests in `error.rs`'s existing test module (`mod tests`, near the end of the file). `sql_errors_name_the_query_field` — `ApiError::Sql(RunError::Invalid(InvalidQuery { message: "bad".into(), position: Some(Position { line: 1, column: 3 }) })).into_response()` has status 400, body `code == "invalid_filter"`, `detail == "invalid query"`, `errors[0].field == "query"`, `errors[0].position == {line: 1, column: 3}`; and the same error through `ApiError::Query` still reports field `filter` and detail `invalid filter`. `sql_timeout_and_memory_map_to_422` — `ApiError::Sql(RunError::Timeout)` → 422 `query_timeout`; `ApiError::Sql(RunError::TooLarge)` → 422 `query_too_large`; `ApiError::Sql(RunError::Failed("x".into()))` → 400 `query_failed` with detail `x`. Read each body with `axum::body::to_bytes(response.into_body(), usize::MAX).await` and parse a `Problem`.

## 3. Route and handler (`routes/sql.rs`)

Module doc: `//! SQL console (spec 05 § SQL console, spec 09, D-H-8).`

Imports: `std::time::{Duration, Instant}`, `axum::extract::{DefaultBodyLimit, Extension, State}`, `axum::http::Method`, `axum::routing::post`, `llmobs_core::ids::ProjectId`, `llmobs_core::time_serde::parse_rfc3339`, `llmobs_storage::{SQL_MAX_ROWS, SqlRequest, TimeWindow}`, `serde_json::{Map, Value}`, `crate::api::sql::{SqlColumnInfo, SqlQueryRequest, SqlQueryResult}`, `crate::auth::roles::ProjectPerm`, `crate::http::AppState`, `crate::http::access::{Access, ProjectAccess, RouteDef}`, `crate::http::error::{self, ApiError, field_error}`, `crate::routes::read::open_query_windowed`. Write `Instant::now()`, not `std::time::Instant::now()` (lint `unused_qualifications`). Drop any the compiler reports unused.

```rust
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
```

The default and largest JSON row count is `llmobs_storage::SQL_MAX_ROWS` (10,000, D98); use that constant, not a copy.

### Request checks

```rust
/// A console request after its fields are checked.
#[derive(Debug)]
struct CheckedSql {
    query: String,
    params: Map<String, Value>,
    window: Option<TimeWindow>,
    limit: usize,
    timeout: Duration,
}

/// D-H-8's field rules. The first failing rule wins, in this order: query, from/to, limit,
/// timeout_ms. Each failure is `400 bad_request` with one field error.
fn check_request(body: SqlQueryRequest) -> Result<CheckedSql, ApiError>
```

| Rule | Field | Message |
|---|---|---|
| `query.trim().is_empty()` | `query` | `query must not be empty` |
| `from` given, `to` missing | `to` | `from and to must be given together` |
| `to` given, `from` missing | `from` | `from and to must be given together` |
| `from` does not parse with `llmobs_core::time_serde::parse_rfc3339` | `from` | `expected an RFC 3339 timestamp` |
| same for `to` | `to` | `expected an RFC 3339 timestamp` |
| `from >= to` (nanoseconds) | `from` | `from must be before to` |
| `limit` outside 1–10,000 | `limit` | `limit must be between 1 and 10000` |
| `timeout_ms` outside 1,000–60,000 | `timeout_ms` | `timeout_ms must be between 1000 and 60000` |

Defaults: `limit` = `SQL_MAX_ROWS`, `timeout_ms` = 60,000. Conversions without `unwrap`:
- `limit`: `usize::try_from(limit)` (a negative value fails → the `limit` field error), then check `1..=SQL_MAX_ROWS`.
- `timeout_ms`: check `MIN_TIMEOUT_MS..=MAX_TIMEOUT_MS` on the `i64`, then `Duration::from_millis(u64::try_from(timeout_ms).map_err(|_| field_error("timeout_ms", …))?)` — the range check makes the conversion infallible; keep the `map_err` instead of an `expect`.
- Window: when both bounds parse and `from < to`, `window = Some(TimeWindow::new(from, to)?)` (it cannot fail then; `?` turns a `RunError` into `ApiError::Query`, which is fine). `parse_rfc3339` accepts any offset (`+01:00`) and converts to UTC.

The `query` is passed on untrimmed. Field errors use H3's shared `crate::http::error::field_error(field, message)` (a `400 bad_request` problem without `detail`).

### Opening the context

In `routes/read.rs`, give `open_query_on` the window instead of copying it:

```rust
/// [`open_query`] for a caller that holds the storage but not the whole state, such as a
/// stream task (the task must not keep an `AppState` alive). `window` is D101's.
pub(crate) async fn open_query_on(
    storage: &Storage,
    project: ProjectId,
    around: Option<i64>,
    window: Option<TimeWindow>,
) -> Result<QueryContext, ApiError> {
    let source = storage.query(project, QueryOptions { around }).await?;
    Ok(QueryContext::new(source, window)?)
}

/// [`open_query`] with D101's window written into the views. The SQL console uses it.
pub(crate) async fn open_query_windowed(
    state: &AppState,
    project: ProjectId,
    window: Option<TimeWindow>,
) -> Result<QueryContext, ApiError> {
    open_query_on(&state.storage, project, None, window).await
}
```

`open_query` passes `None` for the window, and the other callers of `open_query_on` (find them with `grep -rn "open_query_on(" crates/server/src`) add `, None`.

`Storage::query` waits for memory admission (`storage.query.admission_wait`, default 5 s) before returning; that wait is not part of `timeout_ms`, which runs from planning on (`sql_json` wraps planning and execution). `QueryError::Busy` becomes `503 busy`.

### Handler

```rust
async fn run_sql(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Json(body): error::Json<SqlQueryRequest>,
) -> Result<error::Json<SqlQueryResult>, ApiError>
```

The handler logs exactly one line per request that reaches it, when the request ends, whatever the outcome (D-H-8, README § SQL console endpoint). Requests rejected before the handler (access 401/403/404, body 400/413/415) are not logged here. Structure it as:

```rust
    let started = Instant::now();
    let query_len = body.query.len();
    let result = run_json(&state, &access, body).await;
    log_end(access.project.id, "json", query_len, started, &result);
    result.map(error::Json)
```

```rust
/// Checks the request and runs it as JSON.
async fn run_json(state: &AppState, access: &ProjectAccess, body: SqlQueryRequest) -> Result<SqlQueryResult, ApiError>
```

1. `let checked = check_request(body)?;`
2. `let ctx = open_query_windowed(state, access.project.id, checked.window).await?;`
3. `let json = ctx.sql_json(llmobs_storage::SqlRequest { query: &checked.query, params: &checked.params, limit: checked.limit, timeout: checked.timeout }).await.map_err(ApiError::Sql)?;` If clippy reports `large_futures`, wrap the future in `Box::pin(...)`; add no `#[allow]`.
4. Return `SqlQueryResult { columns: json.columns.into_iter().map(|column| SqlColumnInfo { name: column.name, data_type: column.data_type }).collect(), rows: json.rows, truncated: json.truncated }`.

```rust
/// One `info` line per request: ids, sizes, duration, and the failure kind. Never the query
/// text, parameters or values.
fn log_end(project_id: ProjectId, format: &'static str, query_len: usize, started: Instant, result: &Result<SqlQueryResult, ApiError>)
```

- `Ok(result)` → `tracing::info!(project_id = %project_id, format, query_len, rows = result.rows.len(), truncated = result.truncated, duration_ms, "sql query");` (JSON results have no byte cap beyond 10,000 rows; README lists this as a known limit.)
- `Err(error)` → `tracing::info!(project_id = %project_id, format, query_len, kind = failure_kind(error), duration_ms, "sql query");`
- `duration_ms` is `u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)`.

```rust
/// The `kind` field of a failed request.
fn failure_kind(error: &ApiError) -> &'static str
// ApiError::Sql(e) | ApiError::Query(e) => e.kind(); BadRequest { .. } => "bad_request";
// Unavailable { .. } => "busy"; Forbidden { .. } => "forbidden" (H5's export role check
// returns it from inside the handler); anything else => "error".
```

That is the only line the handler writes. `ApiError::into_response` still writes its own `query refused` / `query failed` line for `Sql` errors, as it does for every handler; that line is not the handler's and stays.

No experiment or dataset views are registered (README § Scope): a query naming `experiment_items` fails like any unknown table.

## 4. OpenAPI (`api/openapi.rs`)

1. Import `super::sql::{SqlColumnInfo, SqlQueryRequest, SqlQueryResult}`.
2. In `collect_schemas`, after `set.insert::<MetricsResponse>();`: `set.insert::<SqlQueryRequest>(); set.insert::<SqlQueryResult>(); set.insert::<SqlColumnInfo>();`.
3. In the tags list, after `saved_views`: `tag("sql", "SQL console over the project's views."),`.
4. New function, called in `paths()` after `saved_view_paths`:

```rust
fn sql_paths(paths: PathsBuilder) -> PathsBuilder {
    let run = operation("run_sql", "sql", "Run a SQL query")
        .description(Some(
            "Runs one read-only statement over the project's `spans`, `traces`, `sessions` and `scores` views (spec 05 § SQL console). `from` and `to` window `spans` and `traces` on `start_time` and `scores` on `created_at`. Without them the query reads all history. At most 10,000 rows; `truncated` says when more matched. SQL errors are `invalid_filter` on the `query` field, with a `position` when one is known.",
        ))
        .parameters(Some(vec![project_param()]))
        .request_body(Some(json_body("SqlQueryRequest")))
        .responses(responses(
            "200",
            json_of::<SqlQueryResult>("Columns and rows."),
            &[400, 401, 403, 404, 413, 415, 422, 429, 503],
        ))
        .build();
    paths.path(format!("{PROJECT}/sql"), PathItem::new(HttpMethod::Post, run))
}
```

The `415` description arm in `problem()` exists since H3.

In `crates/server/tests/openapi.rs`: add `"sql"` to `TAGS` (after `"saved_views"`); add `("post", "/api/v1/projects/{project}/sql", "run_sql")` to `OPERATIONS` (after the saved-view rows); in `document_lint`, before the `READ_OPERATIONS` branch, add `} else if *id == "run_sql" { expected_errors.extend(["400", "401", "403", "404", "413", "415", "422", "429"]);` (only 4xx codes are compared; 503 is declared but not checked there). H5 declares no other status.

Then `just openapi`.

## 5. Fixtures

### `fixtures/api/requests/sql-query.json` (exact text, trailing newline)

```json
{
  "query": "SELECT span_kind, count(*) AS spans, min(start_time) AS first_start FROM spans WHERE environment = {env:String} GROUP BY span_kind ORDER BY span_kind",
  "params": {
    "env": "production"
  },
  "from": "2026-01-01T10:00:00Z",
  "to": "2026-01-01T11:00:00Z",
  "limit": 100
}
```

This must be the canonical form of a `SqlQueryRequest` (pretty JSON, field order of the struct): `request_examples_match_their_types_and_the_openapi_document` round-trips it.

### Generator (`src/api_examples.rs`)

1. Import `crate::api::sql::SqlQueryResult` and `serde_json::Value`.
2. Add a POST twin of `fetch`:

```rust
/// `POST` of `body` to the example project's `path`. Asserts `200` and parses the body as `T`.
async fn post_json<T: Serialize + DeserializeOwned>(
    app: &TestApp,
    cookie: &str,
    path: &str,
    body: &Value,
) -> T
```

It sends `app.request(Method::POST, &format!("/api/v1/projects/{EXAMPLE_PROJECT}{path}")).header("cookie", cookie).json(body)`.
3. In `generate()`, after the metrics example and before the static entries:

```rust
    let request: Value = read_json(&api_dir().join("requests/sql-query.json"));
    let sql: SqlQueryResult = post_json(&app, cookie, "/sql", &request).await;
    out.add("run_sql", &[], &[], "static/sql-result.json", &sql);
```

4. In `on_disk_files()`, after the manifest block, also load `static/sql-result.json` when it exists (same code as for `manifest.json`, key `"static/sql-result.json"`), so the up-to-date check compares it. `write_files` already writes every key it is given and only clears `responses/`.

The file lives in `static/` (README § Contracts) although it is generated: the web mocks serve `static/` and `responses/` alike, and the generator now owns this one static file.

### Checks (`tests/api_examples.rs`)

1. Import `llmobs_server::api::sql::{SqlQueryRequest, SqlQueryResult}`.
2. `static_files_match_their_types`: add `roundtrip::<SqlQueryResult>("static/sql-result.json");`.
3. `request_examples_match_their_types_and_the_openapi_document`: add `roundtrip::<SqlQueryRequest>("requests/sql-query.json");` and the pair `("run_sql", "requests/sql-query.json")` to `requests`.
4. `generated_files_roundtrip`: add the arm `"run_sql" => roundtrip::<SqlQueryResult>(&example.file),`.

`manifest_matches_the_operations` and `every_fixture_file_is_used` need no change: the `run_sql` row has no path parameters besides `project`, its 200 declares `application/json`, and it names the static file. The web mocks skip manifest rows of non-GET operations (`web/src/mocks/manifest.ts`), so nothing changes there.

### Expected content of `static/sql-result.json`

From the example dataset (`fixtures/api/README.md`), the production spans in `[10:00, 11:00)` give, in this order:

| `span_kind` | `spans` | `first_start` |
|---|---|---|
| `agent` | 2 | `2026-01-01T10:00:00.000000000Z` |
| `chain` | 1 | `2026-01-01T10:10:00.000000000Z` |
| `llm` | 5 | `2026-01-01T10:00:00.100000000Z` |
| `tool` | 1 | `2026-01-01T10:00:01.950000000Z` |

with `truncated: false`, and `columns[1] == {"name": "spans", "type": "Int64"}`. Only that column type is asserted; the `type` strings of `span_kind` and `first_start` are whatever DataFusion prints, and the generated file is authoritative for them. If the handler returns something else, STOP and report (the dataset or A2 differs from this plan).

## 6. Tests (`routes/sql.rs`)

All `#[tokio::test(flavor = "multi_thread")]` (except the log test, see 10), with `TestApp::new()`, `seed()` and `load_example_dataset(seeded.project.id)`. Path `format!("/api/v1/projects/{}/sql", seeded.project.id)`. Post with `.header("cookie", &seeded.cookie).json(&json!({...}))` unless the case says otherwise. Field errors: `status == 400`, `code == "bad_request"`, `detail` absent, `errors[0].field` and `errors[0].message` as in § Request checks.

1. `example_request_returns_the_example_result` — post the parsed `requests/sql-query.json` (`include_str!("../../../../fixtures/api/requests/sql-query.json")`); 200; the body as `SqlQueryResult` equals the parsed `static/sql-result.json` (`include_str!` too) and has the four rows of the table above (assert them explicitly: names, `spans` as `json!(2)`…, `first_start` strings).
2. `limit_truncates` — `{"query": "SELECT span_id FROM spans ORDER BY span_id", "limit": 3}` → 3 rows, `truncated == true`, first row `{"span_id": "b100000000000011"}`. `"limit": 1` → 1 row, truncated. `"limit": 13` → 13 rows, truncated. `"limit": 14` → 14 rows, `truncated == false` (exactly as many rows as the limit). `"limit": 10000` and no `limit` → 14 rows, `truncated == false`.
3. `window_filters_rows` — `{"query": "SELECT count(*) AS n FROM spans", "from": "2026-01-01T10:00:00Z", "to": "2026-01-01T10:05:00Z"}` → `rows == [{"n": 6}]` (trace 1's four spans and trace 5's two). The same window written with offsets, `"from": "2026-01-01T11:00:00+01:00", "to": "2026-01-01T11:05:00+01:00"` → `[{"n": 6}]`. Without a window → `[{"n": 14}]`. The window applies to the other views too, over the same `[10:00, 10:05)`: `SELECT count(*) AS n FROM traces` → `[{"n": 2}]` (traces 1 and 5 start in it); `SELECT count(*) AS n FROM scores` → `[{"n": 3}]` (scores created at 10:00:30, 10:00:40 and 10:04:30; `scores` is windowed on `created_at`). If storage gives other counts, STOP and report.
4. `params_bind_and_bad_params_fail` — `{"query": "SELECT count(*) AS n FROM spans WHERE environment = {env:String}", "params": {"env": "staging"}}` → `[{"n": 3}]`. With `"params": {}` → 400 `bad_request` with `detail == "missing value for parameter env"`. With `"params": {"env": "staging", "x": 1}` → 400, `detail == "unknown parameter x"`. With `"params": []` → 400 `bad_request` (serde rejects a non-object; `status` and `code` only).
5. `invalid_sql_names_the_query_field` — `{"query": "SELEC 1"}` → 400, `code == "invalid_filter"`, `detail == "invalid query"`, `errors[0].field == "query"`, `errors[0].position` present with `line == 1`. `{"query": "DROP TABLE spans"}` → 400 `invalid_filter`, field `query` (no assertion on `position`).
6. `execution_errors_are_query_failed` — `{"query": "SELECT 1 / 0 AS n"}` → 400 `query_failed`.
7. `request_fields_are_checked` — table-driven: `{"query": "  "}`; a valid `from` with `"to": "soon"` (→ `to`, `expected an RFC 3339 timestamp`); `"from": "9999-12-31T00:00:00Z"` with a valid `to` (out of the nanosecond range → `from`, `expected an RFC 3339 timestamp`); `"from": "2026-01-01T10:00:00Z", "to": "2026-01-01T11:00:00+01:00"` (the same instant → `from`, `from must be before to`); precedence: `{"query": " ", "limit": 0}` → `query`, and `{"query": "SELECT 1", "from": "x", "to": "2026-01-01T10:00:00Z", "limit": 0}` → `from`; `{"query": "SELECT 1", "from": "2026-01-01T10:00:00Z"}` (→ `to`); `{"query": "SELECT 1", "to": "2026-01-01T10:00:00Z"}` (→ `from`); `from` `"yesterday"` with a valid `to` (→ `from`, `expected an RFC 3339 timestamp`); `from == to` (→ `from`, `from must be before to`); `limit` `0`, `-1` and `10001`; `timeout_ms` `999` and `60001`. Accepted boundaries (200): `limit` `1` and `10000`, `timeout_ms` `60000` (the `1000` boundary is in the unit test 8b, so no HTTP query runs with a one-second budget). Plus `{"query": "SELECT 1", "foo": 1}`, `{"query": 1}` and `{"query": "SELECT 1", "from": 123, "to": "2026-01-01T10:00:00Z"}` → 400 `bad_request` (status and code only: serde rejects them); a body without `Content-Type` (use `.body(...)`) → 415; a body over 256 KiB (`{"query": "SELECT '<x repeated 270,000>'"}`) → 413 `payload_too_large`.
8. `read_credentials_can_query` — `{"query": "SELECT 1 AS one"}` answers 200 `[{"one": 1}]` for: a viewer's cookie (`app.add_member(&seeded, "viewer@example.com", OrgRole::Viewer)`), that viewer's read-only PAT (`app.personal_token(viewer.id, false)`), and `seeded.read_key`; it answers 403 for `seeded.ingest_key`. Use `authorization: Bearer …` for tokens and keys. (H3 adds these helpers; if they are missing, STOP.)
8b. `check_request_fills_defaults_and_the_window` (plain `#[test]`) — `check_request` of `{"query": "SELECT 1"}` gives `query == "SELECT 1"`, empty `params`, `window == None`, `limit == SQL_MAX_ROWS`, `timeout == Duration::from_secs(60)`. With `"from": "2026-01-01T10:00:00Z", "to": "2026-01-01T12:00:00+01:00", "limit": 5, "timeout_ms": 1000` it gives `limit == 5`, `timeout == Duration::from_millis(1000)`, and a window with `from() == 1_767_261_600_000_000_000` (10:00Z) and `to() == 1_767_265_200_000_000_000` (11:00Z). `"timeout_ms": 999` fails on `timeout_ms`.
8c. `request_schema_requires_only_query` (plain `#[test]`) — in `serde_json::to_value(crate::api::openapi::openapi())`, `components.schemas.SqlQueryRequest.required == ["query"]`, and `properties.from` and `properties.to` each have `type == "string"`, `format == "date-time"` (and `type` is not an array containing `"null"`).
9. `failure_kind_names_each_error` (plain `#[test]`; admission failures are hard to provoke over HTTP, so the mapping is tested directly): `Sql(RunError::Timeout)` → `"timeout"`, `BadRequest { .. }` → `"bad_request"`, `Unavailable { detail: "x" }` → `"busy"`, `Forbidden { code: ProblemCode::Forbidden }` → `"forbidden"`, `NotFound` → `"error"`.
10. `logs_carry_no_query_text` — `#[tokio::test]` (current thread: `capture_logs` installs its subscriber for the test's own thread, and `oneshot` runs the handler there). Inside `crate::test_support::capture_logs(async { … })` run `{"query": "SELECT 'secret_marker_text' AS s"}` (200), `{"query": "SELECT secret_marker_text FROM spans"}` (400) and `{"query": "  "}` (400). The log text contains `sql query` at least three times, `rows=1` once, `kind=` for the failures (`kind="invalid"` or `kind=invalid` depending on the formatter: assert on `kind=`), and does not contain `secret_marker_text`. Then, in a second `capture_logs`, send `{"query": 1}` (rejected by the extractor before the handler): that log text does not contain `sql query`.

The authorization suite runs `POST …/sql` against every caller by itself; after H1 it predicts the read PAT and the read key correctly. It must pass unchanged.

## Commands

```bash
export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH"
cargo test -p llmobs-server routes::sql
cargo test -p llmobs-server http::error
cargo test -p llmobs-server --test openapi
just openapi
just api-examples
git status --short fixtures/api   # new: requests/sql-query.json, static/sql-result.json; changed: manifest.json
cargo test -p llmobs-server --test api_examples
just check
```

## Done when

- `POST /api/v1/projects/{project}/sql` answers JSON per D98 and D-H-8, with the field checks above.
- `openapi.json` has `run_sql` (tag `sql`) and the three schemas; `schema.gen.ts` is regenerated.
- `fixtures/api/requests/sql-query.json`, `static/sql-result.json` and the `run_sql` manifest row exist and are checked.
- The tests above pass; `just check` passes. One commit, subject `server: add the SQL console endpoint`.

Report "no new dependency".
