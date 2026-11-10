# A2 · 01 — Context and views (Q1–Q5)

Read [README.md](README.md) first: decisions D91–D101, boundaries, verified DataFusion facts, crate layout, public API.

---

## Q1 — Spec and decisions

**Goal:** the specs and the decision log describe A2 before any code is written.

**Precondition:** the A1 design (decisions D83–D90, spec 03 § 6 rewritten for D89) is on `main`. If it is not, stop and report. If decision numbers D91+ are taken by then, renumber the decisions below and every `D9x`/`D10x` reference in `docs/plans/a2-query/`, and say so in the report.

**Edit only:** `docs/mvp-decisions.md`, `docs/spec/05-query.md`, `docs/spec/09-api.md`, `docs/spec/11-ui.md` (§ SQL console only), `docs/spec/03-storage.md` (§ 6, first paragraph only), `docs/plans/a2-query/*` (renumbering only).

**Commit:** `Specify the query layer (A2)`

### 1. `docs/mvp-decisions.md`

Append D91–D101 from the README's decision table, in the log's style: `| D91 | Query context | … |`, one sentence or a short list per cell, and the spec section each decision lives in, in parentheses.

### 2. `docs/spec/05-query.md`

Keep the existing structure. Change or add the following.

**Intro.** After the sentence about D89, add: "`QueryContext::new` (in `llmobs-storage`) builds the DataFusion session from the `ProjectQuery` that `Storage::query` returns (D91): the raw providers, the per-query runtime (memory pool, spill directory) and `target_partitions`."

**§ Logical views.** Add a column-rules paragraph under the table:
- Each view exposes the stored columns of its table in spec 01 order, without `project_id`. `traces` also drops `last_ingest_time` and `partial_count`.
- Binary IDs (`trace_id`, `span_id`, `parent_span_id`, `root_span_id`) are lowercase hex `Utf8`.
- `duration_ms` follows `duration_ns`:
  - on `spans`, `duration_ns` is stored;
  - on `traces` and `sessions`, `duration_ns = end_time − start_time` in nanoseconds, placed after `end_time`.
- `events` and `links` keep their native list-of-struct types. They are listed as `json` in the schema endpoint because clients receive them as JSON.
- `sessions` columns, in order: `session_id, start_time, end_time, duration_ns, duration_ms, trace_count, error_count, total_tokens, cost_total, user_id, tags`.
  - `user_id` is the first non-null `user_id` of the session's traces ordered by `(start_time, trace_id)`.
  - `tags` is the sorted distinct union of the traces' tags.
  - Replace the SQL sketch's `first_value(user_id)` accordingly.

**Hex literals.** Replace "equality filters on hex literals are rewritten…" with a short paragraph:
- An optimizer rule rewrites `id = '<hex>'`, `id <> '<hex>'`, `id IN ('<hex>', …)` and `id NOT IN (…)` on these columns into comparisons on the stored bytes, so the providers can prune with bloom filters.
- The literal must be exactly twice the ID's byte width and contain only hex digits, in either case. Otherwise the comparison stays on the hex string, which never matches an invalid ID.
- A comparison between two ID columns of the same plan node compares bytes.
- Joins between two views on an ID compare the hex strings: correct, but not pruned.

**§ Default exclusions.**
- "The filter explicitly constrains `environment`" means the filter text references the `environment` column anywhere. Then the `llmobs-eval` exclusion is not added; the experiment exclusion still is.
- The exclusions apply to the view the request reads:
  - `traces` and `spans` directly;
  - session and user lists through the traces they aggregate;
  - span-based and score-based metrics through a semi-join on the excluded traces.

**§ Filter DSL › Grammar.** Make the grammar exact:
- **Identifiers:** bare column names of the target view, case-insensitive unless double-quoted. No qualified names.
- **Map access:** `metadata['k']`, `attributes['k']`, `resource_attributes['k']`, with a string literal key, on the views that have the column.
- **Operators:** add `IS [NOT] DISTINCT FROM` and unary `-`. `@>` is planned by DataFusion as `array_has_all`.
- **Casts:** `CAST(x AS t)` and `x::t` with `t` ∈ `BIGINT, DOUBLE, VARCHAR, BOOLEAN, TIMESTAMP`, compiled to `TRY_CAST` (D100).
- **Literals:** add `NULL`.
- **Functions:** the allow-list as today, called without `DISTINCT`, `FILTER`, `OVER`, `ORDER BY` or named arguments.
- **Score functions:** `score`, `score_label`, `has_score` on `traces`, `spans` and `sessions` only, with the semantics of D97 (a single string-literal argument). At most 8 distinct score names per filter.
- **Limits:** 4 KiB and 128 operator tokens (D95). Operator tokens are listed in Q6 of the A2 plan; copy the list here.
- **Errors:** 400 `invalid_filter` with a 1-based character position when one is known.

