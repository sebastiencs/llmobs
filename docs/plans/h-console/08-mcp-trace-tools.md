# H8 — The MCP trace tools

**Commit subject:** `server: add the MCP trace tools`

H8 adds the last three tools to the MCP server: `search_traces` (a trace list over a required window, by filter DSL, search text or saved view), `get_trace` (a trace and its span tree) and `get_span` (one full span). Their output is compact: metadata, attribute and preview values are cut to 200 characters, `get_trace` leaves the previews out unless asked, and every result fits `max_chars` (D-H-9, D-H-10, D-H-11). After H8, `tools/list` has the seven tools of README § Design → MCP server.

## Read first

- README § Design → MCP server, § Contracts between sub-phases.
- `docs/plans/h-console/06-mcp-server.md` § Steps 7–9 and § Tests → Shared helpers, and `docs/plans/h-console/07-mcp-sql-tools.md` § Steps 1–2 and § 4 (what H6 and H7 built).
- The code: `crates/server/src/mcp/{mod.rs,tools.rs,output.rs,sql.rs}`.
- `crates/server/src/routes/traces.rs` (`list_traces`, `get_trace`, `get_span`: H8 runs the same reads) and `crates/server/src/routes/read.rs` (`open_query`, `attach_trace_scores`, `READ_TIMEOUT`).
- `crates/server/src/api/traces.rs`: `TraceSummary`, `SpanSummary`, `TreeSpan`, `SpanDetail`, `build_tree`.
- `crates/server/src/meta/saved_views.rs` (H2): `get_visible`, `find_visible_by_name`, `insert_view`, `NewSavedView`, `Viewer`.
- `crates/storage/src/reads/traces.rs`: `TraceListRequest`, `trace_list`, `trace_detail`, `span_detail`; `crates/storage/src/lib.rs` re-exports `MAX_TRACE_SPANS`.
- `crates/server/src/test_support/mod.rs`: `dataset_span`, `commit_spans`, `add_project`, `add_member`, `personal_token`.
- `fixtures/api/responses/traces.json`, `fixtures/api/responses/traces/a1000000000000000000000000000001.json`, `fixtures/api/responses/spans/a1000000000000000000000000000001/b100000000000012.json` (the tests compare with them).

## Files

| File | Change |
|---|---|
| `crates/server/src/mcp/traces.rs` | new: the three tools, value cutting, span fitting, tests |
| `crates/server/src/mcp/tools.rs` | three tool definitions appended to `tool_list` |
| `crates/server/src/mcp/mod.rs` | `mod traces;`, three dispatcher arms, the tool-name lists in two tests |

No dependency, OpenAPI, fixture or spec change.

## Steps

### 1. `mcp/traces.rs`: imports and constants

```rust
//! MCP tools over traces and spans (D-H-9, D-H-10, D-H-11).

use llmobs_core::ids::ProjectId;
use llmobs_storage as storage;
use rmcp::model::JsonObject;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::output::{self, char_len, render};
use super::sql::required_window;
use super::tools::arguments;
use crate::api::common::TimeParam;
use crate::api::convert;
use crate::api::saved_views::SavedViewKind;
use crate::api::traces::{SpanDetail, build_tree};
use crate::auth::principal::AuthUser;
use crate::auth::roles::ProjectPerm;
use crate::http::AppState;
use crate::http::access::user_project;
use crate::http::error::ApiError;
use crate::meta::saved_views::{self, Viewer};
use crate::routes::read::{READ_TIMEOUT, attach_trace_scores, open_query};

pub(crate) const SEARCH_TRACES: &str = "search_traces";
pub(crate) const GET_TRACE: &str = "get_trace";
pub(crate) const GET_SPAN: &str = "get_span";

/// Default and largest `limit` of `search_traces` (README § Design → MCP server).
const DEFAULT_TRACES: i64 = 20;
const MAX_TRACES: usize = 100;
/// Metadata, attribute and preview values longer than this are cut (D-H-11).
const VALUE_CUT: usize = 200;
/// The note added when [`cut_values`] cut something.
const CUT_NOTE: &str =
    "metadata, attribute and preview values longer than 200 characters were cut to 200 characters plus …";
/// The two preview fields of trace and span summaries.
const PREVIEWS: [&str; 2] = ["input_preview", "output_preview"];
/// The fields of a span detail that can be large. They are cut, all to the same length, only
/// when the span does not fit `max_chars`.
const HEAVY: [&str; 7] = [
    "input",
    "output",
    "system_instructions",
    "tool_definitions",
    "request_params",
    "events",
    "links",
];
```

Use `storage::…` for every storage item (`storage::trace_list`, `storage::TraceListRequest`, `storage::TraceSort`, `storage::SortOrder`, `storage::ListScope`, `storage::trace_detail`, `storage::span_detail`, `storage::MAX_TRACE_SPANS`); `TraceSort` and `SortOrder` also exist in `crate::api`, so do not import them bare. Remove any import the compiler reports unused.

### 2. Helpers

