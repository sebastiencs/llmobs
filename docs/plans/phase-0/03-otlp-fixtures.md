# Phase 0 · Step 3 — OTLP golden fixture format

Implementation plan, split into small sub-phases. Each sub-phase is done by one agent in one go and ends with `just check` green and one commit.

**Prerequisites:** step 1 merged. Step 2 (`02-core-contracts.md`) merged; the fixtures serialize `llmobs_core::Span`.

**Read first:**
- `AGENTS.md`
- `docs/spec/02-ingestion.md`: mapping tables, media, truncation, cost, and especially **§ Normalization rules (clarifications)**. Every expected value in the hand-written cases follows those rules.
- `docs/spec/01-data-model.md`: span columns, canonical message format.
- `docs/plans/phase-0/02-core-contracts.md` § C5: `Span` JSON shape, timestamps as RFC 3339 (D64).
- Decisions D64–D68 in `docs/mvp-decisions.md`.

## Why this step exists

Fixtures are the contract between three parallel tracks:
- **Track B** writes normalization (`llmobs-otel`) and must reproduce `expected.json` from `input.json`.
- **The capture tool** (`tools/capture`, Wave 1) records real instrumentations into this format.
- **The SDK tests** (Wave 2) check that our SDKs emit spans that normalize as expected.

Step 3 delivers four things:
1. the format, written down;
2. a validation test that keeps every fixture well-formed;
3. a fixed test price table;
4. **four hand-written cases**. They are written from the spec, before any normalization code exists, so they pin down our reading of the mapping tables. When Track B's code disagrees with one of them, either the code or the spec is wrong, and that gets reported. It is never fixed silently.

## Scope

**In:**
- `fixtures/otlp/README.md` (the format)
- `fixtures/otlp/prices.json`
- the validation test and its support module in `crates/otel/tests/`
- `just` recipes
- the four cases

**Out:**
- normalization code, and the golden test that runs it (Track B);
- the capture tool (Wave 1);
- SDK test harnesses (Wave 2);
- captured fixtures.

## Final layout

```
fixtures/otlp/
  README.md                     the format (F1)
  prices.json                   test price table (F1)
  genai/chat-cache-tokens/                     (F3)
    case.json  input.json  expected.json
  openinference/llm-flattened-tool-call/       (F4)
  vercel/generate-text/                        (F5)
  edge/media-truncation-rejection/             (F6)
crates/otel/tests/
  support/mod.rs                fixture loader shared with Track B's future golden test (F2)
  fixtures.rs                   validation test (F2)
```

Group directories (first level) are: `genai`, `openinference`, `openllmetry`, `vercel`, `langfuse`, `laminar`, `google_adk`, `llmobs` (spans from our own SDKs) and `edge`, for cross-cutting edge cases. Create only the directories that have cases. Case directory names are kebab-case and describe the scenario.

---

## F1 — Format document and test price table

**Goal:** `fixtures/otlp/README.md` becomes the authoritative description of the format, and `prices.json` exists.

### `fixtures/otlp/README.md`: required content

Write it as a reference document of about 120–180 lines, with the following sections and rules. They are the format; copy their substance exactly.

**1. Purpose.** One paragraph: who consumes fixtures (Track B golden test, capture tool, SDK tests), and that expected outputs encode spec 02.

**2. Layout.** The tree above, the list of group directories, and the naming rule. Every case directory contains **exactly** these three files: `case.json`, `input.json`, `expected.json`.

**3. `case.json`.** The JSON object describing the case:

| Field | Type | Required | Meaning |
|---|---|---|---|
| `description` | string | yes | One or two sentences: what the case exercises. |
| `origin` | `"hand_written"` \| `"captured"` | yes | |
| `source` | object | iff `origin = "captured"` | `{instrumentation, instrumentation_version, language, libraries: {name: version}, captured_at}`. `captured_at` is RFC 3339. |
| `spec_refs` | array of strings | yes, ≥ 1 | Spec sections pinned by this case, e.g. `"02 § Mapping tables / Span kind"`. |
| `settings` | object | no | Per-case overrides of ingest settings. Allowed keys: `max_attribute_bytes`, `media_min_bytes`, `media_max_bytes`, all integers. Unknown keys are an error. |
| `context` | object | no | Overrides of `project_id` (UUID string) and `ingest_time` (RFC 3339). |
| `notes` | string | no | Free text, e.g. which spec ambiguity the case resolves. |

