# 02 — Ingestion

## Endpoints

| Transport | Endpoint | Encodings |
|---|---|---|
| OTLP/HTTP | `POST /v1/traces` on the main HTTP port (default 8080) and on the standard OTLP/HTTP port 4318 | `application/x-protobuf`, `application/json`; `Content-Encoding: gzip` \| `zstd` \| `deflate` \| identity |
| OTLP/gRPC | `opentelemetry.proto.collector.trace.v1.TraceService/Export` on port 4317 | protobuf; gzip |

OTLP/JSON follows the OTLP specification strictly (D108): lowerCamelCase fields, hex trace and span IDs, enums as integers, 64-bit integers as decimal strings or numbers. Enum names and base64 IDs fail decoding, and the request gets `400`. Unknown fields are ignored: `opentelemetry-proto` 0.33.1 `with-serde` does not set `deny_unknown_fields`.

Authentication: `Authorization: Bearer <sk-…>` (project secret key with `ingest` scope). For OTel exporters that only support custom headers this is set via `OTEL_EXPORTER_OTLP_HEADERS="Authorization=Bearer sk-…"`. The key determines the project; there is no project header.

Responses follow OTLP: `200` with `ExportTraceServiceResponse` (with `partial_success.rejected_spans` and an error message when some spans were dropped), `400` malformed, `401` bad key, `413` too large, `429` rate limited (with `Retry-After`), `503` when the WAL is unavailable / disk full.

**Durability contract:** a `200` means every accepted span is in the fsynced WAL.

### Limits (configurable)

| Limit | Default | On violation |
|---|---|---|
| HTTP request body (after decompression) | 10 MiB | 413 |
| gRPC message | 32 MiB | `RESOURCE_EXHAUSTED` |
| Single attribute value | 8 MiB | truncated (below) |
| Spans per request | 50 000 | 413 |
| Per-org ingest rate | from org limits (12) | 429 |

## Pipeline

```
decode ─► authenticate ─► rate-limit/meter ─► for each span:
   detect conventions ─► normalize ─► extract media ─► truncate ─► compute cost ─► store media ─► build row
─► append batch to WAL (group commit, fsync) ─► insert into memtable
─► publish to live bus ─► enqueue online-eval triggers ─► respond 200
```

Normalization is a pure function in `llmobs-otel`: `normalize(request, ingest_context, price_table) -> Normalized { spans, media, rejections }` (D102, D103, D105). It does no I/O. The server stores each returned media blob through `llmobs-storage` before the WAL append. Re-sends are safe because blobs are content-addressed. `ingest_context` carries the project ID, the ingest time, the project's `retention_days` and the limits.

Scalar fields (model, provider, tokens, context, parameters, reported cost) come from precedence tables. Span kind, payloads and time-to-first-token come from one function per convention, called in the order below. There is no per-convention type (D102). The result is a `Vec<Span>` (D105); the server converts it with `llmobs_core::batch` before `append_spans`.

## Convention detection

A span may carry several conventions at once (e.g. Vercel AI SDK emits both `ai.*` and `gen_ai.*`). Detection is per attribute-prefix presence; **extraction precedence** per field is:

`llmobs.*` > OTel GenAI (`gen_ai.*` current) > OpenInference > Vercel AI SDK (`ai.*`) > OpenLLMetry (`gen_ai.prompt.N.*`, `traceloop.*`) > Langfuse (`langfuse.*`) > Laminar (`lmnr.*`) > Google ADK (`gcp.vertex.agent.*`) > generic fallbacks.

The first convention that yields a non-empty value for a field wins. Attributes used to build a normalized field are removed from `attributes` (to avoid storing payloads twice) **except** small attributes (encoded value ≤ 256 bytes, see rule 2 below), which are kept for SQL access.

Within one field, the first convention that yields a value wins as a whole. Message lists from two conventions are never merged (D102). Numbered attributes (`gen_ai.prompt.N.*`, `llm.input_messages.N.*`, …) are parsed by index, never matched by a raw key prefix: `gen_ai.prompt.name` is not part of `gen_ai.prompt.N`.

A convention is listed in `conventions` when the span has at least one attribute or event matching it.

