# A2 · 04 — Read queries (Q14–Q17)

Read [README.md](README.md) first: decisions D91–D101, boundaries, verified DataFusion facts, crate layout, public API.

`llmobs_storage::reads` holds the queries behind the read endpoints of spec 09 § Observability data (D92). Each function:
- takes `&QueryContext` and a request struct;
- runs under the request's `timeout` (`tokio::time::timeout`, `RunError::Timeout`);
- maps DataFusion errors through `classify` (Q12);
- returns Arrow batches whose columns are those of a view, or a documented subset.

The server maps the batches to its API types (`TraceSummary`, `SpanSummary`, …) and does HTTP.

**Golden data:** `fixtures/api/responses/` holds what the API returns for the example dataset. Each test below compares A2's output with those files, through `serde_json::Value`:
- IDs, order and scalar fields;
- timestamps as RFC 3339;
- `scores` arrays only where the sub-phase says.

---

## Q14 — Cursors and the trace list

**Goal:** shared list machinery (sort, keyset cursor, scope) and `reads::trace_list`.

**Edit only:** new `crates/storage/src/reads/{mod.rs,cursor.rs,traces.rs}`, `crates/storage/src/lib.rs`, `crates/storage/Cargo.toml` (`hex` and `serde` if not yet there).

**Commit:** `Add the trace list query`

### Shared types (`reads/mod.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder { Asc, Desc }

#[derive(Debug, Clone, Copy)]
pub struct ListScope { pub window: Option<TimeWindow>, pub include_eval: bool, pub include_experiments: bool }

#[derive(Debug)]
pub struct Page { pub batches: Vec<RecordBatch>, pub next_cursor: Option<String> }