**Defaults** when `settings`/`context` don't override them:
- `max_attribute_bytes = 8388608`, `media_min_bytes = 4096`, `media_max_bytes = 33554432` (spec 02 § Limits);
- `project_id = "01900000-0000-7000-8000-000000000001"`;
- `ingest_time = "2026-01-01T00:00:00.000000000Z"`.

**4. `input.json`.** One OTLP `ExportTraceServiceRequest` in the **standard OTLP/JSON encoding** (OTLP spec, "JSON Protobuf Encoding"):
- lowerCamelCase field names;
- `traceId`/`spanId`/`parentSpanId` as hex strings;
- 64-bit integers (`startTimeUnixNano`, `intValue`, …) as decimal strings;
- enum fields (`kind`, `status.code`) as integers;
- `AnyValue` objects (`{"stringValue": …}`, `{"intValue": "…"}`, `{"doubleValue": …}`, `{"boolValue": …}`, `{"arrayValue": {"values": […]}}`, `{"kvlistValue": {"values": […]}}`, `{"bytesValue": "<base64>"}`).

This is exactly what an exporter configured with `http/json` sends, and what `/v1/traces` accepts with `Content-Type: application/json`. Captured protobuf payloads are converted to this encoding by the capture tool.

**5. `expected.json`.** An object:
```json
{
  "spans": [ <llmobs_core::Span JSON>, … ],
  "media": [ {"sha256": "<64 lowercase hex>", "mime_type": "image/png", "size_bytes": 70}, … ],
  "rejected_spans": 0
}
```
- `spans`: every span the normalizer must accept, in `llmobs_core::Span` serde format (D60, D64). Timestamps are RFC 3339 with 9 fractional digits and `Z`; optional and empty fields are omitted. Sorted by `(trace_id, start_time, span_id)`.
- `media`: every blob the normalizer must extract (the bytes are not stored, only their identity). Sorted by `sha256`. Every entry is referenced by some span's `media_refs`, and every `media_refs` entry appears here.
- `rejected_spans`: number of input spans the normalizer must reject (spec 02 § Normalization rules). It must equal the number of input spans minus `spans.len()`.
- Each span's `project_id` and `ingest_time` equal the case's context (defaults or overrides).

