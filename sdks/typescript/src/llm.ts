/**
 * The manual LLM span: `withLlmSpan` (T8, D-F-13).
 *
 * One chat span with GenAI attributes, started through the T6 runner's
 * building blocks (`startTracedSpan`, `recordError`, `setPayload`). Messages
 * may be given in the canonical format (an object with `parts`) or as the
 * short form `{role, content: "<text>"}`, which becomes one text part; the
 * list is masked first, then converted, then serialized (§ LLM span). With
 * `captureContent: false` none of the message attributes is written; the
 * model, params, usage, cost and TTFT stay. The body's return value is never
 * recorded; `setInput`/`setOutput` remain available for non-chat payloads.
 */

import { context, isSpanContextValid, type Span, trace } from "@opentelemetry/api";
import { hrTime, hrTimeDuration, hrTimeToNanoseconds } from "@opentelemetry/core";
import type { ReadableSpan } from "@opentelemetry/sdk-trace";

import { metadataAttributeName } from "./context.js";
import { debug } from "./log.js";
import { applyMask } from "./mask.js";
import { currentRuntime, type Runtime } from "./runtime.js";
import { attributeValue, serialize } from "./serialize.js";
import { recordError, type SpanHandle, setPayload, startTracedSpan } from "./span.js";

export interface LlmSpanOptions {
  /** Required, non-empty; written as `gen_ai.provider.name`. */
  provider: string;
  /** Required, non-empty; written as `gen_ai.request.model`. */
  model: string;
  inputMessages?: readonly unknown[];
  /** A string becomes one text part. */
  systemInstructions?: string | readonly unknown[];
  /** Only string, boolean and finite number values are written. */
  params?: Readonly<Record<string, unknown>>;
  metadata?: Readonly<Record<string, unknown>>;
}

export interface Usage {
  inputTokens?: number;
  outputTokens?: number;
  cacheReadTokens?: number;
  cacheWriteTokens?: number;
  reasoningTokens?: number;
}

export interface LlmSpanHandle extends SpanHandle {
  setInputMessages(messages: readonly unknown[]): void;
  setOutputMessages(messages: readonly unknown[]): void;
  setResponseModel(model: string): void;
  setUsage(usage: Usage): void;
  setCost(totalUsd: number): void;
  recordFirstToken(): void;
}

/** `Usage` field to attribute name, in the setter's declaration order. */
const USAGE_KEYS: readonly (readonly [keyof Usage, string])[] = [
  ["inputTokens", "gen_ai.usage.input_tokens"],
  ["outputTokens", "gen_ai.usage.output_tokens"],
  ["cacheReadTokens", "gen_ai.usage.cache_read.input_tokens"],
  ["cacheWriteTokens", "gen_ai.usage.cache_creation.input_tokens"],
  ["reasoningTokens", "gen_ai.usage.reasoning.output_tokens"],
];

/** § "Programming errors raise": like `kind`, checked before the body runs. */
function requireNonEmptyString(option: "provider" | "model", value: unknown): string {
  if (typeof value === "string" && value.length > 0) {
    return value;
  }
  throw new TypeError(`${option}: must be a non-empty string`);
}

/** § LLM span "Messages JSON": one message to the canonical shape. */
function toCanonicalMessage(message: unknown): unknown {
  if (typeof message !== "object" || message === null) {
    return message;
  }
  // An object with a `parts` key is already canonical; anything without
  // `role` + string `content` is kept as is (the server records the error).
  if ("parts" in message) {
    return message;
  }
  if ("role" in message && "content" in message && typeof message.content === "string") {
    const converted: Record<string, unknown> = { role: message.role };
    if ("name" in message) {
      converted.name = message.name;
    }
    if ("finish_reason" in message) {
      converted.finish_reason = message.finish_reason;
    }
    converted.parts = [{ type: "text", content: message.content }];
    return converted;
  }
  return message;
}

/** The masked, converted, serialized message list under `key`. Never throws. */
function setMessages(
  span: Span,
  key: "gen_ai.input.messages" | "gen_ai.output.messages",
  messages: readonly unknown[],
  runtime: Runtime,
): void {
  try {
    if (!runtime.config.captureContent) {
      return;
    }
    // Mask the list first, then convert (§ Messages JSON); a mask that
    // replaced the whole list with a non-array is serialized as it is.
    const masked = applyMask(messages);
    const converted = Array.isArray(masked) ? masked.map(toCanonicalMessage) : masked;
    span.setAttribute(key, serialize(converted)[0]);
  } catch (error) {
    debug(`llmobs: setting ${key} failed: ${errorName(error)}`);
  }
}

/** The masked system instructions as parts JSON. Never throws. */
function setSystemInstructions(
  span: Span,
  instructions: string | readonly unknown[],
  runtime: Runtime,
): void {
  try {
    if (!runtime.config.captureContent) {
      return;
    }
    const masked = applyMask(instructions);
    const parts = typeof masked === "string" ? [{ type: "text", content: masked }] : masked;
    span.setAttribute("gen_ai.system_instructions", serialize(parts)[0]);
  } catch (error) {
    debug(`llmobs: setting gen_ai.system_instructions failed: ${errorName(error)}`);
  }
}

/** `gen_ai.request.<key>` for every scalar param (§ LLM span). */
function stampParams(span: Span, params: Readonly<Record<string, unknown>> | undefined): void {
  if (typeof params !== "object" || params === null) {
    return;
  }
  for (const [key, value] of Object.entries(params)) {
    if (
      typeof value === "string" ||
      typeof value === "boolean" ||
      (typeof value === "number" && Number.isFinite(value))
    ) {
      span.setAttribute(`gen_ai.request.${key}`, value);
    }
  }
}

