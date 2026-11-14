# 06 — Evaluation: scores, evaluators, online rules, datasets, experiments

## Scores

Data model in 01; written through:

| Path | `source` |
|---|---|
| `POST /api/v1/scores` (batch ≤ 1000) with `sk` key (`ingest` scope) | `sdk` (D-F-1) |
| `POST /api/v1/projects/{p}/scores` (batch ≤ 1000) with `sk` key or PAT | `api` (or `sdk` when the SDK sets it) |
| UI annotation panel (human, with score definitions as controls) | `human`, `author_user_id` set |
| `POST /api/public/feedback` with `pk` key | `end_user` |
| Online rules / experiment server evaluators | `llm_judge` \| `check` \| `webhook` |

Scores go through a WAL + memtable exactly like spans (03), so they are durable on `200` and visible in seconds. Update = write a new row with the same `score_id` and later `updated_at`; delete = row with `deleted = true`. Every version keeps the first version's `created_at` (03 §5): a UUIDv7 `score_id` takes it from the id's time, any other id from the live version, and a new id from the write time (D-G-26).

The caller decides `source` (D-G-27): a session on the project route writes `human` (with `author_user_id`); a PAT or `sk` key there writes `api`, or `sdk` when the body says so; `POST /api/v1/scores` writes `sdk`; feedback writes `end_user`. Any other `source` in a request is a `400`.

**Deterministic score IDs** (one row per annotator, as Opik's `authored_feedback_scores`): when the client does not provide `score_id`, the server derives it as a UUIDv8 (the first 16 bytes of SHA-256 over the ASCII tag `llmobs.score.v1` followed by the project id, target kind, trace id, span id or session id, name, source and author, each as a big-endian `u32` byte length then its UTF-8 bytes; D-G-28) where `author` = `author_user_id` for `human`, `rule_id` for online judges, `experiment_id + dataset_item_id` for experiments, the key or PAT id for `api`/`sdk`/`end_user`. The SDKs always send a `score_id` (a UUIDv7 per `score()` call, D-F-2), so this applies to other clients that omit it. Consequences: re-annotating replaces your own score instead of adding a duplicate; two reviewers scoring the same trace keep two rows; a backfill re-run overwrites instead of duplicating. Aggregations (`score()` in the DSL, dashboards) average across authors.

Validation: `data_type` ∈ {numeric, boolean, categorical, text}; numeric must be finite; boolean must be 0/1/true/false; categorical requires `value_str`; target must have the matching ID fields; `name`, a session target's id and a categorical label are 1–255 bytes. A batch is all-or-nothing. If a score definition with the same name exists, values are validated against it (range, categories) — otherwise accepted as is. Only live (non-archived) definitions count; definitions are cached 10 s per project (D-I-26).

**End-user feedback** (`pk` key, browser-safe): body `{trace_id, span_id?, name? = "user_feedback", value (number|boolean|string), comment?}`. A number is `numeric`, a boolean `boolean`, a string a `categorical` label; `comment` is the `reason`; the id is derived with the key as author, so a second answer for the same trace and name replaces the first. CORS allowed from any origin; rate limit 10 req/s per key+IP; payload ≤ 4 KiB. The trace is not required to exist yet (feedback can arrive before the trace is flushed).

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
  // temperature is optional and unset by default
  "max_output_tokens": 1024,
  "timeout_ms": 60000
}
```

- Prompt templating: `minijinja` with the variables of the mapping (below); missing variables are an error at job time (`failed: missing variable x`).
- Structured output is a per-connection setting, `structured_output`: `native` (default), `tool` or `prompt` (D157). `native` is OpenAI-compatible `response_format: {type: "json_schema", strict: true}`, Anthropic `output_config.format` with a JSON schema, and Gemini `responseMimeType: "application/json"` with `responseJsonSchema`. `tool` is a forced tool or function call carrying the same schema; it is not the default, because newer Claude models refuse a forced `tool_choice`. `prompt` appends an instruction and the schema to the prompt and parses the text. There is no automatic downgrade and no cached capability state. A provider that rejects the mode returns a permanent error that names the setting to change.
- The schema the model sees is an object with `explanation` (string, present only when `include_reason`) first, then `value` (number or boolean) or `label` (an enum of the labels) (D158). `additionalProperties` is false, every property is required, and there is no `minimum` or `maximum`; ranges are checked after parsing. The same parser reads every mode. It accepts a tool call's arguments, text that is one JSON object, text in a markdown fence, or the first complete JSON object in the text. It coerces `"0.8"` to 0.8 and `"true"` / `"false"` to booleans, joins an array explanation with newlines, and matches labels after trimming and case-folding. Unknown labels, out-of-range numbers, missing fields, an empty answer and a refusal are typed errors. The explanation becomes the score's `reason`. The field the model sees is never named `reasoning`.
- Requests (D159): the OpenAI-compatible adapter uses Chat Completions (`POST {base_url}/chat/completions`, default base `https://api.openai.com/v1`) with `max_completion_tokens`. Anthropic uses Messages with `max_tokens`, which is required. Gemini uses `models/{model}:generateContent` with `maxOutputTokens`. The prompt is one user message. `temperature` is optional and unset by default; the built-in templates do not set it. `max_output_tokens` defaults to 1024. Timeouts are 10 s to connect and 60 s for the whole call (`timeout_ms` on the evaluator, at most 300 s). A response over 4 MiB is an error.
- Result → score: numeric `value_num`; boolean 0/1; categorical `value_str` = label and `value_num` = `label_values[label]` when provided; `reason`.
- **Judge traces** (same pattern as Langfuse, Opik, Phoenix): every evaluator execution (judge, check or webhook, success or failure) is traced into the **user's project** through the internal ingestion path (spans built in-process and handed to the WAL, no HTTP), with:
  - reserved `environment = "llmobs-eval"`;
  - a root `evaluator` span named after the evaluator; a judge adds one `llm` span (full prompt, response, tokens, cost); a webhook adds one child of kind `span` named `webhook` (there is no `http` span kind);
  - metadata `llmobs.eval.target_trace_id`, `llmobs.eval.target_span_id`, `llmobs.eval.rule_id` | `llmobs.eval.experiment_id`, `llmobs.eval.evaluator_id`.
