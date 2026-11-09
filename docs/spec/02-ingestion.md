# 02 — Ingestion

## Endpoints

| Transport | Endpoint | Encodings |
|---|---|---|
| OTLP/HTTP | `POST /v1/traces` on the main HTTP port (default 8080) and on the standard OTLP/HTTP port 4318 | `application/x-protobuf`, `application/json`; `Content-Encoding: gzip` \| `zstd` \| `deflate` \| identity |
| OTLP/gRPC | `opentelemetry.proto.collector.trace.v1.TraceService/Export` on port 4317 | protobuf; gzip |

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
   detect conventions ─► normalize ─► extract media ─► truncate ─► compute cost ─► build row
─► append batch to WAL (group commit, fsync) ─► insert into memtable
─► publish to live bus ─► enqueue online-eval triggers ─► respond 200
```

Normalization is a pure function `(resource, scope, otlp_span, project_settings) -> NormalizedSpan` in `llmobs-otel`, unit-tested with golden fixtures (see *Testing*). Media upload is the only I/O in the per-span path; it is idempotent (content-addressed) so retries are safe.

## Convention detection

A span may carry several conventions at once (e.g. Vercel AI SDK emits both `ai.*` and `gen_ai.*`). Detection is per attribute-prefix presence; **extraction precedence** per field is:

`llmobs.*` > OTel GenAI (`gen_ai.*` current) > OpenInference > Vercel AI SDK (`ai.*`) > OpenLLMetry (`gen_ai.prompt.N.*`, `traceloop.*`) > Langfuse (`langfuse.*`) > Laminar (`lmnr.*`) > generic fallbacks.

The first convention that yields a non-empty value for a field wins. Attributes used to build a normalized field are removed from `attributes` (to avoid storing payloads twice) **except** small scalar attributes (< 256 bytes), which are kept for SQL access.

## Mapping tables

### Span kind

| Source | Value → `span_kind` |
|---|---|
| `llmobs.span.kind` | any valid kind, verbatim |
| `gen_ai.operation.name` | `chat`, `text_completion`, `generate_content`, `response` → `llm`; `embeddings` → `embedding`; `execute_tool` → `tool`; `invoke_agent`, `create_agent` → `agent` |
| `openinference.span.kind` | `LLM` → `llm`, `EMBEDDING` → `embedding`, `TOOL` → `tool`, `AGENT` → `agent`, `CHAIN` → `chain`, `RETRIEVER` → `retriever`, `RERANKER` → `reranker`, `GUARDRAIL` → `guardrail`, `EVALUATOR` → `evaluator` |
| `ai.operationId` (Vercel) | `*.doGenerate`, `*.doStream` → `llm`; `ai.generateText`, `ai.streamText`, `ai.generateObject`, `ai.streamObject` → `chain`; `ai.toolCall` → `tool`; `ai.embed*` → `embedding` |
| `traceloop.span.kind` | `workflow`, `task` → `chain`; `agent` → `agent`; `tool` → `tool` |
| `llm.request.type` (OpenLLMetry) | `chat`, `completion` → `llm`; `embedding` → `embedding` |
| `langfuse.observation.type` | `generation` → `llm`, `embedding`, `tool`, `retriever`, `agent`, `chain`, `evaluator`, `guardrail` verbatim; `span`, `event` → `span` |
| `lmnr.span.type` | `LLM`, `CACHED` → `llm`; `TOOL` → `tool`; `EXECUTOR` → `agent`; `EVALUATOR`, `EVALUATION`, `HUMAN_EVALUATOR` → `evaluator`; others → `chain` |
| fallback | has `gen_ai.request.model` or `llm.model_name` → `llm`; else `span` |

Note: Vercel's outer `ai.generateText` is `chain` (not `llm`) so tokens/cost are not double-counted with its `doGenerate` child. Cost is only computed on `llm`/`embedding` spans.

### Input / output

| Convention | Input | Output |
|---|---|---|
| llmobs | `llmobs.input` (+ `llmobs.input.format`) | `llmobs.output` |
| GenAI | `gen_ai.input.messages` (messages), `gen_ai.system_instructions`, `gen_ai.tool.definitions`; tool spans: `gen_ai.tool.call.arguments` (json) | `gen_ai.output.messages`; tool spans: `gen_ai.tool.call.result` |
| GenAI legacy events | events `gen_ai.system.message`, `gen_ai.user.message`, `gen_ai.assistant.message`, `gen_ai.tool.message` → messages | event `gen_ai.choice` → messages |
| OpenInference | `llm.input_messages.{i}.message.*` (flattened → messages); else `input.value` (format from `input.mime_type`: `application/json` → json, else text) | `llm.output_messages.{i}.message.*`; else `output.value` |
| Vercel | `ai.prompt.messages` (JSON, converted) or `ai.prompt`; tool: `ai.toolCall.args` | `ai.response.text` + `ai.response.toolCalls` → assistant message; `ai.response.object`; tool: `ai.toolCall.result` |
| OpenLLMetry | `gen_ai.prompt.{i}.role/content` (+ `tool_calls`) → messages; else `traceloop.entity.input` | `gen_ai.completion.{i}.*`; else `traceloop.entity.output` |
| Langfuse | `langfuse.observation.input` | `langfuse.observation.output` |
| Laminar | `lmnr.span.input` | `lmnr.span.output` |
| Google ADK | `gcp.vertex.agent.llm_request`, `gcp.vertex.agent.tool_call_args` | `gcp.vertex.agent.llm_response`, `gcp.vertex.agent.tool_response` |

Message conversion: each converter (OpenAI chat format, Anthropic content blocks, Gemini `parts`, OpenInference flattened, Vercel `CoreMessage`) maps into the canonical format of 01. If conversion fails, the raw value is stored with `format = json` and a `normalize_error` entry is added to `attributes`.

### Model, provider, usage

| Field | Sources (in precedence order) |
|---|---|
| `request_model` | `gen_ai.request.model`, `llm.model_name`, `embedding.model_name`, `ai.model.id`, `langfuse.observation.model.name` |
| `response_model` | `gen_ai.response.model`, `ai.response.model` |
| `provider` | `gen_ai.provider.name`, `gen_ai.system`, `llm.provider`, `llm.system`, `ai.model.provider` (normalized: `openai.chat` → `openai`), else inferred from model name via price table |
| `operation` | `gen_ai.operation.name` |
| `input_tokens` | `gen_ai.usage.input_tokens`, `llm.token_count.prompt`, `gen_ai.usage.prompt_tokens`, `ai.usage.inputTokens`, `ai.usage.promptTokens` |
| `output_tokens` | `gen_ai.usage.output_tokens`, `llm.token_count.completion`, `gen_ai.usage.completion_tokens`, `ai.usage.outputTokens`, `ai.usage.completionTokens` |
| `cache_read_tokens` | `gen_ai.usage.cache_read.input_tokens`, `llm.token_count.prompt_details.cache_read`, `ai.usage.cachedInputTokens` |
| `cache_write_tokens` | `gen_ai.usage.cache_creation.input_tokens`, `llm.token_count.prompt_details.cache_write` |
| `reasoning_tokens` | `gen_ai.usage.reasoning.output_tokens`, `llm.token_count.completion_details.reasoning`, `ai.usage.reasoningTokens` |
| `total_tokens` | `llm.token_count.total`, `llm.usage.total_tokens`, else input + output |
| `ttft_ns` | `llmobs.ttft_ns`; `ai.response.msToFirstChunk` (Vercel); other instrumentations' first-token attributes/events as found in fixtures (to be pinned per convention) |
| `request_params` | `gen_ai.request.{temperature,top_p,top_k,max_tokens,frequency_penalty,presence_penalty,stop_sequences,seed}`, `llm.invocation_parameters`, `ai.settings.*` |

Token semantics: we follow OTel — `input_tokens` **includes** cached tokens; `output_tokens` **includes** reasoning tokens. Converters for conventions that report them separately (e.g. Anthropic's raw `cache_read_input_tokens` excluded from `input_tokens`) add them back. Each converter documents which semantics its source uses; fixtures pin it.

### Context fields

| Field | Sources |
|---|---|
| `session_id` | `llmobs.session.id`, `session.id`, `gen_ai.conversation.id`, `traceloop.association.properties.session_id`, `ai.telemetry.metadata.sessionId`, `langfuse.session.id` |
| `user_id` | `llmobs.user.id`, `user.id`, `enduser.id`, `traceloop.association.properties.user_id`, `ai.telemetry.metadata.userId`, `langfuse.user.id` |
| `tags` | `llmobs.tags` (string array), `tag.tags`, `langfuse.trace.tags` |
| `metadata` | `llmobs.metadata.*` (one attribute per key), `metadata` (OI, JSON object string), `ai.telemetry.metadata.*`, `traceloop.association.properties.*`, `langfuse.trace.metadata.*` |
| `environment` | `llmobs.environment`, resource `deployment.environment.name`, `deployment.environment`, `langfuse.environment` |
| `release` | `llmobs.release`, resource `service.version`, `langfuse.release` |
| `service_name` | resource `service.name` |
| experiment linkage | `llmobs.experiment.id`, `llmobs.dataset_item.id` → stored in `metadata` under the reserved keys `llmobs.experiment_id`, `llmobs.dataset_item_id` (06) |

Span-level context fields are copied to the trace partial; the SDK sets them on every span inside a context block so traces are consistent even without the root.

## Media extraction

Applied to `input`, `output`, `system_instructions` after message conversion and to raw `json` payloads.

1. Find candidates:
   - canonical `blob` parts;
   - data URIs `data:<mime>;base64,<…>` anywhere in string values;
   - provider shapes that survived as raw JSON: OpenAI `image_url.url` data URI, `input_audio.data`, Anthropic `source: {type: base64, media_type, data}`, Gemini `inline_data` / `inlineData {mime_type, data}`.
2. Skip if decoded size < `media.min_bytes` (default 4 KiB) — tiny blobs stay inline.
3. Decode base64, compute SHA-256, sniff MIME if missing (magic bytes; fall back to declared).
4. `PUT media/<project_id>/<sha256>` if absent (local: atomic write + rename; S3: `PutObject` with `If-None-Match: *`). Insert/ignore `media` row in SQLite.
5. Replace with `{"type":"uri","modality":…,"mime_type":…,"uri":"media://<sha256>"}` (or for raw JSON shapes, replace the base64 string by `media://<sha256>`).
6. Append the hash to `media_refs`.

