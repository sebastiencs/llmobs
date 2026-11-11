# E · Webhook, judge trace, runner (E15–E17)

See [README.md](README.md). E15 adds the third evaluator kind. E16 builds the judge trace through B1's normalizer (D161). E17 ties everything into the public `EvalRunner`.

## E15 — Webhook evaluator

**Commit:** `eval: add the webhook evaluator`

**Edit only:** `crates/eval/`, the root `Cargo.toml` and `Cargo.lock` (`hmac = "0.13.0"` in the workspace and the crate; `sha2` and `hex` for the crate).

**`webhook.rs`** per D165.

```rust
pub(crate) struct WebhookCall<'a> {
    config: &'a WebhookConfig, secret: &'a Secret,
    evaluator_id: Uuid, evaluator_name: &'a str, project_id: ProjectId,
    trace_id: Option<TraceId>, span_id: Option<SpanId>,
    variables: &'a Variables, output: &'a OutputScore, now_unix_s: i64,
}
pub(crate) struct WebhookReply { pub value: ScoreValue, pub reason: Option<String>, pub status: u16,
                                 pub request_body: Vec<u8>, pub response_body: Option<Value> }
pub(crate) async fn call(client: &EgressClient, call: &WebhookCall<'_>) -> Result<WebhookReply, WebhookError>
```

