# F · SDK tracing (`sdks/python`, `sdks/typescript`)

Track F of Wave 1 builds the tracing half of spec 10 in both SDKs:
- `init()` and the lifecycle (`flush`, `shutdown`, exit hooks, serverless);
- the OTLP exporter (the OTel HTTP/protobuf exporter in Python, our own `fetch` exporter in TypeScript);
- `observe` (decorator / wrapper) for sync and async functions, generators and async generators;
- manual spans (`span` / `withSpan`) and the manual LLM span (`llm_span` / `withLlmSpan`);
- `context(...)` / `withContext(...)` with optional W3C baggage;
- `score(...)` over REST with a background queue;
- payload serialization, the 1 MiB client-side truncation, `capture_content`, masking (also over instrumentor spans), sampling;
- instrumentation wiring: OpenInference instrumentors by name in Python, instrumentor objects plus `llmobs/openai`, `llmobs/anthropic`, `llmobs/langchain` helpers in TypeScript;
- the Vercel AI SDK helper `aiTelemetry()` and the browser `feedback()` entry point (TypeScript);
- cross-language conformance fixtures in `fixtures/otlp/llmobs/sdk-*`, checked by both SDK test suites and by the Rust golden test.

**Out of scope:** datasets, experiments, `Client`, `evaluate` (Wave 2); every server route (the score route is a request to the server track, § Requests); the normalizer (B1). The SDKs are tested against local mock HTTP servers and the shared fixtures; no test needs a running `llmobs serve`.

The plan has **28 sub-phases**: two shared ones, then a Python track and a TypeScript track that run **in parallel**, then one closing review.

| File | Sub-phases |
|---|---|
| [01-shared.md](01-shared.md) | **F1** decisions and spec · **F2** conformance fixtures |
| [02-python.md](02-python.md) | **P1** dependencies, missing-key change, logging · **P2** serialization · **P3** runtime, provider, exporter, lifecycle · **P4** `context()` and the context processor · **P5** `span()` and `observe` (sync, async) · **P6** generators · **P7** `llm_span` · **P8** masking pass, export filter, global mode · **P9** scores · **P10** instrumentation · **P11** conformance tests · **P12** README and examples |
| [03-typescript.md](03-typescript.md) | **T1** dependencies, entry points, runtime globals, missing-key change · **T2** serialization · **T3** fetch exporter · **T4** runtime, provider, lifecycle · **T5** `withContext` and the context processor · **T6** `withSpan` and `observe` · **T7** generators · **T8** `withLlmSpan` · **T9** masking pass, export filter, global mode · **T10** scores · **T11** instrumentation helpers and `aiTelemetry` · **T12** browser `feedback()` · **T13** conformance tests and README |
| [04-review.md](04-review.md) | **X1** cross-language review |

**Order.** F1 → F2 → (P1…P12 ‖ T1…T13) → X1. Inside a track the sub-phases are sequential. P-sub-phases edit only `sdks/python/`; T-sub-phases edit only `sdks/typescript/` (plus `pnpm-lock.yaml` in T1 and T11). So the two tracks never touch the same file. F1, F2 and X1 run alone.

**Every sub-phase:**
- Read `AGENTS.md`, `docs/spec/10-sdk.md` (after F1), `docs/research/sdk/README.md`, this README, and the sub-phase's section.
- Edit only the paths the sub-phase lists. If you need a change elsewhere, stop and report it.
- Run the commands the sub-phase lists, then `just check` (it runs Rust, web, Python and TypeScript). It must pass.
- Make one commit with the subject the sub-phase gives. No attribution lines.
- Report: what you did, any deviation and why, the exact version of any new dependency, anything left unverified.
- When this plan and a spec disagree, the plan wins only if F1 changed the spec accordingly; otherwise stop and ask. When something is not specified here, stop and ask; do not invent behaviour.

## Decisions

Settled with the user on 2026-11-12. F1 writes them into `docs/mvp-decisions.md` and the specs.

