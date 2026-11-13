/**
 * Manual spans: `withSpan`, the `observe` wrapper and the current ids (T6).
 *
 * One runner starts the span on our tracer with the active context as parent,
 * runs the body with the span current, and ends it when the body settles: a
 * thenable result ends the span from `.then` callbacks (a promise of the same
 * value comes back), a synchronous throw is recorded and re-thrown unchanged
 * (§ Span attributes "Errors"). Without a running runtime the body runs
 * untouched: `withSpan` with a no-op handle, `observe`'s wrapper straight
 * through. The check happens at call time, so functions wrapped before
 * `init()` are traced after it.
 *
 * Generator functions take another path (T7): `observe` detects them at wrap
 * time and traces the *generator*, not the call, through `./generators.js`.
 */

import {
  context,
  isSpanContextValid,
  type Span,
  type SpanContext,
  SpanStatusCode,
  trace,
} from "@opentelemetry/api";

import { metadataAttributeName } from "./context.js";
import { type GeneratorPlan, traceAsyncGenerator, traceGenerator } from "./generators.js";
import { debug } from "./log.js";
import { applyMask } from "./mask.js";
import { currentRuntime, type Runtime } from "./runtime.js";
import { attributeValue, serialize } from "./serialize.js";

export type SpanKind =
  | "llm"
  | "embedding"
  | "tool"
  | "agent"
  | "chain"
  | "retriever"
  | "reranker"
  | "guardrail"
  | "evaluator"
  | "span";

const KINDS: readonly string[] = [
  "llm",
  "embedding",
  "tool",
  "agent",
  "chain",
  "retriever",
  "reranker",
  "guardrail",
  "evaluator",
  "span",
];

/**
 * § Span attributes "Programming errors raise": an invalid `kind` is the one
 * error the SDK raises after `init()`, before the user function runs. It is
 * checked when `observe`/`withSpan` is called (wrap time for `observe`).
 */
function checkKind(kind: unknown): SpanKind {
  if (kind === undefined) {
    return "chain";
  }
  if (typeof kind === "string" && KINDS.includes(kind)) {
    return kind as SpanKind;
  }
  throw new TypeError(`kind: must be one of ${KINDS.join(", ")}`);
}

export interface SpanHandle {
  /** 32 lowercase hex, `undefined` for a no-op handle. */
  readonly traceId: string | undefined;
  /** 16 lowercase hex, `undefined` for a no-op handle. */
  readonly spanId: string | undefined;
  setInput(value: unknown): void;
  setOutput(value: unknown): void;
  setMetadata(values: Readonly<Record<string, unknown>>): void;
}

export interface SpanOptions {
  kind?: SpanKind;
  input?: unknown;
  metadata?: Readonly<Record<string, unknown>>;
}

export interface ObserveOptions {
  name?: string;
  kind?: SpanKind;
  captureInput?: boolean;
  captureOutput?: boolean;
  /** Generator functions only: record `fn(items)` instead of the default output rule. */
  reduceOutput?: (items: unknown[]) => unknown;
}

/** What starting a traced span needs. Internal, shared with the generator wrapper (T7). */
export interface SpanStart {
  readonly name: string;
  readonly kind: SpanKind;
  readonly hasInput: boolean;
  readonly inputValue: unknown;
  readonly metadata: Readonly<Record<string, unknown>> | undefined;
}

/** What one traced call knows before it starts. Internal. */
interface TracedPlan extends SpanStart {
  /** `observe`: record the settled result as `llmobs.output` (D-F-11). */
  readonly captureResultAsOutput: boolean;
}

/**
 * The shared runner. The body's return value is never recorded as the output
 * here; `observe` records it through `plan.captureResultAsOutput` and
 * `withSpan` only through `SpanHandle.setOutput`.
 */
function traced<T>(plan: TracedPlan, body: (span: SpanHandle) => T): T {
  const runtime = currentRuntime();
  if (runtime === undefined) {
    return body(NOOP_HANDLE);
  }
  const span = startTracedSpan(runtime, plan);
  if (span === undefined) {
    return body(NOOP_HANDLE);
  }
  const handle = new LiveSpanHandle(span, runtime);
  try {
    const result = context.with(trace.setSpan(context.active(), span), () => body(handle));
    if (isThenable(result)) {
      const settled = (result as PromiseLike<unknown>).then(
        (value) => {
          captureResult(plan, span, runtime, value);
          span.end();
          return value;
        },
        (error) => {
          recordError(span, error);
          span.end();
          throw error;
        },
      );
      return settled as unknown as T;
    }
    captureResult(plan, span, runtime, result);
    span.end();
    return result;
  } catch (error) {
    recordError(span, error);
    span.end();
    throw error;
  }
}

