# H5 — SQL exports

**Commit subject:** `server: stream SQL exports`

`POST /api/v1/projects/{project}/sql` (H4) also exports the whole result as CSV, NDJSON or Parquet, chosen by `Accept`, streamed through A2's `QueryContext::sql_export` (spec 05 § Sandbox item 8, D-H-5, D-H-8).

## Read first

- README § Design → Access (exports need role member) and § SQL console endpoint (`Accept`, headers, the first-item wait, timeout, memory admission, logging, mid-stream failure); § Contracts (test helpers).
- `crates/server/src/routes/sql.rs` (H4), all of it: `check_request`, `run_json`, `log_end`, `failure_kind`; and `routes/read.rs`'s `open_query_windowed`.
- `crates/storage/src/sql/run.rs`: `ExportFormat`, `EXPORT_MAX_ROWS`, `QueryContext::sql_export` and its doc comment (preparation errors are the outer `Err`; the deadline is measured from that call; the timeout and later execution errors are items of the stream; a CSV stream yields its header line first, before any batch is computed), and the test `the_row_cap_limits_each_format` (the 10M cap is tested there with a small cap; H5 does not test it again).
- `crates/server/src/test_support/mod.rs`: `TestApp` (`with_config`, `with_routes`), `TestRequest::send_stream`, `TestStream`, `capture_logs`, `add_member`, `personal_token` (H3; if they are missing, STOP).
- `crates/server/src/api/openapi.rs`: `sql_paths` (H4), `media_bytes`, `string_schema`.
- `crates/server/tests/openapi.rs`: `document_lint`'s `run_sql` branch (H4). H5 declares no new status, so it does not change.

## Files

| File | Change |
|---|---|
| `crates/server/src/routes/sql.rs` | `Accept` negotiation, the export branch, the first-item wait, `LoggedExport`, tests |
| `crates/server/src/test_support/mod.rs` | `TestStream::read_to_end`, `TestStream::next_frame`, `TestApp::serve_loopback` |
| `crates/server/src/api/openapi.rs` | `run_sql`'s 200 lists the export types; a shared binary schema helper |
| `crates/server/openapi.json`, `web/src/api/schema.gen.ts` | `just openapi` |

No new dependency: the Parquet test reads files back with `datafusion::parquet` (DataFusion re-exports the `parquet` crate; `datafusion` is already a server dev-dependency).

## 1. Negotiation

```rust
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
/// visible ASCII) is skipped; when nothing decides, JSON.
fn negotiate(headers: &HeaderMap) -> SqlFormat
```

Use `headers.get_all(axum::http::header::ACCEPT)` and skip values whose `to_str()` fails. `q` values are ignored on purpose (D-H-8).

```rust
impl SqlFormat {
    /// A2's format, `None` for JSON.
    fn export(self) -> Option<ExportFormat>
    /// `Content-Type` of the answer: application/json, text/csv, application/x-ndjson,
    /// application/vnd.apache.parquet.
    fn content_type(self) -> &'static str
    /// File name extension: json, csv, ndjson, parquet.
    fn extension(self) -> &'static str
    /// `format` log field: "json", "csv", "ndjson", "parquet".
    fn label(self) -> &'static str
}
```

## 2. Handler

`run_sql` becomes:

```rust
async fn run_sql(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    headers: HeaderMap,
    error::Json(body): error::Json<SqlQueryRequest>,
) -> Result<Response, ApiError>
```

(`HeaderMap` must come before the body extractor.) Order of checks: access layer → body extraction (400/413/415; a body serde rejects is a 400 even for a viewer asking for CSV) → `negotiate` → for an export, the role → `check_request` (field errors) → admission → preparation and the first item → stream.

1. `let format = negotiate(&headers);`, `let started = Instant::now();`, `let query_len = body.query.len();`.
2. JSON (`format.export()` is `None`): H4's path unchanged: `run_json`, `log_end`, answer `error::Json(result).into_response()`.
3. Export (`Some(export)`): `let result = start_export(&state, &access, body, format, export, started).await;`. On `Err(error)`: `log_failure(access.project.id, format.label(), query_len, started, &error)` and return the error (a problem document; no byte was sent). On `Ok(response)`: return it; the body logs the request's line when it ends (§ 3).