```rust
/// Serializes an API type. It cannot fail for these types, but it is not unwrapped.
fn to_json<T: serde::Serialize>(value: &T) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|error| ApiError::Internal(Box::new(error)))
}

/// A `limit` or similar count, as given or `default`, inside `1..=max`.
///
/// # Errors
///
/// `400 bad_request` with `message` otherwise (negative numbers included).
fn bounded(given: Option<i64>, default: i64, max: usize, message: &str) -> Result<usize, ApiError> {
    match usize::try_from(given.unwrap_or(default)) {
        Ok(value) if (1..=max).contains(&value) => Ok(value),
        _ => Err(ApiError::BadRequest { detail: Some(message.to_owned()), errors: Vec::new() }),
    }
}

/// Cuts to `VALUE_CUT` characters (with `…`, see `output::cut`) every string value of the
/// object fields `maps` of `object`, and the string fields `strings` of `object`. Fields that
/// are absent, `null` or of another type are left alone. Returns whether anything was cut.
fn cut_values(object: &mut Map<String, Value>, maps: &[&str], strings: &[&str]) -> bool
```

`cut_values`: for each name in `maps`, if `object.get_mut(name)` is `Some(Value::Object(inner))`, replace every `Value::String(text)` value of `inner` for which `output::cut(text, VALUE_CUT)` is `Some(short)` by `Value::String(short)`. For each name in `strings`, if `object.get_mut(name)` is `Some(Value::String(text))`, the same. Return `true` when at least one replacement happened.

```rust
/// A `TraceSummary` or `SpanSummary` as JSON, with `metadata` values and the previews cut, or
/// the previews removed when `previews` is false. Returns the value and whether it cut.
fn compact_summary(value: Value, previews: bool) -> (Value, bool)
```

If `value` is not an object, return it unchanged with `false`. Otherwise: when `!previews`, remove both `PREVIEWS` keys (`Map::remove`); then `let cut = cut_values(&mut object, &["metadata"], &PREVIEWS);` and return `(Value::Object(object), cut)`.

The cutting happens on each item before `fit_list` runs, so one long value never pushes every later item out of the result.

### 3. `search_traces`

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchTracesArguments {
    project: String,
    from: Option<TimeParam>,
    to: Option<TimeParam>,
    view: Option<String>,
    filter: Option<String>,
    search: Option<String>,
    limit: Option<i64>,
    include_eval: Option<bool>,
    include_experiments: Option<bool>,
    max_chars: Option<i64>,
}

/// Traces of the project (`ProjectPerm::View`) whose `start_time` is in the required window,
/// newest first, with the default exclusions unless the arguments or the view lift them.
///
/// Answers `{"traces": [TraceSummary, compacted], "truncated", "notes"}`.
pub(crate) async fn search_traces(state: &AppState, user: &AuthUser, raw: JsonObject) -> Result<Value, ApiError>
```

Body, in this order:

1. `let args: SearchTracesArguments = arguments(raw)?;`
2. `let window = required_window(args.from, args.to)?;`
3. `let limit = bounded(args.limit, DEFAULT_TRACES, MAX_TRACES, "limit must be between 1 and 100")?;`
4. `let max_chars = output::max_chars(args.max_chars)?;`
5. `let access = user_project(state, user, &args.project, ProjectPerm::View).await?;`
6. The view, when `args.view` is `Some(key)`: `view_state(state, access.project.id, user, &key).await?` (step 4); otherwise an empty `Map`.
7. Merge, explicit arguments first:
   - `filter = args.filter.or_else(|| string(&view, "filter"))`;
   - `search = args.search.or_else(|| string(&view, "search"))`;
   - `include_eval = args.include_eval.unwrap_or_else(|| flag(&view, "eval"))`;
   - `include_experiments = args.include_experiments.unwrap_or_else(|| flag(&view, "exp"))`;
   where `fn string(view: &Map<String, Value>, key: &str) -> Option<String>` returns the value when it is a non-empty string, else `None`, and `fn flag(view: &Map<String, Value>, key: &str) -> bool` is true when the value is the string `"1"` or the number `1` (`view.get(key) == Some(&json!("1")) || view.get(key) == Some(&json!(1))`). (`filter`, `search`, `eval` and `exp` are list-page URL params a saved view stores, spec 11. The view's `range`, `from`, `to`, `sort`, `order` and `cols` are ignored: the window comes from the arguments and results are always `start_time` descending.)
8. Run the list as `routes::traces::list_traces` does. The window goes into `ListScope`, which filters the list; the context itself is opened without a window, as for every read endpoint (D101):
   ```rust
   let ctx = open_query(state, access.project.id, None).await?;
   let page = storage::trace_list(
       &ctx,
       storage::TraceListRequest {
           filter: filter.as_deref(),
           search: search.as_deref(),
           scope: storage::ListScope { window: Some(window), include_eval, include_experiments },
           sort: storage::TraceSort::StartTime,
           order: storage::SortOrder::Desc,
           limit,
           cursor: None,
           timeout: READ_TIMEOUT,
       },
   )
   .await?;
   let mut traces = convert::trace_summaries(&page.batches)?;
   attach_trace_scores(&ctx, &mut traces).await?;
   ```
   (Wrap a future in `Box::pin` if clippy reports `large_futures`.) A filter error comes back as `invalid_filter` through `?` (`ApiError::Query`).
9. `let more = page.next_cursor.is_some();` There is no cursor in the tool: the agent narrows the window or the filter instead. Each summary → `to_json(&summary)?` → `compact_summary(value, true)`; remember whether any cut.
10. Notes, in order: when `more` and `limit < MAX_TRACES`: `"more traces match; narrow the window or the filter, or raise limit (at most 100)"`; when `more` and `limit == MAX_TRACES`: `"more traces match; narrow the window or the filter"`; when anything was cut: `CUT_NOTE`.
11. Body keys in order: `traces`, `truncated` (`more`), `notes`. `output::fit_list(&mut body, "traces", max_chars, &format!("result cut at {max_chars} characters; later traces left out"));` Return `Ok(Value::Object(body))`.

### 4. The view

```rust
/// The state of the `traces` saved view `key` names. A key of 36 characters that parses as a
/// hyphenated UUID is a view id and is looked up by id only (never by name); any other key is an exact name, and among
/// several visible views with that name the user's own wins, then the lowest id
/// (`find_visible_by_name` order).
///
/// # Errors
///
/// `400 bad_request` `no saved traces view with that name or id` when no view matches;
/// `400 bad_request` `view is not a traces view` when the id names a visible view of
/// another kind.
async fn view_state(state: &AppState, project: ProjectId, user: &AuthUser, key: &str)
    -> Result<Map<String, Value>, ApiError>
