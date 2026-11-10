# 10 — SDKs (Python, TypeScript)

Both SDKs are thin layers over OpenTelemetry: tracing goes out as OTLP/HTTP (protobuf) to `/v1/traces`; everything else uses a small REST client generated from the OpenAPI spec. License: MIT (independent of the server license decision).

## Design rules

1. **Own `TracerProvider` by default.** `init()` creates a dedicated provider + `BatchSpanProcessor` + OTLP exporter and never registers it as the global provider, so we don't hijack the app's existing OTel setup. `init(use_global_provider=True)` instead adds our processor/exporter to the global provider (for apps already using OTel that want one pipeline). `llmobs.tracer_provider()` (TS `llmobs.tracerProvider()`) returns the provider in use (a no-op provider before `init()` or when disabled), for libraries that emit OTel spans themselves and accept a provider or tracer (pydantic-ai, the Vercel AI SDK, …). Spans such libraries send to the global provider reach llmobs only with `use_global_provider=True`.
2. **No custom instrumentation for frameworks.** Auto-instrumentation reuses OpenInference instrumentors (and OpenLLMetry where OpenInference lacks one), installed as optional extras and activated by `init(instrument=[…])`.
3. **Emit the standard.** Our spans use OTel GenAI attributes where a standard exists; `llmobs.*` only for what has no standard (kind, input/output for non-LLM spans, tags, metadata, environment, release, experiment linkage).
4. **Never break the app after `init()`.** `init()` raises `ConfigError` for an invalid configuration, a missing API key included (D80): a misconfigured deployment fails at startup, where it is noticed. Use `enabled=false` to run without llmobs. Decorators, context managers and `score()` used before `init()` or with `enabled=false` do nothing. After `init()` returns, nothing the SDK does raises into the app: exporter errors are logged at debug level, serialization failures fall back to `repr()`, and full buffers drop data (§ Lifecycle). The SDK has no required dependencies beyond `opentelemetry-sdk`, `opentelemetry-exporter-otlp-proto-http` and an HTTP client.
5. **Same concepts, idiomatic APIs** in both languages.

## Configuration

| Option | Env | Default |
|---|---|---|
| `api_key` | `LLMOBS_API_KEY` | required (`sk-…`) |
| `base_url` | `LLMOBS_BASE_URL` | `http://localhost:8080` |
| `environment` | `LLMOBS_ENVIRONMENT` | unset |
| `release` | `LLMOBS_RELEASE` | unset (Python: tries `GIT_COMMIT`/`SOURCE_VERSION` env) |
| `capture_content` | `LLMOBS_CAPTURE_CONTENT` | `true` (set `false` to drop inputs/outputs, keep metrics) |
| `sample_rate` | `LLMOBS_SAMPLE_RATE` | `1.0` (trace-ID ratio sampler) |
| `instrument` | — | `[]` |
| `mask` | — | optional callable applied to every input/output/metadata value before export |
| `enabled` | `LLMOBS_ENABLED` | `true` (no-op mode when false) |

TypeScript spells the options in camelCase (`apiKey`, `baseUrl`, `captureContent`, `sampleRate`); the env vars are the same.

**Resolution rules (D78).** Both SDKs resolve the configuration the same way:
1. An explicit argument wins over the env var, which wins over the default. An argument set to `None`/`undefined` counts as not given.
2. Env values are trimmed; an empty value counts as unset.
3. `enabled` is resolved first. When it is false, no other option is read or validated: the config holds the defaults and the SDK runs in no-op mode.
4. Booleans from env accept `true`/`false`, `1`/`0`, `yes`/`no`, `on`/`off`, case-insensitive.
5. `api_key` is required and must start with `sk-llmobs-`. It never appears in error messages, `repr`/`toString`/`inspect` output, or JSON serialization of the config.
6. `base_url` must be an absolute `http` or `https` URL without query or fragment. A path is allowed (reverse-proxy prefix); trailing slashes are removed. A bracketed host must contain only hex digits, `:` and `.`; brackets anywhere else are rejected.
7. `sample_rate` must be a finite number in `[0, 1]`. From `LLMOBS_SAMPLE_RATE` it must also be a plain ASCII decimal (`1`, `0.25`, `.5`, `1.`): no sign, exponent, digit separators, hex, or non-ASCII digits.
8. `environment` must not be the reserved evaluation environment (`llmobs-eval`, D49).
9. `release` falls back to `GIT_COMMIT`, then `SOURCE_VERSION` (Python only), when neither the argument nor `LLMOBS_RELEASE` is set.
10. `instrument` must be a list of non-empty strings (a bare string is rejected); names are checked when instrumentation is added. `mask` must be callable.
11. An invalid value raises a `ConfigError` naming the option or env var, never its value. Resolution stops at the first problem. A missing API key says how to fix it: `api_key: required; set LLMOBS_API_KEY, or LLMOBS_ENABLED=false to run without llmobs` (TS: `apiKey: …`).

## Lifecycle

