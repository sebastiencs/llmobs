# B2 · The harness (K1–K9)

See [README.md](README.md) for the decisions, the scenario format and the rules. K1 is documentation only. K2–K9 build `llmobs-capture` without running any real instrumentation, so every behaviour here is pinned by Rust unit tests. The first real capture is K10.

## K1 — Spec and decisions

**Commit:** `docs: record the fixture capture decisions`

**Edit only:** `docs/mvp-decisions.md`, `docs/spec/02-ingestion.md`, `tools/README.md`, `docs/spec/README.md` (only if its layout block needs `tools/capture`).

1. **`docs/mvp-decisions.md`:** add rows D114–D120 with the text of this plan's decision table, one short paragraph each, in the style of the existing rows. If the numbers collide with rows already on `main`, renumber this plan's decisions to the next free numbers and update every `D11x` reference under `docs/plans/b2-capture/`.
2. **Spec 02 § Testing:** replace the "Captured cases come from real instrumentations: …" bullet with:
   - one sentence saying captured cases are recorded by `tools/capture` against a local mock provider (D114, D115);
   - the capture matrix of D119 as a short table: convention or library on one axis, and the providers or frameworks captured on the other;
   - one sentence saying captured cases may land before their `expected.json` (D118), with a link to `fixtures/otlp/README.md` § Workflows.
3. **`tools/README.md`:** a section for `tools/capture/`:
   - what it does (two sentences);
   - where the scenarios live;
   - the three recipes (`just capture <group>/<case>`, `just capture-all`, `just check-capture`), saying that the recipes and the tool arrive in K9–K10 of this plan;
   - the rule that captured `input.json` files are never edited by hand (D120);
   - a pointer to this plan.

**Checks:** `just check`.

**Report:** the decision numbers used.

## K2 — Crate and scenario model

**Commit:** `capture: add the scenario model`

**Edit only:**
- the root `Cargo.toml`: `members = ["crates/*", "tools/capture"]`;
- `tools/capture/Cargo.toml`;
- `tools/capture/src/{lib.rs,error.rs,scenario.rs}`.

**Crate.** Package `llmobs-capture`, `publish = false`, `[lints] workspace = true`, the workspace `version`/`edition`/`rust-version`. It's a library only in this sub-phase. The binary target arrives with its `main.rs` in K9, so there is never a binary that does nothing. Dependencies: `serde`, `serde_json`, `thiserror` (all workspace).

**`error.rs`.** `pub enum CaptureError` (`thiserror`). This sub-phase's variants:
- `ScenarioIo { path: PathBuf, source: std::io::Error }`;
- `ScenarioJson { path: PathBuf, source: serde_json::Error }`;
- `ScenarioInvalid { case: String, reason: String }`.

Later sub-phases add their own variants. Error messages name files, case paths, keys and indexes, never prompt or response text.

**`scenario.rs`.** Every struct uses `#[serde(deny_unknown_fields)]`.
- `pub struct CasePath { pub group: String, pub case: String }`.
  - `FromStr` parses `group/case`.
  - `group` must be one of `genai`, `openinference`, `openllmetry`, `vercel`, `langfuse`, `laminar`, `google_adk`, `edge`. That's the `fixtures/otlp/README.md` list without `llmobs`, which D119 excludes. Put the list in a `const` with a comment pointing at that README.
  - `case` must match `^[a-z0-9]+(?:-[a-z0-9]+)*$`.
  - `Display` prints `group/case`.
- `pub struct Scenario`, holding the fields of README § The scenario format:
  - `description: String`;
  - `spec_refs: Vec<String>` (non-empty);
  - `notes: Option<String>`;
  - `settings: Option<serde_json::Map<String, Value>>` (keys limited to `max_attribute_bytes`, `media_min_bytes`, `media_max_bytes`, integer values);
  - `context: Option<serde_json::Map<String, Value>>` (keys limited to `project_id`, `ingest_time`);
  - `instrumentation: String`;
  - `libraries: Vec<String>` (non-empty; must contain `instrumentation`);
  - `env: BTreeMap<String, String>` (default empty);
  - `timestamp_attributes: Vec<TimestampAttribute>` (default empty);
  - `timing_attributes: Vec<String>` (default empty);
  - `turns: Vec<Turn>` (non-empty).
- `pub struct TimestampAttribute { pub key: String, pub format: TimestampFormat }`, with `pub enum TimestampFormat { Rfc3339, UnixNano, UnixMilli }` (serde `snake_case`).
- `timing_attributes` lists attributes whose values are **measured** durations or rates (for example `ai.response.msToFirstChunk`). They're not rewritten, but `--check` (K9) ignores their values.
- `pub struct Turn`:
  - `api: Api`;
  - `model: String`;
  - `message: Option<Message>`;
  - `finish: Option<Finish>`;
  - `usage: Usage`;
  - `dims: Option<u32>`;
  - `ttft_ms: u64` (default 30);
  - `chunk_delay_ms: u64` (default 5);
  - `chunks: u32` (default 3, minimum 1).
