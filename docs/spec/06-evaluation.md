# 06 — Evaluation: scores, evaluators, online rules, datasets, experiments

## Scores

Data model in 01; written through:

| Path | `source` |
|---|---|
| `POST /api/v1/projects/{p}/scores` (batch ≤ 1000) with `sk` key or PAT | `api` (or `sdk` when the SDK sets it) |
| UI annotation panel (human, with score definitions as controls) | `human`, `author_user_id` set |
| `POST /api/public/feedback` with `pk` key | `end_user` |
| Online rules / experiment server evaluators | `llm_judge` \| `check` \| `webhook` |

Scores go through a WAL + memtable exactly like spans (03), so they are durable on `200` and visible in seconds. Update = write a new row with the same `score_id` and later `updated_at`; delete = row with `deleted = true`.

**Deterministic score IDs** (one row per annotator, as Opik's `authored_feedback_scores`): when the client does not provide `score_id`, the server derives it as a UUIDv8 from `hash(project_id, target, trace_id, span_id|session_id, name, source, author)` where `author` = `author_user_id` for `human`, `rule_id` for online judges, `experiment_id + dataset_item_id` for experiments, the key id for `api`/`end_user`. Consequences: re-annotating replaces your own score instead of adding a duplicate; two reviewers scoring the same trace keep two rows; a backfill re-run overwrites instead of duplicating. Aggregations (`score()` in the DSL, dashboards) average across authors.

Validation: `data_type` ∈ {numeric, boolean, categorical, text}; numeric must be finite; boolean must be 0/1/true/false; categorical requires `value_str`; target must have the matching ID fields. If a score definition with the same name exists, values are validated against it (range, categories) — otherwise accepted as is.

**End-user feedback** (`pk` key, browser-safe): body `{trace_id, span_id?, name? = "user_feedback", value (number|boolean|string), comment?}`. CORS allowed from any origin; rate limit 10 req/s per key+IP; payload ≤ 4 KiB. The trace is not required to exist yet (feedback can arrive before the trace is flushed).

## Evaluators

Stored in SQLite `evaluators` (04). Three kinds.

### `llm_judge`

```json
{
  "connection_id": "…", "model": "gpt-5-mini",
  "prompt": "You are grading … Question: {{ input }}\nAnswer: {{ output }}\n…",
  "output": {"type": "numeric", "min": 0, "max": 1}
          | {"type": "boolean"}
          | {"type": "categorical", "labels": ["correct", "partially_correct", "incorrect"],
             "label_values": {"correct": 1, "partially_correct": 0.5, "incorrect": 0}},
  "include_reason": true,
  "temperature": 0, "max_output_tokens": 1024
}
```

- Prompt templating: `minijinja` with the variables of the mapping (below); missing variables are an error at job time (`failed: missing variable x`).
- Structured output per adapter: OpenAI-compatible `response_format: {type: json_schema, strict: true}`; Anthropic forced tool call (`tool_choice: {type: tool}`) with the schema as `input_schema`; Gemini `responseSchema`. Schema = `{"reason": string?, "value"|"label": …}` (reason first so the model reasons before answering). Fallback for providers without structured output: extract the first JSON object from text.
- Result → score: numeric `value_num`; boolean 0/1; categorical `value_str` = label and `value_num` = `label_values[label]` when provided; `reason`.
- **Judge traces** (same pattern as Langfuse, Opik, Phoenix): every evaluator execution (judge, check or webhook) is traced into the **user's project** through the internal ingestion path (spans built in-process and handed to the WAL, no HTTP), with:
  - reserved `environment = "llmobs-eval"`;
  - a root `evaluator` span named after the evaluator, with child `llm` span(s) for judge calls (full prompt, response, tokens, cost) or an `http` span for webhooks;
  - metadata `llmobs.eval.target_trace_id`, `llmobs.eval.target_span_id`, `llmobs.eval.rule_id` | `llmobs.eval.experiment_id`, `llmobs.eval.evaluator_id`.
- The score links back: `metadata['judge.trace_id']`, plus the usage summary `judge.model`, `judge.input_tokens`, `judge.output_tokens`, `judge.cost_usd`, `judge.latency_ms` (so score lists show cost without opening the trace). `eval_jobs.cost_usd` and `usage_daily.judge_*` are updated too.
- **Isolation of `llmobs-eval` traces:** never selected by online rules (no recursion); excluded by default from trace/span/session lists, user aggregates, the dashboard and alert queries (05, 07); visible with the "Show evaluation traces" toggle or an explicit `environment = 'llmobs-eval'` filter; the dashboard has a separate "Evaluation cost" series. The environment name is reserved: ingestion rejects it from external sources (rewritten to `llmobs-eval-external` with a warning attribute).
- UI: "View judge trace" link on every evaluator-produced score; the evaluator test panel shows the same trace inline.

### `check` (built-in, pure Rust)

| `check` | Params | Score |
|---|---|---|
| `exact_match` | `case_sensitive`, `trim` | boolean |
| `contains` | `value` or `values[]`, `mode: any\|all`, `case_sensitive` | boolean |
| `regex` | `pattern` | boolean (match found) |
| `json_valid` | — | boolean |
| `json_schema` | `schema` | boolean, reason = first validation error |
| `levenshtein` | `normalize: true` | numeric 0–1 similarity |

Checks compare `output` against `expected` (or the param value). Text extraction rule: if a variable is a messages array, use the concatenated text of the last assistant message.

### `webhook`

```json
{"url": "https://…", "secret_enc": "…", "timeout_ms": 10000, "headers": {…}}
```

Request: `POST url` with JSON `{evaluator: {id, name}, project_id, trace_id, span_id, variables: {…}}`, header `X-Llmobs-Signature: t=<unix>,v1=<hex HMAC-SHA256(secret, t + "." + body)>`. Response `200` JSON `{value?: number|boolean, label?: string, reason?: string}`; anything else = failure (retried).

## Variable mapping

Online rules and experiment evaluators map template variables to data:

| Source expression | Meaning |
|---|---|
| `trace.input` / `trace.output` | root span (or first `llm` span if no root) input / output |
| `span.input` / `span.output` | the target span (span rules) |
| `span[name="retrieve"].output` | first span in the trace with that name (trace rules) |
| `trace.metadata.<key>`, `span.attributes.<key>` | metadata / attribute values |
| `trace.transcript` | compact rendering of the whole trace: tree of `kind name: input → output` lines, each payload truncated to 2 000 chars, total capped at 60 000 chars — for agent-trajectory judges |
| `expected` | dataset item `expected_output` (experiments only) |
| `item.input`, `item.metadata.<k>` | dataset item fields (experiments only) |

Any expression may end with a JSONPath suffix: `trace.output$.parts[0].content`. Implemented with `serde_json_path`.

## Built-in judge templates

Shipped as code (`llmobs-eval/templates/*.yaml`); "create from template" copies the prompt into a new evaluator the user can edit.

| Template | Variables | Output |
|---|---|---|
| Hallucination | `input`, `output`, `context` (default mapping: `span[kind="retriever"].output`, optional) | boolean (true = hallucinated) |
| Relevance | `input`, `output` | numeric 0–1 |
| Toxicity | `output` | boolean |
| Correctness | `input`, `output`, `expected` | categorical correct / partially_correct / incorrect (1 / 0.5 / 0) |
| Custom | user-defined | user-defined |

## Online rules

`online_rules` (04): `{evaluator, target: trace|span, filter (DSL), variable_mapping, sample_rate, budget_usd, budget_period, enabled}`.

### Triggering

- **Span rules:** evaluated in the ingest path after WAL commit, against the span's light columns (the filter DSL is compiled once per rule into a DataFusion `PhysicalExpr` and evaluated on the committed `RecordBatch`). Matches are sampled and enqueued.
- **Trace rules:** need the trace to be complete. The **trace-completion detector** tracks traces of projects that have at least one enabled trace rule:
  - on every committed batch: upsert `pending_traces(project, trace_id, last_ingest_at, root_ended |= batch contains the root span)` (one SQLite transaction per batch);
  - a sweeper (every 2 s) completes traces where `(root_ended AND now − last_ingest_at ≥ 5 s)` or `now − last_ingest_at ≥ project.settings.trace_completion_timeout_s` (default 60 s);
  - on completion: load the merged trace row, evaluate each trace rule's filter, sample, enqueue, delete the pending row.
- Spans/traces belonging to experiments (`metadata['llmobs.experiment_id']` present) and evaluation traces (`environment = 'llmobs-eval'`) are skipped by online rules. The check happens before the rule filter, so a user filter cannot override it.

**Sampling** is deterministic: `xxh3(rule_id ‖ trace_id[‖ span_id]) / 2^64 < sample_rate`, so re-processing never double-samples.

### Execution

- Worker pool (`eval.workers`, default 16 concurrent jobs). Claim: `UPDATE eval_jobs SET status='running', lease_until=now+120s, attempts=attempts+1 WHERE id IN (SELECT id FROM eval_jobs WHERE status='queued' AND not_before<=now ORDER BY not_before LIMIT ?) RETURNING *`. Expired leases are re-queued at startup and every minute.
- Job: fetch data (heavy columns via the query layer), resolve variables, run evaluator, write score, record cost, mark `done`.
- Retries: provider 429/5xx/timeouts → backoff `min(2^attempts · 5 s, 10 min)`, max 6 attempts; 4xx and parse errors → `failed` immediately.
- **Budget:** before a judge call, estimate cost (prompt chars/4 × input price + max_output_tokens × output price). If `spent + estimate > budget_usd` for the current period, the rule is paused (`paused_reason='budget'`, UI banner, optional alert) and the job is dropped. Actual cost is added to `rule_spend` after the call. The rule resumes automatically at the next period.
- **Error breaker:** if ≥ 50 % of the last 50 jobs of a rule failed, pause with `paused_reason='errors'`.
- **Rate limits per connection:** token bucket per `llm_connection` (configurable RPM, default 300) shared by all rules.
- **Backfill:** "Run on existing data" (filter + time range, capped at 10 000 targets with a cost estimate shown first) enqueues jobs directly.

## Datasets

- Create (name, description, optional input/expected JSON Schemas).
- Items: `input` (any JSON), `expected_output` (optional JSON), `metadata`, optional `source_trace_id/source_span_id`.
- **Versions:** every mutation batch (add / edit / delete / import) creates version `current_version + 1` with a description. Reads default to the latest version; `?version=n` reads a past one.
- **Import:** CSV or JSONL upload (≤ 100 MiB), with a column mapping step: each column → `input.<key>` | `expected.<key>` | `metadata.<key>` | ignore; a single column can map to the whole `input`/`expected`. JSONL rows with `input`/`expected_output`/`metadata` keys import directly. Invalid rows are reported, valid ones imported (one version).
- **From traces:** "Add to dataset" on a trace or span copies `input` → item input and `output` → `expected_output` (editable before saving); records `source_*` (GDPR cascade, 03 §7).
- Export: JSONL / CSV of a version.

## Experiments

Client-side execution with the SDK (10); the server stores results and runs optional server evaluators.

### Protocol

1. `POST /experiments` `{name, dataset: name|id, dataset_version?, metadata, server_evaluators: [evaluator names]}` → `{id, dataset_version, items: [...]}` (items paginated if large).
2. SDK runs `task(item.input)` per item (and per repetition) under a root span named after the experiment, with `metadata['llmobs.experiment_id']`, `metadata['llmobs.dataset_item_id']`, `environment = "experiment"` (unless set). The trace is exported over OTLP as usual.
3. SDK runs local scorers → `POST /scores` with `experiment_id`, `dataset_item_id`, `trace_id`.
4. SDK `POST /experiments/{id}/items` (batches) `{dataset_item_id, repetition, trace_id, output, error, latency_ns}`.
5. SDK `POST /experiments/{id}/finish`. The server enqueues one `eval_job` per (item × server evaluator) with `not_before = now + 10 s` (lets the trace flush), variables resolved with `expected`/`item.*` available. Status becomes `completed` when all jobs are done or failed.

### Results & comparison

- Experiment page: table of items (input, expected, output, per-score values, latency, cost from the trace, link to trace); aggregates per score (mean, median, pass rate for booleans, distribution for categoricals); errors count.
- Compare 2–4 experiments on the same dataset: aggregate deltas, per-item side by side with regressions highlighted (score dropped by > threshold or boolean flipped to fail).
- API returns the same data for CI gating; the SDK exposes `result.summary()` and `result.assert_min("correctness", 0.8)` style helpers (10).