**Numbering:** `main` ends at D168, and the A1 M4 plan (PRs #28 and its implementation branch) holds D169–D170. These are **D171–D186**. F1 checks again and renumbers if needed.

| # | Decision | Source |
|---|---|---|
| D171 | **Key-scoped score route.** `POST /api/v1/scores` takes an `sk-llmobs-` key with the `ingest` scope; the project comes from the key. Body `{"scores": [ScoreCreate, …]}`, 1 to 1000 items (§ Score request). The SDKs send scores only there. The project-scoped `POST /api/v1/projects/{p}/scores` stays for PATs and the UI. | User |
| D172 | **SDK score ids.** Every `score()` call creates a new score with a UUIDv7 generated in the SDK; retries resend the same id, so they are idempotent. Passing `score_id` updates that score instead. SDK scores have `source = "sdk"` and carry `user_id` from the active `context()` when set. This replaces the server-derived id for SDK scores (spec 06 keeps it for clients that omit `score_id`). | User |
| D173 | **Missing API key: warn and run disabled.** When no API key is given (argument and `LLMOBS_API_KEY` both unset), `init()` logs one warning (`llmobs: no API key; set LLMOBS_API_KEY, or LLMOBS_ENABLED=false to silence this; tracing is disabled`) and the SDK runs in no-op mode. Every other invalid value, including a key without the `sk-llmobs-` prefix, still raises `ConfigError`. This changes D80 and spec 10 § Design rules 4 and § Configuration rule 11. | User |
| D174 | **Provider modes.** Default: our own `TracerProvider`, never registered globally. `use_global_provider=True` (TS `useGlobalProvider: true`): Python adds our processor to the global SDK `TracerProvider` when one is set, otherwise registers ours as the global one; TypeScript (OTel JS 2.x providers cannot take processors after construction) registers ours as the global provider when none is set, otherwise logs one warning and stays private. Both SDKs expose `span_processor()` / `spanProcessor()`, our processor, for apps that build their own OTel pipeline. The OTel *context* is always the global one, so instrumentor spans nest under ours in both modes. `sample_rate` applies only to our own provider; in global mode with an existing provider a rate below 1 logs one warning and is ignored. | User, research |
| D175 | **Export filter.** With our own provider every span is exported (only our tracer and the instrumentors we bind create spans there). In global mode the default filter exports a span when its instrumentation scope is `llmobs` or `ai`, or one of its attribute keys starts with `llmobs.`, `gen_ai.`, `llm.`, `openinference.`, `ai.`, `traceloop.`, `langfuse.`, `lmnr.` or `gcp.vertex.agent.`, or is exactly `input.value` or `output.value`. A span without an instrumentation scope counts as not `llmobs`. `export_filter=` (TS `exportFilter`) replaces the rule in both modes; it gets the finished span and returns a boolean; if it raises, the span is exported. LLM spans whose parent was filtered out arrive as detached roots, which spec 01 already handles (`has_root = false`). | User, research (Langfuse) |
| D176 | **Masking and `capture_content`.** `mask` is applied at capture time to every value our SDK records (inputs, outputs, metadata values, LLM messages), before serialization. At export, an extra pass rewrites spans whose scope is **not** `llmobs`: for each attribute (span and event attributes) whose key is in the payload key list (§ Payload keys), `mask` gets the decoded value (JSON-parsed when the string is a JSON object or array, else the string) and the result is serialized back. With `capture_content=false`, our spans record no payload attributes and the export pass deletes those keys from other spans; the OpenInference instrumentors we create get their hide-inputs/hide-outputs options too. If `mask` raises, the value becomes `"[llmobs: mask failed]"` (fail closed) and one debug line is logged without the value. | User, research |
| D177 | **Instrumentation wiring.** Python: `instrument=["openai", …]` names from a fixed registry of OpenInference instrumentors (§ Python instrumentors); an unknown name or a missing package raises `ConfigError` naming the extra to install. TypeScript: `instrument: [...]` takes OTel instrumentation objects (anything with `setTracerProvider`, `enable` and `disable` functions); the subpath helpers `llmobs/openai`, `llmobs/anthropic`, `llmobs/langchain` build the OpenInference instrumentor and call `manuallyInstrument(module)`, so ESM works without loader flags. The OpenInference packages are optional peer dependencies. This replaces spec 10's TS `instrument: ["openai", "anthropic"]`. | User, research |
| D178 | **TypeScript transport.** One `FetchTraceExporter` on every runtime (Node, Bun, Deno, edge): OTLP protobuf from `@opentelemetry/otlp-transformer`, gzip through `CompressionStream` when the runtime has it, `Authorization: Bearer`, retries on 429/502/503/504 and network errors. OTel JS is pinned to the 2.12 line (`@opentelemetry/sdk-trace` 2.12.0), because OpenInference JS still requires OTel JS 2.x; OTel JS 3.0 waits until it moves. | User |
| D179 | **Conformance fixtures.** Each SDK scenario has a case `fixtures/otlp/llmobs/sdk-<scenario>/` whose `input.json` is the canonical OTLP both SDKs must emit (§ Conformance) and whose `expected.json` is the normalized result. Both SDK test suites run the scenario and compare their canonicalized export with `input.json`; the Rust golden test normalizes the same `input.json`. This changes D121's "spec 10's SDK integration tests cover it": the shared fixtures do, and the integration suite against a real `llmobs serve` waits for the server tracks. | User |
| D180 | **Payload encoding.** Our payloads use `llmobs.input`/`llmobs.output` with `llmobs.input.format`/`llmobs.output.format`: a `str` result is recorded as is with format `text`; any other value is serialized with the tolerant encoder (§ Serialization) to compact JSON with format `json`. Strings over 1 MiB (UTF-8 bytes) are cut at a character boundary and get the server's marker `…[llmobs: truncated, original N bytes]`; a cut JSON value becomes format `text`. The SDKs set the OTel attribute count limit to 10 000 on their provider (flattened OpenInference messages exceed the default 128). | Spec 10, research |
| D181 | **What `observe` records.** Python: the bound arguments with defaults applied, the first parameter dropped when it is named `self` or `cls`, `*args` as a list and `**kwargs` as an object under their parameter names. TypeScript: no arguments → no input; one argument → that value; more → a JSON array of them. Generator functions follow spec 10. They are detected when `observe` wraps the function: Python `inspect.isgeneratorfunction`/`isasyncgenfunction`; TypeScript `Object.prototype.toString.call(fn)` equal to `"[object GeneratorFunction]"` or `"[object AsyncGeneratorFunction]"`. Any other function is traced as a call, even when it returns a generator, an iterator, a stream or a promise of one: that call's output is not recorded when the (awaited) return value is a generator or async generator object, and the returned object is passed through untouched, never consumed. Other iterables and `ReadableStream`s are recorded as values by § Serialization. `capture_input` / `capture_output` (TS `captureInput` / `captureOutput`) default to true. | Research |
| D182 | **Context values live in the OTel context.** `context()` stores session id, user id, tags and metadata under one key of the OTel `Context`, so they follow `contextvars` / `AsyncLocalStorage` and `copy_context()`. Nested blocks merge: inner session/user win, tags are the union (outer first, no duplicates), metadata merges by key (inner wins). A span processor stamps them in `on_start` on every span of our provider, instrumentor spans included, together with `llmobs.environment` and `llmobs.release` from `init()`. Baggage stays opt-in (`propagate=True`). | Spec 10, research |
| D183 | **Manual LLM span attributes.** `llm_span` writes GenAI attributes: `gen_ai.operation.name = "chat"`, `gen_ai.provider.name`, `gen_ai.request.model`, `gen_ai.response.model`, `gen_ai.request.<param>`, `gen_ai.input.messages`, `gen_ai.output.messages`, `gen_ai.system_instructions`, the `gen_ai.usage.*` token counts (inputs include cache), `llmobs.cost.total` for a reported cost and `llmobs.ttft_ns`, plus `llmobs.span.kind = "llm"`. Messages may be given in the canonical format or as the short form `{"role", "content": "<text>"}`, which the SDK converts to one text part. | Spec 01, 02 |
| D184 | **Threads (Python).** The SDK does not patch `threading` or executors. The README documents `contextvars.copy_context().run(fn)` and OTel's threading instrumentor. | User |
| D185 | **Init is once per process.** Any `init()` call after the first (whatever the first one did: enabled, disabled, or missing key) logs one warning and changes nothing. Python re-creates the score queue's worker thread in a forked child (`os.register_at_fork`); OTel's batch processor already does the same for spans. | Research |
| D186 | **Browser feedback.** `llmobs/browser` exports only `feedback({ baseUrl, publicKey, traceId, spanId?, name?, value, comment? })`, which posts spec 06's end-user feedback body to `POST {baseUrl}/api/public/feedback` with `Authorization: Bearer <pk-llmobs-…>`. It imports nothing from OTel and resolves to `{ ok: boolean }`, never throwing. | User, spec 06 |

## Shared contract

Everything in this section is identical in both SDKs. Tests in P11 and T13 prove it through the conformance fixtures.

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
| `gen_ai.*`, `llmobs.cost.total`, `llmobs.ttft_ns` | see D183 | `llm_span` | § LLM span |

**Programming errors raise.** An invalid `kind` given to `observe`/`span`, or an invalid `provider`/`model` given to `llm_span`, raises `ValueError` (Python) / `TypeError` (TS) when the API is called (decoration time for `observe`), before the user function runs. These are the only cases where the SDK raises after `init()` (F1 amends spec 10 rule 4). Re-entering an already entered Python `context` object is a no-op logged at debug level.

**Errors.** When the user function (or the body of `span`/`llm_span`) raises, the SDK adds one event itself (it does not call OTel's `record_exception`/`recordException`, whose `exception.type` differs between languages): name `exception`, attributes `exception.type` (Python `type(e).__name__`; TS `e.name` for an `Error`, else `"Error"`), `exception.message` (Python `str(e)`; TS `e.message` for an `Error`, else `String(e)`), `exception.stacktrace` (Python `"".join(traceback.format_exception(e))`; TS `e.stack ?? ""`). The span status becomes error with message `"<exception.type>: <exception.message>"`. The exception is re-raised unchanged. A `GeneratorExit` (Python) or an early `return()` (TS) is not an error.

**Status.** The SDKs leave the status unset on success. The one exception is a generator closed early (spec 10), which ends with status ok.

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
7. `serialize` never raises. If an unexpected error escapes the rules above, the result is `("<unserializable TYPE>", "text")`.

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

### Score request

`POST <base_url>/api/v1/scores`, `Content-Type: application/json`, body:

```json
{"scores": [
  {"score_id": "0193…", "target": {"type": "span", "trace_id": "<32 hex>", "span_id": "<16 hex>"},
   "name": "accuracy", "value": {"data_type": "numeric", "value": 0.8},
   "source": "sdk", "reason": "…", "user_id": "u-17", "metadata": {"k": "v"}}
]}
```

- `target`: `{"type": "span", "trace_id", "span_id"}` when `span_id` is given (then `trace_id` is required), else `{"type": "trace", "trace_id"}` when `trace_id` is given, else `{"type": "session", "session_id"}`. Ids are lowercase hex; `session_id` must be a non-empty string.
- `value`: the existing `llmobs_core::ScoreValue` JSON (spec 01 § Score): `{"data_type":"numeric","value":0.8}`, `{"data_type":"boolean","value":true}`, `{"data_type":"categorical","value":{"label":"good"}}`, `{"data_type":"text","value":"…"}`; Python `bool` / TS `boolean` → `boolean`; `int`/`float` / `number` → `numeric` (must be finite); `str` / `string` → `categorical` with `{"label": s}`, or `text` when `data_type="text"` is passed. `data_type` may also force `numeric`, `boolean` or `categorical` and must match the value's type.
- `reason`, `user_id` (from the active context unless passed), `metadata` (string values only; other values are serialized to compact JSON) are omitted when absent.
- Invalid input: log the warning `llmobs: score not queued: <reason>` (no values) and return `None`/`undefined`; nothing is queued. Reasons, checked in this order: `empty name`, `no target`, `span_id without trace_id`, `invalid trace_id`, `invalid span_id`, `invalid session_id`, `invalid score_id`, `non-finite value`, `data_type mismatch`.
- `score()` returns the `score_id` string when queued.
- Response handling: `2xx` → done. `429`, any `5xx` and network errors/timeouts (10 s per attempt) → retry the same batch up to 3 more times, waiting `Retry-After` when it is a non-negative integer number of seconds (capped at 30), else 1 s, 2 s, 4 s (an HTTP-date `Retry-After` uses the default), then drop with `llmobs: score batch failed after retries; dropped <n> scores`. Any other status → drop at once with `llmobs: score batch rejected with status <code>; dropped <n> scores`.

### Conformance

`fixtures/otlp/llmobs/sdk-<scenario>/input.json` holds the **canonical OTLP** of one scenario. A test converts what the SDK exported into canonical form and compares it, as parsed JSON, with `input.json`.

Canonical form of an `ExportTraceServiceRequest`:
1. **Ids.** Spans are ordered by creation: the tests use a sequential id generator, so sorting by the original span id gives creation order. Trace ids and span ids are then **remapped by rank of first appearance** in that order: the k-th distinct trace id becomes `"%032x" % k`, the k-th span becomes `"%016x" % k` (k from 1). `parentSpanId` uses the same mapping and is omitted for roots (and when the parent is not among the exported spans).
2. **Times.** Span `n` (1-based creation index) gets `startTimeUnixNano = 1767225590000000000 + n·1 000 000` and `endTimeUnixNano = 1767225590000000000 + (1000 − n)·1 000 000`, as decimal strings (2025-12-31T23:59:50Z plus milliseconds). Every event of span `n` gets `timeUnixNano` = that span's end.
3. **Shape.** The whole document is exactly `{"resourceSpans": [{"resource": {"attributes": [...]}, "scopeSpans": [{"scope": {"name": "llmobs", "version": "0.0.0"}, "spans": [...]}]}]}` with nothing else at any level (no `schemaUrl`, no dropped counts). `resource.attributes` keep only `service.name` (the tests set `OTEL_SERVICE_NAME=conformance`) and `telemetry.distro.name`; the scope version is replaced by `"0.0.0"`.
4. **Span fields kept:** `traceId`, `spanId`, `parentSpanId` (non-roots), `name`, `kind` (integer; our spans are `1`, INTERNAL), `startTimeUnixNano`, `endTimeUnixNano`, `attributes`, `events` (omitted when empty), `status` (dropped when `code` is absent or 0; `message` dropped when empty or absent; so `{"code": 1}` ok, `{"code": 2, "message": …}` error). Everything else (`flags`, `traceState`, dropped counts, `links` when empty, `schemaUrl`) is removed.
5. **Attributes** (span, event, resource) are sorted by key. `AnyValue`s use the OTLP/JSON encoding: `stringValue`, `boolValue`, `intValue` as a decimal **string** (convert numbers), `doubleValue` as a number, `arrayValue: {"values": [...]}`.
6. **Exception events:** name `exception`; attributes reduced to `exception.message` and `exception.type`. The scenarios raise an error class named `ScenarioError` in both languages.
7. **Spans** are listed in creation order.

The canonicalizer is test code in each SDK (P11, T13); it is described here so both produce the same thing.

### Scenarios

Every scenario runs with `api_key="sk-llmobs-test"`, `base_url="http://127.0.0.1:9"` and no environment/release unless the row says otherwise, through the SDK's real pipeline with the inner OTLP exporter replaced by an in-memory one (P3, T4 define the test seam), then `flush()`. The TypeScript calls are spelled out under the table.

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

### Language differences

Known, accepted differences that the fixtures do not cover. The SDK READMEs list them.
- **Zero-argument functions and `None`/`undefined`** (above).
- **Integral floats.** JavaScript has one number type, and OTel JS encodes an integral number as `intValue`. So Python `params={"temperature": 1.0}`, `set_cost(1.0)` or metadata `2.0` give a double, TypeScript gives an int. Python always converts `set_cost`'s argument with `float()`. The server reads both (spec 02 rule 20).
- **Large integers.** Python ints beyond ±(2^53−1) inside JSON payloads are written as JSON numbers; TypeScript `bigint` becomes a decimal string. As attributes (metadata) both write a JSON string.
- **Resource attributes:** see § Names.
- **Exporter environment.** Python's `OTLPSpanExporter` also merges `OTEL_EXPORTER_OTLP_HEADERS` / `OTEL_EXPORTER_OTLP_TRACES_HEADERS` from the environment into its request headers, so headers meant for another OTLP backend are sent to llmobs too; TypeScript sends only ours. The Python README warns about it.
- **Abandoned generators.** A half-consumed Python sync generator that is garbage-collected ends its span with status ok; TypeScript (no finalizer) and Python async generators leave it unexported.

### Python instrumentors

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

`llmobs[all]` installs every one. P10 verifies the module and class names of `openai` and `anthropic` with real imports; the other eight are checked by P10's import test only when installed (§ P10).

### Dependency pins

Chosen on 2026-11-12, each at least 7 days old.

**Python** (`sdks/python/pyproject.toml`, runtime, lower bounds):
- `opentelemetry-api>=1.45.2,<2`, `opentelemetry-sdk>=1.45.2,<2`, `opentelemetry-exporter-otlp-proto-http>=1.45.2,<2` (it brings `requests` and `opentelemetry-proto`);
- `requests>=2.32` (used directly by the score transport);
- extras as in § Python instrumentors; `all` = every extra.
- dev group additions (P10): `openinference-instrumentation-openai>=0.1.69`, `openinference-instrumentation-anthropic>=3.3.0`, `openai>=2`, `anthropic>=0.70` — the implementer runs `uv add --dev` and records the resolved versions in the report.

**TypeScript** (`sdks/typescript/package.json`, exact versions):
- `dependencies`: `@opentelemetry/sdk-trace` `2.12.0`, `@opentelemetry/core` `2.12.0`, `@opentelemetry/resources` `2.12.0`, `@opentelemetry/context-async-hooks` `2.12.0`, `@opentelemetry/otlp-transformer` `0.223.0`;
- `peerDependencies`: `@opentelemetry/api` `>=1.9.0 <1.10.0` (one API instance per app; `sdk-trace` 2.12 requires `<1.10.0`); devDependency `@opentelemetry/api` `1.9.1` (`pnpm add --save-peer` writes the range into `devDependencies` too; set that entry to the exact `1.9.1` by hand);
- optional `peerDependencies` (T11, `peerDependenciesMeta.<name>.optional = true`): `@arizeai/openinference-instrumentation-openai` `^4.4.3`, `@arizeai/openinference-instrumentation-anthropic` `^1.0.0`, `@arizeai/openinference-instrumentation-langchain` `^4.1.5`, and the libraries the helpers' types reference: `openai` `^7.0.0`, `@anthropic-ai/sdk` `^1.0.0`, `@langchain/core` `^1.0.0`; all six as exact devDependencies `4.4.3`, `1.0.0`, `4.1.5`, `7.50.0`, `1.5.0`, `1.3.4` for the helper tests.
- Note for the T1 report: `@opentelemetry/otlp-transformer` 0.223.0 also pulls `@opentelemetry/sdk-logs`, `sdk-metrics` and `api-logs`; report the size of the built `dist/index.js` and of its dependency closure.

Peer ranges are the only non-exact versions (a peer needs a range); say so in the T1 and T11 reports.

## Requests to other tracks

| # | To | What |
|---|---|---|
| R1 | The server track that adds score writes (coordinator assigns) | Implement D171: `POST /api/v1/scores` for `sk` keys with the `ingest` scope, body § Score request, 1–1000 items, `200` on success; add the request types (`ScoreCreate`, `ScoreBatch`) to `llmobs-core`'s OpenAPI with a `fixtures/api` example identical to the body above. Until then the SDKs are tested against mocks only. |
| R2 | Coordinator | Spec 06 § Scores: SDK scores carry a client id (D172); the deterministic id applies only when `score_id` is omitted. F1 writes the spec text; the server track implements it. |

## Repository conventions this plan relies on

- `just check-py` = `uv sync --locked`, `ruff format --check`, `ruff check`, `mypy` (strict), `pytest` (warnings are errors).
- `just check-ts` = Biome, `tsc` (two configs), Vitest, `tsdown`, `publint`, `attw --profile esm-only`.
- Rust golden tests read `fixtures/otlp/**` (`cargo test -p llmobs-otel`), and `just fixtures-check` validates fixtures.
- Logs and error messages never contain payloads, keys, emails or attribute values (AGENTS.md § Privacy). Every log line in this plan lists what it may contain.
