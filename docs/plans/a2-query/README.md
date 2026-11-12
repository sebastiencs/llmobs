# A2 · Query layer — plan

Implementation plan for track A2 (Wave 1). It covers:
- the user-facing views over A1's providers;
- the filter DSL;
- the SQL console sandbox;
- the queries behind the read endpoints (lists, details, scores, metrics).

All of it lives in `llmobs-storage` and is tested on in-memory tables, so it does not wait for A1.

The work is split into 20 sub-phases, Q1–Q20. Each is done by one agent, in order, and each ends with `just check` green and one commit. A sub-phase is self-contained: it lists the files it may edit, the exact behaviour, the tests and the commit subject. Do the sub-phase you were given, nothing more.

**Status:** decisions settled with the user (2026-11-10). Nothing is implemented.

**Read first** (every sub-phase):
- `AGENTS.md`;
- this README (decisions, boundaries, verified facts, public API);
- `docs/spec/05-query.md`;
- `docs/spec/01-data-model.md` (columns, trace merge rules);
- `docs/spec/09-api.md` § Conventions and § Response shapes;
- the sub-phase's own file.

| File | Sub-phases |
|---|---|
| [01-context-and-views.md](01-context-and-views.md) | Q1 spec and decisions · Q2 sandboxed context and base views · Q3 `sessions` view · Q4 hex-ID rule · Q5 view catalog |
| [02-filter-dsl.md](02-filter-dsl.md) | Q6 query text (limits, tokens, positions) · Q7 filter validation and compilation · Q8 score functions · Q9 exclusions, time window, search |
| [03-sql-console.md](03-sql-console.md) | Q10 statement validation · Q11 parameters · Q12 execution and JSON results · Q13 exports |
| [04-reads.md](04-reads.md) | Q14 cursors and the trace list · Q15 span list and trace/span detail · Q16 sessions and users · Q17 score list and row scores |
| [05-metrics-and-contract.md](05-metrics-and-contract.md) | Q18 metrics: buckets and trace series · Q19 metrics: span and score series, KPIs, top models · Q20 API contract additions |

## Decisions

These go into `docs/mvp-decisions.md` in Q1. **The numbers may need rebasing:** the A1 design (branch `worktree-a1-storage-design`, not on `main` yet) already uses D83–D90. If other decisions land first, Q1 renumbers these and updates every reference in this directory.