Max per blob: `media.max_bytes` (default 32 MiB); larger blobs are dropped and replaced by `{"type":"uri","uri":"llmobs:dropped","reason":"too_large","size":…}`.

## Truncation

After media extraction, any attribute or payload string longer than `ingest.max_attribute_bytes` (8 MiB) is handled:

- `messages` payloads: keep system messages, the first user message and the last N messages that fit; insert a placeholder message `{"role":"system","parts":[{"type":"text","content":"[llmobs: K messages truncated]"}]}`.
- other strings: cut at a UTF-8 boundary and append `…[llmobs: truncated, original N bytes]`.

The attribute name is added to `truncated`. Spans are never rejected for size alone (only the request).

## Cost computation

Price table (04 §models) rows: `match_pattern` (regex, anchored, case-insensitive), optional `provider`, `valid_from`, unit prices in USD per 1M tokens: `input`, `output`, `cache_read`, `cache_write`, `reasoning` (null ⇒ use `output`), plus optional tiers `[{min_input_tokens, prices…}]` for long-context pricing.

Resolution for span with `model` m, `provider` p, at `start_time` t, in project P:
1. If span has a reported cost (`llmobs.cost.total`, `llm.cost.total`, `gen_ai.usage.cost`) → use it, `cost_source = reported`.
2. Else candidates = project-custom rows ∪ org-custom rows ∪ built-in rows matching m (and p when the row has one), `valid_from ≤ t`; pick by priority project > org > built-in, then latest `valid_from`, then longest pattern.
3. `uncached_input = input_tokens − cache_read − cache_write`, `non_reasoning_output = output_tokens − reasoning` (floored at 0).
4. `cost_input = uncached_input·input + cache_read·cache_read_price + cache_write·cache_write_price`; `cost_output = non_reasoning_output·output + reasoning·reasoning_price`; all /1e6.
5. No match ⇒ costs null, `cost_source = none` (UI shows "—", never $0).

