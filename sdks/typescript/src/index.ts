export { type AiTelemetry, type AiTelemetryOptions, aiTelemetry } from "./ai.js";
export { ConfigError, type LlmobsOptions } from "./config.js";
export { type ContextOptions, withContext } from "./context.js";
export type { Instrumentation } from "./instrument.js";
export {
  type LlmSpanHandle,
  type LlmSpanOptions,
  type Usage,
  withLlmSpan,
} from "./llm.js";
export { flush, init, shutdown, spanProcessor, tracerProvider } from "./runtime.js";
export { type ScoreOptions, score } from "./scores.js";
export {
  currentSpanId,
  currentTraceId,
  type ObserveOptions,
  observe,
  type SpanHandle,
  type SpanKind,
  type SpanOptions,
  withSpan,
} from "./span.js";
export { uuid7 } from "./uuid.js";

/** Package version. Kept equal to package.json. */
export const VERSION = "0.0.0";
