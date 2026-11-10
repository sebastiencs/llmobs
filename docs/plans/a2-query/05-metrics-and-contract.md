# A2 · 05 — Metrics and API contract (Q18–Q20)

Read [README.md](README.md) first: decisions D91–D101, boundaries, verified DataFusion facts, crate layout, public API.

Metrics follow spec 05 § Dashboard metrics as updated by Q1. The server's `MetricsResponse` (`crates/server/src/api/metrics.rs`) is the target shape. A2 returns plain structs with its own enums, and the server maps them one to one with an exhaustive `match`.

---

## Q18 — Metrics: buckets and trace series

**Goal:** `reads::metrics` with bucket resolution and every series computed on `traces`.

**Edit only:** new `crates/storage/src/reads/metrics.rs`, `crates/storage/src/reads/mod.rs`.

**Commit:** `Add trace metrics`

### Types

```rust
pub enum BucketWidth { M1, M5, M10, M15, M30, H1, H6, H12, D1 }   // ns() -> i64
pub enum Bucket { Auto, Fixed(BucketWidth) }
pub enum MetricsGroupBy { Model, Environment, Release }
pub struct MetricsRequest<'a> {
    pub window: TimeWindow, pub bucket: Bucket, pub filter: Option<&'a str>, pub group_by: Option<MetricsGroupBy>,
    pub include_eval: bool, pub include_experiments: bool, pub timeout: Duration,
}
// Final form after Q19; Q18 declares only the trace-series variants (see below).
pub enum SeriesName {
    Traces, Errors, ErrorRate, LatencyP50, LatencyP95, LatencyP99, Cost,
    InputTokens, OutputTokens, CacheReadTokens, CacheWriteTokens, TtftP50, TtftP95,
    LlmLatencyP50, LlmLatencyP95, CostByModel, ScoreAvg, EvalCost,
}
pub struct Point { pub time: i64, pub value: Option<f64> }
pub struct Series { pub name: SeriesName, pub group: Option<String>, pub points: Vec<Point> }
pub struct MetricsData { pub bucket_ns: i64, pub series: Vec<Series> }
pub async fn metrics(ctx: &QueryContext, request: MetricsRequest<'_>) -> Result<MetricsData, RunError>;
```

`SeriesName` mirrors the server's `MetricName` variant for variant, with the same meanings. Q18 adds only the variants it computes (`Traces`, `Errors`, `ErrorRate`, `LatencyP50/95/99`, `Cost`, `EvalCost`). Q19 adds the others, plus `Kpi`, `KpiName`, `ModelUsageRow` and the `kpis` and `top_models` fields of `MetricsData`. Nothing is declared before it is computed.

### Buckets

- Buckets are aligned to their width from the Unix epoch, UTC (spec 05 as updated by Q1). For a width `w`, `first = from − from.rem_euclid(w)` and `last = (to − 1) − (to − 1).rem_euclid(w)`; the count is `(last − first) / w + 1`. The first and last buckets may cover part of their range only: rows still come from `[from, to)`.
- `Auto`: the smallest width whose count is ≤ 120, else `D1`.
- `Fixed(w)`: a count above 2,000 gives `InvalidRequest("too many buckets; pick a wider bucket")`.
- Bucket starts are `first + k × w` for `k = 0 .. count`. Rows are bucketed with `date_bin(<width interval>, start_time, TIMESTAMP '1970-01-01T00:00:00Z')`. The output grid is filled in Rust: every bucket start gets a `Point`, with `value: None` when no row fell in it.
- Aligned buckets keep a chart stable while the time picker moves, and match the trace list filtered to one bucket (spec 11 › Dashboard).

### Trace series

- **Scoped traces:** `traces`, `time_window(window)`, `exclusions(…, references_environment)`, and the user filter on `FilterView::Traces` (`Filter::apply`, so score functions work).
- **Grouping.** With `group_by`, the group key is:
  - `Model`: one row per element of `models`, through `unnest`. A trace counts once under each of its models; traces with no model are left out.
  - `Environment` or `Release`: that column. Null keys are left out.

  Without `group_by`, there is one series per name, with `group: None`.
- **Per bucket (and group):**

  | Series | Value |
  |---|---|
  | `Traces` | `count(*)` |
  | `Errors` | `sum(CASE WHEN error_count > 0 THEN 1 ELSE 0 END)` |
  | `ErrorRate` | errors / traces |
  | `LatencyP50/95/99` | `approx_percentile_cont(duration_ns, q)` |
  | `Cost` | `sum(cost_total)` |

  A bucket with no traces has `None` everywhere.