| Convention | Present when |
|---|---|
| `llmobs` | any `llmobs.*` attribute |
| `genai` | any `gen_ai.*` attribute except the OpenLLMetry ones below, or an event named `gen_ai.*` |
| `openinference` | `openinference.span.kind`, or any `llm.*`, `input.value`, `output.value`, `embedding.*`, `retrieval.*`, `tool.*` attribute |
| `vercel` | any `ai.*` attribute |
| `openllmetry` | any `traceloop.*` attribute, or `gen_ai.prompt.N.*` / `gen_ai.completion.N.*` (N a decimal index), or `llm.request.type`, or `llm.request.functions.N.*` |
| `langfuse` | any `langfuse.*` attribute |
| `laminar` | any `lmnr.*` attribute |
| `google_adk` | any `gcp.vertex.agent.*` attribute |

`llm.request.type` and `llm.request.functions.*` count for both `openinference` (prefix `llm.`) and `openllmetry`. That is harmless: `conventions` is a debug aid.

## Mapping tables

### Span kind

| Source | Value → `span_kind` |
|---|---|
| `llmobs.span.kind` | any valid kind, verbatim |
| `gen_ai.operation.name` | `chat`, `text_completion`, `generate_content`, `response` → `llm`; `embeddings` → `embedding`; `execute_tool` → `tool`; `invoke_agent`, `create_agent` → `agent`; `retrieval` → `retriever`; `invoke_workflow`, `invoke_runner`, `plan` → `chain`. Any other value does not decide the kind |
| `openinference.span.kind` | `LLM` → `llm`, `EMBEDDING` → `embedding`, `TOOL` → `tool`, `AGENT` → `agent`, `CHAIN` → `chain`, `RETRIEVER` → `retriever`, `RERANKER` → `reranker`, `GUARDRAIL` → `guardrail`, `EVALUATOR` → `evaluator` |
| `ai.operationId` (Vercel) | `*.doGenerate`, `*.doStream` → `llm`; `ai.generateText`, `ai.streamText`, `ai.generateObject`, `ai.streamObject` → `chain`; `ai.toolCall` → `tool`; `ai.embed*` → `embedding` |
| `traceloop.span.kind` | `workflow`, `task` → `chain`; `agent` → `agent`; `tool` → `tool` |
| `llm.request.type` (OpenLLMetry) | `chat`, `completion` → `llm`; `embedding` → `embedding` |
| `langfuse.observation.type` | `generation` → `llm`, `embedding`, `tool`, `retriever`, `agent`, `chain`, `evaluator`, `guardrail` verbatim; `span`, `event` → `span` |
| `lmnr.span.type` | `LLM`, `CACHED` → `llm`; `TOOL` → `tool`; `EXECUTOR` → `agent`; `EVALUATOR`, `EVALUATION`, `HUMAN_EVALUATOR` → `evaluator`; others → `chain` |
| fallback | `gen_ai.tool.name` or `gen_ai.tool.call.id` present → `tool`, checked before the model fallback; else has `gen_ai.request.model` or `llm.model_name` → `llm`; else `span` |

Note: Vercel's outer `ai.generateText` is `chain` (not `llm`) so tokens/cost are not double-counted with its `doGenerate` child. Cost is only computed on `llm`/`embedding` spans.

### Input / output

| Convention | Input | Output |
|---|---|---|
| llmobs | `llmobs.input` (+ `llmobs.input.format`) | `llmobs.output` |
| GenAI | `gen_ai.input.messages` (messages), `gen_ai.system_instructions`, `gen_ai.tool.definitions`; when a span attribute is absent, the same key on the event `gen_ai.client.inference.operation.details`; tool spans: `gen_ai.tool.call.arguments` (json) | `gen_ai.output.messages`, span attribute else the same key on that event; tool spans: `gen_ai.tool.call.result` |
| GenAI legacy events | events `gen_ai.system.message`, `gen_ai.user.message`, `gen_ai.assistant.message`, `gen_ai.tool.message` → messages. These are log records in the semconv; on spans they appear only through bridges such as Strands and Logfire | event `gen_ai.choice` → messages |
| OpenInference | `llm.input_messages.{i}.message.*` (flattened → messages); else `input.value` (format from `input.mime_type`: `application/json` → json, else text) | `llm.output_messages.{i}.message.*`; else `output.value` |
| Vercel | `ai.prompt.messages` (JSON, converted) or `ai.prompt`; tool: `ai.toolCall.args`. Part shapes below | On `llm` spans: `ai.response.text` + `ai.response.toolCalls` → one assistant message (finish reason: rule 16). On other kinds: `ai.response.text`, else `ai.response.object`, as a non-message value (rule 10). Tool: `ai.toolCall.result` |
| OpenLLMetry | `gen_ai.prompt.{i}.role/content` (+ `tool_calls`) → messages; else `traceloop.entity.input` | `gen_ai.completion.{i}.*`; else `traceloop.entity.output` |
| Langfuse | `langfuse.observation.input` | `langfuse.observation.output` |
| Laminar | `lmnr.span.input` | `lmnr.span.output` |
| Google ADK | `gcp.vertex.agent.llm_request`, `gcp.vertex.agent.tool_call_args` | `gcp.vertex.agent.llm_response`, `gcp.vertex.agent.tool_response` |

