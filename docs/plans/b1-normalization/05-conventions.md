# B1 · The other conventions (N18–N22)

See [README.md](README.md). Each sub-phase adds one convention module under `crates/otel/src/conventions/`, plus a converter under `convert/` when it needs one. It also registers the module in the payload driver (N9), adds the convention's span-kind, usage, context and TTFT rows to the tables if N6–N8 left any out, and adds hand-written fixtures.

The tables of spec 02 (as amended by N1) are the source of truth. `docs/research/ingestion/genai-mapping.md` gives each competitor's reading, with citations. Read it for the convention you implement, but never copy competitor code.

**Common rules for N18–N22:**
- **Edit only:**
  - `crates/otel/src/conventions/<name>.rs`, `conventions/mod.rs`;
  - `crates/otel/src/convert/<name>.rs` if needed, `convert/mod.rs`;
  - `crates/otel/src/{fields.rs,kind.rs,context.rs,usage.rs,payload.rs}`, only to add this convention's rows;
  - `fixtures/otlp/<group>/` for the convention's group;
  - `crates/otel/tests/golden.rs` (`PENDING` only).
- Every attribute the module reads as a payload is listed in its `PAYLOAD_KEYS`.
- Numbered keys are read with `AttrBag::indexed`, never by prefix (N3).
- **At least these fixtures:** one `llm` span with messages, tokens and a model from the test price table; one tool span; one non-LLM span; and one per special rule below. Use the scope names and versions the real instrumentation uses (`case.json` `notes`: "hand-written from <source>; re-check against a B2 capture").
- A behaviour marked **(D111)** below comes from one source only. Its fixture's `notes` must say so.
- **Report:** what you could not verify, and any spec 02 row you found wrong. Fix the spec in the same commit when the fix is certain. Otherwise, report it.

## N18 — OpenInference

**Commit:** `otel: normalize OpenInference spans`

- **Kind:** `openinference.span.kind`, already in N6. Check it.
- **Input:**
  - `llm.input_messages.N.message.{role,content,name,tool_call_id}`;
  - `…message.contents.M.message_content.{type,text,image.image.url}`;
  - `…message.tool_calls.K.tool_call.{id,function.name,function.arguments}`.

  These are rebuilt into OpenAI-shaped values with a small `convert/openinference.rs` that groups the `indexed` keys, then converted with the N10 converter. A role-`tool` message with `tool_call_id` becomes `tool_call_response`.
- **Output:** `llm.output_messages.N.*`, the same way. `…message.finish_reason`, if present, goes on the message (N1 § 5).
- **Fallbacks:** `input.value`/`output.value` with `input.mime_type`/`output.mime_type` (`application/json` → `Json`, else `Text`). The mime types are consumed attributes (rule 2, last bullet).
- **Tool definitions:** `llm.tools.N.tool.json_schema`, a JSON string per tool.
- **Tool spans:** `tool.name`, `tool.parameters`. Input and output come from `input.value`/`output.value`.
- **Retriever spans:** `retrieval.documents.N.document.{id,content,score,metadata}` → output `Json`, an array of objects in index order. They're payload attributes.
- **Embedding spans:** `embedding.model_name`, `embedding.embeddings.N.embedding.text` → input `Json` (array of texts). `embedding.embeddings.N.embedding.vector` is dropped as a payload: vectors are never stored.
- **Reported cost:** `llm.cost.prompt`, `llm.cost.completion`, `llm.cost.total` (N16).
- **Context:** `session.id`, `user.id`, `metadata` (a JSON object string, expanded per rule 1), `tag.tags`. Already in N7; check.
- **Flattened `llm.input_messages` and `input.value` both present:** messages win and `input.value` is dropped (rule 2). The existing fixture `openinference/llm-flattened-tool-call` pins this. Remove it from `PENDING`.
- **Fixtures:** `openinference/retriever-documents`, `openinference/embedding-vectors-dropped`, `openinference/multimodal-contents`, `openinference/tool-span`, `openinference/chain-json-io`.

## N19 — Vercel AI SDK

