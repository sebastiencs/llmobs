# B1 · Payloads (N9–N13)

See [README.md](README.md). These sub-phases fill `input`, `output`, their formats and previews, `system_instructions` and `tool_definitions`, for the llmobs and GenAI conventions. The other conventions come in [05-conventions.md](05-conventions.md).

## N9 — Payload model, formats and previews

**Edit only:** `crates/otel/src/{payload.rs,normalize.rs,lib.rs}`.

**Commit:** `otel: payload values, formats and previews`

```rust
pub(crate) enum Payload {
    Messages(Vec<llmobs_core::message::Message>),
    Json(serde_json::Value),
    Text(String),
}
pub(crate) struct Io {                 // what a convention function returns for one span
    pub input: Option<Payload>,
    pub output: Option<Payload>,
    pub system_instructions: Option<serde_json::Value>,  // stored as compact JSON text
    pub tool_definitions: Option<Vec<serde_json::Value>>, // rule 7
}
```

Helpers, each unit-tested:

- `decode_json_text(value: &AnyValue) -> Option<serde_json::Value>`, for N1's "Payload encodings":
  - a string holding JSON → parse it;
  - if the result is a JSON string that itself parses as an object or array, parse once more (at most twice);
  - `arrayValue` and `kvlistValue` → `value::to_json`;
  - anything else → `None`.
- `non_message_payload(value: &AnyValue) -> Payload` (rule 10): a JSON object or array → `Json`, any other string → `Text`, other `AnyValue`s → `Json` of their rule 1 encoding.
- `parse_tool_arguments(v: Value) -> Value` (rule 6): a string that parses as JSON → the parsed value, else unchanged.
- `tool_definitions(items) -> Vec<Value>` (rule 7): each item is parsed when it's a JSON string, and kept in index order.
- `preview_input(&Payload) -> Option<String>` and `preview_output(&Payload) -> Option<String>` (rule 9): 512 Unicode scalar values, cut without an ellipsis. With no text parts, the preview is `None` for messages.
- `write(span: &mut Span, io: Io)`: sets `input`, `input_format`, `input_preview`, the same for `output`, `system_instructions` (compact JSON), and `tool_definitions` (a compact JSON array, `None` if empty). Messages are serialized with core's `messages_to_json`.
- **Conversion failure** (spec 02 § Message conversion): a convention function that recognizes its source but can't convert it returns `Payload::Json(raw)`, and the caller sets the attribute `llmobs.normalize_error` to a short code such as `"messages:genai:input"`. Never include the raw text. The code format is `messages:<convention>:<field>`. If several conversions fail, the codes are joined with `,`. Add this format to spec 02 § Message conversion in this commit.

The **driver**, in `normalize.rs` or a `payload.rs` function: for each of the four fields, call the convention functions in precedence order (`llmobs`, `genai`, `openinference`, `vercel`, `openllmetry`, `langfuse`, `laminar`, `google_adk`) and keep the first `Some` (D100). After the winner is chosen, call `drop_lower_payloads(field, winner)`. It marks the payload source keys of every **lower-precedence** convention for that field as `Payload` (rule 2, second bullet). Each convention module exposes `const PAYLOAD_KEYS: &[(Field, KeyPattern)]` for this. A `KeyPattern` is an exact key, an indexed prefix such as `llm.input_messages.`, or an event name. Conventions not written yet contribute nothing. The driver is a plain list of function pointers, so adding a convention later is one line.

Events consumed as payload sources are removed from `span.events` (N1 § 6). The convention function returns the indices of the events it used, and the driver removes them after the winner is chosen. Events of losing conventions are also removed when they are payload sources (rule 2).

Tests: unit tests for every helper; preview cut at exactly 512 scalars with multi-byte characters; rule 10 classification; double-encoded JSON.

---

## N10 — OpenAI chat converter

**Edit only:** `crates/otel/src/convert/{mod.rs,openai.rs}`, `crates/otel/src/lib.rs`.

**Commit:** `otel: convert OpenAI chat messages`

`pub(crate) fn openai_messages(v: &Value) -> Result<Vec<Message>, ConvertError>` converts an OpenAI Chat Completions `messages` array, or a single `choices[].message`, to canonical messages (spec 01 § Canonical message format):
- `role`, and `name` when present;
- `content` as a string → one text part;
- `content` as an array of parts:
  - `text` → text;
  - `image_url` → `uri` with modality `image`. A data-URI `url` becomes a `blob` part with `content` = the base64 and `mime_type` from the URI, so media extraction (N14) sees it.
  - `input_audio {data, format}` → `blob` with modality `audio` and `mime_type` `audio/<format>`;
  - `file` → keep as `Unknown`;
  - `refusal` → text;
- `tool_calls[]` → `tool_call {id, name, arguments}` (rule 6 on `function.arguments`);
- role `tool` with `tool_call_id` → `tool_call_response {id, response}` (rule 6);
- `reasoning_content`, a non-standard but common field → a `reasoning` part;
- unknown fields go into `extra` where core's `Message`/`Part` have one; nothing is dropped silently.