```

- `key.len() == 36` and `Ok(id) = Uuid::try_parse(key)` (`Uuid::parse_str` would also accept 32 hex digits, braces or a `urn:uuid:` prefix; those keys are names here): `saved_views::get_visible(state.meta.reader(), project, Viewer::User(user.id), id).await?`; `None` → the first error; `Some(row)` with `row.kind != SavedViewKind::Traces` → the second error; else `Ok(row.state)`.
- Otherwise: `saved_views::find_visible_by_name(state.meta.reader(), project, Viewer::User(user.id), SavedViewKind::Traces, key).await?` and take the first row's `state`, or the first error when the list is empty.

Both errors are `ApiError::BadRequest { detail: Some(<message>.to_owned()), errors: Vec::new() }`.

### 5. `get_trace`

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetTraceArguments {
    project: String,
    trace_id: String,
    around: Option<TimeParam>,
    #[serde(default)]
    include_payloads: bool,
    max_chars: Option<i64>,
}

/// One trace (`ProjectPerm::View`), its summary and its span tree as a flat pre-order list,
/// like `GET /traces/{trace_id}`. Default exclusions do not apply. `include_payloads` keeps
/// the `input_preview` and `output_preview` of the trace and of every span, cut to 200
/// characters; the full input and output of one span come from `get_span`.
///
/// Answers `{"trace": TraceSummary, "spans": [{"depth", "detached", "span": SpanSummary}],
/// "truncated", "notes"}`.
pub(crate) async fn get_trace(state: &AppState, user: &AuthUser, raw: JsonObject) -> Result<Value, ApiError>
```

Body:

1. `let args: GetTraceArguments = arguments(raw)?; let max_chars = output::max_chars(args.max_chars)?;`
2. `let access = user_project(state, user, &args.project, ProjectPerm::View).await?;`
3. As `routes::traces::get_trace`:
   ```rust
   let ctx = open_query(state, access.project.id, args.around.map(TimeParam::ns)).await?;
   let data = storage::trace_detail(&ctx, &args.trace_id, READ_TIMEOUT)
       .await?
       .ok_or(ApiError::NotFound)?;
   let mut trace = convert::trace_summaries(std::slice::from_ref(&data.trace))?
       .pop()
       .ok_or(ApiError::NotFound)?;
   attach_trace_scores(&ctx, std::slice::from_mut(&mut trace)).await?;
   let spans = build_tree(convert::span_summaries(&data.spans)?);
   ```
   A `trace_id` that is not 32 hex digits fails in `trace_detail` with A2's `trace_id must be 32 hex digits` (`bad_request`). `around` (D154) only narrows which stored files are read; rows still in memory are always seen.
4. `let (trace, mut cut) = compact_summary(to_json(&trace)?, args.include_payloads);`
5. Each `TreeSpan` → `to_json(&tree_span)?`, an object `{depth, detached, span}`; when it is an object, compact its `span` field in place: `if let Some(inner) = object.remove("span") { let (span, c) = compact_summary(inner, args.include_payloads); cut |= c; object.insert("span".to_owned(), span); }` (`span` is the last key, so `remove` + `insert` keeps the order).
6. Notes, in order: when `data.truncated`: `format!("the trace has more than {0} spans; only the first {0} by start time are listed", storage::MAX_TRACE_SPANS)`; when `cut`: `CUT_NOTE`.
7. Body keys in order: `trace`, `spans`, `truncated` (`data.truncated`), `notes`. `output::fit_list(&mut body, "spans", max_chars, &format!("result cut at {max_chars} characters; later spans left out"));` Return it. When even the trace summary with no spans exceeds `max_chars`, `spans` is `[]` and the body is returned over the budget (as in H7's `query_sql`).

   Build the note in a private `fn truncated_note() -> String` so the test `get_trace_truncated_note` can call it.

### 6. `get_span`

```rust
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetSpanArguments {
    project: String,
    trace_id: String,
    span_id: String,
    around: Option<TimeParam>,
    max_chars: Option<i64>,
}

/// One span with every column (`ProjectPerm::View`), like
/// `GET /traces/{trace_id}/spans/{span_id}`: parsed input and output, system instructions,
/// tool definitions, attributes, events and links.
///
/// Answers `{"span": SpanDetail, "notes"}`.
pub(crate) async fn get_span(state: &AppState, user: &AuthUser, raw: JsonObject) -> Result<Value, ApiError>
```

Body:

1. Arguments, `max_chars`, `user_project(…, ProjectPerm::View)` as above.
2. Unlike the HTTP span endpoint, the tool takes `around` (D154) and passes it on; the HTTP endpoint opens its context with `None`. It only speeds up the lookup:
   ```rust
   let ctx = open_query(state, access.project.id, args.around.map(TimeParam::ns)).await?;
   let batch = storage::span_detail(&ctx, &args.trace_id, &args.span_id, READ_TIMEOUT)
       .await?
       .ok_or(ApiError::NotFound)?;
   let span = SpanDetail::from(convert::span(&batch, access.project.id)?);
   ```
3. `let Value::Object(mut span) = to_json(&span)? else { return Err(ApiError::Internal(Box::new(NotAnObject))) };` with `#[derive(Debug, thiserror::Error)] #[error("span detail is not a JSON object")] struct NotAnObject;`. Then `let cut = cut_values(&mut span, &["metadata", "attributes", "resource_attributes"], &PREVIEWS);`. Event and link `attributes` are not cut by this rule: they belong to the `HEAVY` fields `events` and `links`, which are cut only when the span overflows.
4. `let notes = if cut { vec![CUT_NOTE.to_owned()] } else { Vec::new() };`
5. Return `Ok(fit_span(&span, &notes, max_chars))`.

```rust
/// `{"span": span, "notes": notes}`, fitted to `max_chars`.
///
/// When it does not fit, every `HEAVY` field that is present, not `null`, and whose compact
/// JSON (`render`) is longer than `n` characters is replaced by the string
/// `output::cut(&render(field), n)` (its JSON text cut to `n` characters plus `…`), and the
/// note `"fields cut to {n} characters of their JSON text: {names}"` is appended (`names` =
/// the cut fields in `HEAVY` order, joined with ", "). `n` is the largest value in
/// `0..=max_chars` for which the whole body fits, found by binary search; if even `n = 0`
/// does not fit, the body with `n = 0` is returned with the extra last note
/// `"the span exceeds max_chars even with its large fields cut"`.
fn fit_span(span: &Map<String, Value>, notes: &[String], max_chars: usize) -> Value
```

Implementation:

1. `fn build(span: &Map<String, Value>, notes: &[String], n: Option<usize>) -> Value` (private): clone `span`; when `n` is `Some(n)`, cut the `HEAVY` fields as described and remember their names; return `json!({"span": <the map>, "notes": <notes, plus the "fields cut to …" note when at least one field was cut>})`.
2. `let body = build(span, notes, None);` if `char_len(&render(&body)) <= max_chars`, return `body`.
3. `let fits = |n: usize| char_len(&render(&build(span, notes, Some(n)))) <= max_chars;`
4. If `!fits(0)`: take `build(span, notes, Some(0))`, push `"the span exceeds max_chars even with its large fields cut"` onto its `notes` array (`body["notes"].as_array_mut()`), return it.
5. Binary search: `let (mut low, mut high) = (0, max_chars);` while `low < high` { `let mid = (low + high + 1) / 2;` if `fits(mid)` { `low = mid` } else { `high = mid - 1` } }. Return `build(span, notes, Some(low))`.

The body's length is not strictly monotonic in `n` (the note's digits change), but the search only returns an `n` it checked, so the result always fits.

