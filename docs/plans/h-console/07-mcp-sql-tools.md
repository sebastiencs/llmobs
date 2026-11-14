# H7 — The MCP query tools

**Commit subject:** `server: add the MCP query tools`

H7 adds two tools to the MCP server of H6: `query_sql`, which runs one sandboxed SQL query over a required time window (D-H-9, D-H-10), and `list_saved_views`, which lists the saved views and SQL queries the user can see (D-H-6). Both answer compact JSON text that fits a character budget, with `notes` saying what was cut (D-H-11).

## Read first

- README § Design → MCP server, § Saved views, § Contracts between sub-phases.
- `docs/plans/h-console/06-mcp-server.md` § Steps 7–9 and § Tests → Shared helpers (what H6 built).
- The code H6 wrote: `crates/server/src/mcp/{mod.rs,tools.rs,output.rs}`.
- `crates/server/src/meta/saved_views.rs` (H2): `Viewer`, `SavedViewRow`, `list_visible`, `kind_str`, `insert_view`, `NewSavedView`.
- `crates/server/src/routes/read.rs`: `open_query_windowed` (H4).
- `crates/server/src/http/error.rs`: `ApiError::Sql` (H4: an SQL text error names the field `query`), `ApiError::Query`, `From<QueryError> for ApiError` (`QueryError::Busy` → `503 busy`).
- `crates/storage/src/sql/run.rs`: `SqlRequest`, `SqlJson`, `SqlColumn`, `QueryContext::sql_json` (and its test `the_row_cap_sets_truncated` and the column-type test above it); `crates/storage/src/views/mod.rs`: `TimeWindow::new`.
- `crates/server/src/api/common.rs`: `TimeParam` (deserializes a JSON string: RFC 3339 or epoch milliseconds).

## Files

| File | Change |
|---|---|
| `crates/server/src/mcp/sql.rs` | new: `query_sql`, `run_query`, `cut_cells`, `list_saved_views`, `required_window`, tests |
| `crates/server/src/mcp/output.rs` | add `cut`, `max_chars` and their tests |
| `crates/server/src/mcp/tools.rs` | two tool definitions appended to `tool_list` |
| `crates/server/src/mcp/mod.rs` | `mod sql;`, two dispatcher arms, `FROM`/`TO` in `test_helpers`, the tool-name lists in two tests |

No dependency, OpenAPI, fixture or spec change.

## Steps

### 1. `output.rs` additions

```rust
/// Lowest and highest `max_chars` a tool accepts (D-H-10).
pub(crate) const MIN_MAX_CHARS: usize = 1_000;
pub(crate) const MAX_MAX_CHARS: usize = 100_000;

/// `max_chars` as given, or [`DEFAULT_MAX_CHARS`].
///
/// # Errors
///
/// `400 bad_request` `max_chars must be between 1000 and 100000` outside that range
/// (a negative number too: the argument is an `i64`).
pub(crate) fn max_chars(given: Option<i64>) -> Result<usize, ApiError> {
    let value = given.unwrap_or(DEFAULT_MAX_CHARS as i64);
    match usize::try_from(value) {
        Ok(value) if (MIN_MAX_CHARS..=MAX_MAX_CHARS).contains(&value) => Ok(value),
        _ => Err(ApiError::BadRequest {
            detail: Some("max_chars must be between 1000 and 100000".to_owned()),
            errors: Vec::new(),
        }),
    }
}

/// `text` cut to at most `max` characters in total: its first `max - 1` characters followed
/// by `…` (for `max == 0`, the empty string), or `None` when it has at most `max` characters.
/// A cut string has exactly `max` characters, so cutting it again is a no-op. Characters are
/// Unicode scalar values, so a multibyte character is never split.
pub(crate) fn cut(text: &str, max: usize) -> Option<String> {
    if text.chars().count() <= max {
        return None;
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    if max > 0 {
        out.push('…');
    }
    Some(out)
}
```

(`DEFAULT_MAX_CHARS as i64` is a lossless constant cast; if clippy objects, write `i64::try_from(DEFAULT_MAX_CHARS).unwrap_or(20_000)`.)

