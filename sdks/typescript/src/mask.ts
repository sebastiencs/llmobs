/**
 * Capture-time masking (D-F-6).
 *
 * Every value the SDK records (inputs, outputs, metadata values, LLM
 * messages) passes through the configured `mask` before serialization.
 * Without a mask, or without a running runtime, the value is returned
 * unchanged. A mask that raises fails closed: the value becomes the marker
 * and the error name is logged at debug level (never the value).
 */

import { debug } from "./log.js";
import { currentRuntime } from "./runtime.js";

export const MASK_FAILED = "[llmobs: mask failed]";

/** Apply the running runtime's `mask` to `value`. Never throws. */
export function applyMask(value: unknown): unknown {
  const mask = currentRuntime()?.config.mask;
  if (mask === undefined) {
    return value;
  }
  try {
    return mask(value);
  } catch (error) {
    debug(`llmobs: mask raised ${errorName(error)}`);
    return MASK_FAILED;
  }
}

function errorName(error: unknown): string {
  return error instanceof Error ? error.name : typeof error;
}