- **`EvalCost`:** `sum(cost_total)` over the window's traces with `environment = '<names::environment::EVAL>'`, with the user filter and the experiment exclusion (per `include_experiments`), and without the eval exclusion. Never grouped.
- **Grouped output** has one `Series` per `(name, group)`. Sort the series by name in `SeriesName` order, then by group ascending.

One SQL aggregate computes all trace series per `(bucket, group)` at once. Don't run one query per series.

### Tests

The dataset traces are all on 2026-01-01 between 10:00 and 10:30 (fixture README).
1. Window `[10:00, 11:00)` with `Auto` gives `bucket_ns` = 1 min and 60 points per series. `Fixed(M10)` gives 6 points starting at 10:00, 10:10, ….
   - Window `[10:03, 10:27)` with `Fixed(M10)` gives 3 points at 10:00, 10:10, 10:20. The 10:00 point counts only traces starting in `[10:03, 10:10)`.
   - Window `[10:00, 10:30)` with `Fixed(D1)` gives one point at 2026-01-01T00:00:00Z.
2. `Fixed(M10)`, default exclusions:
   - `Traces` per bucket equals a count computed in the test from `responses/traces.json`'s start times;
   - `Errors` and `ErrorRate` likewise;
   - `Cost` equals the sum of `cost_total`, within 1e-12;
   - empty buckets have `None`.
3. Latency: in a bucket with one trace, p50 = p95 = p99 = its `duration_ns`. In a multi-trace bucket, the value lies within the bucket's min and max.
4. `group_by = Model`: every group is a model from the traces' `models`. Per bucket, the sum of `Traces` over groups equals the count of `(trace, model)` pairs.
5. `group_by = Environment` with `include_eval`: groups include `llmobs-eval`. Traces with a null environment are absent.
6. `EvalCost` sums trace 5's cost. It is unaffected by `include_eval`, and is 0 or `None` under a filter that excludes it (state which in the test).
7. A filter `status = 'error'` changes `Traces` to the error traces only. A filter with a parse error gives `RunError::Invalid`.
8. 2,001 buckets give `InvalidRequest`.

### Report

The SQL or DataFrame shape used for the one-pass aggregate. Whether `approx_percentile_cont` was exact on the test buckets.

---

## Q19 — Metrics: span and score series, KPIs, top models

**Goal:** complete `reads::metrics`.

**Edit only:** `crates/storage/src/reads/metrics.rs`.

**Commit:** `Complete dashboard metrics`

### Types added

```rust
// SeriesName gains InputTokens, OutputTokens, CacheReadTokens, CacheWriteTokens, TtftP50, TtftP95,
// LlmLatencyP50, LlmLatencyP95, CostByModel, ScoreAvg, in the order of the final enum in Q18.
pub enum KpiName { Traces, ErrorRate, LatencyP95, CostTotal, TotalTokens }
pub struct Kpi { pub name: KpiName, pub value: Option<f64>, pub previous: Option<f64> }
pub struct ModelUsageRow { pub model: String, pub llm_count: i64, pub total_tokens: Option<i64>, pub cost_total: Option<f64> }
pub struct MetricsData { pub bucket_ns: i64, pub kpis: Vec<Kpi>, pub series: Vec<Series>, pub top_models: Vec<ModelUsageRow> }
```

### Scoped trace IDs

Span and score series keep only rows whose trace passes the request's exclusions and filter (spec 05). The trace set is the scoped traces of Q18, but over `start_time ∈ [from − 1 day, to)` with `end_time ≥ from`, projected to `trace_id`. Rows are kept with a semi-join (`in_subquery` or `JoinType::LeftSemi`). If `filter` is absent and both include flags are true, skip the semi-join.

### Span series

Over `spans` with `start_time` in the window, bucketed on the span's `start_time`:

| Series | Rows | Value | Group |
|---|---|---|---|
| `InputTokens`, `OutputTokens`, `CacheReadTokens`, `CacheWriteTokens` | `span_kind IN ('llm', 'embedding')` | sum of the column | `group_by` on the span's own `model`, `environment` or `release` |
| `TtftP50`, `TtftP95` | `span_kind = 'llm'`, `ttft_ns IS NOT NULL` | `approx_percentile_cont(ttft_ns, q)` | `group_by` as above |
| `LlmLatencyP50`, `LlmLatencyP95` | `span_kind = 'llm'`, `model IS NOT NULL` | `approx_percentile_cont(duration_ns, q)` | always the model; `group_by` ignored |
| `CostByModel` | `span_kind = 'llm'`, `model IS NOT NULL` | `sum(cost_total)` | always the model |

Take the span kind strings from `llmobs_core::enums::SpanKind`.

### Score series

`ScoreAvg`:
- `avg(value_num)` per score name per bucket;
- over `scores` with `created_at` in the window, `target IN ('trace', 'span')` and `value_num IS NOT NULL`, after the semi-join on `trace_id`;
- bucketed on `created_at`; the group is the score name.

### KPIs

Five values for `[from, to)` and for the previous range `[from − (to − from), from)`. Both use the scoped traces (exclusions and filter) of their own range:

| KPI | Value |
|---|---|
| `Traces` | count |
| `ErrorRate` | errors / count |
| `LatencyP95` | `approx_percentile_cont(duration_ns, 0.95)` |
| `CostTotal` | `sum(cost_total)` |
| `TotalTokens` | `sum(total_tokens)` |

When a range has no traces, all its values are `None`. Order: as listed.

### Top models

Over `llm` spans in the window with a non-null model (scoped as above), grouped by model:
- `count(*) AS llm_count`;
- `sum(total_tokens)`, `sum(cost_total)`.

At most 10, ordered `cost_total DESC NULLS LAST, model ASC`.

### Series order

All series, sorted by `SeriesName` declaration order, then group ascending.

### Tests

1. Tokens per bucket equal sums computed in the test from `dataset/spans.json` (llm and embedding spans of visible traces).
2. `LlmLatencyP50` has one series per model present. A single-span bucket equals that span's duration.
3. `CostByModel` and `top_models` agree with the dataset sums. `top_models` is ordered and capped (a hand-built table with 12 models gives 10).
4. `ScoreAvg` per score name equals the mean of `value_num` over the live trace and span scores of visible traces in each bucket. Session-target scores are excluded.
5. KPIs:
   - `[10:00, 11:00)` gives `Traces` = the number of visible traces;
   - `ErrorRate` as computed;
   - `previous` is `None`, since no data exists on 09:00–10:00.
   - A window `[10:15, 10:30)` has a non-empty previous range.
6. The filter semi-join: a filter keeping only trace 1 restricts the token series to trace 1's spans.
7. `include_eval = false` removes trace 5's spans from every span series. `true` brings them back.

### Report

How many queries `metrics` runs in total.

---

## Q20 — API contract additions

**Goal:** the server's API contract carries what A2 introduced: `FunctionSchema.in_sql` (D97) and the new problem codes (D94). The handlers come with the server track; this only changes the contract, as Phase 0 step 4 did (D69).

**Edit only:**
- `crates/server/src/api/{schema.rs,common.rs}`;
- `crates/server/tests/snapshots/` (updated snapshots);
- `crates/server/openapi.json` and `web/src/api/schema.gen.ts`, regenerated with `just openapi`;
- `fixtures/api/static/schema.json`;
- web test or mock files only where the type checker requires the new field.

**Commit:** `Add in_sql and query problem codes to the API contract`

### Changes

- `FunctionSchema` gains `pub in_sql: bool`, documented "Whether the SQL console allows the function." `in_filter`'s doc loses "SQL allows every function".
- `ProblemCode` gains `QueryFailed` (400), `QueryTimeout` (422), `QueryTooLarge` (422), `Busy` (503), each with a doc line from spec 09. `reason_phrase` adds 422 "Unprocessable Content" and 503 "Service Unavailable".
- `fixtures/api/static/schema.json`: every function gets `"in_sql"`: `false` for `score`, `score_label`, `has_score`; `true` for the rest. Keep the field order of the Rust struct.
- Run `just openapi`, then `just check`. The examples test (`crates/server/tests/api_examples.rs`) checks that static files round-trip.

### Tests

- The existing `schema_response` snapshot is updated to include `in_sql`. Review the diff: only that field.
- A unit test maps each new `ProblemCode` to its status.
- `just check-web` passes. If a web file builds a `FunctionSchema`, add `in_sql` there.

### Report

Every web file touched. The snapshot diffs.
