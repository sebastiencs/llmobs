# 10 — SDKs (Python, TypeScript)

Both SDKs are thin layers over OpenTelemetry: tracing goes out as OTLP/HTTP (protobuf) to `/v1/traces`; everything else uses a small REST client generated from the OpenAPI spec. License: MIT (independent of the server license decision).

## Design rules

1. **Own `TracerProvider` by default.** `init()` creates a dedicated provider + `BatchSpanProcessor` + OTLP exporter, so we don't hijack the app's existing OTel setup. `init(use_global_provider=True)` instead adds our processor/exporter to the global provider (for apps already using OTel that want one pipeline).
2. **No custom instrumentation for frameworks.** Auto-instrumentation reuses OpenInference instrumentors (and OpenLLMetry where OpenInference lacks one), installed as optional extras and activated by `init(instrument=[…])`.
3. **Emit the standard.** Our spans use OTel GenAI attributes where a standard exists; `llmobs.*` only for what has no standard (kind, input/output for non-LLM spans, tags, metadata, environment, release, experiment linkage).
4. **Never break the app.** Exporter errors are logged at debug level, never raised; serialization failures fall back to `repr()`; the SDK has no required dependencies beyond `opentelemetry-sdk`, `opentelemetry-exporter-otlp-proto-http` and an HTTP client.
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

## Python API (`pip install llmobs`, extras `llmobs[openai]`, `[anthropic]`, `[langchain]`, `[llama-index]`, `[litellm]`, `[google-genai]`, `[bedrock]`, `[openai-agents]`, `[crewai]`, `[dspy]`, `[all]`)

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

# Context: applied to every span started inside (contextvars), including instrumentor spans
with llmobs.context(session_id="chat-42", user_id="u-17", tags=["beta"], metadata={"plan": "pro"}):
    agent.run(...)

# Scores
trace_id = llmobs.current_trace_id()
llmobs.score(trace_id=trace_id, name="user_feedback", value=True, reason="thumbs up")
llmobs.score(span_id=llmobs.current_span_id(), trace_id=trace_id, name="accuracy", value=0.8)

llmobs.flush()      # blocks until spans and scores are exported (also registered atexit)
llmobs.shutdown()
```

Implementation notes:
- `observe`/`span` record `llmobs.span.kind`, `llmobs.input` (JSON of bound arguments, `self`/`cls` excluded) and `llmobs.output`; exceptions set status `error` + an `exception` event, and are re-raised.
- `context()` is implemented as a `SpanProcessor.on_start` hook reading a `ContextVar`, so instrumentor spans get the attributes too. Values are also stored in OTel **baggage** (`llmobs.session.id`, `llmobs.user.id`) so they propagate across services when the app uses W3C propagation.
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

## TypeScript API (`npm i llmobs`; Node ≥ 18, Bun, Deno; edge runtimes via the fetch-based exporter)

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
- **Vercel AI SDK:** no instrumentor needed. `llmobs.init()` registers the provider; users pass `experimental_telemetry: { isEnabled: true, metadata: { sessionId, userId } }` — the server normalizes `ai.*` attributes (02). A helper `llmobs.aiTelemetry({ sessionId, userId, tags })` builds that object.
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
