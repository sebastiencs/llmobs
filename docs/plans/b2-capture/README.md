# B2 · Fixture capture (`tools/capture`)

Track B2 of Wave 1 builds the tool that records what real instrumentation libraries emit, and uses it to commit captured OTLP fixtures under `fixtures/otlp/`. Track B1 (`docs/plans/b1-normalization/`) blesses each capture's `expected.json` when the normalizer can produce it.

A capture runs one **scenario**: a short Python or TypeScript program that uses a real instrumentation (OpenInference, OpenLLMetry, the Vercel AI SDK, …) against a **local mock** of the OpenAI, Anthropic and Gemini HTTP APIs. The program exports its spans over OTLP/HTTP to a **receiver** in the same tool. The tool then makes the spans deterministic, scrubs everything machine-specific, and writes `case.json` and `input.json`.

The plan has 20 sub-phases, **K1–K20**. Each is done by one agent, in order, and ends with `just check` green and one commit.

**Read first, for every sub-phase:**
- `AGENTS.md`;
- `fixtures/otlp/README.md`;
- `docs/spec/02-ingestion.md` § Testing and the mapping tables (after K1, with this plan's decisions);
- `docs/research/capture/README.md`;
- the sub-phase's own section.

## Files

| File | Sub-phases |
|---|---|
| [01-harness.md](01-harness.md) | K1 spec and decisions · K2 crate and scenario model · K3 OpenAI wire formats · K4 Anthropic and Gemini wire formats · K5 mock server · K6 OTLP receiver · K7 determinism · K8 scrubbing and writer · K9 runner and CLI |
| [02-first-capture.md](02-first-capture.md) | K10 Python project and the first capture (OpenAI via OpenInference) |
| [03-genai-openinference.md](03-genai-openinference.md) | K11 official OTel GenAI · K12 OpenInference providers · K13 OpenInference frameworks |
| [04-vercel.md](04-vercel.md) | K14 TypeScript project and Vercel AI SDK v5 · K15 Vercel AI SDK v6 and v7 |
| [05-other-conventions.md](05-other-conventions.md) | K16 OpenLLMetry providers · K17 OpenLLMetry frameworks and the Traceloop SDK · K18 Langfuse · K19 Laminar · K20 Google ADK |

## Decisions

These were settled with the user on 2026-11-10. K1 writes them into `docs/mvp-decisions.md`, spec 02 and `tools/README.md`.

**Numbering:** B1 landed on `main` as D102–D113, so these decisions are D114–D120.

| # | Decision | Source |
|---|---|---|
| D114 | **Responses come from a local mock provider at the HTTP layer.** It serves the OpenAI Chat Completions, Responses and Embeddings APIs, Anthropic Messages, and Gemini `generateContent`, `streamGenerateContent` and `embedContent`/`batchEmbedContents`, streaming (SSE) included. Each scenario scripts its turns in a provider-neutral form (text, reasoning, tool calls, finish reason, usage, model, delays). The mock renders the script into each provider's wire format. No API keys, no cost, deterministic, synthetic content only. SDK internals are never patched (MLflow's openai/httpx2 breakage). | User, research |
| D115 | **The harness is a Rust tool, `llmobs-capture`, in `tools/capture`.** It's a workspace member that is never shipped. It runs the mock and an OTLP/HTTP receiver on loopback ports, starts the scenario program with a clean environment pointing at both, waits for it, then writes `case.json` and `input.json` through `opentelemetry-proto`'s serde encoding. Scenario programs live in two standalone projects: `tools/capture/py` (uv) and `tools/capture/ts` (pnpm). | User |
| D116 | **Captures are deterministic except for timing.** Trace and span IDs are re-minted from the case path in order of appearance. Times are shifted so the first span starts at `ingest_time − 10 s`, keeping real durations rounded to the millisecond, and timestamp-valued attributes get the same shift. The mock's delays are scripted (time to first chunk, delay between chunks), so timings stay stable to a few milliseconds. Machine-specific values are pinned or dropped: `server.address`/`server.port` become the real provider host and 443, `host.name` becomes `capture-host`, and process, OS-version and path attributes are dropped. Instrumentation and SDK versions are kept. A final leak check refuses to write if a loopback address, the mock or receiver port, the home directory, the repository path or the machine's hostname is still present. A recapture of an unchanged scenario differs only in timing values and `captured_at`. | User, research (LangWatch's plan) |
| D117 | **Captures are run by hand, never in CI.** `just capture <group>/<case>` records one, `just capture-all` records all of them, and `just check-capture` runs the scenario projects' lint and type checks plus a recapture of every scenario compared with the committed `input.json` (timing excluded). The scenario projects are outside the root pnpm workspace and `just check`, so CI never installs LangChain, ADK or three majors of `ai`. Versions are exact pins, bumped by hand when someone recaptures. Dependabot doesn't watch these projects. The 7-day release-age rule still applies: pnpm `minimumReleaseAge` and uv `exclude-newer = "7 days"`. The Rust crate is a workspace member, so its unit tests run in `just check` and CI like any other crate. | User |
| D118 | **A captured case may be committed with only `case.json` and `input.json`.** B1 writes `expected.json` in bless mode when its convention sub-phase lands, then reviews it line by line. The fixture validation test accepts a missing `expected.json` for `origin: "captured"` cases only. The golden test skips those cases and lists them as awaiting bless. This needs request R1 from B1 (below). | User |
| D119 | **What is captured.** Spec 02 § Testing's list is replaced by this matrix: the official OTel GenAI instrumentations (OpenAI v2, Google GenAI); OpenInference and OpenLLMetry for the OpenAI, Anthropic and Gemini clients and for LangChain, LangGraph and the OpenAI Agents SDK, where each has an instrumentation; the Vercel AI SDK v5, v6 and v7; the Langfuse SDK (current major, 5.x, Python); Laminar (Python); Google ADK's native spans and OpenInference's ADK instrumentation. The `llmobs` group (our SDKs) is not captured: spec 10's SDK integration tests cover it. | User |
| D120 | **Case policy.** A recapture replaces `input.json` and `case.json` in place, and git history keeps the old shapes. A captured `input.json` is never edited by hand: change the scenario and recapture. Each instrumentation × provider gets one non-streaming chat case (with a tool call where the API has one) and one streaming case, plus scenario-specific cases (cache tokens, multimodal input, embeddings, agent runs). Majors are kept side by side only where this plan says so (the Vercel AI SDK). | User |

## Boundaries

```
llmobs-capture run <group>/<case>
  ├─ mock provider  (127.0.0.1:0)  ◀── HTTP ── scenario program (uv run / node) ── real client library + instrumentation
  ├─ OTLP receiver  (127.0.0.1:0)  ◀── OTLP/HTTP protobuf ── same program
  └─ after exit: merge → re-mint IDs → shift times → prune defaults → scrub → leak check
       └─ writes fixtures/otlp/<group>/<case>/{case.json, input.json}
B1 golden test, LLMOBS_BLESS=1 ── writes expected.json ── human review ── commit
```

- **B2 owns:**
  - `tools/capture/`;
  - the captured cases' `case.json` and `input.json` under `fixtures/otlp/`;
  - the `capture`, `capture-all` and `check-capture` recipes in `justfile`;
  - `tools/README.md`, `docs/research/capture/`, this plan;
  - the spec 02 § Testing list and the decisions.
- **B2 also edits** these files outside its directories:
  - the root `Cargo.toml`: the workspace member, plus the new dependencies `axum`, `prost`, `tower` and `http-body-util`;
  - `biome.json`, adding `tools/capture/ts/**` to `files.includes`;
  - `AGENTS.md` § Layout and § Commands.
- **B1 owns:**
  - `crates/otel/` (the validation test, the golden test, bless mode);
  - every `expected.json`;
  - the normalization rules.

  B2 never writes an `expected.json` and never changes `crates/otel`.
- **Core:** nothing. B2 uses `opentelemetry-proto` directly and never depends on a workspace crate. The tool must not depend on `llmobs-otel`, so captures don't change when the normalizer does.
- **CI:** nothing beyond the new workspace crate, whose tests run in `check-rust`. No new job, no Dependabot entry.
- **SDKs (track F):** nothing. The `llmobs` group stays hand-written or comes from spec 10's integration suite.

### Requests

These go to the coordinating session. Each must be on `main` before the sub-phase that needs it. If one is missing, the agent stops and reports it.

| # | To | Needed by | Request |
|---|---|---|---|
| R1 | B1 | K10 | **Captured cases without `expected.json`** (D118). In `crates/otel/tests/support/mod.rs` and `fixtures.rs`:<br>• a case directory with `origin: "captured"` may hold only `case.json` and `input.json`; every other check still applies to those two files, and a hand-written case still needs all three;<br>• `fixtures/otlp/README.md` § Layout, § Workflows › Captured case and § expected.json say so.<br>In B1's golden test (N4):<br>• a case without `expected.json` is skipped, and the test prints `awaiting bless: <group>/<case>` for each one;<br>• `just fixtures-bless` writes `expected.json` for these cases too.<br>If N4 hasn't landed when R1 is done, the golden-test half becomes part of N4. |
| R2 | B1 | — | **Re-check against captures.** Once K10–K20 land, B1 blesses each captured case after its convention sub-phase and reviews it. Each D111 behaviour marked "pinned by a captured fixture" is confirmed or corrected against the real capture: the Vercel ≤ v5 + Anthropic exclusive input, event-based TTFT, and the OpenLLMetry Anthropic and ADK token semantics. The `notes` of the hand-written fixtures that said "re-check against a B2 capture" are updated. |
| R3 | B1 | K6 | **`prost` in the root `Cargo.toml`.** B1's N2 and B2's K6 both add `prost = "0.14"` (matching `opentelemetry-proto` 0.33.1). Whichever lands second reuses the existing entry instead of adding one. |

## The scenario format

A scenario is a directory `tools/capture/{py,ts}/scenarios/<group>/<case>/` holding `scenario.json` and the program (`main.py` or `main.ts`). `<group>/<case>` is the fixture path it writes, so the group must be one of the groups in `fixtures/otlp/README.md` and the case name is kebab-case. K2 defines the types, and every later sub-phase writes scenarios in this shape:

```json
{
  "description": "OpenAI chat completion with one tool call, traced by OpenInference.",
  "spec_refs": ["02 § Mapping tables / Input / output"],
  "notes": "Optional free text, copied into case.json.",
  "settings": { "media_min_bytes": 64 },
  "context": { "ingest_time": "2026-01-01T00:00:00.000000000Z" },
  "instrumentation": "openinference-instrumentation-openai",
  "libraries": ["openai", "openinference-instrumentation-openai", "opentelemetry-sdk"],
  "env": { "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT": "true" },
  "timestamp_attributes": [{ "key": "langfuse.observation.completion_start_time", "format": "rfc3339" }],
  "timing_attributes": ["ai.response.msToFirstChunk"],
  "turns": [
    {
      "api": "openai.chat",
      "model": "gpt-4o-mini-2024-07-18",
      "message": {
        "text": null,
        "reasoning": null,
        "tool_calls": [{ "id": "call_weather_1", "name": "get_weather", "arguments": { "city": "Paris" } }]
      },
      "finish": "tool_calls",
      "usage": { "input": 82, "output": 17, "cache_read": 0, "cache_write": 0, "reasoning": 0 },
      "ttft_ms": 30,
      "chunk_delay_ms": 5,
      "chunks": 3
    }
  ]
}
```

- `settings`, `context` and `notes` are copied into `case.json` unchanged (same keys and rules as `fixtures/otlp/README.md` § case.json).
- `instrumentation` names the package that produces the convention under test. It becomes `source.instrumentation`, and its version becomes `source.instrumentation_version`.
- `libraries` names the packages whose versions go into `source.libraries`. The program reports the versions (K10, K14).
- `env` holds extra environment variables for the program, for instrumentation switches only. The names must start with `OTEL_`, `OPENINFERENCE_`, `TRACELOOP_`, `LANGFUSE_` or `LMNR_`, and the values must not look like secrets (K2 validates both).
- `timestamp_attributes` lists the attributes whose values are timestamps and must be shifted with the spans (D116). `format` is `rfc3339`, `unix_nano` or `unix_milli`.
- `timing_attributes` lists the attributes whose values are measured durations or rates. They're kept as captured, and `just check-capture` ignores their values.
- `turns` are served **in order**, one per request to the mock, whatever the endpoint. Every turn of a scenario uses the same provider (OpenAI, Anthropic or Gemini), so the scrubber knows which real host to put back. The fields:
  - `api` must match the request's endpoint: `openai.chat`, `openai.responses`, `openai.embeddings`, `anthropic.messages`, `gemini.generate_content`, `gemini.embed_content`.
  - `usage` uses **OTel semantics**: `input` includes the cache tokens and `output` includes the reasoning tokens. Each renderer converts to its provider's native semantics (K3, K4). The fixture then pins whether the normalizer reconstructs the scripted numbers, which is exactly what spec 02's token rules are about.
  - Streaming is decided by the request (`"stream": true`, or Gemini's `:streamGenerateContent`), never by the script. `ttft_ms` (default 30), `chunk_delay_ms` (default 5) and `chunks` (default 3, the number of pieces the text is split into) only apply when the request streams.
  - Embedding turns use `{"api": "openai.embeddings", "model": …, "dims": 8, "usage": {"input": 6}}` and no `message`.

## Layout at the end of B2

```
tools/capture/
  Cargo.toml                 llmobs-capture (lib + bin), publish = false
  src/
    lib.rs, main.rs          K2, K9
    error.rs                 K2 CaptureError
    scenario.rs              K2 Scenario, Turn, Usage, validation
    wire/                    K3 openai.rs · K4 anthropic.rs, gemini.rs · mod.rs (Rendered, SSE frames)
    mock.rs                  K5 axum app, turn queue, mock log
    receiver.rs              K6 OTLP/HTTP receiver
    determinism.rs           K7 merge, ID re-mint, time shift, default pruning
    scrub.rs                 K8 pinning rules, leak check
    write.rs                 K8 case.json, input.json, canonical JSON
    run.rs                   K9 runner, program environment, --check comparison
  py/                        K10 uv project: pyproject.toml, uv.lock, .python-version, src/capture_py/harness.py,
                                 scenarios/<group>/<case>/{scenario.json, main.py}
  ts/                        K14 pnpm project: package.json, pnpm-lock.yaml, pnpm-workspace.yaml, tsconfig.json,
                                 harness.ts, scenarios/<group>/<case>/{scenario.json, main.ts}
```

## Rules for every sub-phase

- **Edit only** the files the sub-phase lists. The boundary exceptions above are allowed only in the sub-phases that name them.
- **Rust code follows AGENTS.md** like product code:
  - errors are a `thiserror` enum (`CaptureError`); `anyhow` stays reserved for `llmobs-cli`;
  - no `unwrap`/`expect` without an invariant comment;
  - no panics on data from the program, the network or scenario files;
  - `print_stdout`/`print_stderr` are warned workspace-wide, so the CLI reports through `tracing` (or allows the lint on `main` only, with a comment).
- **Rust dependencies:** each one is declared once in `[workspace.dependencies]`. Versions checked on 2026-11-10 (`cargo info`); recheck at the time:
  - `axum` 0.8.9 (default features);
  - `prost` 0.14 (matches `opentelemetry-proto` 0.33.1; the lock has 0.14.4; see R3);
  - `tower` 0.5.3 and `http-body-util` 0.1.5, dev-dependencies for testing the axum apps;
  - already in the workspace: `tokio` (the crate adds the `net` and `process` features locally), `serde`, `serde_json`, `thiserror`, `clap`, `sha2`, `hex`, `chrono`, `tracing`, `tracing-subscriber`, `opentelemetry-proto`, `regex`.
- **Scenario-project dependencies:** exact pins. Pick the newest release **older than 7 days** at implementation time (PyPI's JSON API, `pnpm view`). The versions named in this plan were the latest on 2026-11-10 and are for orientation only. Record the chosen versions in the report.
- **Synthetic content only.** Prompts, tool names and replies are invented. Use no real API key anywhere: the program gets `capture-mock-key` (anything matching `sk-` fails the fixture secret scan). Programs make no network call other than to the mock and the receiver.
- **Never edit a captured `input.json` by hand.** Change the scenario and recapture (D120).
- **Fixtures:** after a capture, run `just fixtures-check`. A capture that fails it (size, secrets, window, layout) is fixed in the scenario, not in the file.
- **Checks:** `just check` green (the PATH needs Node; see AGENTS.md § Commands). From K10 on, `just check-capture` green too.
- **Report:**
  - what was done;
  - the exact versions chosen;
  - every instrumentation behaviour that surprised you (an attribute spec 02 doesn't know, a missing one, a content-capture switch);
  - any deviation from this plan, and why;
  - anything unverified.
- **One commit** per sub-phase, with the given subject and the attribution line of AGENTS.md.

## Order

K1 → K2 → K3 → K4 → K5 → K6 → K7 → K8 → K9 → K10 → (K11, K12, K13, K14, K16, K17, K18, K19, K20 in this order) → K15 after K14.

The order of the captures follows B1's sub-phases (N13 GenAI, N18 OpenInference, N19 Vercel, N20 OpenLLMetry, N21 Langfuse and Laminar, N22 ADK), except that OpenAI via OpenInference goes first (K10) to prove the harness on the most stable instrumentation.

The sub-phases are meant to run one at a time. After K10, the capture sub-phases touch separate scenario directories and fixture groups, but they share `uv.lock`, `pyproject.toml` and `justfile`, so running them in parallel means merging those files.