**6. Comparison rules** (for Track B's golden test, and for anyone comparing):
- Spans are compared as parsed `Span` values, after sorting both sides by `(trace_id, start_time, span_id)`.
- `cost_input`, `cost_output` and `cost_total` are compared with an absolute tolerance of `1e-12`; every other field must be exactly equal.
- `media` is compared exactly (order included), and so is `rejected_spans`.

**7. Price table.** `fixtures/otlp/prices.json` is used **instead of** the built-in LiteLLM snapshot whenever fixtures are normalized (D67). This keeps goldens stable across snapshot refreshes. Its prices are deliberately round and **not real**. The format is an array of rows mirroring `model_prices` (spec 04 § Pricing), prices in USD per 1M tokens:
```json
[{"model_name": "…", "match_pattern": "<regex>", "provider": null, "valid_from": "1970-01-01T00:00:00.000000000Z",
  "price_input": 1.0, "price_output": 4.0, "price_cache_read": null, "price_cache_write": null, "price_reasoning": null,
  "tiers": null}]
```
`match_pattern` is matched against the span's `model`, i.e. `coalesce(response_model, request_model)`, anchored and case-insensitive (spec 02 § Cost computation).

**8. Canonical formatting.** All four JSON file types (`case.json`, `input.json`, `expected.json`, `prices.json`):
- are pretty-printed with 2-space indentation and end with exactly one `\n`;
- keep their object keys in the order produced by the canonical writer, which is `serde_json` with the workspace's `preserve_order` setting.

The validation test rejects non-canonical files. `just fixtures-fmt` rewrites them canonically.

**9. Content rules.**
- Synthetic content only: no real prompts from users, no PII, no real API keys, tokens, emails or hostnames. The validation test scans for common secret patterns.
- Keep `input.json` under 256 KiB. For media cases, use tiny blobs and lower `media_min_bytes` in `settings` instead of shipping large files.
- One scenario per case. Prefer a new case over growing an existing one.

**10. Workflows.**
- **Hand-written case:** write `case.json` and `input.json`, then write `expected.json` **by hand from spec 02**, run `just fixtures-fmt`, then `just fixtures-check`.
- **Captured case** (once the capture tool exists): the tool writes `case.json` and `input.json`. `expected.json` is generated by Track B's golden test in bless mode (`LLMOBS_BLESS=1`, defined by Track B) and **reviewed line by line** before committing.
- **Changing an expected output:** only together with the spec change or bug fix that justifies it, in the same commit.

### `fixtures/otlp/prices.json`

Exactly these rows, in this order. Prices are per 1M tokens, fictitious and round:

| `model_name` | `match_pattern` | `provider` | input | output | cache_read | cache_write | reasoning |
|---|---|---|---|---|---|---|---|
| `claude-sonnet-4-5` | `claude-sonnet-4-5(-\d{8})?` | `anthropic` | 3.0 | 15.0 | 0.3 | 3.75 | null |
| `gpt-4o-mini` | `gpt-4o-mini(-\d{4}-\d{2}-\d{2})?` | `openai` | 1.0 | 4.0 | 0.5 | null | null |
| `text-embedding-3-small` | `text-embedding-3-small` | `openai` | 0.02 | null | null | null | null |

All rows: `valid_from = "1970-01-01T00:00:00.000000000Z"`, `tiers = null`. Write the regexes JSON-escaped (`\\d`).

**Acceptance:** the README covers sections 1–10. `prices.json` is valid JSON. F2 will enforce canonical formatting, so don't spend effort on it now.

**Commit:** `fixtures: document the OTLP fixture format and add the test price table`

---

## F2 — Validation test and fixture loader

**Goal:** `cargo test -p llmobs-otel --test fixtures` validates every fixture. The loader in `tests/support/` is written so Track B's golden test can reuse it as is.

### Dependencies

Add to root `[workspace.dependencies]` at their latest versions (look them up):
- `opentelemetry-proto` with features `["gen-tonic-messages", "trace", "with-serde"]`. These are message types only; do not enable the tonic service, which Track B will add with the gRPC server.
- `prost` (the version `opentelemetry-proto` uses).
- `regex`
- `sha2`
- `base64`

Add the following to `crates/otel/Cargo.toml` **`[dev-dependencies]` only**:
- `opentelemetry-proto`, `serde`, `serde_json`, `regex`, `sha2`, `base64`
- `llmobs-core` is already a normal dependency.

Track B will move what it needs into `[dependencies]`.

**OTLP/JSON conformance check (do this first):** verify that `opentelemetry-proto`'s `with-serde` deserializes standard OTLP/JSON:
- hex `traceId`/`spanId`;
- string-encoded 64-bit integers;
- integer enums;
- lowerCamelCase field names.

Write a small test that deserializes a minimal hand-written request containing each of these. If the crate does **not** conform, stop and report; do not write a custom OTLP/JSON decoder in this step. The production `/v1/traces` JSON path depends on the same answer, so it is a decision for the coordinating session.

### `crates/otel/tests/support/mod.rs`

Public items, used by `fixtures.rs` now and by Track B later:
```rust
pub const FIXTURES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/otlp");

pub struct Case { pub group: String, pub name: String, pub dir: PathBuf,
                  pub meta: CaseMeta, pub input: ExportTraceServiceRequest, pub expected: Expected }
pub struct CaseMeta { description, origin: Origin, source: Option<Source>, spec_refs: Vec<String>,
                      settings: Settings /* defaults applied */, context: Context /* defaults applied */, notes: Option<String> }
pub enum Origin { HandWritten, Captured }
pub struct Settings { pub max_attribute_bytes: u64, pub media_min_bytes: u64, pub media_max_bytes: u64 }
pub struct Context { pub project_id: llmobs_core::ProjectId, pub ingest_time: i64 }
pub struct Expected { pub spans: Vec<llmobs_core::Span>, pub media: Vec<ExpectedMedia>, pub rejected_spans: u32 }
pub struct ExpectedMedia { pub sha256: String, pub mime_type: String, pub size_bytes: u64 }
pub struct PriceRow { /* fields of prices.json */ }

pub fn load_all_cases() -> Vec<Case>;          // panics with the case path on any load error (test code)
pub fn load_prices() -> Vec<PriceRow>;
pub fn sort_spans(spans: &mut [llmobs_core::Span]);      // (trace_id, start_time, span_id)
pub fn canonical_json(value: &serde_json::Value) -> String;  // pretty, 2 spaces, trailing '\n'
```
- `case.json` structs use `#[serde(deny_unknown_fields)]`.
- `Settings`/`Context` are deserialized as all-optional, then defaults applied. The defaults are constants in this module, matching the README.
- Time parsing reuses `llmobs_core`'s RFC 3339 handling where it is public. Otherwise parse with `chrono` and convert to ns; add `chrono` to dev-dependencies if needed.

### `crates/otel/tests/fixtures.rs`: checks

One `#[test]` per check, each iterating all cases and reporting **every** failing case before failing, not just the first:
1. **Layout:** every directory two levels under `fixtures/otlp/` contains exactly `case.json`, `input.json` and `expected.json`. Only `README.md` and `prices.json` are allowed at the top level. Group names are in the allowed list.
2. **`case.json`:** parses, `spec_refs` is non-empty, and `source` is present iff `origin = captured`.
3. **`input.json`:** parses as `ExportTraceServiceRequest` and contains at least one span.
4. **`expected.json`:**
   - parses;
   - `spans` is sorted per the rule;
   - each span's `project_id`/`ingest_time` equal the case context;
   - every expected `(trace_id, span_id)` exists in the input;
   - `spans.len() + rejected_spans == number of input spans`;
   - every expected span with `input` has `input_format`, and the same for output;
   - `span_kind` values are consistent with the payload: a span with `input_format = messages` has an `input` that parses with `llmobs_core::parse_messages`.
5. **Media:**
   - `media` is sorted, each `sha256` is 64 lowercase hex characters, and `size_bytes ≤ media_max_bytes` and `≥ media_min_bytes` of the case;
   - the set of `media` hashes equals the union of all spans' `media_refs`;
   - every `media://<sha>` URI found anywhere in a span's `input`, `output` or `system_instructions` is listed;
   - every listed blob is backed by base64 data in the input: in the raw text of `input.json`, find every run matching `[A-Za-z0-9+/]{N,}={0,2}` with `N = ceil(media_min_bytes · 4 / 3)` (this also finds base64 inside JSON-encoded attribute strings and data URIs). Decode each run that is valid base64, hash it, and check that every listed `sha256` and `size_bytes` appears among the results.
6. **Costs, recomputed** (validates hand-written arithmetic): for each expected span whose `cost_source` is `computed`, find the matching price row:
   - apply the formula from spec 02 § Cost computation, using the expected span's token fields and `model = response_model.or(request_model)`;
   - assert all three cost fields within `1e-12`.

   A span with `cost_source = none` must have no matching row (or `span_kind` not in `llm`/`embedding`) and null cost fields.
7. **Canonical formatting:** each JSON file equals `canonical_json(parsed_value)` byte for byte. With `LLMOBS_FIXTURES_FMT=1` set, the test **rewrites** non-canonical files instead of failing, and prints which files it rewrote.
8. **Secret scan** over all fixture files. Fail on matches of:
   - `sk-[A-Za-z0-9_-]{20,}`, `sk-ant-[A-Za-z0-9_-]{20,}`, `AKIA[0-9A-Z]{16}`;
   - `ghp_[A-Za-z0-9]{36}`, `xox[baprs]-[A-Za-z0-9-]{10,}`;
   - `Bearer [A-Za-z0-9._-]{20,}`;
   - email addresses other than the `@example.com` / `@example.org` domains.
9. **Prices:**
   - `prices.json` parses and every `match_pattern` compiles;
   - at least one row matches each `model` used by a `computed`-cost span;
   - no two rows with the same `provider` have patterns that both match the same model in the fixtures (ambiguity check).

The test passes with **zero cases**. F2 lands before the cases exist, so assert that the format checks still run on `prices.json` and the layout.

### `justfile` recipes

Add:
```just
# Validate OTLP fixtures
fixtures-check:
    cargo test -p llmobs-otel --test fixtures

# Rewrite OTLP fixtures in canonical JSON formatting
fixtures-fmt:
    LLMOBS_FIXTURES_FMT=1 cargo test -p llmobs-otel --test fixtures -- canonical_formatting
```
`just check` already runs all tests, including this one.

**Acceptance:**
- `just fixtures-check` passes with only the README and `prices.json` present.
- Deliberately breaking `prices.json`'s formatting makes it fail, and `just fixtures-fmt` repairs it. Do this manually; don't commit the breakage.

**Commit:** `otel: add OTLP fixture loader and validation test`

---

## Shared instructions for F3–F6 (hand-written cases)

- Write `input.json` as a **realistic** payload of the named instrumentation, as it would really be emitted: attribute names, resource attributes and scope names. Look at the instrumentation's semantic conventions (and at reference implementations' test fixtures, if useful) **without copying** any file.
- Timestamps: spans start on `2025-12-31T23:59:50Z` or later, and every span ends before `ingest_time` (`2026-01-01T00:00:00Z`).
- Trace and span IDs: random-looking, non-zero, lowercase hex (generate them with `openssl rand -hex 16` / `-hex 8`).
- Write `expected.json` **by hand**, field by field, following spec 02 including **§ Normalization rules (clarifications)**. The tables below list every non-trivial expected value. Fields not listed follow mechanically from the spec, and omitted/empty fields are not written.
- `schema_version` = `1`. `conventions` are listed in **precedence order** (spec 02).
- Run `just fixtures-fmt`, then `just fixtures-check`. The cost check (6) validates your arithmetic.
- If a value can't be derived unambiguously from the spec, **stop and report the ambiguity**. Do not pick one silently.

