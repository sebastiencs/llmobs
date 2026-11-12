/**
 * Logging.
 *
 * `warnOnce` deduplicates by key so a repeated condition (a missing key on
 * every `resolveConfig` call, a missing `fetch` on every export) warns once
 * per process. Messages never contain values, keys or user ids (AGENTS.md
 * § Privacy).
 */

import { diag } from "@opentelemetry/api";

import { runtimeGlobals } from "./web.js";

const warned = new Set<string>();

/** Warn `message` the first time `key` is seen in this process. */
export function warnOnce(key: string, message: string): void {
  if (warned.has(key)) {
    return;
  }
  warned.add(key);
  runtimeGlobals().console?.warn(message);
}

/** Debug logging through the OTel diagnostics logger. */
export function debug(message: string): void {
  diag.debug(message);
}

/** Forget the warned keys; tests only. */
export function resetWarningsForTests(): void {
  warned.clear();
}