- The trace is built as an OTLP `ExportTraceServiceRequest` and passed through `llmobs_otel::normalize` with the caller's ingest context and price table, so previews, truncation, media extraction and cost match SDK spans (D161). The caller supplies the trace id (the job's, so a retry reuses it). Span ids are the first 8 bytes of SHA-256(trace id ‖ span index). `judge.cost_usd` is the `llm` span's `cost_total`; when the judge model has no price, the outcome says so.
- The score links back: `metadata['judge.trace_id']`, plus the usage summary `judge.model`, `judge.input_tokens`, `judge.output_tokens`, `judge.cost_usd`, `judge.latency_ms` (so score lists show cost without opening the trace). `eval_jobs.cost_usd` and `usage_daily.judge_*` are updated too; judge-trace spans are not billable units (D-I-31, D178).
- **Isolation of `llmobs-eval` traces:** never selected by online rules (no recursion); excluded by default from trace/span/session lists, user aggregates, the dashboard and alert queries (05, 07); visible with the "Show evaluation traces" toggle or an explicit `environment = 'llmobs-eval'` filter; the dashboard has a separate "Evaluation cost" series. The environment name is reserved: ingestion from external sources rewrites it to `llmobs-eval-external` with a warning attribute. The OTLP handler does the rewrite after normalization; evaluation spans are built in process and keep the name.
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

Checks compare `output` against `expected` (or the param value).

**Defaults** (D164). Text extraction: a string is used as is; a messages array yields the concatenated text parts of its last assistant message, or empty if there is none; any other JSON yields compact JSON; an absent value is an error. `exact_match` defaults to `case_sensitive` true and `trim` true. `contains` defaults to `case_sensitive` false and `mode` any; omitting both `value` and `values` compares the output with `expected`, and an empty needle string is invalid configuration. `regex` uses Rust regex syntax, searches anywhere, and rejects a pattern whose compiled size exceeds 1 MiB or that does not compile. `json_valid` requires the whole extracted text to parse as JSON; an output that is already a JSON object or array is valid. `json_schema` is Draft 2020-12; on failure the reason is the first error's instance path and keyword (for example `/answer: type`) and never the instance value. `levenshtein` is `1 − distance / max(len)` over Unicode scalar values, and 1.0 when both strings are empty; `normalize` defaults to true and then trims, case-folds and collapses whitespace runs on both sides before the distance.

### `webhook`