| # | Topic | Decision |
|---|---|---|
| D91 | Query context | A2 builds the DataFusion `SessionContext`. `Storage::query` (A1) returns a `ProjectQuery`: the three raw providers (`spans`, `traces`, `scores`, stored schemas, D89), the per-query `RuntimeEnv` (memory pool, spill directory) and `target_partitions`. `QueryContext::new` builds the session from that:<ul><li>an explicit function registry: DataFusion's built-in scalar, nested, aggregate and window functions, but no table functions and no higher-order (lambda) functions;</li><li>no `information_schema`, no URL tables, recursive CTEs off;</li><li>parser recursion limit 50, source spans collected;</li><li>the hex-ID optimizer rule;</li><li>only the views registered.</li></ul> |
| D92 | Read queries | The queries behind the read endpoints live in A2 (`llmobs_storage::reads`): trace, span, session, user and score lists with sort and cursor; trace, span, session and user detail; latest scores per row; dashboard metrics. They return Arrow batches with documented columns, or plain structs for metrics. The server maps them to API types and does HTTP. Cursors are encoded and checked by A2. |
| D93 | SQLite views | `experiment_items` and `dataset_items` belong to the server: their schema, their rows (a `MemTable` of at most 1M rows built per query) and their column catalog. The server registers them through `QueryContext::register_sqlite_view`. A2's sandbox only knows their names. |
| D94 | Query errors | Filter, SQL and execution error messages go back to the caller (capped at 2 KiB): the caller is querying their own project. They are never logged; logs carry the error kind, text lengths and the duration. Codes:<ul><li>`invalid_filter` (400) for filter and SQL text;</li><li>`query_failed` (400) for execution errors (bad cast, regex too big, division by zero);</li><li>`query_timeout` (422);</li><li>`query_too_large` (422) for the memory cap;</li><li>`busy` (503) when A1's admission control refuses the query.</li></ul> |
| D95 | Input limits | Filter DSL: at most 4 KiB and 128 operator tokens. SQL: at most 64 KiB and 512 operator tokens. Parser recursion limit 50. Checked on the token stream before parsing. Without these, about 3,000 `OR` terms overflow a 2 MiB stack while DataFusion plans the query (measured on 55.2 release; about 1,000 in a debug build), and the overflow aborts the process. |
| D96 | SQL sandbox | <ul><li>One statement: a query, or a plain `EXPLAIN` of a query (no `ANALYZE`, no `VERBOSE`).</li><li>Table references are unqualified names of registered views or of CTEs in scope.</li><li>No table functions. Recursive CTEs are disabled.</li><li>`score`, `score_label` and `has_score` are not available in SQL (D97).</li><li>DataFusion's `SQLOptions` (no DDL, DML or statements) is checked again on the plan as a second line of defence.</li></ul> |
| D97 | Score functions | `score(name)`, `score_label(name)` and `has_score(name)` exist in the filter DSL only, on the `traces`, `spans` and `sessions` views. For each target they use the latest live score of that name (greatest `updated_at`, then greatest `score_id`, the rule of spec 09 list rows). `score` is its `value_num` (null for categorical and text scores), `score_label` its `value_str`, `has_score` is true when such a score exists. Each distinct name compiles to a left join on a latest-score subquery; at most 8 distinct names per filter. `FunctionSchema` gains `in_sql`. |
| D98 | SQL results | JSON results are `{columns: [{name, type}], rows: [{column: value}], truncated}`:<ul><li>output column names must be unique (400 asking for `AS`);</li><li>timestamps are RFC 3339 with 9 digits, `Z` for UTC (D64);</li><li>binary is lowercase hex, maps are objects, NaN and ±inf are null;</li><li>`type` is the Arrow type as DataFusion prints it.</li></ul> At most 10,000 rows (`limit` 1–10,000, default 10,000), with `truncated: true` when more exist. CSV, NDJSON and Parquet exports stream up to 10M rows and stop there. |
| D99 | SQL parameters | Saved queries use `{name:Type}` placeholders, `Type` ∈ `String`, `Int`, `Float`, `Bool`, `Timestamp` (case-insensitive). Values arrive as JSON: string, integer, number, boolean; `Timestamp` takes RFC 3339 or integer epoch milliseconds. A missing, extra or ill-typed parameter is a 400 that names it. They become DataFusion `$n` placeholders before planning. |
| D100 | DSL casts | The filter DSL allows `CAST(x AS t)` and `x::t` with `t` one of `BIGINT`, `DOUBLE`, `VARCHAR`, `BOOLEAN`, `TIMESTAMP`. They compile to `TRY_CAST`, so `metadata['n']::DOUBLE > 3` skips rows whose value is not a number instead of failing the query. |
| D101 | SQL time window | `POST /sql` (JSON and exports) and the MCP `query_sql` tool take an optional time window `from`/`to` (RFC 3339, both or neither, `from < to`). `QueryContext::new(source, window)` writes it into the view definitions:<ul><li>`spans` and `traces` keep rows with `start_time ∈ [from, to)`;</li><li>`scores` keeps rows with `created_at ∈ [from, to)`;</li><li>`sessions` follows from the windowed `traces`;</li><li>`experiment_items` and `dataset_items` (SQLite) are not windowed.</li></ul>So every scan of a view, in joins, subqueries and CTEs too, carries the window, and storage prunes on it. The SQL console UI always sends its time picker's range. Without a window the query reads all history, bounded by the timeout, the memory cap and admission. The read endpoints build their context without a window and apply their own (`ListScope`, metrics). |

## Boundaries

```
llmobs-server ──Storage::query(project, options)──▶ A1 ──▶ ProjectQuery { spans, traces, scores, runtime, target_partitions, optimizer_rules }
llmobs-server ──QueryContext::new(ProjectQuery, window)──▶ A2: views, hex rule, physical optimizer rules, sandbox
              ──reads::* (lists, details, metrics)──▶ Arrow batches / structs ──▶ server maps to API types
              ──QueryContext::sql_json / sql_export──▶ JSON rows / byte stream
              ──QueryContext::register_sqlite_view──▶ experiment_items, dataset_items (server-built MemTables)
```

