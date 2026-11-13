# 10 — SDKs (Python, TypeScript)

Both SDKs are thin layers over OpenTelemetry: tracing goes out as OTLP/HTTP (protobuf) to `/v1/traces`; everything else uses a small REST client generated from the OpenAPI spec. License: MIT (independent of the server license decision).

## Design rules

1. **Own `TracerProvider` by default.** `init()` creates a dedicated provider + `BatchSpanProcessor` + OTLP exporter and never registers it as the global provider, so we don't hijack the app's existing OTel setup. `llmobs.tracer_provider()` (TS `llmobs.tracerProvider()`) returns the provider in use (a no-op provider before `init()` or when disabled), for libraries that emit OTel spans themselves and accept a provider or tracer (pydantic-ai, the Vercel AI SDK, …). Spans such libraries send to the global provider reach llmobs only with `use_global_provider=True`. With `use_global_provider=True` (TS `useGlobalProvider: true`), Python adds our processor to the global SDK `TracerProvider` when one is set, otherwise registers ours as the global one; TypeScript (OTel JS 2.x providers cannot take processors after construction) registers ours as the global provider when none is set, otherwise logs one warning and stays private. Both SDKs expose `span_processor()` (TS `spanProcessor()`), our processor, for apps that build their own OTel pipeline. The OTel *context* is always the global one, so instrumentor spans nest under ours in both modes. `sample_rate` applies only to our own provider; in global mode with an existing provider a rate below 1 logs one warning and is ignored (D-F-4). With our own provider every span is exported (only our tracer and the instrumentors we bind create spans there). In global mode the default filter exports a span when its instrumentation scope is `llmobs` or `ai`, or one of its attribute keys starts with `llmobs.`, `gen_ai.`, `llm.`, `openinference.`, `ai.`, `traceloop.`, `langfuse.`, `lmnr.` or `gcp.vertex.agent.`, or is exactly `input.value` or `output.value`. A span without an instrumentation scope counts as not `llmobs`. `export_filter=` (TS `exportFilter`) replaces the rule in both modes; it gets the finished span and returns a boolean; if it raises, the span is exported. LLM spans whose parent was filtered out arrive as detached roots, which spec 01 already handles (`has_root = false`) (D-F-5).
2. **No custom instrumentation for frameworks.** Auto-instrumentation reuses OpenInference instrumentors (and OpenLLMetry where OpenInference lacks one), installed as optional extras and activated by `init(instrument=[…])`. Python takes instrumentor names from a fixed registry (§ Instrumentors): an unknown name or a missing package raises `ConfigError` naming the extra to install. TypeScript takes instrumentation objects (anything with `setTracerProvider`, `enable` and `disable` functions); the subpath helpers `llmobs/openai`, `llmobs/anthropic`, `llmobs/langchain` build the OpenInference instrumentor and call `manuallyInstrument(module)`, so ESM works without loader flags. The OpenInference packages are optional peer dependencies (D-F-7).
3. **Emit the standard.** Our spans use OTel GenAI attributes where a standard exists; `llmobs.*` only for what has no standard (kind, input/output for non-LLM spans, tags, metadata, environment, release, experiment linkage).
4. **Never break the app after `init()`.** `init()` raises `ConfigError` for an invalid configuration; a missing API key is not one: with no key given (argument and `LLMOBS_API_KEY` both unset) `init()` logs one warning (`llmobs: no API key; set LLMOBS_API_KEY, or LLMOBS_ENABLED=false to silence this; tracing is disabled`) and the SDK runs in no-op mode (D-F-3). Every other invalid value, including a key without the `sk-llmobs-` prefix, still raises `ConfigError`: a misconfigured deployment fails at startup, where it is noticed. Use `enabled=false` to run without llmobs. Decorators, context managers and `score()` used before `init()` or with `enabled=false` do nothing. After `init()` returns, nothing the SDK does raises into the app: exporter errors are logged at debug level, serialization failures fall back to `repr()`, and full buffers drop data (§ Lifecycle). The SDK has no required dependencies beyond `opentelemetry-sdk`, `opentelemetry-exporter-otlp-proto-http` and an HTTP client. Programming errors raise: an invalid `kind` given to `observe`/`span`, or an invalid `provider`/`model` given to `llm_span`, raises `ValueError` (Python) / `TypeError` (TS) when the API is called (decoration time for `observe`), before the user function runs. These are the only cases where the SDK raises after `init()`. Re-entering an already entered Python `context` object is a no-op logged at debug level.
5. **Same concepts, idiomatic APIs** in both languages.
6. **Init once.** Any `init()` after the first, whatever the first did, logs one warning and changes nothing (D-F-15).

## Configuration

| Option | Env | Default |
|---|---|---|
| `api_key` | `LLMOBS_API_KEY` | unset → one warning, SDK disabled (D-F-3) |
| `base_url` | `LLMOBS_BASE_URL` | `http://localhost:8080` |
| `environment` | `LLMOBS_ENVIRONMENT` | unset |
| `release` | `LLMOBS_RELEASE` | unset (Python: tries `GIT_COMMIT`/`SOURCE_VERSION` env) |
| `capture_content` | `LLMOBS_CAPTURE_CONTENT` | `true` (set `false` to drop inputs/outputs, keep metrics) |
| `sample_rate` | `LLMOBS_SAMPLE_RATE` | `1.0` (trace-ID ratio sampler) |
| `use_global_provider` (TS `useGlobalProvider`) | — | `false` |
| `instrument` | — | `[]` |
| `mask` | — | optional callable applied to every input/output/metadata value before export |
| `export_filter` (TS `exportFilter`) | — | none (D-F-5) |
| `enabled` | `LLMOBS_ENABLED` | `true` (no-op mode when false) |

TypeScript spells the options in camelCase (`apiKey`, `baseUrl`, `captureContent`, `sampleRate`); the env vars are the same.

