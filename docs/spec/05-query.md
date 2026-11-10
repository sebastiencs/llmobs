# 05 — Query: filter DSL, SQL console, search, metrics

Everything compiles to DataFusion plans over the project-scoped providers of 03 §6. Those providers expose the stored schemas (binary IDs, `project_id`); the views below are built on top of them by the query layer (`llmobs-storage` `filter/` and `sql/` modules, D89), which also rewrites hex-literal ID comparisons to binary ones and adds the default exclusions. `QueryContext::new` (in `llmobs-storage`) builds the DataFusion session from the `ProjectQuery` that `Storage::query` returns (D91): the raw providers, the per-query runtime (memory pool, spill directory) and `target_partitions`.

## Logical views exposed to users

| View | Rows | Backed by |
|---|---|---|
| `spans` | one per span | `SpansProvider` |
| `traces` | one per trace (merged; the merge bookkeeping columns `last_ingest_time` and `partial_count` are not exposed) | `TracesProvider` |
| `sessions` | one per session_id | `SELECT session_id, min(start_time) AS start_time, max(end_time) AS end_time, count(*) AS trace_count, sum(error_count) AS error_count, sum(total_tokens) AS total_tokens, sum(cost_total) AS cost_total, first non-null user_id by (start_time, trace_id) AS user_id, sorted distinct union of tags AS tags, duration_ns, duration_ms … FROM traces WHERE session_id IS NOT NULL GROUP BY session_id` |
| `scores` | one per live score | `ScoresProvider` |
| `experiment_items` | experiment results | SQLite rows for the project, exposed via a `MemTable` built per query (bounded to 1M rows). The server owns the schema, the rows and the column catalog, and registers the table through `QueryContext::register_sqlite_view` (D93) |
| `dataset_items` | current version items | same approach |

Each view exposes the stored columns of its table in spec 01 order, without `project_id`. `traces` also drops `last_ingest_time` and `partial_count`. Binary IDs (`trace_id`, `span_id`, `parent_span_id`, `root_span_id`) are lowercase hex `Utf8`. `duration_ms` follows `duration_ns`: on `spans`, `duration_ns` is stored; on `traces` and `sessions`, `duration_ns = end_time − start_time` in nanoseconds, placed after `end_time`, and `duration_ms = duration_ns / 1e6`. `events` and `links` keep their native list-of-struct types. They are listed as `json` in the schema endpoint because clients receive them as JSON.

`sessions` columns, in order: `session_id, start_time, end_time, duration_ns, duration_ms, trace_count, error_count, total_tokens, cost_total, user_id, tags`. `user_id` is the first non-null `user_id` of the session's traces ordered by `(start_time, trace_id)`. `tags` is the sorted distinct union of the traces' tags.

An optimizer rule rewrites `id = '<hex>'`, `id <> '<hex>'`, `id IN ('<hex>', …)` and `id NOT IN (…)` on these ID columns into comparisons on the stored bytes, so the providers can prune with bloom filters. The literal must be exactly twice the ID's byte width and contain only hex digits, in either case. Otherwise the comparison stays on the hex string, which never matches an invalid ID. A comparison between two ID columns of the same plan node compares bytes. Joins between two views on an ID compare the hex strings: correct, but not pruned.

Column reference for each view is generated from the Arrow schemas and served at `GET /api/v1/projects/{p}/schema` (used by the UI autocomplete, the docs and the MCP `get_schema` tool).

## Default exclusions

Unless the request opts in (`include_eval=true` / `include_experiments=true`, or the filter explicitly constrains `environment`), list pages, user aggregates, dashboard series and alert queries add `environment IS DISTINCT FROM 'llmobs-eval'` and exclude experiment traces (`metadata['llmobs.experiment_id'] IS NULL`). "The filter explicitly constrains `environment`" means the filter text references the `environment` column anywhere. Then the `llmobs-eval` exclusion is not added; the experiment exclusion still is.

The exclusions apply to the view the request reads: `traces` and `spans` directly; session and user lists through the traces they aggregate; span-based and score-based metrics through a semi-join on the excluded traces.

