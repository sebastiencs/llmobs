# F · TypeScript track (T1–T13)

See [README.md](README.md) for the decisions and the shared contract; every "§" below refers to it unless it names another file. Every T-sub-phase edits only `sdks/typescript/` (T1 and T11 also `pnpm-lock.yaml`) and runs `just check-ts`, then `just check`. The Python track (P1–P12) runs in parallel and implements the same behaviour; when this file says "as Python", the README section is the source, not the Python code.

**Source layout at the end of the track** (`sdks/typescript/src/`):

| File | Content | Sub-phase |
|---|---|---|
| `index.ts` | public API re-exports, `VERSION` | T1, then each sub-phase adds its names |
| `config.ts` | configuration resolution (exists) | T1 |
| `log.ts` | `warnOnce`, `debug` | T1 |
| `web.ts` | structural types for `fetch`, `Response`, `CompressionStream`, and getters on `globalThis` | T1 |
| `serialize.ts` | § Serialization, attribute value conversion | T2 |
| `exporter.ts` | `FetchTraceExporter` | T3 |
| `runtime.ts` | `Runtime`, `init`, `flush`, `shutdown`, `tracerProvider`, `spanProcessor`, test seam | T4 |
| `processor.ts` | `LlmobsSpanProcessor` | T4, T5, T9 |
| `context.ts` | `withContext`, the context key, merge rules, baggage | T5 |
| `span.ts` | `SpanHandle`, `withSpan`, `observe`, `currentTraceId`, `currentSpanId` | T6 |
| `generators.ts` | traced generator and async generator wrappers | T7 |
| `llm.ts` | `LlmSpanHandle`, `withLlmSpan`, messages conversion | T8 |
| `mask.ts` | capture-time `applyMask` and `MaskingExporter` | T5, T9 |
| `filter.ts` | default export filter | T9 |
| `scores.ts`, `uuid.ts` | `score()`, the score queue, UUIDv7 | T10 |
| `instrument.ts`, `ai.ts` | instrumentation objects, `aiTelemetry` | T11 |
| `openai.ts`, `anthropic.ts`, `langchain.ts` | subpath helpers | T11 |
| `browser.ts` | `feedback()` | T12 |

Tests are `src/<file>.test.ts` (Vitest), plus `src/testing.test-helpers.ts` (T4; excluded from the build because no entry imports it) and `src/conformance.test.ts` (T13). Tests never hit the network: HTTP goes through an injected `fetch` function.

**Rules for every T-sub-phase:**
- Strict TypeScript (`noUncheckedIndexedAccess` is on); no `any` except where a sub-phase allows it with a comment; no `@ts-ignore`.
- Nothing raises into the app after `init()` (spec 10 rule 4): API entry points wrap their own work in `try/catch`, log with `debug("…<ErrorName>")`, and continue with a no-op. User code exceptions are always re-thrown unchanged.
- Never log values, keys, user ids or error messages from user code.
- No `node:` imports in `src/` (the only Node-specific module used is `@opentelemetry/context-async-hooks`, which imports `async_hooks` itself). T13 adds a test for this.

## T1 — Dependencies, entry points, missing-key change, runtime globals, logging

**Commit:** `sdk-ts: add OpenTelemetry dependencies and warn on a missing key`

**Edit:** `sdks/typescript/package.json`, `sdks/typescript/tsdown.config.ts`, `sdks/typescript/tsconfig.json`, `pnpm-lock.yaml`, `src/config.ts`, `src/config.test.ts`, `src/log.ts` (new), `src/log.test.ts` (new), `src/web.ts` (new), `src/web.test.ts` (new), `src/index.ts`.