/** Start the span on our tracer, stamp kind, input and metadata. Never throws. */
export function startTracedSpan(runtime: Runtime, plan: SpanStart): Span | undefined {
  try {
    const span = runtime.tracer.startSpan(plan.name, undefined, context.active());
    span.setAttribute("llmobs.span.kind", plan.kind);
    if (plan.hasInput) {
      setPayload(span, "llmobs.input", plan.inputValue, runtime);
    }
    if (plan.metadata !== undefined) {
      stampMetadata(span, plan.metadata);
    }
    return span;
  } catch (error) {
    debug(`llmobs: starting a span failed: ${errorName(error)}`);
    return undefined;
  }
}

/** `observe`: the settled result as the output, when it is one to record. */
function captureResult(plan: TracedPlan, span: Span, runtime: Runtime, value: unknown): void {
  // § D-F-11 and the README: undefined means "no output"; generator and async
  // generator objects are passed through untouched, never consumed.
  if (plan.captureResultAsOutput && value !== undefined && !isGeneratorObject(value)) {
    setPayload(span, "llmobs.output", value, runtime);
  }
}

/** § Span attributes "Errors": the event ourselves (not `recordException`), the error status, then re-raise. */
export function recordError(span: Span, error: unknown): void {
  try {
    const isError = error instanceof Error;
    const type = isError ? error.name : "Error";
    const message = isError ? error.message : String(error);
    span.addEvent("exception", {
      "exception.type": type,
      "exception.message": message,
      "exception.stacktrace": isError ? (error.stack ?? "") : "",
    });
    span.setStatus({ code: SpanStatusCode.ERROR, message: `${type}: ${message}` });
  } catch (failure) {
    debug(`llmobs: recording an exception failed: ${errorName(failure)}`);
  }
}

/** The masked, serialized payload under `key` plus its format, when content capture is on. Never throws. */
export function setPayload(
  span: Span,
  key: "llmobs.input" | "llmobs.output",
  value: unknown,
  runtime: Runtime,
): void {
  try {
    if (!runtime.config.captureContent) {
      return;
    }
    const [text, format] = serialize(applyMask(value));
    span.setAttribute(key, text);
    span.setAttribute(`${key}.format`, format);
  } catch (error) {
    debug(`llmobs: setting a payload attribute failed: ${errorName(error)}`);
  }
}

/** Each metadata value as its `llmobs.metadata.<key>` attribute, masked per value (D-F-6). Never throws. */
function stampMetadata(span: Span, values: Readonly<Record<string, unknown>>): void {
  try {
    for (const [key, value] of Object.entries(values)) {
      const name = metadataAttributeName(key);
      if (name !== undefined) {
        span.setAttribute(name, attributeValue(applyMask(value)));
      }
    }
  } catch (error) {
    debug(`llmobs: setting a metadata attribute failed: ${errorName(error)}`);
  }
}

/** The live handle; every method is a no-op after the span ended and never raises. */
class LiveSpanHandle implements SpanHandle {
  private readonly span: Span;
  private readonly runtime: Runtime;

  constructor(span: Span, runtime: Runtime) {
    this.span = span;
    this.runtime = runtime;
  }

  get traceId(): string | undefined {
    const spanContext = this.span.spanContext();
    return isSpanContextValid(spanContext) ? spanContext.traceId : undefined;
  }

  get spanId(): string | undefined {
    const spanContext = this.span.spanContext();
    return isSpanContextValid(spanContext) ? spanContext.spanId : undefined;
  }

  setInput(value: unknown): void {
    if (this.span.isRecording()) {
      setPayload(this.span, "llmobs.input", value, this.runtime);
    }
  }

  setOutput(value: unknown): void {
    if (this.span.isRecording()) {
      setPayload(this.span, "llmobs.output", value, this.runtime);
    }
  }

  setMetadata(values: Readonly<Record<string, unknown>>): void {
    if (this.span.isRecording()) {
      stampMetadata(this.span, values);
    }
  }
}

const NOOP_HANDLE: SpanHandle = {
  traceId: undefined,
  spanId: undefined,
  setInput: () => {},
  setOutput: () => {},
  setMetadata: () => {},
};

/** A body result with a `then` method ends the span when it settles. */
function isThenable(value: unknown): value is PromiseLike<unknown> {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { then?: unknown }).then === "function"
  );
}

