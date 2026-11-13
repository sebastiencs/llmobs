/**
 * The OpenAI helper (D-F-7): builds the OpenInference instrumentation for the
 * OpenAI SDK and patches the module by hand, so ESM works without loader
 * flags. Pass the result to `init({ instrument: [...] })`; the patch is active
 * immediately, but spans reach llmobs only once `init` has bound our tracer
 * provider.
 *
 * `captureContent` defaults to `true` and is not re-read by `init`: pass the
 * same value as `init`'s `captureContent` option.
 */

import { OpenAIInstrumentation } from "@arizeai/openinference-instrumentation-openai";
import type OpenAI from "openai";

import type { Instrumentation } from "./instrument.js";

export function openai(
  module: typeof OpenAI,
  options?: { captureContent?: boolean },
): Instrumentation {
  const capture = options?.captureContent ?? true;
  const instrumentation = new OpenAIInstrumentation({
    instrumentationConfig: { enabled: false },
    traceConfig: { hideInputs: !capture, hideOutputs: !capture },
  });
  instrumentation.manuallyInstrument(module);
  return instrumentation;
}