1. **Dependencies** (§ Dependency pins): `dependencies` `@opentelemetry/sdk-trace` `2.12.0`, `@opentelemetry/core` `2.12.0`, `@opentelemetry/resources` `2.12.0`, `@opentelemetry/context-async-hooks` `2.12.0`, `@opentelemetry/otlp-transformer` `0.223.0`; `peerDependencies` `@opentelemetry/api` `">=1.9.0 <1.10.0"`; devDependency `@opentelemetry/api` `1.9.1`. Install with `pnpm --dir sdks/typescript add --save-exact …` (and `--save-peer` for the peer); check `pnpm install --frozen-lockfile` still works from the root. If pnpm's `minimumReleaseAge` rejects a version, stop and report.
2. **Entry points.** The final `package.json` `exports` will be `"."`, `"./openai"`, `"./anthropic"`, `"./langchain"`, `"./browser"`, each `{ "types": "./dist/<name>.d.ts", "default": "./dist/<name>.js" }`. In T1 keep only `"."`; T11 adds the three helpers and T12 adds `"./browser"`, each together with its `tsdown` entry. Change `tsdown.config.ts` `entry` to an array now (`["src/index.ts"]`).
3. **`web.ts` — runtime globals.** The main `tsconfig.json` has `lib: ["ES2023"]` and `types: []`, so no DOM or Node global is declared, not even `console`, `TextEncoder` or `setTimeout`. Do not add the DOM lib. `web.ts` declares structural types for exactly what the SDK uses and one accessor:
   ```ts
   export interface RuntimeGlobals {
     console?: { warn(message: string): void };
     TextEncoder?: new () => { encode(text: string): Uint8Array };
     TextDecoder?: new (label?: string, options?: { fatal?: boolean }) => { decode(bytes: Uint8Array): string };
     setTimeout?: (fn: () => void, ms: number) => unknown;
     clearTimeout?: (handle: unknown) => void;
     setInterval?: (fn: () => void, ms: number) => unknown;   // the handle may have unref()
     clearInterval?: (handle: unknown) => void;
     crypto?: { getRandomValues<T extends Uint8Array>(array: T): T };
     fetch?: FetchLike;
     AbortSignal?: { timeout?(ms: number): unknown };
     CompressionStream?: new (format: "gzip") => unknown;
     DecompressionStream?: new (format: "gzip") => unknown;
     Response?: new (body: unknown) => { arrayBuffer(): Promise<ArrayBuffer>; body: unknown };
     Blob?: new (parts: Uint8Array[]) => { stream(): { pipeThrough(transform: unknown): unknown } };
     process?: { once?(event: string, listener: () => void): unknown; env?: Record<string, string | undefined> };
   }
   export function runtimeGlobals(): RuntimeGlobals;   // globalThis as unknown as RuntimeGlobals
   export type FetchLike = (url: string, init: { method: string; headers: Record<string, string>; body: Uint8Array | string; signal?: unknown; keepalive?: boolean }) => Promise<ResponseLike>;
   export interface ResponseLike { status: number; ok: boolean; headers: { get(name: string): string | null } }
   export function getFetch(): FetchLike | undefined;          // fetch bound to globalThis
   export function gzip(bytes: Uint8Array): Promise<Uint8Array | undefined>;   // undefined when CompressionStream/Response/Blob are missing
   export function timeoutSignal(ms: number): unknown;          // AbortSignal.timeout(ms) or undefined
   export function utf8Encode(text: string): Uint8Array;        // TextEncoder
   export function utf8Decode(bytes: Uint8Array): string;       // TextDecoder
   ```
   Every other file reaches globals only through `web.ts`. `config.ts`'s `defaultEnv()` switches to `runtimeGlobals().process?.env`. Tests (`web.test.ts`): `gzip` round-trips through `DecompressionStream`; `getFetch` is `undefined` when `globalThis.fetch` is temporarily removed.
4. **`log.ts`:** `export function warnOnce(key: string, message: string): void` (a module-level `Set`; `runtimeGlobals().console?.warn(message)` the first time); `export function debug(message: string): void` (`diag.debug(message)` from `@opentelemetry/api`); `export function resetWarningsForTests(): void`.
5. **`tsconfig.json`:** add `"src/**/*.test-helpers.ts"` to `exclude` (test helpers are type-checked by `tsconfig.test.json` only).
6. **`config.ts` changes:**
   - `Config` gains `serviceName: string | undefined`, read from `OTEL_SERVICE_NAME` in the same env source (trimmed, empty = unset); `undefined` when disabled.
   - `instrument`: remove the option from `LlmobsOptions` and `Config` for now (its string form is replaced by D-F-7; T11 adds it back with the new type). Remove its tests.
   - **D-F-3:** a missing API key → `warnOnce("missing-api-key", "llmobs: no API key; set LLMOBS_API_KEY, or LLMOBS_ENABLED=false to silence this; tracing is disabled")` and the disabled config. A wrong prefix still throws.
7. **Tests:** update `config.test.ts` for the missing-key change, `serviceName` and the removed `instrument`; `log.test.ts` for `warnOnce` (spy on `console.warn`).

**Checks:** `just check-ts`, `just check`.

## T2 — Serialization

**Commit:** `sdk-ts: add the tolerant payload serializer`

**Edit:** `src/serialize.ts` (new), `src/serialize.test.ts` (new).

```ts
export type Format = "text" | "json";
export const MAX_PAYLOAD_BYTES = 1_048_576;
export const MAX_DEPTH = 32;
export function serialize(value: unknown): [string, Format];
export function toJsonable(value: unknown): unknown;
export function truncate(text: string, format: Format): [string, Format];
export function attributeValue(value: unknown): string | boolean | number;
```

- `toJsonable`: § Serialization rule 2, TypeScript column, in this order: `null`; `undefined` (handled by the caller per position); `boolean`; `number` (finite → itself; `NaN`/`±Infinity` → the strings); `bigint` → `value.toString()`; `string`; `Date`; `ArrayBuffer` and byte views (`ArrayBuffer.isView(v) && v.BYTES_PER_ELEMENT === 1`) → data URI (base64 via a small local encoder; do not use `Buffer`); `Error` → `{type, message}`; `Map` → object; `Set` → array; `Array` → array (`undefined` items → `null`); objects with a callable `toJSON` → `toJSON()` then recurse; plain objects (`Object.getPrototypeOf(v) === Object.prototype || null`) → own enumerable string keys, `undefined` values omitted; non-byte typed arrays (`ArrayBuffer.isView(v)` with `BYTES_PER_ELEMENT > 1`) → `Array.from(v)`, bigint elements as strings; a `DataView` → the data URI of its bytes (check `DataView` before the typed-array branch); class instances → own enumerable string keys like plain objects; `function`/`symbol` → omitted as an object property, `null` as an array item, `String(value)` at the top level. Depth and cycle markers as rules 3–4 (track a `Set<object>` of the current path). Place the byte-view and typed-array branches before the generic object branch.
- `serialize`: top-level `string` → `truncate(value, "text")`; top-level `undefined` → `["null", "json"]`; else `JSON.stringify(toJsonable(value))` → `truncate(text, "json")`. Any exception → `["<unserializable " + typeName + ">", "text"]` where `typeName` is `value?.constructor?.name ?? typeof value`.
- `truncate`: measure with `utf8Encode(text)`; cut at `MAX_PAYLOAD_BYTES`, then step back while the byte at the cut is a UTF-8 continuation byte (`(b & 0xc0) === 0x80`), decode with `utf8Decode`, append `…[llmobs: truncated, original ${n} bytes]`, format `text`.
- `attributeValue`: `string` → truncated text; `boolean`; `number` integral with `Number.isSafeInteger` → itself; finite non-integral → itself; else `serialize(v)[0]`.