**Commit:** `otel: normalize Vercel AI SDK spans`

`convert/vercel.rs` converts `CoreMessage`/`ModelMessage` (v4, v5 and v6 shapes, N1 § 5) to canonical messages:
- `content` as a string or parts;
- `text` and `reasoning` parts;
- `tool-call {toolCallId, toolName, args | input}`;
- `tool-result {toolCallId, result | output: {type, value}}`;
- `image {image, mimeType | mediaType}` → `blob` when `image` is base64, `uri` when it's a URL;
- `file {data, mediaType | mimeType}` → `blob`.

The rest of the convention:
- **Input:** `ai.prompt.messages` (JSON). On a non-`llm` span, `ai.prompt` is a non-message value (rule 10). Tool spans: `ai.toolCall.args`.
- **Output:**
  - on `llm` spans, `ai.response.text` + `ai.response.toolCalls` (JSON array) form one assistant message, with the finish reason from rule 16;
  - on other kinds, `ai.response.text`, else `ai.response.object` (rule 10);
  - tool spans: `ai.toolCall.result`.
- **Tool definitions:** `ai.prompt.tools`, an array of JSON strings.
- **Model and provider:** `ai.model.id`, `ai.model.provider` with the Vercel alias mapping (N7), `ai.response.model`.
- **Usage:** v4, v5 and v6 keys (N8). Check the `VercelLegacy` semantics against the new fixtures **(D111)**. Anthropic cache write before v6 lives only in `ai.response.providerMetadata` (a JSON string). Read `anthropic.cacheCreationInputTokens` (v5) or `anthropic.usage.cache_creation_input_tokens` from it when no cache-write source exists **(D111)**.
- **Settings:** `ai.settings.*` → `request_params` (N7).
- **Context:** `ai.telemetry.metadata.*` (rule 11). Also `ai.telemetry.functionId` → `metadata["functionId"]`. Add that row to spec 02.
- **TTFT:** `ai.response.msToFirstChunk`, `ai.stream.msToFirstChunk` (N8).
- **Double counting:** the outer `ai.generateText` is `chain` (N6), so its `ai.usage.*` stays in `attributes` (rule 3). Remove the existing fixture `vercel/generate-text` from `PENDING`.
- **Fixtures:** `vercel/stream-text-ttft`, `vercel/tool-call-v5-input-output`, `vercel/v4-args-result`, `vercel/image-part-base64` (with media extraction; lower `media_min_bytes`), `vercel/generate-object`, `vercel/anthropic-provider-metadata-cache-write` **(D111)**.

## N20 — OpenLLMetry (Traceloop)

**Commit:** `otel: normalize OpenLLMetry spans`

- **Kind:** `traceloop.span.kind`, `llm.request.type` (N6).
- **Input:** `gen_ai.prompt.N.{role,content}`, plus `gen_ai.prompt.N.tool_calls.K.{id,name,arguments}` and `gen_ai.prompt.N.tool_call_id`, rebuilt into OpenAI-shaped messages and converted with N10. `content` may be a string or a JSON string of OpenAI content parts: try parsing it as JSON when it starts with `[`.
- **Output:** `gen_ai.completion.N.{role,content,finish_reason,tool_calls.K.*}`.
- **Fallbacks:** `traceloop.entity.input` / `traceloop.entity.output`. These are JSON strings, often `{"args": …, "kwargs": …}` / `{"output": …}` wrappers. Store them as `Json` without unwrapping.
- **Tool definitions:** `llm.request.functions.N.{name,description,parameters}` → `{type: "function", name, description, parameters}` per tool, in index order.
- **Usage:** `gen_ai.usage.prompt_tokens` / `completion_tokens`, plus the underscore cache keys (N8). Anthropic semantics are unverified **(D111)**: treat input as inclusive and rely on the safety net. Add a fixture where the safety net fires.
- **Context:** `traceloop.association.properties.*` (rule 11), `traceloop.workflow.name` → `metadata["workflow"]`. Add that row to spec 02.
- **Precedence check:** a span with both `gen_ai.input.messages` (GenAI) and `gen_ai.prompt.0.*` takes GenAI's input. The OpenLLMetry keys are dropped (rule 2).
- **Fixtures:** `openllmetry/chat-openai`, `openllmetry/chat-anthropic-cache` **(D111)**, `openllmetry/workflow-task-tool`, `openllmetry/functions-tool-definitions`, `openllmetry/genai-and-legacy-prompt`.