**§ Full-text search.** State the escaping exactly:
- `\` becomes `\\`, `%` becomes `\%`, `_` becomes `\_`;
- the pattern is `%<escaped>%`, with `ILIKE … ESCAPE '\'`;
- an empty or all-whitespace search adds nothing.

**§ SQL console › Sandbox.** Replace the list with:
1. Limits: 64 KiB and 512 operator tokens; parser recursion limit 50 (D95).
2. Exactly one statement: a query (`SELECT`, `WITH`, `UNION`/`INTERSECT`/`EXCEPT`, `VALUES`), or `EXPLAIN` of a query without `ANALYZE` or `VERBOSE`.
3. Table references are unqualified names of the registered views (`spans`, `traces`, `sessions`, `scores`, `experiment_items`, `dataset_items`) or of CTEs in scope. No table functions, no `information_schema`, no URL tables, no recursive CTEs.
4. Functions: DataFusion's built-in scalar, nested, aggregate and window functions. No table or higher-order functions. Score functions are DSL-only (D97).
5. The planned statement is checked again with DataFusion's `SQLOptions` (no DDL, DML or statements).
6. Session config: per-query runtime from storage (memory cap), `batch_size = 8192`, `target_partitions` from storage. Timeout: `tokio::time::timeout` around planning and execution; dropping the stream cancels it.
7. Time window (D101): an optional `[from, to)` written into the `spans` and `traces` views (on `start_time`) and the `scores` view (on `created_at`); `sessions` follows from `traces`; the SQLite views are not windowed. Every scan of a view carries it, in joins, subqueries and CTEs too. Without a window the query reads all history.
8. Results: D98.

**§ SQL console › Parameters.** The five types and value rules of D99.
- A placeholder may appear several times and binds one value.
- Placeholders are only recognized outside string literals and comments.
- Writing `$1` directly is rejected (400).

**New § Errors** (end of the file), with the codes of D94:
- `invalid_filter` (400) for filter and SQL text, with `position`;
- `query_failed` (400) for execution errors, with DataFusion's message capped at 2 KiB;
- `query_timeout` (422);
- `query_too_large` (422);
- `busy` (503).

Messages go to the caller and are never logged.

**§ Dashboard metrics.** Add:
- `bucket=auto` picks the smallest width in `1m, 5m, 10m, 15m, 30m, 1h, 6h, 12h, 1d` giving at most 120 buckets over `[from, to)`, else `1d`.
- Buckets are aligned to their width from the Unix epoch, UTC: a 10-minute bucket starts at :00, :10, …; a 1-day bucket at midnight UTC. The first bucket starts at `from` rounded down to the width, so it may cover part of its range only; the last one is the bucket containing `to − 1 ns`.
- An explicit bucket giving more than 2,000 buckets is a 400.
- Points cover every bucket. A bucket with no rows has no value (count series included).
- The filter is a `traces`-view DSL expression. Span series keep spans whose trace passes it, and score series keep scores whose trace passes it, through a semi-join on `trace_id` over traces with `start_time ∈ [from − 1 day, to)` and `end_time ≥ from`.
- Session-target scores are not in the score series.
- With `group_by`, rows whose group value is null are left out of the split series.
- KPIs cover `[from, to)` and the previous range of the same length; every KPI value is absent when its range has no traces.
- `top_models`: `llm` spans with a non-null model, at most 10, by `cost_total` descending (nulls last), then model ascending.

**§ Trace and span APIs.** Add a line: these queries are `llmobs_storage::reads` (D92). List the sort tie-breakers:
- traces: `trace_id`;
- spans: `(span_id, trace_id)`;
- sessions: `session_id`;
- users: `user_id`;
- scores: `score_id`, with lists sorted by `created_at`.

