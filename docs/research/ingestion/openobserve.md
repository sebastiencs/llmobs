# OpenObserve OTLP trace ingestion — research notes for llmobs B1 (`llmobs-otel`, spec 02)

Repo: `/home/sebastien/travaux/openobserve`, local checkout HEAD `8dcaf5e8e` (2026-09-20). Paths are relative to `src/` unless they start with `tests/` or `Cargo.toml`. This session did not run `git` on that repo. History comes from `gh search issues/commits` and `gh issue/pr view` on `openobserve/openobserve`, up to 2026-11-10. Issues and PRs newer than HEAD are marked "(post-HEAD)": the local code still has those bugs.

Short version: OpenObserve converts every span to a **flat JSON object**, one column per attribute key, with no schema declared up front. An LLM layer (`core/src/traces/otel/`, about 8.8k lines) runs only on spans detected as GenAI. It **rewrites the span's attributes in place**: it adds canonical `gen_ai.*` keys (input/output messages, model, provider, usage, cost, agent, tool, evaluation) and deletes the payload attributes it consumed. It recognises many conventions: GenAI, OpenLLMetry/Traceloop, OpenInference, Vercel AI, Langfuse, Google ADK, Logfire, MLflow, LiveKit, Pydantic-AI and SmolAgents. Its precedence rules are **ad hoc and differ for each field**. Message payloads are **not** converted to a canonical form: the raw value is copied. Its recent bug list reads like a checklist for our spec 02.

---

## 1. OTLP decoding

- **Crate: upstream `opentelemetry-proto` 0.32 with `gen-tonic` and `with-serde`** (`Cargo.toml:520-527`). There is no custom OTLP model.
  - Protobuf: `ExportTraceServiceRequest::decode(body)` (`core/src/traces/mod.rs:453`).
  - JSON: `serde_json::from_slice::<ExportTraceServiceRequest>` (`mod.rs:485`). Both go to `handle_otlp_request` (`mod.rs:509`).
- **JSON behaviour comes entirely from the crate's serde derive.** I read `opentelemetry-proto-0.33.1/src/proto.rs` from our cargo registry; 0.32 is not cached locally.
  - IDs are **hex only**, with `const_hex::decode` (`proto.rs:30-50`). There is no base64 fallback and no length check at decode time.
  - `intValue` and the fixed64 times accept a number or a string (`proto.rs:76-90, 180-215`). `bytesValue` is base64.
  - `kind` and `status.code` are plain `i32`, so enum names such as `"SPAN_KIND_SERVER"` are rejected.
- **One bad value rejects the whole request.** Serde decodes the request in one pass, so a single unexpected value fails the full export with `400 Invalid json`. The JSON fixes are all about this:
  - #14599: `doubleValue` broke under `serde_json/arbitrary_precision`, and any fractional attribute dropped the batch.
  - #15999 (post-HEAD): an empty `AnyValue {}` or an empty `arrayValue`/`kvlistValue` rejected the batch; Cloudflare Workers sends these.
  - #12615: the PR states that the crate only "partially implement[s] the proto3 JSON mapping".
- **HTTP** (`api/search/src/traces/mod.rs:106-157`):
  - The content type is matched by **exact equality** on `application/x-protobuf`, or a `starts_with` on JSON. A missing header means JSON. Anything else gets `400 "Bad Request"`.
  - An org header (`cfg.grpc.org_header_key`) **overrides the org in the path** (`:130-137`).
  - Decompression is done by `tower_http::RequestDecompressionLayer` (gzip, deflate, br, zstd; `Cargo.toml:397-410`; `api/http/src/handler/http/router/mod.rs:1924`). A middleware strips `snappy` so that handlers can decode it themselves (`router/decompression.rs`).
  - The body limit is `DefaultBodyLimit::max(ZO_PAYLOAD_LIMIT)`, **200 MiB** by default (`router/mod.rs:2090`, `config/src/config.rs:2329`). It is applied by the `Bytes` extractor, so as far as I can tell it limits the decompressed body.