### 2. `mcp/sql.rs`

```rust
//! MCP tools over SQL and saved views (D-H-6, D-H-9, D-H-10, D-H-11).

use std::time::Duration;

use llmobs_storage::{QueryContext, SqlRequest, TimeWindow};
use rmcp::model::JsonObject;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::output::{self, DEFAULT_MAX_CHARS};
use super::tools::arguments;
use crate::api::common::TimeParam;
use crate::api::saved_views::SavedViewKind;
use crate::auth::principal::AuthUser;
use crate::auth::roles::ProjectPerm;
use crate::http::AppState;
use crate::http::access::user_project;
use crate::http::error::ApiError;
use crate::meta::saved_views::{self, Viewer};
use crate::routes::read::open_query_windowed;

pub(crate) const QUERY_SQL: &str = "query_sql";
pub(crate) const LIST_SAVED_VIEWS: &str = "list_saved_views";

/// How long `query_sql` may run (D-H-10).
const SQL_TIMEOUT: Duration = Duration::from_secs(30);
/// Default and largest `limit` of `query_sql` (D-H-10).
const DEFAULT_ROWS: i64 = 100;
const MAX_ROWS: usize = 1_000;
/// A string longer than this is cut: top-level SQL cells and saved-view state values (D-H-11).
const CELL_CUT: usize = 2_000;
/// Longest window of `query_sql` and `search_traces`, in nanoseconds: 30 days (D-H-9).
const MAX_WINDOW_NS: i128 = 30 * 24 * 3_600 * 1_000_000_000;
/// Most saved views `list_saved_views` returns.
const MAX_VIEWS: usize = 1_000;
```

**The window.**