---

## F3 — Case `genai/chat-cache-tokens`

**Pins:** the GenAI native path, cache-token semantics (input includes cached tokens), cost with cache prices, resource → `environment`/`release`/`service_name`, the rule for keeping consumed attributes, `request_params`, previews.

**Input:** one resource, one scope, one span.

| Where | Content |
|---|---|
| Resource attributes | `service.name = "support-bot"`, `service.version = "1.4.2"`, `deployment.environment.name = "production"`, `telemetry.sdk.language = "python"` |
| Scope | name `"opentelemetry.instrumentation.anthropic"`, version `"0.40.0"` |
| Span | name `"chat claude-sonnet-4-5"`, kind CLIENT (3), no parent, status code UNSET (0); duration 2.4 s |
| Attributes | `gen_ai.operation.name = "chat"`, `gen_ai.provider.name = "anthropic"`, `gen_ai.request.model = "claude-sonnet-4-5"`, `gen_ai.response.model = "claude-sonnet-4-5-20250929"`, `gen_ai.response.id = "msg_01AbCdEf"`, `gen_ai.response.finish_reasons = ["end_turn"]` (array), `gen_ai.request.temperature = 0.2` (double), `gen_ai.request.max_tokens = 1024` (int), `gen_ai.usage.input_tokens = 1200`, `gen_ai.usage.output_tokens = 300`, `gen_ai.usage.cache_read.input_tokens = 1000`, `gen_ai.usage.cache_creation.input_tokens = 100`, `session.id = "sess-42"`, `user.id = "user-17"` |
| Payload attributes (JSON strings) | `gen_ai.system_instructions = [{"type":"text","content":"You are a helpful support assistant."}]`; `gen_ai.input.messages = [{"role":"user","parts":[{"type":"text","content":"How do I reset my password?"}]}]`; `gen_ai.output.messages = [{"role":"assistant","parts":[{"type":"text","content":"Go to Settings, then Security, then Reset password."}],"finish_reason":"end_turn"}]` |

