# E · LLM judges (E9–E14)

See [README.md](README.md). E9 is pure parsing. E10 adds the egress client. E11–E14 add the provider adapters, tested against the in-process mock from E11. Nothing here calls a real provider (D167).

**Wire formats.** The request and response shapes below were written from the providers' public API references on 2026-11-11. Before each adapter sub-phase, check them against the current reference (OpenAI Chat Completions, Anthropic Messages and its structured outputs page, Gemini `generateContent`) and follow the reference where it differs. Report every difference.

## E9 — Judge answer schema and parser

**Commit:** `eval: parse judge answers`

**Edit only:** `crates/eval/`.

**`judge/answer.rs`** (create `judge/mod.rs` with `mod answer;` only).
- `pub(crate) fn answer_schema(output: &JudgeOutput, include_reason: bool) -> Value` per D158. Property order matters, and the workspace's `serde_json` keeps insertion order: `explanation` (`{"type": "string"}`) first when `include_reason`, then `value` (`{"type": "number"}` or `{"type": "boolean"}`) or `label` (`{"type": "string", "enum": [labels…]}`). `"required"` lists every property; `"additionalProperties": false`; no `minimum`, `maximum` or `description`.
- `pub(crate) enum RawAnswer { Object(serde_json::Map<String, Value>), Text(String) }`: what an adapter got back (a tool call's arguments, or text).
- `pub(crate) struct Answer { pub value: ScoreValue, pub explanation: Option<String> }`.
- `pub(crate) fn parse_answer(raw: RawAnswer, output: &JudgeOutput, include_reason: bool) -> Result<Answer, JudgeError>`:
  1. **Find the object.** `Object` is used as is. `Text`: trim; empty → `JudgeError::EmptyAnswer`. Try, in order: the whole text as one JSON value; the content of the first markdown fence (```` ```json ```` or ```` ``` ````); the first complete JSON object in the text (scan each `{`, run `serde_json::Deserializer::from_str(&text[i..]).into_iter::<Value>()`, take the first item that is an object). No JSON found → `NotJson`; JSON that is not an object → `NotAnObject`.
  2. **Explanation.** Field `explanation`, or `reason` when absent. A string as is; an array of strings joined with `\n`; anything else is ignored. Cut to 8 000 characters. Dropped (set to `None`) when `!include_reason`.
  3. **Value.**
     - numeric: field `value`, or `score` when absent. A JSON number, or a string that parses as `f64` after trimming. Must be finite and within `[min, max]` inclusive, else `OutOfRange { min, max }`.
     - boolean: field `value`, or `score` when absent. `true`/`false`, the strings `"true"`/`"false"` (case-insensitive, trimmed), or the numbers 1 and 0.
     - categorical: field `label`, or `value` when absent. A string; normalize (trim, lowercase, strip one trailing `.`) and compare with the normalized labels; the result is the label as configured, with `label_values.get(label)`. No match → `UnknownLabel`.
     - missing → `MissingField { field: "value" | "label" }`; wrong type → `WrongType { field }`.
- `JudgeError` (in `error.rs`) gets `EmptyAnswer`, `NotJson`, `NotAnObject`, `MissingField { field }`, `WrongType { field }`, `OutOfRange { min, max }`, `UnknownLabel`. None of them carries the answer text or the offending value.

**Tests:** `answer_schema` snapshots (`insta`) for the three output types with and without reasons; the parser over a table of inputs: plain object; object with a renamed `score`/`reason`; fenced JSON; prose before and after an object; two objects back to back (takes the first); `"0.8"`; `"TRUE"`; `1` for boolean; `" Correct. "` matches `correct`; array explanation; explanation present with `include_reason = false`; each error; and a check that no error `Display` contains the planted label or text.

**Checks:** `just check`.

## E10 — Egress client

**Commit:** `eval: add the egress client with the SSRF guard`

**Edit only:** `crates/eval/`, the root `Cargo.toml` and `Cargo.lock` (`reqwest` per README § Dependencies; `tokio` with `net` for the crate; `axum` and `tokio` as dev-dependencies).

**`egress.rs`** per D166.

```rust
pub struct EgressPolicy { pub allow_private_network: bool }
pub struct EgressClient { http: reqwest::Client, policy: EgressPolicy }
pub(crate) struct EgressRequest<'a> { url: &'a Url, headers: Vec<(HeaderName, HeaderValue)>, body: Vec<u8>, timeout: Duration, max_response_bytes: usize }
pub(crate) struct EgressResponse { pub status: StatusCode, pub body: Bytes }
impl EgressClient {
    pub fn new(policy: EgressPolicy) -> Result<Self, EgressError>;
    pub(crate) async fn post(&self, req: EgressRequest<'_>) -> Result<EgressResponse, EgressError>;
}
```

- The client: `redirect(Policy::none())`, `no_proxy()` (a proxy would bypass the address check; document it), `connect_timeout(10 s)`, rustls, `user_agent` = `llmobs_core::names::PRODUCT` + `/` + the crate version, and `dns_resolver(Arc<GuardedResolver>)`.
- `GuardedResolver` implements `reqwest::dns::Resolve`: `tokio::net::lookup_host((host, 0))`, then keeps only allowed addresses. None left → an error that becomes `EgressError::Denied { host }`. With `allow_private_network`, nothing is filtered.
- `pub(crate) fn is_denied(ip: IpAddr) -> bool`: the ranges of D166, written with `std::net` methods and explicit octet checks (no extra crate). IPv6: unwrap IPv4-mapped (`::ffff:0:0/96`) and NAT64 (`64:ff9b::/96`) addresses and check the embedded IPv4.
- `post`: accept only `http`/`https` (`EgressError::Scheme`); a literal IP host is checked with `is_denied` before sending (reqwest skips the resolver for literals); read the body in chunks and stop with `EgressError::TooLarge { limit }` past `max_response_bytes`; map reqwest errors to `Timeout`, `Connect`, `Request` without their `Display` (it contains the URL). `EgressError` names the host, never the path or query.
- E2's `WebhookConfig` URL check now uses `Url::parse` (absolute, `http`/`https`, a host, no userinfo).

**Tests:** `is_denied` over a table covering every range's first and last address, a few public addresses, mapped and NAT64 forms; with `allow_private_network = false`, `post` to `http://127.0.0.1:<mock>` and to `http://localhost:<mock>` both fail with `Denied` before any request reaches the mock (the mock counts requests); with `true`, they succeed; a `302` from the mock is returned as a `302`, not followed; the body cap; a timeout against a mock that sleeps; `ftp://` refused; `EgressError` `Display` does not contain a planted query string.

**Checks:** `just check`.

## E11 — Connections, mock provider, OpenAI adapter

**Commit:** `eval: add judge connections and the OpenAI adapter`

**Edit only:** `crates/eval/`.

**`judge/mod.rs`.**

```rust
pub enum Adapter { OpenAi, Anthropic, Gemini }            // FromStr / as_str: "openai" | "anthropic" | "gemini"
pub enum StructuredOutput { Native, Tool, Prompt }        // FromStr / as_str: "native" | "tool" | "prompt"; Default = Native
pub struct JudgeConnection {
    pub adapter: Adapter,
    pub base_url: Option<Url>,
    pub api_key: Secret,
    pub extra_headers: Vec<(String, Secret)>,
    pub structured_output: StructuredOutput,
}
pub(crate) struct JudgeCall<'a> { model: &'a str, prompt: &'a str, output: &'a JudgeOutput, include_reason: bool,
                                  temperature: Option<f64>, max_output_tokens: u32, timeout: Duration }
pub(crate) struct JudgeReply { answer: RawAnswer, output_text: String, usage: JudgeUsage, response_model: Option<String> }
pub struct JudgeUsage { pub input: u64, pub output: u64, pub cache_read: u64, pub cache_write: u64, pub reasoning: u64 }
pub(crate) async fn call(client: &EgressClient, conn: &JudgeConnection, call: &JudgeCall<'_>) -> Result<JudgeReply, JudgeError>
```

- `JudgeUsage` uses OTel semantics (input includes cache tokens, output includes reasoning), like the rest of the project.
- `output_text` is what the model returned (the text, or the tool call's arguments as compact JSON); it becomes the `llm` span's output in E16.
- `call` dispatches on `adapter`; in this sub-phase only `OpenAi` exists, so `Anthropic` and `Gemini` are added to the enum by E12 and E13 together with their arm. `structured_output` other than `Native` arrives in E14 with its arms; until then `StructuredOutput` has only `Native`.
- Model names go into URLs (Gemini) and bodies: reject a model containing `/`, `?`, `#`, whitespace or control characters, or longer than 256 bytes (`JudgeError::InvalidModel`).
- Responses are capped at 4 MiB (D159).

**`judge/http.rs`.** `pub(crate) fn provider_error(adapter, status, body: &[u8]) -> JudgeError` builds `JudgeError::Provider { status, class, provider_type, provider_code, provider_message, hint }`:
- the error object of each adapter: OpenAI `{error: {type, code, message}}`, Anthropic `{type: "error", error: {type, message}}`, Gemini `{error: {code, status, message}}`; unparsable bodies leave the three fields `None`;
- `provider_type`/`provider_code` are kept if they are at most 64 characters of `[A-Za-z0-9_.-]`; `provider_message` is cut to 500 characters and **excluded from `Display`** (D160);
- `class` per D160: 408, 425, 429, 5xx → `Retryable`, except a 429 whose code or type is `insufficient_quota` → `Pause`; 401, 402, 404 → `Pause`; 403 → `Pause` unless the type or code mentions a content filter or safety (`content_filter`, `content_policy_violation`, `PERMISSION_DENIED` stays `Pause`) → `Permanent`; other 4xx → `Permanent`.

**`error.rs`.** `pub enum ErrorClass { Retryable, Pause, Permanent }`. `EgressError` maps to `Retryable` (timeout, connect) or `Permanent` (denied, scheme, too large, request). `JudgeError::class()` covers every variant: parse errors, `Refused`, `ContentFiltered` and `Truncated` are `Permanent`.

**`judge/openai.rs`** (native mode).
- `POST {base}/chat/completions`, `base` defaulting to `https://api.openai.com/v1` (the base includes the version, as OpenAI-compatible servers expect). Headers: `Authorization: Bearer <key>`, `Content-Type: application/json`, then the extra headers.
- Body: `{model, messages: [{role: "user", content: prompt}], max_completion_tokens, temperature? (only when set), response_format: {type: "json_schema", json_schema: {name: "judge_answer", strict: true, schema}}}`.
- Response: `choices[0]`: `message.refusal` non-empty → `Refused`; `finish_reason` `"length"` → `Truncated`, `"content_filter"` → `ContentFiltered`; else `message.content` → `RawAnswer::Text`. A missing choice → `EmptyAnswer`. Usage: `prompt_tokens` → input, `completion_tokens` → output, `prompt_tokens_details.cached_tokens` → cache_read, `completion_tokens_details.reasoning_tokens` → reasoning. `model` → `response_model`.

**`tests/support/mock.rs`.** An axum app bound to `127.0.0.1:0`, started per test:
- routes `POST /v1/chat/completions`, `POST /v1/messages`, `POST /v1beta/models/{*rest}` (Gemini's `model:generateContent`) and `POST /webhook`;
- a queue of scripted replies `MockReply { status: u16, body: Value, delay: Duration }`, served in order whatever the route;
- every request recorded (`path`, headers, JSON body) for assertions;
- `mock.url()` for the base URL. Tests use `EgressPolicy { allow_private_network: true }`.

**Tests** (`tests/openai.rs`): the request body snapshot (with and without temperature); a success for each output type; refusal; `length`; `content_filter`; usage mapping including details; each status class (429, 429 `insufficient_quota`, 401, 403, 404, 400, 500, 503) with a realistic error body; the provider message is in the error but not in its `Display`; a slow mock past the timeout → `Retryable`; the extra headers are sent; the API key appears in the `Authorization` header and nowhere in the error or `Debug` output.

**Checks:** `just check`.

## E12 — Anthropic adapter

**Commit:** `eval: add the Anthropic judge adapter`

**Edit only:** `crates/eval/`.

**`judge/anthropic.rs`** (native mode).
- `POST {base}/v1/messages`, `base` defaulting to `https://api.anthropic.com`. Headers: `x-api-key`, `anthropic-version: 2023-06-01`, `Content-Type: application/json`, the extra headers.
- Body: `{model, max_tokens, messages: [{role: "user", content: prompt}], temperature?, output_config: {format: {type: "json_schema", schema}}}`. Check the structured-outputs field name and whether a beta header is still required against the current reference; use what it says and report it.
- Response: `content[]` text blocks concatenated → `RawAnswer::Text` (skip `thinking` blocks); a `tool_use` block's `input` → `RawAnswer::Object` (used by E14). `stop_reason` `"max_tokens"` → `Truncated`, `"refusal"` → `Refused`. Usage: `input_tokens + cache_read_input_tokens + cache_creation_input_tokens` → input, `output_tokens` → output, the two cache fields → cache_read / cache_write. `model` → `response_model`.
- `Adapter::Anthropic` and its `call` arm arrive here.

**Tests** (`tests/anthropic.rs`): the same list as E11, with Anthropic's error shapes (`overloaded_error` 529 → `Retryable`, `authentication_error` 401 → `Pause`, `invalid_request_error` 400 → `Permanent`) and a thinking block before the text.

**Checks:** `just check`.

## E13 — Gemini adapter

**Commit:** `eval: add the Gemini judge adapter`

**Edit only:** `crates/eval/`.

**`judge/gemini.rs`** (native mode).
- `POST {base}/v1beta/models/{model}:generateContent`, `base` defaulting to `https://generativelanguage.googleapis.com`. Headers: `x-goog-api-key`, `Content-Type: application/json`, the extra headers. The key never goes in the query string.
- Body: `{contents: [{role: "user", parts: [{text: prompt}]}], generationConfig: {maxOutputTokens, temperature?, responseMimeType: "application/json", responseJsonSchema: schema}}`.
- Response: `promptFeedback.blockReason` set → `ContentFiltered`; `candidates[0]`: `finishReason` `MAX_TOKENS` → `Truncated`; `SAFETY`, `PROHIBITED_CONTENT`, `BLOCKLIST`, `SPII`, `RECITATION` → `ContentFiltered`; text of the parts without `thought: true` → `RawAnswer::Text`; a `functionCall.args` part → `RawAnswer::Object` (E14). Usage: `promptTokenCount` → input, `candidatesTokenCount + thoughtsTokenCount` → output, `thoughtsTokenCount` → reasoning, `cachedContentTokenCount` → cache_read. `modelVersion` → `response_model`.
- Gemini error statuses: `RESOURCE_EXHAUSTED` (429) → `Retryable`; `UNAUTHENTICATED`, `PERMISSION_DENIED`, `NOT_FOUND` → `Pause`; `INVALID_ARGUMENT`, `FAILED_PRECONDITION` → `Permanent`; `UNAVAILABLE`, `INTERNAL`, `DEADLINE_EXCEEDED` → `Retryable`.

**Tests** (`tests/gemini.rs`): the same list, plus a thought part skipped, `promptFeedback` blocking, and the model in the path.

**Checks:** `just check`.

## E14 — Tool and prompt modes

**Commit:** `eval: add the tool and prompt structured-output modes`

**Edit only:** `crates/eval/`.

`StructuredOutput` gains `Tool` and `Prompt` (D157), and each adapter gets the two arms. The tool is named `submit_judgement`, with the description "Submit your judgement." and the answer schema as its parameters.

| | `tool` | `prompt` |
|---|---|---|
| OpenAI | `tools: [{type: "function", function: {name, description, parameters: schema, strict: true}}]`, `tool_choice: {type: "function", function: {name}}`; no `response_format`. Reply: `message.tool_calls[0].function.arguments` parsed as an object → `RawAnswer::Object` (unparsable → `NotJson`); no tool call → the content as `Text`. | No `response_format`, no tools; the prompt gets the instruction below. |
| Anthropic | `tools: [{name, description, input_schema: schema}]`, `tool_choice: {type: "tool", name}`; no `output_config`. Reply: the `tool_use` block's `input`; else the text. | Same rule. |
| Gemini | `tools: [{functionDeclarations: [{name, description, parametersJsonSchema: schema}]}]`, `toolConfig: {functionCallingConfig: {mode: "ANY", allowedFunctionNames: [name]}}`; no `responseMimeType`/`responseJsonSchema`. Reply: `functionCall.args`; else the text. | Same rule. |

**Prompt instruction** (appended after two newlines; one constant in `judge/mod.rs`): "Reply with one JSON object and nothing else. It must match this JSON Schema:" followed by a newline and the schema as pretty JSON.

**Hint.** A `400` (or Gemini `INVALID_ARGUMENT`) in `native` or `tool` mode sets `JudgeError::Provider.hint` to "The model may not support this structured_output mode; try \"tool\" or \"prompt\" on the connection." (shown in the UI, D157).

**Tests:** request-body snapshots for the six new combinations; tool replies parsed; a tool-mode reply with text only still parses; prompt mode parses fenced JSON; the hint on a 400 in native and tool mode and not in prompt mode.

**Checks:** `just check`.
