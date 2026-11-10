# A2 · 03 — SQL console (Q10–Q13)

Read [README.md](README.md) first: decisions D91–D101, boundaries, verified DataFusion facts, crate layout, public API.

The SQL console is `POST /api/v1/projects/{p}/sql`. The MCP tool `query_sql` and `llmobs query` call the same functions. A2 provides the sandbox, execution and encoding; the server does HTTP.

---

## Q10 — Statement validation

**Goal:** caller SQL becomes a checked `LogicalPlan`, or an `InvalidQuery` with a position.

**Edit only:** new `crates/storage/src/sql/{mod.rs,validate.rs}`, `crates/storage/src/views/mod.rs` (`SqliteView`, `register_sqlite_view`, the set of registered view names), `crates/storage/src/lib.rs`.

**Commit:** `Validate SQL console statements`

### SQLite-backed views

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteView { ExperimentItems, DatasetItems }
impl QueryContext {
    pub fn register_sqlite_view(&self, view: SqliteView, table: Arc<dyn TableProvider>) -> Result<(), RunError>;
}
```

- It registers under `ViewName::ExperimentItems.as_str()` / `DatasetItems`.
- Registering the same view twice is `RunError::InvalidRequest("<name> is already registered")`, a caller bug the server should never hit.
- The server owns these tables' schemas and rows (D93).

### Behaviour

`pub(crate) async fn plan_sql(ctx: &QueryContext, text: &str) -> Result<LogicalPlan, InvalidQuery>`. Q11 inserts parameter binding between steps 1 and 2.
1. `tokenize(text, &SQL_LIMITS, TextKind::Sql)`.
2. `parser(tokens)`, then `parse_statement()` (failure → `parse_failure`). Accept one optional `;`, then `expect_end`. A second statement fails with "only one statement is allowed", at its first token.
3. `validate_statement(&statement, &registered_names)` (`validate.rs`):

   | Statement | Rule |
   |---|---|
   | `Statement::Query(q)` | Allowed; walk `q`. |
   | `Statement::Explain { statement, analyze, verbose, .. }` | Allowed only when `analyze == false`, `verbose == false`, `query_plan == false`, `estimate == false`, `format` and `options` are `None`, `describe_alias` is `EXPLAIN`, and `statement` is a `Query`. Otherwise: "only plain EXPLAIN of a query is allowed". |
   | anything else | "only SELECT queries are allowed", at the statement's first token. |

   The walk (`sqlparser::ast::Visitor`, or a match over the query tree) enforces the rules below. Recursion is safe after Q6's limits.
   - **`WITH RECURSIVE`:** "recursive queries are not allowed".
   - **Allowed table names:** the registered view names plus every CTE name defined anywhere in the statement. DataFusion enforces CTE scope itself.
   - **`TableFactor::Table`:**
     - a single-part `name`, checked case-insensitively for unquoted names: "unknown table `<name>`" at the name; a qualified name: "use the view name alone, without catalog or schema";
     - `args: Some(_)` (a table function call, `generate_series(…)`): "table functions are not allowed".
   - **Other table factors:** `Derived`, `NestedJoin` and `UNNEST` are allowed. Any other `TableFactor` variant (table functions, JSON tables, PIVOT, …) gives "`<kind>` is not allowed in FROM".
   - **`SELECT … INTO`:** "SELECT INTO is not allowed".
   - **Functions** named `score`, `score_label` or `has_score`: "score functions are only available in filters; join the scores view instead" (D97). Other unknown functions are left to DataFusion's planner, whose diagnostic gives the position.

   Every error has the position of the offending node (`Spanned::span().start` when non-empty).
4. Plan with `ctx.session().state().statement_to_plan(datafusion::sql::parser::Statement::Statement(Box::new(statement))).await`. Failure → `planning_failure`.
5. Defence in depth: `SQLOptions::new().with_allow_ddl(false).with_allow_dml(false).with_allow_statements(false).verify_plan(&plan)`. Failure → "only SELECT queries are allowed", without position.
6. Output names must be unique. If two fields of `plan.schema()` share a `name()`: "column name `<n>` appears twice; rename one with AS", without position.

### Tests (`sql/validate.rs` and `sql/mod.rs`; the example project)

**Allowed.** Each plans:
- `SELECT * FROM spans`;
- `SELECT model, sum(cost_total) FROM spans GROUP BY model`;
- a `WITH` CTE used twice;
- `UNION ALL`;
- `VALUES (1), (2)`;
- a subquery in `FROM`;
- a join `traces t JOIN spans s ON t.trace_id = s.trace_id` with aliases selected uniquely;
- `row_number() OVER (PARTITION BY trace_id ORDER BY start_time)`;
- `SELECT unnest(tags) FROM traces`;
- `EXPLAIN SELECT name FROM spans`, which gives a plan with `plan_type` and `plan` columns;
- a CTE named `spans` that shadows the view.

**Rejected.** Each asserts the message and position:
- `INSERT INTO spans VALUES (1)`;
- `CREATE TABLE t (a INT)`;
- `DROP TABLE spans`;
- `SET datafusion.execution.batch_size = 1`;
- `COPY (SELECT 1) TO '/tmp/x.csv'`;
- `SHOW TABLES`;
- `EXPLAIN ANALYZE SELECT 1`;
- `EXPLAIN VERBOSE SELECT 1`;
- `SELECT 1; SELECT 2`;
- `SELECT * FROM raw_spans`;
- `SELECT * FROM datafusion.public.spans`;
- `SELECT * FROM information_schema.tables`;
- `SELECT * FROM generate_series(1, 3)`;
- `SELECT * FROM read_parquet('/etc/passwd')`;
- `SELECT * FROM '/etc/hostname'`;
- `WITH RECURSIVE r AS (SELECT 1) SELECT * FROM r`;
- `SELECT score('x') FROM traces`;
- `SELECT nme FROM spans` (planner diagnostic position);
- `SELECT a.name, b.name FROM spans a JOIN spans b ON a.span_id = b.span_id`;
- `SELECT 1 INTO t`;
- 65 KiB of text;
- 513 operators.

**Stack.** On a 2 MiB thread with a current-thread runtime, in the debug test build, a query with exactly 512 operators spread over 45 nested parenthesised subexpressions plans and runs.

**SQLite views.** Register a small `MemTable` as `ExperimentItems`: `SELECT * FROM experiment_items` works, and a second registration fails.

### Report

The exact sqlparser 0.62 field names you matched for `Explain` and `TableFactor`. Any statement kind that reached DataFusion's `SQLOptions` check (it should be none).

---

## Q11 — Parameters

**Goal:** `{name:Type}` placeholders with JSON values (D99).

**Edit only:** new `crates/storage/src/sql/params.rs`, `crates/storage/src/sql/mod.rs`.

**Commit:** `Bind SQL console parameters`

### Behaviour

`pub(crate) fn bind_placeholders(tokens: Vec<TokenWithSpan>) -> Result<(Vec<TokenWithSpan>, Vec<Placeholder>), InvalidQuery>`:
- **Raw placeholders.** A `Token::Placeholder` in the caller's text (`$1`, `?`) fails with "write parameters as {name:Type}", at the token.
- **Matching.** The pattern is exactly five consecutive tokens: `LBrace`, an unquoted `Word` (the name), `Colon`, an unquoted `Word` (the type), `RBrace`. No whitespace tokens between them. It is replaced by one `Token::Placeholder("$k")`, whose span runs from the `{` start to the `}` end.
- **Names.** `k` is 1-based, by order of first appearance. The same name reuses its `k`. The same name declared with two types: "parameter `<name>` is declared with two types".
- **Types,** case-insensitive: `String` → `Utf8`, `Int` → `Int64`, `Float` → `Float64`, `Bool` → `Boolean`, `Timestamp` → `Timestamp(ns, "UTC")`. Another word: "unknown parameter type `<t>`; use String, Int, Float, Bool or Timestamp", at the type token.
- **Other braces.** A `{` that does not start the pattern is left alone; the parser reports it.
- Text inside string literals and comments is one token, so it is never matched.

`Placeholder { name: String, kind: ParamKind, position: Position }`.

`pub(crate) fn param_values(placeholders: &[Placeholder], params: &serde_json::Map<String, Value>) -> Result<Vec<ScalarValue>, RunError>` returns the values in `k` order.
- **Missing and extra.** A declared name missing from `params`: `InvalidRequest("missing value for parameter <name>")`. A key of `params` that is not declared: `InvalidRequest("unknown parameter <name>")`.
- **Conversion,** by kind:

  | Kind | JSON value |
  |---|---|
  | `String` | string |
  | `Int` | integer that fits `i64` (no fraction) |
  | `Float` | any number, converted to `f64` |
  | `Bool` | boolean |
  | `Timestamp` | string through `llmobs_core::time_serde::parse_rfc3339`, or integer epoch milliseconds × 1,000,000 with `checked_mul` |

  Anything else: `InvalidRequest("parameter <name> must be <a string|an integer|…>")`. The message never quotes the value.

Wire into `plan_sql`. The public entry becomes `pub(crate) async fn prepare_sql(ctx, text, params) -> Result<LogicalPlan, RunError>`:
- tokenize;
- `bind_placeholders`;
- parse, validate, plan (Q10);
- `param_values`;
- bind the values.

**Binding.** Read `plan.get_parameter_types()`. For each `$k` with an inferred type different from the value's type, `ScalarValue::cast_to` the inferred type. Failure: `InvalidRequest("parameter <name> does not fit where it is used (<type>)")`. Then `plan.with_param_values(values)`.

A plan whose placeholder has no inferred type (`SELECT {x:Int}`) binds the value as declared.

### Tests

1. Each kind is bound in a `WHERE` and gives the expected rows:
   - `name = {n:String}`;
   - `span_count >= {c:Int}`;
   - `cost_total > {c:Float}`;
   - `has_root = {r:Bool}`;
   - `start_time >= {t:Timestamp}` with an RFC 3339 string, and again with epoch milliseconds.
2. `{n:String}` used twice binds once. The two-types case fails.
3. Missing value, extra key, `{c:Int}` with `1.5`, `{c:Int}` with `"1"`, and a timestamp ms that overflows: each fails with its message.
4. `{x:Date}` fails, with the position of `Date`.
5. `$1` and `?` fail with the position.
6. `SELECT '{n:String}' AS s` returns the literal text, with no parameter expected (an extra key fails).
7. `SELECT {x:Int} AS v` returns `v = 3`.
8. `WHERE span_count = {s:String}` with `"2"` casts and matches. With `"abc"` it fails with the "does not fit" message.
9. `SELECT name FROM spans ORDER BY name LIMIT {n:Int}` with `n = 2` returns 2 rows. If DataFusion cannot plan a placeholder in `LIMIT`, reject `LIMIT`/`OFFSET` placeholders in Q10's walk with a positioned "parameters are not allowed in LIMIT or OFFSET" instead, test that, and say which in the report.

### Report

Whether DataFusion inferred a type for each placeholder in test 1, and in test 9's `LIMIT`.

---

## Q12 — Execution and JSON results

**Goal:** `QueryContext::sql_json` runs a query with a timeout and a row cap, and returns D98's JSON shape. Execution errors are classified for the server (D94).

**Edit only:** new `crates/storage/src/sql/{run.rs,encode.rs}`, `crates/storage/src/sql/mod.rs`, `crates/storage/src/query_error.rs`, `crates/storage/src/lib.rs`, `crates/storage/Cargo.toml`.

**Commit:** `Run SQL console queries`

### Dependencies

`crates/storage/Cargo.toml` `[dependencies]`:
- `tokio = { workspace = true }` (move it out of dev-dependencies);
- `serde_json = { workspace = true }`;
- `arrow = { workspace = true, features = ["json"] }`. This is the same `arrow` DataFusion uses; the feature only adds `arrow::json`.

### Errors (`query_error.rs`)

`RunError` gains:

```rust
#[error("{0}")] Failed(String),                               // kind "failed"     → 400 query_failed
#[error("the query took longer than its time limit")] Timeout, // kind "timeout"    → 422 query_timeout
#[error("the query needed more memory than allowed")] TooLarge, // kind "too_large" → 422 query_too_large
```

`pub(crate) fn classify(error: DataFusionError) -> RunError` looks through the `Context`, `Shared` and `Diagnostic` wrappers and maps the root. Write the loop yourself and check that it unwraps all three; `find_root` alone may not cover `Diagnostic`. Never classify by matching message text.

| Root | Result |
|---|---|
| `ResourcesExhausted` | `TooLarge` |
| `Plan`, `SchemaError`, `SQL`, `NotImplemented` | `Invalid(planning_failure(&error))` |
| `Execution`, `ArrowError`, `Internal` | `Failed(cap_message(error.strip_backtrace()))` |
| anything else (`IoError`, `ObjectStore`, `ParquetError`, `External`, …) | `Internal(error)` |

`Internal` maps to `Failed` because DataFusion reports some caller errors as internal; a regex too big to compile is one (README § Verified facts). A1 M6 adds `Busy` here (README § Boundaries).

### Behaviour

```rust
pub struct SqlRequest<'a> { pub query: &'a str, pub params: &'a serde_json::Map<String, Value>, pub limit: usize, pub timeout: Duration }
pub struct SqlColumn { pub name: String, pub data_type: String }
pub struct SqlJson { pub columns: Vec<SqlColumn>, pub rows: Vec<serde_json::Map<String, Value>>, pub truncated: bool }
pub const SQL_MAX_ROWS: usize = 10_000;
impl QueryContext { pub async fn sql_json(&self, request: SqlRequest<'_>) -> Result<SqlJson, RunError> }
```

1. `limit` outside `1..=SQL_MAX_ROWS`: `InvalidRequest("limit must be between 1 and 10000")`. A zero `timeout`: `InvalidRequest`. The server validates `timeout_ms` against spec 09.
2. Everything below runs inside `tokio::time::timeout(request.timeout, …)`. On expiry, return `Timeout`. Dropping the future drops the stream, which cancels DataFusion's tasks.
3. `prepare_sql` (Q11).
4. `DataFrame::new(state, plan).limit(0, Some(limit + 1))`, then `execute_stream()`. Collect batches until `limit + 1` rows are reached. Errors go through `classify`.
5. `truncated = rows > limit`; keep the first `limit` rows (`RecordBatch::slice`).
6. `columns`: each output field's `name()` and `data_type().to_string()`.
7. Encode rows (`encode.rs`, `pub(crate) fn json_rows(batches, schema) -> Result<Vec<Map<String, Value>>, RunError>`):
   - First normalize timestamps. Every `Timestamp(unit, tz)` column is cast (`arrow::compute::cast`) to `Timestamp(Nanosecond, Some("UTC"))` when it has a time zone, else to `Timestamp(Nanosecond, None)`.
   - Then write with `arrow::json::WriterBuilder::new().with_explicit_nulls(true)`, `.with_timestamp_tz_format("%Y-%m-%dT%H:%M:%S%.9fZ")` and `.with_timestamp_format("%Y-%m-%dT%H:%M:%S%.9f")`, into an `ArrayWriter`.
   - Parse the bytes with `serde_json::from_slice::<Vec<Map<String, Value>>>`. The workspace enables `preserve_order`, so keys keep column order.
   - Encoding errors are `Internal`.

The module comment states the D98 encoding rules:
- binary as lowercase hex;
- maps as objects;
- lists as arrays;
- NaN and ±inf as null;
- timestamps with a time zone as UTC with `Z`, naive ones without.

**No logging here.** The server logs `RunError::kind()`, the text length and the duration (D94). Add no `tracing` calls with query text or messages.

### Tests

1. `SELECT trace_id, name, start_time, tags, metadata, cost_total FROM traces ORDER BY start_time DESC LIMIT 3` returns 3 rows. They match `responses/traces.json`'s first three entries for those fields: hex IDs, `start_time` as RFC 3339 with 9 digits and `Z`, `tags` as an array, `metadata` as an object. The column types are `Utf8`, `Timestamp(ns, "UTC")` (assert DataFusion's exact string), and so on.
2. `limit = 2` on a 5-row result returns 2 rows and `truncated: true`. `limit = 5` returns `truncated: false`.
3. `limit = 0` and `limit = 10_001` fail with `InvalidRequest`.
4. `SELECT CAST('NaN' AS DOUBLE) AS x, X'00ff' AS b, CAST(1 AS TIMESTAMP) AS naive` gives `x: null`, `b: "00ff"`, and `naive` without `Z`.
5. `SELECT 1 / 0` gives `Failed` with DataFusion's divide-by-zero message.
6. `SELECT regexp_like(name, '(((a{100}){100}){100})') FROM spans` gives `Failed`, not `Internal`.
7. **Memory:** a context whose runtime has `RuntimeEnvBuilder::new().with_memory_limit(1 << 20, 1.0)`, running a large sort over a hand-built 200,000-row table, gives `TooLarge`.
8. **Timeout:** `SELECT count(*) FROM big a, big b` (a hand-built table of 20,000 rows) with a 50 ms timeout gives `Timeout`, and the call returns within 1 s.
9. **Cancellation releases memory:** with a runtime that has a memory limit large enough for the query (e.g. 1 GiB), `SELECT a.v, b.v FROM big a, big b ORDER BY a.v * b.v DESC` with a 50 ms timeout gives `Timeout`. Then the runtime's `memory_pool.reserved()` reaches 0 within 10 s (poll every 50 ms). Release is not instant (README § Verified facts › Cancellation), so don't assert it right after the call.
10. `classify` on a `Diagnostic`-wrapped `Plan` error (`DataFusionError::Diagnostic(…)`, built in the test) gives `Invalid`, and on a `Context`-wrapped `ResourcesExhausted` gives `TooLarge`.
11. `INSERT …` gives `Invalid` with a position.
12. A missing parameter gives `InvalidRequest`.
13. A context built with a window (D101) applies it: `SELECT count(*) FROM spans` through `sql_json` counts only the window's spans.

### Report

The exact `data_type` strings for the columns in test 1. Whether any DataFusion error kind needed a mapping not in the table.

---

## Q13 — Exports

**Goal:** stream SQL results as CSV, NDJSON or Parquet, up to 10M rows (D98).

**Edit only:** `crates/storage/src/sql/{encode.rs,mod.rs,run.rs}`, `crates/storage/Cargo.toml`, the root `Cargo.toml` (adds `futures`).

**Commit:** `Stream SQL console exports`

### Dependencies

- `futures`: new in `[workspace.dependencies]`. Look up the current 0.3 release with `cargo search futures` and pin it the way the workspace pins others (`futures = "0.3.N"`). Then add `futures = { workspace = true }` to storage.
- `bytes = { workspace = true }` and `parquet = { workspace = true }` in storage's `[dependencies]`. If A1 has added them already, keep one entry.

### Behaviour

```rust
pub enum ExportFormat { Csv, Ndjson, Parquet }
pub const EXPORT_MAX_ROWS: usize = 10_000_000;
impl QueryContext {
    pub async fn sql_export(&self, request: SqlRequest<'_>, format: ExportFormat)
        -> Result<BoxStream<'static, Result<Bytes, RunError>>, RunError>;
}
```

- **Request fields.** `request.limit` is ignored for exports; document it on the field. The server passes `SQL_MAX_ROWS` there.
- **Before streaming.** Preparation errors are returned as the outer `Err`, so the server can still send a problem response. That covers parsing, validation, parameters and planning.
- **Plan.** `limit(0, Some(row_cap))`, with `row_cap = EXPORT_MAX_ROWS`. The internal `export_stream(plan, format, row_cap, timeout)` takes the cap as an argument, so tests can use a small one.
- **Timeout.** The timeout covers the whole stream, measured from the call (`tokio::time::Instant` deadline). When it expires, the stream yields `Err(Timeout)` and ends.
- **Mid-stream errors.** They are yielded as `Err(classify(e))`, then the stream ends. The server can only cut the response at that point. Document it.
- **CSV:**
  - The first chunk is the header line: column names, RFC 4180-quoted.
  - Each batch is encoded to JSON values with `json_rows` (Q12), then written as CSV lines:
    - null → empty field;
    - string → its text;
    - number and bool → their JSON text;
    - array and object → their JSON text.
  - Quote a field when it contains `"`, `,`, `\r` or `\n`, doubling `"`. Lines end with `\r\n`.
  - Nested columns are thus JSON text in their cell.
- **NDJSON.** `arrow::json::LineDelimitedWriter` with the same builder options as Q12, after the same timestamp normalization. One chunk per batch.
- **Parquet:**
  - `parquet::arrow::ArrowWriter` over a `Vec<u8>`, with `WriterProperties` using ZSTD level 1.
  - After each batch, call `flush()` once `in_progress_size()` exceeds 32 MiB. Then take the bytes written so far (`std::mem::take(writer.inner_mut())`) as a chunk.
  - At the end, `close()` the writer and emit the rest, which includes the footer.
  - If the `ArrowWriter` API differs in parquet 59, keep the behaviour (bounded memory, chunks as row groups complete, footer last) and report the difference.

### Tests

1. CSV of `SELECT trace_id, name, tags FROM traces ORDER BY start_time`: the header plus 6 lines. A name containing a comma or quote (hand-built table) is quoted. `tags` is a JSON array text.
2. NDJSON: each line parses to the same object as the matching `sql_json` row.
3. Parquet: read the concatenated chunks back with `ParquetRecordBatchReaderBuilder` (parquet's `arrow` feature). The rows equal `SELECT * FROM spans` collected directly.
4. `row_cap = 3` gives exactly 3 data rows in each format.
5. Invalid SQL gives the outer `Err(Invalid)`, and no stream.
6. `SELECT 1 / x FROM (VALUES (1), (0)) t(x)`: the stream yields `Err(Failed)` and ends.
7. A timeout during the stream (the cross-join table from Q12, 50 ms) yields `Err(Timeout)`.
8. Dropping the stream after its first chunk (a client disconnect) cancels the query: the runtime's `memory_pool.reserved()` reaches 0 within 10 s, polled as in Q12 test 9.

### Report

The `futures` version chosen. Any parquet `ArrowWriter` API difference.