**Resolution rules (D78).** Both SDKs resolve the configuration the same way:
1. An explicit argument wins over the env var, which wins over the default. An argument set to `None`/`undefined` counts as not given.
2. Env values are trimmed; an empty value counts as unset.
3. `enabled` is resolved first. When it is false, no other option is read or validated: the config holds the defaults and the SDK runs in no-op mode.
4. Booleans from env accept `true`/`false`, `1`/`0`, `yes`/`no`, `on`/`off`, case-insensitive.
5. `api_key`, when given, must start with `sk-llmobs-` (a missing key: rule 11, D-F-3). It never appears in error messages, `repr`/`toString`/`inspect` output, or JSON serialization of the config.
6. `base_url` must be an absolute `http` or `https` URL without query or fragment. A path is allowed (reverse-proxy prefix); trailing slashes are removed. A bracketed host must contain only hex digits, `:` and `.`; brackets anywhere else are rejected.
7. `sample_rate` must be a finite number in `[0, 1]`. From `LLMOBS_SAMPLE_RATE` it must also be a plain ASCII decimal (`1`, `0.25`, `.5`, `1.`): no sign, exponent, digit separators, hex, or non-ASCII digits.
8. `environment` must not be the reserved evaluation environment (`llmobs-eval`, D49).
9. `release` falls back to `GIT_COMMIT`, then `SOURCE_VERSION` (Python only), when neither the argument nor `LLMOBS_RELEASE` is set.
10. Python `instrument` must be a list of non-empty strings (a bare string is rejected); names are checked when instrumentation is added. TypeScript `instrument` must be an array of instrumentation objects (each with `setTracerProvider` and `enable` functions). `mask` must be callable. `export_filter` must be callable.
11. An invalid value raises a `ConfigError` naming the option or env var, never its value. Resolution stops at the first problem. A missing API key is not an error: `init()` logs `llmobs: no API key; set LLMOBS_API_KEY, or LLMOBS_ENABLED=false to silence this; tracing is disabled` and the SDK runs disabled (D-F-3). An unknown or uninstalled Python instrumentor is named in the error (instrumentor names are not secret), together with the extra to install.

## Lifecycle

- **Init once.** A second `init()` logs one warning and changes nothing, whatever the first one did: enabled, disabled, or missing key (D-F-15). Python re-creates the score queue's worker thread in a forked child (`os.register_at_fork`); OTel's batch processor already does the same for spans.
- **Exit.** Python: `init()` registers an `atexit` hook that calls `shutdown()`. TypeScript: where `process` exists, `init()` registers one `process.once("beforeExit", …)` listener that calls `shutdown()`; calling `init()` again does not add a second one. `beforeExit` fires when the event loop empties, not on `process.exit()` or a signal, and the SDK installs no signal handlers: apps that exit that way, and serverless handlers, `await llmobs.flush()` (or `shutdown()`) themselves (D80).
- **`flush()`** blocks (Python) or resolves (TS) when pending spans and scores are exported or the export timeout expires. **`shutdown()`** flushes, then stops the exporters; it is idempotent, and spans and scores after it are dropped.
- **Buffers.** Spans go through the OTel `BatchSpanProcessor` with its default queue size; when it is full, new spans are dropped (OTel behaviour). Scores are buffered up to 10 000; when full, new scores are dropped. Dropped score counts are logged at warning level at most once per minute, with counts only; span drops are reported by the OTel batch processor's own logging.
- **Score delivery.** A batch that gets a `2xx` response is done. `429`, any `5xx`, network errors and timeouts (10 s per attempt) retry the same batch up to 3 more times, waiting `Retry-After` when it is a non-negative integer number of seconds (capped at 30), else 1 s, 2 s, 4 s (an HTTP-date `Retry-After` uses the default), then drop with `llmobs: score batch failed after retries; dropped <n> scores`. Any other status drops the batch at once with `llmobs: score batch rejected with status <code>; dropped <n> scores`.
- **Identification.** Spans created by the SDK use the instrumentation scope name `llmobs` and the SDK version as scope version (instrumentor spans keep their own scope). The provider's resource carries `telemetry.distro.name = "llmobs"` and `telemetry.distro.version = <SDK version>` (OTel semantic conventions), so the server can tell SDK versions apart. Storing the version server-side is not in the MVP; `scope_name` is stored (01).

## Recorded data

Everything in this section is identical in both SDKs; the conformance fixtures pin it (§ Testing).

### Names

| Constant | Value |
|---|---|
| Instrumentation scope name of our spans | `llmobs` |
| Scope version | the SDK version (`__version__` / `VERSION`) |
| Resource attributes we add | `telemetry.distro.name = "llmobs"`, `telemetry.distro.version = <SDK version>` |
| `service.name` and other resource attributes | Python: OTel's `Resource.create`, which reads `OTEL_SERVICE_NAME` and `OTEL_RESOURCE_ATTRIBUTES` from `os.environ`. TypeScript: `OTEL_SERVICE_NAME` from the config's env source only (`OTEL_RESOURCE_ATTRIBUTES` is not read). This is a known difference (§ Language differences) |
| OTLP traces URL | `<base_url>/v1/traces` |
| Score URL | `<base_url>/api/v1/scores` |
| Feedback URL (browser) | `<baseUrl>/api/public/feedback` |
| Auth header | `Authorization: Bearer <key>` |
| OTel context key for context values | Python `opentelemetry.context.create_key("llmobs.context")`; TS `createContextKey("llmobs.context")` |

### Span attributes

| Key | Type | Written by | Value |
|---|---|---|---|
| `llmobs.span.kind` | string | `observe`, `span`, `llm_span` | one of `llm`, `embedding`, `tool`, `agent`, `chain`, `retriever`, `reranker`, `guardrail`, `evaluator`, `span`; default `chain`; `llm_span` writes `llm` |
| `llmobs.input` / `llmobs.output` | string | `observe`, `span` | § Serialization |
| `llmobs.input.format` / `llmobs.output.format` | string | same | `text` or `json` (never `messages` from the SDK) |
| `llmobs.session.id`, `llmobs.user.id` | string | context processor | from `context()`, else from baggage |
| `llmobs.tags` | string array | context processor | merged tags; omitted when empty |
| `llmobs.metadata.<key>` | string, bool, int or double | context processor, `set_metadata` | a `str` stays a string; `bool` → bool; an integer within ±(2^53−1) → int; a finite float → double; anything else → compact JSON string from § Serialization |
| `llmobs.environment`, `llmobs.release` | string | context processor | from `init()`, when set |
| `gen_ai.*`, `llmobs.cost.total`, `llmobs.ttft_ns` | see D-F-13 | `llm_span` | § LLM span |