```rust
/// `[from, to)` from a tool's arguments: both required, `from < to`, at most 30 days apart
/// (D-H-9). `search_traces` (H8) uses it too.
///
/// # Errors
///
/// `400 bad_request`, checked in this order: `from and to are required (RFC 3339, at most 30
/// days apart)` when either is missing; `from must be before to` (A2's message, through
/// `ApiError::Query`) when `from >= to`; `the window is at most 30 days` when `to - from`
/// exceeds 30 days (exactly 30 days is accepted).
pub(crate) fn required_window(
    from: Option<TimeParam>,
    to: Option<TimeParam>,
) -> Result<TimeWindow, ApiError> {
    let (Some(from), Some(to)) = (from, to) else {
        return Err(ApiError::BadRequest {
            detail: Some("from and to are required (RFC 3339, at most 30 days apart)".to_owned()),
            errors: Vec::new(),
        });
    };
    let window = TimeWindow::new(from.ns(), to.ns())?;
    if i128::from(to.ns()) - i128::from(from.ns()) > MAX_WINDOW_NS {
        return Err(ApiError::BadRequest {
            detail: Some("the window is at most 30 days".to_owned()),
            errors: Vec::new(),
        });
    }
    Ok(window)
}
```

A `from` or `to` that is not a JSON string holding RFC 3339 or epoch milliseconds fails earlier, in `arguments`, with `invalid arguments: expected an RFC 3339 timestamp or epoch milliseconds` (the `TimeParam` deserializer's message).

**`query_sql`.**

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuerySqlArguments {
    project: String,
    query: String,
    #[serde(default)]
    params: Map<String, Value>,
    from: Option<TimeParam>,
    to: Option<TimeParam>,
    limit: Option<i64>,
    max_chars: Option<i64>,
}

/// Runs `query` on the project (`ProjectPerm::Query`) over the required window, with no
/// default exclusions, like the console (spec 05).
///
/// Answers `{"columns": [{"name", "type"}], "rows": [...], "row_count", "truncated", "notes"}`.
pub(crate) async fn query_sql(state: &AppState, user: &AuthUser, raw: JsonObject) -> Result<Value, ApiError>
```

Body, in this order:

1. `let args: QuerySqlArguments = arguments(raw)?;`
2. `let window = required_window(args.from, args.to)?;`
3. `limit`: `args.limit.unwrap_or(DEFAULT_ROWS)`, then `usize::try_from` it; anything outside `1..=MAX_ROWS` (negative included) is `ApiError::BadRequest { detail: Some("limit must be between 1 and 1000".to_owned()), errors: Vec::new() }`.
4. `let max_chars = output::max_chars(args.max_chars)?;`
5. `let access = user_project(state, user, &args.project, ProjectPerm::Query).await?;`
6. `let ctx = open_query_windowed(state, access.project.id, Some(window)).await?;` (D101: the window is written into every view scan, in joins, subqueries and CTEs too. Admission control can answer `503 busy` here, through `From<QueryError>`.)
7. `run_query(&ctx, &args.query, &args.params, limit, SQL_TIMEOUT, max_chars).await`

```rust
/// Steps 7–11 of `query_sql`, on an open context. Separate so tests can pass a tiny timeout.
async fn run_query(
    ctx: &QueryContext,
    query: &str,
    params: &Map<String, Value>,
    limit: usize,
    timeout: Duration,
    max_chars: usize,
) -> Result<Value, ApiError>
```

1. Run it; SQL errors are `ApiError::Sql`, so an invalid statement names the field `query`:
   ```rust
   let result = ctx
       .sql_json(SqlRequest { query, params, limit, timeout })
       .await
       .map_err(ApiError::Sql)?;
   ```
   If clippy reports `large_futures`, wrap the `sql_json` future in `Box::pin(...)`; add no `#[allow]`.
2. `let mut rows: Vec<Map<String, Value>> = result.rows; let cut = cut_cells(&mut rows);`
3. Notes, in this order:
   - when `result.truncated` and `limit < MAX_ROWS`: `format!("rows truncated at {limit}; pass a larger limit (at most 1000) or narrow the query")`;
   - when `result.truncated` and `limit == MAX_ROWS`: `"rows truncated at 1000; narrow the query or the window"`;
   - when `cut`: `"string values longer than 2000 characters were cut"`.
4. Body, keys in this order: `columns` (`[{"name": column.name, "type": column.data_type}]`), `rows` (each row as `Value::Object`), `row_count` (the number of rows), `truncated` (`result.truncated`), `notes`.
5. `output::fit_list(&mut body, "rows", max_chars, &format!("result cut at {max_chars} characters; later rows left out"));` then set `row_count` to the length of `body["rows"]`. When even zero rows do not fit (only possible with a very wide column list), `rows` is `[]` and the body is returned as is, over the budget.
6. Return `Ok(Value::Object(body))`.

```rust
/// Cuts every top-level string cell longer than `CELL_CUT` characters (see `output::cut`).
/// Strings inside lists and maps are left alone: a cell that is a list or map is returned
/// whole, and `max_chars` bounds the result. Returns whether anything was cut.
fn cut_cells(rows: &mut [Map<String, Value>]) -> bool
```

For every value of every row: if it is `Value::String(text)` and `output::cut(text, CELL_CUT)` is `Some(short)`, replace it by `Value::String(short)`.

The A2 row cap fetches `limit + 1` rows, so a query that returns exactly `limit` rows is not `truncated`.

**`list_saved_views`.**

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListSavedViewsArguments {
    project: String,
    kind: Option<SavedViewKind>,
}

/// Saved views and SQL queries the user sees in the project (`ProjectPerm::View`): their own
/// and every shared one (D-H-6), oldest first (by id).
///
/// Answers `{"views": [{"id", "kind", "name", "shared", "owned", "is_default", "state"}],
/// "truncated", "notes"}`.
pub(crate) async fn list_saved_views(state: &AppState, user: &AuthUser, raw: JsonObject) -> Result<Value, ApiError>
```

Body:

1. `let args: ListSavedViewsArguments = arguments(raw)?;` (an unknown `kind` fails here: `invalid arguments: unknown variant `…`, expected one of …`).
2. `let access = user_project(state, user, &args.project, ProjectPerm::View).await?;`
3. `views_body(state, access.project.id, user, args.kind, MAX_VIEWS).await` — steps 3–7 live in a private `async fn views_body(state: &AppState, project: ProjectId, user: &AuthUser, kind: Option<SavedViewKind>, max_views: usize) -> Result<Value, ApiError>` so a test can pass a small `max_views`. In it: `let page = saved_views::list_visible(state.meta.reader(), project, Viewer::User(user.id), kind, max_views, None).await?;` (`ProjectId` is `llmobs_core::ids::ProjectId`).
4. Each row: first cut its state's top-level string values longer than `CELL_CUT` (same rule as `cut_cells`, on `row.state`; remember whether anything was cut), then `json!({"id": row.id.to_string(), "kind": saved_views::kind_str(row.kind), "name": row.name, "shared": row.shared, "owned": row.owner_user_id == Some(user.id), "is_default": row.is_default, "state": Value::Object(row.state)})`.
5. Notes, in order: when `page.next_cursor.is_some()`: `format!("more than {max_views} saved views; only the first {max_views} are listed")`; when a state value was cut: `"saved-view state values longer than 2000 characters were cut"`.
6. Body keys in order: `views`, `truncated` (`page.next_cursor.is_some()`), `notes`.
7. `output::fit_list(&mut body, "views", DEFAULT_MAX_CHARS, "result cut at 20000 characters; later views left out");` and return `Ok(Value::Object(body))`.

Cutting state values before fitting keeps one large saved query from pushing every later view out of the result.

### 3. Tool definitions (`mcp/tools.rs`)

Append to the `vec!` in `tool_list`, after `get_schema`, in this order:

```rust
        tool(
            super::sql::QUERY_SQL,
            "Run a SQL query",
            "Run one read-only SQL query (SELECT, WITH, UNION, VALUES or EXPLAIN) over the views traces, spans, sessions and scores; get_schema lists their columns. `from` and `to` are required and at most 30 days apart: they bound start_time on spans and traces and created_at on scores, in every part of the query; sessions are built from the windowed traces. No rows are hidden: evaluation traces (environment 'llmobs-eval') and experiment traces are included. Returns columns, rows, row_count, truncated and notes; string cells longer than 2000 characters are cut.",
            json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": PROJECT_ARG},
                    "query": {"type": "string", "description": "The SQL statement. Use {name:Type} placeholders (Type: String, Int, Float, Bool, Timestamp) with `params` instead of inlining values."},
                    "params": {"type": "object", "description": "Values of the {name:Type} placeholders, by name."},
                    "from": {"type": "string", "description": "Window start, inclusive: an RFC 3339 timestamp, or epoch milliseconds as a string."},
                    "to": {"type": "string", "description": "Window end, exclusive: an RFC 3339 timestamp, or epoch milliseconds as a string. At most 30 days after from."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 100, "description": "Most rows to return."},
                    "max_chars": {"type": "integer", "minimum": 1000, "maximum": 100000, "default": 20000, "description": "Most characters in the result; later rows are left out beyond it."}
                },
                "required": ["project", "query", "from", "to"],
                "additionalProperties": false
            }),
        ),
        tool(
            super::sql::LIST_SAVED_VIEWS,
            "List saved views",
            "Saved list views (kind traces, spans, sessions, users) and saved SQL queries (kind sql) you can see in the project: yours and the shared ones. A view's `state` holds its filter, search and range; a SQL query's holds `query` and `params`. Pass a traces view's name or id to search_traces as `view`.",
            json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": PROJECT_ARG},
                    "kind": {"type": "string", "enum": ["traces", "spans", "sessions", "users", "sql"], "description": "Only views of this kind."}
                },
                "required": ["project"],
                "additionalProperties": false
            }),
        ),
