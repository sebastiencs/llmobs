# SDK research: Opik, LangWatch, MLflow tracing

Read-only study of three competitor tracing SDKs, done to inform our Python and TypeScript SDKs (spec 10). File references are relative to each repo root:
- Opik: `/home/sebastien/github/opik`
- LangWatch: `/home/sebastien/github/langwatch`
- MLflow: `/home/sebastien/github/mlflow`

No code was copied.

---

## 1. Opik (Comet): its own REST pipeline, not OTel

Opik's SDKs are not built on OTel. `@track` builds `SpanData`/`TraceData` objects in Opik's own context stack and sends them as REST messages to `/v1/private/spans|traces` through a home-grown queue, batcher and retry stack. OTel only appears as an *ingestion adapter*: `sdks/python/src/opik/integrations/otel/processor.py` (`OpikSpanProcessor`) stamps `opik.trace_id`/`opik.parent_span_id` onto spans from a separately configured OTel provider, so the server can stitch them into Opik traces.

Why it matters:
- the trace/span tree lives in two worlds;
- third-party OTel spans need a bridge processor and server-side heuristics ("SHA-256 + Redis path" in the processor docstring);
- every transport concern was rebuilt by hand: batching, compression, retry, 429 handling, an offline replay manager, payload truncation and a data-loss tracker.

That is about 5k lines in `message_processing/` alone.

### Python (`sdks/python/src/opik/`)

**1. Init and config.**
- `config.py` `OpikConfig` is pydantic-settings with the `OPIK_` env prefix, plus an INI file at `~/.opik.config` (`OPIK_CONFIG_PATH`) and explicit arguments.
- The client is a lazy global singleton. `api_objects/opik_client.py:get_global_client()` is double-checked under a lock, and a context-local override is possible (`set_global_client(..., context_wise=True)`, a ContextVar).
- **Unconfigured:**
  - the URL defaults to Comet cloud (`OPIK_URL_CLOUD`);
  - `Opik()` prints a misconfiguration warning but keeps running, and messages then fail and retry;
  - it is not a no-op.
- **Disable:**
  - `OPIK_TRACK_DISABLE` (`config.py` `track_disable`);
  - runtime `opik.set_tracing_active(False)` (`tracing_runtime_config.py`).
  - The check happens once per call at decorator entry (`decorator/base_track_decorator.py` wrapper).
- Defaults worth noting:
  - `sentry_enable=True` with a hard-coded DSN;
  - `analytics_enable=True`;
  - both in `config.py`, so the SDK phones home by default.

**2. Provider.** None, since it is not OTel.

**3. Transport.**
- **Queue.** `message_processing/message_queue.py` (max 1,000,000 messages, reduced by `maximal_queue_size_batch_factor=10` when batching) feeds 4 consumer threads (`background_workers`).
- **Batching.** `message_processing/batching/batch_manager_constuctors.py`:
  - spans and traces flush every 2 s, max 1000 items;
  - feedback scores flush every 1 s, max 1000, with a 5 MB memory cap (`MAX_BATCH_SIZE_MB=5`).
- **Compression.** JSON gzip at level 6 (`config.py` `request_compression_level`; the comment gives benchmark numbers for choosing 6 over 9).
- **Retry.** `rest_client_configurator/retry_decorator.py`: tenacity, 3 attempts, exponential backoff 5–15 s, and `Retry-After` honoured on 429.
- **Timeouts.** `httpx_client.py`: connect 20 s, read and write 100 s, pool 20 s.
- **Offline buffering.** `message_processing/replay/` replays failed messages once connectivity returns (batch 50, 0.5 s delay).
- **Start and end messages.** `log_start_trace_span=True` by default: every span is sent at start *and* again at end, which doubles traffic. The docstring itself recommends turning it off for short spans.

**4. Lifecycle.**
- **Exit.** `atexit.register(MANAGER.close_all)` (`api_objects/connection_resources.py:443`) drains each connection within `default_flush_timeout`, which is unbounded by default.
- **Flush.** `opik.flush_tracker(timeout)`, `Opik.flush()`, `Opik.end()`.
- **Fork safety.** Only the analytics worker handles fork (`analytics/api.py:219` `os.register_at_fork`). The tracing queue and consumer threads do not, so a forked child silently loses spans.
- **Create, then update.**
  - With batching on, calling `.update()`/`.end()` on a span shortly after creating it can lose data, because the create sits in the batch while the update goes out as a separate PATCH.
  - The SDK logs a warning instead of fixing it: `logging_messages.py:76` `BATCHING_UPDATE_DATA_LOSS_WARNING`, emitted from `api_objects/helpers.py:warn_if_batching_update`.

**5. Decorator.** `decorator/base_track_decorator.py`, `tracker.py`, `generator_wrappers.py`, `inspect_helpers.py`.
- **Usage and options.**
  - `@track` and `@track(...)` both work.
  - Options: `name`, `type` (`general|llm|tool|guardrail`), `tags`, `metadata`, `capture_input`, `ignore_arguments`, `capture_output`, `generations_aggregator`, `flush`, `project_name`, `create_duplicate_root_span`, `entrypoint`, `environment`.
  - Re-decorating is idempotent (`opik_tracked` marker).