**A1 (storage engine)**
- It owns the raw providers, their pushdown, trace merging, tombstones, score last-writer-wins, the memory pool and admission.
- A2 never relies on how a provider works inside, only on its schema (`llmobs_core::schema`) and on the filters it receives.
- Time hints (`?around=`) go through A1's `QueryOptions` when the server calls `Storage::query` (D154). A2 does not see them.
- The "latest N traces" fast path is a physical optimizer rule that A1 puts in `ProjectQuery.optimizer_rules`. `session_state` appends those rules to the session without looking inside them (D153, A1 M3.7).
- Q14 documents the exact plan shape of the trace list (`ORDER BY start_time DESC, trace_id DESC LIMIT n+1` over the `traces` view), so A1's M3 can recognise it.
- **Change requested from A1:** its design says `Storage::query` returns "a `SessionContext` … and the project's providers". Under D91 it returns a `ProjectQuery` with the fields below, and builds no `SessionContext`. A1's own tests can scan its providers through a plain `SessionContext::new()`.
- `ProjectQuery` is defined once, by whichever track lands first (Q2 here, or A1 M1). The other one reuses it.
- When A1 M6 adds admission control, it gives A2's error classifier (`src/query_error.rs`, Q12) a branch that maps its busy error to `RunError::Busy`. Until then nothing produces `Busy`, so Q12 does not add that variant; A1 M6 adds it together with the branch.

**Server (Wave 2)**
- It maps API parameters to A2's request structs and A2's outputs to API types.
- It builds the trace tree (`build_tree` exists already), parses payloads, and does HTTP, SSE and MCP.
- It picks the timeout per surface (UI 30 s, SQL console 60 s, MCP 30 s) and the row cap (MCP ≤ 1,000).
- It passes the request's `from`/`to` as the SQL window (D101), and `None` for the read endpoints.
- It runs queries on runtime threads with at least 8 MiB of stack, as a backstop to D95's limits, which were measured on 2 MiB.
- It builds the SQLite-backed views (D93).
- The live trace stream's filter (SSE) is server work. A2's filter compiler will be reused there when that track starts.

**B1 (normalization)** has no contact with A2.

**Files A2 owns:** `crates/storage/src/views/`, `src/filter/`, `src/sql/`, `src/reads/`, `src/query_error.rs`, `src/query_text.rs`, `src/test_support.rs`, and their entries in `src/lib.rs`. A2 also edits `crates/storage/Cargo.toml` and the root `Cargo.toml` (new dependencies only). A1 adds dependencies there too, so expect small, mechanical merge conflicts on both files and on `Cargo.lock`. Q1 and Q20 edit docs and the server API contract; their files are listed in the sub-phases.

## Verified facts (DataFusion 55.2.0, sqlparser 0.62.0)

Checked against the source and with a probe program before writing this plan. The workspace pins `datafusion = 55.2` with features `nested_expressions, crypto_expressions, datetime_expressions, encoding_expressions, regex_expressions, string_expressions, unicode_expressions, parquet, recursive_protection, sql`. sqlparser comes through DataFusion's re-export `datafusion::sql::sqlparser`, so there is no separate `sqlparser` dependency.

**Session construction**
- `SessionContext::new()` registers the table functions `generate_series` and `range` (`datafusion-functions-table` `all_default_table_functions`).
- A session from `SessionStateBuilder::new()` plus `with_scalar_functions(SessionStateDefaults::default_scalar_functions())`, `with_aggregate_functions(…default_aggregate_functions())`, `with_window_functions(…default_window_functions())` and `with_expr_planners(…default_expr_planners())` has no table functions. Both `generate_series` and `range` then fail with "table function … not found". `SessionStateDefaults` is at `datafusion::execution::SessionStateDefaults`.
- The expression planners are required. Without them, `metadata['k']`, array literals and `@>` do not plan.
- `datafusion.catalog.information_schema` defaults to `false`, and `SHOW TABLES` then fails.
- URL tables (`SELECT * FROM '/etc/passwd'`) are off unless `enable_url_table()` is called.
- `datafusion.execution.enable_recursive_ctes` defaults to `true`. `WITH RECURSIVE` ran in the probe.
- `datafusion.sql_parser.collect_spans` defaults to `false`. With it on, planning errors carry a `Diagnostic` with a `Span` (`DataFusionError::diagnostic()`), for example on an unknown column or an unknown function.
- `datafusion.sql_parser.recursion_limit` defaults to 50.
- A fully qualified `datafusion.public.spans` resolves to the registered table. The rule that table names are unqualified is ours (D96).

