# E · Evaluation library (`crates/eval`)

Track E of Wave 1 builds **`llmobs-eval`**, the part of spec 06 that needs no database. For one evaluator and its input, it produces a score value and the spans that trace the evaluation. It covers:
- the evaluator configuration types and their validation;
- the six built-in checks;
- variable mapping (`trace.*`, `span.*`, `span[name=…]`, metadata, attributes, `trace.transcript`, `expected`, `item.*`, JSONPath suffixes);
- prompt templating with `minijinja`;
- LLM judges over the OpenAI-compatible, Anthropic and Gemini APIs, with structured output and a tolerant answer parser;
- the webhook evaluator (HMAC signature, response parsing);
- the egress HTTP client with spec 12's SSRF guard;
- the judge trace (`environment = "llmobs-eval"`) and the score fields with `judge.*` metadata;
- the five built-in judge templates, with prompts written for this project.

**Out of scope (Wave 2, the eval server track):** the `eval_jobs` queue and its retries, online rules, sampling, budgets, rate limits per connection, datasets, experiments, the encryption of secrets, the `evaluators`, `llm_connections` and `instance_settings` tables, API handlers and the UI. That track calls this crate.

The plan has 20 sub-phases, **E1–E20**. Each is done by one agent, in order, and ends with `just check` green and one commit.