### 7. Tool definitions (`mcp/tools.rs`)

Append after `list_saved_views`, in this order:

```rust
        tool(
            super::traces::SEARCH_TRACES,
            "Search traces",
            "Traces whose start_time is in [from, to), newest first. `from` and `to` are required and at most 30 days apart. Narrow with `filter` (a SQL boolean expression over the traces view, e.g. status = 'error' AND cost_total > 0.1; get_schema lists the columns), `search` (text in span input, output, name or system instructions), or a saved traces `view` (name or id from list_saved_views): explicit filter, search, include_eval and include_experiments override the view's, and the view's range and sort are ignored. Evaluation and experiment traces are hidden unless include_eval / include_experiments. Long metadata and preview values are cut to 200 characters.",
            json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": PROJECT_ARG},
                    "from": {"type": "string", "description": "Window start, inclusive: an RFC 3339 timestamp, or epoch milliseconds as a string."},
                    "to": {"type": "string", "description": "Window end, exclusive: an RFC 3339 timestamp, or epoch milliseconds as a string. At most 30 days after from."},
                    "view": {"type": "string", "description": "A saved traces view: its id, or its exact name."},
                    "filter": {"type": "string", "description": "Filter DSL: a SQL boolean expression over the traces view."},
                    "search": {"type": "string", "description": "Full-text search in span input, output, name and system instructions."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100, "default": 20, "description": "Most traces to return."},
                    "include_eval": {"type": "boolean", "description": "Include evaluation traces (environment 'llmobs-eval')."},
                    "include_experiments": {"type": "boolean", "description": "Include experiment traces."},
                    "max_chars": {"type": "integer", "minimum": 1000, "maximum": 100000, "default": 20000, "description": "Most characters in the result; later traces are left out beyond it."}
                },
                "required": ["project", "from", "to"],
                "additionalProperties": false
            }),
        ),
        tool(
            super::traces::GET_TRACE,
            "Get a trace",
            "One trace: its summary and its spans as a flat pre-order tree (depth, detached, span). Inputs and outputs are left out unless include_payloads, which adds input and output previews cut to 200 characters; get_span returns a span's full input and output.",
            json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": PROJECT_ARG},
                    "trace_id": {"type": "string", "description": "32 hex digits."},
                    "around": {"type": "string", "description": "A time near the trace's start (RFC 3339, or epoch milliseconds as a string). Speeds up the lookup."},
                    "include_payloads": {"type": "boolean", "default": false, "description": "Add input and output previews."},
                    "max_chars": {"type": "integer", "minimum": 1000, "maximum": 100000, "default": 20000, "description": "Most characters in the result; later spans are left out beyond it."}
                },
                "required": ["project", "trace_id"],
                "additionalProperties": false
            }),
        ),
        tool(
            super::traces::GET_SPAN,
            "Get a span",
            "One span with every field: parsed input and output, system instructions, tool definitions, model, tokens, cost, attributes, events and links. Long metadata and attribute values are cut to 200 characters; when the span is larger than max_chars, its large fields are cut to fit.",
            json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": PROJECT_ARG},
                    "trace_id": {"type": "string", "description": "32 hex digits."},
                    "span_id": {"type": "string", "description": "16 hex digits."},
                    "around": {"type": "string", "description": "A time near the span's start (RFC 3339, or epoch milliseconds as a string). Speeds up the lookup."},
                    "max_chars": {"type": "integer", "minimum": 1000, "maximum": 100000, "default": 20000, "description": "Most characters in the result; large fields are cut beyond it."}
                },
                "required": ["project", "trace_id", "span_id"],
                "additionalProperties": false
            }),
        ),
```