**Statement checks**
- `ctx.sql_with_options(sql, SQLOptions::new().with_allow_ddl(false).with_allow_dml(false).with_allow_statements(false))` rejects `CREATE …`, `SET`, `CREATE EXTERNAL TABLE`.
- It **allows `EXPLAIN ANALYZE`**, which executes the query.
- `COPY … TO` failed only because no file format was registered. So we reject on the AST first, and keep `SQLOptions` as a second check.
- `SessionState::statement_to_plan(datafusion::sql::parser::Statement)` plans a statement parsed by us.
- `SessionState::create_logical_expr_from_sql_expr(SQLExprWithAlias, &DFSchema)` plans a parsed DSL expression against a schema.

**Parser**
- `Parser::parse_expr` stops at the first token it cannot use: `"a = 1 )"` and `"a = 1 b"` parse as `a = 1`. The DSL must check that the next token is EOF.
- `Tokenizer::tokenize_with_location` gives each token a `Span` with 1-based line and column, counted in characters (`'é'` counts as one).
- `TokenizerError` has a `location`.
- `ParserError::ParserError(String)` has no position. After a failure, the parser's `peek_token_ref().span` is the offending token, or the EOF token, whose span is empty.
- `Parser::new(&dialect).with_tokens_with_locations(tokens).with_recursion_limit(50)` parses a token stream we have checked.
- `{user:String}` tokenizes as `{`, `user`, `:`, `String`, `}`. Inside a string literal it stays one string token.
- `metadata['k']` parses as `Expr::CompoundFieldAccess { root, access_chain: [AccessExpr::Subscript(Subscript::Index { index })] }`.
- **Deep expressions crash the process.**
  - A left-deep chain of `OR`, `AND` or `+` is not limited by the parser's recursion counter: 100,000 terms parse fine.
  - Planning 3,000 `OR` terms on a 2 MiB stack aborts with "stack overflow" (release; 1,000 in debug), even with `recursive_protection`.
  - 1,000 terms in release and 300 in debug plan and run fine.

  Hence D95.

**Pushdown through the views**
- With a provider that answers `Inexact` for every filter, a query on a view that projects `encode(trace_id, 'hex') AS trace_id` receives:
  - `start_time > <literal>`, with `now() - INTERVAL …` folded;
  - `get_field(metadata, 'customer') = 'acme'`;
  - `array_has_all(tags, ['beta'])` for `tags @> ['beta']`;
  - and `encode(trace_id, 'hex') = '…'`, which no provider can use.
- An `OptimizerRule` added with `SessionStateBuilder::with_optimizer_rule` and `ApplyOrder::BottomUp` that maps expressions with `LogicalPlan::map_expressions` rewrites that comparison into `trace_id = FixedSizeBinary(16, …)`, and the provider receives the binary form.
  - This was tested on lowercase and uppercase literals, and on a two-element `IN`, which DataFusion turns into `OR`.
  - Join keys between two views stay hex strings: correct, just slower.

**Values and types**
- `5e9` compares with an `Int64` column.
- `now() - INTERVAL '1 hour'` compares with `Timestamp(ns, "UTC")`.
- `CAST(duration_ns AS DOUBLE) / 1e6` is `Float64`.
- `array_distinct(flatten(array_agg(tags)))` works on `List(Utf8)`.

**Time windows and timestamp literals**
- `start_time >= '2026-01-01T10:00:03Z'`, `'2026-01-01 10:00:03'` and `BETWEEN` two such strings reach the provider as `start_time >= TimestampNanosecond(…, Some("UTC"))`: the literal is converted, not the column. No rule is needed for string timestamps.
- A filter on the raw scan inside a view's plan (D101) reaches the provider in every scan of the view: a plain query, both sides of a self-join, an `IN (SELECT …)` subquery and a CTE. It is ANDed with the query's own pushed filters.

**Cancellation**
- Dropping the stream of a running query releases its memory reservations, but not instantly: an 8 GB cross-join sort, dropped after a 300 ms timeout, held its reservation for 1.6 s more (32 cores, release). The caller gets `Timeout` at once; the memory pool sees the release later.

**Placeholders**
- `$n` types are inferred from context (`DataFrame::logical_plan().get_parameter_types()`). `DataFrame::with_param_values(Vec<ScalarValue>)` binds them; a missing value fails at execution with "Placeholder '$1' was not provided a value".

