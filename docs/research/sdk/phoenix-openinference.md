# Phoenix + OpenInference SDK design

Research notes for our Python and TypeScript tracing SDKs. Read-only study; no code copied.

**Sources and path prefixes**
- `phoenix/…` is the Phoenix repo (`/home/sebastien/github/phoenix`, checkout of 2026-11). Packages read: `arize-phoenix-otel` 0.17.3 (Python) and `@arizeai/phoenix-otel` 2.3.0 (TS).
- `oi/…` is `openinference/` inside the capture venv (`tools/capture/py/.venv/lib/python3.13/site-packages/`): `openinference-instrumentation` 0.1.75, `openinference-semantic-conventions` 0.1.43, `openinference-instrumentation-openai` 0.1.69.
- `otel/…` is `opentelemetry/` in that same venv.
- The OpenInference TS repo (`@arizeai/openinference-core`) is not available locally. TS helper behaviour below comes from Phoenix docs (`phoenix/docs/phoenix/sdk-api-reference/typescript/…`) and from `phoenix/js/packages/phoenix-otel/test/`. It is marked *(docs)*.

---

## 1. `register()`

### Python: `phoenix.otel.register`

Source: `phoenix/packages/phoenix-otel/src/phoenix/otel/otel.py:65-198`.

**Signature.** `register(*, endpoint, project_name, batch=False, set_global_tracer_provider=True, headers, protocol, verbose=True, auto_instrument=False, api_key, **kwargs)`. Any extra `**kwargs` go to the SDK `TracerProvider` (resource, sampler, id_generator, span_limits).

**Provider.**
- It always builds its own `phoenix.otel.TracerProvider`. That class subclasses `openinference.instrumentation.TracerProvider` (`oi/instrumentation/_tracer_providers.py`), which subclasses the OTel SDK provider.
- With the default `set_global_tracer_provider=True`, it then calls `trace_api.set_tracer_provider` (`otel.py:181`).
- OTel Python allows setting the global provider only once (`otel/trace/__init__.py:515-552`, `_TRACER_PROVIDER_SET_ONCE`). A second call only logs "Overriding of current TracerProvider is not allowed". So if the app set its own provider first, `register()` silently does not become global, and only code given the returned provider explicitly uses it.

**Processor default.**
- The default is **`SimpleSpanProcessor`** (`batch=False`): one synchronous export per span end.
- `verbose=True` prints a banner to stdout that warns "strongly advised to use a BatchSpanProcessor in production" (`otel.py:343-367`).
- Header values in the banner are masked as `****` (`_printable_headers`, `otel.py:699`).

**"Default processor" concept** (`otel.py:270-301`).
- The provider remembers which processor `register` installed.
- `add_span_processor(p)` *replaces* that default unless the caller passes `replace_default_processor=False`. This is how they avoid double export when a user adds their own exporter.
- The implementation edits the SDK's private `_active_span_processor._span_processors` tuple under its lock.