- Body: `{"evaluator": {"id", "name"}, "project_id", "trace_id", "span_id", "variables"}`. Ids in their JSON form (lowercase hex for trace and span ids, `null` when absent). Serialize once; sign and send those exact bytes.
- `X-Llmobs-Signature: t=<now_unix_s>,v1=<hex HMAC-SHA256(secret, "<t>.<body>")>`. Build the header name from `llmobs_core::names` if a constant fits; otherwise add one constant in `webhook.rs` and note it in the report (D46 renames it with the rest).
- Headers: `Content-Type: application/json`, the configured headers, the signature. Timeout `timeout_ms`. Response cap 64 KiB.
- Status: `200` → parse; 408, 425, 429, 5xx → `Retryable`; 401, 403, 404, 410 → `Pause`; 3xx and other 4xx → `Permanent`. `WebhookError::Status { status, class }`.
- Parse a `200` body as a JSON object `{value?, label?, reason?}`. By `output.data_type`: `numeric` needs a finite number in `value`; `boolean` a boolean in `value`; `categorical` a string in `label` that matches one of `output.categories` (E9's label normalization; reuse the function). Anything else → `WebhookError::BadResponse { reason }` with a fixed reason (`"not a JSON object"`, `"missing value"`, `"unknown label"`, …), class `Permanent`. `reason` is cut to 8 000 characters.
- `WebhookError` never carries the URL path or query, header values, the secret or body content.

**Tests** (`tests/webhook.rs`, with the E11 mock's `/webhook` route): the request body and headers; the signature against a fixed vector (secret `whsec_test`, `t = 1700000000`, a fixed body: compute the expected hex once with `openssl dgst -sha256 -hmac` and paste it in the test with that command in a comment); each data type; each status class; a redirect is not followed; bad bodies; the response cap; the secret appears in no error or `Debug` output.

**Checks:** `just check`.

## E16 — Judge trace

**Commit:** `eval: build judge traces through the normalizer`

**Edit only:** `crates/eval/`, the root `Cargo.toml` and `Cargo.lock` (`opentelemetry-proto`, `llmobs-otel` and `sha2` for the crate).

**`trace.rs`** per D161. It builds one `ExportTraceServiceRequest` and calls `llmobs_otel::normalize`.

```rust
pub(crate) struct TraceInput<'a> {
    trace_id: TraceId, evaluator: &'a Evaluator, target: &'a EvalTarget, origin: &'a EvalOrigin,
    started_ns: i64, ended_ns: i64, variables: &'a Variables,
    result: Result<(&'a ScoreValue, Option<&'a str>), &'a EvalError>,
    child: Option<ChildSpan<'a>>,
}
pub(crate) enum ChildSpan<'a> {
    Llm { started_ns: i64, ended_ns: i64, adapter: Adapter, request_model: &'a str, response_model: Option<&'a str>,
          prompt: &'a str, output_text: Option<&'a str>, usage: Option<&'a JudgeUsage>, error: Option<&'a JudgeError> },
    Webhook { started_ns: i64, ended_ns: i64, host: &'a str, status: Option<u16>,
              request_body: &'a [u8], response_body: Option<&'a Value>, error: Option<&'a WebhookError> },
}
pub(crate) fn build(input: &TraceInput, ingest: &IngestContext, prices: &PriceTable) -> Normalized
pub(crate) fn span_id(trace_id: TraceId, index: u8) -> SpanId   // first 8 bytes of SHA-256(trace id ‖ index); never all zero
```

`EvalTarget` and `EvalOrigin` are E17's public types; define them here and let E17 re-export them.

**The OTLP request** (one resource, one scope):
- resource: `service.name` = `llmobs_core::names::environment::EVAL`;
- scope: name `llmobs-eval` (from the crate name), version = the crate version;
- **root span** (index 0, no parent): name = the evaluator's name; attributes `llmobs.span.kind = "evaluator"`, `llmobs.environment = environment::EVAL`, and `llmobs.metadata.<key>` for `metadata_key::EVAL_EVALUATOR_ID`, `EVAL_TARGET_TRACE_ID`, `EVAL_TARGET_SPAN_ID` (when set), and `EVAL_RULE_ID` or `EVAL_EXPERIMENT_ID` from the origin (none for `Test`); `llmobs.input` = the variables as JSON (`llmobs.input.format = "json"`); on success `llmobs.output` = `{"value": …, "reason": …}` (format `json`); on failure, OTel status `ERROR` with the error's `Display` (payload-free by construction) as the message;
- **`llm` child** (index 1): name `chat <request model>`; `llmobs.span.kind = "llm"`; `gen_ai.operation.name = "chat"`; `gen_ai.provider.name` (`openai`, `anthropic`, `gcp.gemini`); `gen_ai.request.model`; `gen_ai.response.model`; the `gen_ai.usage.*` token attributes; `llmobs.input` = a messages array with one user message holding the prompt, `llmobs.output` = one assistant message holding `output_text` (format `messages`); error status as above;
- **`webhook` child** (index 1): kind `span`; `server.address` = host; `http.request.method = "POST"`; `http.response.status_code`; `llmobs.input` = the request body (format `json`), `llmobs.output` = the response body (format `json`) when there is one; error status as above.

Before writing the attribute keys, read B1's `llmobs` and `genai` conventions in `crates/otel/src/conventions/` and spec 02's mapping tables, and use exactly the keys they read (metadata prefix, token keys, provider names). If one of the values above isn't read by any convention, stop and report it rather than changing `crates/otel`.

**After `normalize`:** the result must have no rejections. If it has any (it would be a bug here), log the evaluator id and the rejection counts at `warn` and return what was accepted.

**Score metadata.** `pub(crate) fn judge_metadata(trace: &Normalized, judge_trace_id: TraceId) -> (BTreeMap<String, String>, Option<f64>, bool)` reads the `llm` span: `judge.trace_id` (hex), `judge.model` (response model, else request model), `judge.input_tokens`, `judge.output_tokens`, `judge.cost_usd` (only when `cost_total` is set), `judge.latency_ms` (the llm span's duration). Keys come from `names::score_metadata_key`. It returns the cost and whether the model was unpriced (an `llm` span with tokens and no `cost_total`). For checks and webhooks only `judge.trace_id` and `judge.latency_ms` (the root's duration) are set.

**Tests:** with fixed times, the built-in price table and a model present in `crates/otel/prices/builtin.json`, `insta` snapshots of the normalized spans for: judge success, judge provider error, check success, webhook success, webhook failure. Plus: every span has `environment = "llmobs-eval"`; span ids are stable and distinct; the metadata keys are present; an unknown model gives no cost and `unpriced = true`; no rejections in any case.

**Checks:** `just check`.

## E17 — The runner

**Commit:** `eval: add the evaluation runner`

**Edit only:** `crates/eval/`.

**`runner.rs`.**

```rust
pub struct EvalRunner { client: EgressClient }
pub enum EvalOrigin { Rule(Uuid), Experiment { experiment_id: Uuid, dataset_item_id: Uuid }, Test }
pub struct EvalTarget { pub trace_id: Option<TraceId>, pub span_id: Option<SpanId> }
pub struct RunInput<'a> {
    pub evaluator: &'a Evaluator,
    pub variables: &'a Variables,
    pub connection: Option<&'a JudgeConnection>,
    pub webhook_secret: Option<&'a Secret>,
    pub target: EvalTarget,
    pub origin: EvalOrigin,
    pub judge_trace_id: TraceId,
    pub ingest: &'a IngestContext,
    pub prices: &'a PriceTable,
}
pub struct EvalRun { pub outcome: Result<EvalOutcome, EvalError>, pub judge_trace: Normalized }
pub struct EvalOutcome {
    pub value: ScoreValue, pub reason: Option<String>,
    pub metadata: BTreeMap<String, String>,   // judge.* (D161)
    pub cost_usd: Option<f64>, pub unpriced_model: bool,
}
impl EvalRunner {
    pub fn new(policy: EgressPolicy) -> Result<Self, EvalError>;
    pub async fn run(&self, input: RunInput<'_>) -> EvalRun;
}
```

- The project is `ingest.project_id`; there is no second project field.
- `run` never fails outright: every error is in `outcome`, and the judge trace is always built (the UI shows failed runs too).
- **Judge:** connection required (`EvalError::Config`, `Permanent`, when absent); compile and render the prompt (E8); in `prompt` mode the adapter appends the instruction; call (E11–E14); parse (E9); build the trace with the `llm` child.
- **Check:** `CheckInput` from the variables `output` and `expected`; `run_check` (E4); trace with no child.
- **Webhook:** secret required; `webhook::call` (E15); trace with the `webhook` child.
- Times: `SystemTime::now()` in nanoseconds around each step (a helper in `runner.rs`; the only clock in the crate).
- `tracing::debug!` once per run with the evaluator id, kind, duration and error class. Nothing else is logged.
- `EvalError` gathers `ConfigError`, `CheckError`, `MappingError`, `TemplateError`, `JudgeError`, `WebhookError` and `EgressError`; `EvalError::class() -> ErrorClass` (config, check, mapping and template errors are `Permanent`).

**`lib.rs`** re-exports exactly the public API: `Evaluator`, `EvaluatorConfig`, `JudgeConfig`, `JudgeOutput`, `Check`, `ContainsMode`, `WebhookConfig`, `OutputScore`, `Secret`, `VariableMapping`, `Expr`, `TraceData`, `ItemData`, `Variables`, `resolve`, `PromptTemplate`, `Adapter`, `StructuredOutput`, `JudgeConnection`, `JudgeUsage`, `EgressPolicy`, `EvalRunner`, `RunInput`, `EvalRun`, `EvalOutcome`, `EvalOrigin`, `EvalTarget`, `EvalError`, `ErrorClass`, and the per-area error types. Crate docs: one paragraph with the boundary diagram from the README. Everything else stays `pub(crate)`; remove `pub` from anything earlier sub-phases exposed that isn't on this list.

**Tests** (`tests/runner.rs`, with the mock): one end-to-end test per kind from `Span` literals: `resolve` → `run` → check outcome, metadata and trace; a judge without a connection; a webhook without a secret; a missing variable gives a `Permanent` error and still a trace; an experiment origin sets `EVAL_EXPERIMENT_ID`; a `Test` origin sets neither rule nor experiment; a provider 429 gives a `Retryable` outcome with a trace whose `llm` span has error status.

**Checks:** `just check`.