**Expected span**, non-trivial fields:

| Field | Value |
|---|---|
| `span_kind` | `llm` |
| `status` | `unset` |
| `service_name` / `environment` / `release` | `support-bot` / `production` / `1.4.2` |
| `session_id` / `user_id` | `sess-42` / `user-17` |
| `provider` / `request_model` / `response_model` / `operation` | `anthropic` / `claude-sonnet-4-5` / `claude-sonnet-4-5-20250929` / `chat` |
| tokens | input 1200, output 300, cache_read 1000, cache_write 100, total 1500 (computed: input + output); reasoning omitted |
| cost | uncached = 1200 − 1000 − 100 = 100 → `cost_input = (100·3 + 1000·0.3 + 100·3.75)/1e6 = 0.000975`; `cost_output = 300·15/1e6 = 0.0045`; `cost_total = 0.005475`; `cost_source = computed` |
| `request_params` | `{"max_tokens":1024,"temperature":0.2}` (keys sorted, compact) |
| `input` / `input_format` | the input messages, re-serialized compactly in canonical form / `messages` |
| `output` / `output_format` | the output messages, canonical compact / `messages` |
| `system_instructions` | `[{"type":"text","content":"You are a helpful support assistant."}]` (compact) |
| `input_preview` / `output_preview` | `How do I reset my password?` / `Go to Settings, then Security, then Reset password.` |
| `attributes` | consumed **non-payload** attributes whose encoded value is ≤ 256 bytes, encoded per the map-value rule: `gen_ai.operation.name: "chat"`, `gen_ai.provider.name: "anthropic"`, `gen_ai.request.model`, `gen_ai.response.model`, `gen_ai.response.id`, `gen_ai.response.finish_reasons: "[\"end_turn\"]"`, `gen_ai.request.temperature: "0.2"`, `gen_ai.request.max_tokens: "1024"`, the four usage attributes (`"1200"`, …), `session.id`, `user.id`. The three payload attributes are **absent**. |
| `resource_attributes` | `service.version`, `deployment.environment.name`, `telemetry.sdk.language` (`service.name` removed) |
| `scope_name` / `scope_version` | from the scope |
| `conventions` | `["genai"]` |