Pagination is keyset on `(sort value, tie-breaker)`, nulls last in both directions.

### 3. `docs/spec/09-api.md`

- **§ Conventions › Errors:** add the codes `query_failed`, `query_timeout`, `query_too_large`, `busy` to the code list.
- **§ Observability data, `POST /sql` row.** Request `{query, params?, from?, to?, limit?, timeout_ms?}`:
  - `from`/`to`: RFC 3339, both or neither, `from < to`; the time window of D101;
  - `limit` 1–10,000, default 10,000;
  - `timeout_ms` 1,000–60,000, default 60,000.

  JSON response per D98. Exports stream up to 10M rows.
- **MCP `query_sql`:** add `from?, to?` to its arguments, with the same rules.
- **`GET /schema`:** functions carry `in_filter` and `in_sql`.

### 4. `docs/spec/11-ui.md` § SQL console

Add: the console has the shared time range picker, and every run and export sends its range as `from`/`to` (D101). The range is part of the URL state and of a saved query's run, like the other views.

### 5. `docs/spec/03-storage.md` § 6

In the first paragraph, replace the sentence saying storage builds a `SessionContext` per request with: "For each request, `Storage::query` returns a `ProjectQuery`: the project's `TableProvider`s, all bound to one `ReadState` (catalog, memtable and tombstones, §4), plus a per-query `RuntimeEnv` (memory pool, spill directory) and `target_partitions`. The query layer (05, D91) builds the DataFusion session from it."

Leave the rest of § 6 to A1.

### Checks

