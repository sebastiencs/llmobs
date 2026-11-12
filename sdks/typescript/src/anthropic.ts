/**
 * The Anthropic helper (D-F-7): builds the OpenInference instrumentation for
 * the Anthropic SDK and patches the module by hand, so ESM works without
 * loader flags. Pass the result to `init({ instrument: [...] })`; the patch is
 * active immediately, but spans reach llmobs only once `init` has bound our
 * tracer provider.
 *
 * `captureContent` defaults to `true` and is not re-read by `init`: pass the
 * same value as `init`'s `captureContent` option.
 */

import type Anthropic from "@anthropic-ai/sdk";
import { AnthropicInstrumentation } from "@arizeai/openinference-instrumentation-anthropic";

import type { Instrumentation } from "./instrument.js";

export function anthropic(
  module: typeof Anthropic,
  options?: { captureContent?: boolean },
): Instrumentation {
  const capture = options?.captureContent ?? true;
  const instrumentation = new AnthropicInstrumentation({
    instrumentationConfig: { enabled: false },
    traceConfig: { hideInputs: !capture, hideOutputs: !capture },
  });
  instrumentation.manuallyInstrument(module);
  return instrumentation;
}