**Commit:** `fixtures: add genai/chat-cache-tokens`

---

## F4 — Case `openinference/llm-flattened-tool-call`

**Pins:** OpenInference flattened messages → canonical messages, system message kept inside `input`, a tool call with JSON-parsed arguments, `llm.tools` → `tool_definitions`, the metadata JSON string → `metadata` map, `tag.tags`, lower-precedence payload attributes dropped.

**Input:** resource `service.name = "weather-agent"`; scope `"openinference.instrumentation.openai"` version `"0.1.30"`; one span named `"ChatCompletion"`, kind INTERNAL (1), status OK (1).

| Attributes | |
|---|---|
| kind/model | `openinference.span.kind = "LLM"`, `llm.model_name = "gpt-4o-mini-2024-07-18"`, `llm.provider = "openai"`, `llm.system = "openai"`, `llm.invocation_parameters = "{\"temperature\": 0}"` |
| input messages | `llm.input_messages.0.message.role = "system"`, `llm.input_messages.0.message.content = "You are a weather bot."`, `llm.input_messages.1.message.role = "user"`, `llm.input_messages.1.message.content = "What's the weather in Paris?"` |
| output message | `llm.output_messages.0.message.role = "assistant"`, `llm.output_messages.0.message.tool_calls.0.tool_call.id = "call_1"`, `….tool_call.function.name = "get_weather"`, `….tool_call.function.arguments = "{\"city\": \"Paris\"}"` |
| tools | `llm.tools.0.tool.json_schema = "{\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"parameters\":{\"type\":\"object\",\"properties\":{\"city\":{\"type\":\"string\"}}}}}"` |
| usage | `llm.token_count.prompt = 50`, `llm.token_count.completion = 12`, `llm.token_count.total = 62` |
| raw payloads | `input.value` = a JSON request string (~200 bytes), `input.mime_type = "application/json"`, `output.value` = a JSON response string, `output.mime_type = "application/json"` |
| context | `session.id = "sess-7"`, `user.id = "user-3"`, `metadata = "{\"customer\":\"acme\",\"plan\":\"pro\"}"`, `tag.tags = ["beta"]` (array) |

**Expected span**, non-trivial fields:

| Field | Value |
|---|---|
| `span_kind` / `status` | `llm` / `ok` |
| `provider` / `request_model` | `openai` / `gpt-4o-mini-2024-07-18` (no `response_model`) |
| `input` | `[{"role":"system","parts":[{"type":"text","content":"You are a weather bot."}]},{"role":"user","parts":[{"type":"text","content":"What's the weather in Paris?"}]}]`, format `messages`. The system message stays in `input`; `system_instructions` is absent. |
| `output` | `[{"role":"assistant","parts":[{"type":"tool_call","id":"call_1","name":"get_weather","arguments":{"city":"Paris"}}]}]`, format `messages` (arguments parsed to a JSON object) |
| `tool_definitions` | `[{"type":"function","function":{"name":"get_weather","parameters":{…}}}]` (array of the parsed schemas, compact) |
| `request_params` | `{"temperature":0}` |
| tokens / cost | 50 / 12 / total 62; `cost_input = 50·1.0/1e6 = 0.00005`, `cost_output = 12·4.0/1e6 = 0.000048`, `cost_total = 0.000098`, `computed` |
| `input_preview` / `output_preview` | `What's the weather in Paris?` / *absent* (the last message has no text part) |
| `session_id` / `user_id` / `tags` | `sess-7` / `user-3` / `["beta"]` |
| `metadata` | `{"customer": "acme", "plan": "pro"}` (strings stored verbatim) |
| `attributes` | the consumed non-payload scalars ≤ 256 bytes (`openinference.span.kind`, `llm.model_name`, `llm.provider`, `llm.system`, `llm.invocation_parameters`, token counts, `input.mime_type`, `output.mime_type`, `session.id`, `user.id`, `metadata`, `tag.tags`). **Absent:** every `llm.input_messages.*`, `llm.output_messages.*` and `llm.tools.*` attribute, `input.value`, `output.value`. |
| `conventions` | `["openinference"]` |

**Commit:** `fixtures: add openinference/llm-flattened-tool-call`

---

## F5 — Case `vercel/generate-text`

**Pins:**
- outer `ai.generateText` is a `chain` and carries **no** tokens or cost, so there is no double counting;
- `doGenerate` is an `llm`;
- Vercel and GenAI are both detected, and `conventions` follow precedence order;
- `openai.chat` → `openai`;
- `ai.telemetry.metadata.sessionId`/`userId` → context fields, not metadata;
- other telemetry metadata → `metadata`;
- `ai.prompt` → JSON input, `ai.response.text` → text output on the chain.

**Input:** resource `service.name = "next-app"`; scope `"ai"` version `"5.0.0"`; one trace, two spans.

| Span | Attributes |
|---|---|
| `ai.generateText` (root, INTERNAL, 1.1 s) | `ai.operationId = "ai.generateText"`, `operation.name = "ai.generateText"`, `ai.model.id = "gpt-4o-mini"`, `ai.model.provider = "openai.chat"`, `ai.prompt = "{\"prompt\":\"Say hi to Ada\"}"`, `ai.response.text = "Hi Ada!"`, `ai.response.finishReason = "stop"`, `ai.usage.inputTokens = 9`, `ai.usage.outputTokens = 4`, `ai.settings.maxRetries = 2`, `ai.telemetry.functionId = "greet"`, `ai.telemetry.metadata.sessionId = "s-1"`, `ai.telemetry.metadata.userId = "u-1"`, `ai.telemetry.metadata.feature = "greeting"` |
| `ai.generateText.doGenerate` (child of root, CLIENT, 1.0 s) | `ai.operationId = "ai.generateText.doGenerate"`, `operation.name = "ai.generateText.doGenerate"`, `ai.model.id = "gpt-4o-mini"`, `ai.model.provider = "openai.chat"`, `ai.prompt.messages = "[{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"Say hi to Ada\"}]}]"`, `ai.response.text = "Hi Ada!"`, `ai.response.finishReason = "stop"`, `ai.response.model = "gpt-4o-mini-2024-07-18"`, `ai.usage.inputTokens = 9`, `ai.usage.outputTokens = 4`, `gen_ai.system = "openai.chat"`, `gen_ai.request.model = "gpt-4o-mini"`, `gen_ai.response.model = "gpt-4o-mini-2024-07-18"`, `gen_ai.response.finish_reasons = ["stop"]`, `gen_ai.usage.input_tokens = 9`, `gen_ai.usage.output_tokens = 4`, the same three `ai.telemetry.metadata.*` attributes |

**Expected spans:**

