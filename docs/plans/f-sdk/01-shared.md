# F · Shared sub-phases (F1, F2)

See [README.md](README.md) for the decisions (D171–D186) and the shared contract. F1 is documentation only. F2 writes the conformance fixtures that both SDK tracks test against. Both run before the Python and TypeScript tracks start.

## F1 — Decisions and spec

**Commit:** `docs: record the SDK tracing decisions`

**Edit only:** `docs/mvp-decisions.md`, `docs/spec/10-sdk.md`, `docs/spec/06-evaluation.md`, `docs/spec/09-api.md`, `docs/spec/02-ingestion.md`, `fixtures/otlp/README.md`, and `docs/plans/f-sdk/` (only to renumber).

1. **Numbering.** Find the highest `D<n>` in `docs/mvp-decisions.md` on `origin/main` and on every open PR branch (`gh pr list`, then `git show origin/<branch>:docs/mvp-decisions.md`). If anything uses 171–186, renumber this plan's decisions to the next free block and update every reference under `docs/plans/f-sdk/` and `docs/research/sdk/`.
2. **`docs/mvp-decisions.md`:** add D171–D186 after the last row, one row each, shortened to one or two sentences in the style of the existing rows, each ending with the spec section it changes in parentheses. In the D80 row, append ` Missing API key: changed by D173.` and in the D121 row append ` The llmobs group: see D179.` Change nothing else.
3. **Spec 10 § Design rules:**
   - Rule 1 becomes D174 and D175: the default own provider; `use_global_provider=True` per language; `span_processor()`/`spanProcessor()`; the global OTel context; the default export filter in global mode and `export_filter`; `sample_rate` only on our own provider. Keep the sentence about `tracer_provider()`.
   - Rule 2 becomes D177: Python names from the instrumentor registry (link to a new subsection "Instrumentors" holding the README's § Python instrumentors table), TypeScript instrumentation objects (with `setTracerProvider`, `enable` and `disable`) and the `llmobs/openai`, `llmobs/anthropic`, `llmobs/langchain` helpers.
   - Rule 4: replace "a missing API key included (D80)" with D173's behaviour (warn once, no-op). Add the README's "Programming errors raise" paragraph (invalid `kind`, `provider` or `model`).
   - Add rule 6: "**Init once.** Any `init()` after the first, whatever the first did, logs one warning and changes nothing (D185)."
4. **Spec 10 § Configuration:**
   - Add rows: `use_global_provider` (TS `useGlobalProvider`), no env, default `false`; `export_filter` (TS `exportFilter`), no env, default none (D175).
   - The `api_key` row's default cell becomes "unset → one warning, SDK disabled (D173)".
   - Rule 5 becomes: "`api_key`, when given, must start with `sk-llmobs-` (a missing key: rule 11, D173). It never appears in error messages, …" (keep the rest of the sentence). Rule 10: add "`export_filter` must be callable" and "TypeScript `instrument` must be an array of instrumentation objects (each with `setTracerProvider` and `enable` functions)".
   - Rule 11: replace the missing-key sentence with D173's warning text and "the SDK runs disabled". Add: "An unknown or uninstalled Python instrumentor is named in the error (instrumentor names are not secret), together with the extra to install."
5. **Spec 10 § Lifecycle:** add "A second `init()`…" (D185), the fork rule (D185), and the score retry/drop rules from the README's § Score request (response handling). In the **Buffers** bullet, replace "Dropped counts are logged at warning level at most once per minute, with counts only." with "Dropped score counts are logged at warning level at most once per minute, with counts only; span drops are reported by the OTel batch processor's own logging." Keep the rest.
6. **Spec 10:** add a section **"## Recorded data"** after § Lifecycle containing, verbatim from this plan's README: § Names, § Span attributes, § Serialization, § LLM span, § Payload keys, and the masking rules of D176. Then remove from § Python API's "Implementation notes" the bullets those sections replace (the `observe`/`span` bullet and the serialization bullet), keep the generator bullet, add D181's input and generator-detection rules as a bullet, and replace the first sentence of the `context()` bullet with D182's (values stored under one key of the OTel `Context`, stamped by a span processor in `on_start`). Also copy the README's § Language differences as a subsection. Strip every plan-internal reference while copying ("F1", "P3", "T4", "P10 verifies…", "the test seam").
7. **Spec 10 § Python API:** update the code block: `llm_span(..., params={"temperature": 0.2})`, `s.set_usage(input_tokens=12, output_tokens=40)` stays, add `llmobs.score(...)` returning the id, and replace the scores implementation note with: "Scores are buffered (≤ 10 000) and sent in batches (≤ 1000, every 1 s) to `POST /api/v1/scores` on a background thread with client-generated UUIDv7 ids (D171, D172)." Add a short **"Threads"** paragraph from D184.
8. **Spec 10 § TypeScript API:** update the code block: `import { openai } from "llmobs/openai";` and `instrument: [openai(OpenAI)]`; add a `withLlmSpan` example mirroring the Python one; mention `useGlobalProvider` and `spanProcessor()`. Replace the browser bullet with D186's signature. Add a bullet: "Transport: one fetch-based exporter on every runtime (D178)." Add: "The SDK's context manager uses the `async_hooks` and `events` modules; Cloudflare Workers need the `nodejs_compat` flag."
9. **Spec 10 § Testing:** replace the bullet with: the conformance fixtures (D179) and the canonical form (copy the README's § Conformance list, the scenario table and the TypeScript calls table, without plan-internal references), unit tests per SDK against local mock HTTP servers, and "an integration suite against a real `llmobs serve` is added once the ingest and score routes exist".
10. **Spec 06 § Scores:** in the table, add a row `POST /api/v1/scores` (batch ≤ 1000) with an `sk` key (`ingest` scope) → `sdk` (D171). In "Deterministic score IDs", add after the first sentence: "The SDKs always send a `score_id` (a UUIDv7 per `score()` call, D172), so this applies to other clients that omit it."
11. **Spec 09 § Scores & annotations:** add the row `POST /api/v1/scores` | "batch create with an `sk` key (`ingest` scope); the project comes from the key (D171)".
12. **Spec 02 § Testing:** replace "The `llmobs` group is not captured: spec 10's SDK integration tests cover it (D121)." with "The `llmobs` group is not captured: the SDK conformance cases `llmobs/sdk-*` are the SDKs' expected output (spec 10 § Testing, D179)."
13. **`fixtures/otlp/README.md` § Layout:** after the sentence listing the group directories add: "In `llmobs`, cases named `sdk-<scenario>` are the canonical output of both SDKs for one scenario (spec 10 § Testing). Their `input.json` is in the canonical form described there; the SDK test suites compare against it, so change one only together with both SDKs."

**Checks:** `just check`. Read the changed spec sections once more end to end: no sentence may still say the API key is required at init, that TS `instrument` takes names, or that the server derives SDK score ids.

**Report:** the decision numbers used and every heading you changed.

## F2 — Conformance fixtures

**Commit:** `fixtures: add the SDK conformance cases`

**Edit only:** `fixtures/otlp/llmobs/sdk-*/` (new directories).

Create one case directory per row of the README's § Scenarios: `sdk-observe-tool`, `sdk-observe-nested`, `sdk-observe-error`, `sdk-generator-text`, `sdk-generator-json`, `sdk-span-manual`, `sdk-llm-span`, `sdk-context`, `sdk-capture-off`, `sdk-mask`, `sdk-media-bytes` (11 cases). Each has exactly `case.json`, `input.json` and `expected.json`.

**`case.json`:**
- `description`: one sentence naming the scenario, e.g. "Python `observe` / TypeScript `observe` on a tool function with a defaulted argument.";
- `origin`: `"hand_written"`;
- `spec_refs`: `["10 § Testing", "10 § Recorded data"]` plus the spec 02 sections the expected output exercises (e.g. `"02 § Input / output"`, `"02 § Context fields"`, `"02 § Cost computation"`, `"02 § Media extraction"`);
- `sdk-media-bytes` only: `"settings": {"media_min_bytes": 1}`;
- `notes`: the scenario's call in Python, one line, e.g. `"search_docs(\"pricing\") with @observe(kind=\"tool\", name=\"search_docs\")"`.

**`input.json`:** the canonical form (README § Conformance) of the spans in the scenario row. Write every attribute listed there and nothing else. For each span, in this order of keys: `traceId`, `spanId`, `parentSpanId` (non-roots only), `name`, `kind`, `startTimeUnixNano`, `endTimeUnixNano`, `attributes`, `events`, `status`. Concretely:
- All spans of a scenario share trace `00000000000000000000000000000001`; span `n` is `"%016x" % n` (`0000000000000001`, `0000000000000002`).
- Resource attributes, sorted: `service.name = "conformance"`, `telemetry.distro.name = "llmobs"`. Scope `{"name": "llmobs", "version": "0.0.0"}`.
- `kind`: `1`.
- `llmobs.input` / `llmobs.output` values are the exact compact JSON text from the scenario row (key order as written there), as `stringValue`.
- `llmobs.tags` is `{"arrayValue": {"values": [{"stringValue": "beta"}]}}`.
- `llmobs.metadata.seats` is `{"intValue": "3"}`; `gen_ai.request.temperature` is `{"doubleValue": 0.2}`; token counts are `intValue` strings.
- `gen_ai.input.messages` for `sdk-llm-span` is the string `[{"role":"system","parts":[{"type":"text","content":"Be brief."}]},{"role":"user","parts":[{"type":"text","content":"hi"}]}]`, and `gen_ai.output.messages` is `[{"role":"assistant","finish_reason":"stop","parts":[{"type":"text","content":"hello"}]}]` (README § LLM span: `role`, then `name`/`finish_reason` when present, `parts` last).
- `sdk-observe-error`: `status = {"code": 2, "message": "ScenarioError: boom"}` and `events = [{"timeUnixNano": "<span end>", "name": "exception", "attributes": [exception.message, exception.type]}]`.
- `sdk-observe-error`'s event attributes, sorted: `exception.message = "boom"`, `exception.type = "ScenarioError"` (the canonical form drops `exception.stacktrace`).
- `status` is omitted on every other span (unset).

**`expected.json`:** write it by hand from spec 02, following `fixtures/otlp/README.md` § expected.json and the two existing `llmobs` cases as models. Then:
1. `just fixtures-fmt`, then `just fixtures-check`.
2. Run `cargo test -p llmobs-otel --test golden`. If it fails on a new case, run `just fixtures-bless` and look at `git diff fixtures/otlp/llmobs/`. For each difference decide, by spec 02, who is right:
   - your hand-written value is wrong → keep the blessed value;
   - the normalizer contradicts spec 02 → restore your value (`git checkout` the file from your hand-written copy), **stop**, and report the rule and the case; do not change `crates/`.
3. `git diff --stat` must show only the 11 new directories.

Values to check carefully (they come from spec 02, not from this plan):
- `sdk-llm-span`: `span_kind = llm`, `provider = openai`, `request_model`, `response_model`, `request_params = {"temperature":0.2}`, `input_format = messages` with the two input messages, `output_format = messages`, tokens 12/3/total 15, cost from `fixtures/otlp/prices.json` row `gpt-4o-mini` (input 1.0, output 4.0 per 1M → `cost_input = 0.000012`, `cost_output = 0.000012`, `cost_total = 0.000024`, `cost_source = computed`), previews (input: last user message text `hi`; output `hello`).
- `sdk-context`: `session_id`, `user_id`, `tags`, `metadata = {"plan": "pro", "seats": "3"}`, `environment = staging`, `release = 1.2.3` on both spans.
- `sdk-media-bytes`: `media = [{"sha256": "497790947d4666760ce38f3c00e852c71fdb66cae849bae8e9ede352719e1581", "mime_type": "application/octet-stream", "size_bytes": 70}]` (the SDK declares `application/octet-stream`, and spec 02 § Media extraction sniffs only when no type is declared), and the input becomes `{"data":"media://497790947d4666760ce38f3c00e852c71fdb66cae849bae8e9ede352719e1581"}`.
- `sdk-observe-error`: `status = error`, `status_message = "ScenarioError: boom"`, the event kept in `events`.
- Small consumed attributes (≤ 256 bytes encoded) stay in `attributes` (spec 02 rule 2).

**Checks:** `just fixtures-check`, `cargo test -p llmobs-otel --test golden`, `just check`.

**Report:** any case where you kept a blessed value instead of your own and why, and any normalizer disagreement you stopped on.