Message conversion: each converter (OpenAI chat format, Anthropic content blocks, Gemini `parts`, OpenInference flattened, Vercel `CoreMessage`) maps into the canonical format of 01. If a convention recognizes its source but cannot convert it, the raw value is stored with `format = json` and `attributes` gains `llmobs.normalize_error`. Each failure is a code `messages:<convention>:<field>`, where `<convention>` is the convention's stored name (`llmobs`, `genai`, `openinference`, `vercel`, `openllmetry`, `langfuse`, `laminar`, `google_adk`) and `<field>` is `input`, `output`, `system_instructions` or `tool_definitions`. Several failures are joined with `,` and no spaces, for example `messages:genai:input,messages:genai:output`. The attribute replaces a client-sent value with the same key. The code never contains payload text.

**Tool definitions** are normalized to one element per tool (rule 7), from the first source that yields any:

- `gen_ai.tool.definitions`;
- OpenInference `llm.tools.N.tool.json_schema`;
- Vercel `ai.prompt.tools` (an array of JSON strings);
- OpenLLMetry `llm.request.functions.N.{name,description,parameters}`.

Vercel tool-call parts use `args` (v4) or `input` (v5+). Tool-result parts use `result` (v4) or `output`, where v5+ sends `{type, value}` and the converter takes `value`. `image` and `file` parts carry raw base64 in `image` or `data`, with `mimeType` or `mediaType`, and become `blob` parts.

**Payload encodings.** A message- or JSON-bearing attribute may arrive as a JSON string, as a string containing a JSON string that itself contains JSON (decoded at most twice), or as a structured `arrayValue` or `kvlistValue` converted to JSON with rule 1's encoding.

### Model, provider, usage

| Field | Sources (in precedence order) |
|---|---|
| `request_model` | `gen_ai.request.model`, `llm.model_name`, `embedding.model_name`, `ai.model.id`, `langfuse.observation.model.name` |
| `response_model` | `gen_ai.response.model`, `ai.response.model` |
| `provider` | `gen_ai.provider.name`, `gen_ai.system`, `llm.provider`, `llm.system`, `ai.model.provider` (rule 12), else inferred from model name via price table |
| `operation` | `gen_ai.operation.name` |
| `input_tokens` | `gen_ai.usage.input_tokens`, `llm.token_count.prompt`, `gen_ai.usage.prompt_tokens`, `ai.usage.inputTokens`, `ai.usage.promptTokens` |
| `output_tokens` | `gen_ai.usage.output_tokens`, `llm.token_count.completion`, `gen_ai.usage.completion_tokens`, `ai.usage.outputTokens`, `ai.usage.completionTokens` |
| `cache_read_tokens` | `gen_ai.usage.cache_read.input_tokens`, `llm.token_count.prompt_details.cache_read`, `ai.usage.inputTokenDetails.cacheReadTokens`, `ai.usage.cachedInputTokens`, `gen_ai.usage.cache_read_input_tokens`, `gen_ai.usage.details.cache_read_tokens`, `gen_ai.usage.input_tokens.cached` |
| `cache_write_tokens` | `gen_ai.usage.cache_write.input_tokens`, `gen_ai.usage.cache_creation.input_tokens`, `llm.token_count.prompt_details.cache_write`, `ai.usage.inputTokenDetails.cacheWriteTokens`, `gen_ai.usage.cache_creation_input_tokens`, `gen_ai.usage.cache_write_input_tokens`, `gen_ai.usage.details.cache_write_tokens` |
| `reasoning_tokens` | `gen_ai.usage.reasoning.output_tokens`, `llm.token_count.completion_details.reasoning`, `ai.usage.outputTokenDetails.reasoningTokens`, `ai.usage.reasoningTokens`, `gen_ai.usage.reasoning_tokens`, `gen_ai.usage.output_tokens.reasoning` |
| `total_tokens` | `llm.token_count.total`, `gen_ai.usage.total_tokens`, `ai.usage.totalTokens`, `ai.usage.tokens`, `llm.usage.total_tokens`, else rule 4 |
| `ttft_ns` | `llmobs.ttft_ns` (nanoseconds); `gen_ai.response.time_to_first_chunk` (seconds, ×1e9); `ai.response.msToFirstChunk`, then `ai.stream.msToFirstChunk` (milliseconds, ×1e6); `langfuse.observation.completion_start_time` (an instant: `ttft = instant − start_time`, null when negative). Event-based sources are not read until a captured fixture shows them (D113) |
| `request_params` | every `gen_ai.request.*` except `gen_ai.request.model`, then `llm.invocation_parameters`, `ai.settings.*`, `langfuse.observation.model.parameters` (rule 8) |