| Field | `ai.generateText` | `ai.generateText.doGenerate` |
|---|---|---|
| `span_kind` | `chain` | `llm` |
| `provider` / `request_model` / `response_model` | `openai` / `gpt-4o-mini` / — | `openai` / `gpt-4o-mini` / `gpt-4o-mini-2024-07-18` |
| tokens / cost | **all absent** (not an `llm`/`embedding` span); the `ai.usage.*` attributes stay in `attributes` | 9 / 4 / total 13; `cost_input = 0.000009`, `cost_output = 0.000016`, `cost_total = 0.000025`, `computed` |
| `input` / format | `{"prompt":"Say hi to Ada"}` / `json` | `[{"role":"user","parts":[{"type":"text","content":"Say hi to Ada"}]}]` / `messages` |
| `output` / format | `Hi Ada!` / `text` | `[{"role":"assistant","parts":[{"type":"text","content":"Hi Ada!"}],"finish_reason":"stop"}]` / `messages` |
| previews | input `{"prompt":"Say hi to Ada"}`, output `Hi Ada!` | input `Say hi to Ada`, output `Hi Ada!` |
| `session_id` / `user_id` | `s-1` / `u-1` | `s-1` / `u-1` |
| `metadata` | `{"feature": "greeting"}` | `{"feature": "greeting"}` |
| `conventions` | `["vercel"]` | `["genai", "vercel"]` |

`attributes` follow the retention rule. The three `ai.telemetry.metadata.*` attributes are consumed context: they are kept only if they are scalars ≤ 256 bytes, which they are, so they are kept. `ai.prompt`, `ai.prompt.messages` and `ai.response.text` are payloads and are therefore absent.

**Commit:** `fixtures: add vercel/generate-text`

---

## F6 — Case `edge/media-truncation-rejection`

**Pins:** media extraction to a `media://` reference plus the expected media list, string truncation and its marker, an unknown model giving null cost with `cost_source = none`, and rejection of a span with an all-zero span ID.

**`case.json` settings:** `{"max_attribute_bytes": 1024, "media_min_bytes": 32}`.

**Input:** resource `service.name = "vision-app"`; scope `"llmobs.manual"` version `"0.0.0"`; one trace with **two** spans:

1. A valid GenAI span `"chat vision-x"` (root, CLIENT):
   - `gen_ai.operation.name = "chat"`, `gen_ai.provider.name = "acme"`, `gen_ai.request.model = "vision-x-1"`, `gen_ai.usage.input_tokens = 85`, `gen_ai.usage.output_tokens = 5`;
   - `gen_ai.input.messages` = one user message with a text part `"What is in this image?"` and a `blob` part `{"type":"blob","modality":"image","mime_type":"image/png","content":"<base64>"}`, using this 1×1 PNG: `iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==`;
   - `gen_ai.output.messages` = assistant text `"A single pixel."` with `finish_reason = "stop"`;
   - `app.debug_dump` = a 2000-byte ASCII string (e.g. `"x"` repeated 2000 times; it is synthetic, so no secrets).
2. An invalid span: `spanId = "0000000000000000"`, any name and attributes.

**Expected:**
- `rejected_spans = 1`. Only span 1 appears in `spans`.
- Span 1's `input`: the blob part is replaced by `{"type":"uri","modality":"image","mime_type":"image/png","uri":"media://<sha256>"}`. `<sha256>` is the SHA-256 (lowercase hex) of the **decoded** PNG bytes; compute it with `echo -n '<base64>' | base64 -d | sha256sum`. `media_refs = ["<sha256>"]`.
- `media = [{"sha256": "<sha256>", "mime_type": "image/png", "size_bytes": <decoded length>}]`.
- `attributes["app.debug_dump"]` = the first 1024 bytes of the value, followed by the suffix `…[llmobs: truncated, original 2000 bytes]`. `truncated = ["app.debug_dump"]`.
- The input and output message payloads are a few hundred bytes, well under the 1024-byte limit, so they are **not** truncated. Payloads are measured after media extraction.
- `provider = "acme"`, `request_model = "vision-x-1"`, tokens 85 / 5 / 90, every cost field absent, `cost_source = "none"`.
- `input_preview` = `What is in this image?`, `output_preview` = `A single pixel.`.
- `conventions = ["genai"]`.

**Commit:** `fixtures: add edge/media-truncation-rejection`

---

## F7 — Final review and report

1. `just check` passes, and so does `just fixtures-check`.
2. Re-read each `expected.json` against spec 02 § Mapping tables and § Normalization rules. List every place where you had to interpret the spec.
3. Confirm `fixtures/otlp/README.md` matches what the validation test enforces: same field names, defaults and rules.

**Report back:**
- the `opentelemetry-proto` version and whether its JSON support is conformant (F2);
- every interpretation or ambiguity found while writing F3–F6;
- the versions of new dependencies.

**Commit:** only if F7 changed something (`fixtures: final review fixes`).
