# B2 · OpenLLMetry, Langfuse, Laminar, Google ADK (K16–K20)

See [README.md](README.md) and the common rules in [03-genai-openinference.md](03-genai-openinference.md#common-rules-for-capture-sub-phases).

These libraries include vendor SDKs that like to phone home: anonymous telemetry, their own cloud endpoints, extra APIs. Two rules hold in every sub-phase here:

- **Vendor telemetry and cloud calls are switched off,** through `env` in the scenario (for example `TRACELOOP_TELEMETRY=false`) or through the SDK's init options. Find the switch in the installed source, and list each one in `notes`.
- **Exports go to the receiver.** A vendor SDK that exports traces to its own path (`/api/public/otel/v1/traces`) is pointed at the receiver's base URL, which the receiver accepts (K6). Add `receiver_base_url()` to `capture_py/harness.py`: the OTLP endpoint without `/v1/traces`.
  - **gRPC only:** if a vendor SDK can only export over gRPC, attach `harness.setup()`'s exporter to the SDK's tracer provider instead (most accept an extra span processor or an exporter), and say so in `notes`.
  - **Any other request:** a request from the SDK to any other path gets a 404 from the receiver. If the SDK then fails, find the option that disables that call. If none exists, report it.

If an instrumentation switch needs an environment variable whose prefix isn't in K2's allow-list (`OTEL_`, `OPENINFERENCE_`, `TRACELOOP_`, `LANGFUSE_`, `LMNR_`), add the prefix to the list in `scenario.rs`, with a test, in the same commit.

## K16 — OpenLLMetry providers

**Commit:** `capture: record OpenLLMetry provider instrumentations`

**Packages** (2026-11-10): `opentelemetry-instrumentation-openai` 0.63.0, `opentelemetry-instrumentation-anthropic` 0.63.0, `opentelemetry-instrumentation-google-generativeai` 0.63.0. They're used directly, without `traceloop-sdk`. Content capture is on by default; check `TRACELOOP_TRACE_CONTENT` and set it explicitly.

Check in the source whether `opentelemetry-instrumentation-google-generativeai` instruments the `google-genai` client or only the legacy `google-generativeai` package.
- If it instruments `google-genai`, capture the Gemini case with it.
- If it only instruments the legacy package, capture it with that package if it still installs on CPython 3.13, using the mock's Gemini routes. Otherwise skip the case and report it.

OpenLLMetry has been moving from `gen_ai.prompt.N.*` to the current GenAI conventions. Capture whatever the pinned version emits, and report which form it is (B1 N20 needs it).

**Cases** (group `openllmetry`):

| Case | Scenario |
|---|---|
| `openai-chat-tool` | The K10 conversation. |
| `openai-chat-stream` | A streamed answer with `include_usage`. |
| `openai-embeddings` | Two inputs, `dims` 8. |
| `anthropic-cache-tokens` | As K12's, through OpenLLMetry. Covers D111 (OpenLLMetry Anthropic token semantics): report whether `gen_ai.usage.input_tokens` carried Anthropic's raw `input_tokens` (100) or the inclusive total (2100). |
| `anthropic-stream-tool` | As K12's. |
| `google-genai-multimodal` | As K11's, if the instrumentation supports it (above). |

## K17 — OpenLLMetry frameworks and the Traceloop SDK

**Commit:** `capture: record OpenLLMetry framework instrumentations`

**Packages** (2026-11-10): `opentelemetry-instrumentation-langchain` 0.63.0, `opentelemetry-instrumentation-openai-agents` 0.63.0, `traceloop-sdk` 0.63.0, plus K13's framework pins.

**Cases** (group `openllmetry`):

| Case | Scenario |
|---|---|
| `langchain-chat-tool` | K13's scenario, through `opentelemetry-instrumentation-langchain`. |
| `langgraph-agent` | K13's scenario, through the same instrumentation. |
| `openai-agents-tool` | K13's scenario, through `opentelemetry-instrumentation-openai-agents`. |
| `traceloop-workflow` | `traceloop-sdk`. `Traceloop.init` with the exporter pointed at the receiver, batching disabled, and anonymous telemetry off. A `@workflow` function calls a `@task` function, which calls OpenAI once (the OpenAI instrumentation comes with the SDK). Association properties `session_id`, `user_id` and one more key give the `traceloop.association.properties.*` and `traceloop.workflow.name` attributes (spec 02 rule 11, B1 N20). Disable every instrumentation `traceloop-sdk` would enable other than OpenAI, if the SDK allows it. |

## K18 — Langfuse SDK

**Commit:** `capture: record the Langfuse SDK`

**Package:** `langfuse` 5.2.0 on 2026-11-10, the current major per D119. Read the v5 Python SDK's source and docs for:
- how it is configured: host, keys, and `LANGFUSE_*` variables;
- how trace-level fields are set (session, user, tags, metadata, release, environment);
- the OpenAI drop-in integration.

**Settings:** public key `capture-public-key`, secret key `capture-secret-key` (never anything matching `sk-` or `pk-`), and the host set to `receiver_base_url()`.

**Cases** (group `langfuse`):

| Case | Scenario |
|---|---|
| `openai-wrapper-chat` | The `langfuse.openai` drop-in client: one chat call, with usage including `cache_read`, and trace-level session, user, tags and one metadata key. |
| `observe-nested` | An `@observe` function that calls an `@observe(as_type="tool")` function, then makes an OpenAI call through the drop-in client. |
| `generation-usage-cost` | One OpenAI call through the plain `openai` client, wrapped in a manual generation observation that records `usage_details` (input, output, cached and reasoning keys), `cost_details`, `model_parameters` and `completion_start_time`. List `completion_start_time` in `timestamp_attributes` with the format it actually has. This pins B1 N21's usage, reported-cost (D107) and TTFT sources. |
| `level-error` | An observation with level `ERROR` and a status message, with no OTLP error status set (B1 N21's new rule). It still needs one mock turn, because a scenario has at least one: make one OpenAI call inside it. |

## K19 — Laminar

**Commit:** `capture: record Laminar`

**Package:** `lmnr` 0.8.9 on 2026-11-10. Read its source for:
- `Laminar.initialize` options: base URL, ports, HTTP versus gRPC export, project API key, which instruments are enabled;
- how sessions, users, metadata and tags are set.

**Settings:** project key `capture-project-key`, with only the OpenAI instrument enabled.

**Cases** (group `laminar`):

| Case | Scenario |
|---|---|
| `openai-chat` | One OpenAI chat call, auto-instrumented by Laminar. |
| `openai-chat-stream` | The same call, streamed with usage. |
| `observe-session` | An `@observe` function that makes one OpenAI call, with session ID, user ID, metadata and tags set through Laminar's API. That gives the `lmnr.association.properties.*` attributes for B1 N21. |

## K20 — Google ADK

**Commit:** `capture: record Google ADK`

**Packages** (2026-11-10): `google-adk` 2.14.0, `openinference-instrumentation-google-adk` 1.1.2, and K11's `google-genai` pin, unless ADK requires another version.

**Pointing ADK at the mock:** ADK's Gemini model uses the `google-genai` client. Find how to give it a base URL: a model option, a client the agent accepts, or an environment variable. Use the API key path (`GOOGLE_API_KEY`), not Vertex AI.

**Content:** ADK writes requests and responses into `gcp.vertex.agent.*` attributes. Check whether a switch controls that, and set it.

**Cases:**

| Case | Scenario |
|---|---|
| `google_adk/agent-tool` | An `Agent` with one Python function tool, run once with `Runner` and an in-memory session service. Two `gemini.generate_content` turns: a function call, then the answer. ADK's native spans only. |
| `google_adk/agent-stream` | The same agent with SSE streaming enabled in the run config, so turns stream. |
| `google_adk/agent-thoughts` | The agent with thinking enabled in its generate-content config, answered with reasoning plus text, usage `reasoning` > 0. Covers D111 (ADK token semantics): report whether `gen_ai.usage.output_tokens` includes the thoughts. |
| `openinference/google-adk-agent-tool` | `agent-tool`'s scenario with `openinference-instrumentation-google-adk` enabled. Report how its spans relate to ADK's native ones: both present, nested, or duplicated. |

**After K20**, report to the coordinating session that B2's matrix is complete. List every captured case with the B1 sub-phase that should bless it:

| Group | Sub-phase |
|---|---|
| `genai` | N13 |
| `openinference` | N18 |
| `vercel` | N19 |
| `openllmetry` | N20 |
| `langfuse`, `laminar` | N21 |
| `google_adk` | N22 |

Also list every spec 02 row the captures contradicted. That list is R2's input.