### Token semantics

Every usage source declares whether its input count includes cache tokens and whether its output count includes reasoning tokens (D110). Exclusive sources are added back. Safety net: if `input_tokens < cache_read + cache_write` after resolution, the input is treated as exclusive, the cache is added back, and the warning `input_excludes_cache` is recorded.

| Source | Input includes cache | Output includes reasoning |
|---|---|---|
| GenAI `gen_ai.usage.*` | yes (semconv) | yes |
| OpenInference `llm.token_count.*` | yes | yes |
| Vercel v6 (`inputTokenDetails` present) | yes (`noCacheTokens` + cache) | yes |
| Vercel ≤ v5 + provider `anthropic` | **no** (pinned by a captured fixture, D113) | yes |
| Vercel other | yes | yes |
| OpenLLMetry Anthropic | unverified (D113); treat as yes and rely on the safety net | yes |
| Gemini-native `usage_metadata` (`thoughts_token_count`) | yes | **no**: thoughts are outside the output, so add `reasoning` to the output |
| `gen_ai.usage.*` on an ADK span | unverified (D113); treat as yes | unverified (D113); treat as yes |
| Langfuse `usage_details` | the `input` key excludes `input_cached_tokens` / `cache_read_input_tokens` | yes |

The Vercel ≤ v5 + Anthropic rule keys on the span's `ai.*` attributes plus the normalized provider. It applies even when the count was read from `gen_ai.usage.input_tokens` on that same span, because Vercel mirrors its own count there.

Per-modality counts (`gen_ai.usage.{audio,image,text}.*`, `llm.token_count.*_details.audio`) are subsets. They never add to any column and stay in `attributes` as unconsumed attributes.

### Context fields

| Field | Sources |
|---|---|
| `session_id` | `llmobs.session.id`, `session.id`, `gen_ai.conversation.id`, `traceloop.association.properties.session_id`, `ai.telemetry.metadata.sessionId`, `langfuse.session.id`, `lmnr.association.properties.session_id` |
| `user_id` | `llmobs.user.id`, `user.id`, `enduser.id`, `traceloop.association.properties.user_id`, `ai.telemetry.metadata.userId`, `langfuse.user.id`, `lmnr.association.properties.user_id` |
| `tags` | `llmobs.tags`, `tag.tags`, `ai.telemetry.metadata.tags`, `langfuse.trace.tags`, `lmnr.association.properties.tags`. Each source is a string array or a string containing a JSON array of strings. Any other form, such as a comma-separated list, is not a tag source: it is consumed and stays in `attributes` (rule 2) |
| `metadata` | `llmobs.metadata.*` (one attribute per key), `metadata` (OI, JSON object string), `ai.telemetry.metadata.*`, `traceloop.association.properties.*`, `langfuse.trace.metadata.*`, `lmnr.association.properties.metadata.*` |
| `environment` | `llmobs.environment`, resource `deployment.environment.name`, `deployment.environment`, `langfuse.environment` |
| `release` | `llmobs.release`, resource `service.version`, `langfuse.release`, `lmnr.span.release` (span) |
| `service_name` | resource `service.name` |
| experiment linkage | `llmobs.experiment.id`, `llmobs.dataset_item.id` → stored in `metadata` under the reserved keys `llmobs.experiment_id`, `llmobs.dataset_item_id` (06) |