- **Function kinds.**
  - Sync and async functions are detected by walking the `__wrapped__` chain (`inspect_helpers.is_async`).
  - Generators are handled best of the three SDKs (`generator_wrappers.py`):
    - the span is created lazily on the first `__next__`;
    - it is pushed onto the context **only while `__next__`/`__anext__` runs**, so it never leaks into the consumer;
    - it is ended exactly once, on exhaustion, error, `close()`/`aclose()`, `__del__` (a dropped generator) or an atexit hook for unfinished generators;
    - yielded items go to `generations_aggregator`, defaulting to `"".join(str(x))`.
  - Stream objects returned by LLM SDKs: the subclass `_streams_handler` returns a proxy, and the span ends when the stream is exhausted. The span is *not* in context, so it cannot parent anything.
- **Input.**
  - `inspect.signature(...).bind` plus `apply_defaults`.
  - `self` and `cls` are dropped.
  - On a bind `TypeError` it falls back to `{args, kwargs}`.
  - `ignore_arguments` also filters inside `**kwargs`.
- **Output.**
  - A non-dict return is wrapped as `{"output": value}`.
  - Output set explicitly through `opik_context.update_current_span(output=...)` wins over the return value.
- **Serialization.** `jsonable_encoder.py:encode`:
  - dataclasses, including slots;
  - pydantic (`__dict__` plus extras);
  - Enum → value;
  - Path → str;
  - datetime → ISO;
  - bytes → base64;
  - numpy → `tolist()`;
  - sets, tuples and generators → list;
  - cycles are detected by `id()` and rendered as `"<Cyclic reference ...>"`;
  - everything else falls back to `str()`;
  - **no depth limit**;
  - extension hook: `register_encoder_extension`.
- **Truncation.**
  - `message_processing/payload_truncation.py`: when `input`+`output` exceed `max_payload_size_mb=20`, the field is replaced with a marker dict (`opik_truncated`). Metadata is never truncated.
  - Base64 blobs ≥ 256 KB are extracted into attachments uploaded separately (`min_base64_embedded_attachment_size`).
- **Exceptions.** `error_info_collector.collect` gives `{exception_type, message, traceback}`. The exception is re-raised.
- **Naming.** The function `__name__`; for `functools.partial`, the inner function's name.
- **Privacy defect.** Error-path logs dump the arguments: `LOGGER.error(..., (args, kwargs), ...)` in `base_track_decorator.py` `_prepare_tracking_start_options`. Payloads end up in logs.

**6. Manual and LLM spans.**
- `opik_context.update_current_span(name, input, output, metadata, tags, usage, feedback_scores, model, provider, total_cost, attachments, error_info, prompts)` and `update_current_trace(...)` (`opik_context.py:59,126`).
- Usage expects OpenAI-style keys (`prompt_tokens`, `completion_tokens`, `total_tokens`); other formats are mapped where recognised (`llm_usage/`). `total_cost` overrides server-side cost.
- Low-level API: `Opik().trace()` / `trace.span()`.

**7. Context.**
- `context_storage.py`: ContextVar holding an immutable tuple stack of `SpanData`, plus a ContextVar for the current `TraceData`.
- **Async split.** The class docstring describes a real problem: LangChain runs end callbacks in `asyncio.create_task(copy_context())`, so a pop never reaches the push. Workaround: spans that have an `end_time` are discarded on read (`_discard_finished_spans`), plus `trim_span_data_stack_to_certain_span`.
- **Trace-level data.**
  - `thread_id`, tags and metadata are fields of the *trace entity*; they are not propagated to child spans.
  - They can be set per call without touching the body via `opik_args={"trace": {...}, "span": {...}}` (`decorator/opik_args/api_classes.py`).
- **Executors.** Thread pools get no automatic propagation (ContextVar semantics apply).
- **Distributed tracing.** Custom headers `opik_trace_id`/`opik_parent_span_id` (`opik_context.get_distributed_trace_headers`), not W3C.

**8. Masking.** `anonymizer/` plus `hooks/anonymizer_hook.py:add_anonymizer`:
- regex rules, `(regex, repl)` tuples or callables;
- applied recursively up to `max_depth=10`.

**8. Sampling.** None client-side.

**9. Feedback scores.**
- REST, batched: `Opik.log_traces_feedback_scores` / `log_spans_feedback_scores` / `log_threads_feedback_scores` (`api_objects/opik_client.py:812–970`).
- `FeedbackScoreDict` fields (`types.py:59`):
  - `id` = target trace/span/thread id;
  - `name`;
  - `value`;
  - `reason?`;
  - `category_name?`;
  - `project_name?`.
- Inline form: `update_current_span(feedback_scores=[...])`.
- Ids are client-generated UUIDv7 (`id_helpers.py`).

**10. Integrations.**
- Bundled in the main package under `integrations/` (openai, anthropic, bedrock, langchain, llama_index, dspy, crewai, adk, haystack, …).
- **Explicit opt-in**: `track_openai(client)` wraps the client instance. There is no global auto-patching.
- All are Opik-native wrappers; none use OpenInference or OpenLLMetry.

**11. Tests.**
- The `fake_backend` fixture (`tests/conftest.py:143`) swaps the streamer for `testlib/backend_emulator_message_processor.py`, which rebuilds trace and span trees.
- Tests compare those trees to expected trees using `ANY_BUT_NONE`-style matchers (`testlib/any_compare_helpers.py`).
- `e2e/` runs against a real backend.