### 8. `mcp/mod.rs`

Add `mod traces;`. Add three arms after `LIST_SAVED_VIEWS`, in the same shape as the others (`traces::SEARCH_TRACES` → `traces::search_traces`, `traces::GET_TRACE` → `traces::get_trace`, `traces::GET_SPAN` → `traces::get_span`).

In `tools_list_has_every_tool_read_only` and in `stateless_2026_07_28_requests` (b), the names become `["list_projects", "get_schema", "query_sql", "list_saved_views", "search_traces", "get_trace", "get_span"]`. In the first, also assert `inputSchema.required`: `search_traces` = `["project", "from", "to"]`, `get_trace` = `["project", "trace_id"]`, `get_span` = `["project", "trace_id", "span_id"]`.

## Tests

In `mcp/traces.rs`, `#[cfg(test)] mod tests`. Imports: `std::str::FromStr`; `serde_json::{Map, Value, json}`; `crate::mcp::test_helpers::*`; `crate::mcp::output::{char_len, render}`; `super::*`; `crate::api::saved_views::SavedViewKind`; `crate::auth::roles::{OrgRole, ProjectOverride}`; `crate::meta::members::set_project_override`; `crate::meta::saved_views::{self, NewSavedView}`; `llmobs_core::ids::{SpanId, TraceId}`; `crate::test_support::{TestApp, dataset_span}`. Drop any import the compiler reports unused.