`ConvertError` is a small `thiserror` enum (`NotAnArray`, `NotAMessage { index }`) that carries no content.

Also `openai_responses_output(v: &Value)` for the Responses API `output` items (`message` with `output_text`, `function_call`, `reasoning`). Add it only if the GenAI or OpenInference fixtures of N13/N18 need it. Otherwise leave it out, and the report says so.

Unit tests: one per shape above, plus malformed input → `Err`.

---

## N11 — Anthropic converter

**Edit only:** `crates/otel/src/convert/{mod.rs,anthropic.rs}`.

**Commit:** `otel: convert Anthropic content blocks`

`anthropic_messages(v: &Value, system: Option<&Value>) -> Result<(Vec<Message>, Option<Value>), ConvertError>` converts a Messages API `messages` array plus the optional top-level `system`, which is returned separately as system instructions:
- `content` as a string → text;
- blocks:
  - `text` → text;
  - `thinking` → reasoning; `redacted_thinking` → `Unknown`;
  - `tool_use` → `tool_call {id, name, arguments: input}`;
  - `tool_result` → `tool_call_response {id: tool_use_id, response: content}`, where content may be a string or blocks;
  - `image`/`document` with `source.type = base64` → `blob` with `media_type` and `data`. With `source.type = url` → `uri`.

Unit tests per block type.

---

## N12 — Gemini converter

**Edit only:** `crates/otel/src/convert/{mod.rs,gemini.rs}`.

**Commit:** `otel: convert Gemini contents and parts`

`gemini_contents(v: &Value) -> Result<Vec<Message>, ConvertError>` converts `contents[] {role, parts[]}`:
- role `model` → `assistant`;
- parts:
  - `text` → text, or reasoning when `thought: true`;
  - `inlineData`/`inline_data {mimeType|mime_type, data}` → `blob`;
  - `fileData {mimeType, fileUri}` → `uri`;
  - `functionCall {name, args, id?}` → `tool_call`;
  - `functionResponse {name, response, id?}` → `tool_call_response`.

Also `gemini_system_instruction(v)` → the instruction parts as a JSON value.

Unit tests per part type, in both camelCase and snake_case spellings.

---

## N13 — llmobs and GenAI payloads

**Edit only:**
- `crates/otel/src/conventions/{mod.rs,llmobs.rs,genai.rs}`;
- `crates/otel/src/{payload.rs,normalize.rs,lib.rs}`;
- fixtures under `fixtures/otlp/{llmobs,genai}/`.

**Commit:** `otel: read llmobs and GenAI payloads`

`llmobs.rs`:
- `llmobs.input` + `llmobs.input.format` (`messages`, `json` or `text`; when absent, rule 10 decides) → `Payload`;
- the same for `llmobs.output`;
- `messages` → parse with core `parse_messages`. On failure, the raw value is kept as `Json`, or as `Text` when it isn't JSON, plus `normalize_error`.

`genai.rs` (spec 02 GenAI and GenAI legacy events rows, N1 additions):
- `gen_ai.input.messages` / `gen_ai.output.messages`: canonical already, parsed with `parse_messages` after `decode_json_text`.
- `gen_ai.system_instructions` → `system_instructions`.
- `gen_ai.tool.definitions` → `tool_definitions`.
- Tool spans (`span_kind == Tool`): input = `gen_ai.tool.call.arguments` (rule 6, `Json` or `Text`), output = `gen_ai.tool.call.result`. They're payload attributes.
- Fallback 1: the span event `gen_ai.client.inference.operation.details`, whose attributes hold the same three keys.
- Fallback 2: the legacy events `gen_ai.{system,user,assistant,tool}.message` → input messages, and `gen_ai.choice` → output messages. The event attribute or body shape is `{role?, content, tool_calls?}`, with `content` as a string or OpenAI-style parts. Use the N10 converter for it, which needs a small adapter from event attributes to an OpenAI message value.
- Finish reasons (N1 § 5): fill a missing `finish_reason` on output message `i` from `gen_ai.response.finish_reasons[i]`. That attribute is a consumed attribute, not a payload.
- `PAYLOAD_KEYS` for both modules.

Fixtures:
- `llmobs/input-output-formats`: three spans with messages, json and text;
- `genai/tool-span`;
- `genai/operation-details-event`;
- `genai/legacy-message-events`: `notes` says "shape as emitted by Strands/Logfire bridges; re-check against a B2 capture";
- `genai/finish-reasons-fill`;
- `edge/messages-conversion-failure` (`gen_ai.input.messages` that isn't an array → raw JSON + `normalize_error`);
- `edge/double-encoded-messages`.

Now `genai/chat-cache-tokens` should only need cost (N16). Check, and report its remaining diff.

Unit tests for the precedence between the llmobs and GenAI fields, and that "message lists are never merged". Example: `llmobs.input` wins, so `gen_ai.input.messages` is dropped from `attributes` and isn't merged.