- `pub enum Api { OpenaiChat, OpenaiResponses, OpenaiEmbeddings, AnthropicMessages, GeminiGenerateContent, GeminiEmbedContent }`. Serialized as `openai.chat`, `openai.responses`, `openai.embeddings`, `anthropic.messages`, `gemini.generate_content`, `gemini.embed_content`. `Api::provider() -> Provider`, with `pub enum Provider { OpenAi, Anthropic, Gemini }` and `Provider::host()` returning `api.openai.com`, `api.anthropic.com` or `generativelanguage.googleapis.com`.
- `pub struct Message { pub text: Option<String>, pub reasoning: Option<String>, pub tool_calls: Vec<ToolCall> }`, where `tool_calls` defaults to empty.
- `pub struct ToolCall { pub id: String, pub name: String, pub arguments: serde_json::Value }`, where `arguments` must be a JSON object.
- `pub enum Finish { Stop, ToolCalls, Length }`.
- `pub struct Usage { pub input: u64, pub output: u64, pub cache_read: u64, pub cache_write: u64, pub reasoning: u64 }`. Every field defaults to 0. They use **OTel semantics**: `input` includes `cache_read + cache_write`, and `output` includes `reasoning`.
- `impl Scenario { pub fn load(dir: &Path) -> Result<Scenario, CaptureError> }` reads `dir/scenario.json`. The case path comes from the last two path components; `Scenario` stores it as `pub case: CasePath` (`#[serde(skip)]`, filled by `load`).

**Validation** (`Scenario::validate`, called by `load`), each failing with `ScenarioInvalid` and a reason that names the field and turn index:
1. **One provider per scenario:** every turn's `api.provider()` is the same. The scrubber (K8) relies on this.
2. **Chat turns** (all APIs except the two embedding ones):
   - `message` and `finish` are present;
   - `message` has text, reasoning or at least one tool call;
   - `dims` is absent;
   - `finish` is `ToolCalls` if and only if there are tool calls.
3. **Embedding turns:**
   - `dims` is present and between 1 and 64;
   - `message` and `finish` are absent;
   - usage has only `input`.
4. **Usage consistency:**
   - `input ≥ cache_read + cache_write`;
   - `output ≥ reasoning`.
5. **What each provider can report:**
   - `openai.*`: `cache_write == 0`, and `openai.chat` has no `message.reasoning` (the Chat API never returns reasoning text);
   - `anthropic.messages`: `reasoning == 0` (Anthropic counts thinking inside `output_tokens` and reports no separate number);
   - `gemini.*`: `cache_write == 0`;
   - `gemini.embed_content`: `usage` is all zero (the API reports none).
6. **Tool call IDs** are unique within a scenario, and `name` is non-empty.
7. **`env` keys** start with `OTEL_`, `OPENINFERENCE_`, `TRACELOOP_`, `LANGFUSE_` or `LMNR_`. A value must not contain `sk-`, `pk-`, `Bearer ` or `@` (a cheap guard against secrets and emails).
8. **No duplicates** in `timestamp_attributes` keys, `timing_attributes` or `libraries`.

**Tests** (unit tests in `scenario.rs`):
- a full valid scenario round-trips;
- defaults apply;
- each validation rule has one failing case and checks the reason text;
- unknown fields are rejected;
- `CasePath` parsing, including a bad group, a bad case name and `llmobs`.

**Checks:** `just check`.

## K3 — OpenAI wire formats

**Commit:** `capture: render OpenAI responses`

**Edit only:** `tools/capture/src/lib.rs`, `tools/capture/src/wire/{mod.rs,openai.rs}`, `tools/capture/Cargo.toml` (`base64` from the workspace).

Rendering is pure: a `Turn` plus facts about the request go in, and a response comes out. The HTTP server (K5) adds the delays.

**`wire/mod.rs`:**
- `pub enum Rendered { Json(serde_json::Value), Sse(Vec<SseFrame>) }`.
- `pub struct SseFrame { pub event: Option<String>, pub data: String }`, with `pub fn encode(&self, line_end: &str) -> String` producing `event: <e>{le}` (when present), then `data: <d>{le}{le}`. Every provider uses `"\n"` except Gemini (`"\r\n"`, K4).
- `pub struct Ids { pub turn: usize }` builds deterministic IDs from the 0-based turn index `n`:
  - `chatcmpl-capture-{n+1}`, `resp_capture_{n+1}`, `msg_capture_{n+1}`, `rs_capture_{n+1}`, `fc_capture_{n+1}_{i+1}`;
  - `CREATED: i64 = 1_767_225_600` (2026-01-01T00:00:00Z) for every `created`/`created_at`.