**Tests:** the same table as P2 adapted to TS types (including `undefined` in objects/arrays/top level, `bigint`, `Map` with non-string keys, a class instance, a `toJSON` object, cyclic object/array, a shared reference, depth 32/33, exact-limit and over-limit strings with 2-byte and 4-byte characters at the cut, `Uint8Array`, `ArrayBuffer`, `Int16Array` and `Float64Array` (non-byte typed arrays → an array of their numbers, § Serialization), `Error` subclass, a getter that throws → `"<unserializable X>"`).

**Checks:** `just check-ts`, `just check`.

## T3 — Fetch exporter

**Commit:** `sdk-ts: add the fetch-based OTLP exporter`

**Edit:** `src/exporter.ts` (new), `src/exporter.test.ts` (new).

```ts
export interface FetchTraceExporterOptions {
  url: string;                       // <baseUrl>/v1/traces
  apiKey: string;
  fetch?: FetchLike;                 // tests; default getFetch()
  timeoutMillis?: number;            // per attempt, default 10_000
  sleep?: (ms: number) => Promise<void>; // tests; default setTimeout-based
  now?: () => number;                // tests; default Date.now
}
export class FetchTraceExporter implements SpanExporter {
  constructor(options: FetchTraceExporterOptions);
  export(spans: ReadableSpan[], resultCallback: (result: ExportResult) => void): void;
  forceFlush(): Promise<void>;       // resolves when every in-flight export settled
  shutdown(): Promise<void>;         // forceFlush, then refuse new exports
}
```

- Body: `ProtobufTraceSerializer.serializeRequest(spans)` from `@opentelemetry/otlp-transformer`; `undefined` → callback `FAILED` with an `Error("serialize")`.
- Headers: `Content-Type: application/x-protobuf`, `Authorization: Bearer <apiKey>`; when `gzip(body)` returns bytes, send them with `Content-Encoding: gzip`.
- Attempts: up to 4 (1 + 3 retries), within a **total deadline of 30 s** per `export` call: each attempt's timeout is `min(timeoutMillis, time left)`, and a retry whose wait would end past the deadline is not made (→ `FAILED`). Retry on status 429, 502, 503, 504 and on a rejected `fetch` (network error or timeout). Wait `Retry-After` seconds when the header is a non-negative integer (cap 30), else 1 s, 2 s, 4 s. Tests inject a clock (`now?: () => number`, default `Date.now`) next to `sleep`. Any other non-2xx → `FAILED` without retry. `2xx` → `SUCCESS` (the body is not read; partial success is not reported in the MVP).
- `fetch` missing at construction → every export fails with `FAILED` and `warnOnce("no-fetch", "llmobs: fetch is not available; spans are not exported")`.
- Never throws from `export`; the callback is called exactly once per call.
- `keepalive: true` is **not** set (bodies may exceed the 64 KiB keepalive limit).

**Tests** (inject a fake `fetch` that records calls and answers from a script; inject `sleep` that records durations): success path headers and URL; the body, gunzipped with `DecompressionStream`, is byte-identical to `ProtobufTraceSerializer.serializeRequest(spans)`; gzip absent → no `Content-Encoding` (stub `gzip` via a `globalThis.CompressionStream = undefined` override restored after the test); 503 → 503 → 200 = 3 calls and sleeps `[1000, 2000]`; `Retry-After: 3` → sleep 3000; `Retry-After: 120` → 30000; 400 → one call, `FAILED`; network rejection retried; 4 failures → `FAILED`; `forceFlush` waits for an in-flight export; `shutdown` then `export` → `FAILED`; no fetch → `FAILED` and the warning once. Build `ReadableSpan`s with an `InMemorySpanExporter` from a throwaway `TracerProvider`.

**Checks:** `just check-ts`, `just check`.

## T4 — Runtime, provider, lifecycle

**Commit:** `sdk-ts: add init, the provider and the lifecycle`

**Edit:** `src/runtime.ts` (new), `src/processor.ts` (new), `src/index.ts`, `src/testing.test-helpers.ts` (new), `src/runtime.test.ts` (new).

The processor's `BatchSpanProcessor` uses `exportTimeoutMillis: 45_000` (above the exporter's 30 s deadline, so `flush()` waits for retries).

**`processor.ts`** — `class LlmobsSpanProcessor implements SpanProcessor`: constructor `(exporter: SpanExporter, options: { environment?: string; release?: string; exportFilter?: (s: ReadableSpan) => boolean; defaultFilter: boolean })`; owns `new BatchSpanProcessor({ exporter, scheduledDelayMillis: 1000, exportTimeoutMillis: 45_000 })`; `onStart(span, parentContext)` sets `llmobs.environment`/`llmobs.release` (T5 adds context values); `onEnd(span)` forwards (T9 adds the filter); `forceFlush()`/`shutdown()` delegate. Each method catches and logs per the rules.