Split H4's `log_end` so failures share one function:

```rust
/// The request's line for a failure, any format.
fn log_failure(project_id: ProjectId, format: &'static str, query_len: usize, started: Instant, error: &ApiError)
// tracing::info!(project_id = %project_id, format, query_len, kind = failure_kind(error), duration_ms, "sql query");
```

`log_end` keeps the JSON success line and calls `log_failure` for `Err`.

```rust
/// Role, checks, admission, preparation and the first item; then the streaming response.
async fn start_export(
    state: &AppState,
    access: &ProjectAccess,
    body: SqlQueryRequest,
    format: SqlFormat,
    export: ExportFormat,
    started: Instant,
) -> Result<Response, ApiError>
```

1. Role (D-H-5), before the field checks: when `access.role` is `Some(role)` and `role < Role::Member`, return `Err(ApiError::Forbidden { code: ProblemCode::Forbidden })`. `role` is the effective project role (overrides included). `None` is an `sk` key, which the access layer already required to hold `read`: allowed. So a viewer whose body parses but has an invalid field (`"limit": 0`) gets 403.
2. `let checked = check_request(body)?;`
3. `let ctx = open_query_windowed(state, access.project.id, checked.window).await?;` (memory admission; `busy` → 503; the admission wait is outside `timeout_ms`).
4. Preparation and the first item, inside the request's time limit:

   ```rust
   let prepared = tokio::time::timeout(checked.timeout, async {
       let stream = ctx
           .sql_export(SqlRequest { query: &checked.query, params: &checked.params, limit: checked.limit, timeout: checked.timeout }, export)
           .await?;
       first_item(stream, format).await
   })
   .await;
   let stream = match prepared {
       Err(_elapsed) => return Err(ApiError::Sql(RunError::Timeout)),   // 422 query_timeout
       Ok(Err(error)) => return Err(ApiError::Sql(error)),              // 400/422 problem
       Ok(Ok(stream)) => stream,
   };
   ```

   Wrap the inner future in `Box::pin(...)` only if clippy reports `large_futures`; add no `#[allow]`.

   ```rust
   /// Pulls the stream's first data item (for CSV, the header and the item after it) so an
   /// error there becomes a problem document instead of a cut body (README § SQL console
   /// endpoint). Returns a stream that yields the pulled items first, then the rest.
   async fn first_item(
       mut stream: BoxStream<'static, Result<Bytes, RunError>>,
       format: SqlFormat,
   ) -> Result<BoxStream<'static, Result<Bytes, RunError>>, RunError>
   ```

   Pull `2` items for CSV and `1` otherwise with `StreamExt::next`: `Some(Ok(bytes))` → keep it; `Some(Err(error))` → `return Err(error)`; `None` → stop pulling (an empty result is fine). Return `futures::stream::iter(kept.into_iter().map(Ok)).chain(stream).boxed()`. For Parquet the first item is the first completed row group, which for a small result is the whole file; it is held in memory until sent.

   `sql_export` measures its own deadline from its call, so `timeout_ms` keeps bounding the stream after the response started: a deadline reached then ends the body with an error (D-H-8). The deadline is checked when the body is polled, so a client that stalls without disconnecting can hold the query's memory admission longer (README: accepted).
5. Drop `ctx`. The stream is `'static` and owns the query's runtime, so the memory admission is held until the stream ends or is dropped (client disconnect).
6. `export_response(stream, format, access.project.id, checked.query.len(), started, state.clock.now_ns())`.

```rust
/// The streaming answer of an export: headers and a [`LoggedExport`] body. Separate from
/// `start_export` so tests can feed it a stream that fails after its first bytes.
fn export_response(
    stream: BoxStream<'static, Result<Bytes, RunError>>,
    format: SqlFormat,
    project_id: ProjectId,
    query_len: usize,
    started: Instant,
    now_ns: i64,
) -> Result<Response, ApiError>
```