**Read first, for every sub-phase:**
- `AGENTS.md`;
- `docs/spec/06-evaluation.md` (after E1, with this plan's decisions);
- `docs/research/eval/README.md`;
- this README;
- the sub-phase's own section.

## Files

| File | Sub-phases |
|---|---|
| [01-foundation.md](01-foundation.md) | E1 decisions and spec · E2 crate and evaluator configuration · E3 text checks · E4 JSON and similarity checks |
| [02-variables.md](02-variables.md) | E5 mapping expressions · E6 transcript and value rendering · E7 resolution over a trace · E8 templating |
| [03-judges.md](03-judges.md) | E9 judge answer schema and parser · E10 egress client · E11 connections, mock provider, OpenAI adapter · E12 Anthropic adapter · E13 Gemini adapter · E14 tool and prompt modes |
| [04-webhook-trace-runner.md](04-webhook-trace-runner.md) | E15 webhook evaluator · E16 judge trace · E17 the runner |
| [05-templates-live-review.md](05-templates-live-review.md) | E18 built-in judge templates · E19 live provider tests · E20 review |

## Decisions

Settled with the user on 2026-11-11. E1 writes them into `docs/mvp-decisions.md`, the specs and `AGENTS.md`.

**Numbering:** `main` ends at D154 (A1 M3 took D152–D154). The C plan (PR #19) reserves D123–D134 and the D plan D135–D151, so these are **D155–D167**. E1 checks again and renumbers if needed.

| # | Decision | Source |
|---|---|---|
| D155 | **Crate `llmobs-eval`** in `crates/eval`. It depends on `llmobs-core` and `llmobs-otel`, never on `llmobs-storage` or `llmobs-server`; `llmobs-server` will depend on it. It is a library: given an evaluator, its variables or trace data, and a connection, it returns a score value and the normalized spans of the judge trace. It has no database and no queue. | User |
| D156 | **The database side of evaluation moves to the Wave 2 eval server track**, together with its first consumer: `LLMOBS_ENCRYPTION_KEY`, `secret.key`, the check value in `instance_settings`, AES-256-GCM with AAD `table:column:row_id`, the `llm_connections`, `evaluators` and `online_rules` tables and `rotate-encryption-key`. This changes D135's "Secret encryption is E's" and D's request R-E1. `llmobs-eval` receives decrypted values in plain structs (`JudgeConnection`, the webhook secret) and never stores them. | User |
| D157 | **Structured output.** Each connection has `structured_output`: `native` (default), `tool` or `prompt`. `native` is OpenAI-compatible `response_format: {type: "json_schema", strict: true}`, Anthropic `output_config.format` (JSON schema) and Gemini `responseMimeType: "application/json"` with `responseJsonSchema`. `tool` is a forced tool or function call carrying the same schema; newer Claude models refuse it, so it is never the default. `prompt` appends an instruction with the schema to the prompt and parses the text. The same tolerant parser reads every answer. There is no automatic downgrade and no cached capability state; a provider that rejects the mode returns a permanent error that names the setting to change. This replaces spec 06's "Anthropic forced tool call" and "fallback for providers without structured output". | User, research G1 |
| D158 | **Judge answer.** The schema the model sees is an object with `explanation` (string, present only when `include_reason`) **first**, then `value` (number or boolean) or `label` (enum of the labels). `additionalProperties: false`, every property required, no `minimum`/`maximum` (ranges are checked after parsing). The parser accepts: a tool call's arguments; text that is one JSON object; text in a markdown fence; the first complete JSON object in text. It coerces `"0.8"` to 0.8 and `"true"`/`"false"` to booleans, joins an array `explanation` with newlines, and matches labels after trimming and case-folding. Unknown labels, out-of-range numbers, missing fields, an empty answer and a refusal are typed errors. The explanation becomes the score's `reason`. The field is never named `reasoning` (thinking models treat that as a scratchpad). | Research G2 |
| D159 | **Judge requests.** The OpenAI-compatible adapter uses Chat Completions (`POST {base_url}/chat/completions`, default base `https://api.openai.com/v1`) with `max_completion_tokens`. Anthropic uses Messages with `max_tokens`, which is required. Gemini uses `models/{model}:generateContent` with `maxOutputTokens`. The prompt is sent as one user message. `temperature` is optional and unset by default (reasoning models reject anything but 1); the built-in templates do not set it. `max_output_tokens` defaults to 1024. Timeouts: 10 s to connect, 60 s for the whole call (configurable per evaluator, at most 300 s). Responses over 4 MiB are an error. | User, research G3 |
| D160 | **Errors, no retries in the library.** Each judge or webhook run makes exactly one HTTP call. Failures are classified: **retryable** (timeout, connection error, 408, 425, 429, 5xx), **pause** (401, 402, 403 that is not a content filter, 404, quota exhausted), **permanent** (other 4xx, an unparsable or invalid answer, a refusal or content filter, a missing variable, a rejected mode). Spec 06's queue applies the backoff and the pause. Error `Display` never contains prompt, answer or payload text; the provider's own error type and code are kept, and its message (at most 500 characters) is kept in a separate field for the UI, never logged. | User, research G3 |
| D161 | **Judge traces go through the normalizer.** Every run (judge, check or webhook, success or failure) builds an OTLP `ExportTraceServiceRequest` with our `llmobs.*` and `gen_ai.*` attributes and passes it to `llmobs_otel::normalize` with the caller's `IngestContext` and `PriceTable`. Previews, truncation, media extraction and cost are then computed by the same code as for SDK spans. The root is an `evaluator` span named after the evaluator; a judge adds one `llm` span; a webhook adds one `span`-kind child named `webhook`. The caller supplies the judge trace id (the job's, so a retry reuses it); span ids are the first 8 bytes of SHA-256(trace id ‖ span index). The `llm` span's `cost_total` gives `judge.cost_usd`; when the judge model has no price, the outcome says so. Rewriting a reserved `llmobs-eval` environment from external sources belongs in the server's OTLP handler, not in `normalize`, so the in-process path keeps it. | User, research G4, G8 |
| D162 | **Mapping expressions.** Grammar: `trace.input`, `trace.output`, `trace.metadata.<key>`, `trace.transcript`, `span.input`, `span.output`, `span.metadata.<key>`, `span.attributes.<key>`, `span[name="…"].<field>`, `span[kind="…"].<field>`, `expected`, `item.input`, `item.metadata.<key>`, then an optional RFC 9535 JSONPath suffix (`$…`, through `serde_json_path`) and an optional trailing `?` that marks the variable optional. `span[…]` picks the first matching span by start time. The trace root is the span without a parent (the earliest one if several); with no root, `trace.*` reads the first `llm` span. Payloads decode by their format (`messages` and `json` as JSON, `text` as a string); metadata and attribute values are JSON-decoded. A JSONPath with no match gives "absent", one match gives the value, several give an array. An absent required variable is `missing variable <name>`; an absent optional one renders as an empty string. `item.*` and `expected` exist only with a dataset item. | Research G5 |
| D163 | **Templating.** `minijinja` with strict undefined (a missing variable is an error, never literal text), no auto-escaping, a fuel limit of 100 000 and a template limit of 64 KiB. Values render as: a string as is; a messages array as one `role: text` line per message (non-text parts as `[image]`, `[audio]`, `[file]`, tool calls as `tool_call name(arguments)`); any other JSON as compact JSON; null as an empty string. Templates may index into JSON values (`{{ output.answer }}`). `trace.transcript` renders a tree of `kind name: input → output` lines, two spaces of indent per level, children in start-time order, each payload cut to 2 000 characters with `…`, the whole cut to 60 000 characters with a final `[… n more spans]` line. Characters are Unicode scalar values. | Research G5 |
| D164 | **Check semantics.** Text extraction: a string as is; a messages array gives the concatenated text parts of its last assistant message (empty if none); any other JSON gives compact JSON; absent is an error. `exact_match`: `case_sensitive` true, `trim` true. `contains`: `case_sensitive` false, `mode` any; an empty `values` list is invalid configuration. `regex`: Rust `regex` syntax, searches anywhere, 1 MiB compiled-size limit, an invalid pattern is invalid configuration. `json_valid`: the whole extracted text parses as JSON. `json_schema`: Draft 2020-12 via `jsonschema`; the reason is the first error's instance path and keyword, never the value. `levenshtein`: `1 − distance / max(len)` over characters, 1.0 for two empty strings; with `normalize` (default true), both sides are trimmed, case-folded and have whitespace runs collapsed first. Checks compare `output` with `expected`, or with the parameter value. | Research G6 |
| D165 | **Webhook evaluator.** `POST url` with the spec 06 body, `Content-Type: application/json`, the configured headers and `X-Llmobs-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256(secret, t + "." + body)>`. Timeout `timeout_ms` (default 10 000, at most 60 000). Redirects are not followed. A `200` with a JSON object `{value?, label?, reason?}` that fits the output definition is a score; any other status follows D160; a `200` with a bad body is permanent. Response bodies over 64 KiB are an error. | Spec 06, research G7 |
| D166 | **Egress client with the SSRF guard (spec 12).** One `reqwest` client for judges and webhooks: `http`/`https` only, no redirects, rustls, and a DNS resolver that drops every address in a denied range. If none is left, the call fails before connecting. Denied unless `allow_private_network_egress`: loopback, RFC 1918, CGNAT `100.64.0.0/10`, link-local, unique local `fc00::/7`, unspecified, multicast, broadcast, the `192.0.0.0/24`, `198.18.0.0/15` and documentation ranges, and IPv4-mapped or NAT64 forms of any of these. A literal IP in the URL is checked the same way. Alerts and Slack reuse this client later. | User, spec 12 |
| D167 | **Tests never call a real provider in CI.** Integration tests run an in-process `axum` mock that serves hand-written OpenAI, Anthropic and Gemini responses and records the requests it gets. Opt-in live tests (`#[ignore]`, keys from `LLMOBS_LIVE_*` environment variables) run by hand with `just eval-live` and are never part of `just check`. | User |

## Boundaries

```
Wave 2 eval server ── loads evaluator, decrypts connection/secret, fetches the trace (A2) ──▶
  llmobs_eval::resolve(mapping, TraceData, item)        → Variables
  EvalRunner::run(RunInput { evaluator, variables, connection, target, judge_trace_id, ingest, prices })
        ├─ check   → pure Rust
        ├─ webhook → EgressClient → user's URL
        └─ judge   → template → adapter → EgressClient → provider → answer parser
     → EvalRun { outcome: Result<EvalOutcome, EvalError>, judge_trace: llmobs_otel::Normalized }
Wave 2 eval server ── writes the spans (WAL) and the Score (deterministic id), updates eval_jobs ──▶
```

- **E owns:** `crates/eval/`, `docs/plans/e-eval/`, `docs/research/eval/`, the decisions above, the spec 06 sections it changes, the `eval-live` recipe in `justfile`.
- **E also edits** these files outside its directories:
  - the root `Cargo.toml` (workspace member and the dependencies below) and `Cargo.lock`;
  - `AGENTS.md` § Layout (the crate and the dependency graph) and § Commands (`just eval-live`);
  - `docs/spec/02-ingestion.md`, `docs/spec/04-metadata-db.md`, `docs/spec/12-operations.md` and `docs/spec/README.md`, only where E1 says;
  - `docs/plans/d-meta-auth/README.md`: one line under R-E1 pointing at D156 (E1).
- **Core:** nothing. E uses `Span`, `ScoreValue`, `SpanKind`, the ids, `names::{attr, metadata_key, score_metadata_key, environment}` and `message` as they are.
- **Otel (B1):** nothing to change. E calls `llmobs_otel::normalize`, `IngestContext`, `PriceTable` and `ExportTraceServiceRequest`. Its spans use the `llmobs` convention (`llmobs.span.kind`, `llmobs.input`, `llmobs.output` and their formats, `llmobs.environment`, `llmobs.metadata.*`) plus `gen_ai.provider.name`, `gen_ai.request.model`, `gen_ai.response.model` and `gen_ai.usage.*`. E16 checks that these survive normalization and asks for a change only if one doesn't.
- **Server, storage, web, SDKs:** nothing.

### Requests

These go to the coordinating session.

| # | To | Needed by | Request |
|---|---|---|---|
| R1 | D (and the coordinator) | E1 | D156 moves secret encryption, `instance_settings` and `llm_connections` from E to the Wave 2 eval server track. D's README § Requests R-E1 should point there. E1 adds that line itself, since the D plan is on `main`. |
| R2 | Wave 2 OTLP endpoint | — | Rewrite `environment = "llmobs-eval"` from external sources to `llmobs-eval-external` with a warning attribute in the OTLP handler, after `normalize` (D161). E1 writes this into spec 02 § Validation and spec 06. |
| R3 | Wave 2 eval server | — | Derive the deterministic score id (spec 06 § Scores), write the judge spans and the score, apply retries, pause and budgets from `EvalError::class()` and `EvalOutcome`. |

## Conventions for every sub-phase

**Module layout** (`crates/eval/src/`). New files are created by the sub-phase that first needs them.

```
lib.rs              public re-exports, crate docs                                       (E2)
error.rs            ConfigError, CheckError, MappingError, TemplateError, JudgeError,
                    WebhookError, EgressError, EvalError and ErrorClass                 (E2+)
config.rs           EvaluatorConfig, JudgeConfig, Check, WebhookConfig,
                    OutputDef, OutputScore, validate()                                  (E2)
secret.rs           Secret: a String whose Debug prints "[redacted]"                    (E2)
text.rs             text extraction (E3), value rendering (E6)
check.rs            the six checks                                                       (E3, E4)
mapping/mod.rs      Expr, VariableMapping, parse                                         (E5)
mapping/transcript.rs                                                                     (E6)
mapping/resolve.rs  TraceData, ItemData, Variables, resolve                              (E7)
template.rs         PromptTemplate: compile, undeclared variables, render                (E8)
judge/answer.rs     answer schema, tolerant parser, value mapping                        (E9)
egress.rs           EgressClient, EgressPolicy, the SSRF resolver                        (E10)
judge/mod.rs        JudgeConnection, Adapter, StructuredOutput, JudgeCall, JudgeReply    (E11)
judge/http.rs       status and provider-error classification                             (E11)
judge/openai.rs · judge/anthropic.rs · judge/gemini.rs                                   (E11–E13)
webhook.rs                                                                               (E15)
trace.rs            judge trace: OTLP request builder, span ids                          (E16)
runner.rs           EvalRunner, RunInput, EvalRun, EvalOutcome                           (E17)
templates.rs        the five built-in judge templates                                    (E18)
crates/eval/tests/support/mock.rs   axum mock provider and webhook receiver             (E11)
crates/eval/tests/live.rs           #[ignore] live tests                                 (E19)
```

Everything is `pub(crate)` unless the boundary needs it. The public API is what `lib.rs` re-exports; E17 lists it.

**Errors.** One `thiserror` enum per module, as listed above, gathered by `EvalError`. Messages name evaluator ids, variable names, expression text, HTTP status codes, provider error types and byte sizes. They never contain prompt text, answer text, variable values, payloads, URLs with query strings, header values, secrets or API keys. Every sub-phase that adds an error variant adds a test that its `Display` does not contain a planted payload string (`assert_no_payload` helper, E2).

**Async.** Only network calls are `async` (tokio). Checks, mapping, templating and parsing are plain functions.

**No `unwrap`/`expect`** outside tests unless a comment states the invariant. Nothing panics on provider, webhook or trace input.

**Tests.** Unit tests sit next to the code. Network tests live in `crates/eval/tests/` and use the mock from E11. `insta` snapshots pin request bodies sent to providers (one per adapter and mode) and the judge trace spans (E16). No test needs network access beyond `127.0.0.1`.

## Dependencies

Approved with this plan for the root `[workspace.dependencies]`. Each sub-phase adds only the ones it uses, at these versions (looked up 2026-11-11 with `cargo info`). If a version no longer resolves, take the latest patch of the same minor and say so in the report. If another track already added the same crate, reuse its entry.

| Crate | Version | Features | First used |
|---|---|---|---|
| `strsim` | 0.11.1 | | E4 |
| `serde_json_path` | 0.7.2 | | E5 |
| `minijinja` | 3.1.0 | `default-features = false`: `builtins`, `fuel`, `serde`, `json` (no `macros`, `multi_template` or `debug`: one self-contained template per evaluator) | E8 |
| `reqwest` | 0.12.28 (D's pin; already in `Cargo.lock`) | `default-features = false`: `rustls-tls`, `json` | E10 |
| `hmac` | 0.13.0 | | E15 |

Already in the workspace: `serde`, `serde_json`, `thiserror`, `regex`, `jsonschema`, `uuid`, `hex`, `sha2`, `tokio`, `tracing`, `opentelemetry-proto`, `axum`, `insta`. `reqwest`'s TLS backend must be the one D uses; if D landed with a different feature set, match it.

## Sub-phase order

E1 → E2 → … → E20, strictly in order. E12 and E13 are independent of each other. E20 must be the last, and must be done by a different model from the one that implemented E1–E19.

## Review

E20, 2026-11-12. Findings fixed here, each locked by a test:

| # | Finding | Severity | Test |
|---|---|---|---|
| R1 | `JudgeError`'s derived `Debug` printed `provider_message`, so `{err:?}` (and `EvalError`, `EvalRun`) leaked the provider's text, which D160 says is never logged. `Debug` now prints its length. | Medium | `error::class_tests::debug_omits_the_provider_message`; the adapter tests' `assert_hidden` now checks `Debug` for every variant |
| R2 | `WebhookConfig`'s derived `Debug` printed the URL path and query, header values and `secret_enc`; it reached `Evaluator`, `RunInput` and `WebhookCall`. `Debug` now prints the host, the header names and whether `secret_enc` is set. | Medium | `config::tests::debug_omits_prompts_header_values_url_paths_and_the_secret` |
| R3 | Public payload holders derived `Debug`: `Variables`, `ItemData`, `TraceData` (spans), `EvalOutcome` (reason), `EvalRun` (judge trace), `JudgeConfig` (prompt), `JudgeConnection` (`base_url` path and query). They now print names, counts, lengths and the host. | Medium | `mapping::resolve::tests::debug_prints_names_and_counts_without_values`, `judge::tests::connection_debug_shows_the_host_only`, `runner::debug_of_the_input_and_the_run_holds_no_payload` |
| R4 | A model name containing `\` escaped the Gemini path: the URL parser reads `\` as `/`, so `a\..\..\..\admin` became `/admin:generateContent` on the provider host. `\` and `%` are now rejected with `/`, `?` and `#` (03-judges.md lists only those three). | Medium | `judge::gemini::tests::rejects_a_model_that_would_leave_the_models_path`, `judge::openai::tests::rejects_model_names_that_cannot_be_sent` |
| R5 | Label uniqueness folded only trim and case, while the answer parser also strips one trailing `.`: `["ok", "ok."]` was accepted and the second label could never match. Validation now uses the parser's normalization; `"."` is blank. | Low | `config::tests` (labels cases) |
| R6 | Since E17 added `exchange`, E15's `webhook::call` only forwarded to it for tests, and `WebhookReply` duplicated `status`, `request_body` and `response_body` from `WebhookExchange` (two extra copies of up to 64 KiB). Both removed; `WebhookReply` is now `{ value, reason }`, which departs from the shape in 04-webhook-trace-runner.md. | Low | `webhook_tests` |

Property tests added (`proptest`, 256 cases, `LLMOBS_PROPTEST_CASES` overrides): each adapter's response decoder, the provider error reader, the webhook score reader and the answer parser get arbitrary bytes, arbitrary JSON, shaped replies with arbitrary leaves, truncated JSON and nesting past serde's depth limit. They found no panic and no undocumented error kind. Strategies are in `tests/hostile.rs`.

SSRF (`allow_private_network = false`): `localhost.`, `127.1`, `2130706433`, `0x7f000001`, `0.0.0.0`, `[::]`, `[::1]`, `[::ffff:127.0.0.1]` and `[64:ff9b::7f00:1]` are denied before connecting; the mock saw nothing. A zone id (`[fe80::1%25lo]`) is rejected by the URL parser. A 302 from the mock is returned as is (one request, not followed); the redirect from a public host was not live-tested. With all six `*_PROXY` variables set (in a child process), requests do not go through the proxy. No guard change was needed.

Needs a decision:
- **D166 edge ranges.** The guard allows `0.0.0.0/8` other than `0.0.0.0`, `240.0.0.0/4`, IPv4-compatible `::a.b.c.d`, `64:ff9b:1::/48` (local-use NAT64), `2002::/16` (6to4) and the deprecated `fec0::/10`. None reached the loopback mock; denying them is a one-line change each in `egress.rs`.
- **D160 quota.** Pause on exhausted quota is detected only for OpenAI's `insufficient_quota` at 429. Anthropic's low-credit error is a 400 (permanent), and Gemini's `RESOURCE_EXHAUSTED` is a 429 (retryable, the same code as a rate limit).
- **Webhook categorical output without `categories`** passes validation (E2 lists only the data types), but every response is then `unknown label`.
- **Header precedence.** A judge's `extra_headers` replace the adapter's own headers (documented), including `content-type` and the auth header. Webhook headers are checked against the reserved names only by `validate()`, so a struct built without it can override `content-type`.
- **D164 wording.** This README's D164 says an empty `values` list is invalid; spec 06 and the code compare with `expected` when both `value` and `values` are absent.

Recorded, settled by earlier sub-phases and not changed:
- Webhook status classes (E15) differ from D160: 408, 425, 429 and 5xx retry; 401, 403, 404 and 410 pause; 3xx and every other 4xx, 402 included, are permanent. Judges follow D160 (402 pause; 403 pause unless the type or code names a content filter).
- D158 extra tolerance: the `score` and `reason` aliases, 0/1 as booleans, and one trailing `.` on labels (E9).
- `anthropic-version: 2026-10-21`.
- The experiment origin writes `llmobs.eval.experiment_id`, not the dataset item id.
- The `llm` span input is the rendered template; prompt mode appends the schema inside the adapter.
- A webhook child stores the response body only when the call returns a score; its HTTP attributes stay in `span.attributes`.
- A successful run logs `error_class = none`.
- `Adapter` and `StructuredOutput` have no public `FromStr`; `from_name` and `UnknownName` are crate-private.
- The runner does not call `Evaluator::validate()`: a bad template is a `TemplateError`, a missing connection or webhook secret is `ConfigError` "invalid connection: required" / "invalid secret: required".
- The Custom template is a numeric 0–1 starter named `score`.
- `answer::scan_object` is O(n × 128) on text full of `{` (serde's depth limit), at most about 500 M steps on a 4 MiB answer. Not changed.

Also checked, no finding: every error `Display` and both `tracing` calls (ids, kinds, counts and durations only); the limits (template 64 KiB and fuel, rendered prompt 2 MiB, expression 1 KiB, 32 variables, transcript 2 000 / 60 000, explanation and webhook reason 8 000, provider message 500, model 256 bytes, 20 webhook headers, judge 4 MiB and webhook 64 KiB responses), each already enforced and tested; the five prompts against E18's rules; the public API against E17's list plus E18's `JudgeTemplate` and `builtin_templates`.
