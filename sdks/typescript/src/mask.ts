/**
 * Masking (D-F-6).
 *
 * Two passes share this module. At capture time every value the SDK records
 * (inputs, outputs, metadata values, LLM messages) passes through the
 * configured `mask` before serialization (`applyMask`). At export time
 * `MaskingExporter` rewrites the payloads other instrumentations recorded
 * (§ Payload keys): values are JSON-decoded when they parse as an object or
 * array, masked, and serialized back. With `captureContent: false` the payload
 * keys are dropped instead. A mask that raises fails closed: the value becomes
 * the marker and the error name is logged at debug level (never the value).
 */

import type { Attributes } from "@opentelemetry/api";
import { type ExportResult, ExportResultCode } from "@opentelemetry/core";
import type { ReadableSpan, SpanExporter, TimedEvent } from "@opentelemetry/sdk-trace";

import { debug } from "./log.js";
import { currentRuntime } from "./runtime.js";
import { serialize } from "./serialize.js";
import { runtimeGlobals } from "./web.js";

export const MASK_FAILED = "[llmobs: mask failed]";

/**
 * § Payload keys: the exact attribute names every known schema uses for
 * payload content, in span and event attributes alike.
 */
const PAYLOAD_KEYS: ReadonlySet<string> = new Set([
  "llmobs.input",
  "llmobs.output",
  "gen_ai.input.messages",
  "gen_ai.output.messages",
  "gen_ai.system_instructions",
  "gen_ai.tool.call.arguments",
  "gen_ai.tool.call.result",
  "input.value",
  "output.value",
  "traceloop.entity.input",
  "traceloop.entity.output",
  "ai.prompt",
  "ai.prompt.messages",
  "ai.response.text",
  "ai.response.object",
  "ai.response.toolCalls",
  "ai.toolCall.args",
  "ai.toolCall.result",
  "langfuse.observation.input",
  "langfuse.observation.output",
  "langfuse.trace.input",
  "langfuse.trace.output",
  "lmnr.span.input",
  "lmnr.span.output",
]);

/** Prefixes under which a schema spreads its payload over several keys. */
const PAYLOAD_PREFIXES: readonly string[] = [
  "llm.input_messages.",
  "llm.output_messages.",
  "gen_ai.prompt.",
  "gen_ai.completion.",
];

function isPayloadKey(key: string): boolean {
  return PAYLOAD_KEYS.has(key) || PAYLOAD_PREFIXES.some((prefix) => key.startsWith(prefix));
}

/** Apply the running runtime's `mask` to `value`. Never throws. */
export function applyMask(value: unknown): unknown {
  const mask = currentRuntime()?.config.mask;
  if (mask === undefined) {
    return value;
  }
  return applyWithFailClosed(mask, value);
}

/** Run `mask`; a raising mask fails closed to the marker (D-F-6). */
function applyWithFailClosed(mask: (value: unknown) => unknown, value: unknown): unknown {
  try {
    return mask(value);
  } catch (error) {
    debug(`llmobs: mask raised ${errorName(error)}`);
    return MASK_FAILED;
  }
}

/**
 * The export-time masking pass (D-F-6), installed by `Runtime.create` only
 * when a `mask` is configured or `captureContent` is false. Spans whose scope
 * is `llmobs` were already masked at capture and pass through untouched.
 */
export class MaskingExporter implements SpanExporter {
  private readonly inner: SpanExporter;
  private readonly mask: ((value: unknown) => unknown) | undefined;
  private readonly captureContent: boolean;

  constructor(
    inner: SpanExporter,
    options: {
      mask: ((value: unknown) => unknown) | undefined;
      captureContent: boolean;
    },
  ) {
    this.inner = inner;
    this.mask = options.mask;
    this.captureContent = options.captureContent;
  }

  export(spans: ReadableSpan[], resultCallback: (result: ExportResult) => void): void {
    let masked: ReadableSpan[];
    try {
      masked = spans.map((span) => this.rewrite(span));
    } catch {
      // Fail closed: never let an unmasked batch through (the count is all the
      // message may contain).
      runtimeGlobals().console?.warn(`llmobs: masking failed; dropped ${spans.length} spans`);
      resultCallback({
        code: ExportResultCode.FAILED,
        error: new Error("masking failed"),
      });
      return;
    }
    this.inner.export(masked, resultCallback);
  }

  async shutdown(): Promise<void> {
    await this.inner.shutdown();
  }

  async forceFlush(): Promise<void> {
    await this.inner.forceFlush?.();
  }

  /**
   * A span with no payload key passes by reference; otherwise the rewrite is a
   * new object literal implementing `ReadableSpan` field by field. Never
   * spread the original: its `spanContext` lives on the prototype.
   */
  private rewrite(span: ReadableSpan): ReadableSpan {
    if (span.instrumentationScope.name === "llmobs" || !hasPayloadKeys(span)) {
      return span;
    }
    return {
      name: span.name,
      kind: span.kind,
      spanContext: () => span.spanContext(),
      parentSpanContext: span.parentSpanContext,
      startTime: span.startTime,
      endTime: span.endTime,
      status: span.status,
      attributes: this.rewriteAttributes(span.attributes),
      links: span.links,
      events: span.events.map(
        (event): TimedEvent => ({
          time: event.time,
          name: event.name,
          attributes:
            event.attributes === undefined ? undefined : this.rewriteAttributes(event.attributes),
          droppedAttributesCount: event.droppedAttributesCount,
        }),
      ),
      duration: span.duration,
      ended: span.ended,
      resource: span.resource,
      instrumentationScope: span.instrumentationScope,
      droppedAttributesCount: span.droppedAttributesCount,
      droppedEventsCount: span.droppedEventsCount,
      droppedLinksCount: span.droppedLinksCount,
    };
  }

  /**
   * Payload values: dropped when capture is off or (only strings are masked)
   * when the value is not a string; otherwise decoded, masked, re-encoded.
   * Every other key is copied unchanged.
   */
  private rewriteAttributes(attributes: Attributes): Attributes {
    const mask = this.mask;
    const next: Attributes = {};
    for (const [key, value] of Object.entries(attributes)) {
      if (!isPayloadKey(key)) {
        next[key] = value;
        continue;
      }
      if (!this.captureContent || typeof value !== "string" || mask === undefined) {
        continue;
      }
      next[key] = maskPayloadValue(mask, value);
    }
    return next;
  }
}

function hasPayloadKeys(span: ReadableSpan): boolean {
  if (attributesHavePayloadKey(span.attributes)) {
    return true;
  }
  return span.events.some((event) => attributesHavePayloadKey(event.attributes));
}

function attributesHavePayloadKey(attributes: Attributes | undefined): boolean {
  return attributes !== undefined && Object.keys(attributes).some(isPayloadKey);
}

/** Decode, mask, re-encode: a string result as is, anything else serialized. */
function maskPayloadValue(mask: (value: unknown) => unknown, text: string): string {
  const masked = applyWithFailClosed(mask, decodePayload(text));
  return typeof masked === "string" ? masked : serialize(masked)[0];
}

/** The JSON value when the text is a JSON object or array, else the text. */
function decodePayload(text: string): unknown {
  const stripped = text.trim();
  if (stripped.startsWith("{") || stripped.startsWith("[")) {
    try {
      return JSON.parse(stripped) as unknown;
    } catch {
      return text;
    }
  }
  return text;
}

function errorName(error: unknown): string {
  return error instanceof Error ? error.name : typeof error;
}