- Body: `Body::from_stream(LoggedExport::new(stream, project_id, format.label(), query_len, started))`.
- Headers: `Content-Type: format.content_type()`, `Content-Disposition: attachment; filename="query-<stamp>.<ext>"`, `Cache-Control: no-store`. `<stamp>` is `chrono::DateTime::from_timestamp_nanos(now_ns).format("%Y%m%dT%H%M%SZ")` (UTC). Build the values with `HeaderValue::from_str(&text).map_err(|error| ApiError::Internal(Box::new(error)))?` (the text is ASCII, so this does not fail in practice).
- `Ok((StatusCode::OK, headers, body).into_response())`.

`limit` is checked (H4's rule) but not applied to exports; the stream stops at `EXPORT_MAX_ROWS` (10,000,000) data rows (tested in storage).

## 3. `LoggedExport`

```rust
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
    Failed(&'static str),
}

impl LoggedExport {
    fn new(
        inner: BoxStream<'static, Result<Bytes, RunError>>,
        project_id: ProjectId,
        format: &'static str,
        query_len: usize,
        started: Instant,
    ) -> Self   // bytes 0, outcome Running
}
```

Implement `fmt::Debug` by hand (the workspace lint `missing_debug_implementations` is on, and `BoxStream` has no `Debug`): `formatter.debug_struct("LoggedExport").field("project_id", …).field("format", …).field("bytes", …).field("outcome", …).finish_non_exhaustive()`.

`impl Stream for LoggedExport { type Item = Result<Bytes, std::io::Error>; }` — `poll_next` polls `inner` with `poll_next_unpin` (the struct is `Unpin`: every field is):
- `Ready(Some(Ok(bytes)))` → add `bytes.len()` to `self.bytes`, yield it;
- `Ready(Some(Err(error)))` → `self.outcome = ExportOutcome::Failed(error.kind())`, yield `Err(std::io::Error::other("sql export failed"))` (fixed text: the `RunError` message may quote the query);
- `Ready(None)` → when `outcome` is `Running`, set it to `Done`; yield `None`;
- `Pending` → `Pending`.

A2's stream ends after its first error, so `Failed` is set at most once.

`impl Drop for LoggedExport` writes the line (never the query text, parameters or values), with `duration_ms` as in H4:
- `Done` → `tracing::info!(project_id = %self.project_id, format = self.format, query_len = self.query_len, bytes = self.bytes, duration_ms, "sql query");`
- `Failed(kind)` → the same fields plus `kind`; at `error!` level when `kind == "internal"`, `info!` otherwise;
- `Running` (dropped before the end: the client disconnected or hyper stopped reading) → the same fields plus `kind = "cancelled"`, at `info!`.

When the body yields an `Err`, hyper stops the response without the terminating chunk and drops the body; the client sees a truncated transfer and no extra bytes (D-H-8). Nothing here spawns a task: the body is polled by whoever serves the response. In tests that is the test's own task (`oneshot` and `TestStream`), which is why `capture_logs` sees these lines.

## 4. Test helpers (`test_support/mod.rs`)

```rust
impl TestStream {
    /// Reads data frames until the body ends or fails. Returns the bytes and whether the body
    /// ended with an error. Panics when it does not end within `wait`.
    pub(crate) async fn read_to_end(&mut self, wait: Duration) -> (Vec<u8>, bool)

    /// The next data frame, or `None` when the body ends, fails, or `wait` passes first.
    pub(crate) async fn next_frame(&mut self, wait: Duration) -> Option<Bytes>
}
```

`read_to_end`: loop on `self.body.frame()`: `Some(Ok(frame))` → `if let Ok(data) = frame.into_data() { bytes.extend_from_slice(&data) }` (`into_data` returns `Err` for a trailers frame, which is skipped); `Some(Err(_))` → return `(bytes, true)`; `None` → return `(bytes, false)`. Wrap the loop in `tokio::time::timeout(wait, …)` and `panic!("body did not end")` on timeout. `next_frame` is the same for one data frame. Update `TestStream`'s doc comment: it reads an SSE stream or any other streamed body.

```rust
impl TestApp {
    /// Serves this app's router on `127.0.0.1:<ephemeral>` in a background task and returns
    /// the address. For tests that need a real socket (how hyper ends a failed body).
    pub(crate) async fn serve_loopback(&self) -> SocketAddr
}
```

Bind `tokio::net::TcpListener::bind("127.0.0.1:0")`, read `local_addr()`, then `tokio::spawn(async move { let _ = axum::serve(listener, router.into_make_service_with_connect_info::<SocketAddr>()).await; })` with `router = self.router.clone()` (`ConnectInfo` is required by the client-IP layer). The task ends with the test's runtime.

## 5. OpenAPI (`api/openapi.rs`)

1. Extract the binary string schema that `media_bytes()` builds into

```rust
/// `type: string, format: binary`.
fn binary_string() -> ObjectBuilder {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .format(Some(SchemaFormat::KnownFormat(KnownFormat::Binary)))
}
```

and make `media_bytes()` use `binary_string().description(Some(…same text…))`, so `get_media`'s document does not change.

2. Replace `run_sql`'s 200 with:

```rust
fn sql_result() -> Response {
    ResponseBuilder::new()
        .description("Columns and rows as JSON. With `Accept: text/csv`, `application/x-ndjson` or `application/vnd.apache.parquet`, the whole result in that format instead (at most 10,000,000 rows; `limit` is validated but not applied; exports need role member), with `Content-Disposition: attachment`. The first of those types in `Accept`, or `application/json`, decides; anything else is JSON.")
        .content("application/json", Content::new(Some(Ref::from_schema_name(SqlQueryResult::name()))))
        .content("text/csv", Content::new(Some(string_schema())))
        .content("application/x-ndjson", Content::new(Some(string_schema())))
        .content("application/vnd.apache.parquet", Content::new(Some(binary_string())))
        .build()
}
```

and use `responses("200", sql_result(), &[400, 401, 403, 404, 413, 415, 422, 429, 503])`. `application/json` is listed first (README).

3. Append to `run_sql`'s description: ` Exports stream; an error before the first row is a problem document, a later one cuts the transfer. \`timeout_ms\` bounds the whole export.` There is no `Accept` parameter in the document: negotiation is described in these texts.

`json_of::<SqlQueryResult>` is no longer used there; remove an import only if the compiler reports it unused. Then `just openapi`. `fixtures/api` does not change (`examples_match_the_openapi_document` validates the `application/json` content of the 200, same schema as before).

## 6. Tests (`routes/sql.rs`)

`EXAMPLE` is the parsed `fixtures/api/requests/sql-query.json`; `EXAMPLE_LIMIT_1` is the same with `"limit": 1`. HTTP tests are seeded with the example dataset loaded and use Ada's cookie unless stated; tokens and keys go in `authorization: Bearer <secret>`. Tests that read logs are `#[tokio::test]` (current thread); the others `#[tokio::test(flavor = "multi_thread")]`.

The exact bodies below come from the example dataset and A2's encoders. If one differs only in how A2 encodes a value (CSV quoting, NDJSON number form, Parquet column names), STOP and report; do not change the expectation to match.

1. `negotiate_reads_accept_in_order` (plain `#[test]`) — build a `HeaderMap` per case:

| `Accept` value(s) | Format |
|---|---|
| none | Json |
| `*/*` | Json |
| `text/csv` | Csv |
| `TEXT/CSV` | Csv |
| `application/json, text/csv` | Json |
| `text/csv;q=0.1, application/json` | Csv |
| `text/html, application/x-ndjson` | Ndjson |
| `application/vnd.apache.parquet` | Parquet |
| two headers: `text/html` then `text/csv` (`HeaderMap::append`) | Csv |
| two headers: `HeaderValue::from_bytes(&[0xC3, 0xA9]).unwrap()` (not visible ASCII) then `text/csv` | Csv |

2. `csv_export_streams_every_row` — `EXAMPLE_LIMIT_1` with `accept: text/csv` → 200; `content-type == "text/csv"`; `content-disposition` equals `format!("attachment; filename=\"query-{}.csv\"", stamp)` where `stamp` is `chrono::DateTime::from_timestamp_nanos(app.state.clock.now_ns()).format("%Y%m%dT%H%M%SZ")` (read the clock in the test); `cache-control == "no-store"`; body exactly

```
span_kind,spans,first_start\r\nagent,2,2026-01-01T10:00:00.000000000Z\r\nchain,1,2026-01-01T10:10:00.000000000Z\r\nllm,5,2026-01-01T10:00:00.100000000Z\r\ntool,1,2026-01-01T10:00:01.950000000Z\r\n
```

(four rows although `limit` is 1).
3. `ndjson_export_matches_the_json_rows` — `EXAMPLE` with `accept: application/x-ndjson` → 200, `content-type == "application/x-ndjson"`, filename ends `.ndjson"`; split the body on `\n`, drop the empty last piece, parse each line as `serde_json::Value`: the list equals the `rows` of `fixtures/api/static/sql-result.json`, in order.
4. `parquet_export_reads_back` — `EXAMPLE` with `accept: application/vnd.apache.parquet` → 200; the body starts and ends with `b"PAR1"`; `datafusion::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(body))` builds a reader whose schema field names are `["span_kind", "spans", "first_start"]` and whose batches hold 4 rows in total. Also `{"query": "SELECT span_id FROM spans WHERE span_id = 'none'"}` (zero rows) as Parquet → 200, a readable file with field `span_id` and 0 rows.
5. `exports_need_member` — `{"query": "SELECT 1 AS one"}` with `accept: text/csv`:
   - a viewer's cookie (`app.add_member(&seeded, "viewer@example.com", OrgRole::Viewer)`) → 403 `forbidden`;
   - the viewer with `{"query": "SELECT 1", "limit": 0}` → 403 (role before field checks); the viewer with `{"query": "SELECT 1", "foo": 1}` → 400 (the body extractor runs first);
   - that viewer's read-only PAT (`app.personal_token(viewer.id, false)`) → 403;
   - the viewer without `Accept` → 200 JSON;
   - a member's cookie (`add_member(…, "member@example.com", OrgRole::Member)`) → 200 `text/csv`, body `one\r\n1\r\n`; that member's read-only PAT → 200 `text/csv`;
   - another member, `member2@example.com`, with a project override `viewer` (`crate::meta::members::set_project_override(conn, seeded.project.id, member2.id, ProjectOverride::Viewer)` in a `meta.write`) → 403;
   - `seeded.read_key` → 200 `text/csv`.
6. `preparation_errors_are_problems` — `accept: text/csv`: `{"query": "SELEC 1"}` → 400, `content-type == "application/problem+json"`, `code == "invalid_filter"`, field `query`; `{"query": "SELECT 1", "limit": 0}` → 400 field `limit`; `{"query": "SELECT 1", "params": {"x": 1}}` → 400 `bad_request`, `detail == "unknown parameter x"`.
7. `a_failing_first_batch_is_a_problem` — `{"query": "SELECT CAST(name AS INT) AS n FROM spans"}` with `accept: text/csv` → 400 `query_failed` problem (`content-type == "application/problem+json"`), and the same with `accept: application/x-ndjson` → 400 `query_failed`. The cast of `name` (a column, so not folded at planning) fails when the first batch is computed, which `first_item` waits for. A2 classifies a failed cast as `RunError::Failed` (kind `failed`).
8. `a_failure_after_the_first_bytes_ends_the_body_with_an_error` (`#[tokio::test]`) — no query can be made to fail deterministically after its first batch over the 14-span dataset, so feed `export_response` a stream directly: `futures::stream::iter(vec![Ok(Bytes::from_static(b"n\r\n")), Ok(Bytes::from_static(b"1\r\n")), Err(RunError::Failed("boom".to_owned()))]).boxed()` with `SqlFormat::Csv`, a random `ProjectId`, `query_len` 10, `Instant::now()`, and `0` for `now_ns`. Inside `capture_logs`, read the response body with `http_body_util::BodyExt::frame`: two data frames `n\r\n` and `1\r\n`, then an error frame. The log text contains `sql query`, `bytes=6` and `kind=` and does not contain `boom`. This proves the body ends with an error frame; test 9 checks what a client sees.
9. `a_real_client_sees_a_cut_transfer` — `TestApp::with_routes(Router::new().route("/test/export", get(|| async { export_response(<the stream of test 8>, SqlFormat::Csv, ProjectId::from_uuid(Uuid::now_v7()), 10, Instant::now(), 0) })))`, then `let addr = app.serve_loopback().await;` and with `reqwest::Client::builder().no_proxy().build().unwrap()`: `GET http://{addr}/test/export` → status 200 and `response.bytes().await` is an `Err` (the chunked body ended without its last chunk). (`llmobs_core::ids::ProjectId::from_uuid` exists.)
10. `a_client_that_goes_away_releases_the_query` — `#[tokio::test]`, on `TestApp::with_config(|config| { config.storage.query.max_memory_total = Some(ByteSize::mib(64)); config.storage.query.max_memory = ByteSize::mib(64); config.storage.query.initial_memory = ByteSize::mib(64); config.storage.query.admission_wait = Duration::from_millis(200); })` (`bytesize::ByteSize`), so only one query is admitted at a time; if storage rejects this config, STOP and report. Inside `capture_logs`: send `EXAMPLE` with `accept: text/csv` through `.send_stream()` and read one frame with `next_frame(Duration::from_secs(10))`; while that stream is held, `{"query": "SELECT 1 AS one"}` as JSON → 503 `busy`; drop the `TestStream`; the same JSON query → 200. The log text contains `cancelled`.
11. `export_lines_carry_sizes_not_text` — `#[tokio::test]`. Inside `capture_logs`, in this order, each response read fully and dropped inside the block: a CSV export of `{"query": "SELECT 'secret_marker_text' AS s"}` (200; its body `s\r\nsecret_marker_text\r\n` is 23 bytes); a CSV export of `{"query": "SELECT secret_marker_text FROM spans"}` (400); a CSV export of `{"query": "SELECT 1"}` by a viewer (403). The log text contains `sql query` at least three times, `bytes=23`, `format="csv"` or `format=csv` (assert `contains("format=")` and `contains("csv")`), `kind=` for the two failures and `forbidden` for the 403; the success line has no `kind=` (check the line that contains `bytes=23`); the text contains neither `secret_marker_text` nor `CAST(`.

Not tested here: a timeout after the response started. Through the API `timeout_ms` is at least 1,000 ms and the example queries finish in a few milliseconds, so it cannot be made deterministic; A2's export tests cover the stream deadline. The 10M row cap is A2's `the_row_cap_limits_each_format`.

Keep H4's tests; they still pass (no `Accept` header means JSON).

## Commands

```bash
export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH"
cargo test -p llmobs-server routes::sql
cargo test -p llmobs-server --test openapi
just openapi
cargo test -p llmobs-server --test api_examples
just fmt
just check
```

## Done when

- `Accept` selects CSV, NDJSON or Parquet; exports stream every row up to 10M with the three headers; callers whose effective role is below member (sessions and PATs) get 403 before the field checks; `read` keys may export.
- A preparation error, an error in the first data item, or running out of time before it, is a problem document; a later failure ends the body with an error, and a real client sees a cut transfer.
- Every request that reaches the handler writes one `sql query` line; exports write it when the stream ends, fails or is dropped.
- `openapi.json` lists the four 200 content types; `get_media` is unchanged.
- The tests pass; `just check` passes. One commit, subject `server: stream SQL exports`.

Report "no new dependency".
