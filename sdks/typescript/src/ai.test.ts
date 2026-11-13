import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { aiTelemetry } from "./ai.js";
import { defaultFilter } from "./filter.js";
import { resetForTests } from "./runtime.js";
import { finished, startMemory } from "./testing.test-helpers.js";

beforeEach(async () => {
  await resetForTests();
});

afterEach(async () => {
  await resetForTests();
});

describe("aiTelemetry", () => {
  it("is disabled without a runtime", () => {
    expect(aiTelemetry({ sessionId: "s1", functionId: "chat" })).toEqual({ isEnabled: false });
  });

  it("returns a tracer of our provider with the context values as metadata", () => {
    startMemory();
    const telemetry = aiTelemetry({
      sessionId: "s1",
      userId: "u1",
      tags: ["prod"],
      metadata: { plan: "pro" },
      functionId: "chat",
    });
    expect(telemetry.isEnabled).toBe(true);
    expect(telemetry.functionId).toBe("chat");
    expect(telemetry.tracer).toBeDefined();
    expect(telemetry.metadata).toEqual({
      plan: "pro",
      sessionId: "s1",
      userId: "u1",
      tags: ["prod"],
    });
  });

  it("omits the keys that were not given", () => {
    startMemory();
    const telemetry = aiTelemetry();
    expect(telemetry.isEnabled).toBe(true);
    expect(telemetry.functionId).toBeUndefined();
    expect(telemetry.metadata).toEqual({});
  });

  it("creates spans of the ai scope that pass the default filter", async () => {
    // Global mode is where the D-F-5 default filter decides a span's fate.
    const { exporter } = startMemory({ useGlobalProvider: true });
    const telemetry = aiTelemetry();
    telemetry.tracer?.startSpan("ai.generateText").end();
    const [span] = await finished(exporter);
    if (span === undefined) {
      throw new Error("expected one exported span");
    }
    expect(span.name).toBe("ai.generateText");
    expect(span.instrumentationScope.name).toBe("ai");
    expect(defaultFilter(span)).toBe(true);
  });
});
