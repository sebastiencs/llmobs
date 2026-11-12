# SDK research: Langfuse Python v5.1 and Laminar Python (lmnr 0.8.8)

Sources: the installed packages under `tools/capture/py/.venv/.../site-packages/{langfuse,lmnr}`. Paths below are relative to each package root. The Langfuse server is at `/home/sebastien/github/langfuse` and Laminar's at `/home/sebastien/github/lmnr`. TypeScript SDK sources are not available locally. TS notes come from langfuse.com docs and are marked **(docs)**. Notes marked **(unverified)** come from memory.

---

## 1. Init and client

**Langfuse**
- `Langfuse(...)` (`_client/client.py:300`). Each argument falls back to an env var: `public_key`/`LANGFUSE_PUBLIC_KEY`, `secret_key`/`LANGFUSE_SECRET_KEY`, and `base_url` → `LANGFUSE_BASE_URL` → `host` → `LANGFUSE_HOST` → `https://cloud.langfuse.com`. Also `timeout`/`LANGFUSE_TIMEOUT` (5 s), `sample_rate`/`LANGFUSE_SAMPLE_RATE`, `environment`/`LANGFUSE_TRACING_ENVIRONMENT`, `release`/`LANGFUSE_RELEASE`, and `tracing_enabled` AND `LANGFUSE_TRACING_ENABLED`. All env vars are documented in `_client/environment_variables.py`.
- Release auto-detection: `get_common_release_envs()` in `_utils/environment.py` reads platform vars such as the git SHA on Vercel or Heroku.
- **Missing key:** logs a warning and sets the tracer to `NoOpTracer`, so the client is disabled and never raises (`client.py:369-385`). `OTEL_SDK_DISABLED=true` also gets a warning.
- **Singleton per public key:** `LangfuseResourceManager._instances: Dict[public_key, …]`, guarded by a class `RLock` (`_client/resource_manager.py:82-179`). A second `Langfuse()` with the same key reuses the first one's resources, and its new config is ignored.
- `get_client(public_key=None)` (`_client/get_client.py`):
  - zero instances: creates `Langfuse()` from env;
  - one instance: returns a client for it;
  - several instances and no key: returns a **disabled** client with a warning, to avoid "cross-project leakage".
- `@observe` resolves its client through `get_client`, using the `langfuse_public_key` kwarg or a `_current_public_key` contextvar that nested calls inherit.
- **Multi-project isolation:** each Langfuse tracer carries the scope attribute `public_key`. A processor drops `langfuse-sdk` spans whose `public_key` differs from its own (`_client/span_processor.py:291-306`).

**Laminar**
- `Laminar.initialize(...)` is a classmethod on a process-wide static class (`sdk/laminar.py:225`). A second call logs "already initialized" and is ignored.
- Config: `project_api_key` → `LMNR_PROJECT_API_KEY`; `base_url` → `LMNR_BASE_URL` → `https://api.lmnr.ai`.
  - Ports are separate: `http_port` 443 and `grpc_port` 8443.
  - `from_env` also reads a `.env` file through python-dotenv (`sdk/utils.py:241`).
  - Global `metadata` merges over the JSON in `LMNR_TRACE_METADATA`.
  - `release` → `LMNR_RELEASE` → platform vars → git commit (`opentelemetry_lib/tracing/release.py`).
- **Missing key:** **raises `ValueError`**, unless `OTEL_EXPORTER_OTLP_ENDPOINT`/`HEADERS` are set, or `LMNR_TRAJECTORY_PATH` (local file mode) is set (`laminar.py:348-358`).
- Before `initialize`, `observe` is a pass-through (`decorators/__init__.py:185`) and `start_as_current_span` yields a non-recording span (`laminar.py:982`).
- **Global side effect:** sets `OTEL_ATTRIBUTE_COUNT_LIMIT=10000` when it is unset (`laminar.py:417-420`), because each message is at least two attributes.
- `LMNR_DISABLE_TRACING=true` is read **per span**. The processor marks those spans `lmnr.internal.disabled` and drops them (`tracing/processor.py:157,261`).
- No multi-project support.

## 2. TracerProvider and coexistence

