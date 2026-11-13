# G · M2 — Read API

Implementation plan for milestone M2 of track G. It has 8 sub-phases, G2.1 to G2.8. Each one is done by one agent in one go, in order, and ends with `just check` green and one commit.

**Prerequisites** (checked when this plan was written, 2026-11-13):
- M1 is merged (PR #43).
- The A2 review fixes M-2, M-3 and M-4 are merged (PR #48): `bucket=auto` is capped at 2,000 days, session and user details no longer collect every row, and cost sums are exact.
- Node `26.13.0` is installed at `$HOME/.local/share/nvm/v26.13.0/bin`. It is not on the non-interactive `PATH`.

**Read first** (every sub-phase):
- `AGENTS.md`, all of it.
- This file's sections up to § G2.1, then your own sub-phase.
- `docs/plans/g-spine/00-design.md`: § Decisions, § Architecture (State, Modules, Read API (M2)), § Testing.
- `docs/spec/09-api.md`: § Conventions, § Observability data, § Response shapes.
- `docs/spec/05-query.md`: § Default exclusions, § Full-text search, § Dashboard metrics.
- `docs/mvp-decisions.md`: D92, D94, D101, D153, D154, D-A2-3, D-A2-4, D-G-1, D-G-14, D-G-17, D-G-23, D-G-24, D-G-25.
- Code:
  - `crates/storage/src/reads/mod.rs`, plus the public items and doc comments of `reads/{traces,sessions,users,scores,metrics}.rs`;
  - `crates/storage/src/query_error.rs`, `crates/storage/src/views/catalog.rs` (public items), and `crates/storage/src/lib.rs` (the `pub use` list, `Storage::query`, `QueryOptions`, `get_media`);
  - `crates/server/src/api/{common,traces,sessions,users,scores,metrics,schema,payload}.rs`;
  - `crates/server/src/http/{mod.rs,access.rs,error.rs}`, `crates/server/src/routes/projects.rs` (a handler in the house style), `crates/server/src/test_support/mod.rs`.

## What M2 is, and what it is not

When M2 is done:
- every read operation already declared in `crates/server/src/api/openapi.rs` has a handler: `list_traces`, `get_trace`, `list_spans`, `get_span`, `list_sessions`, `get_session`, `list_users`, `get_user`, `list_scores`, `get_metrics`, `get_schema`, `get_media`;
- `fixtures/api/responses/` and `fixtures/api/manifest.json` are written by those handlers (`just api-examples`), and the hand-written model in `crates/server/tests/api_examples.rs` is gone;
- `openapi.json` declares the errors the read handlers really return (D-G-23).

**Out of scope:**
- the two SSE streams `stream_traces` and `stream_trace` (M4);
- score writes, gRPC, usage counters (M3);
- the UI on the real API (M5);
- saved views, the SQL console and MCP (track H). `list_saved_views` keeps its static example.

M2 adds no routes beyond the twelve above.

## Decisions this plan relies on

| # | Summary |
|---|---|
| D-G-1 | One `AppState`. M2 adds one field, `metrics_cache`. |
| D-G-14 | Read handlers are thin: parse params, open a query context, call the A2 read with a 30 s timeout, convert the Arrow rows in `api::convert`, answer JSON. |
| D-G-17 | `fixtures/api` example responses come from the real handlers. |
| D-G-23 | The read operations declare `422` and `503`. Media declares `503`. A new problem code, `range_not_satisfiable` (416), is added. |
| D-G-24 | Media ranges: the server reads the whole blob (at most `media.max_bytes`) and slices it. One `bytes=` range is honoured; anything else is served as `200` with the whole blob. |
| D-G-25 | The example generator is a unit test inside `llmobs-server`. `get_schema` is compared with `static/schema.json`. `get_metrics` is generated over a fixed window and listed with an empty query. |
| D94 | Query error codes: `invalid_filter`, `query_failed`, `query_timeout`, `query_too_large`, `busy`. Messages go to the caller and are never logged. |
| D101 | Read endpoints build their context without a window. A2's `ListScope` applies the request's window. |
| D154 | `?around=` goes through `QueryOptions { around }`. |

## Versions

No new crate. In `crates/server/Cargo.toml`, move `arrow.workspace = true` from `[dev-dependencies]` to `[dependencies]` (G2.2). `datafusion` stays a dev-dependency. Report "no new dependency" at the end of every sub-phase.

## Conventions for every sub-phase

- **Node.** Before any `just` recipe that runs web or TypeScript checks (`just check`, `just check-web`, `just openapi`), run `export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH"` in the same shell command. `node --version` must print `v26.13.0`.
- **Checks.** While iterating, `just check-rust` is enough. Before the commit, run `just check`; it must pass. Never commit with a failing check.
- **Errors and panics.** No `unwrap()`/`expect()` outside tests (AGENTS.md). Errors are `thiserror` enums; `ApiError` is the handler error.
- **Privacy.** A handler never logs filter text, search text, a payload or an attribute value. Log ids, counts and `RunError::kind()` only.
- **Tests.** HTTP tests build a `TestApp`, call `seed()`, call `load_example_dataset` (G2.1), and send requests with `.header("cookie", &seeded.cookie)`. They use `#[tokio::test(flavor = "multi_thread")]` like `routes/otlp.rs`. Assert exact values from § Dataset facts.
- **Names.** Storage types share names with API types (`TraceSort`, `SortOrder`, `MetricsGroupBy`, `Kpi`, `ViewName`, `Page`). In server code, refer to storage items through `llmobs_storage::…` (or `use llmobs_storage as storage;` and `storage::TraceSort`), never with a bare `use` that shadows the API type.
- **Large futures.** If clippy reports `large_futures` on an A2 call, wrap the future in `Box::pin(...)` and add no `#[allow]`.
- **STOP rules.** Stop and report instead of improvising when:
  - an A2 function's behaviour differs from what this plan states (for example a column is missing or has another type);
  - a test from § Dataset facts fails, and the failure is in what storage returns, not in your code;
  - in G2.7, an `example_scenarios` assertion fails, or a regenerated file differs between two runs.
- **Commit.** One commit per sub-phase, with the subject given in the sub-phase and no trailer lines.
- **Report.** What you did; every deviation from this plan, and why; "no new dependency"; anything left unverified.

## Final layout after M2

```
crates/server/src/
  api/
    convert.rs        Arrow rows → API types; ConvertError (G2.2–G2.5)
    traces.rs         `stored_payload` becomes pub(crate) (G2.3)
    common.rs         ProblemCode::RangeNotSatisfiable (G2.6)
    metrics.rs        MetricsBucket and MetricsGroupBy derive Hash (G2.5)
    openapi.rs        READ_ERRORS, media errors, problem descriptions (G2.6)
  http/
    error.rs          ApiError::Query, ApiError::RangeNotSatisfiable, From<RunError>, From<QueryError> (G2.1, G2.6)
    mod.rs            AppInner.metrics_cache (G2.5)
    access_suite.rs   placeholders for {trace_id}, {span_id}, {session_id}, {user_id}, {sha256} (G2.3)
  routes/
    read.rs           READ_TIMEOUT, open_query, window, list_scope, page_limit, sort mapping, score attachment (G2.1, G2.3, G2.4)
    traces.rs         list_traces, get_trace, list_spans, get_span (G2.3)
    sessions.rs       list_sessions, get_session (G2.4)
    users.rs          list_users, get_user (G2.4)
    scores.rs         list_scores (G2.5)
    schema.rs         get_schema (G2.5)
    metrics.rs        get_metrics, MetricsCache (G2.5)
    media.rs          get_media (G2.6)
  test_support/mod.rs load_example_dataset (G2.1), seed_examples (G2.7)
  api_examples.rs     #[cfg(test)] generator of fixtures/api (G2.7)
crates/server/tests/api_examples.rs   file checks only; the hand model is deleted (G2.7)
crates/server/tests/support/mod.rs    loaders only; the reference aggregations are deleted (G2.7)
crates/cli/src/main.rs                8 MiB worker stacks (G2.1)
```

## Shared reference

### A2 outputs

All read functions are re-exported at the crate root: `llmobs_storage::{trace_list, trace_detail, span_list, span_detail, session_list, session_detail, user_list, user_detail, score_list, latest_scores, metrics}`. The request types are there too. Each function takes `&QueryContext` and enforces its own `timeout` (it returns `RunError::Timeout`).

| Function | Returns | Columns used by G |
|---|---|---|
| `trace_list` | `Page { batches, next_cursor }` | traces view columns (table below) |
| `trace_detail(ctx, trace_id, timeout)` | `Option<TraceDetailData { trace, spans, truncated }>` | `trace`: traces view, one row. `spans`: `SPAN_LIST_COLUMNS`, ordered `start_time, span_id` |
| `span_list` | `Page` | `SPAN_LIST_COLUMNS` |
| `span_detail(ctx, trace_id, span_id, timeout)` | `Option<RecordBatch>` | every spans view column (all `Span` fields except `project_id`) |
| `session_list` | `Page` | sessions view columns |
| `session_detail(ctx, session_id, include_eval, include_experiments, timeout)` | `Option<SessionDetailData { session, turns, roots, truncated }>` | `session`: sessions view. `turns`: traces view. `roots`: `trace_id, input, input_format, output, output_format` |
| `user_list` | `Page` | `USER_COLUMNS` |
| `user_detail(ctx, user_id, include_eval, include_experiments, timeout)` | `Option<UserDetailData { user, sessions }>` | `user`: `USER_COLUMNS`. `sessions`: sessions view |
| `score_list` | `Page` | scores view columns |
| `latest_scores(ctx, ScoreTargets::Traces(&[&str]) or Sessions(..), timeout)` | `Vec<RecordBatch>` | `LATEST_SCORE_COLUMNS`: `target_key, name, score_id, data_type, value_num, value_str, source` |
| `metrics` | `MetricsData` (plain structs) | — |

`ids` are lowercase hex `Utf8` in every view (`trace_id`, `span_id`, `parent_span_id`, `root_span_id`). Link ids inside `spans.links` stay `FixedSizeBinary` (16 and 8 bytes). Ids are passed to A2 as strings; A2 accepts either case.

### Column → field tables

The converter reads columns **by name**, never by position. "Cast" means: when the column's type differs from the one listed, convert it with `arrow::compute::cast` first. Use the `llmobs_core::schema::{spans,traces,scores}` constants for names. Columns that only views add (`duration_ns` on traces and sessions, `trace_count`, `session_count`, `first_seen`, `last_seen`, `target_key`) get local `const`s in `convert.rs`.

**`TraceSummary`** ← traces view:

| Field | Column | Type (cast to) | Null |
|---|---|---|---|
| `trace_id` | `trace_id` | `Utf8`, parsed with `TraceId::from_str` | error |
| `root_span_id` | `root_span_id` | `Utf8` → `SpanId` | `None` |
| `has_root` | `has_root` | `Boolean` | error |
| `name` | `name` | `Utf8` | error |
| `start_time`, `end_time` | same | `Timestamp(ns, "UTC")` | error |
| `duration_ns` | `duration_ns` | `Int64` | error |
| `status` | `status` | `Utf8` → `SpanStatus::parse` | error |
| `session_id`, `user_id`, `environment`, `release`, `input_preview`, `output_preview` | same | `Utf8` | `None` |
| `tags`, `models` | same | `List(Utf8)` | empty |
| `metadata` | `metadata` | `Map(Utf8, Utf8)` | empty |
| `span_count`, `error_count`, `llm_count` | same | `Int64` | error |
| `input_tokens` … `total_tokens` (6) | same | `Int64` | `None` |
| `cost_total` | `cost_total` | `Float64` | `None` |
| `scores` | — | `Vec::new()`; filled by the handler | — |

**`SpanSummary`** ← `SPAN_LIST_COLUMNS`. The fields are the same as the columns, except:
- `trace_id`, `span_id` and `parent_span_id` are parsed from hex (`parent_span_id` may be `None`);
- `span_kind` uses `SpanKind::parse`, and `status` uses `SpanStatus::parse`;
- `model` is `response_model`, or `request_model` when that is null. This is the same rule as `SpanSummary::from(&Span)`; do not read the stored `model` column;
- `request_model`, `response_model`, `cost_input` and `cost_output` are read only to compute `model`, or not at all.

**`Span`** (for `SpanDetail`) ← spans view row. Every `Span` field is read from its own column, except `project_id`, which comes from the caller's `ProjectAccess`:
- **Enums.** `cost_source` uses `CostSource::parse`, and `input_format`/`output_format` use `PayloadFormat::parse` (null → `None`). `conventions` is `List(Utf8)`, each item parsed with `Convention::parse`.
- **`events`.** `List(Struct{time: Timestamp, name: Utf8, attributes: Utf8?})` → `SpanEvent`.
- **`links`.** `List(Struct{trace_id: FixedSizeBinary(16), span_id: FixedSizeBinary(8), attributes: Utf8?})` → `SpanLink`, using `TraceId::from_slice` and `SpanId::from_slice`.
- **`schema_version`.** `UInt16`.
- **Ingest fields.** `ingest_time` is a timestamp. `media_refs` and `truncated` are `List(Utf8)`.

`SpanDetail::from(span)` then does the payload parsing.

**`SessionSummary`** ← sessions view:
- `session_id`, `start_time`, `end_time`, `duration_ns`, `trace_count` and `error_count` are required;
- `total_tokens`, `cost_total` and `user_id` are nullable;
- `tags` is `List(Utf8)`;
- `scores` starts as `Vec::new()`.

**`EndUserSummary`** ← `USER_COLUMNS`: `user_id` (required), `first_seen`, `last_seen` (timestamps), `trace_count`, `session_count`, `error_count` (`Int64`, required), `total_tokens` (`Int64`, nullable), `cost_total` (`Float64`, nullable).

**`Score`** ← scores view row, `project_id` from the caller:
- **Target.** `target` is `ScoreTargetKind::parse`:
  - `span` → `ScoreTarget::Span { trace_id, span_id }`, both from hex, both required;
  - `trace` → `ScoreTarget::Trace { trace_id }`;
  - `session` → `ScoreTarget::Session { session_id }`, from the `session_id` column, which is required.
- **`session_id`.** For `span` and `trace` targets, `Score.session_id` is the `session_id` column (nullable). For `session` targets it is `None`, because `scores_to_record_batch` stores the target's id in that column.
- **`value`.** See `score_value` below.
- **UUIDs.** `score_id` is `Utf8` → `Uuid`, required. `evaluator_id`, `rule_id`, `experiment_id`, `dataset_item_id` and `author_user_id` are `Utf8` → `Uuid`, nullable.
- **Other fields.** `user_id` and `reason` are nullable. `name` is required. `source` uses `ScoreSource::parse`. `metadata` is a map. `created_at`, `updated_at` and `ingest_time` are timestamps. `deleted` is `Boolean`.

**`ScoreBrief`** ← `LATEST_SCORE_COLUMNS`. It has `score_id`, `name`, `value` (`score_value`) and `source`. Briefs are grouped by `target_key` and keep A2's order (by name).

**`score_value(data_type, value_num, value_str)`** turns `ScoreDataType::parse(data_type)` into a value:
- `numeric` → `Numeric(value_num)`;
- `boolean` → `Boolean(value_num != 0.0)`;
- `categorical` → `Categorical { label: value_str, value: value_num }`;
- `text` → `Text(value_str)`.

A null required input is a `ConvertError::Null`. `numeric` and `boolean` need `value_num`; `categorical` and `text` need `value_str`.

**Strings may be `Utf8View`.** DataFusion's Parquet reader returns `Utf8View` for strings after a flush, while memtable rows are `Utf8`. Every string read goes through the casting helper: top-level columns, list items, map keys and values, and the `name`/`attributes`/`time` children of `events` and `links`. The struct-child helper is `fn child(array: &StructArray, name: &'static str, data_type: &DataType) -> Result<ArrayRef, ConvertError>`: `column_by_name`, then `cast` when the type differs.

**Lists and maps.**
- A list is read with `ListArray::value(row)`, cast to `Utf8`, and null items are skipped.
- A map is read with `MapArray::value(row)`, a struct of `(keys, values)` cast to `Utf8`. An entry whose value is null is skipped: `llmobs_core::batch` never writes one.

### Dataset facts

`fixtures/api/dataset/` holds 6 traces, 14 spans and 6 scores, all on 2026-01-01 UTC. The tests assert these facts.

| Trace (`a1…000N`) | Root / spans | Start → end | Session, user | Environment | Notes |
|---|---|---|---|---|---|
| 1 | `b1…11` root, children `12`, `13`, `14` | 10:00:00 → 10:00:04.2 | `sess-001`, `user-42` | production | tags `beta`; cost 0.0015 + 0.00156 = 0.00306 |
| 2 | `21` root, `22` | 10:05:00 → 10:05:02.5 | `sess-001`, `user-42` | production | tags `beta`, `refund`; cost 0.00246 |
| 3 | `31` root, `32` | 10:10:00 → 10:10:00.8 | `sess-002`, `user-7` | production | both spans `error`; no cost |
| 4 | `41` root, `42`, `43` | 10:20:00 → 10:20:03 | —, `user-42` | staging | `43` input has a media `uri` part; cost 0.00133 |
| 5 | `51` root, `52` | 10:01:00 → 10:01:01.5 | `sess-001`, `user-42` | `llmobs-eval` | excluded by default |
| 6 | `61` only, parent `b1000000000000ff` missing | 10:30:00 → 10:30:02 | — | production | no root; one detached span; cost 0.0027 |

Scores (`01900000-0000-7000-8000-000000000301` … `…0306`; created at):

| Id | Target | Name | Value | Source | Created |
|---|---|---|---|---|---|
| 0301 | trace 1 | `helpfulness` | numeric 0.5 | `api` | 10:00:30 |
| 0302 | trace 1 | `helpfulness` | numeric 0.9 (updated 10:01:02) | `llm_judge` | 10:00:40 |
| 0303 | span 1/`14` | `tone` | categorical `friendly` (1.0) | `llm_judge` | 10:04:30 |
| 0304 | trace 2 | `thumbs` | boolean true | `end_user` | 10:07:00 |
| 0305 | session `sess-001` | `resolved` | boolean true | `human` | 10:08:00 |
| 0306 | trace 3 | `note` | text `Needs retry with backoff.` | `human` | 10:11:00 |

Media: `497790947d4666760ce38f3c00e852c71fdb66cae849bae8e9ede352719e1581`, `image/png`, 70 bytes (`fixtures/api/media/<sha>.png`).

Expected results:
- **Trace list.** The default list, newest first, is traces 6, 4, 3, 2, 1. With `include_eval=true` it is 6, 4, 3, 2, 5, 1.
- **Scores on traces.** Trace 1 has one brief, `helpfulness` numeric 0.9: 0302 wins because its `updated_at` is later. Trace 2 has `thumbs` true and trace 3 has `note`.
- **Span list.** The default list has 12 spans (trace 5's two are excluded). The first is `b100000000000061`.
- **Session list.** The default order is `sess-002` (10:10) then `sess-001` (10:00):
  - by default, `sess-001` has `trace_count` 2 and tags `["beta", "refund"]`;
  - with `include_eval=true` it has `trace_count` 3 and tags `["beta", "eval", "refund"]`;
  - its scores are `[resolved: boolean true]`.
- **User list.** The order is `user-42` (last seen 10:20:03) then `user-7`:
  - `user-42` has `trace_count` 3 and `session_count` 1 (trace 4 has no session);
  - with `include_eval=true`, `user-42` has `trace_count` 4.
- **Score list.** By default it is 0306, 0305, 0304, 0303, 0302, 0301.
- **Metrics.** Over `[10:00, 11:00)` with `bucket=10m`:
  - `bucket_ns` is `600000000000`;
  - KPI `traces` is 5, and KPI `cost_total` is 0.00955 (compare within 1e-12);
  - the `previous` values are absent.

## G2.1 — Query errors, read helpers, worker stacks, example dataset loader

**Files:** `crates/server/src/http/error.rs`, `crates/server/src/routes/{mod.rs,read.rs}` (new `read.rs`), `crates/server/src/test_support/mod.rs`, `crates/cli/src/main.rs`.

### Steps

1. **`ApiError::Query`.** In `http/error.rs`, add:
   ```rust
   /// A2 refused or failed the query (D94). The message goes to the caller and is never logged.
   #[error("query error")]
   Query(#[source] llmobs_storage::RunError),
   ```
   `problem()` maps it:
   | `RunError` | Problem |
   |---|---|
   | `Invalid(q)` | code `InvalidFilter`, `detail: Some("invalid filter")`, `errors: [FieldError { field: "filter", message: q.message, position: q.position.map(\|p\| TextPosition { line: p.line, column: p.column }) }]` |
   | `InvalidRequest(m)` | code `BadRequest`, `detail: Some(m)` |
   | `Failed(m)` | code `QueryFailed`, `detail: Some(m)` |
   | `Timeout` | code `QueryTimeout`, `detail: Some(error.to_string())` |
   | `TooLarge` | code `QueryTooLarge`, `detail: Some(error.to_string())` |
   | `Internal(_)` | code `Internal`, `detail: Some("internal error")` |

   In `into_response`, before building the problem, match on a reference, `if let Self::Query(error) = &self`, as the existing `Self::Internal` code does. Binding by value moves the `RunError` and breaks the later `self.problem()`. Then:
   - if it is `RunError::Internal(_)`, log `tracing::error!(kind = error.kind(), "query failed")`;
   - otherwise log `tracing::info!(kind = error.kind(), "query refused")`.

   Never log the error's message or source.
2. **Conversions.**
   - `impl From<llmobs_storage::RunError> for ApiError` → `ApiError::Query(error)`.
   - `impl From<llmobs_storage::error::QueryError> for ApiError`:
     - `Busy` → `ApiError::Unavailable { detail: "too many queries are running" }`;
     - `DataFusion(e)` → `ApiError::Query(RunError::Internal(e))`.
3. **`routes/read.rs`.** Create it, `pub(crate) mod read;` in `routes/mod.rs`. Contents:
   ```rust
   //! Shared plumbing of the read handlers (D-G-14).

   /// Time allowed for one read: planning, running and score lookups each get this budget.
   pub(crate) const READ_TIMEOUT: Duration = Duration::from_secs(30);

   /// Admits a query on `project` and builds A2's context, without a window (D101).
   pub(crate) async fn open_query(state: &AppState, project: ProjectId, around: Option<i64>)
       -> Result<QueryContext, ApiError>
   // = state.storage.query(project, QueryOptions { around }).await? then QueryContext::new(source, None)?

   /// `[from, to)`. One bound alone leaves the other side open (`i64::MIN` / `i64::MAX`). Neither: `None`.
   /// `from >= to` is A2's `InvalidRequest` (400). No 30-day cap: spec 05's search limit is the
   /// UI's time picker; the server passes the window through and A2 only requires one for `search`.
   pub(crate) fn window(from: Option<TimeParam>, to: Option<TimeParam>) -> Result<Option<TimeWindow>, ApiError>

   /// The window plus the default-exclusion flags.
   pub(crate) fn list_scope(from: Option<TimeParam>, to: Option<TimeParam>, include_eval: bool, include_experiments: bool)
       -> Result<ListScope, ApiError>

   /// `Limit` as A2's `usize`. `Limit` is already 1–1000.
   pub(crate) fn page_limit(limit: Limit) -> usize   // usize::try_from(limit.get()).unwrap_or(llmobs_storage::MAX_PAGE)

   pub(crate) fn sort_order(order: SortOrder) -> llmobs_storage::SortOrder   // Asc → Asc, Desc → Desc
   ```
   `QueryOptions` is `llmobs_storage::QueryOptions`. `TimeWindow`, `ListScope` and `QueryContext` are `llmobs_storage::…`.
4. **Worker stacks.** In `crates/cli/src/main.rs`, add `.thread_stack_size(8 * 1024 * 1024)` to the `new_multi_thread()` builder, with the comment `// A2 asks for at least 8 MiB per worker: DataFusion plans recursively (A2 plan § Boundaries).`
5. **Example dataset loader.** In `test_support/mod.rs`, add to `impl TestApp`:
   ```rust
   /// Appends `fixtures/api/dataset` (spans and scores) to `project` and stores the PNGs of
   /// `fixtures/api/media`. Every row's `project_id` is set to `project`.
   pub(crate) async fn load_example_dataset(&self, project: ProjectId)
   ```
   - **Spans.** Read `concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/api/dataset/spans.json")` as `Vec<Span>`. Set each `project_id`, build the batch with `llmobs_core::batch::spans_to_record_batch`, and call `self.state.storage.append_spans(project, batch)`.
   - **Scores.** Do the same with `scores.json`, `scores_to_record_batch` and `append_scores`.
   - **Media.** For every `*.png` in `fixtures/api/media`, the file stem is the hex SHA-256. Call `put_media(project, MediaBlob { sha256: <hex decoded>, mime_type: "image/png".to_owned(), bytes: Bytes::from(file) })`.
   - `unwrap()` is fine here: this is test code.

### Tests

- In `http/error.rs`'s test module, one test per `RunError` row checks the status, `code`, `detail` and `errors`. `Invalid` checks `field == "filter"` and the position. Also test `QueryError::Busy` → 503 with `Retry-After: 1`.
- In `routes/read.rs` (`#[cfg(test)] mod tests`):
  - `window(None, None)` is `None`;
  - `from` alone is `[from, i64::MAX)`, and `to` alone is `[i64::MIN, to)`;
  - `from == to` is `Err` (400 `bad_request`).
- In `test_support` (a `#[tokio::test(flavor = "multi_thread")]` in `routes/read.rs`'s tests):
  - `seed()`, `load_example_dataset(seeded.project.id)`, then `open_query`;
  - `llmobs_storage::trace_list` with `filter: None`, `search: None`, the default scope, `TraceSort::StartTime`, `Desc`, `limit: 50`, `cursor: None` and `READ_TIMEOUT` returns 5 rows;
  - its `trace_id` column, read with `as_string::<i32>()`, is `["a1…6", "a1…4", "a1…3", "a1…2", "a1…1"]`. This checks A2 against § Dataset facts before G writes any converter.

### Done when

`just check` passes. Commit: `server: add query errors, read helpers and the example dataset loader`.

## G2.2 — `api::convert`: column access, traces, spans, scores on rows

**Files:** `crates/server/Cargo.toml` (`arrow` moves to `[dependencies]`), `crates/server/src/api/{mod.rs,convert.rs}` (new `convert.rs`).

### Steps

1. `api/mod.rs`: `pub(crate) mod convert;` (after `common`). Start `convert.rs` with `#![cfg_attr(not(test), allow(dead_code))] // handlers call these from G2.3 on`. `api/` has no such allowance, unlike `routes/`, and clippy runs with `-D warnings`.
2. **`ConvertError`:**
   ```rust
   /// A stored row does not have the shape the API expects. The text names the column, never a value.
   #[derive(Debug, thiserror::Error)]
   pub(crate) enum ConvertError {
       #[error("column {0} is missing")] Missing(&'static str),
       #[error("column {0} has an unexpected type")] Type(&'static str),
       #[error("column {0} is null")] Null(&'static str),
       #[error("column {0} has an invalid value")] Value(&'static str),
   }
   ```
   And `impl From<ConvertError> for ApiError` (in `convert.rs`) → `ApiError::Internal(Box::new(error))`.
3. **Column access** (private functions):
   ```rust
   fn column(batch: &RecordBatch, name: &'static str, data_type: &DataType) -> Result<ArrayRef, ConvertError>
   // batch.column_by_name(name) or Missing; when its type != data_type, arrow::compute::cast, failure → Type
   fn strings(batch, name) -> Result<StringArray, ConvertError>          // Utf8
   fn ints(batch, name) -> Result<Int64Array, ConvertError>              // Int64
   fn floats(batch, name) -> Result<Float64Array, ConvertError>          // Float64
   fn bools(batch, name) -> Result<BooleanArray, ConvertError>           // Boolean
   fn times(batch, name) -> Result<TimestampNanosecondArray, ConvertError> // Timestamp(Nanosecond, Some("UTC"))
   fn lists(batch, name) -> Result<ListArray, ConvertError>              // downcast only, no cast
   fn maps(batch, name) -> Result<MapArray, ConvertError>                // downcast only, no cast
   ```
   Each typed accessor downcasts the `ArrayRef` with `as_any().downcast_ref::<T>()` and clones it; a failed downcast is `Type(name)`.

   Row helpers:
   - `opt_string`, `req_string`, `opt_i64`, `req_i64`, `opt_f64`, `req_bool` and `req_time` (`TimestampNanosecondArray::value` is already `i64`). `opt_*` returns `None` on null; `req_*` returns `Null(name)`;
   - `list_strings(&ListArray, row, name) -> Result<Vec<String>, ConvertError>`;
   - `map_strings(&MapArray, row, name) -> Result<BTreeMap<String, String>, ConvertError>`;
   - `req_trace_id` and `req_span_id` (hex `Utf8` → id; a parse error is `Value(name)`), and `opt_span_id`.
4. **Converters** (`pub(crate)`):
   - `trace_summaries(batches: &[RecordBatch]) -> Result<Vec<TraceSummary>, ConvertError>`, per § Column → field tables, in row order across batches;
   - `span_summaries(batches: &[RecordBatch]) -> Result<Vec<SpanSummary>, ConvertError>`;
   - `score_briefs(batches: &[RecordBatch]) -> Result<HashMap<String, Vec<ScoreBrief>>, ConvertError>`: keyed by `target_key`, each vector in row order;
   - `score_value(data_type: &str, value_num: Option<f64>, value_str: Option<String>, ) -> Result<ScoreValue, ConvertError>`: errors name the column (`data_type`, `value_num` or `value_str`).

   Read the columns once per batch, before the row loop, not once per row.

### Tests (in `convert.rs`)

- **Full traces.** Load the dataset (as in G2.1) and run `trace_list` with `include_eval: true`. Assert:
  - six summaries, in the § Dataset facts order;
  - trace 1: `has_root`, `root_span_id == b100000000000011`, `span_count == 4`, `tags == ["beta"]`, `cost_total == Some(0.00306)` (within 1e-12), `status == Ok`, `scores` empty;
  - trace 6: `has_root == false`, `root_span_id == None`;
  - trace 3: `status == Error`, `error_count == 2`.
- **Spans.** Run `span_list` (defaults, `limit` 50): 12 summaries, the first `b100000000000061`. Span `12` has `model == Some("claude-sonnet-4-5")`.
- **Score briefs.** `latest_scores(ctx, ScoreTargets::Traces(&[t1, t2, t3]), READ_TIMEOUT)` → `score_briefs` gives:
  - `t1`: `[helpfulness: Numeric(0.9), source LlmJudge, score_id …0302]`;
  - `t2`: `[thumbs: Boolean(true)]`;
  - `t3`: `[note: Text("Needs retry with backoff.")]`.
- **`score_value`.** One case per data type, plus `numeric` with a null `value_num` → `Null("value_num")`.
- **`Utf8View`.** Take the span-list batch from the first test, cast every `Utf8` column to `Utf8View` with `arrow::compute::cast`, rebuild the batch, and check that `span_summaries` gives the same result.
- **Missing column.** A `RecordBatch` built by hand with only a `trace_id` column gives `trace_summaries` → `Err(Missing("has_root"))`, or whichever column the converter reads first. Assert `Missing(_)`.

### Done when

`just check` passes. Commit: `server: convert trace and span rows to API types`.

## G2.3 — Trace and span handlers

**Files:** `crates/server/src/routes/{mod.rs,read.rs,traces.rs}` (new `traces.rs`), `crates/server/src/api/{convert.rs,traces.rs}`, `crates/server/src/http/{access.rs,access_suite.rs}`.

### Steps

1. **`api/traces.rs`.** Make `stored_payload` `pub(crate)`. It is reused for session turns in G2.4.
2. **`convert.rs`.** Add `span(batch: &RecordBatch, project_id: ProjectId) -> Result<Span, ConvertError>`, which reads row 0 (a batch with no rows is `Null("span_id")`). Fields follow the `Span` table in § Column → field tables. Struct and list children go through `child(..)` (§ Strings may be `Utf8View`):
   - `events`: `lists(..)` → `value(row)` → `as_struct()`, then the `time`, `name` and `attributes` children by name;
   - `links`: the same, with `trace_id`/`span_id` as `FixedSizeBinaryArray` and `value(i)` → `TraceId::from_slice` / `SpanId::from_slice`. A parse error is `Value("links")`.
3. **Score attachment, in `read.rs`:**
   ```rust
   /// Fills `scores` of each trace with its latest score per name (spec 09 § Response shapes).
   pub(crate) async fn attach_trace_scores(ctx: &QueryContext, traces: &mut [TraceSummary]) -> Result<(), ApiError>
   ```
   - Collect `trace.trace_id.to_string()` for every row; return at once when there are none.
   - Call `latest_scores(ctx, ScoreTargets::Traces(&refs), READ_TIMEOUT)` and `convert::score_briefs`.
   - Set `trace.scores = briefs.remove(&id).unwrap_or_default()`.

   Every caller passes at most 1,000 rows.
4. **`routes/traces.rs`:**
   ```rust
   pub(crate) fn routes() -> Vec<RouteDef>   // four rows, below
   ```
   | Method | Path | Handler |
   |---|---|---|
   | GET | `/api/v1/projects/{project}/traces` | `list_traces` |
   | GET | `/api/v1/projects/{project}/traces/{trace_id}` | `get_trace` |
   | GET | `/api/v1/projects/{project}/traces/{trace_id}/spans/{span_id}` | `get_span` |
   | GET | `/api/v1/projects/{project}/spans` | `list_spans` |

   Every row has `access: Access::Project { perm: ProjectPerm::View, session_only: false }` and `documented: true`. Register them with `routes.extend(crate::routes::traces::routes());` in `http::access::routes()`, before the `otlp` line.

   Handlers. Each takes:
   - `State(state): State<AppState>`;
   - `Extension(access): Extension<ProjectAccess>`;
   - path structs `#[derive(Debug, Deserialize)] struct TracePath { trace_id: String }` and `struct SpanPath { trace_id: String, span_id: String }`, extracted with `error::Path`;
   - `error::Query<…Params>`: `TraceListParams`, `TraceDetailParams` or `SpanListParams`. `get_span` takes no query extractor; its operation has no query parameters.

   Each returns `Result<error::Json<…>, ApiError>`.
   - **`list_traces`:**
     - `scope = list_scope(from, to, include_eval, include_experiments)?` and `ctx = open_query(&state, id, None).await?`;
     - call `llmobs_storage::trace_list(&ctx, TraceListRequest { filter: params.filter.as_deref(), search: params.search.as_deref(), scope, sort, order: sort_order(params.order), limit: page_limit(params.limit), cursor: params.cursor.as_deref(), timeout: READ_TIMEOUT })`;
     - `sort` maps `TraceSort` variant by variant to `llmobs_storage::TraceSort`;
     - then `trace_summaries`, `attach_trace_scores`, and answer `Page { data, next_cursor: page.next_cursor }`.
   - **`list_spans`.** Same shape with `SpanListRequest`, `SpanSort` and `span_summaries`. No scores.
   - **`get_trace`:**
     - `ctx = open_query(&state, id, params.around.map(TimeParam::ns))` and `trace_detail(&ctx, &trace_id, READ_TIMEOUT)`;
     - `None` → `ApiError::NotFound`;
     - the trace is `trace_summaries(std::slice::from_ref(&data.trace))?` with its first element; an empty vector is `ApiError::NotFound`;
     - then `attach_trace_scores` on that one row;
     - `spans = build_tree(span_summaries(&data.spans)?)`;
     - answer `TraceDetail { trace, spans, truncated: data.truncated }`.
   - **`get_span`.** `ctx = open_query(&state, id, None)`, then `span_detail(&ctx, &trace_id, &span_id, READ_TIMEOUT)`. `None` → `NotFound`. Otherwise answer `SpanDetail::from(convert::span(&batch, access.project.id)?)`.
5. **`routes/mod.rs`.** Add `pub(crate) mod traces;`. Until M4 routes `/traces/stream`, a `GET …/traces/stream` matches `/traces/{trace_id}` and answers 400. That is expected; do not add a stream route.
6. **`http/access_suite.rs`.** In `concrete_path`, chain `.replace("{trace_id}", "a1000000000000000000000000000001")`, `.replace("{span_id}", "b100000000000011")`, `.replace("{session_id}", "sess-001")`, `.replace("{user_id}", "user-42")` and `.replace("{sha256}", "0000000000000000000000000000000000000000000000000000000000000000")`. Put them **before** `.replace("{user}", …)`. `"{user_id}"` does not contain `"{user}"`, but keep this order anyway so the intent is clear.

### Tests (in `routes/traces.rs`)

All on the seeded project with the dataset loaded.
- **Default list.**
  - `GET …/traces` → 200: trace ids 6, 4, 3, 2, 1, and `next_cursor` null;
  - trace 1's `scores` is `[helpfulness 0.9]`;
  - trace 2's is `[thumbs true]`.
- **Exclusions.** `?include_eval=true` → 6, 4, 3, 2, 5, 1.
- **Paging.** `?limit=2` → 6, 4 with a cursor. Following the cursors gives 3, 2, then 1 with `next_cursor` null; no trace repeats or is skipped.
- **Filter.** `?filter=name%20%3D%20'answer-question'` → only trace 3.
- **Invalid filter.**
  - The filter `name = 'needle-7f3a' )` (URL-encode it with `serde_urlencoded` or by hand) → 400, `code == "invalid_filter"`, `errors[0].field == "filter"`, and `errors[0].message` is not empty.
  - Do not assert the position here. G2.1's unit test covers the position mapping, and A2 decides when a position is known.
  - Wrap the request in `assert_logs_clean(&["needle-7f3a"], …)`: the filter text is not logged.
- **Window.** `?from=2026-01-01T10:15:00Z&to=2026-01-01T11:00:00Z` → 6 and 4. `?from=2026-01-01T11:00:00Z&to=2026-01-01T10:00:00Z` → 400 `bad_request`.
- **Sort.** `?sort=cost_total&order=asc` → 4, 2, 6, 1, 3 (null cost last).
- **Trace detail.**
  - `GET …/traces/a1…1` → 4 tree spans `11, 12, 13, 14` with depths `0, 1, 1, 1`, `detached` all false, and `truncated` false;
  - `trace.scores` is `[helpfulness 0.9]`;
  - uppercase `A1…1` gives the same answer.
- **Detached span.** `GET …/traces/a1…6` → one span, `detached` true. `trace.has_root` is false.
- **Unknown trace.** `GET …/traces/a1…9` → 404 `not_found`. `GET …/traces/zz` → 400 `bad_request`.
- **Span list.** `GET …/spans` → 12 rows. `?include_eval=true` → 14.
- **Span detail.**
  - `GET …/traces/a1…4/spans/b1…43` → 200;
  - `media_refs == ["4977…1581"]`;
  - `input` is `Payload::Messages` with a `Part::Uri`;
  - `model == Some("gpt-4o-mini")`.
- **Unknown span.** `GET …/traces/a1…4/spans/b1…99` → 404.
- **Other projects.** A second project from `add_project` has no data: its `GET …/traces` → 200 with an empty `data`.

### Done when

`just check` passes. The access suite covers the four routes. Commit: `server: serve trace and span reads`.

## G2.4 — Session and user handlers

**Files:** `crates/server/src/routes/{mod.rs,read.rs,sessions.rs,users.rs}` (two new files), `crates/server/src/api/convert.rs`, `crates/server/src/http/access.rs`.

### Steps

1. **`convert.rs`:**
   - `session_summaries(batches) -> Result<Vec<SessionSummary>, ConvertError>`;
   - `end_user_summaries(batches) -> Result<Vec<EndUserSummary>, ConvertError>`;
   - `root_payloads(batches) -> Result<HashMap<TraceId, (Option<Payload>, Option<Payload>)>, ConvertError>`:
     - read the `trace_id`, `input`, `input_format`, `output` and `output_format` columns;
     - each payload is `stored_payload(format, text)`, where `format` is `PayloadFormat::parse` of a non-null `input_format`/`output_format`;
     - the first row of a trace id wins, and later rows are ignored.
2. **`read.rs`:**
   ```rust
   /// Fills `scores` of each session with its latest score per name.
   pub(crate) async fn attach_session_scores(ctx: &QueryContext, sessions: &mut [SessionSummary]) -> Result<(), ApiError>
   ```
   It mirrors `attach_trace_scores`, with `ScoreTargets::Sessions` and the `session_id` key.
3. **`routes/sessions.rs`.** Routes `GET /api/v1/projects/{project}/sessions` (`list_sessions`) and `GET /api/v1/projects/{project}/sessions/{session_id}` (`get_session`), with access and registration as in G2.3.
   - **`list_sessions`:**
     - `SessionListRequest { filter, scope: list_scope(..)?, sort, order, limit, cursor, timeout: READ_TIMEOUT }`, with `sort` mapped from `SessionSort`;
     - then `session_summaries` and `attach_session_scores`.
   - **`get_session`:**
     - `error::Query<ExclusionParams>` (from `api::common`) and `session_detail(&ctx, &session_id, include_eval, include_experiments, READ_TIMEOUT)`;
     - `None` → `NotFound`;
     - `session`: the first `session_summaries(slice::from_ref(&data.session))` row, then `attach_session_scores`;
     - `turns`: `trace_summaries(&data.turns)` → `attach_trace_scores`;
     - each turn is `SessionTurn { trace, input, output }`, with `input`/`output` taken from `root_payloads(&data.roots)` (`None, None` when absent);
     - answer `SessionDetail { session, turns, truncated: data.truncated }`.
4. **`routes/users.rs`.** Routes `GET …/users` (`list_users`) and `GET …/users/{user_id}` (`get_user`).
   - **`list_users`:** `UserListRequest { filter, scope, sort, order, limit, cursor, timeout }` with `sort` mapped from `UserSort`, then `end_user_summaries`.
   - **`get_user`:**
     - `ExclusionParams` and `user_detail(..)`;
     - `None` → `NotFound`;
     - `user` is the first `end_user_summaries` row;
     - `sessions = session_summaries(&data.sessions)` → `attach_session_scores`;
     - answer `EndUserDetail { user, sessions }`.
5. **`routes/mod.rs`.** Add both modules. Register both route lists in `access::routes()` after `traces`.

### Tests

In `routes/sessions.rs`:
- **List.** `GET …/sessions` → `sess-002`, `sess-001`:
  - `sess-001` has `trace_count` 2, `tags == ["beta", "refund"]` and `scores == [resolved: Boolean(true)]`;
  - `sess-002` has `tags == []` and `scores == []`.
- **List with `include_eval=true`.** `sess-001` has `trace_count` 3 and `tags == ["beta", "eval", "refund"]`.
- **Detail.** `GET …/sessions/sess-001`:
  - the turns are traces 1 then 2;
  - turn 1's `input` is `Some(Payload::Text(_))` (the root's `input_format` is `text`);
  - each turn's `trace.scores` matches the trace list;
  - `truncated` is false.
- **Detail with `include_eval=true`.** Turns 1, 5, 2.
- **Unknown session.** `GET …/sessions/nope` → 404.

In `routes/users.rs`:
- **List.** `GET …/users` → `user-42` (`trace_count` 3, `session_count` 1), then `user-7` (`trace_count` 1).
- **List with `include_eval=true`.** `user-42` has `trace_count` 4.
- **Detail.** `GET …/users/user-42` → `sessions` is `[sess-001]`, with `trace_count` 2 and `scores` `[resolved]`.
- **Unknown user.** `GET …/users/nope` → 404.

### Done when

`just check` passes. Commit: `server: serve session and user reads`.

## G2.5 — Scores, schema and metrics handlers

**Files:** `crates/server/src/routes/{mod.rs,scores.rs,schema.rs,metrics.rs}` (three new files), `crates/server/src/api/{convert.rs,metrics.rs}`, `crates/server/src/http/{access.rs,mod.rs}`, `crates/server/src/serve.rs`, `crates/server/src/test_support/mod.rs`.

### Steps

1. **`convert.rs`.** `scores(batches, project_id: ProjectId) -> Result<Vec<Score>, ConvertError>`, per the `Score` table.
2. **`convert.rs` (infallible).** These two map storage types to API types variant by variant; the variant names are identical (`LatencyP95` → `LatencyP95`).
   ```rust
   pub(crate) fn metrics_response(data: llmobs_storage::MetricsData, from: i64, to: i64,
       group_by: Option<api::metrics::MetricsGroupBy>) -> MetricsResponse
   pub(crate) fn schema_response() -> SchemaResponse
   ```
   - **`metrics_response`:**
     - `KpiName` → `KpiMetric`, and `Kpi { name, value, previous }` → `Kpi { metric, value, previous }`;
     - `SeriesName` → `MetricName`, and `Series { name, group, points }` → `MetricSeries { metric, group, points }`;
     - `Point` → `MetricPoint`;
     - `ModelUsageRow` → `ModelUsage`;
     - `bucket_ns` is copied, and `group_by` is echoed.
   - **`schema_response`:**
     - `view_catalog()` → `ViewSchema`, mapping `llmobs_storage::ViewName` to `api::schema::ViewName` (`ExperimentItems` and `DatasetItems` included) and `ColumnKind` to `ColumnType`;
     - `function_catalog()` → `FunctionSchema { name, signature, description, in_filter, in_sql }`;
     - strings are `to_owned()`.
3. **`routes/scores.rs`.** `GET /api/v1/projects/{project}/scores` → `list_scores`. Build `ScoreListRequest`:
   - `target` and `source` pass through;
   - for `trace_id` and `span_id`, own the strings first (`let trace_id = params.trace_id.map(|id| id.to_string());`), then borrow them (`trace_id.as_deref()`);
   - `session_id` and `name` use `as_deref()`;
   - `window` is `window(from, to)?`;
   - `limit`, `cursor` and `timeout` are as before.

   Answer `Page<Score>` from `convert::scores(&page.batches, access.project.id)`.
4. **`routes/schema.rs`.** `GET /api/v1/projects/{project}/schema` → `get_schema`, which answers `error::Json(convert::schema_response())`. It opens no query.
5. **Metrics cache, in `routes/metrics.rs`:**
   ```rust
   /// `GET /metrics` answers, kept 10 s per (project, params) (spec 05 § Dashboard metrics).
   #[derive(Debug, Default)]
   pub(crate) struct MetricsCache { entries: Mutex<HashMap<MetricsKey, Cached>> }   // std::sync::Mutex
   #[derive(Debug, Clone, PartialEq, Eq, Hash)]
   struct MetricsKey { project: ProjectId, from: i64, to: i64, bucket: MetricsBucket,
       filter: Option<String>, group_by: Option<MetricsGroupBy>, include_eval: bool, include_experiments: bool }
   #[derive(Debug)]
   struct Cached { expires_at: i64, response: Arc<MetricsResponse> }
   const TTL_NS: i64 = 10_000_000_000;
   const MAX_ENTRIES: usize = 256;
   ```
   - **`get(&self, key, now)`.** Returns the response when `now < expires_at`.
   - **`put(&self, key, response, now)`.** When `len >= MAX_ENTRIES`, first `retain` the entries that have not expired; if still `>= MAX_ENTRIES`, `clear()`. Then insert with `expires_at = now.saturating_add(TTL_NS)`.
   - **Poisoning.** A poisoned lock is recovered with `.lock().unwrap_or_else(PoisonError::into_inner)`.
   - **`Hash`.** Add `Hash` to the derives of `MetricsBucket` and `MetricsGroupBy` in `api/metrics.rs`. This does not change `openapi.json`.
6. **State.** Add `pub(crate) metrics_cache: crate::routes::metrics::MetricsCache` to `AppInner`, with the doc `/// \`GET /metrics\` answers cached 10 s (spec 05).` Initialise it with `MetricsCache::default()` in `serve.rs` and `test_support/mod.rs`.
7. **`get_metrics`.** `GET /api/v1/projects/{project}/metrics`:
   - build the key from the params, with `now = state.clock.now_ns()`. On a cache hit, answer `(*response).clone()`;
   - otherwise:
     - `window = TimeWindow::new(params.from.ns(), params.to.ns())?`;
     - `ctx = open_query(..)`;
     - call `llmobs_storage::metrics(&ctx, MetricsRequest { window, bucket, filter: params.filter.as_deref(), group_by, include_eval, include_experiments, timeout: READ_TIMEOUT })`;
     - `bucket` is `MetricsBucket::Auto` → `Bucket::Auto`, or a width → `Bucket::Fixed(BucketWidth::…)` with the same name (`M1` → `M1`, …, `D1` → `D1`);
     - `group_by` maps to `llmobs_storage::MetricsGroupBy`;
     - convert with `metrics_response`, `put` the result, and answer it.
   - Errors are not cached.
8. **Wiring.** Register the three route lists in `access::routes()` after `users`. Every route uses `ProjectPerm::View`.

### Tests

- **`routes/scores.rs`:**
  - `GET …/scores` → ids 0306 … 0301 in order;
  - 0305 has `target == Session { session_id: "sess-001" }` and `session_id == None`;
  - 0303 has `target == Span { trace 1, span 14 }` and `value == Categorical { label: "friendly", value: Some(1.0) }`;
  - `?name=helpfulness` → 0302, 0301;
  - `?target=session` → 0305;
  - `?limit=4` then the cursor gives 4 rows, then 2, with `next_cursor` null at the end;
  - every row's `project_id` is the seeded project's id.
- **`routes/schema.rs`.** `GET …/schema` → equals `serde_json::from_str::<SchemaResponse>(include_str!("../../../../fixtures/api/static/schema.json"))`.
- **`routes/metrics.rs`:**
  - `?from=2026-01-01T10:00:00Z&to=2026-01-01T11:00:00Z&bucket=10m` → `bucket_ns == 600_000_000_000`;
  - the `kpis` are in order `traces` (5.0), `error_rate`, `latency_p95`, `cost_total` (0.00955 within 1e-12), `total_tokens`;
  - every `previous` is `None`;
  - every `series[i].points.len() == 6`.
- **Cache.**
  1. Call the same metrics URL once.
  2. Append one more trace's span with `append_spans`. Use a copy of the dataset span `b100000000000061` with these changes:
     - trace id `a1000000000000000000000000000007`;
     - `start_time` 10:40:00 and `end_time` 10:40:02;
     - `parent_span_id: None`.

     Build the batch with `spans_to_record_batch`. The same URL still answers `traces` 5.0.
  3. Advance the clock with `app.state.clock.advance(Duration::from_secs(11))`. Now it answers 6.0.
- **Missing params.** No `from` → 400 `bad_request`. `from == to` → 400.

### Done when

`just check` passes. Commit: `server: serve scores, schema and metrics`.

## G2.6 — Media and the read contract

**Files:** `crates/server/src/routes/{mod.rs,media.rs}` (new `media.rs`), `crates/server/src/api/{common.rs,openapi.rs}`, `crates/server/src/http/{access.rs,error.rs}`, `crates/server/tests/openapi.rs`, `crates/server/openapi.json`, `web/src/api/schema.gen.ts` (both regenerated).

### Steps

1. **`ProblemCode::RangeNotSatisfiable`.** Add it in `api/common.rs`, after `UnsupportedMediaType`, with the doc `/// The \`Range\` header does not overlap the blob.`. Its status is `416` and its reason phrase `"Range Not Satisfiable"`.
2. **`ApiError::RangeNotSatisfiable { size: u64 }`.**
   - `#[error("range not satisfiable")]`;
   - the problem is `Problem::new(ProblemCode::RangeNotSatisfiable, None)`;
   - `into_response` adds the header `Content-Range: bytes */{size}`.
3. **`routes/media.rs`.** `GET /api/v1/projects/{project}/media/{sha256}` → `get_media`, `ProjectPerm::View`, registered after `metrics`.
   - **Path.** `struct MediaPath { sha256: String }`. Anything other than exactly 64 characters of `[0-9a-f]` is `ApiError::NotFound`. Decode it with `hex::decode` into `[u8; 32]`.
   - **Read.** `state.storage.get_media(access.project.id, &sha, None)` reads the whole blob (D-G-24). Errors:
     - `NotFound` and `ProjectDropped` → `ApiError::NotFound`;
     - `Object(_)`, `Catalog(_)` and `Query(_)` → `ApiError::Unavailable { detail: "storage is unavailable" }`;
     - `DiskFull` → `ApiError::Unavailable { detail: "storage is out of disk space" }`;
     - every other variant → `ApiError::Internal(Box::new(MediaReadFailed))`. `MediaReadFailed` is a local unit error whose display is `"media read failed"`, so no object path or hash is logged.
   - **Range.** `parse_range(header: &str, size: u64) -> RangeAnswer`, a pure function:
     ```rust
     enum RangeAnswer { Whole, Part { start: u64, end_inclusive: u64 }, Unsatisfiable }
     ```
     Rules (RFC 9110 §14):
     - **Not a range.** The header is trimmed. One that does not start with `bytes=`, or that contains a `,`, is `Whole`. A header that is not valid UTF-8 is `Whole` too.
     - **`bytes=a-b`.** Needs `a <= b`; otherwise it is `Whole` (a malformed range is ignored). When `a >= size` it is `Unsatisfiable`. Otherwise it is `Part { start: a, end_inclusive: min(b, size - 1) }`.
     - **`bytes=a-`.** When `a >= size` it is `Unsatisfiable`. Otherwise it is `Part { a, size - 1 }`.
     - **`bytes=-n`.** `n == 0` is `Unsatisfiable`. Otherwise it is `Part { size.saturating_sub(n), size - 1 }`.
     - **Bad numbers.** A number that does not parse as `u64` makes the header `Whole`.
     - **Empty blob.** When `size == 0`, any range that parsed is `Unsatisfiable`.
   - **Response headers.** Always send:
     - `Content-Type`: the stored MIME type (`HeaderValue::from_str`; an invalid value falls back to `application/octet-stream`);
     - `X-Content-Type-Options: nosniff` and `Content-Disposition: inline`;
     - `Content-Security-Policy: sandbox`;
     - `Cache-Control: private, max-age=31536000, immutable` and `Accept-Ranges: bytes`.

     `Whole` → 200 with the bytes. `Part` → 206 with `bytes.slice(start..end_inclusive + 1)` and `Content-Range: bytes {start}-{end_inclusive}/{size}`. `Unsatisfiable` → `ApiError::RangeNotSatisfiable { size }`.
4. **Contract, in `api/openapi.rs`:**
   - add `const READ_ERRORS: &[u16] = &[400, 401, 403, 404, 422, 429, 503];`;
   - use it for `list_traces`, `get_trace`, `get_span`, `list_spans`, `list_sessions`, `get_session`, `list_users`, `get_user`, `list_scores` and `get_metrics`;
   - `get_media` errors become `&[401, 403, 404, 416, 429, 503]`;
   - `QUERY_ERRORS` is unchanged;
   - in `problem()`, add `422 => "The query took longer than its time limit or needed more memory than allowed."`, and change `503` to `"The server is busy or a dependency is unavailable. Retry later."` (it also covers the SMTP test).
5. **`tests/openapi.rs`.** `document_lint` pins each operation's set of `4xx` responses. Add
   ```rust
   /// Read operations with a handler (D-G-23): their errors include 422.
   const READ_OPERATIONS: &[&str] = &[
       "list_traces", "get_trace", "get_span", "list_spans", "list_sessions", "get_session",
       "list_users", "get_user", "list_scores", "get_metrics",
   ];
   ```
   and, just before the `QUERY_OR_BODY` branch, `else if READ_OPERATIONS.contains(id) { expected_errors.extend(["400", "401", "403", "404", "422", "429"]); }`. Leave `QUERY_OR_BODY` as it is: `stream_traces` and the saved-view operations keep `QUERY_ERRORS`. `get_media`'s branch is unchanged (503 is not `4xx`).
6. **Regenerate.** Run `export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH" && just openapi`. Commit the regenerated `crates/server/openapi.json` and `web/src/api/schema.gen.ts`. Hand-edit neither.

### Tests

- **`parse_range` (unit, `media.rs`).** One case per rule above, with `size` 70:
  - `bytes=0-9` → `Part{0,9}`; `bytes=60-100` → `Part{60,69}`; `bytes=10-` → `Part{10,69}`; `bytes=-5` → `Part{65,69}`; `bytes=-100` → `Part{0,69}`;
  - `bytes=70-` → `Unsatisfiable`; `bytes=-0` → `Unsatisfiable`;
  - `bytes=5-1` → `Whole`; `bytes=0-1,4-5` → `Whole`; `items=0-1` → `Whole`; `bytes=x-1` → `Whole`.
- **HTTP (dataset loaded).**
  - **Whole.** `GET …/media/4977…` → 200; the body equals `fixtures/api/media/<sha>.png`; `content-type` is `image/png`; all six fixed headers are present.
  - **Part.** `Range: bytes=0-9` → 206, `content-range` is `bytes 0-9/70`, and the body is the first 10 bytes.
  - **Unsatisfiable.** `Range: bytes=70-` → 416, `content-range` is `bytes */70`, and the problem `code` is `range_not_satisfiable`.
  - **Not found.** An uppercase sha → 404. An unknown sha (64 × `0`) → 404.
- **Contract.** `cargo test -p llmobs-server --test openapi` passes after `just openapi`. The diff of `openapi.json` shows only these changes:
  - the `422` and `503` responses, and `400` on `get_span`;
  - the `416` problem code;
  - the two descriptions.

### Done when

`just check` passes, the web checks included. Commit: `server: serve media and declare the read errors`.

## G2.7 — Example responses from the real handlers

**Files:**
- `crates/server/src/{lib.rs,api_examples.rs}` (new `api_examples.rs`) and `crates/server/src/test_support/mod.rs`;
- `crates/server/tests/{api_examples.rs,support/mod.rs}`;
- `justfile`;
- `fixtures/api/` (README, manifest, `responses/`, `static/metrics.json` deleted);
- `web/src/**/*.test.ts(x)`, only as step 8 allows.

### Steps

1. **`seed_examples`.** In `test_support/mod.rs`, add `pub(crate) const EXAMPLE_PROJECT: &str = "01900000-0000-7000-8000-000000000001";`. The generator parses it with `.parse::<ProjectId>()`. Only the project id is pinned; the org and user ids are random, and no generated file contains them. Then add:
   ```rust
   /// The example user, org and project of `fixtures/api/README.md` § Fixed identifiers, with
   /// the project's fixed id `01900000-0000-7000-8000-000000000001`. Returns a session cookie
   /// for `ada@example.com`. Used by `just api-examples` (D-G-25).
   pub(crate) async fn seed_examples(&self) -> String
   ```
   In one `meta.write` closure:
   - `insert_user` (`ada@example.com`, name `Some("Ada Example")`, no password, instance admin);
   - `insert_org(conn, "Acme", "acme", now)`;
   - `add_org_member(.., OrgRole::Owner, ..)`;
   - `insert_project(conn, org.id, "Support bot", "support-bot", Some(30), now)`;
   - then `sqlx::query("UPDATE projects SET id = ? WHERE id = ?").bind(EXAMPLE_PROJECT).bind(project.id.to_string()).execute(&mut *conn).await.map_err(MetaError::Db)?`. Nothing references the project yet, so the update is safe.

   Then return `session_cookie(self, user.id).await`.
2. **Order.** Write the generator (steps 1–3), including the static manifest entries copied from today's `build()`, before step 5 deletes `build()`.

   **`api_examples.rs`.** Create `crates/server/src/api_examples.rs` and declare it in `lib.rs` as `#[cfg(test)] mod api_examples;`. Module doc: `//! \`just api-examples\`: \`fixtures/api\` example responses, produced by the real read handlers over \`fixtures/api/dataset\` (D-G-17, D-G-25).` It holds:
   - `Example` (the same fields and serde as today's struct in `tests/api_examples.rs`);
   - `canonical_json` (copy it from `tests/support/mod.rs`, with its comment);
   - `api_dir()`;
   - `examples_write_requested()`, `on_disk_files()`, `collect_dir()` and `write_files()`, moved unchanged from `tests/api_examples.rs`;
   - `async fn generate() -> (BTreeMap<String, String>, Vec<Example>)`;
   - `#[tokio::test(flavor = "multi_thread")] async fn examples_are_up_to_date()`, with the same compare-or-write logic as today.
3. **`generate()`.**
   1. **Setup.** Call `TestApp::new()`, then `seed_examples()`, then `load_example_dataset(<the fixed project id>)`.
   2. **Fetching.** A helper `fetch<T: Serialize + DeserializeOwned>(app, cookie, path_and_query) -> T`:
      - sends `GET /api/v1/projects/01900000-0000-7000-8000-000000000001{path_and_query}` with the cookie;
      - asserts status 200, printing the path and the body on failure;
      - parses the body as `T`.

      Every file's text is `canonical_json(&value)`.
   3. **Responses.** Produce, in this order:
      | Operation | Request (after the project prefix) | File | Manifest `path_params` / `query` |
      |---|---|---|---|
      | `list_traces` | `/traces` | `responses/traces.json` | `{}` / `{}` |
      | `list_traces` | `/traces?include_eval=true` | `responses/traces-include-eval.json` | `{}` / `{include_eval: "true"}` |
      | `get_trace` | `/traces/{id}` for each trace of the include-eval list | `responses/traces/{id}.json` | `{trace_id}` / `{}` |
      | `get_span` | `/traces/{id}/spans/{span}` for each `spans[].span.span_id` of that detail | `responses/spans/{id}/{span}.json` | `{trace_id, span_id}` / `{}` |
      | `list_spans` | `/spans` | `responses/spans.json` | `{}` / `{}` |
      | `list_sessions` | `/sessions` | `responses/sessions.json` | `{}` / `{}` |
      | `get_session` | `/sessions/{id}` for each session of that list | `responses/sessions/{id}.json` | `{session_id}` / `{}` |
      | `list_sessions` | `/sessions?include_eval=true` | `responses/sessions-include-eval.json` | `{}` / `{include_eval: "true"}` |
      | `get_session` | `/sessions/sess-001?include_eval=true` | `responses/sessions/sess-001-include-eval.json` | `{session_id: "sess-001"}` / `{include_eval: "true"}` |
      | `list_users` | `/users` | `responses/users.json` | `{}` / `{}` |
      | `get_user` | `/users/{id}` for each user of that list | `responses/users/{id}.json` | `{user_id}` / `{}` |
      | `list_users` | `/users?include_eval=true` | `responses/users-include-eval.json` | `{}` / `{include_eval: "true"}` |
      | `get_user` | `/users/user-42?include_eval=true` | `responses/users/user-42-include-eval.json` | `{user_id: "user-42"}` / `{include_eval: "true"}` |
      | `list_scores` | `/scores` | `responses/scores.json` | `{}` / `{}` |
      | `get_metrics` | `/metrics?from=2026-01-01T10:00:00Z&to=2026-01-01T11:00:00Z&bucket=10m` | `responses/metrics.json` | `{}` / `{}` (the mocks serve it for any range; D-G-25) |

      The types are `Page<TraceSummary>`, `TraceDetail`, `SpanDetail`, `Page<SpanSummary>`, `Page<SessionSummary>`, `SessionDetail`, `Page<EndUserSummary>`, `EndUserDetail`, `Page<Score>` and `MetricsResponse`. All are `200` and `application/json`.
   4. **Checked but not written:**
      - **`get_schema`.** `/schema` parsed as `SchemaResponse` must equal `static/schema.json` parsed the same way (`assert_eq!`, message `"static/schema.json differs from GET /schema"`). Manifest: `get_schema`, `static/schema.json`.
      - **`get_media`.** The raw body of `/media/{MEDIA}` must equal the bytes of `media/{MEDIA}.png`, with `content-type` `image/png`. Manifest: `get_media`, `{sha256: MEDIA}`, `media/{MEDIA}.png`, `image/png`.
   5. **Static entries.** Copy them unchanged from today's `build()`: `list_admin_users`, `post_admin_user`, `get_admin_settings`, `get_auth_config`, `post_setup`, `post_login`, `post_signup`, `get_me`, `list_orgs`, `get_org`, `list_org_members`, `list_org_invites`, `post_org_invite`, `get_invite_preview`, `list_project_members`, `list_projects`, `get_project`, `list_api_keys`, `post_api_key`, `list_personal_tokens`, `post_personal_token` and `list_saved_views`. Their statuses and `path_params` stay as they are. `get_metrics` is no longer static.
   6. **Sort.** Sort the manifest by `(operation_id, file)`. The files map holds every `responses/…` file plus `"manifest.json"` → `canonical_json(&manifest)`.
4. **`justfile`.** The `api-examples` recipe becomes `LLMOBS_API_EXAMPLES_WRITE=1 cargo test -p llmobs-server --lib api_examples::examples_are_up_to_date`.
5. **`tests/api_examples.rs`.** Delete the hand model:
   - the functions `params`, `example_created`, `example_status`, `example`, `page`, `group_spans`, `summaries`, `build`, `session_views`, `user_views`, `turn`, `expected_files`, `on_disk_files`, `collect_dir`, `write_files`, `examples_write_requested` and the test `examples_are_up_to_date`;
   - the struct `Built`, and every import and constant that becomes unused.

   Keep `Example` (for reading the manifest) and every other test. Then:
   - in `static_files_match_their_types`, remove the `static/metrics.json` line;
   - in `generated_files_roundtrip`, add `"get_metrics" => roundtrip::<MetricsResponse>(&example.file),` and remove `"get_metrics"` from the no-op list.
6. **`tests/support/mod.rs`.**
   - Delete `live_scores`, `trace_summary`, `dataset_scores`, and every other function only the hand model used. Keep `api_dir`, `canonical_json`, `read_json` and `dataset_spans` (`dataset_is_consistent` and `every_fixture_file_is_used` use it). Before deleting anything, `grep` each helper's callers in `tests/`: keep exactly the ones a remaining test calls. Clippy runs with `--all-targets -D warnings`, so an unused helper fails the check.
   - Replace the module doc with `//! Fixture loaders shared by the example checks.`
   - Remove the unused imports.
7. **Regenerate.**
   1. `git rm fixtures/api/static/metrics.json`.
   2. Run `just api-examples`.
   3. Run `cargo test -p llmobs-server --lib api_examples` five times. Every run must pass, which shows the output is deterministic. If a value jitters between runs (for example a metrics percentile), STOP and report the file and field; do not pin settings to hide it.
   4. Run `cargo test -p llmobs-server --test api_examples`. `example_scenarios` must pass unchanged (STOP rule).
   5. Run `cargo test -p llmobs-storage`. A2's own tests read `fixtures/api/responses` as the expected answers (`reads/*.rs`, `views/mod.rs`, `filter/mod.rs`). STOP if any of them fails: G must not edit `crates/storage`.
   6. Run `git diff --stat fixtures/api` and put the list in the report. For each changed file, describe what changed in one line, for example "cost_total of trace 1 is now 0.00306 (exact sum, D-A2-4)".
8. **Web.** Run `export PATH=… && just check-web`. Change a web test only when it asserts a value that comes from a fixture file and that value changed in step 7. For example, the dashboard test's `"Cost: $0.00955"` would become the new KPI value. Rules for those edits:
   - update the expected value to the new one;
   - never delete a test or an assertion;
   - never change non-test web code;
   - list every edited assertion, with its old and new values, in the report.

   If a web test fails for another reason, STOP and report.
9. **`fixtures/api/README.md`:**
   - **Layout table.** `responses/` and `manifest.json` are now written by `just api-examples`, "the real read handlers over `dataset/`". Remove the `static/metrics.json` mention.
   - **New paragraph.**
     - `get_schema` is checked against `static/schema.json`, which A2's catalog test also reads.
     - `get_metrics` is generated over `[2026-01-01T10:00:00Z, 11:00:00Z)` with `bucket=10m`. Its manifest query is empty, so the mocks serve it for any range.
   - **Last paragraph.** Replace "Hand-written JSON has to deserialize…" with the checks that remain, which `tests/api_examples.rs` runs: round-trip, the OpenAPI schemas, manifest coverage, dataset consistency and the scenario assertions.

### Done when

`just check` passes. `just api-examples` leaves the tree clean (`git status --short` is empty after a second run). Commit: `server: generate the API examples from the real read handlers`.

## G2.8 — Docs and roadmap

**Files:** `docs/plans/g-spine/00-design.md`, `docs/roadmap.md`.

### Steps

1. **`00-design.md`.** In the **Status** line, "M1 implemented (PR #43)" becomes "M1 implemented (PR #43), M2 implemented (PR #<this PR>)".
2. **`docs/roadmap.md`, G row.** The status becomes "**In progress.** M1 and M2 implemented." The plan cell already says "M1, M2 written". Leave the other rows alone.
3. **Report.** List the deviations from this plan, collected from every sub-phase report, in the PR description, not in a doc.

### Done when

`just check` passes. Commit: `docs: mark G M2 implemented`.

## After M2

- **M3:** score writes, gRPC, usage counters. **M4:** the live bus and SSE. M4 reuses `routes/read.rs` and `convert::trace_summaries` for the trace-stream re-reads (D-G-13).
- **Open items** (also in the design's § Open items):
  - the media whole-read (D-G-24) costs up to `media.max_bytes` per range request. A1 is asked for a size-only lookup;
  - `api_examples.rs` still lists the static examples of D's endpoints. They become generated when their owners touch them.
