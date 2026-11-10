# OTLP GenAI mapping: competitors vs spec 02

Read-only comparison (Nov 2026) of how five competitors map LLM semantic conventions, checked against `docs/spec/02-ingestion.md` (§ Convention detection, Mapping tables, Normalization rules, Media extraction, Cost computation) and `fixtures/otlp/`. No code is copied; this file only describes behaviour and cites `path:line`.

Path prefixes:
- **LMNR** = `/home/sebastien/github/lmnr/app-server/src/`
- **LF** = `/home/sebastien/github/langfuse/packages/shared/src/server/otel/OtelIngestionProcessor.ts` (and **LFT** = `.../otel/ObservationTypeMapper.ts`)
- **PHX** = `/home/sebastien/github/phoenix/src/phoenix/`
- **OPIK** = `/home/sebastien/github/opik/apps/opik-backend/src/main/java/com/comet/opik/domain/`
- **LW** = `/home/sebastien/github/langwatch/packages/span-normalisation/src/`

Semconv references:
- GenAI conventions moved out of the main semconv repo into `open-telemetry/semantic-conventions-genai`. In semconv v1.41 every `gen_ai.*` attribute is marked "Deprecated: moved".
- The genai-repo registry (`docs/registry/attributes/gen-ai.md`, fetched 2026-11-10):
  - Spells cache writes `gen_ai.usage.cache_write.input_tokens` (renamed from `cache_creation.input_tokens`, semantic-conventions-genai #440; see PHX `trace/gen_ai/conversion.py:61-64`).
  - Adds per-modality subsets: `gen_ai.usage.{text,image,audio}.{input,output}_tokens` and `gen_ai.usage.{text,image,audio}.cache_read.input_tokens`.
  - Defines TTFT as the span attribute `gen_ai.response.time_to_first_chunk` (double, **seconds**). It has no `time_to_first_token` attribute; `gen_ai.server.time_to_first_token` and `gen_ai.client.operation.time_to_first_chunk` are *metric* names.
  - Its `gen_ai.operation.name` values add `retrieval`, `invoke_workflow`, `fetch_response`, `invoke_runner`, `plan`, `realtime_inference`, `user_speech` and the memory ops (`create_memory`, `search_memory`, …).
  - All cache, reasoning and modality counts are documented as **included** in `input_tokens` / `output_tokens`.

## 1. Laminar (Rust)

- **Detection.**
  - Span type comes from an if-chain (LMNR `traces/spans.rs:466`): `lmnr.span.type`, then `gen_ai.operation.name` (chat / text_completion / embeddings / generate_content → LLM, execute_tool → Tool), then the presence of `gen_ai.tool.call.arguments|result` → Tool, then the presence of `gen_ai.system` or a request/response model → LLM. The last step is commented as a "quick hack" (`spans.rs:499`).
  - **No OpenInference support at all**: `llm.input_messages` and `llm.token_count.*` are never read.
  - Before parsing, Vercel keys under the `aisdk.*`, `streamText.*` and similar prefixes are aliased into `ai.*` (`spans.rs:57-63, 221-320`). Alias copies never overwrite an existing key (`spans.rs:351`).
- **Input/output.** Each later step overrides the earlier one:
  1. OpenLLMetry `gen_ai.prompt.N.*`, else Vercel `ai.prompt.messages` (`spans.rs:809, 827`).
  2. OpenRouter `gen_ai.prompt` / `gen_ai.completion` (`traces/openrouter.rs`).
  3. `gen_ai.input/output.messages`, with `gen_ai.system_instructions` prepended as a system message (`spans.rs:863-889`, `1524-1564`).
  4. Tool `gen_ai.tool.call.*` (`spans.rs:897`).
  5. LiteLLM `llm.openai.messages` / `llm.anthropic.messages` (`spans.rs:975`).
  6. Vercel outer `ai.prompt` (`spans.rs:1008`) and `ai.toolCall.*` (`spans.rs:1053`).
  7. `traceloop.entity.*`, only if still empty (`spans.rs:1087`).
  8. `lmnr.span.input/output`, which always wins (`spans.rs:1119-1144`).
  - Legacy `gen_ai.*.message` / `gen_ai.choice` events are ignored.
- **Tokens.** Every spelling is normalized into a legacy underscore key (`spans.rs:231-285`):
  - cache read: `cache_read_input_tokens` ← `ai.usage.cachedInputTokens`, `ai.usage.inputTokenDetails.cacheReadTokens`, `gen_ai.usage.cache_read.input_tokens`, `gen_ai.usage.details.cache_read_tokens` (pydantic-ai), `gen_ai.usage.input_tokens.cached` (OpenRouter);
  - cache write: `cache_creation_input_tokens` ← `...cache_creation.input_tokens`, `...cache_write.input_tokens`, `ai.usage.inputTokenDetails.cacheWriteTokens`, `gen_ai.usage.details.cache_write_tokens`, plus the `...ephemeral_5m` / `ephemeral_1h` splits;
  - reasoning: `reasoning_tokens` ← `gen_ai.usage.reasoning.output_tokens`, `gen_ai.usage.output_tokens.reasoning`;
  - audio: in/out, in both spellings;
  - not aliased: Vercel v4 `ai.usage.promptTokens` / `completionTokens`.
- **Semantics.**
  - Cache is assumed to be **inside** input: regular = max(0, input − read − write) (`spans.rs:387`). A test pins the clipping to 0 when a source reports exclusive input (`spans.rs:4055`), so Anthropic-native counts are under-counted.
  - Reasoning is inside output (`language_model/costs/cost_calculator.rs:228-247`).
  - Long-context tiers use input + cache (`cost_calculator.rs:131`).
- **Provider.** `gen_ai.system` (split at the first `.`) wins over `gen_ai.provider.name`; the latter is mapped (`aws.bedrock` → `bedrock`, `gcp.vertex_ai` → `vertex_ai`, `azure.ai.openai` → `azure`, `spans.rs:452, 1301`). That precedence is backwards versus semconv.
- **Reported cost.** `gen_ai.usage.input_cost`, `gen_ai.usage.output_cost`, `gen_ai.usage.cost` (OpenLIT), `gen_ai.usage.total_cost` (OpenRouter) (`spans.rs:411-436`). Any value > 0 disables price lookup entirely (`traces/utils.rs:72`).
- **Context.**
  - session: `lmnr.association.properties.session_id`, `ai.telemetry.metadata.session_id|sessionId`, `session.id` (`spans.rs:168`);
  - user: same pattern (`spans.rs:181`);
  - tags: `lmnr.association.properties.tags|labels`, `ai.telemetry.metadata.tags` (`spans.rs:639`);
  - metadata: `lmnr.association.properties.metadata.*` ∪ `ai.telemetry.metadata.*` (`spans.rs:658`).
- **TTFT and finish reason.**
  - TTFT is not extracted at all.
  - Finish reason is left raw and read at query time (`ch/spans.rs:350-381`).
- **Tool definitions** (`traces/dedup/tools.rs:47`), in order: `ai.prompt.tools`, then `gen_ai.tool.definitions`, then `llm.request.functions.N.{name,description,parameters}`.
- **Structure.** One large imperative function with `normalize_if_absent` alias lists. Consumed payload keys are deleted, and a regex filter (`spans.rs:1315`) drops `gen_ai.(prompt|completion).N.(content|role)`; usage keys are kept on purpose.
- **Issues.**
  - TODO: `prompt_tokens` is a non-spec name (`traces/span_attributes.rs:5`).
  - TODO: `lmnr.span.output` tool calls are not parsed (`spans.rs:1142`).
  - #2706: the preview shows reasoning instead of the answer.
  - #2307: tier over-estimation.

## 2. Langfuse (TypeScript)

- **Type detection.** A priority registry of 12 mappers (LFT `:165-521`):
  - `langfuse.observation.type`;
  - `openinference.span.kind`;
  - `gen_ai.operation.name` (also accepts `completion`, `generate`; `create_agent` / `invoke_agent` → AGENT);
  - Genkit;
  - Vercel doGenerate/doStream/doEmbed. These need a model key present; matched on `operation.name` prefix or `ai.operationId` (LFT `:284`);
  - `ai.toolCall`;
  - **`gen_ai.tool.name` or `gen_ai.tool.call.id` → TOOL** (Pydantic AI, LFT `:399`);
  - LiveKit / Flue;
  - model-key fallback.
- **Input/output.** A ~20-step first-match chain (LF `:1885-2454`). Scope-gated branches come first (`ai`, `genkit-tracer`, …). Then:
  - the **`gen_ai.client.inference.operation.details` span event**, carrying `gen_ai.input/output.messages` (LF `:2134-2156`);
  - legacy `gen_ai.{system,user,assistant,tool}.message` / `gen_ai.choice` events (LF `:2158`);
  - Semantic Kernel `gen_ai.content.prompt` / `completion` events (LF `:2215`);
  - Google ADK `gcp.vertex.agent.*`;
  - Logfire;
  - `mlflow.spanInputs`;
  - `traceloop.entity.*`;
  - `input.value`;
  - pydantic-ai;
  - flattened `gen_ai.prompt.*` / `llm.input_messages.*`;
  - generic `gen_ai.input.messages` last (LF `:2447`).
  - `gen_ai.system_instructions` is prepended unless a system message already exists (LF `:2492`).
  - Bodies are stored raw. Read-time normalization lives in `utils/normalized-io/` and is flagged experimental.
- **Model.** `langfuse.observation.model.name`, `litellm.provider.model`, `gen_ai.response.model`, `ai.model.id`, `gen_ai.request.model`, `llm.response.model`, `llm.model_name`, `model` (LF `:3023-3049`). The response model is preferred.
- **No provider field.**
- **Agent spans.** Model, usage and cost are skipped when `gen_ai.operation.name` is `invoke_agent` or `agent_step` (LF `:3006`).
- **Usage.** A generic sweep over every `gen_ai.usage.*` and `llm.token_count.*` key (LF `:3318-3488`):
  - cache read accepts `cache_read.input_tokens`, `cache_read_input_tokens`, `cache_read_tokens`, `details.cache_read_tokens`, `prompt_details.cache_read`, `input_cached_tokens`;
  - cache write accepts `cache_creation.input_tokens`, `cache_write.input_tokens`, `cache_creation_input_tokens`, `cache_write_tokens`, `prompt_details.cache_write`, plus `cache_creation.{5m,1h}_input_tokens`;
  - reasoning accepts `reasoning.output_tokens`, `completion_details.reasoning`;
  - every unknown key passes through as a usage type.
  - The Vercel path (LF `:3086-3271`) also reads `ai.usage.tokens`, `cachedInputTokens`, `reasoningTokens`, and **`ai.response.providerMetadata`**: OpenAI `cachedPromptTokens`; Anthropic `usage.cache_creation_input_tokens` and the ephemeral split; Bedrock `cacheReadInputTokens` / `cacheWriteInputTokens`.
- **Semantics.** Input is made **exclusive** by subtracting cache read and write (LF `:3441-3449`). Output is made exclusive of reasoning and audio (LF `:3451`). Totals are summed when absent.
  - Open bug #10592: double-counted cached input.
  - Open bug #18588: modality breakdowns inflate totals.
  - Open bug #18565: OpenInference `prompt_details.audio` is not subtracted.
  - Closed #17117: the `cache_write.input_tokens` alias was missing, so writes were double-counted and priced at $0.
- **Cost.** `langfuse.observation.cost_details`, else `gen_ai.usage.cost`, else `llm.cost.total` (LF `:3490-3512`). Any reported cost disables inference.
- **TTFT** (LF `:3532-3574`): `langfuse.observation.completion_start_time` (ISO), else span start + `ai.response.msToFirstChunk` / `ai.stream.msToFirstChunk` (ms), else `gen_ai.client.operation.time_to_first_chunk` (seconds). `gen_ai.response.time_to_first_chunk` is not read.
- **Context.**
  - session: `langfuse.session.id`, `session.id`, `gen_ai.conversation.id`, `ai.telemetry.metadata.sessionId` (LF `:2782`);
  - user: `user.id`, `langfuse.user.id`, `ai.telemetry.metadata.userId`, then resource-level ids (LF `:2739`);
  - tags: `langfuse.trace.tags`, `ai.telemetry.metadata.tags`, `tag.tags`, accepted as an array, a JSON array, or a CSV string (LF `:3576`);
  - environment: span first, then resource (LF `:2534`).
- **Model parameters.** `langfuse.observation.model.parameters`, then `llm.invocation_parameters`, then a sweep of every `gen_ai.request.*` key minus `model` (LF `:2852-2981`).
- **Finish reason.** Extracted only on Vercel spans (LF `:2906`).
- **Tool definitions** (`packages/shared/src/server/ingestion/extractToolsBackend.ts:502-607`): `ai.prompt.tools`, `gen_ai.tool.definitions`, pydantic `function_tools`, `llm.tools.N.tool.json_schema`.
- **Structure.** Type detection uses a registry. Everything else is if-chains gated by scope name. Consumed payload keys come from a hardcoded list plus prefixes (LF `:1900-2009`). Usage, model and cost keys remain duplicated in metadata.

## 3. Phoenix (Python, OpenInference)

- **Conversion.** Native OpenInference. GenAI `gen_ai.*` is converted into OpenInference keys (PHX `trace/gen_ai/conversion.py`, targets semconv v1.41.1, `:50-64`). Conversion is skipped when no key starts with `gen_ai.` (`:75`).
- **Merge.** Synthesized keys are merged with `setdefault`, so **OpenInference wins**. Message namespaces are all-or-nothing: if any `llm.input_messages.*` exists, all synthesized ones are dropped, so differently numbered lists are never spliced together (PHX `trace/otel.py:74-94`, issue #14961). Raw `gen_ai.*` keys are kept too.
- **Kind** (`conversion.py:304-340`): chat / text_completion / generate_content → LLM; embeddings → EMBEDDING; execute_tool → TOOL; **retrieval → RETRIEVER**; invoke_agent / create_agent → AGENT. Without an operation name, kind is inferred from key presence.
- **Provider.** `gen_ai.provider.name`, else `gen_ai.system`, through a fixed table into `llm.provider` / `llm.system` (`conversion.py:343-413`). Cost lookup uses `llm.provider` only.
- **Tokens** (`conversion.py:136-163`):
  - `gen_ai.usage.input_tokens` / `output_tokens` → `llm.token_count.prompt` / `completion`;
  - `cache_read.input_tokens` → `prompt_details.cache_read`;
  - `cache_write.input_tokens`, else `cache_creation.input_tokens` → `prompt_details.cache_write`;
  - `reasoning.output_tokens` → `completion_details.reasoning`.
  - Closed #17439 fixed both the reasoning and the cache_write rename.
- **Semantics.**
  - Prompt details (`cache_read`, `cache_write`, `audio`) are a **decomposition of** the prompt count, not additive (PHX `server/api/types/Span.py:404-408`).
  - The cost calculator subtracts details from prompt and completion and prices each `token_type` row, falling back to the input or output rate (PHX `server/cost_tracking/cost_details_calculator.py:119-196`).
  - When `total > prompt + completion`, the excess is attributed to one side (PHX `server/cost_tracking/helpers.py:23-60`).
- **Reported cost.** `llm.cost.*` is **not** ingested (open #15240).
- **TTFT.** None. Span events are stored, and only `exception` is parsed.
- **Finish reason.** `gen_ai.response.finish_reasons` → `llm.finish_reason` and per-message `message.finish_reason` (`conversion.py:205-211, 474-479`).
- **Tools.** `gen_ai.tool.definitions` → `llm.tools.N.tool.json_schema` (OpenAI function shape, `conversion.py:586-653`). Tool calls → `message.tool_calls.N.tool_call.{id,function.name,function.arguments}`.
- **Bugs.**
  - Open #16916: only the last tool_call_response part is kept (`conversion.py:497-504`).
  - Every non-text part (audio, video) is written as type `image` (`conversion.py:523-529`).
  - One malformed message drops the whole list (`conversion.py:611-625`).
- **Unflatten.** A trie with sparse indices compacted (PHX `trace/attributes.py:243-353`). A node can carry both a value and children.

## 4. Opik (Java)

- **Rule table.** A flat global rule list, first match wins: Logfire, GenAI, OpenInference, LiveKit, Pydantic, LiteLLM, General, Smolagents, LangFuse (OPIK `mapping/OpenTelemetryMappingRuleFactory.java:41-68`). Each rule maps an exact key or a `startsWith` prefix to a bucket: INPUT, OUTPUT, METADATA, MODEL, PROVIDER, USAGE, COST, TAGS, THREAD_ID or DROP (`mapping/OpenTelemetryMappingRule.java:29-51`).
- **No gating.** Bare Logfire keys (`prompt`, `usage`, `result`) apply to every framework, and **unmapped keys default to INPUT** (`OpenTelemetryMapper.java:194-197`).
- **No message conversion.**
  - Vercel `ai.*`, flattened OpenInference and Traceloop keys match no rule, so they land in input as flat dotted keys. That includes outputs (open #7686).
  - Events are stored in `metadata["opentelemetry.events"]` (`mapping/OpenTelemetryEventsMapper.java:45-68`). Legacy `gen_ai.*.message` events are not lifted.
- **Span type and model.**
  - Span type is set as a side effect of matching any key that carries a type, last match wins (`Mapper:201`). `invoke_agent` is forced to `general` afterwards (`Mapper:301-303`).
  - Model is last-writer-wins between request and response (`Mapper:204-206`).
- **Provider.** `gen_ai.system` first, then `gen_ai.provider.name` (`Mapper:208-220`), then a resolver chain (`otel/ProviderResolvers.java:27-43`).
- **Usage.** The prefix is stripped, then a small rename map runs (`mapping/OpenTelemetryMappingUtils.java:27-33`).
  - Cost formats are chosen per provider: OpenAI, Anthropic, Gemini, Bedrock (`token_usage/format/UsageFormats.java:28-56`). Each format lists shape variants.
  - The Anthropic `otel_genai` variant assumes **input includes cache** and subtracts it; the `anthropic_native` variant assumes it does not (`token_usage/format/AnthropicUsageFormat.java:9-58`).
  - Heuristic override (`token_usage/format/VariantInput.java:16-47`): input < cache ⇒ exclusive; `total == input + cache + output` ⇒ exclusive.
  - Gemini: `thoughts_token_count` sits *outside* candidates (`token_usage/format/GeminiUsageFormat.java:33-74`).
- **Cost.** `gen_ai.usage.cost` is authoritative. A missing cache price falls back to the input price (`cost/SpanCostCalculator.java:167-169`).
  - Double-count guard: a parent's usage is cleared when a child has usage (`OpenTelemetryService.java:244-263`).
- **TTFT.** The model has a `ttft` field, but nothing feeds it from OTel.
- **Context.**
  - thread: `gen_ai.conversation.id` / `thread_id`;
  - no user mapping;
  - tags: only `opik.tags`.
- **Bugs.**
  - The prefix `llm.invocation_parameters.*` contains a literal `*` and never matches (`otel/OpenInferenceMappingRules.java:19`).
  - A string `gen_ai.usage.cost.currency` throws on ingest (open #8564).
  - Double usage values are dropped (`Utils:140-149`).
  - Numeric arrays become `""` (`Utils:76-80`).
  - The bare `input` prefix catches `input_tokens`.

## 5. LangWatch (TypeScript, newest design)

- **Structure.** One canonicaliser class per convention, run in a fixed order said to be load-bearing (LW `spanCanonicalisation.ts:77-109`):
  1. LangWatch
  2. GenAI
  3. Anthropic
  4. VertexAdk
  5. Mastra
  6. OpenInference
  7. Traceloop
  8. Vercel
  9. assorted coding agents
  10. Strands
  11. Logfire
  12. Haystack
  13. LegacyOtel
  14. Fallback
  15. **FreshInput**
- **Attribute bag.** `takeAttribute` reads *and deletes*; `takeAttributesByPrefix` and `takeEvents` mark events consumed (LW `canonicalAttributes.ts:8-19, 89-141`). Output uses set and set-if-absent; leftover attributes plus `out` form the result, and applied rule ids are recorded (`spanCanonicalisation.ts:135-159`). Flattened arrays are rebuilt upstream (`modules/trace/contract/src/otlp-decoding/otlp-attribute-flattening.ts:9-87`).
- **Canonical output.** The target is GenAI keys themselves: `gen_ai.input.messages`, `gen_ai.usage.*`. OpenInference `llm.input_messages`, `llm.model_name` and `llm.invocation_parameters` are folded in during the GenAI pass (LW `genAiSpan.ts:62-95`).
- **Tokens.**
  - Canonical cache read is `gen_ai.usage.cache_read.input_tokens`, with aliases `cache_read_input_tokens` (OpenLLMetry/Strands), `cached_input_tokens`, `llm.token_count.prompt_details.cache_read`, `ai.usage.inputTokenDetails.cacheReadTokens`, `ai.usage.cachedInputTokens`.
  - Canonical write is `cache_creation.input_tokens`, with aliases `cache_creation_input_tokens`, `cache_write_input_tokens`, `cache_write.input_tokens`, `prompt_details.cache_write`, `ai.usage.inputTokenDetails.cacheWriteTokens` (`genAiSpan.ts:182-277`).
  - `reasoning.output_tokens` is moved to `reasoning_tokens`.
  - Anthropic `anthropic.usage.cache_creation.ephemeral_{5m,1h}_input_tokens` are split out (`anthropicCanonicaliser.ts:58-99`).
- **Semantics.** The canonical input is **exclusive** (fresh). The FreshInput pass subtracts cache once and skips when:
  - the source already sends fresh input;
  - `total == input + output + cache`;
  - input < cache (`freshInputCanonicaliser.ts:13-58`).
  - **Vercel:** v6 reports inclusive input with `ai.usage.inputTokenDetails.noCacheTokens`. **v5 + Anthropic provider reports exclusive input**, other v5 providers inclusive (LW `vercel/vercelCore.ts:84-152`).
  - Reasoning is inside output and not priced separately.
- **TTFT.**
  - At canonicalisation, `gen_ai.response.time_to_first_chunk` (s) is converted to `gen_ai.server.time_to_first_token` (ms) (`genAiSpan.ts:191-203`).
  - At read time, first-token **events** win: `gen_ai.content.chunk`, `llm.content.completion.chunk`, `first_token`, `llm.first_token`, `ai.stream.firstChunk`, "First Token Stream Event".
  - Then attributes: `gen_ai.server.time_to_first_token`, `ai.response.msToFirstChunk`, `langwatch.timestamps.first_token_at` (`modules/trace/contract/src/features/span/span-cost-metrics.ts:14-29, 191-284`).
- **Provider.** `gen_ai.system` **overwrites** `gen_ai.provider.name` (`genAiSpan.ts:47-51`), a bug.
- **Legacy events.** Legacy `gen_ai.*.message` events are converted only on Strands spans (`strandsCanonicaliser.ts:149-179`).
- **Other gaps.**
  - Unflatten requires uniform sub-keys per index, so a mixed tool_calls list stays flat (`otlp-attribute-flattening.ts:48-76`).
  - `llm.token_count.total` is dropped before the FreshInput check, so OpenInference always has cache subtracted.

## Cross-cutting observations

- **Token semantics disagree.** Everyone agrees that OTel GenAI `input_tokens` includes cache and that reasoning ⊂ output; our spec matches the semconv. The failure mode is *sources that lie*:
  - Vercel v5 + Anthropic: exclusive input, also mirrored into `gen_ai.usage.input_tokens` on the same span.
  - Anthropic-native or LiteLLM shapes.
  - Gemini native: thoughts outside candidates.
  - LangWatch and Opik guard against these with a **shape heuristic** (input < cache ⇒ exclusive; total = input + cache + output ⇒ exclusive). Laminar does not, and clips to 0.
- **Structure.** LangWatch's take-and-delete bag is the cleanest way to implement our rule 2 (consumed attributes) and our per-field precedence. Opik's prefix table shows the hazards of unanchored prefixes (`input` matching `input_tokens`, a literal `*`). Phoenix shows that converting one convention into another is lossy (audio as `image`, last tool response wins).
- **Agent spans carrying aggregate usage.** Strands, the OpenAI Agents SDK and Pydantic AI put usage on `invoke_agent` spans. Langfuse (LF `:3006`) and Opik drop it. Our rule 3 does the same: tokens only on llm/embedding.
- **Encoding.** Payload attributes arrive as a JSON string, a double-encoded JSON string (LMNR `traces/utils.rs:293`; Opik `Utils:44-83`), or a structured `kvlistValue` / `arrayValue`. Spec 02 does not say we accept all three for `gen_ai.input.messages` and similar keys.

## Candidate TTFT sources per convention

| Convention | Source | Unit / kind | Seen in |
|---|---|---|---|
| llmobs | `llmobs.ttft_ns` | ns | spec |
| GenAI (genai repo) | `gen_ai.response.time_to_first_chunk` | double seconds, attr | semconv, LW `genAiSpan.ts:191` |
| GenAI (non-standard) | `gen_ai.client.operation.time_to_first_chunk`, `gen_ai.server.time_to_first_token` | metric names reused as attrs (s / ms, ambiguous) | LF `:3555`, LW |
| Vercel | `ai.response.msToFirstChunk` (doStream), `ai.stream.msToFirstChunk` (streamText); event `ai.stream.firstChunk` | ms; event time | LF `:3556-3558`, LW |
| Langfuse | `langfuse.observation.completion_start_time` | ISO timestamp | LF `:3532` |
| OpenInference | event "First Token Stream Event" | event timestamp − start | LW read-time list (unverified emitter) |
| OpenLLMetry | events `llm.content.completion.chunk` / `gen_ai.content.chunk` (first one) | event timestamp | LW (unverified) |
| LangWatch | `langwatch.timestamps.first_token_at` | unix ms | LW |
| Laminar, Google ADK | none found | — | — |

## Gap table vs spec 02

| # | Area | Spec 02 today | Competitors / semconv | Who is right | Recommendation |
|---|---|---|---|---|---|
| 1 | Cache-write key | only `gen_ai.usage.cache_creation.input_tokens` | the genai repo renamed it to `gen_ai.usage.cache_write.input_tokens` (#440); Phoenix, Laminar, Langfuse and LangWatch read both. Langfuse #17117 was exactly this miss | semconv | Read `cache_write.input_tokens` first, then `cache_creation.input_tokens`. Add a fixture. Keep chat-cache-tokens on the old key |
| 2 | Underscore cache keys (OpenLLMetry, Strands, LiteLLM) | missing | `gen_ai.usage.cache_read_input_tokens`, `gen_ai.usage.cache_creation_input_tokens`, `cache_write_input_tokens`, `gen_ai.usage.details.cache_{read,write}_tokens`, `gen_ai.usage.input_tokens.cached` (LMNR `spans.rs:231-285`; LW `genAiSpan.ts:207-234`) | competitors | Add them as lower-precedence aliases in the OpenLLMetry/fallback slot |
| 3 | Vercel cache and reasoning keys | `ai.usage.cachedInputTokens`, `ai.usage.reasoningTokens` only | v6 `ai.usage.inputTokenDetails.{noCacheTokens,cacheReadTokens,cacheWriteTokens}`, `ai.usage.outputTokenDetails.reasoningTokens`, `ai.usage.totalTokens`/`tokens`; Anthropic cache write only in `ai.response.providerMetadata` (LF `:3086-3271`) | competitors | Add the v6 detail keys. Read cache write from `providerMetadata.anthropic.usage.cache_creation_input_tokens` when absent |
| 4 | Vercel v5 + Anthropic token semantics | "converters add back" stated generically | v5 Anthropic reports **exclusive** input, also mirrored to `gen_ai.usage.input_tokens` on the same span, so GenAI precedence picks up the wrong semantics (LW `vercelCore.ts:114-145`) | LangWatch | Key the semantics fix on scope `ai` + provider, not on the attribute prefix. Rule: no `inputTokenDetails` and provider anthropic ⇒ input += cache read + write. Pin with a captured fixture |
| 5 | Exclusive-input safety net | none | Opik `VariantInput.java:16-47` and LW FreshInput: input < cache ⇒ exclusive; total = in + cache + out ⇒ exclusive | competitors | Add a normalization rule: if `input_tokens < cache_read + cache_write`, treat as exclusive, add back, and record `normalize_warning` in attributes. Never clip silently (Laminar bug) |
| 6 | Reasoning keys | `reasoning.output_tokens`, OI, Vercel | also `gen_ai.usage.reasoning_tokens`, `gen_ai.usage.output_tokens.reasoning`; Gemini-native thoughts are **outside** output (Opik `GeminiUsageFormat.java:33-74`) | competitors | Add the aliases. ADK/Gemini-native converter adds thoughts to output |
| 7 | Modality tokens | not mentioned | `gen_ai.usage.{audio,image,text}.*`, `llm.token_count.{prompt,completion}_details.audio`; they are subsets, and Langfuse #18588/#18565 inflate totals by summing them | semconv | Do not add them to totals or columns. Keep them in `attributes` (small) and document "subset, not additive" |
| 8 | Reported cost | `llmobs.cost.total`, `llm.cost.total`, `gen_ai.usage.cost` → total only | `llm.cost.prompt/completion` (OI), `gen_ai.usage.input_cost/output_cost`, `gen_ai.usage.total_cost` (OpenRouter); `gen_ai.usage.cost.currency` crashes Opik (#8564) | mixed | Map input/output parts to `cost_input`/`cost_output` when present. Accept `total_cost`. Ignore non-numeric `cost.*` siblings. Specify that a partial report (input only) does not fall back to price lookup for the other half, or state that it does |
| 9 | Provider precedence and aliases | `gen_ai.provider.name` > `gen_ai.system` ✓; alias table only for `openai.chat`-style | Laminar, Opik and LangWatch get the precedence backwards; Phoenix and Laminar map semconv values (`aws.bedrock`, `gcp.vertex_ai`, `gcp.gemini`, `azure.ai.openai`, `x_ai` vs `xai`) | spec ✓ | Keep the precedence. Extend the alias table with semconv well-known values and Vercel ids (`google.generative-ai`, `amazon-bedrock`, `azure.chat`). Note the `xai`/`x_ai` mismatch |
| 10 | Span kind from `gen_ai.operation.name` | chat / text_completion / generate_content / response, embeddings, execute_tool, invoke/create_agent | `retrieval` → retriever (Phoenix), `invoke_workflow` → chain, plus `completion` / `generate` (Langfuse); memory ops are new | semconv | Add `retrieval` → retriever and `invoke_workflow` / `invoke_runner` / `plan` → chain. Memory ops and unknown values → `span`. `response` is not a semconv value; keep it only if seen in fixtures |
| 11 | Kind fallback | model present → llm | `gen_ai.tool.name` / `gen_ai.tool.call.id` / `.arguments` → tool before the model check (LFT `:399`, LMNR `spans.rs:497`) | competitors | Add the tool-key fallback ahead of the model fallback |
| 12 | Messages carried in events | legacy per-message events only | semconv content event `gen_ai.client.inference.operation.details` carries `gen_ai.input/output.messages` as attributes (LF `:2134-2156`); Semantic Kernel `gen_ai.content.prompt/completion` events (LF `:2215`); OpenRouter plain `gen_ai.prompt`/`gen_ai.completion` JSON | Langfuse | Add the operation-details event to the GenAI row, ranked just after attributes. Consider SK events. Mark consumed events as removed (LW `takeEvents`) |
| 13 | Legacy events reality | listed as span events | in semconv they were *log records*; on spans they appear only via bridges (Logfire, Strands). LangWatch handles them only for Strands | — | Keep them, but source fixtures from Strands/Logfire. Do not expect OTel Python instrumentations to put them on spans |
| 14 | Tool definitions sources | `gen_ai.tool.definitions` only (rule 2 mentions `llm.tools.*`) | `llm.tools.N.tool.json_schema` (OI), `ai.prompt.tools` (array of JSON strings), `llm.request.functions.N.{name,description,parameters}` (OpenLLMetry) (LMNR `traces/dedup/tools.rs:47`) | competitors | Add all three to the Input/output table as `tool_definitions` sources, normalized to one element per tool |
| 15 | Vercel message shapes | "Vercel CoreMessage" | v4 tool-call `args`, v5 `input`; tool-result `result` vs `output {type,value}`; `image`/`file` parts with **raw base64, no data URI** | — | Specify the v4/v5/v6 part mapping in the converter. Add Vercel `image`/`file` (`data`, `mediaType`) to media extraction candidates |
| 16 | Payload encoding | unspecified | JSON string, double-encoded string, or structured kvlist/array AnyValue all occur (LMNR `traces/utils.rs:293`) | — | State that message-bearing attributes accept a string (parse up to 2 levels) or structured AnyValue |
| 17 | OI vs GenAI message precedence | GenAI > OpenInference per field | Phoenix: OI wins, all-or-nothing per namespace (#14961) | consistent | Keep ours, but specify all-or-nothing: never merge message lists from two conventions |
| 18 | TTFT | Vercel `ai.response.msToFirstChunk` only; rest TBD | see TTFT table; semconv `gen_ai.response.time_to_first_chunk` (seconds) | semconv | Add `gen_ai.response.time_to_first_chunk` × 1e9, `ai.stream.msToFirstChunk`, `langfuse.observation.completion_start_time` − start. Events (`ai.stream.firstChunk`, "First Token Stream Event") only once a captured fixture shows them. Ignore metric-named attributes unless captured |
| 19 | Laminar context fields | Laminar is a convention, but no context keys listed | `lmnr.association.properties.{session_id,user_id,tags,metadata.*}`, `lmnr.span.release` (LMNR `spans.rs:168-690`) | Laminar | Add them to the context table at Laminar precedence |
| 20 | Langfuse fields | input/output/session/user/tags/metadata only | `langfuse.observation.{model.name,model.parameters,usage_details,cost_details,completion_start_time,level}`, `langfuse.trace.name`, `langfuse.user.id` alias handling (LF ATTR `:3-62`) | Langfuse | Map `usage_details` and `cost_details` (JSON) to tokens/cost, `model.parameters` to `request_params`, and `completion_start_time` to TTFT |
| 21 | Tags formats | string array | Langfuse accepts a JSON-array string and CSV; `ai.telemetry.metadata.tags` (Laminar, Langfuse) | competitors | Accept an array or a JSON-array string. Add `ai.telemetry.metadata.tags`. Skip CSV (ambiguous) |
| 22 | `request_params` | an explicit key list | Langfuse sweeps every `gen_ai.request.*` minus `model` (LF `:2852-2981`); semconv adds `gen_ai.request.stream`, `choice.count`, `encoding_formats`, `gen_ai.output.type`, `service_tier` | competitors | Sweep `gen_ai.request.*` minus `model`. Keep `gen_ai.request.stream` (useful for TTFT) |
| 23 | Finish reason | rule 16 for flat Vercel only | Phoenix maps `gen_ai.response.finish_reasons` → per message; OpenLLMetry `gen_ai.completion.N.finish_reason` | Phoenix | For GenAI output messages, take each message's own `finish_reason`; when absent, fill from `finish_reasons[i]`. OpenLLMetry: map `.finish_reason` per index |
| 24 | Total tokens | `llm.token_count.total`, `llm.usage.total_tokens` | also `gen_ai.usage.total_tokens`, `ai.usage.totalTokens`; LW drops OI total before using it for the semantics check | — | Add the aliases. Use reported total as the exclusive-input heuristic input (gap 5) before discarding |

Unverified: OpenLLMetry Anthropic `input_tokens` semantics (inclusive or exclusive of cache), Google ADK `gen_ai.usage.*` semantics, and which emitters really produce the first-token events. The captured fixtures listed in spec 02 § Testing should pin all three.