/** D-F-11: generator and async generator objects (functions are detected in T7). */
function isGeneratorObject(value: unknown): boolean {
  const tag = Object.prototype.toString.call(value);
  return tag === "[object Generator]" || tag === "[object AsyncGenerator]";
}

/**
 * Run `fn` inside one of our spans. Its return value is never recorded as the
 * output; only the handle's `setOutput` writes one. A thenable result ends the
 * span when it settles and yields a promise of the same value; a synchronous
 * throw is recorded and re-thrown unchanged.
 */
export function withSpan<T>(name: string, fn: (span: SpanHandle) => T): T;
export function withSpan<T>(name: string, options: SpanOptions, fn: (span: SpanHandle) => T): T;
export function withSpan<T>(
  name: string,
  fnOrOptions: SpanOptions | ((span: SpanHandle) => T),
  maybeFn?: (span: SpanHandle) => T,
): T {
  const raw = typeof fnOrOptions === "function" ? {} : fnOrOptions;
  const kind = checkKind(raw.kind);
  // The overloads guarantee fn: first in the two-argument form, last in the
  // three-argument form.
  const fn = (typeof fnOrOptions === "function" ? fnOrOptions : maybeFn) as (span: SpanHandle) => T;
  return traced(
    {
      name,
      kind,
      hasInput: "input" in raw,
      inputValue: raw.input,
      metadata: raw.metadata,
      captureResultAsOutput: false,
    },
    fn,
  );
}

/**
 * Wrap `fn` so each call runs inside one of our spans. `this` is preserved;
 * the input follows D-F-11 (no arguments → none, one → that value, more → the
 * array of them) and the output is the return value or resolved value
 * (`undefined` and generator objects are not recorded).
 *
 * Generator and async generator functions (detected at wrap time) are traced
 * as generators instead: no span at call time, the span starts at the first
 * `next()`, the output is the yielded items (or `reduceOutput(items)`), an
 * early `return()` ends with status ok. See `./generators.js`. The kind is
 * checked at wrap time; the runtime at call time, so a function wrapped
 * before `init()` is traced after it.
 */
export function observe<A extends unknown[], R>(
  fn: (...args: A) => R,
  options?: ObserveOptions,
): (...args: A) => R {
  const raw = options ?? {};
  const kind = checkKind(raw.kind);
  const name = raw.name ?? (fn.name || "anonymous");
  const captureInput = raw.captureInput ?? true;
  const captureOutput = raw.captureOutput ?? true;
  const tag = Object.prototype.toString.call(fn);
  const generatorTag =
    tag === "[object GeneratorFunction]" || tag === "[object AsyncGeneratorFunction]";
  return function (this: unknown, ...args: A): R {
    if (!generatorTag) {
      return traced(
        {
          name,
          kind,
          hasInput: captureInput && args.length > 0,
          inputValue: args.length === 1 ? args[0] : args,
          metadata: undefined,
          captureResultAsOutput: captureOutput,
        },
        () => fn.apply(this, args),
      );
    }
    // Generator mode (D-F-11): no span at call time, and fn.apply runs none
    // of the body. The wrap-time tag check guarantees the return value is a
    // generator, so the casts are sound; with no runtime it goes back unwrapped.
    const runtime = currentRuntime();
    const gen = fn.apply(this, args);
    if (runtime === undefined) {
      return gen;
    }
    const plan: GeneratorPlan = {
      name,
      kind,
      hasInput: captureInput && args.length > 0,
      inputValue: args.length === 1 ? args[0] : args,
      metadata: undefined,
      runtime,
      captureOutput,
      reduceOutput: raw.reduceOutput,
    };
    return tag === "[object GeneratorFunction]"
      ? (traceGenerator(gen as Generator<unknown, unknown, unknown>, plan) as R)
      : (traceAsyncGenerator(gen as AsyncGenerator<unknown, unknown, unknown>, plan) as R);
  };
}

/** The trace id of the current span (ours or an instrumentor's), when valid. */
export function currentTraceId(): string | undefined {
  return currentSpanContext()?.traceId;
}

/** The span id of the current span (ours or an instrumentor's), when valid. */
export function currentSpanId(): string | undefined {
  return currentSpanContext()?.spanId;
}

function currentSpanContext(): SpanContext | undefined {
  const spanContext = trace.getSpan(context.active())?.spanContext();
  return spanContext !== undefined && isSpanContextValid(spanContext) ? spanContext : undefined;
}

function errorName(error: unknown): string {
  return error instanceof Error ? error.name : typeof error;
}
