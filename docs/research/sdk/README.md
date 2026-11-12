# SDK research (track F)

Notes on how other LLM-observability SDKs build tracing, read from their source in November 2026 to decide spec 10's open points. Code was read to compare designs; none was copied.

| File | Covers |
|---|---|
| [langfuse-laminar.md](langfuse-laminar.md) | Langfuse Python 5.1 and Laminar (`lmnr` 0.8.8) Python SDKs; TS behaviour from their docs |
| [phoenix-openinference.md](phoenix-openinference.md) | Phoenix `register()` (Python and TS), OpenInference instrumentation core, annotations |
| [opik-langwatch-mlflow.md](opik-langwatch-mlflow.md) | Opik (non-OTel), LangWatch and MLflow tracing, Python and TS |

## What we took, and where it landed (decisions D-F-1–D-F-16, `docs/plans/f-sdk/`)

| Finding | Decision |
|---|---|
| Everyone sends scores/feedback over REST, not OTLP; most generate ids client-side; Phoenix queues them server-side until the span exists | D-F-1, D-F-2 |
| Langfuse filters exported spans to LLM-related ones (own scope, known LLM scopes, `gen_ai.*`) after noise complaints; filtering creates detached roots | D-F-5 |
| Langfuse/Laminar/LangWatch attach to or replace the global provider in various ways; Phoenix TS `global: true` stomps on the app's setup; MLflow isolates and loses nesting with plain-OTel libraries | D-F-4 (own provider, shared global context) |
| OpenInference JS needs `manuallyInstrument(module)` under ESM; Phoenix/LangWatch take instrumentor objects; Laminar ships forked instrumentations | D-F-7 |
| Langfuse v5 added export-time masking of all spans because capture-time masking misses instrumentor payloads | D-F-6 |
| Opik's generator handling (lazy start, current only while running, ends once) is the best; MLflow lacks `try/finally`; LangWatch leaks the span context into the consumer | spec 10 generator rules, D-F-11 |
| Nobody bounds serializer depth; only Opik detects cycles; Laminar truncates at 10 MB | D-F-10 |
| Context values applied in a span processor reach third-party spans (Langfuse `propagate_attributes`); OpenInference's `using_*` only reach its own tracer | D-F-12 |
| Every competitor warns and no-ops on a missing key | D-F-3 |
| Laminar monkey-patches `threading`; LangWatch's thread fallback bleeds between requests | D-F-14 |
| OpenInference raises the attribute count limit to 10 000 for flattened messages | D-F-10 |
| OTel JS 3.0 (2026-10-20) is not yet supported by OpenInference JS | D-F-8 |

## Gaps

- Langfuse and Laminar TypeScript sources were not available locally; their TS facts come from docs and are marked *(docs)* or *(unverified)* in the notes.
- `@arizeai/openinference-core` TS helpers were read from docs, not source.