**Langfuse**: it shares the global provider (`_init_tracer_provider`, `resource_manager.py:628-669`).
- **No global provider set** (`ProxyTracerProvider`): it creates `TracerProvider(resource={langfuse.environment, langfuse.release}, sampler=TraceIdRatioBased(rate) if rate<1, id_generator)` **and calls `trace.set_tracer_provider`**.
- **Global provider already set:** it **adds its processor to the user's provider**. `id_generator` is then ignored with a warning, and so is `sample_rate`, because the provider is not theirs.
- `tracer_provider=` lets the user pass an explicit provider, for example an isolated one.
- Langfuse's own tracer is `provider.get_tracer("langfuse-sdk", version, attributes={"public_key": pk})` (`constants.py`).
- **Export filter** (`_client/span_filter.py`, public at `span_filter.py`): `should_export_span` defaults to `is_default_export_span`, which accepts a span if any of these hold:
  1. the scope is `langfuse-sdk`;
  2. any attribute key starts with `gen_ai.`;
  3. the scope matches a known LLM instrumentation prefix (`KNOWN_LLM_INSTRUMENTATION_SCOPE_PREFIXES`). Matching is boundary-aware (`== p` or `startswith(p + ".")`). The list includes `openinference`, `ai` (Vercel AI SDK), `litellm`, `langsmith`, `haystack`, `pydantic-ai`, `strands-agents`, `vllm`, `agent_framework`, `autogen-core`, and about 25 `opentelemetry.instrumentation.{openai,anthropic,bedrock,langchain,…}` scopes.
  - HTTP, DB and framework spans are **dropped** by default.
  - A user callback **replaces** the default. Users compose by calling `is_default_export_span` inside it.
  - If the callback raises, the span is dropped and an error is logged.
- **"App root" marking** (`span_processor.py:342-374`):
  - The filter drops intermediate spans, so the server cannot find the trace root reliably. At `on_start`, the processor tracks each span id → "expected exported" and sets `langfuse.internal.is_app_root=true` on the first exported span whose parent is not exported.
  - A `langfuse_trace_id` baggage entry suppresses the mark when a parent service already claimed the root.
  - The server reads it in `packages/shared/src/server/otel/OtelIngestionProcessor.ts:1239`.
  - `as_root` (`langfuse.internal.as_root`) is set when the user forces a new trace via `trace_context`.
- TS **(docs)**: the user adds `new LangfuseSpanProcessor()` to their own `NodeSDK`. Isolation goes through `setLangfuseTracerProvider(provider)` from `@langfuse/tracing` with a non-global `NodeTracerProvider`. The default filter is the same, and `isDefaultExportSpan` is exported. Multi-project uses several processors, or `withLangfuseProject(credentials, fn)`.

**Laminar**: owns a provider and isolated context (`opentelemetry_lib/tracing/__init__.py:71-233`).
- Always builds its **own** `TracerProvider(resource={service.name: argv[0]})`. It is built once per process and reused across init/shutdown cycles.
- It sets that provider as global **only if** no global is registered and `set_global_tracer_provider=True` (the default). With an existing global, Laminar's provider stays private. Instrumentations are given Laminar's provider explicitly, so their spans still flow to Laminar. The docs warn that `False` "may break some external instrumentations, e.g. LiteLLM".
- Also creates and globally registers a `LoggerProvider` with an OTLP log exporter.
- **No span filter:** every span that reaches Laminar's provider is exported, which includes HTTP and DB spans when Laminar's provider is the global one. The only drops are disabled spans and Anthropic's native OTel spans, which are duplicates of Laminar's own wrapper (`processor.py:148-155`).
- **Isolated context** (`tracing/context.py`): Laminar keeps its **own** `ContextVar` runtime context (`IsolatedContextVarsRuntimeContext`) plus a token stack.
  - Laminar spans parent from the isolated context, not from the global OTel context. Trace continuation from an app's global context needs `preserve_global_context=True` on `observe`.
  - Each decorated call *also* attaches to the global OTel context (`decorators/__init__.py:219-223`), "so that spans [of third-party auto-instrumentations] know their parent".
  - The result is two context systems to keep in sync. This is fragile: the detach is skipped when the asyncio task changed (`:241-249`).

## 3. Exporter