**Regex**
- An oversized regex fails with `Regular expression did not compile: CompiledTooBig(10485760)`, reported as a DataFusion **internal** error. The error classifier must not turn every `Internal` into a 500 (Q12).
- The `regex` crate is linear-time, so there is no catastrophic backtracking.

**JSON encoding**
- arrow-json 59 (`arrow` feature `json`) encodes `Binary` and `FixedSizeBinary` as hex strings and maps as objects. It encodes temporal types through formatters (`with_timestamp_tz_format`, `with_timestamp_format`) and NaN/inf as null.
- `StructMode::ObjectOnly` (the default) writes rows as objects.

**Function signatures used later**
- `functions_aggregate::expr_fn::approx_percentile_cont(order_by: Sort, percentile: Expr, centroids: Option<Expr>)`;
- `first_value(expr, order_by: Vec<SortExpr>)`;
- `expr_fn::try_cast(expr, DataType)`;
- `date_bin`, `encode`, `array_has_all`, `regexp_like`.

## Crate layout (A2's modules)

```
crates/storage/src/
  query_error.rs    RunError (Q2, grows in Q6 and Q12), DataFusion error classification (Q12)
  query_text.rs     Position, InvalidQuery, token pre-checks, tokenizing and parsing with positions (Q6)
  views/
    mod.rs          ProjectQuery, QueryContext, ViewName, TimeWindow (Q2)
    session.rs      sandboxed SessionState (Q2)
    spans.rs        spans view (Q2)
    traces.rs       traces view (Q2)
    scores.rs       scores view (Q2)
    sessions.rs     sessions view and the sessions-from-traces plan (Q3)
    hex.rs          HexIdRule (Q4)
    catalog.rs      columns, kinds, descriptions, functions for GET /schema (Q5)
  filter/
    mod.rs          Filter, FilterView, parse + compile (Q7)
    validate.rs     AST allow-list (Q7)
    scores.rs       score()/score_label()/has_score() joins (Q8)
    scope.rs        default exclusions, time window, search (Q9)
  sql/
    mod.rs          SqlRequest, prepare, sql_json, sql_export (Q10–Q13)
    validate.rs     statement allow-list (Q10)
    params.rs       {name:Type} (Q11)
    run.rs          execution with timeout and row cap (Q12)
    encode.rs       JSON rows, CSV, NDJSON, Parquet (Q12, Q13)
  reads/
    mod.rs          shared request types, sort keys, Page (Q14)
    cursor.rs       cursor encoding and predicates (Q14)
    traces.rs       trace list, trace detail, span list, span detail (Q14, Q15)
    sessions.rs     session list and detail (Q16)
    users.rs        user list and detail (Q16)
    scores.rs       score list, latest scores per row (Q17)
    metrics.rs      dashboard metrics (Q18, Q19)
  test_support.rs   #[cfg(test)] dataset tables from fixtures/api/dataset (Q2)
```

`src/lib.rs` declares the modules. A2's public items are re-exported as listed below; everything else is `pub(crate)`.

## Public API

The A2 part of `llmobs-storage`'s public surface. Names may be adjusted in the sub-phase that adds them, as long as this section is updated in the same commit.