```

### 4. `mcp/mod.rs`

- Add `mod sql;` next to `mod tools;`.
- In `call_tool`, add two arms after `GET_SCHEMA`, in the same shape:
  ```rust
              sql::QUERY_SQL => (sql::QUERY_SQL, sql::query_sql(&self.state, &self.user, arguments).await),
              sql::LIST_SAVED_VIEWS => (
                  sql::LIST_SAVED_VIEWS,
                  sql::list_saved_views(&self.state, &self.user, arguments).await,
              ),
  ```
- In `test_helpers`, add:
  ```rust
      /// A window covering the example dataset: all of 2026-01-01, UTC.
      pub(crate) const FROM: &str = "2026-01-01T00:00:00Z";
      pub(crate) const TO: &str = "2026-01-02T00:00:00Z";
  ```
- In `tools_list_has_every_tool_read_only` and in `stateless_2026_07_28_requests` (b), the names become `["list_projects", "get_schema", "query_sql", "list_saved_views"]`; in the first, also assert `query_sql`'s `inputSchema.required` = `["project", "query", "from", "to"]` and `list_saved_views`'s = `["project"]`.

## Tests

In `mcp/sql.rs`, `#[cfg(test)] mod tests`. Imports: `std::time::Duration`; `serde_json::{Map, Value, json}`; `crate::mcp::test_helpers::*`; `crate::mcp::output`; `super::*`; `crate::api::saved_views::SavedViewKind`; `crate::auth::roles::OrgRole`; `crate::meta::saved_views::{self, NewSavedView}`; `crate::routes::read::open_query_windowed`; `crate::api::common::TimeParam`; `crate::test_support::TestApp`. Drop any import the compiler reports unused.