| | Langfuse | Laminar |
|---|---|---|
| Protocol | OTLP **HTTP/protobuf** (`opentelemetry-exporter-otlp-proto-http`) | OTLP **gRPC** by default on port 8443; HTTP/protobuf with `force_http=True` (`tracing/exporter.py:167-184`) |
| Endpoint | `{base_url}/api/public/otel/v1/traces`, overridable by `LANGFUSE_OTEL_TRACES_EXPORT_PATH` | gRPC `{base}:8443`; HTTP `{base}:443/v1/traces` (path added only if missing). The standard `OTEL_EXPORTER_OTLP_{ENDPOINT,HEADERS,PROTOCOL}` env vars apply when no API key is given |
| Auth | `Authorization: Basic base64(pk:sk)`, plus `x-langfuse-sdk-name`, `x-langfuse-sdk-version`, `x-langfuse-public-key`, `x-langfuse-ingestion-version: 4` (a server write-path hint, `OtelIngestionProcessor.ts:345`); `additional_headers` | `authorization: Bearer <project key>` |
| Compression | gzip by default; resolved from the arg, then `LANGFUSE_OTEL_COMPRESSION`, then `OTEL_EXPORTER_OTLP_TRACES_COMPRESSION`, then `OTEL_EXPORTER_OTLP_COMPRESSION`; gzip or none only, because the server decodes only gzip (`span_processor.py:77-127`) | gzip, always |
| Batch | `BatchSpanProcessor` with OTel defaults (queue 2048, batch 512, 5 s delay) unless `flush_at`/`LANGFUSE_FLUSH_AT` or `flush_interval`/`LANGFUSE_FLUSH_INTERVAL` are set; export timeout = `timeout` (5 s) | `BatchSpanProcessor(max_export_batch_size=64)` with a 30 s export timeout, because big LLM payloads caused `DEADLINE_EXCEEDED`. Optional `SizeLimitedBatchSpanProcessor` (`flush_by_size`, 32 MiB, `tracing/batch_processor.py`) flushes before a span would exceed the byte budget. `disable_batch` or `LMNR_DEBUG` switch to `SimpleSpanProcessor` |
| Max request | `max_request_size` = `LANGFUSE_OTEL_MAX_BATCH_SIZE_BYTES` (64 MiB). Oversized batches are dropped and logged | byte budget only with `flush_by_size` |
| Retry | the OTel HTTP exporter's built-in exponential backoff | the OTel exporter's built-in retry |
| Custom | `span_exporter=` replaces the exporter, used for tests and other backends | `exporter=` in `init_tracing` |

Langfuse wraps the exporter in `LangfuseTransformingSpanExporter` (`_client/span_exporter.py`). At export time it uploads media found in data URIs and applies `mask_otel_spans`.

## 4. Lifecycle

