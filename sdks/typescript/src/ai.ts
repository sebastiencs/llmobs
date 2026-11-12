/**
 * `aiTelemetry` (spec 10 § Vercel AI SDK): the Vercel AI SDK sends spans to
 * the global tracer unless it is given one, so this hands out a tracer from
 * the llmobs provider (scope `ai`, which the default export filter keeps,
 * D-F-5) plus the context values as telemetry metadata. Users pass the result
 * as `experimental_telemetry`. Before `init()` (or when disabled) it returns
 * `{ isEnabled: false }` and the AI SDK traces nothing.
 */

import type { AttributeValue, Tracer } from "@opentelemetry/api";

import { currentRuntime } from "./runtime.js";

export interface AiTelemetryOptions {
  sessionId?: string;
  userId?: string;
  tags?: readonly string[];
  metadata?: Readonly<Record<string, string | number | boolean>>;
  functionId?: string;
}

export interface AiTelemetry {
  isEnabled: boolean;
  tracer?: Tracer;
  functionId?: string;
  metadata?: Record<string, AttributeValue>;
}

/** Telemetry settings for the Vercel AI SDK's `experimental_telemetry`. */
export function aiTelemetry(options: AiTelemetryOptions = {}): AiTelemetry {
  const runtime = currentRuntime();
  if (runtime === undefined) {
    return { isEnabled: false };
  }
  const metadata: Record<string, AttributeValue> = { ...options.metadata };
  if (options.sessionId !== undefined) {
    metadata.sessionId = options.sessionId;
  }
  if (options.userId !== undefined) {
    metadata.userId = options.userId;
  }
  if (options.tags !== undefined) {
    metadata.tags = [...options.tags];
  }
  const telemetry: AiTelemetry = {
    isEnabled: true,
    tracer: runtime.provider.getTracer("ai"),
    metadata,
  };
  if (options.functionId !== undefined) {
    telemetry.functionId = options.functionId;
  }
  return telemetry;
}
