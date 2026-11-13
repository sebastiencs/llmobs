/**
 * Traced generators and async generators (T7, D-F-11).
 *
 * `observe` detects generator functions at wrap time and, in generator mode,
 * creates no span at call time: it returns `traceGenerator` /
 * `traceAsyncGenerator` around the inner generator. The span starts when the
 * body starts (the first `next()`), with the context active then as parent,
 * and is current only while the body runs: every step runs inside
 * `context.with(spanContext, …)`. Exhaustion ends the span with the status
 * unset and the yielded items as the output; an early `return()` from the
 * consumer ends it with status ok and the items so far (an early `return()`
 * is not an error, § Span attributes "Errors"); an exception from the body, or
 * from a `throw()` the consumer delivers and the body does not catch, records
 * the error per § Span attributes "Errors" and is re-thrown. A `throw()` the
 * body catches keeps the loop going.
 *
 * A generator abandoned half-consumed — never exhausted, never `return()`ed —
 * is never exported: TypeScript has no reliable finalizer, so its span stays
 * unended. Consume generators fully, `break` out of the loop (which calls
 * `return()`), or call `return()` yourself.
 */

import { context, type Span, SpanStatusCode, trace } from "@opentelemetry/api";

import { debug } from "./log.js";
import type { Runtime } from "./runtime.js";
import { recordError, type SpanStart, setPayload, startTracedSpan } from "./span.js";

/** What a traced generator knows before its body starts. Internal. */
export interface GeneratorPlan extends SpanStart {
  readonly runtime: Runtime;
  /** `observe`: keep the yielded items and record them as the output. */
  readonly captureOutput: boolean;
  /** `observe`: record `fn(items)` instead of the default output rule. */
  readonly reduceOutput: ((items: unknown[]) => unknown) | undefined;
}

/** The next thing to hand the inner generator: a sent value or a thrown error. */
type Delivery = { kind: "next"; value: unknown } | { kind: "throw"; error: unknown };

const START: Delivery = { kind: "next", value: undefined };

/**
 * Wrap `inner` so the generator runs inside one of our spans (see the module
 * doc). The wrapper starts the span on the first `next()`, forwards sent
 * values and consumer `throw()`s, and ends the span on exhaustion (status
 * unset, the output), on an early `return()` (status ok, the items so far) or
 * on an exception (the error, then re-throw). An abandoned wrapper never ends
 * its span, so it is never exported.
 */
export function* traceGenerator<Y, R, S>(
  inner: Generator<Y, R, S>,
  plan: GeneratorPlan,
): Generator<Y, R, S> {
  const span = startTracedSpan(plan.runtime, plan);
  if (span === undefined) {
    return yield* inner;
  }
  const spanContext = trace.setSpan(context.active(), span);
  const items: unknown[] = [];
  const keeping = plan.runtime.config.captureContent && plan.captureOutput;
  let ended = false;
  let delivery: Delivery = START;
  try {
    for (;;) {
      let result: IteratorResult<Y, R>;
      try {
        result = context.with(spanContext, () => deliver(inner, delivery));
      } catch (error) {
        ended = true;
        recordError(span, error);
        span.end();
        throw error;
      }
      if (result.done === true) {
        ended = true;
        recordOutput(span, plan, items);
        span.end();
        return result.value;
      }
      if (keeping) {
        items.push(result.value);
      }
      try {
        delivery = { kind: "next", value: yield result.value };
      } catch (error) {
        delivery = { kind: "throw", error };
      }
    }
  } finally {
    if (!ended) {
      ended = true;
      try {
        // The consumer's return value wins (the finally falls through), so
        // what inner.return reports back is ignored.
        context.with(spanContext, () => inner.return(undefined as R));
        span.setStatus({ code: SpanStatusCode.OK });
        recordOutput(span, plan, items);
      } catch (error) {
        recordError(span, error);
      }
      span.end();
    }
  }
}

/**
 * The `async function*` equivalent of `traceGenerator`: the span starts on the
 * first `next()`, each step's continuations run inside the span context, and
 * the span ends on exhaustion, early `return()` or exception as its sync twin.
 * An abandoned async generator never ends its span, so it is never exported.
 */
export async function* traceAsyncGenerator<Y, R, S>(
  inner: AsyncGenerator<Y, R, S>,
  plan: GeneratorPlan,
): AsyncGenerator<Y, R, S> {
  const span = startTracedSpan(plan.runtime, plan);
  if (span === undefined) {
    return yield* inner;
  }
  const spanContext = trace.setSpan(context.active(), span);
  const items: unknown[] = [];
  const keeping = plan.runtime.config.captureContent && plan.captureOutput;
  let ended = false;
  let delivery: Delivery = START;
  try {
    for (;;) {
      let result: IteratorResult<Y, R>;
      try {
        // The step's promise is created inside the span context, so the
        // body's continuations run there; the consumer keeps its own.
        result = await context.with(spanContext, () => deliverAsync(inner, delivery));
      } catch (error) {
        ended = true;
        recordError(span, error);
        span.end();
        throw error;
      }
      if (result.done === true) {
        ended = true;
        recordOutput(span, plan, items);
        span.end();
        return result.value;
      }
      if (keeping) {
        items.push(result.value);
      }
      try {
        delivery = { kind: "next", value: yield result.value };
      } catch (error) {
        delivery = { kind: "throw", error };
      }
    }
  } finally {
    if (!ended) {
      ended = true;
      try {
        // The consumer's return value wins (the finally falls through), so
        // what inner.return reports back is ignored.
        await context.with(spanContext, () => inner.return(undefined as R | PromiseLike<R>));
        span.setStatus({ code: SpanStatusCode.OK });
        recordOutput(span, plan, items);
      } catch (error) {
        recordError(span, error);
      }
      span.end();
    }
  }
}

/** One inner step of a generator, with a consumer `throw()` forwarded. */
function deliver<Y, R, S>(inner: Generator<Y, R, S>, delivery: Delivery): IteratorResult<Y, R> {
  return delivery.kind === "next"
    ? // The first step sends undefined; S is the declared TNext.
      inner.next(delivery.value as S)
    : inner.throw(delivery.error);
}

/** One inner step of an async generator, with a consumer `throw()` forwarded. */
function deliverAsync<Y, R, S>(
  inner: AsyncGenerator<Y, R, S>,
  delivery: Delivery,
): Promise<IteratorResult<Y, R>> {
  return delivery.kind === "next"
    ? // The first step sends undefined; S is the declared TNext.
      inner.next(delivery.value as S)
    : inner.throw(delivery.error);
}

/**
 * The generator output rule: no items → no output; every item a string →
 * their concatenation (text); otherwise the items as a JSON list.
 * `plan.reduceOutput`, when given, records `fn(items)` instead; a throw falls
 * back to the default rule with a debug log. Never throws.
 */
function recordOutput(span: Span, plan: GeneratorPlan, items: unknown[]): void {
  if (items.length === 0) {
    return;
  }
  if (plan.reduceOutput !== undefined) {
    try {
      setPayload(span, "llmobs.output", plan.reduceOutput(items), plan.runtime);
      return;
    } catch (error) {
      debug(`llmobs: reduceOutput failed (${errorName(error)}); recording the items instead`);
    }
  }
  if (items.every((item) => typeof item === "string")) {
    setPayload(span, "llmobs.output", items.join(""), plan.runtime);
    return;
  }
  setPayload(span, "llmobs.output", items, plan.runtime);
}

function errorName(error: unknown): string {
  return error instanceof Error ? error.name : typeof error;
}