Span-level context fields are copied to the trace partial; the SDK sets them on every span inside a context block so traces are consistent even without the root.

## Normalization rules (clarifications)

These rules resolve ambiguities in the tables above. They were fixed while writing the first hand-written fixtures (D68); `fixtures/otlp/` pins them.

1. **Map values** (`attributes`, `resource_attributes`, `metadata`): OTLP `stringValue` is stored verbatim (no JSON quotes). Every other `AnyValue` is stored as its compact JSON encoding: int → `1200`, double → `0.2`, bool → `true`, array → `["stop"]`, kvlist → JSON object, bytes → base64 JSON string. A metadata source that is a JSON object string (OpenInference `metadata`) is expanded into one entry per key, using the same rule for each value.
2. **Consumed attributes:**
   - *Payload* attributes are always removed from `attributes` once consumed. These are every attribute that feeds `input`, `output`, `system_instructions` or `tool_definitions`, including flattened `llm.input_messages.*`, `llm.output_messages.*`, `llm.tools.*` and `gen_ai.prompt.*`/`gen_ai.completion.*`.
   - Payload attributes of **lower-precedence** conventions for a field that a higher-precedence source already filled are removed too, for example OpenInference `input.value` when `llm.input_messages.*` exist. Events consumed as payload sources are removed from `events`. That covers `gen_ai.client.inference.operation.details`, the legacy `gen_ai.*.message` events and `gen_ai.choice`.
   - Every other consumed attribute is kept in `attributes` if its encoded value is at most 256 bytes; otherwise it is removed.
   - *Format hints* (`input.mime_type`, `output.mime_type`) are not payload attributes. They are consumed attributes and follow the previous bullet, even when the payload they describe was dropped.
3. **Tokens and cost** are normalized only on `llm` and `embedding` spans. On other kinds the usage attributes stay in `attributes`, and the token and cost columns are null. This prevents double counting with child LLM spans (Vercel `ai.generateText`). `provider`, `request_model`, `response_model`, `operation` and `request_params` are normalized on every kind.
4. **`total_tokens`**: taken from a total attribute if present, else `input_tokens + output_tokens` when both are known, else null.
5. **System messages:** system messages that arrive *inside* a message list (OpenInference, OpenLLMetry, Vercel) stay in `input` as `role: system` messages. `system_instructions` is filled only from dedicated sources (`gen_ai.system_instructions`).
6. **Tool calls:** a tool call's `arguments` (and a tool response's `response`) is parsed into a JSON value when it is a string containing valid JSON; otherwise it is kept as a JSON string.
7. **`tool_definitions`**: a JSON array, with one element per tool (parsed JSON schema objects), in index order.
8. **`request_params`**: a compact JSON object with keys sorted alphabetically. Null when there are no parameters. Sources, in precedence order: every `gen_ai.request.*` attribute except `gen_ai.request.model` (the key is the suffix), `llm.invocation_parameters` (a JSON object string, expanded), `ai.settings.*` (the suffix, with no case conversion: `ai.settings.maxRetries` → `maxRetries`; `intValue` → JSON number), and `langfuse.observation.model.parameters` (a JSON object string, expanded). A parameters value that is not a JSON object is ignored and the attribute stays. On a duplicate key the higher-precedence source wins. The source attributes are consumed attributes (rule 2).
9. **Previews** (max 512 Unicode scalar values, cut with no ellipsis):
   - `messages` input: text parts of the last `user` message, joined with `\n`. If there is no user message, use the last message's text parts.
   - `messages` output: text parts of the last message, joined with `\n`. Null if it has none.
   - `json`: the compact JSON.
   - `text`: the text.