**Programming errors raise.** An invalid `kind` given to `observe`/`span`, or an invalid `provider`/`model` given to `llm_span`, raises `ValueError` (Python) / `TypeError` (TS) when the API is called (decoration time for `observe`), before the user function runs. These are the only cases where the SDK raises after `init()`. Re-entering an already entered Python `context` object is a no-op logged at debug level.

**Errors.** When the user function (or the body of `span`/`llm_span`) raises, the SDK adds one event itself (it does not call OTel's `record_exception`/`recordException`, whose `exception.type` differs between languages): name `exception`, attributes `exception.type` (Python `type(e).__name__`; TS `e.name` for an `Error`, else `"Error"`), `exception.message` (Python `str(e)`; TS `e.message` for an `Error`, else `String(e)`), `exception.stacktrace` (Python `"".join(traceback.format_exception(e))`; TS `e.stack ?? ""`). The span status becomes error with message `"<exception.type>: <exception.message>"`. The exception is re-raised unchanged. A `GeneratorExit` (Python) or an early `return()` (TS) is not an error. A Python `BaseException` that is not an `Exception` (`KeyboardInterrupt`, `asyncio.CancelledError`, …) ends the span with the status unset, records no output, and is re-raised — through `observe` (functions, coroutine functions and both generator wrappers) exactly as inside `span()`/`llm_span()`. TypeScript has no equivalent class of exceptions: every throw, an aborted call included, goes through the error rule above.

**Status.** The SDKs leave the status unset on success. The one exception is a generator closed early (§ Python API), which ends with status ok.

`metadata` keys are strings; a key that is empty or contains a character outside `[A-Za-z0-9_.-]` is skipped and logged once at debug level (key length only, never the key).

### Serialization

One function per SDK, `serialize(value) -> (text, format)`, used for every payload and for non-scalar metadata values. Masking (when configured) has already run on the value.

1. A `str` (TS `string`) at the top level → `(value, "text")`. Everything else → compact JSON (no spaces, non-ASCII kept as is) with `"json"`.
2. Inside JSON, by type (Python | TypeScript):
   - `None` → `null` | `null`; `undefined` in an object property is omitted, in an array becomes `null`, at the top level becomes `null`.
   - `bool` → bool | `boolean` → bool.
   - `int` → number | `number` integral → number; `bigint` → decimal string.
   - `float`: finite → number; `NaN`, `inf`, `-inf` → the strings `"NaN"`, `"Infinity"`, `"-Infinity"` | same for `number`.
   - `str` → string.
   - `bytes`, `bytearray`, `memoryview` → `"data:application/octet-stream;base64,<standard base64>"` | `Uint8Array` (any `ArrayBufferView` with `BYTES_PER_ELEMENT === 1`), `ArrayBuffer` → same.
   - `datetime`, `date`, `time` → `isoformat()` | `Date` → `toISOString()`; an invalid `Date` → `null`.
   - `uuid.UUID`, `decimal.Decimal`, `pathlib.PurePath` → `str(value)` | —.
   - `enum.Enum` → its `.value`, serialized recursively | —.
   - dataclass instance → object of its fields in declaration order (`dataclasses.fields`, not `asdict`) | —.
   - object with a callable `model_dump` (pydantic v2) → `model_dump(mode="json")`, then serialized recursively; if it raises, fall through to the last rule | object with a callable `toJSON` → `toJSON()`, serialized recursively.
   - `Mapping` → object; keys converted with `str(key)` | `Map` → object, keys with `String(key)`; any other object (plain or class instance, after the rows above) → own enumerable string keys in insertion order.
   - `list`, `tuple` → array; `set`, `frozenset` → array in iteration order | `Array` → array; `Set` → array; a typed array that is not a byte array (`ArrayBuffer.isView(v)` and `BYTES_PER_ELEMENT > 1`, e.g. `Int16Array`, `Float64Array`) → array of its elements (`bigint` elements → decimal strings); a `DataView` → the byte data URI of its bytes.
   - numpy scalar (type's module is `numpy` and it has `item`) → `value.item()` serialized; numpy array (module `numpy`, has `tolist`) → `value.tolist()` serialized | —.
   - `BaseException` → `{"type": <class name>, "message": str(e)}` | `Error` → `{"type": e.name, "message": e.message}`.
   - functions, classes, modules, generators, coroutines → the last rule (`repr`) | `function` and `symbol`: as an object property → omitted; as an array item → `null`; at the top level → `String(value)` (JSON.stringify semantics).
   - Anything else → `repr(value)` | `String(value)`; if that raises → `"<unserializable TYPE>"` with the type name.
3. **Depth:** the top-level container is at depth 1; a container that would be at depth 33 is replaced by the string `"[llmobs: max depth]"`. Containers are: Python `dict`/`Mapping`, `list`, `tuple`, `set`, `frozenset`, dataclass instances, `model_dump` results; TypeScript objects, arrays, `Map`, `Set` and non-byte typed arrays. Scalars never count.
4. **Cycles:** a container that is already on the current path is replaced by `"[llmobs: cycle]"` (shared, non-cyclic references are serialized each time).
5. **Lone surrogates:** Python first replaces unpaired surrogate code points in every string with U+FFFD (`s.encode("utf-16", "surrogatepass").decode("utf-16", "replace")`), which is what TypeScript's `TextEncoder` does.
6. **Truncation:** after encoding, if the UTF-8 length of the text exceeds 1 048 576 bytes, cut it at the largest character boundary ≤ 1 048 576 bytes and append `…[llmobs: truncated, original N bytes]` (N = the original UTF-8 length, decimal); the format becomes `text`.
7. `serialize` never raises. If an unexpected error escapes the rules above, the result is `("<unserializable TYPE>", "text")`. A conversion failure replaces the failing *value* in place with the `"<unserializable TYPE>"` JSON string — the last row of rule 2 at any depth, and likewise a failing accessor deeper inside a container — so only a failure of the top-level value itself yields the rule-7 tuple.

### LLM span

`llm_span` / `withLlmSpan` options and setters, and what each writes:

| Option / setter | Attribute | Notes |
|---|---|---|
| (always) | `llmobs.span.kind = "llm"`, `gen_ai.operation.name = "chat"` | |
| `provider` (required, non-empty string) | `gen_ai.provider.name` | written as given |
| `model` (required, non-empty string) | `gen_ai.request.model` | |
| `params` (mapping of string → scalar) | `gen_ai.request.<key>` | only `str`, `bool`, `int`, finite `float` values; others skipped |
| `input_messages` / `inputMessages`, `set_input_messages` | `gen_ai.input.messages` | messages JSON (below) |
| `system_instructions` / `systemInstructions` (string or list of parts) | `gen_ai.system_instructions` | a string becomes `[{"type":"text","content":s}]`, then JSON |
| `set_output_messages` / `setOutputMessages` | `gen_ai.output.messages` | messages JSON |
| `set_response_model(m)` | `gen_ai.response.model` | |
| `set_usage(input_tokens=, output_tokens=, cache_read_tokens=, cache_write_tokens=, reasoning_tokens=)` | `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens`, `gen_ai.usage.cache_read.input_tokens`, `gen_ai.usage.cache_creation.input_tokens`, `gen_ai.usage.reasoning.output_tokens` | ints ≥ 0 only; others skipped; `None` skipped. `input_tokens` includes cache tokens (GenAI semantics) |
| `set_cost(total_usd)` | `llmobs.cost.total` | finite float ≥ 0 |
| `record_first_token()` / `recordFirstToken()` | `llmobs.ttft_ns` | now − span start, ns, int; only the first call counts |
| `set_metadata(**kv)` | `llmobs.metadata.<k>` | as above |

**Messages JSON.** The list is masked, then each message is converted:
- an object with a `parts` key → kept as is;
- an object with `role` and a string `content` → `{"role": role, "parts": [{"type": "text", "content": content}]}`, keeping `name` and `finish_reason` when present (in that order after `role`; `parts` last);
- anything else → kept as is (the server records a conversion error, spec 02).
Then the list goes through § Serialization (always JSON, so `json` format is irrelevant here; the attribute is the JSON text). With `capture_content=false`, none of `gen_ai.input.messages`, `gen_ai.output.messages`, `gen_ai.system_instructions` is written.

**Context managers and `withSpan`.** The value returned by the body of `span`/`llm_span`/`withSpan`/`withLlmSpan` is never recorded; only `set_output`/`setOutput` writes the output.

### Payload keys (export-time masking and `capture_content=false`)

Exact keys: `llmobs.input`, `llmobs.output`, `gen_ai.input.messages`, `gen_ai.output.messages`, `gen_ai.system_instructions`, `gen_ai.tool.call.arguments`, `gen_ai.tool.call.result`, `input.value`, `output.value`, `traceloop.entity.input`, `traceloop.entity.output`, `ai.prompt`, `ai.prompt.messages`, `ai.response.text`, `ai.response.object`, `ai.response.toolCalls`, `ai.toolCall.args`, `ai.toolCall.result`, `langfuse.observation.input`, `langfuse.observation.output`, `langfuse.trace.input`, `langfuse.trace.output`, `lmnr.span.input`, `lmnr.span.output`.

Prefixes (any key that starts with one): `llm.input_messages.`, `llm.output_messages.`, `gen_ai.prompt.`, `gen_ai.completion.`.

Only string values are masked; a non-string value under a payload key is deleted when masking is on (and when `capture_content=false`). Span attributes and event attributes are both processed. Spans whose scope name is `llmobs` are skipped (already masked at capture). With neither `mask` nor `capture_content=false`, the pass is not installed.

### Masking and `capture_content`

`mask` is applied at capture time to every value our SDK records (inputs, outputs, metadata values, LLM messages), before serialization. At export, an extra pass rewrites spans whose scope is **not** `llmobs`: for each attribute (span and event attributes) whose key is in the payload key list (§ Payload keys), `mask` gets the decoded value (JSON-parsed when the string is a JSON object or array, else the string) and the result is serialized back. With `capture_content=false`, our spans record no payload attributes and the export pass deletes those keys from other spans; the OpenInference instrumentors we create get their hide-inputs/hide-outputs options too. If `mask` raises, the value becomes `"[llmobs: mask failed]"` (fail closed) and one debug line is logged without the value (D-F-6).

### Language differences

Known, accepted differences that the fixtures do not cover. The SDK READMEs list them.
- **Zero-argument functions and `None`/`undefined`** (§ Testing).
- **Integral floats.** JavaScript has one number type, and OTel JS encodes an integral number as `intValue`. So Python `params={"temperature": 1.0}`, `set_cost(1.0)` or metadata `2.0` give a double, TypeScript gives an int. Python always converts `set_cost`'s argument with `float()`. The server reads both (spec 02 rule 20).
- **Large integers.** Python ints beyond ±(2^53−1) inside JSON payloads are written as JSON numbers; TypeScript `bigint` becomes a decimal string. As attributes (metadata) both write a JSON string.
- **Resource attributes:** see § Names.
- **Exporter environment.** Python's `OTLPSpanExporter` also merges `OTEL_EXPORTER_OTLP_HEADERS` / `OTEL_EXPORTER_OTLP_TRACES_HEADERS` from the environment into its request headers, so headers meant for another OTLP backend are sent to llmobs too; TypeScript sends only ours. The Python README warns about it.
- **Abandoned generators.** A half-consumed Python sync generator that is garbage-collected ends its span with status ok; TypeScript (no finalizer) and Python async generators leave it unexported.

## Python API (`pip install llmobs`; Python ≥ 3.11; extras `llmobs[openai]`, `[anthropic]`, `[langchain]`, `[llama-index]`, `[litellm]`, `[google-genai]`, `[bedrock]`, `[openai-agents]`, `[crewai]`, `[dspy]`, `[all]`)

```python
import llmobs

llmobs.init(
    api_key="sk-llmobs-…", base_url="https://llmobs.acme.com",
    environment="production", release="1.4.2",
    instrument=["openai", "anthropic", "langchain"],
)

# Decorator — sync, async, generators and async generators supported
@llmobs.observe(kind="tool", name="search_docs")          # kind default "chain"; name default = qualified fn name
def search_docs(query: str) -> list[str]: ...

# Context manager for manual spans
with llmobs.span("rerank", kind="reranker", input=docs) as s:
    ranked = rerank(docs)
    s.set_output(ranked)
    s.set_metadata(model="bge-reranker")

# Manual LLM span (when no instrumentor exists)
with llmobs.llm_span("call", provider="acme", model="acme-1", params={"temperature": 0.2},
                     input_messages=[{"role": "user", "content": "hi"}]) as s:
    resp = client.complete(...)
    s.set_output_messages([{"role": "assistant", "content": resp.text}])
    s.set_usage(input_tokens=12, output_tokens=40)

# Context: applied to every span started inside (contextvars), including instrumentor spans.
# propagate=True also sends session/user to downstream services in W3C baggage.
with llmobs.context(session_id="chat-42", user_id="u-17", tags=["beta"], metadata={"plan": "pro"}):
    agent.run(...)

# Scores — each call mints a UUIDv7 and returns it; retries resend the same id
trace_id = llmobs.current_trace_id()
score_id = llmobs.score(trace_id=trace_id, name="user_feedback", value=True, reason="thumbs up")
score_id = llmobs.score(span_id=llmobs.current_span_id(), trace_id=trace_id, name="accuracy", value=0.8)

llmobs.flush()      # blocks until spans and scores are exported (shutdown is registered atexit)
llmobs.shutdown()
```

Implementation notes:
- `observe` records the bound arguments with defaults applied, the first parameter dropped when it is named `self` or `cls`, `*args` as a list and `**kwargs` as an object under their parameter names; `capture_input` / `capture_output` default to true. A call whose arguments cannot be bound records the fallback input `{"args": […], "kwargs": {…}}` and the call's own error on one span — for a generator function too, where Python raises at call time, before any generator exists (TypeScript checks nothing at call time; the body raises at the first step instead). Generator functions are detected when `observe` wraps the function (`inspect.isgeneratorfunction` / `inspect.isasyncgenfunction`); any other function is traced as a call, even when it returns a generator, an iterator, a stream or an awaitable: that call's output is not recorded when the (awaited) return value is a generator or async generator object, and the returned object is passed through untouched, never consumed (D-F-11).
- Generators and async generators: the span starts when the generator body starts (first `next()` / `__anext__()`), is the current span only while the body runs (the OTel context is attached and detached around each step), and ends when the generator is exhausted, closed or raises. The output is the yielded items: one string when every item is a `str`, otherwise a JSON list; `observe(reduce_output=fn)` records `fn(items)` instead. Closing early (`break`, `close()`, `aclose()`) ends the span with status `ok` and the items yielded so far; an exception sets `error`. Items are kept only while `capture_content` is true, and the 1 MiB truncation applies to the recorded output.
- `context()` stores session id, user id, tags and metadata under one key of the OTel `Context`, so they follow `contextvars` and `copy_context()`; a span processor stamps them in `on_start` on every span of our provider, instrumentor spans included. `context(..., propagate=True)` also writes `session_id` and `user_id` to OTel **baggage** as `llmobs.session.id` and `llmobs.user.id`, so they cross service boundaries when the app uses W3C propagation. It is opt-in because baggage is sent as an HTTP header on every outgoing request, third-party APIs included (D80). Tags and metadata never go into baggage. The `on_start` hook reads the `Context` first, then those two baggage keys, so a downstream service running the llmobs SDK picks them up without calling `context()`; a service on plain OTel adds OTel's baggage span processor (the keys are the attribute names the server reads, 02).
- Scores are buffered (≤ 10 000) and sent in batches (≤ 1000, every 1 s) to `POST /api/v1/scores` on a background thread with client-generated UUIDv7 ids (D-F-1, D-F-2).

### Threads

The SDK does not patch `threading` or executors. The README documents `contextvars.copy_context().run(fn)` and OTel's threading instrumentor (D-F-14).

### Instrumentors

`instrument` names, the extra that installs them, and the class `init` creates (`<Class>().instrument(tracer_provider=<ours>, config=TraceConfig(...))`):

| Name | Extra | Module | Class | Distribution (min version) |
|---|---|---|---|---|
| `openai` | `llmobs[openai]` | `openinference.instrumentation.openai` | `OpenAIInstrumentor` | `openinference-instrumentation-openai>=0.1.69` |
| `anthropic` | `llmobs[anthropic]` | `openinference.instrumentation.anthropic` | `AnthropicInstrumentor` | `openinference-instrumentation-anthropic>=3.3.0` |
| `langchain` | `llmobs[langchain]` | `openinference.instrumentation.langchain` | `LangChainInstrumentor` | `openinference-instrumentation-langchain>=0.1.82` |
| `llama-index` | `llmobs[llama-index]` | `openinference.instrumentation.llama_index` | `LlamaIndexInstrumentor` | `openinference-instrumentation-llama-index>=4.7.1` |
| `litellm` | `llmobs[litellm]` | `openinference.instrumentation.litellm` | `LiteLLMInstrumentor` | `openinference-instrumentation-litellm>=0.1.53` |
| `google-genai` | `llmobs[google-genai]` | `openinference.instrumentation.google_genai` | `GoogleGenAIInstrumentor` | `openinference-instrumentation-google-genai>=1.5.1` |
| `bedrock` | `llmobs[bedrock]` | `openinference.instrumentation.bedrock` | `BedrockInstrumentor` | `openinference-instrumentation-bedrock>=0.1.63` |
| `openai-agents` | `llmobs[openai-agents]` | `openinference.instrumentation.openai_agents` | `OpenAIAgentsInstrumentor` | `openinference-instrumentation-openai-agents>=2.7.1` |
| `crewai` | `llmobs[crewai]` | `openinference.instrumentation.crewai` | `CrewAIInstrumentor` | `openinference-instrumentation-crewai>=1.1.23` |
| `dspy` | `llmobs[dspy]` | `openinference.instrumentation.dspy` | `DSPyInstrumentor` | `openinference-instrumentation-dspy>=0.1.50` |

`llmobs[all]` installs every one.

### Datasets and experiments

```python
client = llmobs.Client()            # REST client, same config

ds = client.datasets.create("faq-v1", description="FAQ questions")
ds.add_items([{"input": {"q": "…"}, "expected_output": {"a": "…"}, "metadata": {"topic": "billing"}}])
ds = client.datasets.get("faq-v1", version=3)

def task(input: dict) -> str:       # receives item.input; may also accept (input, item)
    return my_app.answer(input["q"])

def exact(output, expected, **_) -> bool:            # scorer: any of (input, output, expected, metadata, trace_id)
    return output.strip() == expected["a"].strip()

def length_ok(output, **_) -> llmobs.Score:          # rich return
    return llmobs.Score(value=len(output) < 500, reason=f"{len(output)} chars")

result = llmobs.evaluate(
    dataset="faq-v1",
    task=task,
    scorers=[exact, length_ok],
    server_evaluators=["correctness"],   # evaluator names run server-side (06)
    experiment_name="gpt-5-mini prompt v3",
    metadata={"model": "gpt-5-mini", "prompt": "v3"},
    concurrency=8, repetitions=1,
)
print(result.summary())                   # aggregates per score; link to the UI
result.assert_min("exact", 0.8)           # raises for CI gating
```

`evaluate` follows the protocol in 06 §Experiments; async tasks are supported (`await llmobs.aevaluate(...)`). Task exceptions are recorded per item and do not abort the run.

## TypeScript API (`npm i llmobs`; ESM only; Node ≥ 22.12, Bun, Deno; edge runtimes via the fetch-based exporter)

```ts
import * as llmobs from "llmobs";
import { openai } from "llmobs/openai";
import OpenAI from "openai";

llmobs.init({
  apiKey: process.env.LLMOBS_API_KEY,
  baseUrl: "https://llmobs.acme.com",
  environment: "production",
  instrument: [openai(OpenAI)],   // instrumentation objects (optional peer deps);
  // useGlobalProvider: true,     // ours becomes the global provider; llmobs.spanProcessor() exposes our processor
});

const searchDocs = llmobs.observe(async (query: string) => { … }, { kind: "tool", name: "search_docs" });

await llmobs.withSpan("rerank", { kind: "reranker", input: docs }, async (span) => {
  const ranked = await rerank(docs);
  span.setOutput(ranked);
  return ranked;
});

// Manual LLM span (when no instrumentor exists)
await llmobs.withLlmSpan("call", {
  provider: "acme", model: "acme-1", params: { temperature: 0.2 },
  inputMessages: [{ role: "user", content: "hi" }],
}, (span) => {
  const resp = client.complete(...);
  span.setOutputMessages([{ role: "assistant", content: resp.text }]);
  span.setUsage({ inputTokens: 12, outputTokens: 40 });
});

await llmobs.withContext({ sessionId: "chat-42", userId: "u-17", tags: ["beta"] }, async () => {
  await agent.run();
});

const scoreId = llmobs.score({ traceId: llmobs.currentTraceId(), name: "user_feedback", value: true });
await llmobs.flush();
```

- Context propagation uses `AsyncLocalStorage` (Node) via the OTel context manager. The SDK's context manager uses the `async_hooks` and `events` modules; Cloudflare Workers need the `nodejs_compat` flag.
- `withContext({ ..., propagate: true }, fn)` writes baggage with the same rules as Python. `observe` records no input when the function is called with no arguments, the single argument when called with one, and a JSON array of them when called with more (`captureInput` / `captureOutput` default to true). Generator functions are detected when `observe` wraps the function (`Object.prototype.toString.call(fn)` is `"[object GeneratorFunction]"` or `"[object AsyncGeneratorFunction]"`) and follow the Python generator rules (`reduceOutput` option); any other function is traced as a call, even when it returns a generator, an async generator, an iterator, a `ReadableStream` or a promise of one: the call's output is not recorded when the awaited return value is a generator or async generator object, and the returned object is passed through untouched, never consumed (D-F-11).
- **Vercel AI SDK:** no instrumentor needed; the server normalizes `ai.*` attributes (02). The AI SDK sends spans to the global tracer unless it is given one, so `llmobs.aiTelemetry({ sessionId, userId, tags })` returns `{ isEnabled: true, tracer, metadata }` with a tracer from the llmobs provider, and users pass it as `experimental_telemetry`. With `useGlobalProvider: true`, a plain `experimental_telemetry: { isEnabled: true, metadata: { sessionId, userId } }` works too.
- Transport: one fetch-based exporter on every runtime (D-F-8).
- Browser bundle (`llmobs/browser`): only `feedback({ baseUrl, publicKey, traceId, spanId?, name?, value, comment? })` — no tracing. It posts spec 06's end-user feedback body to `POST {baseUrl}/api/public/feedback` with `Authorization: Bearer <pk-llmobs-…>`; it imports nothing from OTel and resolves to `{ ok: boolean }`, never throwing (D-F-16).
- Datasets/experiments: `new llmobs.Client()`, `client.datasets.*`, `llmobs.evaluate({ dataset, task, scorers, serverEvaluators, experimentName, concurrency })` with the same semantics as Python.

## Raw OpenTelemetry (no SDK)

Documented recipe for any language:

```bash
OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=https://llmobs.acme.com/v1/traces
OTEL_EXPORTER_OTLP_TRACES_HEADERS="Authorization=Bearer sk-llmobs-…"
OTEL_EXPORTER_OTLP_TRACES_PROTOCOL=http/protobuf
```

plus any GenAI / OpenInference / OpenLLMetry instrumentation, and the attribute reference for `session.id`, `user.id`, `llmobs.tags`, `llmobs.metadata.*`, `llmobs.environment`.

## Testing

- Unit tests per SDK, against local mock HTTP servers.
- **Conformance fixtures (D-F-9).** Each SDK scenario has a case `fixtures/otlp/llmobs/sdk-<scenario>/` whose `input.json` is the canonical OTLP both SDKs must emit and whose `expected.json` is the normalized result. Both SDK test suites run the scenario and compare their canonicalized export with `input.json`; the Rust golden test normalizes the same `input.json` (02 § Testing).
- An integration suite against a real `llmobs serve` is added once the ingest and score routes exist.

### Canonical form

`fixtures/otlp/llmobs/sdk-<scenario>/input.json` holds the **canonical OTLP** of one scenario. A test converts what the SDK exported into canonical form and compares it, as parsed JSON, with `input.json`.

Canonical form of an `ExportTraceServiceRequest`:
1. **Ids.** Spans are ordered by creation: the tests use a sequential id generator, so sorting by the original span id gives creation order. Trace ids and span ids are then **remapped by rank of first appearance** in that order: the k-th distinct trace id becomes `"%032x" % k`, the k-th span becomes `"%016x" % k` (k from 1). `parentSpanId` uses the same mapping and is omitted for roots (and when the parent is not among the exported spans).
2. **Times.** Span `n` (1-based creation index) gets `startTimeUnixNano = 1767225590000000000 + n·1 000 000` and `endTimeUnixNano = 1767225590000000000 + (1000 − n)·1 000 000`, as decimal strings (2025-12-31T23:59:50Z plus milliseconds). Every event of span `n` gets `timeUnixNano` = that span's end.
3. **Shape.** The whole document is exactly `{"resourceSpans": [{"resource": {"attributes": [...]}, "scopeSpans": [{"scope": {"name": "llmobs", "version": "0.0.0"}, "spans": [...]}]}]}` with nothing else at any level (no `schemaUrl`, no dropped counts). `resource.attributes` keep only `service.name` (the tests set `OTEL_SERVICE_NAME=conformance`) and `telemetry.distro.name`; the scope version is replaced by `"0.0.0"`.
4. **Span fields kept:** `traceId`, `spanId`, `parentSpanId` (non-roots), `name`, `kind` (integer; our spans are `1`, INTERNAL), `startTimeUnixNano`, `endTimeUnixNano`, `attributes`, `events` (omitted when empty), `status` (dropped when `code` is absent or 0; `message` dropped when empty or absent; so `{"code": 1}` ok, `{"code": 2, "message": …}` error). Everything else (`flags`, `traceState`, dropped counts, `links` when empty, `schemaUrl`) is removed.
5. **Attributes** (span, event, resource) are sorted by key. `AnyValue`s use the OTLP/JSON encoding: `stringValue`, `boolValue`, `intValue` as a decimal **string** (convert numbers), `doubleValue` as a number, `arrayValue: {"values": [...]}`.
6. **Exception events:** name `exception`; attributes reduced to `exception.message` and `exception.type`. The scenarios raise an error class named `ScenarioError` in both languages.
7. **Spans** are listed in creation order.

The canonicalizer is test code in each SDK; it is described here so both produce the same thing.

### Scenarios

Every scenario runs with `api_key="sk-llmobs-test"`, `base_url="http://127.0.0.1:9"` and no environment/release unless the row says otherwise, through the SDK's real pipeline with the inner OTLP exporter replaced by an in-memory one, then `flush()`. The TypeScript calls are spelled out under the table.

| Case `sdk-…` | Calls | Spans (creation order) |
|---|---|---|
| `observe-tool` | `@observe(kind="tool", name="search_docs") def search_docs(query: str, limit: int = 2) -> list[str]` returning `["doc-1", "doc-2"]`, called `search_docs("pricing")`. TS: `observe(({ query, limit = 2 }) => …, { kind: "tool", name: "search_docs" })` called with `{ query: "pricing", limit: 2 }`. | 1 `search_docs`: kind `tool`; input `{"query":"pricing","limit":2}` json; output `["doc-1","doc-2"]` json; status unset |
| `observe-nested` | `@observe(name="answer") def answer(question)` calls `@observe(kind="retriever", name="retrieve") def retrieve(question)` returning `[{"id": "d1"}]`, then returns `"It is 42."`; called with `"why?"` | 1 `answer`: kind `chain`, input `{"question":"why?"}` json, output `It is 42.` text. 2 `retrieve` (parent 1): kind `retriever`, input `{"question":"why?"}` json, output `[{"id":"d1"}]` json |
| `observe-error` | `@observe(name="fails") def fails(x)` raises `ScenarioError("boom")`; called with `1` | 1 `fails`: kind `chain`, input `{"x":1}` json, no output, status `{"code":2,"message":"ScenarioError: boom"}`, one `exception` event (`exception.message = "boom"`, `exception.type = "ScenarioError"`) |
| `generator-text` | `@observe(name="stream") def stream(prompt)` yields `"Hel"`, `"lo"`; called with `"hi"` and fully consumed | 1 `stream`: input `{"prompt":"hi"}` json, output `Hello` text, status unset |
| `generator-json` | `@observe(name="numbers") def numbers()` yields `{"n": 1}`, `{"n": 2}`; fully consumed | 1 `numbers`: input `{}` json, output `[{"n":1},{"n":2}]` json |
| `span-manual` | `with span("rerank", kind="reranker", input=["a", "b"]) as s: s.set_output(["b", "a"]); s.set_metadata(model="bge")` | 1 `rerank`: kind `reranker`, input `["a","b"]` json, output `["b","a"]` json, `llmobs.metadata.model = "bge"` |
| `llm-span` | `with llm_span("chat", provider="openai", model="gpt-4o-mini", input_messages=[{"role": "system", "content": "Be brief."}, {"role": "user", "content": "hi"}], params={"temperature": 0.2}) as s: s.set_output_messages([{"role": "assistant", "content": "hello", "finish_reason": "stop"}]); s.set_response_model("gpt-4o-mini-2024-07-18"); s.set_usage(input_tokens=12, output_tokens=3)` | 1 `chat`: kind `llm`, `gen_ai.operation.name = "chat"`, `gen_ai.provider.name = "openai"`, `gen_ai.request.model`, `gen_ai.request.temperature = 0.2` (double), `gen_ai.response.model`, `gen_ai.input.messages`, `gen_ai.output.messages`, `gen_ai.usage.input_tokens = 12`, `gen_ai.usage.output_tokens = 3` |
| `context` | `init(…, environment="staging", release="1.2.3")`; `with context(session_id="chat-42", user_id="u-17", tags=["beta"], metadata={"plan": "pro", "seats": 3}):` call `@observe(name="turn") def turn()` which calls `@observe(name="lookup") def lookup()`; both return `None` | 1 `turn` and 2 `lookup` (parent 1), each with `llmobs.session.id`, `llmobs.user.id`, `llmobs.tags = ["beta"]`, `llmobs.metadata.plan = "pro"`, `llmobs.metadata.seats = 3` (int), `llmobs.environment`, `llmobs.release`; input `{}` json; output `null` json |
| `capture-off` | `init(…, capture_content=False)`; `@observe(kind="tool", name="lookup") def lookup(query)` returns `"x"`; called with `"q"` | 1 `lookup`: only `llmobs.span.kind = "tool"` |
| `mask` | `init(…, mask=redact)` where `redact` replaces every string equal to `"alice@example.com"` (recursively in dicts/lists) with `"[redacted]"`; `@observe(name="notify") def notify(email)` returns `"sent to alice@example.com"`; called with `"alice@example.com"` | 1 `notify`: input `{"email":"[redacted]"}` json, output `sent to alice@example.com` text (the mask replaces whole equal strings only) |
| `media-bytes` | `@observe(name="upload") def upload(data: bytes)` returns `"ok"`; called with the 70-byte PNG below | 1 `upload`: input `{"data":"data:application/octet-stream;base64,<b64>"}` json, output `ok` text. `case.json` sets `settings.media_min_bytes = 1` so the server extracts it (as `application/octet-stream`: the SDK declares that type and the server sniffs only when none is declared) |

The PNG (70 bytes, 1×1 pixel, SHA-256 `497790947d4666760ce38f3c00e852c71fdb66cae849bae8e9ede352719e1581`), base64: `iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==`.

**TypeScript calls.** Python records `{}` as the input of a zero-argument function and `null` as the output of a function returning `None`; TypeScript records no input for zero arguments and no output for `undefined`. The TypeScript scenarios therefore pass one object argument and return `null` where Python returns `None`, so both languages emit identical spans. Each TS test states this in a comment. The calls, with `observe` options matching the Python row:

| Case | TypeScript |
|---|---|
| `observe-tool` | `observe(({ query, limit = 2 }: { query: string; limit?: number }) => ["doc-1", "doc-2"], { kind: "tool", name: "search_docs" })({ query: "pricing", limit: 2 })` |
| `observe-nested` | `retrieve = observe(({ question }) => [{ id: "d1" }], { kind: "retriever", name: "retrieve" })`; `answer = observe(({ question }) => { retrieve({ question }); return "It is 42."; }, { name: "answer" })`; `answer({ question: "why?" })` |
| `observe-error` | `observe(({ x }) => { throw new ScenarioError("boom"); }, { name: "fails" })({ x: 1 })` inside `expect(...).toThrow` |
| `generator-text` | `observe(function* ({ prompt }) { yield "Hel"; yield "lo"; }, { name: "stream" })({ prompt: "hi" })`, consumed with `[...gen]` |
| `generator-json` | `observe(function* (_: Record<string, never>) { yield { n: 1 }; yield { n: 2 }; }, { name: "numbers" })({})`, consumed |
| `span-manual` | `withSpan("rerank", { kind: "reranker", input: ["a", "b"] }, (s) => { s.setOutput(["b", "a"]); s.setMetadata({ model: "bge" }); })` |
| `llm-span` | `withLlmSpan("chat", { provider: "openai", model: "gpt-4o-mini", inputMessages: [{ role: "system", content: "Be brief." }, { role: "user", content: "hi" }], params: { temperature: 0.2 } }, (s) => { s.setOutputMessages([{ role: "assistant", content: "hello", finish_reason: "stop" }]); s.setResponseModel("gpt-4o-mini-2024-07-18"); s.setUsage({ inputTokens: 12, outputTokens: 3 }); })` |
| `context` | `init` options `environment: "staging", release: "1.2.3"`; `lookup = observe((_: Record<string, never>) => null, { name: "lookup" })`; `turn = observe((_: Record<string, never>) => { lookup({}); return null; }, { name: "turn" })`; `withContext({ sessionId: "chat-42", userId: "u-17", tags: ["beta"], metadata: { plan: "pro", seats: 3 } }, () => turn({}))` |
| `capture-off` | `init` option `captureContent: false`; `observe(({ query }) => "x", { kind: "tool", name: "lookup" })({ query: "q" })` |
| `mask` | `init` option `mask: redact` where `redact` returns a deep copy with every string equal to `"alice@example.com"` replaced by `"[redacted]"` (arrays and plain objects recursed); `observe(({ email }) => "sent to alice@example.com", { name: "notify" })({ email: "alice@example.com" })` |
| `media-bytes` | `observe(({ data }: { data: Uint8Array }) => "ok", { name: "upload" })({ data: <the PNG bytes> })` |

Note for `observe-tool`: the TS input is the argument object as passed, so the test passes `limit: 2` explicitly (TS cannot see destructuring defaults).
