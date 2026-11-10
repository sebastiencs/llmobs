# 05 — Query: filter DSL, SQL console, search, metrics

Everything compiles to DataFusion plans over the project-scoped providers of 03 §6.

## Logical views exposed to users

| View | Rows | Backed by |
|---|---|---|
| `spans` | one per span | `SpansProvider` |
| `traces` | one per trace (merged; the merge bookkeeping columns `last_ingest_time` and `partial_count` are not exposed) | `TracesProvider` |
| `sessions` | one per session_id | `SELECT session_id, min(start_time) AS start_time, max(end_time) AS end_time, count(*) AS trace_count, sum(cost_total) AS cost_total, sum(total_tokens) AS total_tokens, sum(error_count) AS error_count, first_value(user_id) AS user_id, <distinct union of tags> AS tags, max(end_time) - min(start_time) AS duration_ns … FROM traces WHERE session_id IS NOT NULL GROUP BY session_id` |
| `scores` | one per live score | `ScoresProvider` |
| `experiment_items` | experiment results | SQLite rows for the project, exposed via a `MemTable` built per query (bounded to 1M rows) |
| `dataset_items` | current version items | same approach |

`project_id` is not a visible column (always the current project). Binary IDs are shown as lowercase hex strings: views expose `trace_id`, `span_id`, `parent_span_id` as `Utf8` via `encode(…, 'hex')`, and equality filters on hex literals are rewritten to binary comparisons before pushdown so bloom filters still apply.

Column reference for each view is generated from the Arrow schemas and served at `GET /api/v1/projects/{p}/schema` (used by the UI autocomplete, the docs and the MCP `get_schema` tool).

## Default exclusions

Unless the request opts in (`include_eval=true` / `include_experiments=true`, or the filter explicitly constrains `environment`), list pages, user aggregates, dashboard series and alert queries add `environment IS DISTINCT FROM 'llmobs-eval'` and exclude experiment traces (`metadata['llmobs.experiment_id'] IS NULL`). The SQL console applies **no** implicit exclusion (users see raw data; the schema docs explain the reserved environments).

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

- **Identifiers:** columns of the target view only. `metadata['k']`, `attributes['k']`, `resource_attributes['k']` map access allowed.
- **Operators:** `= != <> < <= > >= AND OR NOT IS [NOT] NULL IN (…) BETWEEN LIKE ILIKE`, arithmetic, `@>` (array contains, rewritten to `array_has_all`).
- **Literals:** strings, numbers (incl. `5e9`), booleans, `INTERVAL '…'`, array literals.
- **Functions (allow-list):** `now()`, `lower`, `upper`, `length`, `coalesce`, `array_has`, `array_length`, `date_trunc`, `abs`, plus our UDFs:
  - `score(name)` → latest numeric score of that name attached to the row's target (trace/span/session), via a semi-join on `scores`; `score_label(name)` → categorical/text value.
  - `has_score(name)` → boolean.
  - `duration_ms` is available as a derived column (`duration_ns / 1e6`) on the `spans`, `traces` and `sessions` views, for convenience.
- Anything else (subqueries, casts to unsupported types, window functions, unknown identifiers/functions) → 400 with a position-annotated error message.

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

On the `traces` view, search runs on `spans` first (semi-join: `trace_id IN (SELECT trace_id FROM spans WHERE <search> AND <time window>)`). The query is escaped (`%`, `_`, `\`). Search **requires a time window** (default: the page's time picker, max 30 days unless the user confirms "search all") because it is a scan. DataFusion's `ILIKE` on Utf8 runs on the arrow string kernels; with column projection only the needed heavy columns are read.

Later (post-MVP): Tantivy index per partition built at compaction.

## SQL console

`POST /api/v1/projects/{p}/sql` with `{query, params?, limit?, timeout_ms?}`.

### Sandbox

1. Parse with `sqlparser` (DataFusion dialect). Exactly one statement; must be `Query` (SELECT / WITH / UNION / VALUES). Reject DDL, DML, `COPY`, `SET`, `EXPLAIN ANALYZE` (plain `EXPLAIN` allowed), `CREATE`, `INSERT`.
2. Table references must resolve to the views above. No catalogs/schemas, no table functions (`read_parquet`, `read_csv` and any `TableFunction` are not registered in the session).
3. Function allow-list = DataFusion scalar + aggregate + window built-ins minus anything doing I/O (none in core today; the list is still explicit), plus our UDFs.
4. Session config: memory pool `query.max_memory`, `datafusion.execution.batch_size = 8192`, timeout via `tokio::time::timeout` + cancellation of the stream.
5. Results: Arrow → JSON rows (UI, max 10 000 rows, `truncated: true` beyond) or streamed CSV/Parquet/NDJSON for export (`Accept` header; max 10M rows).

### Parameters

`{name:Type}` placeholders (as in Laminar/ClickHouse) are rewritten to DataFusion `$n` placeholders with typed values; used by the UI's saved queries and the MCP tool.

### Examples shipped in the UI

- Cost per model per day.
- p50/p95/p99 latency of LLM spans per model.
- Traces with the most tool calls.
- Average judge score per release.
- Users by spend.

## Dashboard metrics (fixed dashboard)

`GET /api/v1/projects/{p}/metrics?from&to&bucket=auto&filter=<dsl>&group_by=model|environment|release`

Returns time series (bucket = `date_bin` over `start_time`, auto-sized for ~120 points):

| Series | Query (sketch) |
|---|---|
| traces | `count(*)` on `traces` |
| errors | `sum(error_count > 0)` on `traces` |
| error rate | errors / traces |
| latency p50/p95/p99 (traces) | `approx_percentile_cont(duration_ns, q)` on `traces` |
| LLM latency p50/p95 by model | on `spans WHERE span_kind='llm'` |
| tokens (input/output/cache) | sums on `spans WHERE span_kind IN ('llm','embedding')` |
| cost | `sum(cost_total)` on `traces` (total) and on spans grouped by model |
| top models by cost | spans grouped by model |
| scores | `avg(value_num)` per score name per bucket on `scores` |
| evaluation cost | `sum(cost_total)` on `traces WHERE environment = 'llmobs-eval'` |
| TTFT p50/p95 | spans `ttft_ns` |

`group_by` splits the series that are not already split (traces, errors, error rate, trace latency, total cost, tokens, TTFT) by the group value; the response's `group` field carries it. With `group_by=model`, a trace-based series counts each trace once under every model in its `models` list. Series that are inherently split carry their own key in `group`: the model for LLM latency and cost by model, the score name for score averages.

All series honor the filter DSL (applied to the view the series is computed on; filters on trace columns are applied to spans through a semi-join on `trace_id`). Results are cached for 10 s per `(project, params)`.

## Trace and span APIs

| Operation | Plan |
|---|---|
| Trace list | `traces` view, filter, sort `start_time DESC` (or by `cost_total`, `duration`, `total_tokens`), cursor `(sort_value, trace_id)`; fast path from 03 §6 |
| Trace detail | `spans WHERE trace_id = X`, light columns only for the tree; heavy columns fetched per span on demand (`GET …/spans/{span_id}`) |
| Session detail | `traces WHERE session_id = X ORDER BY start_time`, then root input/output per trace for the chat view |
| Span list | `spans` view with filter (e.g. all LLM calls for model X) |
| User list | `SELECT user_id, count(*), sum(cost_total), max(end_time) … FROM traces GROUP BY user_id` |

Trace tree assembly happens in Rust after the scan (parent/child map); spans whose parent is missing, or that sit in a parent cycle, are emitted as roots with `detached: true`, in pre-order by `(start_time, span_id)` (09 § Response shapes).