- **gRPC:** a router node forwards to an ingester over tonic with gzip and `ZO_GRPC_MAX_MESSAGE_SIZE`, **32 MiB** by default (`api/grpc/src/router/grpc/ingest/traces.rs:65-70`, `config.rs:1472-1477`). The ingester calls the same `handle_otlp_request` and maps the HTTP status to a gRPC code (`api/grpc/src/handler/grpc/request/otlp.rs:27-66`; 503 maps to `UNAVAILABLE`, other 4xx to `INVALID_ARGUMENT`). This mapping is the fix for #14640: before it, overload and write errors were answered with gRPC OK, and `partial_success` was dropped.

## 2. Attributes: flattening, typing, merging

- **`AnyValue` → JSON loses types** (`core/src/ingestion/grpc.rs:22-55`):
  - bool, int and double all become **JSON strings** (`"true"`, `"42"`, `"0.5"`);
  - arrays and kvlists stay JSON arrays and objects;
  - **bytes are decoded as UTF-8, with `unwrap_or_default`**, so invalid UTF-8 is silently stored as `""`. Before that change it was an `unwrap`, and the server panicked: #4297;
  - `StringValueStrindex` becomes `null`.

  A typed variant, `get_val_with_type_retained` (`:117`), exists but the trace path does not use it. Because of this, every numeric `gen_ai.*` field has to be parsed back from a string (`otel/extractors/utils.rs:18-28`) and cast to its type again after flattening (`traces/mod.rs:134-192`).