**12. Visible scars.**
- the create/update batching data-loss warning;
- the `LangChain copy_context` stack split;
- OPIK-7127 (atexit strong-ref pin, `opik_client.py:202`);
- the deprecated `_use_batching` kept next to `batching`;
- spans sent twice by default.

### TypeScript (`sdks/typescript/src/opik/`)

**Init.**
- `config/Config.ts`: env (`OPIK_API_KEY`, `OPIK_URL_OVERRIDE`, `OPIK_PROJECT_NAME`, `OPIK_WORKSPACE`, `OPIK_BATCH_DELAY_MS`, `OPIK_HOLD_UNTIL_FLUSH`, `OPIK_TRACK_DISABLE`, …), then the INI file, then explicit arguments.
- `validateConfig` **throws** when the URL is missing, or when the API key or workspace is missing on cloud. It skips those checks when `trackDisable` is set.
- `track` lazily creates its own `OpikClient` (`decorators/track.ts:getTrackOpikClient`).

**Transport.**
- `client/BatchQueue.ts`: a debounced queue keyed by entity id. Delay is 300 ms, batch size 100 (`@todo: change to check payload size`).
- **Update merging.** An update whose create is still queued is merged into it (`queue.set(id, {...entity, ...updates})`), which fixes the Python create/update race. Once the create has flushed, every update is a **separate PATCH per span** (`client/SpanBatchQueue.ts:updateEntity`), and deletes are one per id.
- HTTP goes through the Fern-generated fetch client.

**Lifecycle.**
- No exit hook for tracing: the user must `await client.flush()`.
- `holdUntilFlush` targets serverless.
- `node:async_hooks` is imported unconditionally, so the SDK is Node-only.

**Decorator** (`decorators/track.ts`).
- `track(fn)`, `track(opts, fn)` and the method decorator (both the TC39 and legacy forms).
- Input is `{arguments: args}`: positional, **no parameter names**.
- Output is the object itself, or `{result}` for a primitive.
- `enrichSpan(result)` hook.
- **Promise only**: no generator or async-iterator handling, so a returned stream is logged as an opaque object.
- The default span type in `logSpan` is `"llm"`, a surprising default.

**Context.** `AsyncLocalStorage<{span, trace}>` (`trackStorage`). Distributed headers are custom (`context.ts`).

**Integrations.** Separate packages: `opik-openai`, `opik-langchain`, `opik-vercel`, `opik-gemini`, `opik-otel`.

---

## 2. LangWatch: OTel-native, single-span-tree, JSON blobs in attributes

### Python (`sdks/python/src/langwatch/`)

**1. Init and config.**
- `langwatch.setup(api_key, endpoint_url, base_attributes, tracer_provider, instrumentors, span_exclude_rules, debug, skip_open_telemetry_setup)` (`utils/initialization.py`).
- `Client` is a class-level singleton (`client.py`, `__new__` plus ClassVars). Calling `setup()` again with a new key or endpoint tears the provider down and rebuilds it.
- Env vars:
  - `LANGWATCH_API_KEY`;
  - `LANGWATCH_ENDPOINT` (default `https://app.langwatch.ai`);
  - `LANGWATCH_DEBUG`;
  - `LANGWATCH_SKIP_OTEL_SETUP`;
  - `LANGWATCH_PROJECT_ID`.
- **Unconfigured.**
  - `ensure_setup()` (called from every trace and span constructor) auto-runs `setup(debug=True)` only when `LANGWATCH_API_KEY` exists.
  - Otherwise nothing is set up and spans go to the global `ProxyTracerProvider`, i.e. a no-op.
  - Explicit `setup()` without a key raises `ValueError` when it attaches the exporter.
