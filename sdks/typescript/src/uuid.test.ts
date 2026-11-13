import { describe, expect, it } from "vitest";

import { uuid7 } from "./uuid.js";

/** Lowercase UUID with version 7 and the RFC variant bits. */
const UUIDV7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

/** The 48-bit Unix millisecond prefix of an id. */
function timestampMs(id: string): number {
  return Number.parseInt(`${id.slice(0, 8)}${id.slice(9, 13)}`, 16);
}

describe("uuid7", () => {
  it("produces lowercase UUIDv7 strings", () => {
    for (let i = 0; i < 100; i++) {
      expect(uuid7()).toMatch(UUIDV7);
    }
  });

  it("embeds the current Unix millisecond timestamp", () => {
    const before = Date.now();
    const id = uuid7();
    const after = Date.now();
    expect(timestampMs(id)).toBeGreaterThanOrEqual(before);
    expect(timestampMs(id)).toBeLessThanOrEqual(after);
  });

  it("returns 1000 unique ids with a non-decreasing timestamp", () => {
    const ids: string[] = [];
    let previous = 0;
    for (let i = 0; i < 1000; i++) {
      const id = uuid7();
      ids.push(id);
      const ms = timestampMs(id);
      expect(ms).toBeGreaterThanOrEqual(previous);
      previous = ms;
    }
    expect(new Set(ids).size).toBe(1000);
  });

  it("throws when the runtime has no crypto", () => {
    const previous = globalThis.crypto;
    Object.defineProperty(globalThis, "crypto", { value: undefined, configurable: true });
    try {
      expect(() => uuid7()).toThrow("crypto is not available");
    } finally {
      Object.defineProperty(globalThis, "crypto", { value: previous, configurable: true });
    }
  });
});