Built-in prices: a snapshot of LiteLLM's `model_prices_and_context_window.json`, converted at build time to our format and embedded; refreshed every release. Costs are computed once at ingest and **not** recomputed when prices change (documented; a recompute job is post-MVP).

Embedding spans use `input` price only.

## Live bus

After WAL commit, each batch is published on an in-process `tokio::sync::broadcast` channel as lightweight events `{project_id, trace_id, span_id, start_time, …light columns}`. SSE handlers subscribe and filter by project / trace. Slow subscribers that lag are sent a `resync` event and the client refetches.

## Online-eval triggers

For each committed span, matching online rules (06) with `target = span` are evaluated against the span's light columns; sampled hits are inserted into the `eval_jobs` queue. Trace-level rules are triggered by the trace-completion detector (06), which is fed from the same batch (trace_id, ingest_time, has_root, root end).

## Testing

- `fixtures/otlp/<convention>/<case>.json`: real OTLP exports captured from each instrumentation (OpenAI via OpenInference, OpenAI via OpenLLMetry, Vercel AI SDK v5/v6 generateText + streamText + tool calls, Anthropic with cache tokens, Gemini multimodal, LangChain, LangGraph, OpenAI Agents SDK, Google ADK, Laminar, Langfuse SDK v3).
- `fixtures/otlp/…/<case>.expected.json`: the normalized span(s). Golden tests diff them.
- Property tests: normalization never panics on arbitrary attribute maps (proptest).