**Endpoint and protocol** (`otel.py:744-799`).
- Endpoint precedence: `endpoint` argument, then `PHOENIX_COLLECTOR_ENDPOINT`, then `OTEL_EXPORTER_OTLP_ENDPOINT`, then a `.env.phoenix` file found by walking up from the cwd, then `http://localhost:6006` (`settings.py:272-300`).
- Protocol inference: a URL with a path, or the Phoenix Cloud host, means `http/protobuf`. A bare origin means **gRPC on port `PHOENIX_GRPC_PORT`** (default 4317; `_construct_grpc_endpoint` swaps the port).
- So **with zero configuration the Python SDK exports over gRPC**. The HTTP exporter appends `/v1/traces` when it is missing (`_construct_http_endpoint`).
- A recent fix ("pick the OTLP protocol once", #17531 in the changelog) shows this inference had been inconsistent between components.

**Headers and auth.**
- `api_key` becomes `authorization: Bearer <key>`.
- Exporter wrappers (`HTTPSpanExporter`/`GRPCSpanExporter`, `otel.py:544-666`):
  - Header names are lowercased.
  - When no headers are given, they merge `PHOENIX_CLIENT_HEADERS` (W3C-baggage `k=v,k2=v2` format, URL-decoded, `settings.py:400+`) with `PHOENIX_API_KEY`.
  - `PHOENIX_API_KEY` is added only when no `authorization` header is present.
- `OTEL_EXPORTER_OTLP_HEADERS` is left for the OTel exporter itself to read.
- There is a warning when an endpoint read from `.env.phoenix` would receive credentials from a different source (`warn_if_using_file_endpoint_with_credentials`), which guards against credential exfiltration through a planted file.

**`auto_instrument`** (default False, `otel.py:806-818`).
- Discovers instrumentors with `importlib.metadata.entry_points(group="openinference_instrumentor")`. Each OpenInference instrumentor package declares one, e.g. `openinference_instrumentation_openai-0.1.69.dist-info/entry_points.txt`: `openai = openinference.instrumentation.openai:OpenAIInstrumentor`.
- It instantiates each one and calls `.instrument(tracer_provider=tp)`.
- Every installed package is instrumented, with no allow-list, and an entry point that fails to load raises.

**Project name.**
- Set as a *resource* attribute `openinference.project.name` (`oi/semconv/resource/__init__.py`).
- Resolution order: `project_name` argument, then `PHOENIX_PROJECT`, then `PHOENIX_PROJECT_NAME`, then `"default"` (`settings.py:303-334`). A one-time warning is logged when the two env vars conflict.
- When the user passes a `resource`, the project attribute is merged in and wins (`otel.py:151-160`).
- One provider therefore means one project. For notebooks, `dangerously_using_project(name)` (`oi/instrumentation/_projects.py`) monkeypatches `ReadableSpan.__init__` to rewrite each span's resource. This hack exists because the project is a resource attribute and not a span attribute.

**Span limits.** The OpenInference `TracerProvider` raises `max_span_attributes` from the OTel default 128 to **10,000**, unless `OTEL_SPAN_ATTRIBUTE_COUNT_LIMIT` or `OTEL_ATTRIBUTE_COUNT_LIMIT` is set (`_tracer_providers.py:44-66`). This is necessary because flattened messages (`llm.input_messages.{i}.message.contents.{j}…`) exceed 128 attributes quickly.

### TypeScript: `register(params)`

Source: `phoenix/js/packages/phoenix-otel/src/register.ts`.

**Parameters.** `{projectName, url, apiKey, headers, batch, instrumentations, spanProcessors, global=true, diagLogLevel}`.

**Provider.** It builds a `NodeTracerProvider` with resource `{openinference.project.name}`. `instrumentations` are passed to `registerInstrumentations({tracerProvider})`. There is no entry-point discovery in TS; the user lists the instrumentations.

**Processor default.**
- **`batch = true`** in TS (`getDefaultSpanProcessor`), the opposite of Python's default.
- The processor is a `LazyOpenInferenceSpanProcessor` (`src/lazyOpenInferenceSpanProcessor.ts`). It `import()`s the ESM-only `@arizeai/openinference-vercel` so that the CJS build still loads.
  - Spans that start or end before the import resolves are buffered and replayed.
  - If the import fails, it falls back to a plain Batch/Simple processor.
  - That vercel processor translates Vercel AI SDK spans to OpenInference *at export time*.
- `spanProcessors` replaces the default processor entirely.

**Transport.** Always `@opentelemetry/exporter-trace-otlp-proto` (HTTP + protobuf); there is no gRPC in TS. `ensureCollectorEndpoint` appends `/v1/traces` when it is missing and canonicalizes doubled or trailing slashes.

**Endpoint chain** (changelog 2.2.0):
1. `url`
2. `PHOENIX_COLLECTOR_ENDPOINT`
3. `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` (verbatim)
4. `OTEL_EXPORTER_OTLP_ENDPOINT` (with `/v1/traces` appended)
5. `PHOENIX_ENDPOINT`
6. localhost

An info line names the env var that supplied the endpoint. This was added after users "lost every span" by setting only the OTel variables.

**Headers.** Explicit headers override env headers case-insensitively. `apiKey` becomes `authorization: Bearer` unless an explicit authorization header exists (`mergeHeaders`, `getDefaultSpanProcessor`).

**`global: true`** (`attachGlobalTracerProvider`):
- Snapshots the existing OTel globals (read via `Symbol.for("opentelemetry.js.api.1")`).
- **Disables them** (`trace.disable()`, `context.disable()`, `propagation.disable()`).
- Installs its own provider, a new `AsyncLocalStorageContextManager` and a W3C tracecontext+baggage propagator.
- Mounts are kept on a stack. `provider.shutdown()` is wrapped to detach and restore the previous mount or snapshot. This lets experiments push a per-run provider temporarily.

---

## 2. Coexistence with existing OTel setups

- **Python is passive.** Because of OTel's set-once rule, `register()` cannot override an existing global provider. The documented paths are:
  - `register(set_global_tracer_provider=False)`, then pass `tracer_provider=` to each instrumentor;
  - or add Phoenix's `BatchSpanProcessor(endpoint=…)` (Phoenix-aware defaults, `otel.py:430-500`) to the user's own provider.
- **Double export is avoided** by the replaceable "default processor" (`add_span_processor` replaces it).
- **Double instrumentation is avoided** by the OTel `BaseInstrumentor` singleton: `_instance` plus the `_is_instrumented_by_opentelemetry` flag, so a second `instrument()` only logs a warning (`otel/instrumentation/instrumentor.py:41-116`).
- **TS is aggressive.** `global: true` replaces whatever is global, including the context manager and propagator, and restores it on shutdown. It is safe for experiment scoping but stomps on an app's own NodeSDK.
- **Provider resolved at call time (TS).** Helpers (`withSpan`, `traceChain`…) resolve the global tracer *when the wrapped function runs*, not when it is wrapped (*docs* `packages/phoenix-otel/tracing-helpers.mdx:47-51`). Module-level wrappers therefore follow later provider swaps.
- **OITracer applies to every tracer from the OpenInference provider.**
  - `TracerProvider.get_tracer` returns an `OITracer` (`_tracer_providers.py:27-33`). Any instrumentor, including non-OpenInference OTel ones, that obtains its tracer from that provider gets OpenInference masking and context attributes.
  - When the global provider is someone else's, only OpenInference instrumentors get this, because they wrap their tracer in `OITracer` themselves (`oi/instrumentation/openai/__init__.py:36-46`).

---

## 3. Context attributes (`using_*`)

Source: `oi/instrumentation/context_attributes.py`.

**Mechanism.**
- Plain OTel **context values**. The *span attribute name itself* is the context key: `set_value("session.id", v, ctx)` then `attach`.
- No baggage, no span processor, no cross-process propagation.
- They work as context managers (sync and async) and as decorators.
- The decorator handles sync, async, generator and async-generator functions. Generators use a "rolling context" (`_RollingOtelContext`, `_drive_sync_generator_in_rolling_otel`): the context is re-attached on every `send`/`throw`/`close`, because a context attached once would leak across yields.

**Keys** (`CONTEXT_ATTRIBUTES`, line 36):

| Helper | Attribute(s) |
|---|---|
| `using_session(id)` | `session.id` |
| `using_user(id)` | `user.id` |
| `using_metadata(dict)` | `metadata`, a **JSON string** via `safe_json_dumps` (`json.dumps(default=str, ensure_ascii=False)`) |
| `using_tags(list)` | `tag.tags` (string array) |
| `using_prompt_template(template, version, variables)` | `llm.prompt_template.template`, `llm.prompt_template.version`, `llm.prompt_template.variables` (JSON string) |
| `using_attributes(...)` | all of the above |

- An empty string or empty collection means "not set".
- Nested managers override; they do not merge. An inner `using_metadata` replaces the outer metadata entirely.

**Who reads them.**
- `get_attributes_from_context()` (line 456).
- `OITracer.start_span` merges `{context, user attributes, span kind}` in that order (`_tracers.py:159-198`), so context attributes are overridden by explicit ones.
- OpenAI-style instrumentors also call it explicitly when building request attributes (`openai/_request.py:376,455`).
- **Plain OTel tracers do not get them.** A non-OpenInference span has no `session.id` unless its tracer came from an OpenInference `TracerProvider`, or the user copies `get_attributes_from_context()` by hand (TS docs: `context-attributes.mdx:217-234`).
- So Phoenix sessions only work on OpenInference spans, or on spans whose tracer came from an OpenInference provider.

**TS equivalents** *(docs)*:
- `setSession(ctx, {sessionId})`, `setUser`, `setMetadata`, `setTags`, `setPromptTemplate` and `setAttributes(ctx, attrs)` are pure functions that return a new `Context`, used with `context.with(...)`.
- `getAttributesFromContext(ctx)` exists for manual spans.
- `setAttributes` allows arbitrary keys; Python has no equivalent for arbitrary keys.

**`capture_span_context()`** (`oi/instrumentation/_capture.py`). A ContextVar collector: every `OITracer.start_span` appends its span context. `get_first_span_id()` and `get_last_span_id()` return hex IDs to pass to the annotation API. This is how users get the span ID of an auto-instrumented LLM call for feedback.

**`suppress_tracing()`** (`oi/instrumentation/config.py:43-80`). Sets OTel's `_SUPPRESS_INSTRUMENTATION_KEY`. `OITracer.start_span` then returns `INVALID_SPAN`, still wrapped as an `OpenInferenceSpan`, so helper calls stay safe. TS re-exports `suppressTracing` from `@opentelemetry/core`.

---

## 4. Decorators and `OITracer`

Source: `oi/instrumentation/_tracers.py`, `_spans.py`.

**OITracer.** A `wrapt.ObjectProxy` over an OTel `Tracer`.
- `start_span` and `start_as_current_span` take an extra keyword-only `openinference_span_kind`.
- Masking runs **before the sampler** (`_get_masked_attributes_for_sampling`, a no-side-effect variant), so samplers never see hidden data. Real masking then runs per `set_attribute`.
- It uses its own `_IdGenerator` built on `secrets.randbits`, replacing the default `RandomIdGenerator`. A user's `random.seed()` would otherwise produce colliding trace and span IDs (`_tracers.py:79-107`).

**OpenInferenceSpan** (`_spans.py`). A proxy span.
- Every `set_attribute` goes through `TraceConfig.mask`.
- Non-recording spans skip masking side effects such as blob upload.
- The span kind (`openinference.span.kind`) is kept aside and **re-applied at `end()`**, so it cannot be overwritten or dropped. This also lets `set_input` work on non-recording spans.
- When `enable_genai_semconv` is set, it adds `gen_ai.*` attributes at `end()`.

**Span kinds** (`oi/semconv/trace/__init__.py:683-695`): `TOOL, CHAIN, LLM, RETRIEVER, EMBEDDING, AGENT, RERANKER, UNKNOWN, GUARDRAIL, EVALUATOR, PROMPT, DECISION`.

**Decorators.** `@tracer.chain`, `.agent`, `.retriever`, `.reranker`, `.guardrail`, `.evaluator` share `_chain`. `.tool` and `.llm`/`.decision` (`_model_call`) are separate. Each works with or without `(name=…)`.

**Input capture.**
- `inspect.signature(wrapped).bind(*args, **kwargs)` followed by **`apply_defaults()`**, so default-valued parameters are captured too.
- `self` is excluded: wrapt passes `instance` separately and the bound method's signature omits it.
- **`chain`-family unwraps a single argument** (`_tracers.py:825-829`). `f(q)` records `input.value = q`; several arguments record the args dict as JSON. `tool` and `llm` never unwrap and always record a JSON dict of the arguments.
- The span name is `name`, or `Class.method`, or the function `__name__` (`_infer_span_name`).

**Serialization and mime** (`_attributes.py:444-527`):

| Value | `*.value` | `*.mime_type` |
|---|---|---|
| `str`, `bool`, `int`, `float` | `str(v)` | `text/plain` |
| `Sequence`, `Mapping`, dataclass, pydantic | `json.dumps` with an encoder that handles dataclass → `asdict`, pydantic → `model_dump(mode="json")`, datetime → ISO, anything else → `str()`; `ensure_ascii=False` | `application/json` |
| anything else | `str(v)` | `text/plain` |

Only two mime types exist (`OpenInferenceMimeTypeValues`). Passing an explicit `mime_type` forces `str()` (text) or `json.dumps` (JSON, unless the value is already a string).

**Output.**
- `process_output` sets `output.value` **only if it was not already set** inside the body (`_ChainContext.process_output`). The user can call `span.set_output(...)` manually inside the function, but needs `start_as_current_span` to get the span object.
- Status is explicitly set to `OK` on success.
- `llm` and `decision` accept `process_input(*args, **kwargs) -> attrs` and `process_output(result) -> attrs` hooks. Hook exceptions become `warnings.warn` and the call is not broken.

**Generators.**
- Only `llm` and `decision` handle (async) generators. They collect every yielded item into a list and record **the list** as the output, also when the generator raises.
- `chain`, `tool` and the rest only branch on sync vs async. A generator function decorated with `@tracer.chain` ends its span immediately with `str(<generator object>)` as output. This is a known gap.
- In TS, `withSpan` handles sync and async functions only (*docs* `tracing-helpers.mdx:86-90`).

**Exceptions.** `use_span(record_exception=True, set_status_on_exception=True)` records an `exception` event, sets status ERROR and re-raises. TS behaves the same: it records, sets ERROR, ends the span and rethrows.

**Span methods.**
- `set_input(value, mime_type=)` and `set_output(...)` raise `ValueError` when the span is not an OpenInference span.
- `set_tool(name, description, parameters)` works only on TOOL spans.
- `@tracer.tool` infers `tool.description` from the docstring and `tool.parameters` as a JSON Schema built from type hints: `Annotated[T, "desc"]` gives a description, and `Union`/`Literal`/`datetime` are mapped (`_tracers.py:974-1060`).

**TS** *(docs)*:
- `withSpan(fn, {name, kind=CHAIN, tracer, attributes, processInput, processOutput, openTelemetrySpanKind})`.
- `traceChain`, `traceAgent` and `traceTool` are stable; `traceLLM`, `traceRetriever`, `traceReranker`, `traceEmbedding`, `traceGuardrail`, `traceEvaluator` and `tracePrompt` are `@experimental`.
- `@observe({kind, name})` decorates class methods and preserves `this`.
- `OITracer({tracer, traceConfig})`.

---

## 5. Manual LLM helpers and attribute keys

`get_llm_attributes(provider, system, model_name, request_model_name, response_model_name, invocation_parameters, input_messages, output_messages, token_count, tools)` (`_attributes.py:584-615`) returns a flat dict.

**Model and provider.**
- `llm.model_name` is `model_name`, else the response model, else the request model. `llm.request.model_name` and `llm.response.model_name` are set only when given.
- `llm.provider` is the host (openai, azure, aws, google…). `llm.system` is the API family (openai, anthropic, vertexai…).
- Helpers infer the provider from the host suffix (`infer_llm_provider_from_host`) and the system from the model prefix.
- `llm.invocation_parameters` is a JSON string.

**Messages**, flattened:
- `llm.input_messages.{i}.message.role` and `.message.content` (string).
- Multi-part content: `llm.input_messages.{i}.message.contents.{j}.message_content.type`, `….message_content.text`, `….message_content.image.image.url`.
- Reasoning: `….message_content.signature`, `.data`, `.encrypted_content`.
- Tools: `….message.tool_call_id`, and `….message.tool_calls.{k}.tool_call.id`, `.tool_call.function.name`, `.tool_call.function.arguments` (JSON string). Output messages use `llm.output_messages.*`.
- Other keys exist: `message.name`, `message.function_call_*`, audio, video, document.

**Tokens.**
- `llm.token_count.prompt`, `.completion`, `.total`.
- `llm.token_count.prompt_details.cache_read`, `.cache_write`, `.audio`, `.cache_input`.
- `llm.token_count.completion_details.reasoning`, `.audio`.

**Cost.** `llm.cost.prompt`, `.completion`, `.total`, plus `llm.cost.prompt_details.{input,cache_read,cache_write,cache_input,audio}` and `llm.cost.completion_details.{output,reasoning,audio}`. These are semconv keys; the helpers do not set them, because Phoenix computes cost server-side.

**Tools offered to the model.** `llm.tools.{i}.tool.json_schema` (JSON string), `.tool.name`, `.tool.description`. A TOOL span itself uses `tool.name`, `tool.description`, `tool.parameters` and `tool.id`.

**Other attributes.**
- `llm.finish_reason`.
- Embeddings: `embedding.embeddings.{i}.embedding.{text,vector}`.
- Retrieval: `retrieval.documents.{i}.document.{id,content,score,metadata}`.
- Reranker: `reranker.*`. Agent: `agent.name`. Graph: `graph.node.{id,name,parent_id}`. Prompt: `prompt.{vendor,id,url}`.
- `decision.*` mirrors `llm.*` for scoring and selection models.

**Annotations as span attributes** (`_attributes.py:236-350`).
- `get_annotation_attributes(annotations, scope="span"|"trace"|"session")` emits `annotations.{i}.annotation.{name,score,label,explanation,annotator_kind,identifier,metadata}`, or `trace.annotations.*` / `session.annotations.*`. An `evaluation.*` variant also exists.
- Each annotation needs a name and at least one of score, label or explanation.
- No ingestion path for these was found in Phoenix server code (`phoenix/src/phoenix`); this is a new convention.

---

## 6. TraceConfig and masking

Source: `oi/instrumentation/config.py`.

**Construction.**
- A frozen dataclass. Each field defaults to `None` and resolves as constructor argument, then env var, then default (`_parse_value`).
- A bad env value logs a warning and uses the default. A bad constructor type raises `TypeError`.
- Booleans accept only `"true"`/`"false"`, case-insensitive.

**Env vars**, all default false:
- `OPENINFERENCE_HIDE_INPUTS`, `_HIDE_OUTPUTS`
- `_HIDE_INPUT_MESSAGES`, `_HIDE_OUTPUT_MESSAGES`
- `_HIDE_INPUT_IMAGES`, `_HIDE_INPUT_TEXT`, `_HIDE_OUTPUT_TEXT`
- `_HIDE_LLM_INVOCATION_PARAMETERS`, `_HIDE_LLM_TOOLS`
- `_HIDE_EMBEDDINGS_VECTORS` (`_HIDE_EMBEDDING_VECTORS` is deprecated), `_HIDE_EMBEDDINGS_TEXT`
- `_HIDE_PROMPTS`, `_HIDE_CHOICES`, `_HIDE_RETRIEVAL_DOCUMENTS`
- `OPENINFERENCE_ENABLE_GENAI_SEMCONV`
- `OPENINFERENCE_BASE64_IMAGE_MAX_LENGTH` (default **32,000** characters)
- `OPENINFERENCE_BLOB_UPLOADER`: names an entry point in the `openinference_blob_uploader` group.

**How masking is applied.**
- `TraceConfig.mask(key, value)` is called per attribute by `OpenInferenceSpan.set_attribute`, so masking happens **at the source, in-process, before export**.
- It is key-pattern matching (substring tests on the flattened key) and returns one of three outcomes:
  - drop: `None`. Used for `input.mime_type` and for whole message trees when `hide_*_messages` is set.
  - replace with `"__REDACTED__"`: used for `input.value` and `output.value`.
  - keep.
- The hide checks come before the image-size check, so hidden images never reach an uploader.
- Values can be lazy callables, which are resolved only when kept, so redacted data is never even serialized.

**Images and documents.** A base64 data URI at `…image.url` or a document URL longer than `base64_image_max_length` is handed to the `BlobUploader`, which returns an absolute URI such as `s3://` or `https://`. Without an uploader, or on failure or an invalid URI, the value is replaced with `__REDACTED__` (`_externalize_or_redact`).

**TS.** `traceConfig` has the same options in camelCase (`hideInputs`, `hideLLMTools`…). It is passed per instrumentation (`new OpenAIInstrumentation({traceConfig})`) or to `OITracer` *(docs)*.

---

## 7. Payload size and truncation

- **OpenInference has no general string truncation.** The only size control is the base64 image and document limit (32k characters, then redact or upload).
- **OTel SDK defaults** (`otel/sdk/trace/__init__.py:79-84, 640-700`): `max_attribute_length = None`, meaning unlimited, unless `OTEL_ATTRIBUTE_VALUE_LENGTH_LIMIT` or `OTEL_SPAN_ATTRIBUTE_VALUE_LENGTH_LIMIT` is set. Attribute count is 128 by default, raised to 10k by the OpenInference provider (§1). Events and links are 128 each.
- The TS `register()` passes no `spanLimits`, so the JS SDK defaults apply (count 128, length unlimited). Large flattened LLM spans could lose attributes in TS; this was not verified against openinference-core.
- **BatchSpanProcessor defaults** (`otel/sdk/trace/export/__init__.py:41-44`): schedule delay 5000 ms, max export batch 512, export timeout 30000 ms, max queue 2048. All are overridable through `OTEL_BSP_*`. Spans are **dropped** when the queue is full.
- Neither SDK caps request bytes on the client side.

---

## 8. Annotations and scores

Source: `phoenix/packages/phoenix-client/src/phoenix/client/resources/spans/__init__.py:746-1106`; server `phoenix/src/phoenix/server/api/routers/v1/spans.py:1214-1280`.

**Transport.** REST over httpx, separate from OTLP: `POST /v1/span_annotations?sync=<bool>` with body `{"data": [SpanAnnotationData…]}`. There are also `/v1/trace_annotations`, `/v1/session_annotations`, `/v1/document_annotations` and `/v1/span_notes`.

**Item shape.**
```
{span_id: hex16, name, annotator_kind: "LLM"|"CODE"|"HUMAN"|…,
 result: {label?, score?: float, explanation?}, metadata?: obj, identifier?: str}
```
- At least one of label, score or explanation is required.
- **Upsert key:** `(name, span_rowid, identifier)`. A null identifier counts as `""`, so re-logging overwrites; a distinct identifier allows several annotations with the same name.
- The name `note` is reserved and returns 400; notes go through `/v1/span_notes`, which appends with a generated identifier.

**API.**
- `client.spans.add_span_annotation(span_id=, annotation_name=, annotator_kind="HUMAN", label, score, explanation, metadata, identifier, sync=False)`.
- `log_span_annotations(span_annotations=[…], sync=False)` sends one POST per call, with no client-side buffering.
- `log_span_annotations_dataframe(df)` sends chunks of 100 rows.
- An async client mirrors all of these. TS: `logSpanAnnotations({spanAnnotations, sync=false})` and `addSpanAnnotation` (`phoenix/js/packages/phoenix-client/src/spans/`).

**`sync` semantics.**
- `sync=false` (the default): the server *enqueues* precursors and returns `{"data": []}` immediately. The bulk inserter retries when the span is not ingested yet, every 10 s up to 60 times (`phoenix/src/phoenix/db/insertion/constants.py`). This solves the race where feedback arrives before the span.
- `sync=true`: the server checks that the spans exist (404 listing missing IDs), upserts, and returns the inserted GlobalIDs.

**Span ID.** Lowercase hex, obtained via `capture_span_context()` or `span.get_span_context().span_id`. Annotations are keyed by span ID only, not by trace ID plus span ID; the server looks up `span_id` across the database.

---

## 9. Testing approach

- **phoenix-otel Python** (`phoenix/packages/phoenix-otel/tests/test_otel.py`): unit tests with `unittest.mock` (patching env getters and `print`). They assert processor type, exporter endpoint, transport, resource merging, kwargs pass-through and header masking. There are no network tests.
- **phoenix-otel TS** (`phoenix/js/packages/phoenix-otel/test/`): vitest.
  - `register.test.ts` and `endpointConformance.test.ts` hold an env × argument matrix for URL resolution.
  - `openinference.test.ts` uses a counting `SpanProcessor` stub to check that wrappers route to the current global provider and that parenting works.
  - `lazyOpenInferenceSpanProcessor.test.ts` uses `InMemorySpanExporter`.
  - `entryPoints.test.ts` and `browserConfigSmoke.ts` check the ESM/CJS exports and a browser build of the config.
- **OpenInference instrumentors.** The usual pattern is `InMemorySpanExporter` behind a `SimpleSpanProcessor` on a fresh provider, often with VCR cassettes for provider HTTP. That repo is not checked out here, so this is from memory and unverified.
- **phoenix-client TS** ships `testing/`, `vitest/` and `jest/` reporters. They turn test suites into Phoenix experiments, which is evaluation tooling and not SDK tests.

---

## 10. TS specifics

- **Context manager.** `register({global:true})` installs a fresh `AsyncLocalStorageContextManager` from `@opentelemetry/context-async-hooks` and enables it. Without `global`, the user's own context manager is used, and wrappers fall back to the no-op context if none exists.
- **Runtime.**
  - Node only: `NodeTracerProvider` and async_hooks; Node ≥ 18 (changelog 2.0.0).
  - Dual ESM/CJS package. The ESM-only vercel processor is loaded through dynamic `import()` plus a buffer; `./vercel` is an ESM-only subpath.
  - `@arizeai/phoenix-config` has a browser build selected through a conditional export.
  - No edge runtime (Vercel Edge, Workers) support was found in register.
- **Flush and shutdown.** The docs only say `await provider.shutdown()` before exit, or in a SIGTERM handler, and at the end of serverless handlers (`docs/…/phoenix-otel/overview.mdx:195-212`).
  - There is no `waitUntil`/`after()` integration and no auto-flush.
  - Next.js setup is "`instrumentation.ts` calls `register`" (README:163-246).
  - `LazyOpenInferenceSpanProcessor.forceFlush` and `shutdown` first await the dynamic import.
- **Vercel AI SDK.** Translation happens in a span processor (`openinference-vercel`) that rewrites `ai.*`/`gen_ai.*` attributes into OpenInference keys before export. This is processor-side normalization; Phoenix itself does not normalize at ingest.
- **`createNoOpProvider()`.** A provider with no processors, used for tests and dry runs.

---

## 11. Notable mistakes and changes (changelogs)

Python changelog: `phoenix/packages/phoenix-otel/CHANGELOG.md`. TS changelog: `phoenix/js/packages/phoenix-otel/CHANGELOG.md`.

- **Protocol inference.** gRPC for a bare origin and HTTP for a path is surprising. It was patched repeatedly: "Explicit OTEL protocol override" #6067, "Handle default protocol" #6146, "infers GRPC port from env" #6017, "pick the OTLP protocol once" #17531.
- **Fragile private-API coupling.** These crashes came from reaching into SDK private fields to print the banner or replace processors:
  - "OTel moved the location of span exporters on batch processors" #7919, handled by the dual lookup at `otel.py:315-322`;
  - "register crashes when printing details" #7754;
  - "register() crash with exporter-otlp-proto-http 1.45" #16549.
- **Default-processor replacement surprised users.** It got an opt-out (#7736, `replace_default_processor`) and a fix (#15021, "only remove the default processor when replacing it").
- **Env var sprawl.**
  - `PHOENIX_PROJECT` vs `PHOENIX_PROJECT_NAME` was later unified with a conflict warning (TS 1.1.0).
  - `OTEL_EXPORTER_OTLP_ENDPOINT` support was added late (#8095); the TS 2.2.0 notes say spans had been silently sent to localhost.
  - `.env.phoenix` discovery (0.17.0) came with ownership and permission checks, a 64 KiB cap, and an opt-out via `PHOENIX_DISCOVER_CONFIG=false`.
- **Python/TS default mismatch.** Python uses `batch=False` and gRPC by default; TS uses `batch=true` and HTTP. The Python banner warns about Simple in production instead of changing the default.
- **ESM-only dependency in a CJS package.** It forced the lazy buffering processor (TS 2.0.0), and AI SDK v6 translation was dropped.
- **Deprecated alias kept.** `HIDE_EMBEDDING_VECTORS` vs `HIDE_EMBEDDINGS_VECTORS`.
- **Seeded RNG.** The custom ID generator shows they hit trace-ID collisions caused by user `random.seed()`.
- **`dangerously_using_project`.** It exists because the project lives on the Resource.

---

## Implications for us

1. **One processor default: batch.** Make batching the default in both SDKs, with a simple/sync mode only for tests and notebooks. Do not repeat Phoenix's Python/TS split.
2. **OTLP over HTTP + protobuf only, in both SDKs.** Do not infer gRPC from URL shape. Take one base URL and append `/v1/traces` when it is missing (canonicalize trailing slashes). Accept a full traces URL as-is.
3. **Small, documented env chain.** `LLMOBS_ENDPOINT`, then `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` (verbatim), then `OTEL_EXPORTER_OTLP_ENDPOINT` (+`/v1/traces`), then localhost. Log once, at info level, which source won. One name per setting, with no aliases (our no-compat rule).
4. **Auth as a single `Authorization: Bearer sk-llmobs-…` header** built from `api_key` or `LLMOBS_API_KEY`. Explicit headers win case-insensitively. Never print header values; mask them like Phoenix's `****`.
5. **Project as a resource attribute is a trap.** It forces one project per provider and leads to hacks like `dangerously_using_project`. Either derive the project from the API key server-side (a key is scoped to a project), or accept a per-span `llmobs.project` override. Decide this in the spec.
6. **Global registration.** Python: set the global provider only when none is set, and warn clearly when one exists. TS: never `disable()` the user's globals by default. Document "add our processor to your provider" as the first-class coexistence path, using our own processor class, not a private-field hack.
7. **Don't touch SDK private internals.** No banner introspection or editing of `_span_processors`. Keep a reference to our own processor instead. Phoenix broke three times on this.
8. **Raise the attribute count limit to about 10k on our provider, in both languages**, unless the user set OTel limits. Flattened messages overflow 128. Verify that the TS default really drops attributes and fix it there too.
9. **Context attributes via OTel context values, applied by a span processor's `on_start`**, not only by a tracer wrapper. That way `session.id`, `user.id`, tags and metadata land on *every* span, including spans from third-party OTel instrumentors. This closes Phoenix's main gap. Make the context managers decorator-capable, including (async) generators with a rolling context.
10. **Metadata as typed attributes, not one JSON string.** Prefer `llmobs.metadata.<key>` scalars, and merge nested scopes instead of replacing them. Tags are a string array.
11. **Decorators.**
    - Bind the signature, apply defaults and exclude `self`/`cls`.
    - Choose one rule for single arguments (Phoenix unwraps for chain but not for tool or llm, which is inconsistent) and use it everywhere.
    - Use JSON with a tolerant encoder (dataclass, pydantic, datetime, fallback `str`) and set `input.mime_type`/`output.mime_type`.
    - Support generators and async generators for *all* span kinds; aggregate output, with a hook.
    - Always set ERROR status and record the exception.
    - Output set manually inside the body wins.
12. **Resolve the tracer at call time** in TS wrappers and Python decorators, so module-level decorated functions follow provider changes and test providers.
13. **Masking in the SDK, before the sampler and before export.** Use typed config with env overrides (`LLMOBS_HIDE_INPUTS`/`OUTPUTS`/`MESSAGES`…). Use lazy values so redacted payloads are never serialized. Use a single redaction token. Logs never contain payloads (our privacy rule).
14. **Size controls.** Cap base64 media inline (about 32k characters) and offload or redact larger media. Our server already handles media (spec 02); decide whether the SDK strips or forwards it. Add an optional per-attribute length cap (`OTEL_ATTRIBUTE_VALUE_LENGTH_LIMIT` respected) so a single huge prompt cannot blow the OTLP request limit.
15. **Scores over REST, asynchronous by default, with server-side retry until the span arrives.** Keep a sync option that returns IDs and reports a missing span. Use an upsert key `(name, span_id, identifier)`. Reference spans by hex span ID, and also by trace ID to avoid cross-trace ambiguity. Provide a `capture_span_context`-style helper to get the IDs of auto-instrumented spans.
16. **Ship a cryptographic ID generator** that `random.seed` cannot influence. The convention is UUIDv7 for entities; OTel trace and span IDs stay random.
17. **TS runtime targets.**
    - Node first, using `AsyncLocalStorageContextManager` only when no context manager is set.
    - Expose `flush()` and `shutdown()`, plus a serverless helper (`withFlush(handler)` or `waitUntil`-compatible) instead of telling users to call `shutdown` per request.
    - Avoid ESM-only hard dependencies in a dual package.
18. **Optional auto-instrumentation via entry-point discovery** in Python, with an allow-list and per-instrumentor failure isolation (a log line, not a crash). Rely on `BaseInstrumentor`'s singleton for idempotence.
19. **Testing.** Expose a test helper: a provider plus an in-memory exporter fixture for users. Golden-test our SDK output against the shared `fixtures/otlp/` files so the SDK and server normalization agree on keys.
20. **Prefer OpenInference attribute names on the wire, or translate them at ingest.** Phoenix translates Vercel AI SDK spans client-side in a span processor; we normalize server-side (spec 02), so our SDK stays thin. Only our own `llmobs.*` additions are new.
