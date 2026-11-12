import { diag } from "@opentelemetry/api";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { applyMask, MASK_FAILED } from "./mask.js";
import { resetForTests } from "./runtime.js";
import { startMemory } from "./testing.test-helpers.js";

beforeEach(async () => {
  await resetForTests();
});

afterEach(async () => {
  await resetForTests();
});

describe("applyMask", () => {
  it("returns the value unchanged without a runtime", () => {
    const value = { email: "alice@example.com" };
    expect(applyMask(value)).toBe(value);
  });

  it("returns the value unchanged when the runtime has no mask", () => {
    startMemory();
    const value = { email: "alice@example.com" };
    expect(applyMask(value)).toBe(value);
  });

  it("applies the configured mask", () => {
    startMemory({
      mask: (value) => (typeof value === "string" ? value.toUpperCase() : value),
    });
    expect(applyMask("secret")).toBe("SECRET");
  });

  it("fails closed with the marker when the mask raises", () => {
    startMemory({
      mask: (value): unknown => {
        if (typeof value === "string") {
          throw new RangeError("no");
        }
        return value;
      },
    });
    const debug = vi.spyOn(diag, "debug").mockImplementation(() => {});
    try {
      expect(applyMask("secret")).toBe(MASK_FAILED);
      expect(debug).toHaveBeenCalledWith("llmobs: mask raised RangeError");
    } finally {
      debug.mockRestore();
    }
  });
});