pub const MAX_PAGE: usize = 1000;
```

- `limit` outside `1..=MAX_PAGE`: `InvalidRequest("limit must be between 1 and 1000")`.
- A filter or search that is empty or only whitespace counts as absent.

### Cursor (`reads/cursor.rs`)

```rust
pub(crate) struct Cursor { list: String, sort: String, order: SortOrder, value: CursorValue, tie: Vec<String> }
pub(crate) enum CursorValue { Null, Int(i64), Float(f64) }   // timestamps are Int (ns)
```

**Encoding.** `encode()` is the lowercase hex of the compact JSON `{"l":…,"s":…,"o":"asc"|"desc","v":null|int|float,"t":[…]}`.

**Decoding.** `decode(text, list, sort, order)`:
- text longer than 4 KiB, non-hex, invalid JSON or wrong field types: `InvalidRequest("invalid cursor")`;
- a different `list`, `sort` or `order`: `InvalidRequest("the cursor belongs to another list or sort order")`.

**Keyset predicate.** `pub(crate) fn after(cursor, sort_col: &str, tie_cols: &[&str]) -> Expr` builds the predicate for rows after the cursor in the order `sort_col <order> NULLS LAST, tie_1 <order>, …`. The tie columns are non-null strings, and `T > t` is the lexicographic tuple comparison spelled out with `OR`/`AND`. With `<` for `Desc` and `>` for `Asc`:

| Cursor value | Predicate |
|---|---|
| non-null `v` | `sort_col <op> v OR (sort_col = v AND T <op> t) OR sort_col IS NULL` |
| null | `sort_col IS NULL AND T <op> t` |

**Ordering.** `pub(crate) fn order_by(sort_col, tie_cols, order) -> Vec<SortExpr>` gives `sort_col <order> NULLS LAST`, then each tie column in `<order>`.

**Page assembly.** Run with `limit(0, Some(limit + 1))`. If more than `limit` rows come back, keep `limit`, and build `next_cursor` from the last kept row:
- the sort value is read as `i64` from `Int64` or `Timestamp(ns)` columns, as `f64` from `Float64`, or null;
- the tie values are strings.

### Trace list (`reads/traces.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceSort { StartTime, Duration, CostTotal, TotalTokens }   // columns start_time, duration_ns, cost_total, total_tokens
pub struct TraceListRequest<'a> {
    pub filter: Option<&'a str>, pub search: Option<&'a str>, pub scope: ListScope,
    pub sort: TraceSort, pub order: SortOrder, pub limit: usize, pub cursor: Option<&'a str>, pub timeout: Duration,
}
pub async fn trace_list(ctx: &QueryContext, request: TraceListRequest<'_>) -> Result<Page, RunError>;
```

Plan, in this order:
1. Start from `ctx.session().table("traces")`.
2. Parse the filter with `parse_filter(FilterView::Traces, …)`.
3. Add the conjuncts that apply:
   - `time_window(window)` when a window is set;
   - `exclusions(scope.include_eval, scope.include_experiments, filter.references_environment)`;
   - `search(Traces, …)`;
   - the cursor predicate.
4. `Filter::apply` (score joins, then the user expression).
5. `sort(order_by(<column>, &["trace_id"], order))`, then `limit(0, Some(limit + 1))`.

Output columns: the `traces` view's, in view order.

**Plan shape for A1.** Without score functions, the default list is `Limit(n+1) ← Sort(start_time DESC NULLS LAST, trace_id DESC) ← Filter ← traces view`. Put this sentence in the module comment; A1 M3's "latest N" fast path must recognise this shape (README § Boundaries).

### Tests

1. Default request: no filter, no window, desc by start time, limit 50. IDs and order equal `responses/traces.json`. Each row's scalar fields equal the fixture's: `name`, `status`, `span_count`, `error_count`, `cost_total`, `total_tokens`, `tags`, `models`, `metadata`, `session_id`, `user_id`, `environment`.
2. `include_eval = true` gives `responses/traces-include-eval.json`'s IDs and order.
3. **Paging:**
   - for every `TraceSort` × `SortOrder`, walking pages of `limit = 2` with `include_eval = true` gives exactly the single-query result with `limit = 50`;
   - the walk covers at least one trace whose sort value is null, so `NULLS LAST` and the null-cursor branch are exercised. Use a hand-built traces table when the dataset has no null `cost_total` or `total_tokens`.
4. `next_cursor` is `None` on the last page.
5. A cursor from a `CostTotal` walk, used with `StartTime`, gives `InvalidRequest`. So do `"zz"` and the hex of `{}`.
6. A filter error gives `RunError::Invalid` with a position. A filter using `score(…)` works (one dataset case).
7. A search plus a window gives the traces of the matching spans. A search without a window gives `InvalidRequest`.
8. A window `[10:05, 10:21)` gives exactly the traces starting in it.
9. `limit` 0 and 1,001 give `InvalidRequest`.

### Report

The `EXPLAIN` of the default request, and whether the `Sort` + `Limit` became a single `SortExec` with `fetch` (TopK).

---

## Q15 — Span list, trace detail, span detail

**Goal:** `reads::span_list`, `reads::trace_detail`, `reads::span_detail`.

**Edit only:** `crates/storage/src/reads/{traces.rs,mod.rs}`.

**Commit:** `Add span list and trace detail queries`

### Span list

```rust
pub enum SpanSort { StartTime, Duration, CostTotal, TotalTokens }   // start_time, duration_ns, cost_total, total_tokens
pub struct SpanListRequest<'a> { /* same fields as TraceListRequest, sort: SpanSort */ }
pub async fn span_list(ctx: &QueryContext, request: SpanListRequest<'_>) -> Result<Page, RunError>;
```

- Same plan as the trace list, on the `spans` view with `FilterView::Spans`, `search(Spans, …)` and tie columns `["span_id", "trace_id"]`.
- **Light columns:** `llmobs_core::schema::SPANS_LIGHT_COLUMNS` without `project_id`, in that order, with `duration_ms` after `duration_ns`. Name the list `pub const SPAN_LIST_COLUMNS: &[&str]` so the server can rely on it.
- Default exclusions apply to the span's own `environment` and `metadata` (spec 05).

### Trace detail

```rust
pub const MAX_TRACE_SPANS: usize = 20_000;
pub struct TraceDetailData { pub trace: RecordBatch, pub spans: Vec<RecordBatch>, pub truncated: bool }
pub async fn trace_detail(ctx: &QueryContext, trace_id: &str, timeout: Duration) -> Result<Option<TraceDetailData>, RunError>;
```

- `trace_id` that is not 32 hex digits: `InvalidRequest("trace_id must be 32 hex digits")`. Either case is accepted. Compare through the view, lowercased; Q4 turns it into bytes.
- `trace`: the one `traces` row, all view columns. When there is none, return `Ok(None)`.
- `spans`: `SPAN_LIST_COLUMNS` from `spans` where `trace_id = <id>`, ordered `start_time ASC, span_id ASC`, limited to `MAX_TRACE_SPANS + 1`. `truncated` is true when the extra row exists; drop it.
- No default exclusions (spec 09: they don't apply to trace detail).
- Duplicates are returned as stored. The server's tree builder keeps the first (spec 09).
- The `?around=` hint is A1's, through `Storage::query`'s options (README § Boundaries).

### Span detail

```rust
pub async fn span_detail(ctx: &QueryContext, trace_id: &str, span_id: &str, timeout: Duration) -> Result<Option<RecordBatch>, RunError>;
```

- IDs are validated like the trace detail's (`span_id`: 16 hex digits).
- It returns all `spans` view columns of the row with that `(trace_id, span_id)`. With duplicates, the greatest `ingest_time` wins (`ORDER BY ingest_time DESC LIMIT 1`).

### Tests

1. `span_list` default: the IDs and order equal `responses/spans.json`, and the scalar fields match per row.
2. Span paging: the walk test from Q14, with `SpanSort`.
3. `trace_detail` for each dataset trace:
   - the trace row equals that trace's entry in `responses/traces-include-eval.json`;
   - the span IDs, in order, equal the `(start_time, span_id)`-sorted spans of `responses/traces/<id>.json`, which are pre-ordered.

   Compare as a set plus the sort rule, not against the tree order.
4. Uppercase `trace_id` input works. `"abc"` gives `InvalidRequest`. An unknown valid ID gives `Ok(None)`.
5. Truncation: a hand-built spans table with 5 spans in one trace, with `MAX_TRACE_SPANS` passed as 3. Make the cap a parameter of an internal function so tests can lower it. Result: 3 spans and `truncated: true`.
6. `span_detail` for trace 1 span 2 equals `responses/spans/a1…01/b1…12.json` on the scalar fields (name, kind, times, tokens, cost, `status_message`). The `input` column holds the stored text that the fixture shows parsed.
7. Duplicate rows (hand-built) give the greatest `ingest_time`.

### Report

Nothing special beyond the usual.

---

## Q16 — Sessions and users

**Goal:** `reads::session_list`, `session_detail`, `user_list`, `user_detail`.

**Edit only:** new `crates/storage/src/reads/{sessions.rs,users.rs}`, `crates/storage/src/reads/mod.rs`.

**Commit:** `Add session and user queries`

### Sessions

```rust
pub enum SessionSort { StartTime, EndTime, TraceCount, CostTotal }
pub struct SessionListRequest<'a> { pub filter: Option<&'a str>, pub scope: ListScope, pub sort: SessionSort,
    pub order: SortOrder, pub limit: usize, pub cursor: Option<&'a str>, pub timeout: Duration }
pub async fn session_list(ctx: &QueryContext, request: SessionListRequest<'_>) -> Result<Page, RunError>;
```

- Traces are scoped first:
  - `traces` view;
  - the time window on the traces' `start_time`;
  - exclusions with `references_environment = false`, since the `sessions` view has no `environment`.

  So a session's aggregates cover its traces inside the window. Document this on the function; spec 05 (Q1) says the same.
- Then `sessions_from_traces` (Q3), then the user filter on `FilterView::Sessions` (score joins on `session_id`), the cursor and sort (tie `session_id`), and the limit.
- Output: the `sessions` view columns.

```rust
pub const MAX_SESSION_TURNS: usize = 1000;
pub struct SessionDetailData { pub session: RecordBatch, pub turns: Vec<RecordBatch>, pub roots: Vec<RecordBatch>, pub truncated: bool }
pub async fn session_detail(ctx: &QueryContext, session_id: &str, include_eval: bool, include_experiments: bool,
    timeout: Duration) -> Result<Option<SessionDetailData>, RunError>;
```

- Scoped traces: `traces` where `session_id = <id>`, plus exclusions. No window.
- `session`: `sessions_from_traces` of them, one row. None → `Ok(None)`.
- `turns`: those traces, all `traces` view columns, ordered `start_time ASC, trace_id ASC`, limited to `MAX_SESSION_TURNS + 1`, with `truncated` as in the trace detail.
- `roots`: for the kept turns, the root span's `trace_id, input, input_format, output, output_format`. Join `spans` on `trace_id` and `span_id = root_span_id`. Turns without a root have no row.

### Users

```rust
pub enum UserSort { LastSeen, TraceCount, CostTotal }
pub struct UserListRequest<'a> { pub filter: Option<&'a str>, pub scope: ListScope, pub sort: UserSort,
    pub order: SortOrder, pub limit: usize, pub cursor: Option<&'a str>, pub timeout: Duration }
pub async fn user_list(ctx: &QueryContext, request: UserListRequest<'_>) -> Result<Page, RunError>;
```

- Scoped traces: `traces`, the window, exclusions (with the filter's `references_environment`), and the user filter on `FilterView::Traces`, **before** grouping (spec 09 `UserListParams`). Then `user_id IS NOT NULL`.
- Group by `user_id`:
  - `min(start_time) AS first_seen`, `max(end_time) AS last_seen`;
  - `count(*) AS trace_count`, `count(DISTINCT session_id) AS session_count`;
  - `sum(error_count) AS error_count` (non-null Int64);
  - `sum(total_tokens) AS total_tokens`, `sum(cost_total) AS cost_total`.
- Sort columns `last_seen`, `trace_count`, `cost_total`; tie `user_id`.
- Output columns in this order: `user_id, first_seen, last_seen, trace_count, session_count, error_count, total_tokens, cost_total`. Name the list `pub const USER_COLUMNS`.

```rust
pub const MAX_USER_SESSIONS: usize = 100;
pub struct UserDetailData { pub user: RecordBatch, pub sessions: Vec<RecordBatch> }
pub async fn user_detail(ctx: &QueryContext, user_id: &str, include_eval: bool, include_experiments: bool,
    timeout: Duration) -> Result<Option<UserDetailData>, RunError>;
```

- `user`: the aggregate above over the user's excluded traces, without window or filter. None → `Ok(None)`.
- `sessions`: whole sessions, not only the user's traces in them. Take `sessions_from_traces` over excluded traces whose `session_id` is in the set of this user's traces' session IDs. Order by `start_time DESC, session_id DESC`, limit `MAX_USER_SESSIONS`. This matches the fixture generator (`crates/server/tests/api_examples.rs` `user_views`).

### Tests

1. `session_list` default and with `include_eval`: rows equal `responses/sessions.json` and `sessions-include-eval.json` (all fields but `scores`).
2. `session_detail("sess-001")` default and with `include_eval`: the session row equals `responses/sessions/sess-001*.json`'s `session`. The turn trace IDs, in order, equal its `turns`. Each root's `input` is present where the fixture has one.
3. `session_detail` of an unknown ID gives `Ok(None)`.
4. Session paging walk for every `SessionSort` × `SortOrder` (as in Q14).
5. The session filter `trace_count >= 2`, and `has_score('<a session score name>')`.
6. `user_list` default and with `include_eval` equal `responses/users*.json`.
7. `user_detail("user-42")` default and with `include_eval`, and `user-7`, equal `responses/users/*.json`: the user row and the session IDs in order.
8. User paging walk. A filter `environment = 'production'` on users keeps evaluation traces out without `include_eval`, and the exclusion logic sees the reference.
9. Window semantics: a window covering only one of a session's traces gives `trace_count = 1` for that session.

### Report

How `count(DISTINCT session_id)` planned.

---

## Q17 — Score list and row scores

**Goal:** `reads::score_list` (`GET /scores`) and `reads::latest_scores` (the `scores` field of trace and session rows).

**Edit only:** new `crates/storage/src/reads/scores.rs`, `crates/storage/src/reads/mod.rs`.

**Commit:** `Add score queries`

### Score list

```rust
pub struct ScoreListRequest<'a> {
    pub target: Option<ScoreTargetKind>, pub trace_id: Option<&'a str>, pub span_id: Option<&'a str>,
    pub session_id: Option<&'a str>, pub name: Option<&'a str>, pub source: Option<ScoreSource>,
    pub window: Option<TimeWindow>,            // on created_at
    pub limit: usize, pub cursor: Option<&'a str>, pub timeout: Duration,
}
pub async fn score_list(ctx: &QueryContext, request: ScoreListRequest<'_>) -> Result<Page, RunError>;
```

- `ScoreTargetKind` and `ScoreSource` come from `llmobs_core::enums`. Compare with their serialized names, not string literals.
- The IDs are validated as in Q15.
- Equality conjuncts for each present field, plus the `created_at` window.
- Order `created_at DESC, score_id DESC`, fixed. The cursor's sort name is `created_at`, and `order` is always `Desc`.
- Output: the `scores` view columns.

### Latest scores per row

```rust
pub enum ScoreTargets<'a> { Traces(&'a [&'a str]), Sessions(&'a [&'a str]) }
pub const LATEST_SCORE_COLUMNS: &[&str] = &["target_key", "name", "score_id", "data_type", "value_num", "value_str", "source"];
pub async fn latest_scores(ctx: &QueryContext, targets: ScoreTargets<'_>, timeout: Duration) -> Result<Vec<RecordBatch>, RunError>;
```

- At most `MAX_PAGE` keys: more gives `InvalidRequest`. Trace IDs are validated as hex.
- From the `scores` view:
  - `Traces`: `target = 'trace' AND trace_id IN (…)`, with `target_key = trace_id`;
  - `Sessions`: `target = 'session' AND session_id IN (…)`, with `target_key = session_id`.
- Latest per `(target_key, name)` by `updated_at DESC, score_id DESC` (`row_number`, same rule as D97 and spec 09).
- Sorted by `target_key, name`.
- An empty key list returns no batches, with no query.

### Tests

1. `score_list` default equals `responses/scores.json`: IDs, order, fields.
2. Each filter field narrows correctly: target, `trace_id` (uppercase accepted), `span_id`, `session_id`, name, source, window.
3. Paging walk with `limit = 2`.
4. `latest_scores(Traces(all dataset trace IDs))` equals, per trace, the `scores` arrays of `responses/traces-include-eval.json`: name order, `score_id`, value, source. `Sessions` does the same against `responses/sessions-include-eval.json`.
5. "Latest" ties (hand-built): the greater `score_id` wins.
6. 1,001 keys give `InvalidRequest`. An empty list gives empty output.

### Report

Nothing special beyond the usual.