**`runtime.ts`:**

```ts
export class Runtime {
  readonly config: Config;
  readonly provider: TracerProvider;     // the sdk-trace class
  readonly ownsProvider: boolean;
  readonly processor: LlmobsSpanProcessor;
  readonly tracer: Tracer;               // provider.getTracer("llmobs", VERSION)
  static create(config: Config, seams?: { exporter?: SpanExporter; idGenerator?: IdGenerator; fetch?: FetchLike }): Runtime;
  flush(): Promise<void>;
  shutdown(): Promise<void>;             // idempotent
}
export function init(options?: LlmobsOptions): void;
export function flush(): Promise<void>;
export function shutdown(): Promise<void>;
export function tracerProvider(): ApiTracerProvider;   // `new ProxyTracerProvider()` (no delegate → no-op) when not running
export function spanProcessor(): SpanProcessor | undefined;
export function currentRuntime(): Runtime | undefined; // internal
export function installRuntime(runtime: Runtime): void; // internal + test seam
export function resetForTests(): Promise<void>;       // see below
```

- `Runtime.create`, own mode: exporter `seams.exporter ?? new FetchTraceExporter({ url: `${baseUrl}/v1/traces`, apiKey, fetch: seams.fetch })`; resource `defaultResource().merge(resourceFromAttributes({ "telemetry.distro.name": "llmobs", "telemetry.distro.version": VERSION, ...(config.serviceName ? { "service.name": config.serviceName } : {}) }))`; sampler `new ParentBasedSampler({ root: new TraceIdRatioBasedSampler(config.sampleRate) })`; `spanLimits: { attributeCountLimit: 10_000 }`; `idGenerator: seams.idGenerator` when given; `spanProcessors: [processor]`. Never register it globally (global mode comes in T9).
- **Context manager:** on **every** `Runtime.create`, build `manager = new AsyncLocalStorageContextManager()` and call `context.setGlobalContextManager(manager.enable())`. If it returns `false` (one is already registered: the app's, or ours from an earlier runtime), call `manager.disable()` and keep the registered one (the API also logs a diag error "Attempted duplicate registration"; that is expected). Never disable the registered manager.
- `resetForTests()`: `await` shutdown of the current runtime, then `context.disable()`, `trace.disable()`, `propagation.disable()`, clear `warnOnce`, the init-called flag and the instrument list. It never clears the `beforeExit` flag.
- `init(options)`: if the module-level init-called flag is set → `warnOnce("init-twice", "llmobs: init() was already called; this call is ignored")` and return; set the flag first, so a disabled first `init` also counts (D-F-15); `resolveConfig` (throws `ConfigError`); disabled → return; `installRuntime(Runtime.create(config))`; then, once per process (a module-level flag), when `runtimeGlobals().process?.once` is a function, `process.once("beforeExit", () => void shutdown())`.
- `flush()` resolves after `processor.forceFlush()` (T10 adds scores); never rejects. `shutdown()`: flush, `processor.shutdown()`, and `provider.shutdown()` when owned; idempotent; afterwards `currentRuntime()` is `undefined` while a second `init` is still refused.
- `testing.test-helpers.ts`: `class SequentialIdGenerator implements IdGenerator` (trace ids and span ids from counters starting at 1, formatted per § Conformance); `startMemory(options?: Partial<LlmobsOptions>, env?: Record<string, string>)` → `{ exporter: InMemorySpanExporter; runtime: Runtime }` building `resolveConfig({ apiKey: "sk-llmobs-test", baseUrl: "http://127.0.0.1:9", ...options }, { OTEL_SERVICE_NAME: "conformance", ...env })`, then `Runtime.create(config, { exporter, idGenerator: new SequentialIdGenerator() })` and `installRuntime`; `finished(exporter)` → `await flush()` then `exporter.getFinishedSpans()`. Every test file calls `await resetForTests()` in `beforeEach` and `afterEach`.

**Tests:** as P3's list where it applies: disabled/no key; provider not global (`trace.getTracerProvider()` is still the proxy); scope name and version; resource attributes incl. `service.name = conformance`; environment/release stamping; `sampleRate: 0`; second `init` warns, also after a disabled first `init`; `shutdown` idempotent and spans after it dropped; the `beforeExit` listener registered once (spy on `process.once`); the context manager registered, and an existing one kept (register a `AsyncLocalStorageContextManager` first, then `init`, and check `context.with` still uses it); an end-to-end export through `FetchTraceExporter` with an injected `fetch` (pass `seams.fetch`) proving URL, auth and gzip.

**Checks:** `just check-ts`, `just check`.

## T5 — `withContext` and the context processor

**Commit:** `sdk-ts: add withContext and stamp context values on spans`

**Edit:** `src/context.ts` (new), `src/mask.ts` (new), `src/processor.ts`, `src/index.ts`, `src/context.test.ts` (new), `src/mask.test.ts` (new).

`mask.ts`: `applyMask(value: unknown): unknown` as Python's `_mask.apply` (P4): the running runtime's `mask`, the marker `"[llmobs: mask failed]"` when it throws, the value unchanged without a mask. Tests: no runtime, no mask, a mask, a throwing mask.

```ts
export interface ContextOptions {
  sessionId?: string; userId?: string; tags?: readonly string[];
  metadata?: Readonly<Record<string, unknown>>; propagate?: boolean;
}
export function withContext<T>(options: ContextOptions, fn: () => T): T;
export function currentValues(ctx?: Context): ContextValues;   // internal
```

- Same merge rules, validation (invalid values dropped silently, a string `tags` treated as one tag), mask on metadata values, baggage keys and on-start stamping as P4. The values are stored with `ctx.setValue(CONTEXT_KEY, merged)`; with `propagate`, `propagation.setBaggage(ctx, baggage)` where the baggage is the existing one (`propagation.getBaggage(ctx) ?? propagation.createBaggage()`) plus `llmobs.session.id`/`llmobs.user.id` entries. `fn` runs inside `context.with(ctx, fn)`; its return value (a promise included) is returned unchanged.

**Tests:** as P4 adapted: stamping on our spans and on a "third-party" tracer of our provider; nesting; isolation of two concurrent async flows (`Promise.all` of two `withContext` calls each awaiting a timer); baggage only with `propagate` and only the two keys; values read from baggage alone; invalid input dropped; metadata attribute types (`3` → int, `{a:1}` → JSON string, bad key skipped); the processor never throws on garbage under the key.

**Checks:** `just check-ts`, `just check`.

## T6 — `withSpan` and `observe`

**Commit:** `sdk-ts: add withSpan and observe`

**Edit:** `src/span.ts` (new), `src/index.ts`, `src/span.test.ts` (new), `src/observe.test.ts` (new).

```ts
export type SpanKind = "llm" | "embedding" | "tool" | "agent" | "chain" | "retriever" | "reranker" | "guardrail" | "evaluator" | "span";
export interface SpanHandle {
  readonly traceId: string | undefined;
  readonly spanId: string | undefined;
  setInput(value: unknown): void;
  setOutput(value: unknown): void;
  setMetadata(values: Readonly<Record<string, unknown>>): void;
}
export interface SpanOptions { kind?: SpanKind; input?: unknown; metadata?: Readonly<Record<string, unknown>> }
export function withSpan<T>(name: string, fn: (span: SpanHandle) => T): T;
export function withSpan<T>(name: string, options: SpanOptions, fn: (span: SpanHandle) => T): T;
export interface ObserveOptions { name?: string; kind?: SpanKind; captureInput?: boolean; captureOutput?: boolean }   // T7 adds reduceOutput
export function observe<A extends unknown[], R>(fn: (...args: A) => R, options?: ObserveOptions): (...args: A) => R;
export function currentTraceId(): string | undefined;
export function currentSpanId(): string | undefined;
```

- **Kind check:** an invalid `kind` → `TypeError("kind: must be one of …")` thrown by `observe(...)`/`withSpan(...)` before the user function runs. `observe` checks at wrap time.
- **No runtime at call time:** `withSpan` calls `fn` with a no-op handle; the `observe` wrapper calls `fn` directly.
- **`withSpan`'s return value is never recorded as output**; only `setOutput` writes it.
- **Span lifetime:** start with `runtime.tracer.startSpan(name, undefined, context.active())`, run the body in `context.with(trace.setSpan(context.active(), span), …)`. If the body returns a thenable, end when it settles and return a promise of the same value (`.then(onOk, onErr)`, where `onErr` records the error and re-throws); if it throws synchronously, record and re-throw; else end immediately.
- **`observe`:** returns `function (this: unknown, ...args: A): R` calling `fn.apply(this, args)`. Name: `options.name ?? (fn.name || "anonymous")`. Input per D-F-11 (no args → none; one → that value; more → the array); output: the return value or resolved value; `undefined` → none; a generator or async generator object → none (D-F-11), returned untouched; T7 handles generator functions.
- Errors, status and metadata rules per the README.

**Tests:** as P5 adapted (kind; no-runtime pass-through and late `init`; inputs for 0/1/2 args; `this` preserved on a method; sync throw; async reject; promise identity is not required but the resolved value is; nested parent/child; concurrent async siblings; `captureInput`/`captureOutput`/`captureContent: false`; mask and failing mask; `setMetadata`; handle after end ignored; `currentTraceId`).

**Checks:** `just check-ts`, `just check`.

## T7 — Generators and async generators

**Commit:** `sdk-ts: trace generators and async generators`

**Edit:** `src/generators.ts` (new), `src/span.ts`, `src/generators.test.ts` (new).

- `ObserveOptions` gains `reduceOutput?: (items: unknown[]) => unknown`.
- **Detection at wrap time (D-F-11):** `Object.prototype.toString.call(fn)` is `"[object GeneratorFunction]"` → generator mode; `"[object AsyncGeneratorFunction]"` → async generator mode; anything else → the T6 call wrapper (whose output rule already skips generator objects). In generator mode the wrapper creates **no** span at call time: it calls `fn.apply(this, args)` (which runs none of the body), captures the input from `args`, and returns `traceGenerator(gen, …)` / `traceAsyncGenerator(gen, …)`. With no runtime at call time it returns `gen` unwrapped.
- `traceGenerator` is a `function*` whose body: starts the span (parent = the context active at the first `next()`), loops `const r = context.with(spanCtx, () => inner.next(sent))`, pushes `r.value` when not done and capture is on, `yield`s it and receives the next `sent`; on `done` ends with status unset and the output, returning `r.value`. A `throw` delivered by the consumer at the `yield` is forwarded with `inner.throw(e)` inside `context.with`; an exception from `inner` → error per README, end, re-throw. Early `return()` by the consumer reaches the wrapper's `finally`: call `inner.return(undefined)` inside `context.with`, end with status ok and the items so far. `traceAsyncGenerator` is the `async function*` equivalent with `await`.
- Output rule and `reduceOutput` as P6.
- Abandoned generators (never finished, never `return()`ed) are never exported; document it in JSDoc and README (TypeScript has no reliable finalizer for this).

**Tests:** as P6 adapted (start at first `next`; the generator's span is current only inside its body; exhaustion; `break` in `for…of` → ok and partial output; `for await…of` with `break`; exception in body; `gen.throw()`; non-string items; `reduceOutput`; capture off; a plain function returning a generator object → a call span without output, the generator untouched; an `async` function resolving to an async generator → the same; an `AsyncIterable` that is not a generator object, and a `ReadableStream`, are returned unchanged and not consumed; they are recorded as values by § Serialization, so a `ReadableStream` (a class instance with no own enumerable keys) is recorded as `{}` — assert exactly that).

**Checks:** `just check-ts`, `just check`.

## T8 — `withLlmSpan`

**Commit:** `sdk-ts: add the manual LLM span`

**Edit:** `src/llm.ts` (new), `src/index.ts`, `src/llm.test.ts` (new).

```ts
export interface LlmSpanOptions {
  provider: string; model: string;
  inputMessages?: readonly unknown[]; systemInstructions?: string | readonly unknown[];
  params?: Readonly<Record<string, unknown>>; metadata?: Readonly<Record<string, unknown>>;
}
export interface Usage { inputTokens?: number; outputTokens?: number; cacheReadTokens?: number; cacheWriteTokens?: number; reasoningTokens?: number }
export interface LlmSpanHandle extends SpanHandle {
  setInputMessages(messages: readonly unknown[]): void;
  setOutputMessages(messages: readonly unknown[]): void;
  setResponseModel(model: string): void;
  setUsage(usage: Usage): void;
  setCost(totalUsd: number): void;
  recordFirstToken(): void;
}
export function withLlmSpan<T>(name: string, options: LlmSpanOptions, fn: (span: LlmSpanHandle) => T): T;
```

Exactly § LLM span, as P7 (token counts must be `Number.isSafeInteger` and ≥ 0; `provider`/`model` invalid → `TypeError` before `fn` runs). `recordFirstToken` uses `hrTimeToNanoseconds` over `hrTimeDuration(span.startTime, now)` — read the span start through the sdk span object (`(span as unknown as ReadableSpan).startTime`; allowed cast with a comment).

**Tests:** as P7 adapted, including the `llm-span` scenario's attribute set.

**Checks:** `just check-ts`, `just check`.

## T9 — Masking pass, export filter, global mode

**Commit:** `sdk-ts: mask instrumentor payloads, filter exports, support the global provider`

**Edit:** `src/config.ts`, `src/config.test.ts`, `src/mask.ts`, `src/filter.ts` (new), `src/processor.ts`, `src/runtime.ts`, `src/mask.test.ts`, `src/filter.test.ts` (new), `src/runtime.test.ts`.

- **Options.** `Config` and `LlmobsOptions` gain `useGlobalProvider` (boolean, default `false`, no env var; non-boolean → `ConfigError("useGlobalProvider", "must be a boolean")`) and `exportFilter` (`(span: ReadableSpan) => boolean`; not a function → `ConfigError("exportFilter", "must be callable")`; type from `@opentelemetry/sdk-trace`). Disabled config: `false` / `undefined`. Tests in `config.test.ts`.

- **`MaskingExporter implements SpanExporter`**, installed when `config.mask` or `!config.captureContent`: same rules as P8 (a span counts as not `llmobs` when its `instrumentationScope.name` differs). A rewritten span is a new object literal implementing `ReadableSpan` field by field (`name`, `kind`, `spanContext: () => s.spanContext()`, `parentSpanContext`, `startTime`, `endTime`, `status`, `attributes: <new>`, `links`, `events: <new TimedEvent[]>`, `duration`, `ended`, `resource`, `instrumentationScope`, `droppedAttributesCount`, `droppedEventsCount`, `droppedLinksCount`); never spread the original (its `spanContext` is a prototype method). On an unexpected error: fail closed (drop the batch, `FAILED`, warning `llmobs: masking failed; dropped <n> spans`).
- **Filter**: `defaultFilter(span: ReadableSpan): boolean` per D-F-5; `onEnd` applies `exportFilter` / `defaultFilter` as P8.
- **Global mode** (D-F-4): an app provider exists unless `trace.getTracerProvider()` is a `ProxyTracerProvider` whose `getDelegate()` is identical (`===`) to `new ProxyTracerProvider().getDelegate()` (the API's shared no-op provider; never compare constructor names, they break under minification). When one exists: `warnOnce("global-provider-exists", "llmobs: a global tracer provider is already registered; llmobs keeps its own provider. Add spanProcessor() to your provider to send its spans to llmobs")` and continue in own mode with `defaultFilter: false`. Otherwise build ours with `defaultFilter: true` and `trace.setGlobalTracerProvider(provider)`.

**Tests:** as P8 adapted. Global-mode tests reset the API globals between tests with `trace.disable()` and `context.disable()` in `afterEach` (public API), then re-register what the test needs.

**Checks:** `just check-ts`, `just check`.

## T10 — Scores

**Commit:** `sdk-ts: add score() with a background queue`

**Edit:** `src/scores.ts` (new), `src/uuid.ts` (new), `src/runtime.ts`, `src/index.ts`, `src/scores.test.ts` (new), `src/uuid.test.ts` (new).

```ts
export interface ScoreOptions {
  name: string; value: boolean | number | string;
  traceId?: string; spanId?: string; sessionId?: string; reason?: string;
  dataType?: "numeric" | "boolean" | "categorical" | "text";
  metadata?: Readonly<Record<string, unknown>>; userId?: string; scoreId?: string;
}
export function score(options: ScoreOptions): string | undefined;
export function uuid7(): string;
```

- Body, validation (reasons and warning texts), return value and response handling: § Score request (JSON keys are snake_case as written there).
- `uuid7()`: RFC 9562 layout from `Date.now()` and `crypto.getRandomValues` (`globalThis.crypto`; if missing, `score()` warns once and returns `undefined`).
- `ScoreQueue` (owned by `Runtime`, gets the same `fetch` seam): buffer up to 10 000 (drop and count beyond, warning at most once per minute with the count); a timer `setInterval(tick, 1000)` started lazily, `unref()`ed when the returned handle has `unref`; a tick sends one batch of up to 1000 when no send is in flight; `flush()` sends batches until empty and awaits them; `shutdown()` flushes then clears the timer. Retries with the injected `sleep`.
- `Runtime.flush`/`shutdown` include the queue.

**Tests:** as P9 adapted (no fork test): body per target/value type, `dataType`, invalid inputs, `userId` from `withContext`, mask on `reason`/metadata, UUIDv7 bits and uniqueness, batching 2500 → 1000/1000/500, retry/`Retry-After`, 400 drop, buffer full warning, `flush`, `shutdown`, after-shutdown `undefined`, use fake timers (`vi.useFakeTimers()`) for the interval.

**Checks:** `just check-ts`, `just check`.

## T11 — Instrumentation helpers and `aiTelemetry`

**Commit:** `sdk-ts: add instrumentation helpers and aiTelemetry`

**Edit:** `package.json`, `tsdown.config.ts`, `pnpm-lock.yaml`, `src/config.ts`, `src/config.test.ts`, `src/instrument.ts` (new), `src/openai.ts` (new), `src/anthropic.ts` (new), `src/langchain.ts` (new), `src/ai.ts` (new), `src/runtime.ts`, `src/index.ts`, `src/testing.test-helpers.ts`, `src/instrument.test.ts` (new), `src/ai.test.ts` (new).

- **Option.** `instrument` comes back (D-F-7) in `LlmobsOptions`/`Config` as `readonly Instrumentation[]`, with `export interface Instrumentation { setTracerProvider(provider: TracerProvider): void; enable(): void; disable(): void }` in `instrument.ts` (`TracerProvider` from `@opentelemetry/api`). Validation: an array of objects having those three functions, else `ConfigError("instrument", "must be a list of instrumentation objects")`. Disabled config: `[]`.

- `package.json`: optional peers and devDependencies of § Dependency pins; `exports` gains `./openai`, `./anthropic`, `./langchain`; `tsdown` entries `src/openai.ts`, `src/anthropic.ts`, `src/langchain.ts`.
- Helpers (each file imports only its OpenInference package and types):
  ```ts
  // src/openai.ts
  import type OpenAI from "openai";
  export function openai(module: typeof OpenAI, options?: { captureContent?: boolean }): Instrumentation;
  ```
  builds `new OpenAIInstrumentation({ instrumentationConfig: { enabled: false }, traceConfig: { hideInputs: !capture, hideOutputs: !capture } })`, calls `manuallyInstrument(module)`, and returns it. `anthropic(module: typeof Anthropic, …)` with `AnthropicInstrumentation`; `langchain(module: typeof CallbackManagerModule, …)` where the module is `import * as CallbackManagerModule from "@langchain/core/callbacks/manager"` and the class `LangChainInstrumentation`. `captureContent` defaults to `true`; `init` does not re-read it, so the README tells users to pass the same value as `init`'s.
- `enableInstrumentations(runtime: Runtime): void` in `instrument.ts`: for each `config.instrument` item, `item.setTracerProvider(runtime.provider)` then `item.enable()`; any throw → debug log and continue. `init` calls it after `installRuntime`, and `startMemory` calls it too, so tests use the production path. `Runtime.shutdown` calls `disable()` on each.
- `ai.ts`: `export function aiTelemetry(options?: { sessionId?: string; userId?: string; tags?: readonly string[]; metadata?: Readonly<Record<string, string | number | boolean>>; functionId?: string }): { isEnabled: boolean; tracer?: Tracer; functionId?: string; metadata?: Record<string, AttributeValue> }` → when running: `{ isEnabled: true, tracer: runtime.provider.getTracer("ai"), functionId, metadata: { ...metadata, sessionId, userId, tags } }` with absent keys omitted; when not running: `{ isEnabled: false }`.
- `index.ts` exports `aiTelemetry` and the `Instrumentation` type.

**Tests:** `openai` end to end: `new OpenAI({ apiKey: "test", baseURL: "http://mock/v1", fetch: fakeFetch })` where `fakeFetch` returns `new Response(JSON.stringify(<the chat completion JSON from P10>), { status: 200, headers: { "content-type": "application/json" } })`; `init` (via `startMemory`) with `instrument: [openai(OpenAI)]`; inside `withContext({ sessionId: "s1" })` and `observe` → one OpenInference span, child of ours, stamped with `llmobs.session.id`; `anthropic` the same with the P10 message JSON; `captureContent: false` on the helper hides inputs; `shutdown` disables (no new span after). `langchain`: construct the helper with the real module and assert it returns an object with the three functions (no end-to-end run). A hand-written fake `Instrumentation` records `setTracerProvider`/`enable`/`disable` calls. `aiTelemetry` shape when running and not running; a span created with its tracer has scope `ai` and passes the default filter.

**Checks:** `just check-ts`, `just check`.

## T12 — Browser `feedback()`

**Commit:** `sdk-ts: add the browser feedback entry point`

**Edit:** `package.json`, `tsdown.config.ts`, `src/web.ts`, `src/config.ts`, `src/browser.ts` (new), `src/browser.test.ts` (new).

```ts
export interface FeedbackOptions {
  baseUrl: string; publicKey: string; traceId: string; spanId?: string;
  name?: string; value: number | boolean | string; comment?: string;
  fetch?: FetchLike;   // tests
}
export function feedback(options: FeedbackOptions): Promise<{ ok: boolean }>;
```

- `exports["./browser"]` and a `tsdown` entry. `browser.ts` imports only `./web.js` (no OTel, no `config.ts`). A test parses `src/browser.ts`'s import statements and asserts the only import is `./web.js`.
- Move `hasHttpAuthority` from `config.ts` to `web.ts` (exported) and import it in both `config.ts` and `browser.ts`, so the URL rule has one copy and `browser.ts` never imports `config.ts`.
- Validation (resolve `{ ok: false }` without a request when invalid): `baseUrl` absolute `http(s)` per `hasHttpAuthority`, no query or fragment, trailing slashes removed; `publicKey` starts with `pk-llmobs-`; `traceId` 32 lowercase hex; `spanId` 16 lowercase hex; `value` finite when a number; the JSON body ≤ 4096 bytes (spec 06).
- Request: `POST {baseUrl}/api/public/feedback`, `Content-Type: application/json`, `Authorization: Bearer <publicKey>`, `keepalive: true`, body `{trace_id, span_id?, name?, value, comment?}` (absent keys omitted). `{ ok: response.ok }`; a rejected fetch → `{ ok: false }`. Never throws, never retries.

**Tests:** body and headers; every validation rule; non-2xx → `ok: false`; fetch rejection → `ok: false`; no global fetch and no injected fetch → `ok: false`.

**Checks:** `just check-ts`, `just check`.

## T13 — Conformance tests and README

**Commit:** `sdk-ts: check the conformance fixtures and document the SDK`

**Edit:** `src/conformance.test.ts` (new), `src/conformance.test-helpers.ts` (new), `src/imports.test.ts` (new), `README.md`, `src/readme.test.ts` (new), `src/readme-examples.test-helpers.ts` (new).

- `conformance.test-helpers.ts`: `canonical(spans: ReadableSpan[]): unknown` per § Conformance: `JsonTraceSerializer.serializeRequest(spans)` → `JSON.parse(new TextDecoder().decode(bytes))` → the same rewrites as P11 (ids already hex here; normalize `intValue` to a decimal string with `String(v)`; `kind` and `status.code` are already integers).
- `conformance.test.ts`: one `it` per scenario, titled with the case directory name (`it("sdk-observe-tool", …)`), each running the TS variant of the scenario row (README § Language differences) through `startMemory` with the scenario's options, then comparing with `fixtures/otlp/llmobs/sdk-<case>/input.json` (read with `node:fs` — allowed in tests). Define `class ScenarioError extends Error { override name = "ScenarioError"; }`. A last test asserts every `sdk-*` directory name appears in an exported `SCENARIOS` list that the `it`s are generated from.
- `imports.test.ts`: reads every `src/*.ts` file except `*.test.ts` and `*.test-helpers.ts`, and asserts there is no `node:` import, no bare `async_hooks`/`events`/`fs`/`http` import and no `require(`.
- `README.md`: the same sections as P12, TypeScript versions, plus: the `llmobs/openai|anthropic|langchain` helpers; `aiTelemetry()` with the Vercel AI SDK; edge runtimes and Cloudflare Workers (`nodejs_compat`, `ctx.waitUntil(llmobs.flush())`); `llmobs/browser`.
- README examples: write each tested example as a function in `src/readme-examples.test-helpers.ts`, preceded by a comment `// readme:<name>`. `readme.test.ts` runs each function against `startMemory` and asserts the spans it produced, and checks that every README block preceded by `<!-- readme:<name> -->` is byte-identical to that function's body (dedented). Mark at least the quick start, `withLlmSpan`, `withContext`, `score` and `aiTelemetry` examples.

**Checks:** `just check-ts`, `just check`, and `just ts-smoke` on `pnpm --dir sdks/typescript pack` output.