/** The fixed attributes of the LLM span, before the body runs. Never throws. */
function stampLlmStart(
  span: Span,
  provider: string,
  model: string,
  options: LlmSpanOptions,
  runtime: Runtime,
): void {
  try {
    span.setAttribute("gen_ai.operation.name", "chat");
    span.setAttribute("gen_ai.provider.name", provider);
    span.setAttribute("gen_ai.request.model", model);
    stampParams(span, options.params);
    if (options.inputMessages !== undefined) {
      setMessages(span, "gen_ai.input.messages", options.inputMessages, runtime);
    }
    if (options.systemInstructions !== undefined) {
      setSystemInstructions(span, options.systemInstructions, runtime);
    }
  } catch (error) {
    debug(`llmobs: starting the LLM span attributes failed: ${errorName(error)}`);
  }
}

/** Each metadata value as its `llmobs.metadata.<key>` attribute (D-F-6). Never throws. */
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
class LiveLlmSpanHandle implements LlmSpanHandle {
  private readonly span: Span;
  private readonly runtime: Runtime;
  private firstTokenRecorded = false;

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

  setInputMessages(messages: readonly unknown[]): void {
    if (this.span.isRecording()) {
      setMessages(this.span, "gen_ai.input.messages", messages, this.runtime);
    }
  }

  setOutputMessages(messages: readonly unknown[]): void {
    if (this.span.isRecording()) {
      setMessages(this.span, "gen_ai.output.messages", messages, this.runtime);
    }
  }

  setResponseModel(model: string): void {
    if (!this.span.isRecording()) {
      return;
    }
    try {
      this.span.setAttribute("gen_ai.response.model", model);
    } catch (error) {
      debug(`llmobs: setting gen_ai.response.model failed: ${errorName(error)}`);
    }
  }

  /** Safe integers ≥ 0 only; anything else is skipped with a debug line (no values). */
  setUsage(usage: Usage): void {
    if (!this.span.isRecording()) {
      return;
    }
    try {
      for (const [field, attribute] of USAGE_KEYS) {
        const value = usage[field];
        if (value === undefined) {
          continue;
        }
        if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) {
          this.span.setAttribute(attribute, value);
        } else {
          debug(`llmobs: skipped ${attribute}`);
        }
      }
    } catch (error) {
      debug(`llmobs: setting a usage attribute failed: ${errorName(error)}`);
    }
  }

  setCost(totalUsd: number): void {
    if (!this.span.isRecording()) {
      return;
    }
    if (typeof totalUsd === "number" && Number.isFinite(totalUsd) && totalUsd >= 0) {
      try {
        this.span.setAttribute("llmobs.cost.total", totalUsd);
      } catch (error) {
        debug(`llmobs: setting llmobs.cost.total failed: ${errorName(error)}`);
      }
    } else {
      debug("llmobs: skipped llmobs.cost.total");
    }
  }

  /** `llmobs.ttft_ns`: now − span start; only the first call counts. */
  recordFirstToken(): void {
    if (this.firstTokenRecorded || !this.span.isRecording()) {
      return;
    }
    this.firstTokenRecorded = true;
    try {
      // The API `Span` type hides the SDK fields; the live object is the SDK
      // span, whose `startTime` is the HrTime `startSpan` created it with.
      // The one cast T8's plan allows.
      const startTime = (this.span as unknown as ReadableSpan).startTime;
      this.span.setAttribute(
        "llmobs.ttft_ns",
        hrTimeToNanoseconds(hrTimeDuration(startTime, hrTime())),
      );
    } catch (error) {
      debug(`llmobs: recording the first token failed: ${errorName(error)}`);
    }
  }
}

const NOOP_LLM_HANDLE: LlmSpanHandle = {
  traceId: undefined,
  spanId: undefined,
  setInput: () => {},
  setOutput: () => {},
  setMetadata: () => {},
  setInputMessages: () => {},
  setOutputMessages: () => {},
  setResponseModel: () => {},
  setUsage: () => {},
  setCost: () => {},
  recordFirstToken: () => {},
};

/**
 * Run `fn` inside one manual LLM span. `provider` and `model` are checked
 * before the body runs (a `TypeError` otherwise); the body's return value is
 * never recorded — only the handle's setters write attributes.
 */
export function withLlmSpan<T>(
  name: string,
  options: LlmSpanOptions,
  fn: (span: LlmSpanHandle) => T,
): T {
  const provider = requireNonEmptyString("provider", options.provider);
  const model = requireNonEmptyString("model", options.model);
  const runtime = currentRuntime();
  if (runtime === undefined) {
    return fn(NOOP_LLM_HANDLE);
  }
  const span = startTracedSpan(runtime, {
    name,
    kind: "llm",
    hasInput: false,
    inputValue: undefined,
    metadata: options.metadata,
  });
  if (span === undefined) {
    return fn(NOOP_LLM_HANDLE);
  }
  stampLlmStart(span, provider, model, options, runtime);
  const handle = new LiveLlmSpanHandle(span, runtime);
  try {
    const result = context.with(trace.setSpan(context.active(), span), () => fn(handle));
    if (isThenable(result)) {
      // The body settles later: end the span from an observer chain and hand
      // the thenable itself back (same settlement, same value, never recorded).
      void result.then(
        () => span.end(),
        (error) => {
          recordError(span, error);
          span.end();
        },
      );
      return result;
    }
    span.end();
    return result;
  } catch (error) {
    recordError(span, error);
    span.end();
    throw error;
  }
}

/** A body result with a `then` method ends the span when it settles. */
function isThenable(value: unknown): value is PromiseLike<unknown> {
  return (
    typeof value === "object" &&
    value !== null &&
    "then" in value &&
    typeof value.then === "function"
  );
}

function errorName(error: unknown): string {
  return error instanceof Error ? error.name : typeof error;
}