`#[tokio::test(flavor = "multi_thread")]` except the log test (`#[tokio::test]`, current thread; see H6 § Tests). Every `query_sql` test calls `app.load_example_dataset(seeded.project.id).await` first and passes `"project": seeded.project.id.to_string()`, `"from": FROM`, `"to": TO` unless the row says otherwise. `T1` … `T6` stand for `a1000000000000000000000000000001` … `…006`: write them out in full. Tool arguments are passed to `call` as `&json!(…)` (H6's helpers take `&Value`). "Error `c` / `m`" means `is_error` is true, `body["error"]["code"]` = `c` and `body["error"]["message"]` = `m`.

Expected column types: `trace_id` and `name` of the `traces` view print as `Utf8` (asserted by A2's own test in `crates/storage/src/sql/run.rs`), and `count(*)` as `Int64` (H4's `static/sql-result.json`). If either differs, STOP and report.

**`query_sql`**

| Test | Arguments (besides project/from/to) | Asserts |
|---|---|---|
| `query_sql_returns_rows_without_exclusions` | `query`: `SELECT trace_id, name FROM traces ORDER BY start_time` | not an error; body = `{"columns":[{"name":"trace_id","type":"Utf8"},{"name":"name","type":"Utf8"}],"rows":[{"trace_id":T1,"name":"support-agent"},{"trace_id":T5,"name":"judge helpfulness"},{"trace_id":T2,"name":"support-agent"},{"trace_id":T3,"name":"answer-question"},{"trace_id":T4,"name":"describe-image"},{"trace_id":T6,"name":"chat claude-sonnet-4-5"}],"row_count":6,"truncated":false,"notes":[]}` (trace 5, the evaluation trace, is included: the console has no exclusions) |
| `query_sql_window_is_half_open` | same query; `from` `"2026-01-01T10:10:00Z"`, `to` `"2026-01-01T10:20:00Z"` | `rows[*].trace_id` = `[T3]` (T3 starts exactly at 10:10 and is in; T4 starts exactly at 10:20 and is out) |
| `query_sql_windows_every_view` | `from` `"2026-01-01T10:06:00Z"`: (a) `SELECT count(*) AS n FROM scores`; (b) `SELECT count(*) AS n FROM (SELECT * FROM spans) AS s`; (c) `WITH t AS (SELECT trace_id FROM traces) SELECT count(*) AS n FROM t`; and (a) again with `from` `FROM` | (a) `rows` = `[{"n":3}]` (scores created at 10:07, 10:08 and 10:11); (b) `[{"n":6}]` (the spans of T3, T4 and T6); (c) `[{"n":3}]`; (a) over the whole day: `[{"n":6}]` |
| `query_sql_binds_parameters` | `SELECT count(*) AS n FROM spans WHERE span_kind = {kind:String}`, `params` `{"kind": "llm"}` | `columns` = `[{"name":"n","type":"Int64"}]`; `rows` = `[{"n":7}]` |
| `query_sql_reports_the_row_cap` | the first query, `limit` 2; then `SELECT trace_id FROM traces ORDER BY start_time LIMIT 2` with `limit` 2; then the first query with `limit` 1000; then `SELECT s.span_id FROM spans s CROSS JOIN spans a CROSS JOIN spans b` (14³ = 2,744 rows) with `limit` 1000 | first: ids `[T1, T5]`, `row_count` 2, `truncated` true, `notes` = `["rows truncated at 2; pass a larger limit (at most 1000) or narrow the query"]`; second: 2 rows, `truncated` false, `notes` `[]`; third: 6 rows, `truncated` false; fourth: `truncated` true, `notes[0]` = `"rows truncated at 1000; narrow the query or the window"` (fitting to 20,000 characters then adds a second note and keeps fewer rows) |
| `query_sql_cuts_long_strings` | `SELECT concat(input, input, input, input, input, input, input) AS big FROM spans WHERE span_id = 'b100000000000014'` | `rows[0].big` has 2,000 characters (`chars().count()`) and ends with `…`; `truncated` false; `notes` = `["string values longer than 2000 characters were cut"]` (that span's stored input is 359 characters, so the concatenation has 2,513) |
| `query_sql_orders_its_notes` | `SELECT concat(input, input, input, input, input, input, input) AS big FROM spans WHERE input IS NOT NULL ORDER BY length(input) DESC`, `limit` 1 (the filter keeps a null `input` from sorting first) | `notes` = `["rows truncated at 1; pass a larger limit (at most 1000) or narrow the query", "string values longer than 2000 characters were cut"]` |
| `query_sql_fits_max_chars` | `SELECT span_id, input FROM spans ORDER BY span_id`, `max_chars` 1000 | `truncated` true; `notes` = `["result cut at 1000 characters; later rows left out"]`; `output::char_len(&output::render(&body))` ≤ 1000; `row_count` = `rows.len()`; `0 < rows.len() < 14`; `rows[0].span_id` = `"b100000000000011"` |
| `query_sql_validates_arguments` | one call per case | no `to` → error `bad_request` / `from and to are required (RFC 3339, at most 30 days apart)`; `to` = `FROM` → `bad_request` / `from must be before to`; `to` = `"2026-01-31T00:00:01Z"` → `bad_request` / `the window is at most 30 days`; `from` = `"yesterday"` → `bad_request`, message starts with `invalid arguments: expected an RFC 3339 timestamp or epoch milliseconds`; `limit` 0, 1001 and -1 → `bad_request` / `limit must be between 1 and 1000`; `max_chars` 999, 100001 and -5 → `bad_request` / `max_chars must be between 1000 and 100000`; the parameter query with `params` `{}` → `bad_request` / `missing value for parameter kind`; an extra argument `"x": 1` → `bad_request`, message starts with ``invalid arguments: unknown field `x` ``. And `to` = `"2026-01-31T00:00:00Z"` (exactly 30 days) with `SELECT 1 AS one` is not an error, `rows` = `[{"one":1}]` |
| `query_sql_reports_sql_errors` | (a) `DELETE FROM traces`; (b) `SELEC 1`; (c) `SELECT 1 / 0 AS x` (A2's own test `division_by_zero_is_a_failed_query` shows this is `RunError::Failed`) | (a) error `invalid_filter` / `only SELECT queries are allowed`; (b) `is_error`, `code` `invalid_filter`, a non-empty `message`, and `error.position.line` = 1 with `error.position.column` ≥ 1; (c) `is_error`, `code` `query_failed` |
| `query_sql_needs_a_visible_project` | `project` `"acme/nope"` | body = `{"error":{"code":"not_found","message":"Not Found"}}` |
| `query_sql_works_for_a_viewer_with_a_read_token` | `let viewer = app.add_member(&seeded, "viewer@example.com", OrgRole::Viewer).await;` with `app.personal_token(viewer.id, false)`; `SELECT 1 AS one` | not an error; `rows` = `[{"one":1}]` |
| `run_query_times_out` | no tool call: `let window = required_window(Some(t(FROM)), Some(t(TO))).unwrap();` with `fn t(s: &str) -> TimeParam { serde_json::from_value(Value::String(s.to_owned())).unwrap() }`; `let ctx = open_query_windowed(&app.state, seeded.project.id, Some(window)).await.unwrap();` then `run_query(&ctx, "SELECT span_kind, count(*) AS n FROM spans GROUP BY span_kind", &Map::new(), 10, Duration::from_nanos(1), 20_000).await`. The `GROUP BY` plan runs through a `RepartitionExec`, so the query future returns `Pending` on its first poll (a plain in-memory aggregate can finish inline and win the race against the timeout, however small) | `Err(ApiError::Sql(llmobs_storage::RunError::Timeout))` (match with `matches!`); as a tool error it is code `query_timeout` (`output::failure`, already tested in H6) |

`503 busy` (admission control) comes from `From<QueryError> for ApiError`, which the HTTP read routes already test; H7 adds no test for it.

**`list_saved_views`**

Setup for these tests (no dataset needed): `let bob = app.add_member(&seeded, "bob@example.com", OrgRole::Member).await;` ada is `seeded.user_id`; `let (playground, _) = app.add_project(&seeded, "playground", None).await;` `let now = app.state.clock.now_ns();`. Insert with

```rust
let id = app.state.meta.write(move |conn| Box::pin(async move {
    let row = saved_views::insert_view(conn, NewSavedView {
        project_id, kind, name: name.to_owned(), state, owner_user_id, shared, is_default,
    }, now).await?;
    Ok(row.id)
})).await.unwrap();
```

in this order (state values as JSON objects built with `json!` and `.as_object().unwrap().clone()`):

| # | Project | Owner | Kind | Name | State | Shared | Default |
|---|---|---|---|---|---|---|---|
| 1 | support-bot | ada | `Traces` | `Mine` | `{"filter": "status = 'error'"}` | false | true |
| 2 | support-bot | ada | `Sql` | `Costs` | `{"query": "SELECT 1", "params": {}}` | true | false |
| 3 | support-bot | bob | `Traces` | `Bob only` | `{}` | false | false |
| 4 | support-bot | bob | `Traces` | `Team` | `{"search": "refund"}` | true | false |
| 5 | playground | ada | `Traces` | `Elsewhere` | `{}` | true | false |
| 6 | support-bot | bob | `Traces` | `Orphan` | `{}` | false | false |

Then orphan view 6: `sqlx::query("UPDATE saved_views SET owner_user_id = NULL WHERE id = ?").bind(id6.to_string()).execute(&mut *conn).await?;` inside `app.state.meta.write`.

| Test | Call | Asserts |
|---|---|---|
| `list_saved_views_shows_own_and_shared` | ada's read token, `{"project": seeded.project.id.to_string()}` | body exactly: `{"views":[{"id":"<1>","kind":"traces","name":"Mine","shared":false,"owned":true,"is_default":true,"state":{"filter":"status = 'error'"}},{"id":"<2>","kind":"sql","name":"Costs","shared":true,"owned":true,"is_default":false,"state":{"query":"SELECT 1","params":{}}},{"id":"<4>","kind":"traces","name":"Team","shared":true,"owned":false,"is_default":false,"state":{"search":"refund"}}],"truncated":false,"notes":[]}` — view 3 is bob's private view, view 5 is in another project, view 6 has no owner and is private, so nobody sees it |
| `list_saved_views_filters_by_kind` | ada, `"kind": "sql"` | `views[*].id` = `[<2>]` |
| `list_saved_views_for_another_user` | bob's read token (`app.personal_token(bob.id, false)`) | `views[*].id` = `[<2>, <3>, <4>]`; `owned` = `[false, true, true]` |
| `list_saved_views_errors` | ada: `"kind": "nope"`; `"project": "acme/nope"` | first: `bad_request`, message starts with ``invalid arguments: unknown variant `nope` ``; second: `{"error":{"code":"not_found","message":"Not Found"}}` |
| `list_saved_views_reports_more_views` | no tool call: `views_body(&app.state, seeded.project.id, &user, None, 2)` where `user` is ada's `AuthUser` (`AuthUser { id: seeded.user_id, is_instance_admin: true, email: "ada@example.com".to_owned() }`) | `views[*].id` = `[<1>, <2>]`; `truncated` true; `notes` = `["more than 2 saved views; only the first 2 are listed"]` |
| `list_saved_views_fits_its_budget` | insert 300 more ada private `Traces` views named `v000`…`v299`, each with state `{"filter": <150 × "f">}`; list with ada | `truncated` true; the last note is `"result cut at 20000 characters; later views left out"`; `char_len(&render(&body))` ≤ 20,000; `views[0].id` = `<1>` |
| `list_saved_views_cuts_long_state_values` | insert one more ada view in support-bot, `Traces`, name `Long`, state `{"filter": <2,500 × "a">}`, shared false; list with ada | the `Long` view's `state.filter` has 2,000 characters ending with `…`; `notes` = `["saved-view state values longer than 2000 characters were cut"]` |

**Logs**

| Test | Steps | Asserts |
|---|---|---|
| `query_sql_logs_no_sql_text` (`#[tokio::test]`) | `let (app, seeded, tok) = app_with_token().await;` load the dataset; `capture_info_logs` (H6's `test_helpers`) around `call(&app, &tok, "query_sql", {"project": …, "from": FROM, "to": TO, "query": "SELECT 'canary-sql-1' AS c FROM spans WHERE name = {p:String}", "params": {"p": "canary-param-2"}})` (not an error, no rows) and a second call whose query is `SELEC 'canary-sql-3'` | the logs contain `tool call` and `query_sql`; they contain none of `canary-sql-1`, `canary-param-2`, `canary-sql-3` |

**Unit tests**

- `output.rs`, `cut_counts_characters`: `cut("héllo", 5)` = `None`; `cut("héllo", 2)` = `Some("h…")`; `cut("", 0)` = `None`; a string of 2,000 `é` → `cut(…, 2000)` = `None`; of 2,001 `é` → `Some` with 2,000 characters, 1,999 `é` then `…`, and cutting that again is `None`; `cut("x", 0)` = `Some("")`.
- `output.rs`, `max_chars_bounds`: `max_chars(None)` = `Ok(20_000)`; `Some(1_000)` and `Some(100_000)` are `Ok` with that value; `Some(999)`, `Some(100_001)` and `Some(-1)` are `Err(ApiError::BadRequest { .. })` (`matches!`).
- `sql.rs`, `cut_cells_cuts_top_level_strings_only`: rows `[{"a": <2,001 × x>, "b": <2,000 × x>, "c": [<3,000 × x>], "d": {"k": <3,000 × x>}, "e": 5}]` → returns `true`; `a` has 2,000 characters ending with `…`; `b` keeps 2,000; `c` and `d` are unchanged; `e` is `5`. A second call returns `false`.
- `sql.rs`, `required_window_bounds`: `(None, Some(t(TO)))` → `Err(BadRequest)`; `(t(FROM), t(FROM))` → `Err(ApiError::Query(RunError::InvalidRequest(_)))`; 30 days → `Ok`; 30 days and 1 ns (`"2026-01-31T00:00:00.000000001Z"`) → `Err(BadRequest)`.

## Commands

```bash
cargo test -p llmobs-server mcp
export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH" && just check
```

## Done when

- `tools/list` lists four read-only tools; `query_sql` and `list_saved_views` answer as above.
- `query_sql` refuses calls without a window, with `from >= to` or with more than 30 days, caps rows at `limit` (≤ 1,000) and its run at 30 s, cuts top-level string cells at 2,000 characters and fits the result to `max_chars`; SQL errors name the `query` field's position.
- No log line carries the query, parameters, view names or states (the dispatcher logs only the tool name, user id, status and duration).
- `just check` passes; one commit, `server: add the MCP query tools`.

Report "no new dependency".