## N21 — Langfuse and Laminar

**Commit:** `otel: normalize Langfuse and Laminar spans`

Langfuse (`langfuse.*`):
- **Kind:** `langfuse.observation.type` (N6).
- **Payloads:** `langfuse.observation.input` / `.output`, as messages when they parse as an OpenAI-style message array, else rule 10. The same applies to `langfuse.trace.input` / `.output`, used only when the observation keys are absent.
- **Model:** `langfuse.observation.model.name`, plus `langfuse.observation.model.parameters` → `request_params` (N7).
- **Usage:** `langfuse.observation.usage_details`, a JSON object string. Keys:
  - `input`, `output`, `total`;
  - `input_cached_tokens` / `cache_read_input_tokens` → cache read;
  - `cache_creation_input_tokens` → cache write;
  - `output_reasoning_tokens` / `reasoning_tokens` → reasoning.

  `input` excludes cached tokens, per N1's token semantics table. Add the parsed object as a usage source after OpenLLMetry in precedence.
- **Reported cost:** `langfuse.observation.cost_details` (JSON: `input`, `output`, `total`) → D107.
- **TTFT:** `langfuse.observation.completion_start_time` (N8).
- **Context:** `langfuse.session.id`, `langfuse.user.id`, `langfuse.trace.tags`, `langfuse.trace.metadata.*`, `langfuse.environment`, `langfuse.release` (N7). `langfuse.observation.level` and `.status_message`: `level = ERROR` → `status = error` when the OTLP status is unset. Add that rule to spec 02.

Laminar (`lmnr.*`):
- **Kind:** `lmnr.span.type` (N6).
- **Payloads:** `lmnr.span.input` / `.output` (JSON strings; messages when they parse as canonical or OpenAI-style messages, else rule 10).
- **Context:** `lmnr.association.properties.{session_id,user_id,tags,metadata.*}`, `lmnr.span.release` (N1 § 5).
- **Path:** `lmnr.span.path` stays in `attributes`, since it's not normalized.

**Fixtures:** `langfuse/generation-usage-details`, `langfuse/cost-details-reported`, `langfuse/completion-start-time`, `langfuse/level-error`, `laminar/llm-span`, `laminar/association-properties`.

## N22 — Google ADK

**Commit:** `otel: normalize Google ADK spans`

- **Payloads:**
  - `gcp.vertex.agent.llm_request` is a JSON string of a Gemini `GenerateContentRequest`: `contents` go through N12, `config.system_instruction` through `gemini_system_instruction`, and `config.tools` become tool definitions;
  - `gcp.vertex.agent.llm_response` is a JSON string of a `GenerateContentResponse`: `content` (one message) goes through N12, plus `finish_reason`;
  - tool spans: `gcp.vertex.agent.tool_call_args` / `tool_response` → `Json`.
- **Usage:** ADK also emits `gen_ai.usage.*`, read through GenAI (N8). Gemini-native semantics put thoughts **outside** output (N1 table). Apply the add-back when the usage came from the `llm_response.usage_metadata` (`thoughts_token_count`). For `gen_ai.usage.*` on ADK spans the semantics are unverified **(D111)**: treat them as GenAI-inclusive.
- **Kind:** ADK sets `gen_ai.operation.name` (`call_llm` → **add** `llm` to the N6 table and spec 02 with a note; `execute_tool` → `tool`; `invoke_agent` → `agent`). Check this against ADK's source if it's available, and write down what you checked.
- **Fixtures:** `google_adk/call-llm`, `google_adk/execute-tool`, `google_adk/thoughts-tokens` **(D111)**.
