# How competitors get instrumentation fixtures

Research for track B2 (`tools/capture`), done on 2026-11-10 in the checkouts under `/home/sebastien/github/`. It asks one question: how do LLM observability projects get the OTel spans that real instrumentation libraries emit, so that their ingestion code can be tested against them? Paths are relative to each repository. No code was copied.

## Summary

- **Nobody keeps a library of real OTLP exports** from OpenInference, OpenLLMetry, the Vercel AI SDK, ADK and the like. Every competitor tests its ingestion against attribute maps written by hand in the test code. Some of those maps were copied from real spans, but none were recorded by a tool.
- Real data, where it exists, lives **at the HTTP layer** (Phoenix's vcrpy cassettes, Helicone's provider response snapshots) or **after ingestion** (Langfuse's framework traces, stored in its own format).
- **Version drift is handled by pinning, not by recapturing.** MLflow caps `openai` because 3.x broke its mocks; Langfuse's captures are frozen at whatever versions were live on their date.
- **Scrubbing is manual everywhere.** LangWatch is the only one that writes down a procedure (re-mint IDs, shift times, scrub hosts and paths), and it isn't built.

B2 is therefore new ground. The useful lessons are about mocking and determinism, not about fixture formats.

## Per project

### Langfuse

- `worker/src/__tests__/chatml/framework-traces/`: 21 real traces from frameworks (ADK, Gemini, LangGraph, OpenAI Agents, pydantic-ai, the Vercel AI SDK…), downloaded from Langfuse Cloud by `download_traces.js` from the docs' example projects. They're stored **after ingestion** (`{trace, observations}`), so the OTLP is gone; only the scope names and versions survive in `metadata`. The script never refreshes an existing file.
- `packages/shared/src/utils/normalized-io/conventions/providers/*/fixtures.ts` (~12k lines): per-convention fixtures, each block labelled with its provenance: "docs-derived, not captured", "reconstructed from a user-provided trace excerpt", "anonymized customer payload". That labelling is worth copying: our `case.json` `origin` and `notes` already do it.
- `otelMapping.servertest.ts` (~11k lines) builds OTLP requests inline, with hand-typed scope names.
- The Vercel AI SDK's own test helper (`MockLanguageModelV4` from `ai/test`) is used for their own telemetry tests. It bypasses the provider packages, so the spans it produces don't look like production `@ai-sdk/openai` spans.
- Anonymisation is manual ("do not regenerate" in a README).

### Laminar (lmnr)

- About 92 inline Rust tests in `app-server/src/traces/spans.rs`, built from `HashMap` attribute literals, one per convention (OpenAI, LangChain, AI SDK v4/v5, Google GenAI, GenAI semconv). One comment says a test is "based on real span data from the logs". No tooling, no mocks.
- `processor_tests.rs` can dump a normalized snapshot (`PROCESSOR_SNAPSHOT_OUT`) to diff two builds. That's the same idea as our bless mode.

### LangWatch

- About 25 canonicaliser test files, spans built inline. ADK spans are described as "anonymised replicas" of real payloads.
- `services/sims/telemetrysim/fixtures/README.md` designs a recorded-fixture corpus (`<family>/<name>.otlp.json` + `.expected.json`) and a manual capture recipe: point the exporter at a local collector with a JSON file exporter, scrub users, paths, hostnames, emails and prompt text, drop keys. The planned loader would "re-mint trace, span and session ids and shift every instant" from a seed. The directory is empty and the loader isn't built.
- SDK tests mock the provider at the HTTP client (`httpx.MockTransport`, msw, a Go round-tripper with literal SSE `data:` lines). Their e2e jobs make real LLM calls, with a comment that they "spend real LLM money".

### Phoenix

- Normalizer tests are inline dicts (`tests/unit/trace/gen_ai/test_conversion.py`).
- 29 vcrpy cassettes (`record_mode="once"`, all headers deleted). Their guide for agents: never hand-edit a cassette, delete and re-record; pin outputs with "reply with exactly…" at temperature 0; pop every expected attribute and then assert none are left, so unexpected attributes fail the test.
- `scripts/testing/otel_gen_ai/send_{openai,anthropic,google}_traces.py`: `uv run --script` scripts that run the official `opentelemetry-instrumentation-*-v2` libraries against **live** APIs and send OTLP to a local Phoenix. The cassettes they write are gitignored, so this is manual testing, not fixtures. One docstring says record mode "once" while the code uses "all".
- Real OpenInference traces for demos are parquet files on GCS, downloaded at runtime.

### MLflow

- Translators for OpenInference, Traceloop, Vercel AI, ADK, GenAI semconv, Langfuse… are tested with inline attribute dicts. The Vercel values look copied from a real span.
- **A hand-written fake OpenAI server** (`tests/openai/mock_openai.py`, FastAPI, started as a subprocess on a free port): `/chat/completions` (with SSE, honouring `stream_options.include_usage`), `/responses` (with SSE events), `/completions`, `/embeddings`, `/images/generations`. Special behaviours are chosen with **magic strings** in the first message (`TOOL_CALL_STREAM`, `ERROR_MID_STREAM`, …). The chat response echoes the input. IDs and timestamps are constants.
- **Anthropic and Gemini are mocked by patching SDK internals** (`anthropic._base_client.SyncAPIClient.post`, `google.genai.models.Models._generate_content`). `ml-package-versions.yml` caps `openai` at 2.47.0 because "openai 3.x switched its internal HTTP client from httpx to httpx2, which breaks the autolog tests' mocking". That's the cost of mocking below the HTTP layer.
- Library versions are tracked in one YAML file, with a nightly cross-version workflow that includes dev builds.

### Opik

- Backend mapping tests build protobufs in Java, inline.
- SDK integration tests (23 libraries) call **live** providers with keys from repository secrets, and compare with `ANY_BUT_NONE`/`ANY_STRING` matchers to absorb non-determinism. Requirements are mostly unpinned; legacy versions get their own workflows.

### Helicone

- OTLP ingestion (a legacy OpenLLMetry path) has no tests.
- 15 `.snapshot` files of real provider responses (stream and non-stream), unscrubbed (real `chatcmpl-…` IDs). Worker tests mock providers with Cloudflare's `fetchMock`.

## What B2 takes from this

1. **Mock at the HTTP layer, never inside an SDK** (MLflow's httpx2 lesson). The official client libraries then parse our mock's responses exactly as in production, and an SDK changing its HTTP client doesn't break anything.
2. **Script the responses explicitly per scenario.** MLflow's magic strings and echo replies make scenarios implicit. A neutral script (text, tool calls, finish reason, usage) is readable in review and pins the token counts the normalizer must reproduce.
3. **Use the real provider packages.** Test helpers such as `ai/test`'s mock language model bypass `@ai-sdk/openai`, so their spans differ from production. The HTTP mock lets the real packages run.
4. **Automate determinism and scrubbing** (LangWatch's unbuilt plan): re-mint IDs from a seed, shift times into the window, pin or drop host, process and path attributes, then refuse to write if anything machine-specific is left.
5. **Label provenance and versions** in every case (Langfuse's block comments, MLflow's version file): our `case.json` `source` records the instrumentation and library versions.
6. **Recapture instead of hand-editing** (Phoenix's rule for cassettes): a captured `input.json` is never edited by hand; change the scenario and recapture.
7. **Catch unexpected attributes** (Phoenix's pop-then-assert): our golden comparison is already exact, so every attribute an instrumentation adds shows up in review.
