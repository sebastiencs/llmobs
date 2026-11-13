/**
 * Third-party instrumentations wired into our runtime (D-F-7).
 *
 * `instrument` takes any object with `setTracerProvider`, `enable` and
 * `disable` functions (the OpenTelemetry `Instrumentation` shape), so the SDK
 * stays independent of the OpenInference packages: the subpath helpers
 * `llmobs/openai`, `llmobs/anthropic` and `llmobs/langchain` build one from
 * the matching optional peer dependency. Nothing here raises into the app:
 * a failing instrumentation is logged at debug level and skipped.
 */

import type { TracerProvider } from "@opentelemetry/api";
import { debug } from "./log.js";
import type { Runtime } from "./runtime.js";

/** What `init({ instrument: [...] })` accepts; OTel instrumentations satisfy it. */
export interface Instrumentation {
  setTracerProvider(provider: TracerProvider): void;
  enable(): void;
  disable(): void;
}

/** The instrumentations the installed runtime enabled, so shutdown can disable them. */
let enabled: readonly Instrumentation[] = [];

/**
 * Bind and enable the config's instrumentations: our tracer provider first,
 * then `enable()`. A throwing item is logged and skipped; the rest proceed.
 */
export function enableInstrumentations(runtime: Runtime): void {
  const items: Instrumentation[] = [];
  for (const item of runtime.config.instrument) {
    try {
      item.setTracerProvider(runtime.provider);
      item.enable();
      items.push(item);
    } catch (error) {
      debug(`llmobs: instrumentation skipped after ${errorName(error)}`);
    }
  }
  enabled = items;
}

/** Disable everything `enableInstrumentations` enabled, then forget the list. */
export function disableInstrumentations(): void {
  const items = enabled;
  enabled = [];
  for (const item of items) {
    try {
      item.disable();
    } catch (error) {
      debug(`llmobs: instrumentation disable failed: ${errorName(error)}`);
    }
  }
}

function errorName(error: unknown): string {
  return error instanceof Error ? error.name : typeof error;
}