`just check` (docs only, but the plan files' renumbering must leave no dangling reference: `grep -rn "D9[0-9]\|D10[0-9]" docs/` and read the hits).

### Report

The decision numbers used. Any spec sentence you could not reconcile with A1's text.

---

## Q2 — Sandboxed context and base views

**Goal:** `QueryContext::new(ProjectQuery, Option<TimeWindow>)` builds the sandboxed session and registers the `spans`, `traces` and `scores` views over the raw providers, with the time window of D101 when one is given. Tests run on the example project.

**Edit only:**
- `crates/storage/Cargo.toml`;
- the root `Cargo.toml` (new workspace dependencies only);
- `crates/storage/src/lib.rs`;
- new: `crates/storage/src/query_error.rs`, `src/views/{mod.rs,session.rs,spans.rs,traces.rs,scores.rs}`, `src/test_support.rs`.

**Commit:** `Add the query context and base views`

### Dependencies

`crates/storage/Cargo.toml`:
- `[dependencies]`: `datafusion = { workspace = true }`, `thiserror = { workspace = true }`.
- `[dev-dependencies]`: `tokio = { workspace = true }`, `serde_json = { workspace = true }`.

If A1 has already added any of them, keep one entry.

### `ProjectQuery` (`views/mod.rs`)

Exactly the README's struct. If `ProjectQuery` already exists in the crate (A1 M1 landed first), use that one: add any missing field, remove nothing, and say so in the report. Document each field. `Debug` is derived manually if `RuntimeEnv` does not implement `Debug`: print the field names and `target_partitions` only.

### `RunError` (`query_error.rs`)

```rust
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// A request parameter outside the query text is invalid. Mapped to 400 `bad_request` by the server.
    #[error("{0}")]
    InvalidRequest(String),
    /// DataFusion failed on something that is not the caller's fault. Logged by kind only.
    #[error("internal query error")]
    Internal(#[source] DataFusionError),
}
impl RunError { pub fn kind(&self) -> &'static str }   // "invalid_request", "internal"
```

Later sub-phases add variants (Q6 `Invalid`, Q12 `Failed`, `Timeout`, `TooLarge`), keeping the README's order. The `Display` text never includes the source's text.

### `TimeWindow` (`views/mod.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeWindow { from: i64, to: i64 }      // ns since epoch, UTC; from inclusive, to exclusive
impl TimeWindow {
    pub fn new(from: i64, to: i64) -> Result<TimeWindow, RunError>;   // from < to, else InvalidRequest("from must be before to")
    pub fn from(&self) -> i64;
    pub fn to(&self) -> i64;
    pub(crate) fn predicate(&self, column: &str) -> Expr;   // column >= from AND column < to
}
```

`predicate` uses literals `ScalarValue::TimestampNanosecond(Some(v), Some("UTC".into()))`, so the provider receives typed timestamp bounds.

### Session (`views/session.rs`)

`pub(crate) fn session_state(runtime: Arc<RuntimeEnv>, target_partitions: NonZeroUsize) -> SessionState`:
- `SessionConfig::new()`, then:
  - `.with_target_partitions(target_partitions.get())`;
  - `.with_batch_size(8192)`;
  - `.with_information_schema(false)`;
- on `options_mut()`:
  - `sql_parser.collect_spans = true`;
  - `sql_parser.recursion_limit` = 50 (a `const PARSER_RECURSION_LIMIT: usize = 50`, used again by Q6);
  - `execution.enable_recursive_ctes = false`;
- `SessionStateBuilder::new()`, with:
  - `.with_config(config)`;
  - `.with_runtime_env(runtime)`;
  - `.with_scalar_functions(SessionStateDefaults::default_scalar_functions())`;
  - `.with_aggregate_functions(SessionStateDefaults::default_aggregate_functions())`;
  - `.with_window_functions(SessionStateDefaults::default_window_functions())`;
  - `.with_expr_planners(SessionStateDefaults::default_expr_planners())`;
  - `.build()`.

No table functions, no higher-order functions, no file formats, no table factories, no `with_default_features()`. A comment above the builder lists what is left out and why (D91).

### Views

`pub(crate) fn spans_view(raw: Arc<dyn TableProvider>, window: Option<TimeWindow>) -> Result<LogicalPlan, DataFusionError>`, and likewise `traces_view` and `scores_view`:
- start from `LogicalPlanBuilder::scan("raw_spans", provider_as_source(raw), None)` (`raw_traces`, `raw_scores`);
- with a window, add `filter(window.predicate(<column>))` on the raw scan: `start_time` for `spans` and `traces`, `created_at` for `scores` (D101);
- end with a `project(...)` and `build()`.

The raw names are never registered, so SQL cannot name them.

Projection rules (spec 05 as updated by Q1; names from `llmobs_core::schema::{spans, traces, scores}`):

| View | Columns |
|---|---|
| `spans` | Every stored column but `project_id`, in stored order. `trace_id`, `span_id`, `parent_span_id` become `encode(col, 'hex') AS col`. `duration_ms` = `CAST(duration_ns AS DOUBLE) / 1e6` right after `duration_ns`. |
| `traces` | Every stored column but `project_id`, `last_ingest_time`, `partial_count`, in stored order. `trace_id` and `root_span_id` are hex. After `end_time`: `duration_ns` = `CAST(end_time AS BIGINT) − CAST(start_time AS BIGINT)`, then `duration_ms` as above. |
| `scores` | Every stored column but `project_id`. `trace_id` and `span_id` are hex. |

Write each projection as a list driven by the core schema, so a new stored column appears in the view without code changes. Iterate `spans_schema().fields()`, skip the excluded names, map the ID names, and insert the computed columns after their anchor.

`QueryContext::new(source, window)`:
- builds the state;
- creates `SessionContext::new_with_state`;
- registers the three views, each built with `window`, with `register_table(name, Arc::new(ViewTable::new(plan, None)))`.

The names come from `ViewName::as_str()` (`"spans"`, `"traces"`, `"sessions"`, `"scores"`, `"experiment_items"`, `"dataset_items"`; Q3 registers `sessions`). Errors map to `RunError::Internal`.

`QueryContext` keeps the `SessionContext` private. Give it `pub(crate) fn session(&self) -> &SessionContext` for the other A2 modules.

`ViewName` is `Copy`, has `as_str`, and is public. `SqliteView` and `register_sqlite_view` come in Q10, where the sandbox uses them.

### Test support (`test_support.rs`, `#[cfg(test)]`)

- `pub(crate) fn dataset_spans() -> Vec<Span>` and `dataset_scores() -> Vec<Score>`: read `fixtures/api/dataset/{spans,scores}.json` (path from `env!("CARGO_MANIFEST_DIR")`).
- `pub(crate) fn spans_table(spans: &[Span]) -> Arc<MemTable>`: `llmobs_core::batch::spans_to_record_batch`.
- `pub(crate) fn live_scores_table(scores: &[Score]) -> Arc<MemTable>`. What A1's `ScoresProvider` returns:
  - last writer wins per `score_id` by `(updated_at, ingest_time)`;
  - deleted scores dropped;
  - then `scores_to_record_batch`.
- `pub(crate) fn traces_table(spans: &[Span]) -> Arc<MemTable>`: one merged row per trace, built with arrow builders so that `batch.schema() == traces_schema()`. Merge rules: spec 01 § Trace, with the fixed ordering of spec 01:
  - root = the parentless span with the smallest `(start_time, span_id)`;
  - "first non-null" = root first, then by `(start_time, span_id)`;
  - previews from the root, else the first `llm` span;
  - `tags` and `models` sorted distinct (`models` = `coalesce(response_model, request_model)`);
  - `metadata` = root's map plus other spans' missing keys;
  - token and cost sums are null when every input is null;
  - `last_ingest_time` = max `ingest_time`; `partial_count` = 1.

  This mirrors `crates/server/tests/support/mod.rs::trace_summary`. Read it, don't copy across crates.
- `pub(crate) fn example_query() -> ProjectQuery`: the three tables above from the dataset, `Arc::new(RuntimeEnv::default())`, `target_partitions` 2. Tests build their context with `QueryContext::new(example_query(), None)` unless they test the window.
- `pub(crate) async fn rows(ctx: &QueryContext, sql: &str) -> Vec<RecordBatch>`: runs trusted test SQL through `ctx.session().sql(sql)`.
- `pub(crate) struct RecordingTable`: wraps a `MemTable`, answers `Inexact` to every pushed filter, and records each filter's `Display` string in an `Arc<Mutex<Vec<String>>>`. Used by Q4 and later pushdown tests.

### Tests (`views/mod.rs` and `views/session.rs` test modules)

1. `traces_table` agrees with the fixture.
   - For each trace in `fixtures/api/responses/traces-include-eval.json`, the merged row has the same `start_time`, `end_time`, `name`, `status`, `span_count`, `error_count`, `total_tokens`, `cost_total`, `tags` and `models`.
   - Compare through `serde_json::Value`; timestamps are RFC 3339 in the fixture.
2. Each view's column names and order equal spec 05 as updated by Q1. Assert against literal lists in the test.
3. `SELECT trace_id, root_span_id, duration_ns, duration_ms FROM traces WHERE name = '<trace 1 root name>'` returns the hex IDs from the fixture, `duration_ns` = end − start, and `duration_ms` = that / 1e6.
4. `SELECT span_id, parent_span_id FROM spans` gives 16-character lowercase hex. Null parents stay null.
5. `SELECT count(*) FROM scores` equals the number of live dataset scores.
6. The sandbox:
   - `SELECT * FROM generate_series(1, 3)` and `range(3)` fail with "not found";
   - `SELECT * FROM information_schema.tables` fails;
   - `SHOW TABLES` fails;
   - `WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 3) SELECT * FROM r` fails;
   - `SELECT * FROM raw_spans` fails;
   - `SELECT * FROM '/etc/hostname'` fails.

   These go through `ctx.session().sql`, so they test the session itself, not Q10's validator.
7. The registry keeps what users need. Each of these plans:
   - `regexp_like`, `encode`, `date_bin`, `date_trunc`, `array_has_all`, `approx_percentile_cont`, `first_value`, `row_number`;
   - a `metadata['k']` access;
   - an array literal `['a']`.
8. The time window (D101), with a window `[10:05, 10:20)` on 2026-01-01:
   - `SELECT trace_id FROM traces` returns exactly the traces starting in it; the trace starting at 10:20 is not there;
   - `SELECT count(*) FROM spans` counts the spans starting in it;
   - `SELECT count(*) FROM scores` counts the live scores created in it;
   - with `RecordingTable` providers, `SELECT count(*) FROM spans a JOIN spans b ON a.span_id = b.span_id`, `SELECT count(*) FROM traces WHERE trace_id IN (SELECT trace_id FROM spans)` and `WITH x AS (SELECT * FROM spans) SELECT count(*) FROM x` each record the two timestamp bounds in **every** scan;
   - `TimeWindow::new(5, 5)` and `TimeWindow::new(6, 5)` fail with `InvalidRequest`.

### Report

The exact form of each projection (expressions, ordering). Any DataFusion API name that differed from this plan.

---

## Q3 — `sessions` view

**Goal:** the `sessions` view and the reusable "sessions from traces" plan.

**Edit only:** `crates/storage/src/views/{mod.rs,sessions.rs}`.

**Commit:** `Add the sessions view`

### Behaviour

`pub(crate) fn sessions_from_traces(traces: LogicalPlan) -> Result<LogicalPlan, DataFusionError>`:
- The input has the `traces` view's columns.
- Filter `session_id IS NOT NULL`.
- Aggregate by `session_id`:
  - `min(start_time) AS start_time`, `max(end_time) AS end_time`;
  - `count(*) AS trace_count` (Int64);
  - `sum(error_count) AS error_count`;
  - `sum(total_tokens) AS total_tokens`, `sum(cost_total) AS cost_total`;
  - `first_value(user_id ORDER BY start_time, trace_id)` with nulls ignored (`ExprFunctionExt::null_treatment(NullTreatment::IgnoreNulls)`), `AS user_id`;
  - `array_sort(array_distinct(flatten(array_agg(tags)))) AS tags`.
- Project to `session_id, start_time, end_time, duration_ns, duration_ms, trace_count, error_count, total_tokens, cost_total, user_id, tags`, with `duration_ns` and `duration_ms` as in the `traces` view.

`error_count` must stay non-null Int64: cast or coalesce as needed, and assert the output type in a test. `tags` must be `List(Utf8)` non-null; an empty union is `[]`.

`QueryContext::new` registers `sessions` as a `ViewTable` over `sessions_from_traces(<traces view plan>)`.

### Tests

1. Every column of `SELECT * FROM sessions ORDER BY session_id` equals `fixtures/api/responses/sessions-include-eval.json` (`data`), field by field. Compare times as RFC 3339; `scores` is not a view column.
2. A small hand-built traces `MemTable` with one session over three traces checks the ordering rules:
   - the first trace by `(start_time, trace_id)` has a null `user_id` and the second has `u2`, so `user_id` = `u2`;
   - tags `["b","a"]`, `[]`, `["a"]` give `["a","b"]`;
   - `total_tokens` is null when all three are null.
3. Traces without `session_id` produce no session row.
4. Output schema: types and nullability as in the column table of spec 05 (`error_count` and `trace_count` non-null).

### Report

How nulls are ignored in `first_value`, and whether `error_count` needed a cast.

---

## Q4 — Hex-ID literal rule

**Goal:** comparisons between view ID columns and hex literals reach the providers as byte comparisons.

**Edit only:** `crates/storage/src/views/{hex.rs,session.rs,mod.rs}`, `crates/storage/src/test_support.rs` (only if `RecordingTable` needs a fix).

**Commit:** `Push hex ID literals down as bytes`

### Behaviour

`#[derive(Debug)] pub(crate) struct HexIdRule;` implements `datafusion::optimizer::OptimizerRule`:
- `name()` is `"hex_id_literals"`;
- `apply_order()` is `Some(ApplyOrder::BottomUp)`;
- `supports_rewrite()` is `true`.

`session_state` adds it with `.with_optimizer_rule(Arc::new(HexIdRule))`.

`rewrite` maps every expression of the node with `LogicalPlan::map_expressions` and `transform_up`. Types come from the schema the node's expressions are evaluated against:
- for `TableScan`, the qualified source schema;
- otherwise the merge of the inputs' schemas.

Only `encode(<expr>, 'hex')` where `<expr>`'s type is `FixedSizeBinary(n)` counts as an ID. Call it `E(c)`. Rewrites:

| Pattern (either operand order) | Becomes |
|---|---|
| `E(c) = 'lit'`, `E(c) <> 'lit'` | `c = X'…'` / `c <> X'…'` (`ScalarValue::FixedSizeBinary(n, Some(bytes))`) |
| `E(c) IN ('lit', …)`, `NOT IN` | `c IN (X'…', …)` / `NOT IN`, only if **every** literal is valid |
| `E(a) = E(b)`, `E(a) <> E(b)` with the same `n` | `a = b` / `a <> b` |

- A literal is valid when it is a non-null `Utf8`, `Utf8View` or `LargeUtf8` of exactly `2n` ASCII hex digits, in either case.
- Anything else is left unchanged: it never matches a valid ID, so results stay right.
- Decode with the `hex` crate (already a workspace dependency; add it to `storage` with `{ workspace = true }`).
- No `unwrap`.

Add a module comment: joins between two views on IDs are not rewritten (each side's `encode` sits in its own projection), so they compare hex strings.

### Tests (using `RecordingTable` from `test_support` as the raw providers)

1. `SELECT name FROM spans WHERE trace_id = '<trace 1 hex>'`: the provider receives a filter whose `Display` starts with `trace_id = FixedSizeBinary(16`, and the rows are trace 1's spans.
2. The same with the hex in uppercase: the same filter, the same rows.
3. `trace_id IN (<5 valid hex>)` on `traces`: the provider receives a byte-based `IN` or `OR`, with no `encode(` in it. The rows are those traces.
4. `span_id = '<16 hex>'` and `parent_span_id = '<16 hex>'` on `spans`, and `root_span_id = …` on `traces`: rewritten.
5. `trace_id = 'abc'` (too short), `trace_id = 'zz…'` (32 non-hex) and a 16-hex literal against `trace_id`: not rewritten (`encode(` still in the received filter), zero rows, no error.
6. `trace_id IN ('<valid>', 'nope')`: not rewritten; returns the one valid trace's row.
7. `trace_id <> '<hex>'`: rewritten; returns every other trace.
8. `SELECT count(*) FROM spans s JOIN scores c ON s.trace_id = c.trace_id`: correct count (hex join).
9. A subquery: `SELECT name FROM traces WHERE trace_id IN (SELECT trace_id FROM scores WHERE trace_id = '<trace 1 hex>')` returns trace 1. The provider of `scores` receives the byte filter.

### Report

Whether subqueries were reached by the rule without extra work. The received filter strings for tests 1 and 3.

---

## Q5 — View catalog

**Goal:** the column and function catalog behind `GET /schema`, matching the example project's `static/schema.json`.

**Edit only:** `crates/storage/src/views/{catalog.rs,mod.rs}`, `crates/storage/src/lib.rs`.

**Commit:** `Add the view catalog`

### Behaviour

```rust
pub enum ColumnKind { String, Integer, Float, Boolean, Timestamp, StringList, StringMap, Json }
pub struct ColumnInfo { pub name: &'static str, pub kind: ColumnKind, pub nullable: bool, pub description: &'static str }
pub struct ViewColumns { pub view: ViewName, pub description: &'static str, pub columns: Vec<ColumnInfo> }
pub struct FunctionInfo { pub name: &'static str, pub signature: &'static str, pub description: &'static str, pub in_filter: bool, pub in_sql: bool }
pub fn view_catalog() -> Vec<ViewColumns>;     // traces, spans, sessions, scores, in that order
pub fn function_catalog() -> Vec<FunctionInfo>;
```

- `ColumnKind` mirrors the server's `ColumnType`, which maps it one to one.
- Descriptions and view descriptions: copy them from `fixtures/api/static/schema.json`, which already has the wording, so the server serves the same text.
- `function_catalog` lists the DSL allow-list in spec 05 order, with the fixture's signatures and descriptions:
  - `in_filter: true` for all of them;
  - `in_sql: true` for all but `score`, `score_label` and `has_score` (D97).
- `experiment_items` and `dataset_items` are not in `view_catalog` (D93).

Kinds:
- `Utf8` → `String`;
- integers → `Integer`;
- `Float64` → `Float`;
- `Boolean` → `Boolean`;
- `Timestamp` → `Timestamp`;
- `List(Utf8)` → `StringList`;
- `Map` → `StringMap`;
- `List(Struct)` → `Json`;
- `request_params`, `system_instructions`, `tool_definitions` → `Json`.

Nullability is taken from the stored schema for stored columns. Computed columns are non-null. `sessions` follows its column list in spec 05.

### Tests

1. The `view_catalog()` names, order, kinds, nullability and descriptions equal `static/schema.json` → `views`, view by view. Map `ColumnKind` to the fixture's snake_case strings in the test.
2. Each catalog view's column names equal the registered view's schema field names, in order (`QueryContext` over `example_query()`), so the catalog cannot drift from the views.
3. For every column, the kind agrees with the view's Arrow type under the mapping above.
4. `function_catalog()` matches the fixture's `functions` (name, signature, description, `in_filter`), and has `in_sql == false` exactly for the three score functions.

### Report

Any fixture description that did not fit a column, and what you did.