- **Merge by `serde(flatten)`.** The record is a `Span` struct (`common/src/meta/traces.rs:23-45`) in which `reference`, `attributes` and `service` (the resource attributes) are all `#[serde(flatten)]` into a single JSON object (`traces/mod.rs:853-889`).
  - Resource keys get a `service_` prefix, or `service_attr_` when they collide with a reserved name (`resource_attribute_key`, `:431-440`).
  - Span keys get an `attr_` prefix when they collide with one of the 33 `RESERVED_SPAN_FIELDS` or with a resource key (`span_attribute_key`, `:419-429`, `:87-124`).
  - **Scope attributes are dropped**: only `scope.name` is read, for LLM detection (`:695`).
  - `status.code`/`status.message` are copied into the attributes (`:711-714`, issue #4851).
  - Events and links are serialised as **JSON strings** in `events`/`links` columns (`:867-876`).
- **Key sanitisation and nesting** (`config/src/utils/flatten.rs`):
  - `format_key` lowercases each key and maps every character that is not alphanumeric or `_` to `_` (`:156-180`, which uses `unsafe as_bytes_mut`). So `gen_ai.usage.input_tokens` becomes `gen_ai_usage_input_tokens`, and `ai.usage.promptTokens` becomes `ai_usage_prompttokens`.
  - Objects (kvlist values) are flattened with `_` (`:93-130`). Arrays are **stringified as JSON** (`:135-153`).
  - The doc comment says collisions are an error (`:28-33`), but the code does a plain `Map::insert`. **Collisions are silent last-write-wins**: `a.b` and `a_b`, or `Foo` and `foo`, end up in one column.
- **Columns:** each distinct key becomes a stream column, and its type is inferred from the first value seen. The default is Utf8, because scalars are strings. LLM streams get a fixed set of `gen_ai_*` fields typed up front (Int64 for tokens, Float64 for cost and TTFT; `db/src/schema.rs:~200-275`).
  - **`ZO_COLS_PER_RECORD_LIMIT` = 1000** (`config.rs:2482`). Above it, `check_request_columns_limit` (`schema/src/lib.rs:96-116`) fails `check_batch_schema`, and the **whole request** gets a 400 (`traces/mod.rs:1693-1703, 1133-1137`).
  - More than 1000 schema fields automatically switches the stream to a user-defined schema (UDS), which then drops undeclared keys (`config.rs:2352-2357`, `refactor_map` at `traces/mod.rs:1223-1228`).

## 3. Pipeline shape

- `handle_otlp_request` (`traces/mod.rs:509-1175`) is **one 670-line async function**. For each span it:
  1. validates the IDs;
  2. builds the attribute maps;
  3. if `is_llm_trace` matches, runs `OtelIngestionProcessor::process_span_with_pricing_and_agent_mapping` on the mutable span attribute map (`:696-748`);
  4. runs inferred-service, service-graph and DB-monitoring enrichments (`:803-851`);
  5. serialises the `Span` to a JSON value, then `flatten` (`finalize_and_buffer_trace_span`, `:1182-1236`).

  Batches then pass through optional VRL/LLM-evaluation pipelines (`:926-1076`), the SDR redaction regexes (`:1101-1122`), the trace time index, alert evaluation, and finally the WAL (`write_traces`, `:1642-1798`).
- **The LLM processor is "struct of extractors", not table-driven** (`otel/processor.rs:67-102`). It has 11 unit-struct extractors with concrete methods; there is no trait. It runs in three phases (`:141-170`):
  - `extract_all` reads everything into a `SpanExtractions` struct (`:45-65`);
  - it then deletes the I/O attributes (`retain(!is_input_output_attribute)`);
  - `compute_usage_and_cost` and `emit_enriched_attributes` write back canonical `gen_ai.*` keys (`:366-635`).

  The **output is the same attribute map**, not a typed struct. Field precedence is encoded as ordered key arrays inside each extractor.
- **Performance tricks:**
  - `is_llm_trace` is a cheap `contains_key` gate, so non-LLM spans skip the processor (`extractors/gen_ai_operation.rs:187-264`);
  - `format_label_name_cow` avoids allocations (`flatten.rs:202-208`);
  - the resource key set is normalised once per resource (`traces/mod.rs:411-417`);
  - org pricing is preloaded once per request (`:617-626`).

  These are small next to the cost of `HashMap<String, Value>` → `serde_json::Value` → flatten → Arrow for every span, plus `resource.clone()` per span (`:865`). #14910 is a perf PR on DB-monitoring enrichment cost in this same loop.
- **Request-scoped bug:** `service_name` is declared **outside** the resource loop (`:609`). A `ResourceSpans` without `service.name` therefore inherits the previous resource's name. The LLM fallback, which takes the service name from span attributes (`:697-705`), also changes it for every later span in the request.

## 4. Validation

- **IDs:**
  - a span is rejected (counted in `rejected_spans`) when the trace ID length is not 16 or the span ID length is not 8 (`traces/mod.rs:657-670`);
  - **all-zero IDs are accepted**;
  - a parent ID of the wrong length is **silently treated as a root** (`:674-683`);
  - links with bad IDs are dropped without being counted (`:756-769`);
  - `flags` is hard-coded to `1`, with `// TODO add appropriate value` (`:866`).
- **Time window:**
  - `_timestamp = start_time / 1000` (µs) must fall inside `[now − ZO_INGEST_ALLOWED_UPTO, now + ZO_INGEST_ALLOWED_IN_FUTURE]`, which defaults to **5 h in the past and 24 h in the future** (`config.rs:2448-2452`, check at `traces/mod.rs:558-560, 784-798`);
  - `start_time = 0` is therefore rejected as "too old";
  - **`end < start` is accepted**, and the duration saturates to 0 (`span_duration_micros`, `:442-445`);
  - the 5 h default makes backfills and delayed batch exports fail by default (#14293 hit it).
- **Partial success (`format_response`, `:1550-1583`):**
  - the `error_message` is **hard-coded** to "exceeding the allowed retention period", **whatever the actual reason** (a bad ID, a pipeline error, a flatten failure);
  - for OTLP/JSON, any rejection returns **HTTP 206 Partial Content**, which is not OTLP: the spec requires 200;
  - in a single request, one span that fails `finalize_and_buffer_trace_span` returns **500 for the whole request** (`:893-920`).
- **The LLM layer never rejects anything.** Bad numbers are coerced: `normalize_llm_field_types` turns non-numeric token values into `get_int_value(...)` and non-finite floats into `0.0` (`:172-192`).

## 5. Size limits and truncation

- **There is no per-attribute or per-payload truncation** in the trace path. The only size limits are the request body (200 MiB HTTP, 32 MiB gRPC) and the 1000-column limit. A 5 MB prompt is stored whole, and **twice** when it arrived as GenAI events: the `events` JSON column is never pruned, while the extracted copy goes to `gen_ai.input.messages` (`input_output.rs:271-325`, `traces/mod.rs:867-871`).
- **Flattened message arrays are capped at index 100** (`MAX_INDEX`, `input_output.rs:31`). `build_array_from_keys` skips indices above it (`:538`), but `set_nested_value` **clamps** them to 100 (`:627`). So nested indices above 100 are merged into element 100 instead of being dropped.
- `ZO_INDEX_ALL_MAX_VALUE_LENGTH` (`config.rs:2806`) only limits full-text indexing, not storage.

## 6. LLM/GenAI handling, compared with our spec 02

- **Detection** (`gen_ai_operation.rs:187-264`):
  - any of about 25 keys marks a span as LLM;
  - this includes the bare generic keys **`input`, `input.value` and `prompt`**, and any span with scope `ai`;
  - false positives are likely, for example any non-LLM span that has an `input` attribute;
  - the stream is then marked `is_llm` (`traces/mod.rs:743-747, 1149-1159`).
- **Operation/kind** (`map_to_gen_ai_operation_name`, `:57-185`) returns a GenAI operation name, not an enum:
  - precedence: `gen_ai.operation.name` (if it is one of 9 known values) > `llm.request.type` > `traceloop.span.kind` > `openinference.span.kind` > `langfuse.observation.type` > Vercel scope `ai` + a model key → `chat` > tool keys → `execute_tool` > any model key → `chat` > `span`;
  - our order puts OpenInference ahead of Traceloop and Langfuse; theirs is the reverse;
  - **every Vercel span with a model is `chat`**, including the outer `ai.generateText`. Our spec maps it to `chain` precisely to avoid double counting;
  - `response` (OpenAI Responses) is not a known value.
- **Input/output** (`input_output.rs:38-160`): **one convention wins for both fields together**, in this order:
  1. GenAI events (`gen_ai.*.message`, `gen_ai.choice`);
  2. Langfuse;
  3. `traceloop.entity.*`;
  4. Traceloop nested `gen_ai.prompt.*`/`gen_ai.completion.*`;
  5. `gen_ai.input/output.messages`;
  6. `gen_ai.tool.call.*`;
  7. Semantic Kernel events;
  8. Vercel;
  9. ADK `gcp.vertex.agent.*`, with a `"{}"` → tool args/response swap;
  10. Logfire, LiveKit, MLflow, `input.value`, `input`, Pydantic tool args;
  11. OpenInference `llm.*_messages.*` **last**.

  The standard `gen_ai.input.messages` therefore loses to legacy formats and to Langfuse, which is the opposite of our precedence. Values are copied as-is, as a string or JSON. **There is no canonical message conversion.** Flattened keys are rebuilt into nested JSON (`convert_key_path_to_nested_object`, `:447-595`). The result is written to `gen_ai.input.messages`/`gen_ai.output.messages` (`processor.rs:392-404`), and the UI handles each shape. #14127: the UI went blank on GenAI v5 `parts` messages.
- **Prefix-match bug, #16271 (fixed post-HEAD):** `starts_with("gen_ai.prompt")` (`input_output.rs:222, 407`) also matches the semconv attributes `gen_ai.prompt.name`/`.version`. As a result:
  - `{name, version}` became the input;
  - the real `gen_ai.input/output.messages` were deleted.

  The lesson is to match `gen_ai.prompt.<digit>`, not a raw prefix.
- **Model:** `gen_ai.response.model` > `ai.model.id` > `gen_ai.request.model` > Langfuse > `llm.response.model` > `llm.model_name` > `model` (`extractors/model.rs:29-45`). The result is **written back as `gen_ai.response.model`** (`processor.rs:378-383`), so a request-only model looks like a response model. We keep `request_model` and `response_model` separate.
- **Provider:** `gen_ai.provider.name` > `gen_ai.system` > `ai.model.provider` > Langfuse `ls_provider` (`provider.rs:28-44`). It is **not lowercased or aliased**: `openai.chat` stays as it is. Our rule 12 covers this.
- **Usage** (`extractors/usage.rs:52-290`):
  - **last write wins**: GenAI, then `prompt_cache_miss`, then OpenInference, then Vercel `ai.*` (scope-gated), then Langfuse `usage_details` all overwrite the same keys. So OpenInference beats GenAI when both are present;
  - cache reads come from a six-way `else if` chain (GenAI, Anthropic raw, OpenAI `prompt_tokens_details.cached_tokens`, `cache_read_tokens`, `cached_tokens`, DeepSeek `prompt_cache_hit_tokens`);
  - one **span-wide `input_includes_cache` boolean** is toggled by whichever source matched last; this is their version of our "token semantics" note;
  - **reasoning tokens are not extracted at all**;
  - `set_val_if_not_zero` drops explicit zeros (`extractors/mod.rs:61-69`).
- **Estimated tokens:** when a generation or embedding span has I/O but no usage, OpenObserve **counts tokens with tiktoken**, falling back to `o200k_base` for unknown models, over `value.to_string()` of the JSON payload (`processor.rs:275-289`, `pricing.rs:30-37`). The estimate includes JSON punctuation, is wrong for non-OpenAI models, and is stored like reported usage, with no flag.
- **Zero defaults:** `input`, `output` and `total` default to **0** on every LLM span, and `cost.total` defaults to **0.0** (`processor.rs:337-359`). So agent and tool spans report "0 tokens, $0" instead of null (#16012/#16024, open). Tokens are recorded on `invoke_agent` spans as well as on `chat` spans, and sums double count them (#16272, open).
- **Cost:**
  - reported costs (`gen_ai.usage.cost`, Langfuse `cost_details`) win;
  - otherwise an org pricing definition is used: Langfuse-style tiers with prices keyed by usage key, `valid_from` and source priority Org > MetaOrg > BuiltIn (`db/src/model_pricing.rs:230-280`), plus tier conditions and **UTC time-of-day windows** for DeepSeek peak pricing (`:375-404`);
  - otherwise a **hard-coded table of about 60 regexes** (`otel/pricing.rs:222-460`, "as of March 2026"). It is **unanchored**, first match wins, and the order is maintained by hand (`gpt-4o` must come before `gpt-4`). It only prices input and output, **ignoring cache prices**;
  - built-in definitions are synced every 6 h from a GitHub JSON file (`builtins/src/model_pricing.rs:65-140`, `config.rs:2210-2219`). So **cost depends on network sync timing**;
  - tiers are selected with cache-inclusive input (`build_pricing_usage`, `processor.rs:672-705`);
  - extra columns: `estimated_without_cache`, `cache_read_savings` and `net_cache_impact` (`:727-774`);
  - #10992: cache tokens were once left out of cost.
- **TTFT bug:** Langfuse `completion_start_time` is an **absolute** timestamp. It is parsed to µs and written as `gen_ai.response.time_to_first_chunk = ts / 1e6` (`processor.rs:208-210, 520-527`), which is epoch seconds, not a latency. No test asserts its value.
- **Params:**
  - Vercel settings (`maxSteps`, `toolChoice`, and also `finishReason` and `system`), `llm.invocation_parameters` and Langfuse `model.parameters` are **re-emitted as `gen_ai.request.<key>`** (`parameters.rs`, `processor.rs:407-412`);
  - user-controlled JSON keys therefore become **new columns**, which brings stream columns closer to the 1000-column limit (§2);
  - we store a single `request_params` JSON value instead.
- **Context:**
  - user: `user.id` > `ai.telemetry.metadata.userId` > `service_user.id`;
  - session: `session.id` > `gen_ai.conversation.id` > Vercel > Langfuse metadata > `service_session.id` (`metadata.rs:38, 72`);
  - the `service_` lookups exist because resource keys are already prefixed when the processor runs. The extractors must know about the storage prefix: a layering leak;
  - session is written back as `gen_ai.conversation.id`;
  - there is no `tags`/`metadata` model.
- **Not handled** (all covered by our spec): media extraction, message truncation, canonical messages, previews, `system_instructions` as a separate field (a schema column exists but no extractor fills it), `tool_definitions`, Laminar `lmnr.*`, Vercel `ai.usage.*` naming, and the `normalize_error` attribute.

## 7. Tests and fixtures

- The tests are **hand-built `HashMap` unit tests** for each extractor: about 300 `#[test]` in `traces/otel/**` (69 in `gen_ai_operation.rs`, 31 in `pricing.rs`, 24 in `processor.rs`) and 69 in `traces/mod.rs`. Examples: `processor.rs:793-1200` (removing I/O attributes, provider, Langfuse, DeepSeek peak pricing) and `traces/mod.rs:1940` (`test_normalize_llm_field_types`).
- **There are no captured OTLP fixtures and no golden tests** for real instrumentations. The TraceLoop prefix bug (#16271) and the TTFT unit bug would both have been caught by a fixture from a real SDK.
- End-to-end tests:
  - `tests/trace_input.json` (one OTLP/JSON request) with `tests/integration_test.rs`;
  - Python API tests in `tests/api-testing/tests/ingest/test_traces.py`;
  - Playwright tests `tests/ui-testing/playwright-tests/Traces/traceGenAiParts.spec.js` and `GeneralTests/model-pricing.spec.js`.
- An API fuzz job exists: it found the non-ASCII trace-ID panic, #15866 (post-HEAD).

## 8. Mistakes and known issues

- **Decoding:** #14599 (doubles), #15999 (empty AnyValue, post-HEAD) and #12615 (metrics int/histogram over JSON). In all three, one value rejected or emptied the whole batch, because the crate's serde was trusted.
- **Wire semantics:**
  - #14640: gRPC answered errors with OK and dropped `partial_success` (fixed at HEAD);
  - #14293: a JSON export got a protobuf response labelled `application/json` (logs, fixed by #14674);
  - the hard-coded partial-success message and HTTP 206 are still present (§4).
- **Panics on input:** #4297 (`unwrap` on bytes UTF-8) and #15866 (byte slicing of a trace ID). `traces/mod.rs` still has `try_into().unwrap()` after a length check (`:663, 672, 680, 763, 771`, which is safe) and `json::to_string(..).unwrap()` (`:870, 875, 882`).
- **LLM semantics:**
  - #16271: a prefix match deleted the real messages;
  - #16272 (open): tokens are double counted on agent and chat spans;
  - #16012/#16024 (open): agent spans are stored with zero usage and cost instead of null, and cost had float noise; #16120 rounds the computed cost;
  - #10992: cache tokens were not priced;
  - #16303 (post-HEAD): a string `gen_ai.prompt.version` was rejected;
  - #14127: GenAI v5 `parts` messages were not rendered.
- **Schema:** #11490 ("No field named llm_usage_tokens_input"); #11602/#11603 renamed `llm_*` columns to `gen_ai_*`, which broke queries; #14928 (duplicate field names on wide trace streams).
- **TODOs:** `flags: 1, // TODO` (`traces/mod.rs:866`). The flatten doc comment promises collision errors that the code never raises (`flatten.rs:28-33`).

---

## Recommendations for llmobs B1

**Adopt**
1. **Gate the LLM work with cheap checks**, like `is_llm_trace` (`gen_ai_operation.rs:187`), but only on convention-specific keys. Never on bare `input`, `model` or `prompt`.
2. **Separate extraction, computation and emission**, as in `processor.rs:141-170`. Extract into a typed struct first, decide which attributes are consumed, then build the output. In our case the output is a typed `NormalizedSpan`, not a mutated map.
3. **Track whether `input_tokens` includes cache tokens for each source** (`UsageDetails.input_includes_cache`, `usage.rs:30-36`). Make it a property of each converter, not a span-wide flag that the last match overwrites. This is our spec 02 "token semantics" rule, and fixtures should pin it for each convention.
4. **Collect the cache-token aliases they found:** OpenAI `prompt_tokens_details.cached_tokens`, DeepSeek `prompt_cache_hit/miss_tokens`, and Anthropic raw `cache_read_input_tokens`/`cache_creation_input_tokens` (`usage.rs:121-205`). Add them to our mapping table, with their semantics.
5. **Price tiers on cache-inclusive context, and bill uncached input** (`build_pricing_usage`, `processor.rs:672-705`). This matches our step 3 and tiered rows. Time-of-day windows (DeepSeek) are worth a line in the price-row format, after the MVP.
6. **Validate the conventions they support that we lack:** Logfire `prompt`/`all_messages_events`, MLflow `mlflow.spanInputs/Outputs`, LiveKit `lk.*`, Pydantic-AI `tool_arguments`/`tool_response`, and Semantic Kernel `gen_ai.content.prompt/completion` events. Add them to the convention backlog. Capture fixtures before writing mappings.

**Avoid**
1. **Don't stringify scalars or decode bytes as UTF-8** (`grpc.rs:22-55`). Keep the `AnyValue` type through normalization (our rule 1). Store bytes as base64. Never use `unwrap_or_default` on input.
2. **Don't trust `opentelemetry-proto`'s serde as our OTLP/JSON decoder.** It has no base64 IDs, no enum names, no ID length checks, and one bad value fails the whole batch. Either use a tolerant decoder that keeps going for each span, or at least fixture-test empty `AnyValue`, string int64, fractional doubles, enum names and base64 IDs.
3. **Use per-field precedence, not one convention for the whole I/O pair, and no last-write-wins for usage.** Their order puts `gen_ai.input.messages` behind Langfuse and Traceloop, and OpenInference usage overrides GenAI usage. Our spec's "first non-empty by precedence for each field" is right. Write it once as a table.
4. **Never use a raw prefix for indexed conventions** (#16271). Match `gen_ai.prompt.<n>.` and `llm.input_messages.<n>.` with a digit segment. Drop indices above the cap; don't clamp them. Note how a too-broad consumed-attribute list deletes data.
5. **Null, never 0:** never default tokens or cost to 0 on spans without usage (#16024). Don't estimate tokens with tiktoken and store the estimate as fact. If we ever estimate, use a separate `usage_source`. Compute tokens and cost only on `llm`/`embedding` spans, and make Vercel's outer span `chain` (#16272 is the double counting our spec 02 rule 3 prevents).
6. **Keep request and response models separate.** Don't relabel request values as response values. Lowercase and alias the provider.
7. **Unit-check every time field:** a timestamp is not a duration (the TTFT bug). Every converter output should have a fixture asserting its value.
8. **Don't let user-controlled keys become columns** (invocation params, metadata), and don't apply a column limit that rejects a whole request. Our `attributes`/`metadata` maps and `request_params` JSON avoid both.
9. **Wire behaviour:**
   - accept `end < start`, but record it;
   - reject all-zero IDs (rule 15) instead of accepting them;
   - give the real reason in `partial_success.error_message`;
   - always answer 200 for partial success, never 206;
   - never return 500 for the whole request because of one span;
   - match `Content-Type` on the media type, ignoring parameters;
   - make the old/future time window generous by default (OpenObserve's 5 h rejects backfills), or leave it off for an LLM tool that ingests evaluations after the fact.
10. **Make costs reproducible:** don't fetch prices from the network at runtime. Our embedded LiteLLM snapshot plus project/org rows gives deterministic costs at ingest. Anchor regexes and order matches deterministically (by priority, then longest pattern), not by hand-maintained list order.
11. **Cap payload sizes and don't store them twice:** remove consumed GenAI events from `events` the same way we remove consumed attributes. Apply our truncation, since OpenObserve has none.