- **Disable.**
  - `disable_sending` makes the provider sampler `ALWAYS_OFF`.
  - Per trace, `disable_sending=True` is refcounted on the client (issue #3981: one trace's flag "poisoned" later traces in reused workers).

**2. Provider.** `client.py:__ensure_otel_setup`:
- The global is `ProxyTracerProvider` → create a `TracerProvider` and **set it globally**.
- The global is an SDK `TracerProvider` → **attach the LangWatch processor to it**, with a warning.
- A `tracer_provider=` argument → dedicated provider; the exporter is attached there and the global is untouched.
- **Span filtering:**
  - `FilterableBatchSpanProcessor.on_end` (`exporters/filterable_batch_span_exporter.py`) drops spans by name only (`exact_match|includes|starts_with|ends_with`);
  - the default is *no filter*, so all global spans (HTTP, DB, …) are exported.

**3. Transport.**
- Stock `OTLPSpanExporter` (HTTP/protobuf) to `{endpoint}/api/otel/v1/traces`.
- gzip unless `OTEL_EXPORTER_OTLP_*COMPRESSION` is set (`client.py:_default_otlp_compression`).
- Timeout is `OTEL_EXPORTER_OTLP_TRACES_TIMEOUT` (30 s).
- Batch processor settings come from `OTEL_BSP_*`, defaulting to:
  - batch 100;
  - queue 512;
  - delay 1000 ms;
  - export timeout 10 s.
- Retries are the OTel exporter's built-in backoff.
- Wire format is pinned by `tests/test_trace_wire_format.py` (a local HTTP receiver decodes gzip protobuf).

**4. Lifecycle.**
- `atexit.register(provider.force_flush)`, but only when LangWatch created the provider (`client.py:__create_new_tracer_provider`).
- Fork handling is inherited from OTel's `BatchSpanProcessor` (it re-inits its worker after fork).

**5. Decorators.**
- `@langwatch.trace(...)` (`telemetry/tracing.py`) creates a `LangWatchTrace` plus a root span.
- `@langwatch.span(type=..., name=..., capture_input, capture_output, ...)` (`telemetry/span.py`).
- Both work as decorators and as sync or async context managers; the decorator clones the configured span per call.
- **Generators** (`span.py:586–627`):
  - the `with span:` block wraps the *whole* generator body, so the span stays attached to the OTel context across `yield`s;
  - it leaks into consumer code between items;
  - for async generators it detaches in a different context ("Failed to detach context");
  - output is `"".join(items)` if all items are `str`, else the list.
- **Input** (`span.py:_set_callee_input_information`):
  - positional arguments are zipped with the signature, then kwargs are merged;
  - **no `bind`, so defaults are missing and `*args` is mis-mapped**;
  - `self` is dropped and the span is renamed `Class.method`.
- **Serialization.**
  - `utils/transformation.py` wraps values in typed envelopes: `{"type": "text"|"chat_messages"|"json"|"raw"|"list"|"guardrail_result"|"evaluation_result", "value": ...}` (`convert_typed_values`).
  - The JSON encoder `SerializableWithStringFallback`: pydantic `model_dump(exclude_unset)`, set → list, NaN → null, otherwise `str()`.
  - No cycle detection.
  - Every input and output is `deepcopy`'d before encoding (CPU cost, and side effects on odd objects).
- **Truncation.** Off by default (`tracing.py:101`: "No client-side truncation by default … backend owns sizing", #4215). Opt-in `max_string_length` (≥ 100 bytes, binary-search UTF-8 cut).
- **Exceptions.** `record_exception` plus status.

**6. Attributes.** `attributes.py`:
- `langwatch.span.type`;
- `langwatch.input` / `langwatch.output` (JSON strings of typed envelopes);
- `langwatch.params`;
- `langwatch.metrics` (JSON: `prompt_tokens`, `completion_tokens`, `cost`, …), also mirrored into `gen_ai.usage.prompt_tokens|completion_tokens` (the *old* semconv names, `span.py` `update`);
- `langwatch.rag_contexts`;
- `langwatch.timestamps`;
- `gen_ai.request.model`;
- `langwatch.thread.id`, `langwatch.user.id`, `langwatch.customer.id`;
- `langwatch.prompt.*`.
- Span types (`domain/__init__.py:136`): span, llm, chain, tool, agent, guardrail, evaluation, rag, workflow, component, module, server, client, producer, consumer, task, unknown.
- Bug: `UserHash = "user.email"` (copy-paste).

**7. Context.** `telemetry/context.py`:
- ContextVars for the current trace and span.
- **Thread fallback.** On a child thread with no context, it falls back to a **process-global "main thread" list** of the last trace and span, so concurrent requests can bleed into each other's threads. It is guarded to skip the asyncio case.
- **Trace metadata** (`user_id`, `thread_id`, `labels`, …) is serialized as one JSON `metadata` attribute on the **root span only**; it is not propagated to children.
- **Nesting under OTel.** When `@trace` starts inside an existing OTel span, the root also gets a `Link(relationship=parent_trace)` (`tracing.py:_create_root_span`).

**8. Masking.** No SDK-level hook in Python (the TS side has `dataCapture` modes).

**8. Sampling.** `sampling.py` only *reads* the global sampler's rate.

**9. Evaluations.** `evaluation/__init__.py:_add_evaluation`:
- an evaluation is an OTel **child span of type `evaluation`**, whose output holds the result, plus a span event `langwatch.evaluation.custom` with `json_encoded_event`;
- id `generate_ksuid("eval")` is client-generated;
- fields: `passed`, `score`, `label`, `details`, `cost`, `status` (`processed|skipped|error`), `is_guardrail`.
- Server-run evaluators go over REST (`evaluate(slug, ...)`).
- **Result: scores travel inside the trace stream; there is no separate score endpoint.**

**10. Integrations.**
- `instrumentors=[...]` accepts any OTel `BaseInstrumentor`.
- Hard dependencies include `openinference-instrumentation-{openai,langchain,haystack}` and `opentelemetry-instrumentation-crewai` (`pyproject.toml`), a heavy base install.
- Own wrappers: `trace.autotrack_openai_calls(client)`, `autotrack_litellm_calls`, `autotrack_dspy`, and a LangChain callback.

**11. Tests.**
- Unit tests with in-process exporters.
- A wire-format test with a stub HTTP server.
- `test_disable_sending_singleton_poisoning.py` is a regression test for the singleton bug.

**12. Changelog** (`sdks/python/CHANGELOG.md:676`): "get rid of context for tracking current span, rely exclusively on the opentelemetry one instead, fixing out-of-parent spans". Their parallel context store caused mis-parenting, and the main-thread fallback list is still there.

### TypeScript (`sdks/typescript/src/observability-sdk/`)

**Setup.** `setupObservability(options)` (`setup/node/setup.ts`, types in `setup/node/types.ts`), built on `NodeSDK`.
- Options: `langwatch: {apiKey, endpoint, processorType: batch|simple} | "disabled"`, `serviceName`, `attributes`, `dataCapture`, `tracerProvider` (dedicated), `traceExporter`, `spanProcessors`, `sampler`, `instrumentations`, `contextManager`, `textMapPropagator`, plus `advanced.{throwOnSetupError, skipOpenTelemetrySetup, attachToExistingProvider, disabled, disableAutoShutdown}`.
- **Errors** become no-op handles unless `throwOnSetupError` is set.
- **Existing global provider.** The default is to **no-op with a warning** and *not* attach. `attachToExistingProvider` is opt-in. This is the **opposite of the Python default**.
- `UNSAFE_forceOpenTelemetryReinitialization`.
- Next.js 15 workaround: if the global is still `ProxyTracerProvider` after `sdk.start()`, register it manually (`setup.ts:516`; CHANGELOG #754).

**Exporter.** `exporters/langwatch-trace-exporter.ts`:
- `OTLPTraceExporter` (proto).
- `x-langwatch-sdk-*` headers.
- gzip.
- **Filters** applied in `export()`:
  - `TraceFilter = {preset: "vercelAIOnly"|"excludeHttpRequests"} | {include|exclude: {instrumentationScopeName, name: Match[]}}`, where `Match` is `equals|startsWith|matches(RegExp)` with `ignoreCase`, applied with AND semantics;
  - the default is `excludeHttpRequests`;
  - this drops parents after the fact, so children can reference a missing parent span.
- **User exporters.** A user-provided `traceExporter` is wrapped in `SimpleSpanProcessor` (one export per span, `setup.ts:396,478,596`).

**Lifecycle.**
- Auto-shutdown on `beforeExit`/`SIGINT`/`SIGTERM`. It re-raises the signal only if it was the sole listener, and `process.exit()` bypasses it (`setup.ts:190–240`).
- `ObservabilityHandle.shutdown()`.
- Core APIs (`./observability`) work with any provider; only `./observability/node` needs Node. Edge users bring their own provider, e.g. Vercel's.

**Spans.**
- No decorator, and no automatic argument capture.
- `getLangWatchTracer(name)` returns a Proxy over the OTel tracer that adds `withActiveSpan(name, opts?, ctx?, fn)`. It ends the span, sets status and records exceptions for sync and async callbacks (`tracer/implementation.ts`).
- `LangWatchSpan` setters (`span/types.ts`): `setType`, `setInput(type?, value)`, `setOutput`, `setRequestModel`, `setResponseModel`, `setRAGContexts`, `setMetrics`, `setSelectedPrompt`, `addEvaluation`.
- `@opentelemetry/api` is a peer dependency.

**Data capture.** `features/data-capture/types.ts`: mode `all|input|output|none`. A predicate type is declared but not accepted by `DataCaptureOptions`; the types and docs drift.

---

## 3. MLflow tracing: OTel underneath, MLflow trace entity on top

### Python (`mlflow/tracing/`)

**1. Init and config.**
- No init call: tracing is lazily enabled on first span.
- The destination comes from the tracking URI and experiment (`mlflow.set_tracking_uri`, `set_experiment`), `mlflow.tracing.set_destination(...)` (optionally context-local), or `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`.
- **Unconfigured:** writes to the default local tracking store, not a no-op.
- **Disable:** `mlflow.tracing.disable()`/`enable()` (`provider.py:1010,1051`), the `@trace_disabled` decorator, or `mlflow.tracing.context(enabled=False)`.
- Env vars live in `mlflow/environment_variables.py:873–1436` (`MLFLOW_TRACE_*`, `MLFLOW_ASYNC_TRACE_LOGGING_*`, `MLFLOW_TRACE_SAMPLING_RATIO`, …).

**2. Provider.** `provider.py`:
- **The default is an isolated provider.** Confusingly, `MLFLOW_USE_DEFAULT_TRACER_PROVIDER=True` *means isolated* (`environment_variables.py:1090`).
- In that mode MLflow even keeps its **own runtime context** (`mlflow_runtime_context`), separate from OTel's global context (`set_span_in_context`, `provider.py:404`).
- Consequence: pure-OTel libraries do not nest under `@mlflow.trace` spans unless `MLFLOW_TRACE_PROPAGATE_TO_OTEL_CONTEXT=true` (opt-in, because it "makes MLflow's active span visible to all OpenTelemetry instrumentation … FastAPI, requests").
- `get_bridged_tracer_provider()` hands MLflow's provider to integrations.
- **Unified mode** (`=false`): MLflow adds its processors to an existing global SDK provider (skipping types already present) or installs its own (`provider.py:_initialize_tracer_provider`).
- **Exporter selection** (`_get_span_processors`):
  - the explicit destination wins;
  - otherwise OTLP if its env var is set (replacing MLflow unless `MLFLOW_TRACE_ENABLE_OTLP_DUAL_EXPORT`);
  - otherwise the MLflow tracking server, or an inference table in Databricks serving.
- **Filtering:**
  - none by name;
  - isolation is the filter;
  - `span_processors` callables can mutate spans (see §8).
- Logs that OTel emits for context misuse are permanently silenced ("Failed to detach context", "Setting attribute on ended span", "Calling end() on an ended span", `provider.py:718–727`). This is a telling admission.
- `_IsolatedRandomIdGenerator` exists because user `random.seed()` produced duplicate span ids. It is re-seeded at fork (`provider.py:78–120`).

**3. Transport.** `export/mlflow_v3.py`:
- Spans are sent **incrementally** as OTLP protobuf (`log_spans`, which POSTs `ExportTraceServiceRequest` to the server's OTLP path; `store/tracking/rest_store.py:~2580`, server ≥ 3.4).
- In addition, **when the root ends** the trace info goes out (`start_trace`), along with full trace data as a JSON artifact for old stores, plus attachments.
- **Async queue** (`export/async_export_queue.py`):
  - bounded queue of 1000 (`MLFLOW_ASYNC_TRACE_LOGGING_MAX_QUEUE_SIZE`);
  - 10 workers;
  - **drops on full**, with a warning at most every 30 s;
  - dispatch is throttled so the ThreadPoolExecutor's unbounded internal queue cannot grow (comment at `_dispatch_task`);
  - retry timeout 500 s.
- Span batching (`MLFLOW_ASYNC_TRACE_LOGGING_MAX_SPAN_BATCH_SIZE=5`, 5 s) applies only to the UC-table exporter.

**4. Lifecycle.**
- `atexit` flush of the async queue (`async_export_queue.py:44`).
- `mlflow.flush_trace_async_logging()`.
- **In-memory trace buffer** (`trace_manager.py`):
  - TTL 3600 s;
  - max 1000 traces;
  - `MLFLOW_TRACE_TIMEOUT_SECONDS` force-ends traces that never close.

**5. Decorator.** `fluent.py:97–545`.
- `@mlflow.trace(name, span_type, attributes, output_reducer, trace_destination, sampling_ratio_override, log_level, links, description)`.
- Sync and async functions share one coroutine-based `_WrappingContext`.
- **Input.** `utils/__init__.py:capture_function_input_args`: `bind` plus `apply_defaults`; `self` dropped; `cls` dropped only when it is a type; on failure, a warning and `None`.
- **Output.** The return value, unless the body called `span.set_outputs`.
- **Generators** (`_wrap_generator`):
  - `start_span_no_context`;
  - the span is made active **only around each `next()`**, as the docstring explains with an A–F diagram;
  - every chunk is recorded as a span **event** `mlflow.chunk.item.{i}` (expensive for long streams);
  - `output_reducer` folds the chunks.
  - **Bug:** there is no `try/finally` around `yield`, so a consumer that stops early (`break`, `GeneratorExit`) leaves the span open until the trace timeout.
- **Serialization.** `TraceJSONEncoder` (`utils/__init__.py:69`):
  - pydantic `model_dump`;
  - dataclass via shallow fields (deliberately *not* `asdict()`, whose deepcopy crashed objects in GC);
  - known side-effecting types (LlamaIndex streaming responses, whose `__str__` consumes the stream) become their class name;
  - otherwise `str()`.
  - No cycle handling.
- **Truncation.** Span payloads are not truncated. Only the trace `request_preview`/`response_preview` are cut (1000 chars OSS / 10000 Databricks, `utils/truncation.py`). Preview extraction picks the last user or assistant message from OpenAI-shaped payloads and replaces NUL bytes, because Postgres rejects them.

**6. Attributes.** `constant.py:108–152`:
- `mlflow.spanInputs` / `mlflow.spanOutputs` (JSON);
- `mlflow.spanType` (`LLM|CHAT_MODEL|RETRIEVER|TOOL|AGENT|CHAIN|EMBEDDING|PARSER|RERANKER|UNKNOWN`);
- `mlflow.chat.tokenUsage` (`{input_tokens, output_tokens, total_tokens}`);
- `mlflow.llm.cost`;
- `mlflow.llm.model`, `mlflow.llm.provider`;
- `mlflow.chat.tools`;
- `mlflow.message.format`;
- `mlflow.spanFunctionName`;
- `mlflow.traceRequestId`.
- Optional translation to GenAI semconv on OTLP export (`MLFLOW_ENABLE_OTEL_GENAI_SEMCONV`).

**7. Context.**
- **Trace-level values.** Session and user are *trace metadata* (`mlflow.trace.session`, `mlflow.trace.user`) on the TraceInfo entity, set via `mlflow.update_current_trace(session_id=, user=, tags=, metadata=)` (`fluent.py:1507`) or the scope helper `mlflow.tracing.context(metadata, tags, session_id, user, enabled)` (a ContextVar in `context.py`). These are not span attributes.
- **Async.** A cross-asyncio-context detach `ValueError` is swallowed (`provider.py:447`).
- **Threads.** Thread pools need an explicit `copy_context`.
- **Distributed.** `distributed/` handles W3C propagation.

**8. Masking.** `mlflow.tracing.configure(span_processors=[fn(LiveSpan)])` (`config.py`) runs before export and is usable as a context manager.

**8. Sampling.** `ParentBased(_MlflowSampler(ratio))` from `MLFLOW_TRACE_SAMPLING_RATIO`, plus a per-function `sampling_ratio_override` held in a ContextVar.

**9. Assessments** (feedback and expectation; `assessment.py`, `client.py:703`).
- `log_feedback(trace_id, name, value, source, error, rationale, metadata, span_id)`.
- **Transport.**
  - If the trace is still active, the assessment is appended to the in-memory TraceInfo and shipped with the trace.
  - Otherwise it is a synchronous REST `create_assessment`.
  - The assessment id is server-assigned.
- Source types are HUMAN, LLM_JUDGE and CODE. An error can replace the value.

**10. Integrations.**
- `mlflow.<flavor>.autolog()` per library (openai, anthropic, langchain, …): explicit, own patching.
- A separate lightweight `mlflow-tracing` package exists (`tests/tracing/helper.py:skip_when_testing_trace_sdk`).

**11. Tests.**
- A real local store, with `get_traces()`/`purge_traces()` helpers (`tests/tracing/helper.py`).
- `capture_otel_export()`.
- `create_mock_otel_span`.

### TypeScript (`libs/typescript/core/src/`)

**Init and provider.**
- `init({trackingUri, experimentId})` leads to `core/provider.ts:initializeSDK`, which starts a `NodeSDK({spanProcessors:[MlflowSpanProcessor]})`.
- It **registers the global provider**, unlike Python's isolated default. `getTracer` uses the global `trace.getTracer`.
- Re-init shuts down the previous SDK.

**Processor and export** (`exporters/mlflow.ts`).
- `onStart` registers TraceInfo for root spans. The root test is "no parent span id", so a span with a remote parent is treated as non-root and **dropped** with "No trace ID found".
- Spans are buffered in `InMemoryTraceManager`.
- `onEnd` of the root aggregates token usage and exports the whole trace through REST.
- A WAL mode (`exporters/wal/`) uses a daemon over IPC that fsyncs `queue.log` and uploads from another process. It exists for short-lived hooks (Claude Code, Codex), and `flushTraces()` waits only for WAL durability.

**API** (`core/api.ts`).
- `trace(fn, opts)`, the method decorator, `withSpan`, `startSpan`.
- Argument names are recovered by **parsing `func.toString()`** (`core/utils/index.ts:mapArgsToObject`), which breaks under minification and falls back to `{args}`.
- Promise only: no generator or async-iterable support.

**Context.**
- `AsyncLocalStorage` scope `tracingContext({metadata, tags, sessionId, user, enabled}, fn)` (`core/context.ts`). Its values are copied into TraceInfo at root start.
- Node-only (`node:async_hooks`, `node:fs`).

**Integrations.** Separate packages under `libs/typescript/integrations/` (openai, anthropic, gemini, vercel, claude-code, codex, …).

**Tests.** Jest against a real MLflow server started in global setup (`jest.global-server-setup.ts`).

---

## Comparison

| Concern | Opik | LangWatch | MLflow |
|---|---|---|---|
| Provider ownership | No OTel; own REST client singleton | Py: reuse the global SDK provider (attach) or create and set the global; dedicated via an argument. TS: no-op if a global exists unless `attachToExistingProvider`; dedicated via an option | Py: isolated provider *and* isolated context by default; unified mode opt-in. TS: NodeSDK sets the global |
| Span filtering | N/A (only `@track`/integration data is sent) | Py: none by default, name rules in `on_end`. TS: exporter-level filters, default `excludeHttpRequests` (can orphan children) | Isolation is the filter; `span_processors` mutators; no name filter |
| Generator handling | Best: lazy span, in context only during `__next__`, ended on exhaust/close/GC/atexit; aggregator. TS: none | `with` spans the whole generator, so context leaks across yields; join strings. TS: none | Active only during `next()`; every chunk a span event; `output_reducer`; **no finally, so early break leaves the span open**. TS: none |
| Serialization limits | Own encoder: cycles caught, no depth limit, bytes as base64, numpy; marker past 20 MB per span; 256 KB base64 to attachments | `deepcopy` then JSON with `str()` fallback; NaN to null; no cycle guard; no truncation by default (opt-in per string) | JSON encoder with `str()` fallback, special-cases side-effecting `__str__`; no truncation (previews only, 1k/10k chars) |
| Score transport | Separate REST batch endpoints (1 s / 1000 / 5 MB), client UUIDv7 ids | Inside the trace: `evaluation` child span plus a span event, client KSUID | In-memory on the active trace (shipped with it), else synchronous REST; server ids |
| Transport | JSON+gzip REST, 4 threads, 2 s/1000 batches, tenacity 3×, offline replay | Stock OTLP/HTTP proto + gzip, BSP 100/512/1 s | OTLP proto per span + trace-info REST at root end; bounded queue drops when full |
| Exit/flush | atexit (Py); manual `flush()` (TS) | atexit force_flush (Py, own provider only); signal hooks (TS) | atexit queue flush; TS WAL daemon |

---

## Implications for us

1. **Be OTel-native end to end, like LangWatch and MLflow, not like Opik.**
   - Opik needed about 5k lines of queue, retry, replay and truncation code, plus a bridge processor, to reach what `BatchSpanProcessor` and the OTLP exporter give for free.
   - Our server already ingests OTLP, so the SDK should be a thin layer over `opentelemetry-sdk`.
2. **Default to a dedicated `TracerProvider` that we own, but share the global OTel *context*.**
   - Spans from OpenInference and OpenLLMetry instrumentors and from user OTel code should nest under our decorator spans.
   - MLflow's separate runtime context made this opt-in (`MLFLOW_TRACE_PROPAGATE_TO_OTEL_CONTEXT`) and is a usability trap.
   - Offer an explicit "attach to the existing global provider" mode.
   - Never silently attach the way LangWatch Python does, and never silently no-op the way LangWatch TS does. Pick one documented behaviour and keep it identical in Python and TS.
3. **Filter exported spans by instrumentation scope at the processor (`on_end`), not the exporter.** The default export set is our tracer plus known GenAI instrumentation scopes plus anything under a span we created. Document what happens to dropped parents, and re-parent rather than orphan. LangWatch's `excludeHttpRequests` exporter filter can leave children pointing at missing parents.
4. **Unconfigured means a no-op with a single warning.** Never default to a cloud URL (Opik), a local store (MLflow), or a throw at import or config time (Opik TS `validateConfig`). Provide `LLMOBS_ENABLED=false` / a runtime toggle that is checked per call, as Opik's `is_tracing_active` is.
5. **Generators and streams: copy Opik's semantics.**
   - The span starts lazily.
   - It is current *only* while the generator body runs: attach and detach the OTel context around each `next`/`anext`.
   - It ends exactly once on exhaustion, error, `close`/`aclose`, GC or exit.
   - Wrap the yield in `try/finally`; MLflow's lack of one is a real leak.
   - Do not record per-chunk events by default (MLflow). Use a configurable reducer, defaulting to string join, else a list.
6. **The TS decorator must support async iterables and returned streams.** All three TS SDKs handle only Promises, so streaming LLM code is mis-traced. This is a gap we can win.
7. **Python input capture uses `signature.bind` plus `apply_defaults`, drops a bound `self`/`cls`, and takes `ignore_arguments` (also applied inside `**kwargs`).** LangWatch's zip-based mapping loses defaults and `*args`.
8. **TS input capture should not parse `fn.toString()`** (MLflow), which breaks under minification. Use positional `{args: [...]}` by default and accept an optional `argNames` / `mapInput(args)` option. Opik's `{arguments}` is honest but nameless.
9. **One serializer per SDK with explicit, tested rules:**
   - pydantic/dataclass (shallow fields, no deepcopy);
   - bytes → size marker or base64 under a cap;
   - numpy/pandas → list or summary;
   - Enum, datetime, Path;
   - cycle detection;
   - **a depth limit and a size limit**, both absent in all three;
   - `str()` fallback guarded by try/except;
   - NaN/Inf → null (LangWatch);
   - never `deepcopy` user objects (LangWatch);
   - never call side-effecting `__str__` on known stream types (MLflow).
10. **Truncate client-side with a visible marker,** e.g. `{"llmobs.truncated": true, "original_bytes": N}`, at a configurable per-attribute limit, so a single span cannot exceed our OTLP request limit. LangWatch's "server owns sizing" pushes the failure to ingestion. Pick limits that match spec 02 truncation.
11. **Put trace-level context (session, user, tags, metadata) in a ContextVar / AsyncLocalStorage scope and stamp it on *every* span at start via a SpanProcessor `on_start`, as attributes.**
    - Opik, LangWatch and MLflow all keep it on the root or trace entity only. That breaks when the root is not ours (OTel parent from HTTP), when spans are exported before the root ends, or when spans are filtered.
    - For cross-process propagation, also offer W3C baggage, not Opik-style custom headers.
12. **No thread-global fallbacks** (LangWatch's `main_thread_langwatch_trace` list). Provide helpers to propagate context into executors: `llmobs.wrap(fn)` / `copy_context().run`, and document thread pools.
13. **Do not silence OTel context warnings** (MLflow). Design attach/detach so they never fire, and test async-generator and cross-task cases explicitly. Opik's `copy_context` LangChain case is the canonical test.
14. **Scores travel over a separate REST endpoint (spec 09), batched and asynchronous, with client-generated UUIDv7 ids so they are idempotent on retry** (Opik).
    - Allow scoring a span or trace that has not been exported yet, because the target id is known client-side.
    - Avoid MLflow's synchronous network call from user code, and LangWatch's eval-as-span, which mixes evaluation data into trace storage and cannot score after the fact without a span.
15. **Span model: one immutable span per OTLP export, sent at end only.**
    - No start+end double send (Opik `log_start_trace_span`), and no create-then-PATCH (Opik's data-loss warning and per-span PATCH in TS).
    - Long-running spans can be a later concern.
16. **Lifecycle.**
    - Python: `atexit` `force_flush` with a bounded timeout (Opik's default is unbounded), and `os.register_at_fork` to rebuild our processor in children (Opik's tracing queue lacks it).
    - TS: explicit `await llmobs.flush()` / `shutdown()`, plus optional `beforeExit` hooks that never call `process.exit` and never swallow signals (LangWatch's careful re-raise logic is the model).
    - Keep the core free of `node:*` imports so edge and serverless can bring their own provider; put the Node setup in a subpath export (LangWatch `./observability/node`).
17. **Masking hook.** A single `mask(span_attributes) -> attributes` callback (MLflow `span_processors`, Opik anonymizers), applied in our processor before export, plus capture modes `all|input|output|none` (LangWatch TS).
18. **Privacy in SDK logs.** Never log arguments or outputs on error paths (Opik logs `(args, kwargs)`). Log function name and span id only, in line with the AGENTS.md privacy rule.
19. **Instrumentations are opt-in extras, not hard dependencies.** LangWatch Python pulls in OpenInference instrumentations unconditionally. Accept any `BaseInstrumentor` / OTel `Instrumentation` in `init`, rather than wrapping clients ourselves (Opik `track_openai`, MLflow autolog), since server-side normalization (spec 02) already understands OpenInference and OpenLLMetry.
20. **No telemetry or Sentry from the SDK by default** (Opik ships both on). **Test with an in-memory span exporter** plus golden OTLP fixtures from `fixtures/otlp/`. A stub HTTP receiver that asserts gzip + protobuf on the wire is a cheap, valuable test (LangWatch `test_trace_wire_format.py`).