10. **Payload format for non-message values:** a value that parses as a JSON object or array is `json`; any other string is `text`. So Vercel `ai.prompt` (JSON object string) is `json`, and `ai.response.text` on a non-`llm` span is `text`.
11. **Context fields consumed from metadata-like sources** (`ai.telemetry.metadata.sessionId`/`userId`, `traceloop.association.properties.session_id`/`user_id`, …) fill `session_id`/`user_id` and are **not** duplicated in the `metadata` map. Their sibling keys do go to `metadata`, with the source prefix stripped (`ai.telemetry.metadata.feature` → `feature`).
12. **`provider`** is lowercased, then mapped through an alias table owned by `llmobs-otel`. Exact keys are applied first. Unknown values are kept lowercased.
    - Exact: `openai.chat`, `openai.responses` → `openai`; `anthropic.messages` → `anthropic`; `xai` → `x_ai`.
    - Semconv well-known values are kept as they are: `aws.bedrock`, `azure.ai.openai`, `azure.ai.inference`, `gcp.vertex_ai`, `gcp.gemini`, `gcp.gen_ai`, `x_ai`, `ibm.watsonx.ai`, `mistral_ai`, `groq`, `deepseek`, `perplexity`, `cohere`, `anthropic`, `openai`.
    - Vercel ids, applied only to a value read from `ai.model.provider`: `google.generative-ai` → `gcp.gemini`; `google.vertex.*` → `gcp.vertex_ai`; `amazon-bedrock` → `aws.bedrock`; `azure.*` → `azure.ai.openai`; `xai.*` → `x_ai`; `mistral.*` → `mistral_ai`; otherwise `<name>.<anything>` → `<name>`. A semconv value such as `aws.bedrock` is not passed through these rules.
13. **`conventions`** are listed in precedence order (§ Convention detection), without duplicates.
14. **Truncation of strings:** cut at the largest UTF-8 character boundary ≤ the limit, then append the marker. The stored value is therefore slightly longer than the limit. Payloads are measured **after** media extraction.
15. **Rejected spans:** a span is rejected (counted in `partial_success.rejected_spans`; the reason codes are rule 21) when:
    - its trace ID or span ID is missing, of the wrong length or all zeros;
    - `start_time_unix_nano` or `end_time_unix_nano` is above `i64::MAX` (rule 17);
    - its `start_time` is before 2000-01-01T00:00:00Z, before `ingest_time − retention_days` of its project (when the project has a retention), or more than 1 day after `ingest_time` (D85). The ingest context carries the project's `retention_days`.

    The other spans of the request are still accepted. An all-zero `parentSpanId`, or one whose length is not 8 bytes, is treated as absent. `partial_success.error_message` is rule 21.
16. **Finish reasons.** In a GenAI output message list, each message keeps its own `finish_reason`. A missing one is filled from `gen_ai.response.finish_reasons[i]`. For OpenLLMetry, `gen_ai.completion.N.finish_reason` goes on message N. For OpenInference, `llm.output_messages.N.message.finish_reason` goes on message N when present. For a message built from flat attributes (Vercel `doGenerate`/`doStream`): the first element of `gen_ai.response.finish_reasons` when present, else `ai.response.finishReason`. GenAI wins by the precedence of § Convention detection. The flat-attribute source attributes are consumed attributes (rule 2).
17. **Timestamps** (D106). `start_time_unix_nano` and `end_time_unix_nano` above `i64::MAX` reject the span. `end < start` sets `end_time = start_time` and records the warning `end_before_start`. A zero `end_time_unix_nano` (an unended span) counts as `end < start`.
18. **Warnings.** Recoverable oddities are recorded as warning codes in the attribute `llmobs.normalize_warning`: a JSON array of strings, sorted, without duplicates. The normalizer writes this attribute and replaces a client-sent value with the same key. Codes: `duplicate_key`, `end_before_start`, `input_excludes_cache`, `invalid_event_time`, `invalid_number`, `max_depth`. The set is closed. Adding a code means updating this rule.
19. **AnyValue limits** (D107). Nesting deeper than 32 levels (array or kvlist inside array or kvlist) is replaced at depth 33 by the string `[llmobs: max depth]`. The attribute name goes into `truncated`, and the `max_depth` warning is recorded. Duplicate keys in one key-value list: the last one wins, and `duplicate_key` is recorded. This applies to attributes, resource attributes, event and link attributes, and nested kvlists.
20. **Numbers.** A field typed as an integer accepts `intValue`, a `doubleValue` with no fractional part, or a string holding a decimal integer. A field typed as a float accepts `intValue`, `doubleValue` or a numeric string. Anything else does not fill the field. The attribute stays in `attributes`, unconsumed, and `invalid_number` is recorded. Negative token counts are invalid numbers.
21. **Rejection reasons** (D111). The rejection reasons are `invalid_trace_id`, `invalid_span_id`, `timestamp_overflow` and `start_time_out_of_range`. `partial_success.error_message` lists at most 4 of them with counts, sorted by count descending then by code, for example `rejected 3 spans: invalid_span_id ×2, start_time_out_of_range ×1`. When more than 4 exist, the rest are `, … ×{rest}`. The word is `span` when the total is 1. The message never contains a value from the request.

