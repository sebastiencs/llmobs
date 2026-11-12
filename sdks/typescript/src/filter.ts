/**
 * The default export filter (D-F-5).
 *
 * Only used in global mode, where the provider we registered may also carry
 * spans from unrelated instrumentations: a span is exported when its scope is
 * ours (`llmobs`) or the Vercel AI SDK's (`ai`), or when one of its attribute
 * keys belongs to a known LLM schema. `exportFilter` replaces this rule.
 */

import type { ReadableSpan } from "@opentelemetry/sdk-trace";

/** Scope names whose spans are always ours to export. */
const EXPORTED_SCOPES: ReadonlySet<string> = new Set(["llmobs", "ai"]);

const ATTRIBUTE_PREFIXES: readonly string[] = [
  "llmobs.",
  "gen_ai.",
  "llm.",
  "openinference.",
  "ai.",
  "traceloop.",
  "langfuse.",
  "lmnr.",
  "gcp.vertex.agent.",
];

/** Keys the schemas use for top-level payloads without a namespace. */
const EXACT_ATTRIBUTE_KEYS: ReadonlySet<string> = new Set(["input.value", "output.value"]);

/** D-F-5: export a span of our (global) provider or drop it as foreign noise. */
export function defaultFilter(span: ReadableSpan): boolean {
  if (EXPORTED_SCOPES.has(span.instrumentationScope.name)) {
    return true;
  }
  for (const key of Object.keys(span.attributes)) {
    if (
      EXACT_ATTRIBUTE_KEYS.has(key) ||
      ATTRIBUTE_PREFIXES.some((prefix) => key.startsWith(prefix))
    ) {
      return true;
    }
  }
  return false;
}