**Langfuse**
- `flush()` runs `tracer_provider.force_flush()` (the **whole** provider, including the user's other processors), then wakes the score consumer and `join()`s the score and media queues (`resource_manager.py:601-616`).
- `shutdown()` unregisters the atexit hook, flushes, then stops and joins the consumer threads. It does **not** shut down the provider.
- `atexit.register(self.shutdown)` runs per resource manager.
- **Fork safety:**
  - `os.register_at_fork(after_in_child=…)` through a `WeakMethod` recreates the httpx clients (inherited sockets would corrupt TLS), the score and media queues (so items are not duplicated across workers), the consumer threads, and the class lock (which may be held by a parent thread that no longer exists).
  - On macOS it sets `no_proxy=*` to avoid SystemConfiguration crashes after fork (`resource_manager.py:397-477`).
  - Span export relies on OTel BSP's own fork handler.
- **Serverless:** call `flush()` at the end of each invocation. TS **(unverified)**: `LangfuseSpanProcessor({ exportMode: "immediate" })` plus `forceFlush()` in Vercel/Lambda.
- The flush docstring notes that ingestion is async on the server, so data is not queryable for 15–30 s after `flush()` (`client.py:2224-2231`).

**Laminar**
- `Laminar.flush()` → `force_flush` on the span and log processors.
- `Laminar.force_flush()` **shuts down and rebuilds** the processor and exporter, and clears the isolated context. It is intended for the end of a Lambda invocation, "where a regular flush might be killed… because the export runs in a background thread" (`laminar.py:1910-1924`). It breaks active traces.
- `shutdown()` tears down tracing so that a later `initialize()` works; the providers are kept. `atexit.register(wrapper.exit_handler)` is registered in `tracing/__init__.py:231`.
- **Fork:** `SizeLimitedBatchSpanProcessor` restarts its flush thread in the child through `register_at_fork` (`batch_processor.py:168-200`).

## 5. `observe` decorator

| Aspect | Langfuse (`_client/observe.py`) | Laminar (`sdk/decorators.py`, `opentelemetry_lib/decorators/__init__.py`) |
|---|---|---|
| Forms | `@observe` and `@observe(...)` | `@observe(...)` only (keyword-only factory) |
| Sync/async | picks a wrapper with `_is_coroutine_function` | `is_async` follows `__wrapped__`; the async wrapper `await`s |
| Generators | detected on the **result** (`inspect.isgenerator`/`isasyncgen`), plus a duck-typed starlette `StreamingResponse.body_iterator`. Wrapped in `_ContextPreserved{Sync,Async}GeneratorWrapper`, which copies `contextvars` at creation and runs each `next`/`__anext__` in it (async: `asyncio.create_task(..., context=ctx)`). The span ends on exhaustion, error, `close()`/`aclose()` or `__del__`. Cancellation before the first resume is deferred so cleanup runs first | a result `isinstance` check on `Generator`/`AsyncGenerator`. A plain `for … yield` re-yield wrapper; the span ends in `finally`. Item iteration does **not** re-enter the span context |
| Chunk output | items collected. With `transform_to_string(items)` the output is the callback's result; otherwise all-`str` chunks are `"".join`ed and anything else stays a list | always a list of chunks, or `output_formatter(list)` |
| Input | `{"args": args[1:] if is_method, "kwargs": kwargs}`. `is_method` is true when a parameter is **named** `self`/`cls` *anywhere* in the signature | **named-parameter dict** `{param: value}` built from `inspect.signature`. `self`/`cls` are skipped only as the first parameter. `ignore_inputs=[names]` drops individual parameters; `input_formatter(*args, **kwargs)` replaces the whole input |
| Control kwargs | `langfuse_trace_id`, `langfuse_parent_observation_id` and `langfuse_public_key` are popped from the **call's** kwargs (magic kwargs) | none; decorator args `session_id`, `user_id`, `tags`, `metadata` |
| Flags | `capture_input`/`capture_output`, defaulting to `LANGFUSE_OBSERVE_DECORATOR_IO_CAPTURE_ENABLED` (true) | `ignore_input`, `ignore_output`, `output_formatter` |
| Serialization | `json.dumps(cls=EventSerializer)` (`_utils/serializer.py`): max depth 20 (then `"<dict>"`), a cycle guard by `id` for `__dict__` objects. Handles pydantic `model_dump` (recursed), dataclasses `asdict`, numpy `.item()`/`.tolist()`, bytes (utf-8 or `"<not serializable bytes>"`), NaN/Inf as strings, ints beyond 2^53 as strings (the server is JS), `SecretStr` → `"<secret>"`, enum value, Path, UUID, langchain `Serializable.to_json`, `__slots__`. Falls back to `"<TypeName>"` and never raises. A non-`str` value becomes a JSON string; a `str` is stored **raw**, not JSON-quoted | `orjson.dumps` (dataclass, UUID, UTC-Z, non-str keys) with `default_json`: bytes as **base64**, pydantic/Mapping/Sequence/set unwrapped, otherwise `str(o)`. Retries with stringified keys and finally returns `"{}"`. Cycles and depth errors end in `"{}"`. **Every** value is JSON-encoded, strings included |
| Size limit | none in the SDK; only OTel attribute limits and the 64 MiB batch cap | 10 MB per input/output, truncated with `"...[Laminar: truncated]"` (`tracing/span.py:42-62`) |
| Exceptions | `update(level="ERROR", status_message=str(e) or type)`, which also sets OTel status ERROR (`span.py:794-814`). **No `record_exception` event**, so no stack trace. Catches `CancelledError` too | `span.record_exception(e, escaped=True)` plus `set_status(ERROR, str(e))` |
| Name | `name or func.__name__` | `name or fn.__name__` |
| Type | `as_type ∈ {span, generation, embedding, agent, tool, chain, retriever, evaluator, guardrail}` (`constants.py`); invalid → warning and `span` | `span_type ∈ {DEFAULT, LLM, TOOL, EXECUTOR, EVALUATOR, HUMAN_EVALUATOR, EVALUATION}`; evaluator types set `trace_type=EVALUATION` |
| Nesting | OTel global context (`start_as_current_observation(end_on_exit=False)`) | isolated context plus the global attach (see §2) |
| Threads | nothing built in. Docs tell users to call `ThreadingInstrumentor().instrument()` **(docs)** | **monkey-patches `threading.Thread.__init__`** so targets inherit the isolated context and token stack (`context.py:245-283`), and runs a forked `ThreadingInstrumentor` (`opentelemetry/instrumentation/threading`) on init |

## 6. Manual spans and LLM spans

**Langfuse**
- `start_observation(name, as_type, input, output, metadata, model, …)` returns a typed wrapper: `LangfuseSpan`, `LangfuseGeneration`, `LangfuseAgent`, and so on (`_client/span.py`).
- `start_as_current_observation(...)` is a context manager with `end_on_exit`.
- `trace_context={"trace_id", "parent_span_id"}` builds a `NonRecordingSpan` parent so a trace can be continued by id (`client.py:1665`).
- `create_trace_id(seed=)` and `_create_observation_id(seed=)` are deterministic: `sha256(seed)[:16]` and `[:8]` (`client.py:1708-1803`).
- `wrapper.update(name, input, output, metadata, version, level, status_message, completion_start_time, model, model_parameters, usage_details, cost_details, prompt)` (`span.py:816`). It masks and processes media first, then sets attributes with `_set_span_attributes_within_limit`. That helper drops new keys rather than reserved ones when `OTEL_SPAN_ATTRIBUTE_COUNT_LIMIT` (default 128) would be exceeded, and warns.
- `update_current_span`, `update_current_generation`, `create_event`, `set_current_trace_as_public`.
- Generation attributes (`attributes.py`):
  - `langfuse.observation.model.name`
  - `…model.parameters` (JSON)
  - `…usage_details` (JSON dict of arbitrary str→int, e.g. `{"input":…, "output":…, "cache_read_input_tokens":…}`)
  - `…cost_details` (JSON dict str→float, USD)
  - `…completion_start_time` (ISO JSON string)
  - `…prompt.name` and `…prompt.version`
  - input/output as JSON; messages are not split into indexed attributes.

**Laminar**
- `Laminar.start_as_current_span(name, input, span_type, context, parent_span_context, tags, user_id, session_id, metadata, attributes)`, plus `start_span`, `start_active_span`, `use_span` (`laminar.py:923-1640`).
- Setters: `set_span_output`, `set_span_attributes({Attributes.X: v})` (non-OTel values are JSON-dumped), `set_span_tags`/`add_span_tags`, `set_trace_session_id`/`user_id`/`metadata`. `LaminarSpan` also has `set_input`/`set_output`.
- LLM attributes are **GenAI semconv** (`tracing/attributes.py`):
  - `gen_ai.usage.input_tokens` and `gen_ai.usage.output_tokens`
  - `llm.usage.total_tokens`
  - `gen_ai.system`, `gen_ai.request.model`, `gen_ai.response.model`, `gen_ai.response.id`
  - cost as `gen_ai.usage.input_cost`, `…output_cost`, `gen_ai.usage.cost`
- Cross-service propagation: `serialize_span_context()` / `LaminarSpanContext` is a JSON blob with trace id, span id, path, ids_path, user/session/metadata and trace_type. It can be passed via `parent_span_context=` or the `LMNR_SPAN_CONTEXT` env var for subprocesses (`laminar.py:480`).

## 7. Context propagation (session, user, tags, metadata)

**Langfuse**
- `propagate_attributes(user_id, session_id, metadata, version, tags, trace_name, environment, prompt, as_baggage=False)` is a top-level context manager (`_client/propagation.py:122`).
- It stores the values in the **OTel context** under the keys `langfuse.propagated.<k>`. It also sets them **on the current span immediately**. `LangfuseSpanProcessor.on_start` then copies them onto **every** new span, third-party instrumentation spans included (`span_processor.py:237-277`).
- Merge rules: tags union, metadata dict-merge.
- Validation: strings ≤200 chars (otherwise dropped with a warning). `environment` must match `^(?!langfuse)[a-z0-9_-]+$` and be ≤40 chars.
- Spans created **before** entering the block do not get the values. The docs call this out as an anti-pattern. Trace-level fields are server-side aggregations of span attributes.
- `as_baggage=True` also writes W3C baggage `langfuse_<key>` and `langfuse_metadata_<k>`. The processor reads baggage at `on_start` too. **Privacy caveat in the docstring:** baggage "is added to HTTP headers on ALL outbound requests", so it is off by default.
- `langfuse_trace_id` baggage is used for app-root dedup across services.

**Laminar**
- Decorator `session_id`/`user_id`/`metadata`, `start_as_current_span(...)` and `Laminar.set_trace_*` put the values in the **isolated context** (`CONTEXT_{USER_ID,SESSION_ID,METADATA,TRACE_TYPE}_KEY`, `context.py:128-167`).
- `LaminarSpanProcessor.on_start` reads them from `parent_context` and writes `lmnr.association.properties.{user_id,session_id,trace_type,metadata.<k>}` on every span (`processor.py:208-236`).
- Global init `metadata` lands in the root isolated context.
- No baggage.
- Each span also gets a computed `lmnr.span.path` (list of names), `lmnr.span.ids_path` (UUID-formatted span ids), `lmnr.span.instrumentation_source=python`, `lmnr.span.sdk_version`, `lmnr.span.language_version` and `lmnr.span.release`. These come from processor-side maps keyed by span id (`processor.py:162-206`).

## 8. Attribute names

| Concept | Langfuse | Laminar |
|---|---|---|
| input / output | `langfuse.observation.input` / `.output` (JSON string, raw if `str`) | `lmnr.span.input` / `lmnr.span.output` (always JSON) |
| span type | `langfuse.observation.type` | `lmnr.span.type` |
| metadata | `langfuse.observation.metadata.<k>` (per-key JSON values, strings JSON-quoted in v5 so `"123"` ≠ `123`); trace-level `langfuse.trace.metadata.<k>` | `lmnr.association.properties.metadata.<k>` (scalar as is, else JSON) |
| session / user | `session.id` / `user.id` (OTel semconv-like) | `lmnr.association.properties.session_id` / `.user_id` |
| tags | `langfuse.trace.tags` (string array) | `lmnr.association.properties.tags` (deduplicated array) |
| trace name | `langfuse.trace.name` | the root span name |
| level / status | `langfuse.observation.level` (DEBUG/DEFAULT/WARNING/ERROR), `…status_message` | OTel status + exception event |
| model / usage / cost | `langfuse.observation.model.name`, `…usage_details`, `…cost_details` (JSON dicts) | `gen_ai.*` semconv (above) |
| environment / release | `langfuse.environment` and `langfuse.release` as **resource** attributes (when Langfuse owns the provider) and as span attributes via propagation; `langfuse.version` | `lmnr.span.release` (span); no environment concept |
| internal | `langfuse.internal.{as_root,is_app_root}`, `langfuse.trace.public` | `lmnr.internal.disabled`, `lmnr.span.{path,ids_path,parent_path,parent_ids_path}` |

Both servers also normalize third-party conventions (OpenInference, OpenLLMetry `traceloop.*`, GenAI) on ingest. The SDK attributes cover only manual and decorator spans.

## 9. Masking, sampling, environment

**Langfuse**
- `mask(*, data) -> Any` is applied **in the SDK at `update()` time** to input, output and metadata, before serialization (`span.py:723-761`). If the function raises, the value becomes `"<fully masked due to failed mask function>"` (per key for dict metadata).
- It only covers Langfuse wrapper spans, not third-party instrumentation spans.
- v5 adds `mask_otel_spans(params) -> {span_patches: {OtelSpanIdentifier: OtelSpanPatch(set_attributes, delete_attributes)}}`, which runs **at export time on whole batches**, so it covers every span (`types.py`, `span_exporter.py:307-509`). Spans are read-only snapshots; only attributes can be patched. An invalid result or an exception **drops the batch**: fail closed.
- TS **(docs)**: `new LangfuseSpanProcessor({ mask: ({data}) => string })` gets the **stringified** attribute value.
- Sampling: `TraceIdRatioBased(sample_rate)` on the provider Langfuse creates. With a user provider, the user's sampler applies.
  - Scores check the same ratio sampler on their trace id so that scores of dropped traces are not sent (`resource_manager.py:487-546`).
  - Non-ratio samplers keep all scores.
- Environment: client-level resource attribute, overridable per request with `propagate_attributes(environment=)`. Scores inherit the span's `langfuse.environment`.

**Laminar**
- No masking hook in the tracing core. `session_recording_options.mask_input` is for browser recordings only; the server repo has a separate `pii-redactor/` service.
- Sampling is not exposed.
- No environment.

## 10. Scores and feedback

**Langfuse**: REST, not OTLP.
- `create_score(name, value, trace_id|session_id|dataset_run_id, observation_id, score_id, data_type ∈ NUMERIC|BOOLEAN|CATEGORICAL|TEXT|CORRECTION, comment, config_id, metadata, timestamp, environment)` (`client.py:1885`).
- Values: bool and int are coerced to float; strings are categorical. The id is a random 16-hex string unless given. Naive timestamps are treated as UTC.
- Enqueued (non-blocking, `Queue(100_000)`, full → warn and drop) to a `ScoreConsumer` daemon thread. It posts batches to `POST /api/public/scores`: `flush_at` 15, `flush_interval` 1 s, byte-capped, `backoff` retry ×3 on retryable errors (`_task_manager/score_consumer.py`).
- Sugar: `score_current_span`, `score_current_trace`, `span.score()`, `span.score_trace()`.

**Laminar**
- No generic score API in the SDK.
- Eval scores go with datapoints (`POST /v1/evals/{id}/datapoints/{dp}` with `scores: {name: number}`, `sdk/client/*/resources/evals.py`).
- Post-hoc signals go through `client.traces.push_metadata(trace_id, dict)` → `POST /v1/traces/metadata`, which the server merges shallowly; and `client.tags.tag(trace_id, tags)`. Both work only after the trace is flushed; a 404 means "not ingested yet".

## 11. Instrumentation

**Langfuse**
- Bundles **no** generic auto-instrumentation. It relies on users installing OpenInference, OpenLLMetry, Vercel AI and similar, and accepts them through the default scope allow-list.
- Ships its own wrappers:
  - `langfuse.openai`, a drop-in `openai` module patch (`openai.py`, about 1.6k LOC);
  - `langfuse.langchain.CallbackHandler`, a LangChain callback.
- `LANGFUSE_OPENAI_SKIP_RAW_RESPONSES` exists to avoid double spans when LiteLLM uses the raw-response API.

**Laminar**
- **Auto-instruments everything installed, on `initialize`** (`tracing/instruments.py:192`). The default is `set(Instruments) - {LANGFUSE}`.
- `instruments=` restricts the set and `disabled_instruments=` subtracts from it.
- It vendors and forks about 30 instrumentations under `opentelemetry_lib/opentelemetry/instrumentation/` (openai, anthropic, google_genai, langchain, langgraph, litellm, mcp, openai_agents, pydantic_ai, claude_agent, …) and also pulls OpenLLMetry `opentelemetry-instrumentation-*==0.62.4` through the `[all]` extras.
- Hard-coded de-duplication heuristics:
  - with pydantic-ai installed, provider instrumentors are dropped unless deepagents is present;
  - an `in_litellm_context` flag;
  - Anthropic native spans are suppressed.
- Includes browser and session recording (playwright, browser-use, CDP).
- A "Langfuse bridge" can attach to Langfuse's provider.
- TS **(unverified)**: `Laminar.initialize({ instrumentModules: { OpenAI } })` because ESM needs module objects passed explicitly.

## 12. Mistakes and lessons visible in the code

- **Langfuse v3 (2025) moved to OTel; v4/v5 tightened it.** The default `should_export_span` filter and the scope allow-list exist because sharing the global provider meant exporting every HTTP and DB span. `is_app_root` exists because filtering breaks the "root span" notion. Langfuse now has to keep and grow a scope allow-list ("create a GitHub issue … to expand this default allow list").
- Sharing the global provider couples Langfuse to the app's sampler and limits. `flush()` force-flushes the user's other exporters, and `id_generator` and `sample_rate` are silently ignored when a provider exists. The TS SDK went further and made the user wire the processor, with `setLangfuseTracerProvider` for isolation.
- v5 changed metadata encoding to JSON-encode strings ("server decodes metadata values from v5 SDKs"), and sends `x-langfuse-ingestion-version: 4`. They needed a protocol version header to change attribute semantics: **version the wire format from day one**.
- Ints above 2^53 are turned into strings because the server is JS.
- They hit the OTel default attribute count limit (128). Langfuse drops attributes with warnings; Laminar overrides `OTEL_ATTRIBUTE_COUNT_LIMIT` process-wide, which is an intrusive global mutation.
- Fork bugs (Gunicorn `--preload`) caused silent loss of scores and media and TLS corruption. Hence the elaborate `register_at_fork`, including the macOS proxy workaround.
- Async-generator cancellation, context loss in generators and GC-time `__del__` finalization each needed custom wrappers in Langfuse.
- Laminar's isolated context and `Thread.__init__` monkey-patch, plus the dual attach to the global context, produce complex detach conditions (same asyncio task check). There is a whole `force_flush` = rebuild-processor path for Lambda.
- Laminar's gRPC default needed `max_export_batch_size=64` and a 30 s timeout because large LLM payloads caused `DEADLINE_EXCEEDED`, and later byte-based batching.
- `get_client()` returns a *disabled* client when several projects exist: a safe default after leakage concerns.

## 13. Testing

The installed wheels ship no tests. The code is clearly designed for:
- injectable exporters: Langfuse `span_exporter=` and `tracer_provider=`, Laminar `exporter=`, which allow tests with an OTel `InMemorySpanExporter`;
- singleton reset hooks: `LangfuseResourceManager.reset()`, Laminar `reset_tracing`/`clear_tracing_state` (`tracing/wrapper.py`);
- an injectable `httpx_client` for mocking the REST and score APIs.

Langfuse's server-side ingestion tests live in `packages/shared/src/server/otel/*.test.ts`.

---

## Implications for us

1. **Do not take over the global provider silently.**
   - Python: create a private `TracerProvider` by default, and register it globally only when none exists (Laminar's rule). Also offer `add_to(provider)` / a `SpanProcessor` export for apps that already run OTel (Langfuse TS style).
   - TS: export `LlmobsSpanProcessor` as the primary API, plus a `register()` convenience.
2. **Ship a default export filter and make it composable.** Our llmobs scope, any `gen_ai.*` attribute, and known LLM scopes (OpenInference, OpenLLMetry, Vercel `ai`, …), with boundary-aware prefix matching. Expose `is_default_export_span` so a user callback can extend the default instead of replacing it. Drop the span when the callback throws.
3. **Handle filtered-out parents.** Either mark the app root in the SDK (Langfuse `is_app_root`) or, better, have our ingestion re-parent orphans to the nearest exported ancestor. Decide this in spec 02 rather than leaving it to the SDK.
4. **Exporter:** OTLP HTTP/protobuf with gzip on by default, one documented path, and `Authorization: Bearer sk-llmobs-…` (no gRPC; one binary). Send `x-llmobs-sdk-{name,version}` and a **wire/attribute-convention version header** from v1.
5. **Batching:** a byte-aware batch processor (count *and* bytes, about 4–8 MiB per request), because LLM payloads are large. Keep OTel queue defaults, and drop oversized single spans with a logged id and size, never the content.
6. **Lifecycle:**
   - `flush()` flushes only *our* processor and score queue, not the user's provider;
   - `shutdown()` is idempotent;
   - register at exit;
   - `register_at_fork` re-creates HTTP clients, queues and threads in the child;
   - document per-invocation `flush()` for serverless, and offer an immediate/simple-processor mode (TS `exportMode`).
7. **A missing key gives a no-op client and a single warning**, never a raise (Laminar's raise is hostile in tests and CI). Add an `LLMOBS_TRACING_ENABLED=false` kill switch and honour `OTEL_SDK_DISABLED`.
8. **Use one OTel context.** Do not build an isolated context like Laminar's. For threads, rely on `contextvars` and document or auto-enable `opentelemetry-instrumentation-threading` (an optional `instrument_threads=True`). Never monkey-patch `Thread.__init__` ourselves.
9. **`observe` decorator:**
   - support `@observe` and `@observe(...)`, sync, async, generators and async generators, with context re-entered for each item (Langfuse's wrapper approach, including `aclose`, `close` and cancellation);
   - capture input as a **named-parameter dict** via `inspect.signature` (Laminar), skipping only a bound first `self`/`cls`, with `ignore_inputs=[…]`;
   - avoid magic call kwargs.
10. **Generator output:** join all-`str` chunks; otherwise store the list, with an optional `reduce_output` callback.
11. **One shared serializer with explicit rules:**
    - pydantic `model_dump(mode="json")`, dataclasses, enum, UUID, datetime as RFC 3339 UTC, bytes as `<bytes len=N>` or base64 under a cap, numpy, sets as lists, NaN/Inf as strings;
    - a cycle guard and depth limit;
    - a fallback of `"<TypeName>"` that never raises.
    - **Always JSON-encode values, strings included** (the lesson of Langfuse v5), so the server can tell `"123"` from `123`.
12. **Truncate in the SDK** to a configurable per-attribute limit (Laminar uses 10 MB; ours should be smaller, e.g. 1 MiB) with an explicit truncation marker attribute. Raise the span attribute count limit **on our own provider's `SpanLimits`**, not through process env.
13. **Errors:** set OTel status ERROR *and* `record_exception` (type, message, stacktrace event). Treat `CancelledError` as an error and record it as cancelled. Langfuse loses stack traces.
14. **Propagation API:** a `propagate(session_id=, user_id=, tags=, metadata=)` context manager stored in the OTel context and stamped onto **every** span by our processor's `on_start`, third-party spans included. Also set it on the current span on entry. Validate lengths and drop with a warning. Baggage stays **opt-in** with a privacy warning.
15. **Attribute names:** emit standard GenAI semconv for model, usage and cost wherever one exists (as Laminar does). Use the `llmobs.*` namespace (one constant) only for input, output, span kind, metadata.<k> and tags. Use `session.id` and `user.id`. Put environment and release on the **resource**, with a per-request override attribute. Fix the span-kind enum to match our data model (spec 01).
16. **Masking:** offer one hook that runs at **export time** on every exported span's attributes. It should fail closed (drop the span, log its id), because it covers third-party spans too. An optional `mask(value)` can run at capture time on decorator input and output to avoid serializing secrets at all.
17. **Scores:** do not route them through OTLP. Use a REST batch endpoint with a background queue (bounded, non-blocking, retry with backoff, flushed by `flush()`).
    - API: `score(trace_id=, span_id=, name=, value=, …)`, `score_current_span(...)`, `score_current_trace(...)`.
    - Score ids are UUIDv7 generated client-side, which gives idempotent retries.
    - Skip scores of traces sampled out by a ratio sampler.
18. **Instrumentation:** do **not** vendor or fork instrumentations (Laminar's ongoing maintenance burden). Accept OpenInference, OpenLLMetry and GenAI via the filter. Offer an opt-in `instrument=[...]` helper that only *calls* installed upstream instrumentors. Never auto-enable everything on init.
19. **Deterministic ids:** helpers to derive trace and span ids from a seed (hash-truncate) for linking external ids. Accept `trace_context={"trace_id","parent_span_id"}` to continue traces.
20. **Testing:** make the exporter, provider and HTTP client injectable. Test with an OTel `InMemorySpanExporter` and assert the exact attributes against shared golden fixtures in `fixtures/otlp/`. Add a fake HTTP server for score batching, retries and fork tests.