- `pub fn split_text(text: &str, chunks: u32) -> Vec<&str>` splits into at most `chunks` non-empty pieces, at char boundaries, as evenly as possible. Tool-call argument JSON strings use the same function.

**`wire/openai.rs`.** Before writing it, read the response types of the `openai` Python package at the version K10 pins: `openai/types/chat/chat_completion*.py`, `openai/types/responses/response*.py`, `openai/types/create_embedding_response.py`. Every field those types require must be present. The shapes below are the minimum; add whatever the types require and list it in the report.

- `pub fn chat(turn, ids, stream: bool, include_usage: bool) -> Rendered`.
  - **Non-streaming**:
    ```json
    {"id","object":"chat.completion","created","model","system_fingerprint":"fp_capture",
     "choices":[{"index":0,"message":{"role":"assistant","content":<text|null>,"refusal":null,
                 "tool_calls":[{"id","type":"function","function":{"name","arguments":"<compact JSON>"}}]},
                 "logprobs":null,"finish_reason":"stop|tool_calls|length"}],
     "usage":{"prompt_tokens":input,"completion_tokens":output,"total_tokens":input+output,
              "prompt_tokens_details":{"cached_tokens":cache_read,"audio_tokens":0},
              "completion_tokens_details":{"reasoning_tokens":reasoning,"audio_tokens":0,
                 "accepted_prediction_tokens":0,"rejected_prediction_tokens":0}}}
    ```
    `tool_calls` is omitted when there are none.
  - **Streaming**: `chat.completion.chunk` frames with no event name:
    1. `{delta:{role:"assistant",content:""}}`;
    2. one frame per text piece (`{delta:{content}}`);
    3. per tool call `i`, one frame `{delta:{tool_calls:[{index:i,id,type:"function",function:{name,arguments:""}}]}}`, then one frame per argument piece (`{delta:{tool_calls:[{index:i,function:{arguments:piece}}]}}`);
    4. a frame `{delta:{}, finish_reason}`;
    5. when `include_usage`, every earlier frame carries `"usage": null` and one more frame follows with `choices: []` and the usage object;
    6. the final frame has data `[DONE]`.