## Media extraction

Applied to `input`, `output`, `system_instructions` after message conversion and to raw `json` payloads.

1. Find candidates:
   - canonical `blob` parts;
   - data URIs `data:<mime>;base64,<…>` anywhere in string values;
   - provider shapes that survived as raw JSON: OpenAI `image_url.url` data URI, `input_audio.data`, Anthropic `source: {type: base64, media_type, data}`, Gemini `inline_data` / `inlineData {mime_type, data}`;
   - Vercel `image` and `file` parts (raw base64 in `image` or `data`). The Vercel converter turns them into `blob` parts.
2. Skip if decoded size < `media.min_bytes` (default 4 KiB) — tiny blobs stay inline.
3. Decode base64, compute SHA-256, sniff MIME if missing. Sniffing uses a fixed magic-byte table: PNG, JPEG, GIF, WebP, PDF, WAV, MP3, OGG, FLAC, MP4/M4A, WebM. Otherwise the declared type, else `application/octet-stream`.
4. The server stores the blob through `llmobs-storage` (03 §9): it writes `media/<project_id>/<sha256>` durably if absent and records the `media` row in its catalog (normalization returns the blob; D103).
5. Replace with `{"type":"uri","modality":…,"mime_type":…,"uri":"media://<sha256>"}` (or for raw JSON shapes, replace the base64 string by `media://<sha256>`).
6. Append the hash to `media_refs`.

Max per blob: `media.max_bytes` (default 32 MiB); larger blobs are dropped and replaced by `{"type":"uri","uri":"llmobs:dropped","reason":"too_large","size":…}`.

## Truncation

After media extraction, these strings are checked against `ingest.max_attribute_bytes` (8 MiB): `input`, `output`, `system_instructions`, `tool_definitions`, every value in `attributes` and `resource_attributes`, each event's `attributes` JSON, `status_message`, and `name`.

- **Messages.** The candidate list is measured by its serialized length.
  1. Keep every system message, the first user message, and the longest suffix that still fits. Where a contiguous run of messages was removed, insert one placeholder `{"role":"system","parts":[{"type":"text","content":"[llmobs: K messages truncated]"}]}`. `K` is the number removed in that run.
  2. If the kept list still exceeds the limit, cut the longest string leaf with the string rule: text or reasoning `content`, or tool `arguments` / `response` when that value is a JSON string. Cut it to the length that makes the list fit, or to 1 KiB when a shorter cut would be required. Repeat, longest first, until the list fits or no leaf is longer than 1 KiB.
  3. If it still does not fit, store the serialized messages cut with the string rule and set the format to `text`. `attributes` gains `llmobs.normalize_error` code `truncate:<field>`, where `<field>` is `input` or `output`.
- **JSON payloads.** `input` or `output` whose format is `json` is cut as one string. The format becomes `text`, because the cut text is no longer valid JSON.
- **Other strings.** Cut at the largest UTF-8 character boundary ≤ the limit, then append `…[llmobs: truncated, original N bytes]`. The stored value is slightly longer than the limit. A value whose length equals the limit is kept.

`truncated` lists the column name (`input`, `output`, `system_instructions`, `tool_definitions`, `name`, `status_message`), the attribute key, or `events[<i>].attributes`. The list is sorted and has no duplicates. Spans are never rejected for size alone (only the request).

## Cost computation

Price table (04 §models) rows: `match_pattern` (regex, anchored, case-insensitive), optional `provider`, `valid_from`, unit prices in USD per 1M tokens: `input`, `output`, `cache_read`, `cache_write`, `reasoning` (null ⇒ use `output`), plus optional tiers `[{min_input_tokens, prices…}]` for long-context pricing.