```json
{"url": "https://…", "secret_enc": "…", "timeout_ms": 10000, "headers": {…}}
```

Request: `POST url` with `Content-Type: application/json`, the configured headers, and JSON `{evaluator: {id, name}, project_id, trace_id, span_id, variables: {…}}`. The signature header is `X-Llmobs-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256(secret, t + "." + body)>` (D165). `timeout_ms` defaults to 10 000 and is at most 60 000. Redirects are not followed. A response body over 64 KiB is an error. A `200` whose JSON object `{value?: number|boolean, label?: string, reason?: string}` fits the output definition is a score. Any other status is classified by D160. A `200` with a body that is not that object is a permanent error.

## Variable mapping

Online rules and experiment evaluators map template variables to data:

| Source expression | Meaning |
|---|---|
| `trace.input` / `trace.output` | input / output of the trace root: the span with no parent, or the earliest such span if several; if there is no root, the first `llm` span |
| `span.input` / `span.output` | the target span (span rules) |
| `span[name="retrieve"].output` | first span with that name, by start time |
| `span[kind="retriever"].output` | first span with that kind, by start time |
| `trace.metadata.<key>`, `span.metadata.<key>`, `span.attributes.<key>` | metadata / attribute values |
| `trace.transcript` | compact rendering of the whole trace, for agent-trajectory judges (rendering below) |
| `expected` | dataset item `expected_output` (experiments only; absent without a dataset item) |
| `item.input`, `item.metadata.<k>` | dataset item fields (experiments only; absent without a dataset item) |

Any expression may end with an RFC 9535 JSONPath suffix (`trace.output$.parts[0].content`, through `serde_json_path`) and an optional trailing `?` that marks the variable optional (D162). Payloads decode by their format (`messages` and `json` as JSON, `text` as a string); metadata and attribute values are JSON-decoded. A JSONPath with no match is absent, one match is the value, and several matches are an array. An absent required variable is `missing variable <name>`. An absent optional variable renders as an empty string.

**Rendering** (D163). Templates use `minijinja` with strict undefined (a missing variable is an error, never literal text), no auto-escaping, a fuel limit of 100 000 and a template limit of 64 KiB. In a prompt, a string renders as itself, null as an empty string, a messages array as one `role: text` line per message (non-text parts as `[image]`, `[audio]` or `[file]`, tool calls as `tool_call name(arguments)`), and any other JSON as compact JSON. Templates may index into JSON values (`{{ output.answer }}`). `trace.transcript` renders a tree of `kind name: input → output` lines, two spaces of indent per level, children in start-time order. Each payload is cut to 2 000 characters with `…`, and the whole rendering is cut to 60 000 characters with a final `[… n more spans]` line. Lengths are Unicode scalar values.

## Built-in judge templates

Shipped as code in `llmobs-eval`; "create from template" copies the prompt into a new evaluator the user can edit.

| Template | Variables | Output |
|---|---|---|
| Hallucination | `input`, `output`, `context` (default mapping: `span[kind="retriever"].output?`) | boolean (true = hallucinated) |
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
- Retries: failures arrive classified by `llmobs-eval` as retryable, pause or permanent (D160). Retryable (timeout, connection error, 408, 425, 429, 5xx) uses backoff `min(2^attempts · 5 s, 10 min)`, max 6 attempts. Permanent (other 4xx, an unparsable or invalid answer, a refusal or content filter, a missing variable, a rejected mode) becomes `failed` immediately. A pause-class error is not retried.
- **Budget:** before a judge call, estimate cost (prompt chars/4 × input price + max_output_tokens × output price). If `spent + estimate > budget_usd` for the current period, the rule is paused (`paused_reason='budget'`, UI banner, optional alert) and the job is dropped. Actual cost is added to `rule_spend` after the call. The rule resumes automatically at the next period.
- **Error breaker:** a pause-class error (401, 402, 403 that is not a content filter, 404, quota exhausted) pauses the rule at once with `paused_reason='errors'`. Otherwise, if ≥ 50 % of the last 50 jobs of a rule failed, pause with `paused_reason='errors'`.
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

## Library

`llmobs-eval` implements evaluator execution, variable mapping, prompt templating and judge traces, and it has no database (D155). Given an evaluator, its variables or trace data, and a connection, it returns a score value and the normalized spans of the judge trace. The server track owns the job queue, the evaluation tables, secret encryption and the API handlers (D156).