- **Exit.** Python: `init()` registers an `atexit` hook that calls `shutdown()`. TypeScript: where `process` exists, `init()` registers one `process.once("beforeExit", …)` listener that calls `shutdown()`; calling `init()` again does not add a second one. `beforeExit` fires when the event loop empties, not on `process.exit()` or a signal, and the SDK installs no signal handlers: apps that exit that way, and serverless handlers, `await llmobs.flush()` (or `shutdown()`) themselves (D80).
- **`flush()`** blocks (Python) or resolves (TS) when pending spans and scores are exported or the export timeout expires. **`shutdown()`** flushes, then stops the exporters; it is idempotent, and spans and scores after it are dropped.
- **Buffers.** Spans go through the OTel `BatchSpanProcessor` with its default queue size; when it is full, new spans are dropped (OTel behaviour). Scores are buffered up to 10 000; when full, new scores are dropped. Dropped counts are logged at warning level at most once per minute, with counts only.
- **Identification.** Spans created by the SDK use the instrumentation scope name `llmobs` and the SDK version as scope version (instrumentor spans keep their own scope). The provider's resource carries `telemetry.distro.name = "llmobs"` and `telemetry.distro.version = <SDK version>` (OTel semantic conventions), so the server can tell SDK versions apart. Storing the version server-side is not in the MVP; `scope_name` is stored (01).

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
with llmobs.llm_span("call", provider="acme", model="acme-1",
                     input_messages=[{"role": "user", "content": "hi"}]) as s:
    resp = client.complete(...)
    s.set_output_messages([{"role": "assistant", "content": resp.text}])
    s.set_usage(input_tokens=12, output_tokens=40)

# Context: applied to every span started inside (contextvars), including instrumentor spans.
# propagate=True also sends session/user to downstream services in W3C baggage.
with llmobs.context(session_id="chat-42", user_id="u-17", tags=["beta"], metadata={"plan": "pro"}):
    agent.run(...)

# Scores
trace_id = llmobs.current_trace_id()
llmobs.score(trace_id=trace_id, name="user_feedback", value=True, reason="thumbs up")
llmobs.score(span_id=llmobs.current_span_id(), trace_id=trace_id, name="accuracy", value=0.8)

llmobs.flush()      # blocks until spans and scores are exported (shutdown is registered atexit)
llmobs.shutdown()
```

Implementation notes:
- `observe`/`span` record `llmobs.span.kind`, `llmobs.input` (JSON of bound arguments, `self`/`cls` excluded) and `llmobs.output`; exceptions set status `error` + an `exception` event, and are re-raised.
- Generators and async generators: the span starts when the generator body starts (first `next()` / `__anext__()`), is the current span only while the body runs (the OTel context is attached and detached around each step), and ends when the generator is exhausted, closed or raises. The output is the yielded items: one string when every item is a `str`, otherwise a JSON list; `observe(reduce_output=fn)` records `fn(items)` instead. Closing early (`break`, `close()`, `aclose()`) ends the span with status `ok` and the items yielded so far; an exception sets `error`. Items are kept only while `capture_content` is true, and the 1 MiB truncation applies to the recorded output.
- `context()` is implemented as a `SpanProcessor.on_start` hook reading a `ContextVar`, so instrumentor spans get the attributes too. `context(..., propagate=True)` also writes `session_id` and `user_id` to OTel **baggage** as `llmobs.session.id` and `llmobs.user.id`, so they cross service boundaries when the app uses W3C propagation. It is opt-in because baggage is sent as an HTTP header on every outgoing request, third-party APIs included (D80). Tags and metadata never go into baggage. The `on_start` hook reads the `ContextVar` first, then those two baggage keys, so a downstream service running the llmobs SDK picks them up without calling `context()`; a service on plain OTel adds OTel's baggage span processor (the keys are the attribute names the server reads, 02).
- Values are serialized with a tolerant JSON encoder (dataclasses, pydantic models, numpy scalars, datetimes, bytes → base64 data URI so the server extracts media); strings > 1 MiB are truncated client-side with the same marker as the server.
- Scores are buffered and sent in batches (≤ 1000, every 1 s) via `POST /scores` on a background thread.

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

llmobs.init({
  apiKey: process.env.LLMOBS_API_KEY,
  baseUrl: "https://llmobs.acme.com",
  environment: "production",
  instrument: ["openai", "anthropic"],     // OpenInference JS instrumentors (optional peer deps)
});

const searchDocs = llmobs.observe(async (query: string) => { … }, { kind: "tool", name: "search_docs" });

await llmobs.withSpan("rerank", { kind: "reranker", input: docs }, async (span) => {
  const ranked = await rerank(docs);
  span.setOutput(ranked);
  return ranked;
});

await llmobs.withContext({ sessionId: "chat-42", userId: "u-17", tags: ["beta"] }, async () => {
  await agent.run();
});

llmobs.score({ traceId: llmobs.currentTraceId(), name: "user_feedback", value: true });
await llmobs.flush();
```

- Context propagation uses `AsyncLocalStorage` (Node) via the OTel context manager.
- `withContext({ ..., propagate: true }, fn)` writes baggage with the same rules as Python. `observe` applies the Python generator rules when the wrapped function returns a generator or async generator (`reduceOutput` option).
- **Vercel AI SDK:** no instrumentor needed; the server normalizes `ai.*` attributes (02). The AI SDK sends spans to the global tracer unless it is given one, so `llmobs.aiTelemetry({ sessionId, userId, tags })` returns `{ isEnabled: true, tracer, metadata }` with a tracer from the llmobs provider, and users pass it as `experimental_telemetry`. With `useGlobalProvider: true`, a plain `experimental_telemetry: { isEnabled: true, metadata: { sessionId, userId } }` works too.
- Browser bundle (`llmobs/browser`): only `feedback({ publicKey, traceId, value, comment })` — no tracing.
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

- Unit tests per SDK; an integration suite runs each SDK against a real `llmobs serve` (started in CI from the built binary with a temp data dir) and asserts the normalized spans via the REST API — the same fixtures feed the server's golden tests (02 §Testing).