`#[tokio::test(flavor = "multi_thread")]` except the log test (`#[tokio::test]`). Every test starts with `let (app, seeded, tok) = app_with_token().await; app.load_example_dataset(seeded.project.id).await;`. `P` is `seeded.project.id.to_string()`; `T1` … `T6` stand for `a1000000000000000000000000000001` … `…006` (write them out). Tool arguments are passed to `call` as `&json!(…)` (H6's helpers take `&Value`). "Error `c` / `m`" means `is_error` is true, `body["error"]["code"]` = `c` and `body["error"]["message"]` = `m`. Fixtures are loaded with `serde_json::from_str::<Value>(include_str!("../../../../fixtures/api/responses/…")).unwrap()`.

Saved views are inserted as in H7's tests:

```rust
async fn insert(app: &TestApp, project_id: ProjectId, owner: Uuid, kind: SavedViewKind,
                name: &str, state: Value, shared: bool) -> Uuid {
    let now = app.state.clock.now_ns();
    let name = name.to_owned();
    let Value::Object(state) = state else { panic!("state must be a JSON object") };
    app.state.meta.write(move |conn| Box::pin(async move {
        let row = saved_views::insert_view(conn, NewSavedView {
            project_id, kind, name, state, owner_user_id: owner, shared, is_default: false,
        }, now).await?;
        Ok(row.id)
    })).await.unwrap()
}
```

**`search_traces`** (arguments besides `{"project": P, "from": FROM, "to": TO}`)

| Test | Arguments | Asserts |
|---|---|---|
| `search_traces_matches_the_list_endpoint` | none | not an error; `body["traces"]` = `traces.json`'s `data` (T6, T4, T3, T2, T1 with their scores; nothing to cut); `truncated` false; `notes` `[]` |
| `search_traces_lifts_exclusions_on_request` | `"include_eval": true` | ids `[T6, T4, T3, T2, T5, T1]` |
| `search_traces_filters_and_searches` | (a) `"filter": "status = 'error'"`; (b) `"search": "refund"`; (c) `"from": "2026-01-01T10:06:00Z"` | (a), (b): ids `[T3]`; (c): `[T6, T4, T3]` |
| `search_traces_reports_more` | (a) `"limit": 2`; (b) `"limit": 100, "to": "2026-01-01T10:06:00Z"` (two traces, T2 and T1, in that window) | (a) ids `[T6, T4]`, `truncated` true, `notes` = `["more traces match; narrow the window or the filter, or raise limit (at most 100)"]`; (b) ids `[T2, T1]`, `truncated` false, `notes` `[]` |
| `search_traces_validates_arguments` | no `from`; `"to": FROM`; `"to": "2026-01-31T00:00:01Z"`; `"limit": 0`, `101` and `-1`; `"max_chars": 999` and `100001`; `"x": 1`; `"filter": "nope = 1"` | error `bad_request` / `from and to are required (RFC 3339, at most 30 days apart)`; `bad_request` / `from must be before to`; `bad_request` / `the window is at most 30 days`; the three limits: `bad_request` / `limit must be between 1 and 100`; both: `bad_request` / `max_chars must be between 1000 and 100000`; `bad_request`, message starts with ``invalid arguments: unknown field `x` ``; filter: `is_error`, `code` `invalid_filter`, a non-empty `message` |
| `search_traces_fits_max_chars` | `"include_eval": true, "max_chars": 1000` | `truncated` true; the last note is `"result cut at 1000 characters; later traces left out"`; `char_len(&render(&body))` ≤ 1000; `traces.len()` = 1 and `traces[0].trace_id` = `T6` (T6's summary is about 570 characters and T4's about 600) |

**Saved views in `search_traces`.** `let bob = app.add_member(&seeded, "bob@example.com", OrgRole::Member).await;` and `let bob_tok = app.personal_token(bob.id, false).await;`. ada is `seeded.user_id`.

| Test | Setup | Calls and asserts |
|---|---|---|
| `view_by_name_and_id` | ada private `Traces` `Errors` `{"filter": "status = 'error'"}` | `"view": "Errors"` → `[T3]`; `"view": "<Errors id>"` → `[T3]` |
| `explicit_arguments_override_the_view` | ada private `Traces` `Errors` as above; ada private `Traces` `With eval` `{"eval": "1"}` | `"view": "Errors", "filter": "name = 'support-agent'"` → `[T2, T1]`; `"view": "With eval"` → six ids, `T5` fifth; `"view": "With eval", "include_eval": false` → `[T6, T4, T3, T2, T1]` |
| `view_name_prefers_own_then_lowest_id` | bob shared `Traces` `Pick` `{"filter": "status = 'error'"}`; then ada private `Traces` `Pick` `{"filter": "name = 'describe-image'"}`; then bob shared `Traces` `Shared` `{"filter": "name = 'describe-image'"}`; then bob shared `Traces` `Shared` `{"filter": "status = 'error'"}` | ada `"view": "Pick"` → `[T4]` (her own wins over bob's older shared view); ada `"view": "Shared"` → `[T4]` (both are bob's; the lower id, the first inserted, wins); bob `"view": "Pick"` → `[T3]` (ada's view is private) |
| `others_private_views_are_not_found` | bob private `Traces` `Secret` `{"filter": "status = 'error'"}` | ada `"view": "Secret"` → error `bad_request` / `no saved traces view with that name or id`; ada `"view": "<Secret id>"` → the same |
| `hex_names_resolve_by_name` | ada private `Traces` named `0190000000007000800000000000abcd` (32 hex digits) `{"filter": "status = 'error'"}` | `"view": "0190000000007000800000000000abcd"` → `[T3]` (not a 36-character UUID, so it is a name) |
| `view_errors` | ada shared `Sql` `Costs` `{"query": "SELECT 1", "params": {}}`; ada private `Traces` named `01900000-0000-7000-8000-00000000abcd` `{"filter": "status = 'error'"}` | `"view": "Costs"` → `bad_request` / `no saved traces view with that name or id`; `"view": "<Costs id>"` → `bad_request` / `view is not a traces view`; `"view": "nope"` → `bad_request` / `no saved traces view with that name or id`; `"view": "01900000-0000-7000-8000-00000000abcd"` → `bad_request` / `no saved traces view with that name or id` (a UUID-shaped key is looked up by id only) |

| Test | Setup | Calls and asserts |
|---|---|---|
| `view_flags_and_search` | ada private `Traces` `Exp` `{"exp": "1", "search": "refund"}`; ada private `Traces` `Num` `{"eval": 1}` | `"view": "Exp"` → `[T3]` (its search applies; no experiment traces exist in the dataset, so `exp` changes nothing visible but must not fail); `"view": "Exp", "search": "order 1234"` → the search argument replaces the view's: `[T1]` (only T1's spans contain `order 1234`); `"view": "Num"` → six ids, `T5` fifth; `"include_experiments": true` alone → `[T6, T4, T3, T2, T1]` |
| `hidden_projects_are_not_found` | `let (playground, _) = app.add_project(&seeded, "playground", None).await;` `let viewer = app.add_member(&seeded, "viewer@example.com", OrgRole::Viewer).await;` override `none` on playground for `viewer.id` (`set_project_override(conn, playground.id, viewer.id, ProjectOverride::None)` in `app.state.meta.write`); `let vtok = app.personal_token(viewer.id, false).await;` | with `vtok`: `search_traces` `{"project": "acme/playground", "from": FROM, "to": TO}` and `get_trace` `{"project": "acme/playground", "trace_id": T1}` → both `{"error":{"code":"not_found","message":"Not Found"}}` |

**`get_trace`**

| Test | Arguments | Asserts |
|---|---|---|
| `get_trace_matches_the_detail_endpoint` | `{"project": P, "trace_id": T1, "include_payloads": true}` | `body["trace"]` = the fixture `traces/<T1>.json`'s `trace`; `body["spans"]` = its `spans`; `truncated` false; `notes` `[]` |
| `get_trace_leaves_previews_out_by_default` | same without `include_payloads` | equal to the fixture's `trace` and `spans` after removing `input_preview` and `output_preview` from `trace` and from every `spans[i].span`; `spans[*].span.span_id` = `["b100000000000011", "b100000000000012", "b100000000000013", "b100000000000014"]`; `spans[*].depth` = `[0, 1, 1, 1]` |
| `get_trace_marks_a_detached_root` | `trace_id` `T6` | `spans.len()` 1; `spans[0].detached` true; `spans[0].depth` 0 |
| `get_trace_fits_max_chars` | `T1`, no payloads, `"max_chars": 1500` | `truncated` true; `notes` = `["result cut at 1500 characters; later spans left out"]`; `char_len(&render(&body))` ≤ 1500; `spans.len()` = 1 and `spans[0].span.span_id` = `"b100000000000011"` (the body without spans is about 800 characters and the four spans about 430, 650, 460 and 650, so exactly one fits) |
| `get_trace_takes_around` | `T1`, `"around": "2026-01-01T10:00:00Z"`; `T1`, `"around": "soon"` | first: not an error, `trace.trace_id` = `T1`; second: `bad_request`, message starts with `invalid arguments: expected an RFC 3339 timestamp or epoch milliseconds`. (`around` only prunes stored files, and the test data is still in memory, so a far-away `around` cannot be shown to miss it.) |
| `get_trace_summary_alone_over_budget` | commit the long-values span of § Long values below with, in addition, `metadata` holding 20 keys `k00`…`k19` each with 190 `m` (cut to nothing: under 200); `get_trace` of `…007`, `"max_chars": 1000` | `spans` = `[]`; `truncated` true; the last note is `"result cut at 1000 characters; later spans left out"`; the body may exceed 1,000 characters (the trace summary alone does not fit, as documented in step 5) |
| `get_trace_truncated_note` (plain `#[test]`) | no tool call: `truncated_note()` = `"the trace has more than 20000 spans; only the first 20000 by start time are listed"` (loading 20,001 spans is too slow for a unit test; the branch is one `if`) |
| `get_trace_errors` | `trace_id` `a1000000000000000000000000000009`; `trace_id` `xyz`; `"max_chars": 999`; `"x": 1` | `{"error":{"code":"not_found","message":"Not Found"}}`; `bad_request` / `trace_id must be 32 hex digits`; `bad_request` / `max_chars must be between 1000 and 100000`; `bad_request`, message starts with ``invalid arguments: unknown field `x` `` |

**`get_span`**

| Test | Arguments | Asserts |
|---|---|---|
| `get_span_matches_the_span_endpoint` | `{"project": P, "trace_id": T1, "span_id": "b100000000000012"}`, and again with `"around": "2026-01-01T10:00:00Z"` | both: `body["span"]` = `spans/<T1>/b100000000000012.json`; `notes` `[]` |
| `get_span_cuts_large_fields_to_fit` | same, `"max_chars": 1300` (the full result is about 1,680 characters, about 1,000 of them small fields) | `char_len(&render(&body))` ≤ 1300; `body["span"]["span_id"]` = `"b100000000000012"`; `body["span"]["output"]` is a string ending with `…`; the last note starts with `"fields cut to "` and contains `output`; `body["span"]["events"]` is still `[]` (two characters: never longer than `n`) |
| `get_span_errors` | `span_id` `b1000000000000ee`; `"max_chars": 100001`; `"x": 1` | `{"error":{"code":"not_found","message":"Not Found"}}`; `bad_request` / `max_chars must be between 1000 and 100000`; `bad_request`, message starts with ``invalid arguments: unknown field `x` `` |

**Long values in all three tools.** `values_are_cut_to_200_characters`: build one more span from `dataset_span(13)` (trace 6's only span) and commit it with `app.commit_spans(&seeded.project, vec![span]).await`, after setting: `trace_id = TraceId::from_str("a1000000000000000000000000000007").unwrap()`, `span_id = SpanId::from_str("b100000000000071").unwrap()`, `parent_span_id = None`, `name = "long-values".to_owned()`, `start_time = 1_767_264_000_000_000_000` (10:40 UTC), `end_time = start_time + 1_000_000_000`, `metadata = {"note": 300 × "m"}`, `attributes = {"long": 300 × "a"}`, `resource_attributes = {"svc.long": 300 × "r"}`, `input_preview = Some(300 × "i")`, `output_preview = Some(300 × "o")`. Then:

- `search_traces` with `"from": "2026-01-01T10:35:00Z"`: one trace, `trace_id` `…007`; its `metadata.note`, `input_preview` and `output_preview` have 201 characters ending with `…`; `notes` = `[CUT_NOTE]`.
- `get_trace` of `…007` with `"include_payloads": true`: `trace.metadata.note`, `trace.input_preview`, `spans[0].span.metadata.note` and `spans[0].span.input_preview` have 201 characters; `notes` = `[CUT_NOTE]`. Without `include_payloads`: no preview keys, `spans[0].span.metadata.note` has 201 characters, `notes` = `[CUT_NOTE]`.
- `get_span` of `…007` / `b100000000000071`: `metadata.note`, `attributes.long`, `resource_attributes["svc.long"]`, `input_preview` and `output_preview` have 201 characters; `notes` = `[CUT_NOTE]`.

**Logs**

| Test | Steps | Asserts |
|---|---|---|
| `trace_tools_log_no_arguments` (`#[tokio::test]`) | `capture_info_logs` (H6's `test_helpers`) around `search_traces` with `"filter": "name = 'canary-f-1'"` and `"search": "canary-s-2"`, and `get_trace` with `trace_id` `"canary-t-3"` | the logs contain `tool call` and `search_traces`; none of `canary-f-1`, `canary-s-2`, `canary-t-3` |

**Unit tests** (plain `#[test]`)

- `cut_values_cuts_maps_and_strings`: object `{"metadata": {"a": <300 × x>, "b": "short", "n": 5}, "input_preview": <250 × y>, "output_preview": null, "other": <300 × z>}`; `cut_values(&mut o, &["metadata"], &PREVIEWS)` → `true`; `metadata.a` has 201 characters ending with `…`; `metadata.b` = `"short"`; `metadata.n` = `5`; `input_preview` has 201 characters; `output_preview` is `null`; `other` keeps 300. A second call returns `false`.
- `compact_summary_drops_previews_when_asked`: `compact_summary(json!({"input_preview": "a", "output_preview": "b", "name": "n"}), false)` → `(json!({"name": "n"}), false)`.
- `fit_span_returns_a_fitting_span_unchanged`: span `{"span_id": "x", "input": {"format": "text", "value": "hi"}}`, no notes, `max_chars` 1000 → `{"span": {…same…}, "notes": []}`.
- `fit_span_cuts_heavy_fields_only`: span `{"span_id": "x", "input": {"format": "text", "value": <5000 × a>}, "output": {"format": "text", "value": <5000 × b>}, "events": null}`; `max_chars` 1000 → rendered length ≤ 1000; `span.span_id` = `"x"`; `span.events` is `null` (null is never cut); `span.input` and `span.output` are strings ending with `…` with equal character counts `c`; `notes` = `[format!("fields cut to {} characters of their JSON text: input, output", c - 1)]`.
- `fit_span_reports_when_nothing_fits`: span with `"metadata"` holding 100 keys `k0`…`k99`, each a 190-character value (not heavy), `max_chars` 1000 → the last note is `"the span exceeds max_chars even with its large fields cut"`.
- `bounded_limits`: `bounded(None, 20, 100, "m")` = `Ok(20)`; `Some(100)` → `Ok(100)`; `Some(0)`, `Some(101)`, `Some(-1)` → `Err(ApiError::BadRequest { .. })`.

## Commands

```bash
cargo test -p llmobs-server mcp
export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH" && just check
```

## Done when

- `tools/list` lists the seven read-only tools of README § Design → MCP server.
- `search_traces`, `get_trace` and `get_span` return what the read endpoints return for the example dataset, compacted as above, and fit `max_chars`.
- `search_traces` requires a window of at most 30 days and resolves saved traces views by id or exact name.
- `just check` passes; one commit, `server: add the MCP trace tools`.

Report "no new dependency".