```rust
// views (Q2, Q5)
pub struct ProjectQuery {
    pub spans: Arc<dyn TableProvider>,
    pub traces: Arc<dyn TableProvider>,
    pub scores: Arc<dyn TableProvider>,
    pub runtime: Arc<RuntimeEnv>,
    pub target_partitions: NonZeroUsize,
    /// Physical optimizer rules the session appends: the trace list fast path (D153, A1 M3.7).
    pub optimizer_rules: Vec<Arc<dyn PhysicalOptimizerRule + Send + Sync>>,
}
pub struct QueryContext { /* private: SessionContext */ }
pub struct TimeWindow { /* from inclusive, to exclusive, ns UTC */ }   // TimeWindow::new(from, to)
impl QueryContext {
    pub fn new(source: ProjectQuery, window: Option<TimeWindow>) -> Result<QueryContext, RunError>;  // D101
    pub fn register_sqlite_view(&self, view: SqliteView, table: Arc<dyn TableProvider>) -> Result<(), RunError>;
}
pub enum ViewName { Traces, Spans, Sessions, Scores, ExperimentItems, DatasetItems }
pub enum SqliteView { ExperimentItems, DatasetItems }
pub fn view_catalog() -> Vec<ViewColumns>;          // Q5
pub fn function_catalog() -> Vec<FunctionInfo>;     // Q5

// errors (Q6, Q12)
pub struct Position { pub line: u32, pub column: u32 }
pub struct InvalidQuery { pub message: String, pub position: Option<Position> }
pub enum RunError { Invalid(InvalidQuery), InvalidRequest(String), Failed(String), Timeout, TooLarge, Internal(DataFusionError) }
// `Busy` is added by A1 M6 together with admission control.

// SQL console (Q10–Q13)
pub struct SqlRequest<'a> { pub query: &'a str, pub params: &'a serde_json::Map<String, Value>, pub limit: usize, pub timeout: Duration }
pub struct SqlJson { pub columns: Vec<SqlColumn>, pub rows: Vec<serde_json::Map<String, Value>>, pub truncated: bool }
pub enum ExportFormat { Csv, Ndjson, Parquet }
impl QueryContext {
    pub async fn sql_json(&self, request: SqlRequest<'_>) -> Result<SqlJson, RunError>;
    pub async fn sql_export(&self, request: SqlRequest<'_>, format: ExportFormat)
        -> Result<BoxStream<'static, Result<Bytes, RunError>>, RunError>;
}

// reads (Q14–Q19): free functions taking &QueryContext and a request struct
pub mod reads { /* trace_list, trace_detail, span_list, span_detail, session_list, session_detail,
                   user_list, user_detail, score_list, latest_scores, metrics */ }
```

## Order and parallelism

```
Q1 ─▶ Q2 ─▶ Q3 ─▶ Q4 ─▶ Q5 ─▶ Q6 ─▶ Q7 ─▶ Q8 ─▶ Q9 ─▶ Q10 ─▶ Q11 ─▶ Q12 ─┬─▶ Q13
                                                                        └─▶ Q14 ─▶ Q15 ─▶ Q16 ─▶ Q17 ─▶ Q18 ─▶ Q19
Q20 any time after Q1
```

- **Q1–Q12 run in order.** Q1 is the spec and decisions. Q2 adds the crate dependencies, the context and the test dataset. Q12 finishes the error classifier, which the read queries use.
- **After Q12, two lanes can run at once:**
  - the exports, Q13 (`src/sql/encode.rs`, `src/sql/mod.rs`);
  - the read queries, Q14–Q19 (`src/reads/`).

  They share no file.
- **Q20** changes the server API contract: `in_sql` and the new problem codes. It can run any time after Q1, on its own, since it touches `crates/server/`, `fixtures/api/` and the generated web types.

## Conventions for every sub-phase

- **Errors.** Use `thiserror` enums. Messages never include payload content or literal values from the stored data. Filter and SQL text the caller sent may appear in messages returned to that caller, but in nothing that is logged (D94).
- **No panics.** No `unwrap`/`expect` outside tests, unless a comment states the invariant. No recursion over user-controlled input in our own code: the token pre-checks (Q6) bound the AST before any recursive walk.
- **No detached work.** A2 never calls `tokio::spawn` or `spawn_blocking`. All work runs inside the future or stream returned to the caller, so dropping it (timeout, client disconnect) cancels everything DataFusion started.
- **Tests:**
  - use the dataset tables from `test_support` (Q2);
  - assert exact rows, not just counts, unless the sub-phase says otherwise;
  - cover the error case of every rule the sub-phase adds.
- **Checks.** `just check` must pass. Rust-only sub-phases may run `just check-rust` while iterating, but run `just check` before committing. Node is not on the non-interactive PATH on the dev machine; see `.node-version` and put that Node version first on `PATH` before `just check`.
- **Report.** At the end, report:
  - what was done;
  - every deviation from this plan and why;
  - exact versions of new dependencies;
  - anything left unverified.
- **Commit.** One commit per sub-phase, subject as given, ending with the `Co-Authored-By` line the session gives you.

## After A2

These are not sub-phases of this plan:
- Once A1 M1 has landed, an end-to-end test runs `Storage::query` → `QueryContext::new` → `reads::trace_list` on appended spans. That test belongs to whichever of the two tracks lands second.
- The server's handlers for these endpoints, the SSE filter, and the MCP tools are server work (Wave 2).
- A Tantivy index for search is post-MVP (D25).