Resolution for span with `model` m, `provider` p, at `start_time` t, in project P:
1. Reported cost (D109), on `llm` and `embedding` spans only. Any of these keys sets `cost_source = reported`, and the price table is not used. `llmobs.cost.total`, `llm.cost.total`, `gen_ai.usage.cost`, `gen_ai.usage.total_cost` fill `cost_total` (first numeric value). `llm.cost.prompt` and `gen_ai.usage.input_cost` fill `cost_input`. `llm.cost.completion` and `gen_ai.usage.output_cost` fill `cost_output`. A non-numeric value does not fill its column and stays in `attributes` (rule 20); the key still counts. Reported parts fill their columns and missing parts stay null. `cost_total` is the reported total, else the sum when both parts are present, else null. On any other span kind, rule 3 leaves the cost columns null and these keys stay in `attributes`.
2. Otherwise, on an `llm` or `embedding` span with a model (`coalesce(response_model, request_model)`), candidates are the rows whose pattern matches, whose `valid_from` is at or before `start_time`, and whose provider matches. A row with a provider matches when the span's provider equals it, or the span has no provider. A row with no provider matches any span. Pick by scope priority project > org > built-in, then the latest `valid_from`, then the longest `match_pattern`, then the lowest row index. When the span has no provider and the chosen row has one, `provider` becomes that row's provider, including when the computed cost is null. A span with no model is a miss. On any other kind the cost columns stay null (rule 3).
3. The tier with the greatest `min_input_tokens` that is still ≤ `input_tokens` (the input column, cache included) supplies the unit prices. A null price on that tier uses the row's base price. When no tier qualifies, the base prices are used.
4. `uncached_input = input_tokens − cache_read − cache_write`, `non_reasoning_output = output_tokens − reasoning` (floored at 0).
5. `cost_input = uncached_input·input + cache_read·cache_read_price + cache_write·cache_write_price`; `cost_output = non_reasoning_output·output + reasoning·reasoning_price`; all /1e6. A term whose token count is 0 or absent contributes 0, even if its unit price is null. A term whose token count is greater than 0 and whose unit price is null (after `reasoning` → `output`) makes its part null; nothing else falls back. `cost_total = cost_input + cost_output`, null if either is null. `cost_source` stays `computed` (D82).
6. No match ⇒ costs null, `cost_source = none` (UI shows "—", never $0).

Built-in prices: a snapshot of LiteLLM's `model_prices_and_context_window.json`, converted to our format and committed as `crates/otel/prices/builtin.json`. The snapshot is converted by `just prices-update` and is never fetched at build time or at runtime (D104). It is refreshed by hand each release, and the diff is reviewed. Costs are computed once at ingest and **not** recomputed when prices change (documented; a recompute job is post-MVP).

Embedding spans use the `input` price only: `cost_output` is null and `cost_total = cost_input` (D82).

## Live bus

After WAL commit, each batch is published on an in-process `tokio::sync::broadcast` channel as lightweight events `{project_id, trace_id, span_id, start_time, …light columns}`. SSE handlers subscribe and filter by project / trace. Slow subscribers that lag are sent a `resync` event and the client refetches.

## Online-eval triggers

For each committed span, matching online rules (06) with `target = span` are evaluated against the span's light columns; sampled hits are inserted into the `eval_jobs` queue. Trace-level rules are triggered by the trace-completion detector (06), which is fed from the same batch (trace_id, ingest_time, has_root, root end).

## Testing

The fixture format is defined in `fixtures/otlp/README.md` (D66). Each case is a directory `fixtures/otlp/<group>/<case>/` with `case.json` (metadata, settings), `input.json` (OTLP/JSON `ExportTraceServiceRequest`) and `expected.json` (normalized spans in `llmobs_core::Span` JSON, extracted media, rejected count). Fixtures use the fixed test price table `fixtures/otlp/prices.json`, not the embedded LiteLLM snapshot (D67).

- Hand-written cases pin this spec's rules (Phase 0 step 3).
- Captured cases come from real instrumentations: OpenAI via OpenInference, OpenAI via OpenLLMetry, Vercel AI SDK v5/v6 generateText + streamText + tool calls, Anthropic with cache tokens, Gemini multimodal, LangChain, LangGraph, OpenAI Agents SDK, Google ADK, Laminar, Langfuse SDK v3. They are recorded by `tools/capture`, and their expected outputs are blessed by the golden test and then reviewed.
- Golden test (`llmobs-otel`): normalize every `input.json` and compare with `expected.json`. Costs are compared with a `1e-12` tolerance; everything else exactly.
- Property tests: normalization never panics on arbitrary attribute maps (proptest, in `just test`). There is no cargo-fuzz (D112).