The SQL console applies **no** implicit exclusion (users see raw data; the schema docs explain the reserved environments).

## Filter DSL

A **SQL boolean expression** — the body of a `WHERE` clause — evaluated against one view (`traces`, `spans`, `sessions` or `scores` depending on the screen).

```
status = 'error' AND model LIKE 'gpt-5%'
cost_total > 0.10 AND tags @> ['beta']
metadata['customer'] = 'acme' AND duration_ns > 5e9
start_time > now() - INTERVAL '1 hour'
score('hallucination') >= 0.5
input ILIKE '%refund%'
```

### Grammar

Parsed with `sqlparser::Parser::parse_expr` (DataFusion dialect), then validated:

- **Identifiers:** bare column names of the target view, case-insensitive unless double-quoted. No qualified names.
- **Map access:** `metadata['k']`, `attributes['k']`, `resource_attributes['k']`, with a string literal key, on the views that have the column.
- **Operators:** `= != <> < <= > >= AND OR NOT`, unary `-`, `IS [NOT] NULL`, `IS [NOT] DISTINCT FROM`, `IN (…)`, `BETWEEN`, `LIKE`, `ILIKE`, arithmetic. `@>` (array contains) is planned by DataFusion as `array_has_all`.
- **Casts:** `CAST(x AS t)` and `x::t` with `t` ∈ `BIGINT, DOUBLE, VARCHAR, BOOLEAN, TIMESTAMP`, compiled to `TRY_CAST` (D100).
- **Literals:** strings, numbers (incl. `5e9`), booleans, `NULL`, `INTERVAL '…'`, array literals.
- **Functions (allow-list):** `now()`, `lower`, `upper`, `length`, `coalesce`, `array_has`, `array_length`, `date_trunc`, `abs`, called without `DISTINCT`, `FILTER`, `OVER`, `ORDER BY` or named arguments.
- **Score functions:** `score(name)`, `score_label(name)` and `has_score(name)` on `traces`, `spans` and `sessions` only, each with a single string-literal argument (D97). For each target they use the latest live score of that name (greatest `updated_at`, then greatest `score_id`). `score` is its `value_num` (null for categorical and text scores), `score_label` its `value_str`, `has_score` is true when such a score exists. Each distinct name compiles to a left join on a latest-score subquery. At most 8 distinct score names per filter.
- **Limits:** at most 4 KiB and 128 operator tokens (D95). An operator token is one of `=`, `==`, `<>`/`!=`, `<`, `>`, `<=`, `>=`, `<=>`, `+`, `-`, `*`, `/`, `%`, integer division, `||`, `@>`, `<@`, `->`, `->>`, `#>`, `#>>`, `@@`, `?`, `?&`, `?|`, `&`, `|`, `^`, `<<`, `>>`, `~`, `~*`, `!~`, `!~*`, `::`, or a word whose keyword is `AND`, `OR`, `NOT`, `IS`, `IN`, `BETWEEN`, `LIKE`, `ILIKE`, `SIMILAR`, `XOR` or `DIV`.
- Anything else (subqueries, casts to other types, window functions, unknown identifiers/functions) → 400 `invalid_filter` with a 1-based character position when one is known.
- `duration_ms` is available as a derived column (`duration_ns / 1e6`) on the `spans`, `traces` and `sessions` views.

### UI builder

The visual builder produces the same text expression (each chip = one conjunct); the text field and chips are two views of the same AST. Unsupported (hand-written) expressions are shown as a single raw chip.

### Saved views

All list-page state lives in URL search params; a **saved view** stores those params under a name (`saved_views`, 04): private or shared with the project, with an optional default per page. The SQL console saves queries the same way (`kind = 'sql'`, with `{name:Type}` parameters). Saved views are also exposed to the MCP server (`list_saved_views`, and `search_traces` accepts `view`).

## Full-text search

The search box on list pages adds a conjunct:

```
(input ILIKE '%q%' OR output ILIKE '%q%' OR name ILIKE '%q%'
 OR system_instructions ILIKE '%q%')          -- spans view
```

`\` becomes `\\`, `%` becomes `\%` and `_` becomes `\_`. The pattern is `%<escaped>%`, with `ILIKE … ESCAPE '\'`. An empty or all-whitespace search adds nothing.

On the `traces` view, search runs on `spans` first (semi-join: `trace_id IN (SELECT trace_id FROM spans WHERE <search> AND <time window>)`). Search **requires a time window** (default: the page's time picker, max 30 days unless the user confirms "search all") because it is a scan. DataFusion's `ILIKE` on Utf8 runs on the arrow string kernels; with column projection only the needed heavy columns are read.

Later (post-MVP): Tantivy index per partition built at compaction.

## SQL console

`POST /api/v1/projects/{p}/sql` with `{query, params?, from?, to?, limit?, timeout_ms?}` (09).

### Sandbox

1. Limits: 64 KiB and 512 operator tokens (the same operator tokens as the filter DSL); parser recursion limit 50 (D95).
2. Exactly one statement: a query (`SELECT`, `WITH`, `UNION`/`INTERSECT`/`EXCEPT`, `VALUES`), or `EXPLAIN` of a query without `ANALYZE` or `VERBOSE`.
3. Table references are unqualified names of the registered views (`spans`, `traces`, `sessions`, `scores`, `experiment_items`, `dataset_items`) or of CTEs in scope. No table functions, no `information_schema`, no URL tables, no recursive CTEs.
4. Functions: DataFusion's built-in scalar, nested, aggregate and window functions. No table or higher-order functions. Score functions are DSL-only (D97).
5. The planned statement is checked again with DataFusion's `SQLOptions` (no DDL, DML or statements).
6. Session config: per-query runtime from storage (memory cap), `batch_size = 8192`, `target_partitions` from storage. Timeout: `tokio::time::timeout` around planning and execution; dropping the stream cancels it.
7. Time window (D101): an optional `[from, to)` written into the `spans` and `traces` views (on `start_time`) and the `scores` view (on `created_at`); `sessions` follows from `traces`; the SQLite views are not windowed. Every scan of a view carries it, in joins, subqueries and CTEs too. Without a window the query reads all history.
8. Results (D98): `{columns: [{name, type}], rows: [{column: value}], truncated}`. Output column names must be unique. Timestamps are RFC 3339 with 9 fractional digits, `Z` for UTC (D64). Binary is lowercase hex, maps are objects, NaN and ±inf are null. `type` is the Arrow type as DataFusion prints it. At most 10,000 rows (`limit` 1–10,000, default 10,000), with `truncated: true` when more exist. CSV, NDJSON and Parquet exports stream up to 10M rows and stop there.

### Parameters

`{name:Type}` placeholders, `Type` ∈ `String`, `Int`, `Float`, `Bool`, `Timestamp` (case-insensitive), are rewritten to DataFusion `$n` placeholders with typed values (D99). Values arrive as JSON: string, integer, number, boolean; `Timestamp` takes RFC 3339 or integer epoch milliseconds. A missing, extra or ill-typed parameter is a 400 that names it. A placeholder may appear several times and binds one value. Placeholders are only recognized outside string literals and comments. Writing `$1` directly is rejected (400). Used by the UI's saved queries and the MCP tool.

### Examples shipped in the UI

- Cost per model per day.
- p50/p95/p99 latency of LLM spans per model.
- Traces with the most tool calls.
- Average judge score per release.
- Users by spend.

## Dashboard metrics (fixed dashboard)

`GET /api/v1/projects/{p}/metrics?from&to&bucket=auto&filter=<dsl>&group_by=model|environment|release`

`bucket=auto` picks the smallest width in `1m, 5m, 10m, 15m, 30m, 1h, 6h, 12h, 1d` giving at most 120 buckets over `[from, to)`, else `1d`. Buckets are aligned to their width from the Unix epoch, UTC: a 10-minute bucket starts at :00, :10, …; a 1-day bucket at midnight UTC. The first bucket starts at `from` rounded down to the width, so it may cover part of its range only; the last one is the bucket containing `to − 1 ns`. An explicit bucket giving more than 2,000 buckets is a 400. Points cover every bucket. A bucket with no rows has no value (count series included). Rows are bucketed with `date_bin` over the series' time column.

Returns time series:

| Series | Query (sketch) |
|---|---|
| traces | `count(*)` on `traces` |
| errors | `sum(error_count > 0)` on `traces` |
| error rate | errors / traces |
| latency p50/p95/p99 (traces) | `approx_percentile_cont(duration_ns, q)` on `traces` |
| LLM latency p50/p95 by model | on `spans WHERE span_kind='llm'` |
| tokens (input/output/cache) | sums on `spans WHERE span_kind IN ('llm','embedding')` |
| cost | `sum(cost_total)` on `traces` (total) and on spans grouped by model |
| top models by cost | `llm` spans with a non-null model, at most 10, by `cost_total` descending (nulls last), then model ascending |
| scores | `avg(value_num)` per score name per bucket on `scores` |
| evaluation cost | `sum(cost_total)` on `traces WHERE environment = 'llmobs-eval'` |
| TTFT p50/p95 | spans `ttft_ns` |

The filter is a `traces`-view DSL expression. Span series keep spans whose trace passes it, and score series keep scores whose trace passes it, through a semi-join on `trace_id` over traces with `start_time ∈ [from − 1 day, to)` and `end_time ≥ from`. Session-target scores are not in the score series.

`group_by` splits the series that are not already split (traces, errors, error rate, trace latency, total cost, tokens, TTFT) by the group value; the response's `group` field carries it. With `group_by=model`, a trace-based series counts each trace once under every model in its `models` list. With `group_by`, rows whose group value is null are left out of the split series. Series that are inherently split carry their own key in `group`: the model for LLM latency and cost by model, the score name for score averages.

KPIs cover `[from, to)` and the previous range of the same length; every KPI value is absent when its range has no traces.

All series honor the filter DSL and the default exclusions (applied as above). Results are cached for 10 s per `(project, params)`.

## Trace and span APIs

These queries are `llmobs_storage::reads` (D92).

| Operation | Plan |
|---|---|
| Trace list | `traces` view, filter, sort `start_time DESC` (or by `cost_total`, `duration`, `total_tokens`), cursor `(sort_value, trace_id)`; fast path from 03 §6 |
| Trace detail | `spans WHERE trace_id = X`, light columns only for the tree; heavy columns fetched per span on demand (`GET …/spans/{span_id}`) |
| Session detail | `traces WHERE session_id = X ORDER BY start_time`, then root input/output per trace for the chat view |
| Span list | `spans` view with filter (e.g. all LLM calls for model X) |
| User list | `SELECT user_id, count(*), sum(cost_total), max(end_time) … FROM traces GROUP BY user_id` |

Sort tie-breakers: traces `trace_id`; spans `(span_id, trace_id)`; sessions `session_id`; users `user_id`; scores `score_id`, with lists sorted by `created_at`. Pagination is keyset on `(sort value, tie-breaker)`, nulls last in both directions.

Trace tree assembly happens in Rust after the scan (parent/child map); spans whose parent is missing, or that sit in a parent cycle, are emitted as roots with `detached: true`, in pre-order by `(start_time, span_id)` (09 § Response shapes).

## Errors

Filter, SQL and execution errors (D94) go back to the caller, capped at 2 KiB, and are never logged. Logs carry the error kind, the text lengths and the duration.

| Code | Status | When |
|---|---|---|
| `invalid_filter` | 400 | Filter and SQL text, with `position` when one is known |
| `query_failed` | 400 | Execution errors (bad cast, regex too big, division by zero), with DataFusion's message capped at 2 KiB |
| `query_timeout` | 422 | The query exceeded its time limit |
| `query_too_large` | 422 | The query needed more memory than the cap |
| `busy` | 503 | Admission control refused the query |