- `pub fn responses(turn, ids, stream: bool) -> Rendered`.
  - **`output` items, in this order:**
    - a `reasoning` item `{type:"reasoning",id,summary:[{type:"summary_text",text}]}` when `message.reasoning` is set;
    - a `message` item `{type:"message",id,status:"completed",role:"assistant",content:[{type:"output_text",text,annotations:[]}]}` when `text` is set;
    - one `function_call` item per tool call: `{type:"function_call",id:"fc_…",call_id:<tool id>,name,arguments:"<compact JSON>",status:"completed"}`.
  - **`status`**: `completed`, or `incomplete` with `incomplete_details: {reason:"max_output_tokens"}` for `Finish::Length`.
  - **`usage`**: `{input_tokens:input, input_tokens_details:{cached_tokens:cache_read}, output_tokens:output, output_tokens_details:{reasoning_tokens:reasoning}, total_tokens}`.
  - **Streaming**: named events (`event:` equals the data's `type`), with a `sequence_number` counting from 0:
    - `response.created` and `response.in_progress` (status `in_progress`, empty `output`);
    - per item, `response.output_item.added`, then:
      - messages: `response.content_part.added`, one `response.output_text.delta` per piece, `response.output_text.done`, `response.content_part.done`;
      - function calls: one `response.function_call_arguments.delta` per piece, then `.done`;
      - reasoning: `response.reasoning_summary_part.added`, the `response.reasoning_summary_text.delta` pieces, `.done`, `response.reasoning_summary_part.done`;
    - then `response.output_item.done` for the item;
    - finally `response.completed` (or `response.incomplete`) carrying the full response.

    Check event names and required fields against `openai/types/responses/response_stream_event.py`.
- `pub fn embeddings(turn, ids, inputs: usize, base64: bool) -> Rendered`.
  - The body: `{object:"list", data:[{object:"embedding", index:i, embedding:<vector>}], model, usage:{prompt_tokens:input, total_tokens:input}}`.
  - Vector `i` has `dims` values `((i * 7 + j) % 11) as f32 / 10.0 - 0.5`.
  - With `base64` (the Python client sends `encoding_format: "base64"` by default), `embedding` is the base64 of the little-endian `f32` bytes.

**Tests:**
- golden JSON per shape, built with `serde_json::json!` in the test;
- streaming:
  - frame sequences for text only, tool calls only, text plus tool calls, `include_usage` on and off, and `Length`;
  - the concatenated deltas equal the text and the argument JSON;
  - `sequence_number`s are consecutive;
- embeddings in float and base64 form (decode the base64 back in the test);
- `split_text` on multi-byte text, `chunks` larger than the text, and `chunks = 1`.

**Checks:** `just check`.

## K4 — Anthropic and Gemini wire formats

**Commit:** `capture: render Anthropic and Gemini responses`

**Edit only:** `tools/capture/src/wire/{mod.rs,anthropic.rs,gemini.rs}`.

As in K3, first read the response types of the clients K12 pins: `anthropic/types/message.py` and `raw_message_stream_event.py`, and `google/genai/types.py` (`GenerateContentResponse`, `EmbedContentResponse`). Add any required field and report it.

**`wire/anthropic.rs`.** `pub fn messages(turn, ids, stream: bool) -> Rendered`.
- **Content blocks, in this order:**
  - `{type:"thinking",thinking:<reasoning>,signature:"capture-signature"}` when reasoning is set;
  - `{type:"text",text}` when text is set;
  - per tool call, `{type:"tool_use",id,name,input:<arguments object>}`.
- **`stop_reason`**: `end_turn`, `tool_use` or `max_tokens`. `stop_sequence: null`.
- **`usage` in Anthropic semantics**:
  - `input_tokens = input − cache_read − cache_write`;
  - `output_tokens = output`;
  - `cache_read_input_tokens = cache_read`;
  - `cache_creation_input_tokens = cache_write`.
- **Non-streaming:** `{id:"msg_capture_…",type:"message",role:"assistant",model,content,stop_reason,stop_sequence,usage}`.
- **Streaming**, named events:
  1. `message_start`, with the message's empty `content`, `stop_reason: null`, and the usage above with `output_tokens: 1` (as the real API does);
  2. per block:
     - `content_block_start`: text starts with `text:""`; thinking with `thinking:"",signature:""`; tool use with `input:{}`;
     - the deltas: `text_delta`, `thinking_delta`, then one `signature_delta` with `capture-signature`, or `input_json_delta` with `partial_json` pieces;
     - `content_block_stop`;
  3. `message_delta` with `{stop_reason, stop_sequence:null}` and `usage:{output_tokens:output}`;
  4. `message_stop`.

**`wire/gemini.rs`:**
- **`pub fn generate_content(turn, ids, stream: bool) -> Rendered`.**
  - **Parts, in this order:**
    - `{text:<reasoning>,thought:true}` when reasoning is set;
    - `{text}` when text is set;
    - `{functionCall:{id,name,args}}` per tool call.
  - **Candidate:** `{content:{role:"model",parts},finishReason:"STOP"|"MAX_TOKENS",index:0}` (`ToolCalls` → `STOP`).
  - **`usageMetadata` in Gemini semantics:**
    - `promptTokenCount = input` (it includes cached tokens);
    - `cachedContentTokenCount = cache_read`, omitted when 0;
    - `candidatesTokenCount = output − reasoning`;
    - `thoughtsTokenCount = reasoning`, omitted when 0;
    - `totalTokenCount = input + output`.
  - **Body:** `{candidates:[…], usageMetadata, modelVersion:model, responseId:"capture-{n+1}"}`.
  - **Streaming** (`alt=sse`, frames encoded with `"\r\n"`, no event names):
    - one frame per text piece (reasoning pieces first), each with a candidate holding one part and `usageMetadata: {promptTokenCount, totalTokenCount: promptTokenCount}`;
    - the last frame carries the function calls (Gemini sends them whole), `finishReason` and the full `usageMetadata`.
- **`pub fn embed_content(turn, inputs: usize, batch: bool) -> Rendered`:** `{embedding:{values}}`, or with `batch`, `{embeddings:[{values}…]}`. The vectors use the same formula as K3.

**Tests:** the same coverage as K3 for both providers, plus:
- the usage conversions with cache and reasoning tokens (the Anthropic `input_tokens` subtraction, Gemini's candidates/thoughts split);
- Gemini frames ending in `\r\n\r\n`.

**Checks:** `just check`.

## K5 — Mock provider server

**Commit:** `capture: serve scripted turns from a mock provider`

**Edit only:**
- `tools/capture/src/{lib.rs,error.rs,mock.rs}`;
- `tools/capture/Cargo.toml`: `axum` and `tokio` (features `net`, `time` added locally); dev-dependencies `tower` (feature `util`) and `http-body-util`;
- the root `Cargo.toml` for `axum`, `tower` and `http-body-util`.

**API:**
```rust
pub struct Mock { /* turns, next index, log */ }
pub struct MockLog { pub served: Vec<Served>, pub errors: Vec<String> }
pub struct Served { pub turn: usize, pub api: Api, pub stream: bool }
impl Mock {
    pub fn new(turns: Vec<Turn>) -> Arc<Mock>;
    pub fn router(self: &Arc<Self>) -> axum::Router;
    pub fn finish(&self) -> MockLog;   // also records "N turns not served" in errors
}
```

**Routes:**
- `POST /v1/chat/completions` → `openai.chat`;
- `POST /v1/responses` → `openai.responses`;
- `POST /v1/embeddings` → `openai.embeddings`;
- `POST /v1/messages` → `anthropic.messages`;
- `POST /v1beta/models/{model_action}`: split the segment at its last `:`. The action is one of:
  - `generateContent`;
  - `streamGenerateContent` (requires query `alt=sse`; otherwise answer 400 and log the error);
  - `embedContent`;
  - `batchEmbedContents`.
- **Fallback:** any other method or path answers 404 `{"error":{"message":"llmobs-capture mock: no route <METHOD> <path>"}}` and logs the error.

**Per request:**
1. Parse the body as JSON. The only facts read from it:
   - `stream`;
   - `stream_options.include_usage`;
   - for embeddings, the number of `input` items (a string counts as 1, an array as its length) and `encoding_format == "base64"`;
   - for Gemini batches, `requests.len()`.

   Any parse failure answers 400 and is logged with the route only.
2. Take the next turn.
   - **No turn left:** answer 500, log `request <k>: no scripted turn left`.
   - **`turn.api` doesn't match the route:** answer 500, log `turn <n>: expected <api>, got <route api>`.
   - The turn is consumed in both error cases, so one mistake doesn't cascade.
3. Render (K3/K4) and record a `Served` entry.
4. **Non-streaming:** sleep `ttft_ms`, then answer 200 `application/json`.

   **Streaming:** answer 200 `text/event-stream` with `cache-control: no-cache`, and a body stream that sleeps `ttft_ms` before the first frame and `chunk_delay_ms` between frames.
5. **Response headers:** OpenAI `x-request-id: req_capture_{n+1}`, Anthropic `request-id: req_capture_{n+1}`. Nothing else is provider-specific.

Request bodies are never logged, stored or written anywhere.

**Tests** (`tower::ServiceExt::oneshot` against the router, with `tokio::time::pause` so the delays take no real time):
- one success per route, streaming and not;
- a wrong API for the turn;
- turns exhausted;
- unserved turns reported by `finish`;
- the unknown route;
- Gemini streaming without `alt=sse`;
- the body of a streamed response decodes into the frames K3/K4 produced;
- the delays happen in the right places (measured with the paused clock).

**Checks:** `just check`.

## K6 — OTLP receiver

**Commit:** `capture: receive OTLP exports`

**Edit only:**
- `tools/capture/src/{lib.rs,error.rs,receiver.rs}`;
- `tools/capture/Cargo.toml`: `prost`, `opentelemetry-proto` (workspace features), `tokio` features;
- the root `Cargo.toml` for `prost` (R3).

**API:**
```rust
pub struct Receiver { /* exports, last_export_at, other_signals, errors */ }
impl Receiver {
    pub fn new() -> Arc<Receiver>;
    pub fn router(self: &Arc<Self>) -> axum::Router;
    pub async fn wait_idle(&self, quiet: Duration, max: Duration);  // returns once no export arrived for `quiet`, or after `max`
    pub fn finish(&self) -> Received;
}
pub struct Received { pub exports: Vec<ExportTraceServiceRequest>, pub other_signals: usize, pub errors: Vec<String> }
```

**Behaviour:**
- **Traces:** `POST` to any path ending in `/v1/traces` is accepted. That covers `/v1/traces` and the Langfuse SDK's `/api/public/otel/v1/traces`, and any authorization header is ignored.
  - `application/x-protobuf`: decode with `prost::Message::decode`.
  - `application/json`: decode with `serde_json` through `opentelemetry-proto`'s serde types.
  - Success answers 200 with an empty `ExportTraceServiceResponse`, encoded in the request's content type.
- **Bad requests:**
  - any `Content-Encoding` other than `identity` answers 415 and logs `compressed export: set OTEL_EXPORTER_OTLP_COMPRESSION=none`;
  - a decode failure answers 400 and logs the content type and body size only.
- **Other signals:** `POST` to paths ending in `/v1/logs` or `/v1/metrics` answers 200, and the request is counted in `other_signals` and discarded. Some SDKs export logs or metrics too; they're out of scope (spec 02 ingests traces only).
- **Body limit:** 32 MiB, as in spec 02's gRPC limit; above it, 413 is logged.
- The time of each accepted trace export is recorded for `wait_idle`.

**Tests:**
- a protobuf export and a JSON export of the same request decode to equal values;
- the Langfuse path;
- gzip refused;
- garbage refused;
- the logs and metrics counter;
- `wait_idle` with a paused clock: it returns after the quiet period, and returns at `max` when exports keep arriving.

**Checks:** `just check`.

## K7 — Determinism

**Commit:** `capture: make captures deterministic`

**Edit only:** `tools/capture/src/{lib.rs,error.rs,determinism.rs}`, `tools/capture/Cargo.toml` (`sha2`, `hex`, `chrono` from the workspace).

`pub fn deterministic(exports: Vec<ExportTraceServiceRequest>, case: &CasePath, ingest_time: i64, timestamps: &[TimestampAttribute]) -> Result<ExportTraceServiceRequest, CaptureError>` runs these steps in order:

1. **Merge.** The exports become one request.
   - Resource spans with equal resources (same attributes, same `schema_url`) are merged, and so are scope spans with equal scopes (name, version, attributes, `schema_url`).
   - **Span order:** spans are sorted by `(start_time, end_time, name)`. Equal keys keep their arrival order.
   - **Scope and resource order:** scopes and resources are ordered by their earliest span.

   This makes the output independent of how the SDK batched its exports.
2. **Re-mint IDs.**
   - Walk the spans in the final order. Assign trace IDs `k = 0, 1, …` in order of first appearance, and the same for span IDs, with one counter across all traces. A span's own ID is assigned before its parent's.
   - Parent and link IDs not seen as span IDs get the next number when they are first met.
   - **The new ID** is the first 16 (trace) or 8 (span) bytes of `sha256("llmobs-capture\0{group}/{case}\0trace\0{k}")` (or `…\0span\0{k}`). If those bytes are all zero, use `k + 1_000_000`.
   - **ID text inside attributes:** in every string `AnyValue`, at any depth, in span, event, link, scope and resource attributes, each occurrence of an original ID's lowercase hex is replaced with the new ID's hex. The match is case-insensitive, so W3C `traceparent` values and SDK-specific ID attributes stay consistent.
3. **Shift times.**
   - `base = ingest_time − 10 s`, `min_start` = the smallest span start.
   - Every instant `t` (span start and end, event times) becomes `base + round_ms(t − min_start)`, rounding half up to whole milliseconds.
   - **Timestamp attributes:** each attribute listed in `timestamps` is shifted the same way. `Rfc3339` values are parsed with `chrono` and written back with the same number of fractional digits and the same offset style (`Z` or `+00:00`); `UnixNano` and `UnixMilli` values are integers or decimal strings, kept in their original type.
   - **Errors:**
     - `WindowExceeded { span_ms }` if any shifted instant is ≥ `ingest_time` (the run took 10 s or more): the scenario must do less;
     - `InstantBeforeStart` for an event or timestamp attribute before `min_start`;
     - `TimestampAttribute { key }` for a listed attribute that doesn't parse. This error names the key, never the value.
4. **Prune proto3 defaults** that `opentelemetry-proto`'s serde would otherwise write as noise:
   - span `traceState` `""`, `parentSpanId` empty, `flags` 0;
   - `droppedAttributesCount`, `droppedEventsCount` and `droppedLinksCount` 0;
   - empty `events` and `links`;
   - `status` with code 0 and an empty message (the whole status);
   - empty `schemaUrl`;
   - scope `version` `""` and empty scope `attributes`.

   Pruning works on the typed values before serialization, or on the JSON tree **by field name at those positions only**. It never reaches inside an `AnyValue`: `{"boolValue": false}` and `{"intValue": "0"}` are real values. `parentSpanId` is the only ID that can be empty.

**Tests:**
- two captures of the "same" run that differ in original IDs, absolute times (by hours) and export batching produce byte-identical JSON;
- parent, link and attribute ID replacement, including an uppercase `traceparent`;
- rounding at ±0.5 ms;
- the window error;
- `Rfc3339` with 3 and 9 fractional digits and with `+00:00`, `UnixNano` as an int and as a string, `UnixMilli`;
- the error for an unparseable timestamp attribute doesn't contain the value;
- default pruning, including that a `false` bool attribute and a `0` int attribute survive.

**Checks:** `just check`.

## K8 — Scrubbing and the writer

**Commit:** `capture: scrub captures and write fixtures`

**Edit only:** `tools/capture/src/{lib.rs,error.rs,scrub.rs,write.rs}`.

**`scrub.rs`.** Signature: `pub fn scrub(request: &mut ExportTraceServiceRequest, ctx: &ScrubContext) -> Result<(), CaptureError>`, where `pub struct ScrubContext { pub provider: Provider, pub mock_port: u16, pub receiver_port: u16, pub home: PathBuf, pub repo_root: PathBuf, pub hostname: String }`.

1. **Resource attributes.**
   - **Drop:**
     - `host.id`, `host.arch`, `host.image.*`;
     - `process.pid`, `process.parent_pid`, `process.owner`, `process.command`, `process.command_line`, `process.command_args`, `process.executable.name`, `process.executable.path`, `process.working_directory`, `process.runtime.description`;
     - `os.description`, `os.version`, `os.build_id`;
     - `container.id`, `service.instance.id`.
   - **Pin:** `host.name` = `capture-host`.
   - **Keep:**
     - `service.name`;
     - `telemetry.*`;
     - `process.runtime.name`, `process.runtime.version` (pinned by `.python-version` and `.node-version`);
     - `os.type`;
     - anything else not listed.
2. **Span, event and link attributes.**
   - `server.address` equal to `127.0.0.1` or `localhost` becomes `ctx.provider.host()`.
   - `server.port` equal to the mock port becomes 443, keeping the int type.
3. **Every string value** (resource, scope, span, event and link attributes, at any depth, and span and event names), with replacements applied longest first:
   - `http://127.0.0.1:{mock_port}` and `http://localhost:{mock_port}` → `https://{host}`;
   - `repo_root` → `/capture`;
   - `home` → `/home/capture`;
   - `hostname` → `capture-host`, only when it is at least 3 characters long.
4. **Leak check** (after the rules above):
   - fail with `Leak { locations: Vec<String> }` if any string still contains `127.0.0.1`, `localhost`, `[::1]`, `:{mock_port}`, `:{receiver_port}`, `home`, `repo_root`, `hostname` or `capture-mock-key`;
   - also fail if any `server.port` is still the mock or receiver port.

   A location is a path like `resourceSpans[0].scopeSpans[1].spans[3].attributes["url.full"]`. **No values.**

**`write.rs`.** Signature: `pub fn write_case(dir: &Path, scenario: &Scenario, request: &ExportTraceServiceRequest, source: &Source) -> Result<Written, CaptureError>`.
- **`input.json`:** `serde_json::to_string_pretty` of the request (`opentelemetry-proto` serde: camelCase, hex IDs, 64-bit integers as strings, integer enums) plus one `\n`.
  - Over 256 KiB fails with `TooLarge { bytes }`.
  - The output must pass `crates/otel/tests/support`'s OTLP/JSON walker. Check this by running `just fixtures-check` on a sample in K10, not with a dependency on the test crate.
- **`case.json`:**
  ```json
  {"description", "origin":"captured",
   "source":{"instrumentation","instrumentation_version","language","libraries":{<sorted by name>},"captured_at"},
   "spec_refs", "settings"?, "context"?, "notes"?}
  ```
  - Use the field order of `CaseFile` in `crates/otel/tests/support/mod.rs`, which `fixtures/otlp/README.md` § Canonical formatting asks for.
  - `captured_at` is UTC now, RFC 3339 with whole seconds and `Z`.
  - `pub struct Source { instrumentation, instrumentation_version, language: Language, libraries: BTreeMap<String, String>, captured_at }`, `pub enum Language { Python, Typescript }` (serialized `python`, `typescript`).
- **Existing files:**
  - creates `dir` if needed;
  - overwrites `case.json` and `input.json`;
  - leaves `expected.json` alone. If one exists and `input.json` changed, `Written.stale_expected = true`, and the CLI (K9) tells the user to re-bless with B1's bless mode.

**Tests:**
- every scrub rule;
- a leak in a nested kvlist attribute is reported with its path and without its value;
- the API key never survives;
- the writer's output is canonical (parse it and pretty-print it again: byte-identical);
- the size limit;
- the `case.json` field order;
- `stale_expected`.

**Checks:** `just check`.

## K9 — Runner and CLI

**Commit:** `capture: run scenarios from the command line`

**Edit only:**
- `tools/capture/src/{lib.rs,error.rs,run.rs,main.rs}`;
- `tools/capture/Cargo.toml` (`[[bin]] name = "llmobs-capture"`, `clap`, `tracing`, `tracing-subscriber`, `tokio` features `process`, `rt-multi-thread`, `macros`);
- `justfile`;
- `AGENTS.md` (§ Layout and § Commands).

**Scenario discovery.** `pub fn discover(repo_root: &Path) -> Result<Vec<(CasePath, Language, PathBuf)>, CaptureError>` scans `tools/capture/py/scenarios/*/*/scenario.json` and `tools/capture/ts/scenarios/*/*/scenario.json`. The same case path in both fails. A missing directory is not an error.

**Program command:**
- Python: `uv run --project {root}/tools/capture/py --frozen python main.py`, cwd the scenario directory.
- TypeScript: `node main.ts`, cwd the scenario directory. Node 26 strips types natively; the TypeScript project (K14) uses erasable syntax only.

**Program environment** (`pub fn program_env(...) -> BTreeMap<String, String>`, applied with `env_clear()`):
- **Passed through from the parent:** `PATH`, `HOME`, and every `UV_*` and `XDG_*` variable (uv's cache).
- **Fixed:**
  - `LANG=C.UTF-8`, `TZ=UTC`, `PYTHONHASHSEED=0`;
  - `LLMOBS_CAPTURE_MOCK_URL=http://127.0.0.1:{mock_port}`;
  - `LLMOBS_CAPTURE_OTLP_ENDPOINT=http://127.0.0.1:{receiver_port}/v1/traces`;
  - `LLMOBS_CAPTURE_META={temp file}`;
  - `LLMOBS_CAPTURE_LIBRARIES={comma-separated scenario.libraries}`;
  - `LLMOBS_CAPTURE_CASE={group}/{case}`;
  - `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `GEMINI_API_KEY`, `GOOGLE_API_KEY`, all `capture-mock-key`;
  - `OTEL_EXPORTER_OTLP_PROTOCOL=http/protobuf`;
  - `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` = the receiver URL;
  - `OTEL_EXPORTER_OTLP_COMPRESSION=none`;
  - `OTEL_TRACES_SAMPLER=always_on`;
  - `OTEL_SERVICE_NAME=capture-app`.
- **Then** the scenario's `env`, which may override the `OTEL_*` values above.

Nothing else from the parent environment reaches the program. A real `OPENAI_API_KEY` in the developer's shell must never be seen by it.

**`pub async fn run_case(...) -> Result<Outcome, CaptureError>`:**
1. Load the scenario.
2. Bind the mock and the receiver on `127.0.0.1:0` and serve both.
3. Spawn the program. Forward its stdout and stderr lines to `tracing` at debug level, prefixed with the case path.
4. Wait for the program, with a 120 s timeout that kills it (`ProgramTimeout`). A non-zero exit fails (`ProgramFailed { code }`).
5. `receiver.wait_idle(300 ms, 5 s)`.
6. **Collect.** Any of these fails with all of the errors listed:
   - `mock.finish()` reports errors or unserved turns;
   - the receiver reports errors;
   - no spans arrived;
   - the meta file (`{"libraries": {name: version}}`, written by the program's harness, K10/K14) doesn't hold every scenario library.
7. **Process:** `deterministic` (K7, `ingest_time` from `context.ingest_time` or the README default), then `scrub` (K8).
8. **Write or check:**
   - **Write** (default): `write_case` to `fixtures/otlp/{group}/{case}/`.
   - **Check** (`--check`): build the two files in memory and compare them with the committed ones, after masking:
     - every span start and end time;
     - event times;
     - the values of `timestamp_attributes` and `timing_attributes`;
     - `source.captured_at`.

     Differences fail with `CheckFailed { case, paths }`, listing at most 20 JSON paths and no values. A missing committed case fails with "not captured yet".

**CLI** (`clap`):
- `llmobs-capture list` prints the discovered cases, one per line.
- `llmobs-capture run [--check] (--all | <group/case>...)` runs cases one at a time and prints `captured <case>` or `checked <case>`, plus a re-bless hint when `stale_expected`. It stops at the first failure and exits 1.

`repo_root` is `env!("CARGO_MANIFEST_DIR")/../..`. That's acceptable for a dev tool that is only run through `cargo run`, and a comment says so. Output goes through `tracing-subscriber` (fmt, to stderr) so the workspace print lints hold.

**`justfile`** (a new "Fixture capture" section):
```
# Capture OTLP fixtures from real instrumentations (tools/capture), e.g. `just capture openinference/openai-chat-tool`
capture +cases:
    cargo run -q -p llmobs-capture -- run {{cases}}

# Recapture every scenario
capture-all:
    cargo run -q -p llmobs-capture -- run --all
```
K10 and K14 add the project syncing and `check-capture`.

**`AGENTS.md`:**
- § Layout: add `tools/capture/   fixture capture: mock provider, OTLP receiver, scenarios (B2)` under `tools/`.
- § Commands: add the two recipes.

**Tests:**
- discovery: temp directories, a duplicate case, missing directories;
- `program_env`: nothing leaks from a parent environment with a fake `OPENAI_API_KEY` and `AWS_SECRET_ACCESS_KEY`, and scenario `env` overrides;
- the `--check` masking and the diff paths;
- `run_case` with a program that fails, using `false` as the command through an internal `Program` argument. This uses the real type with a different argv; there is no trait.
- The full success path is exercised for real in K10.

**Checks:** `just check`. `just capture-all` runs and reports zero cases.
