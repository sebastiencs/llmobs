import { describe, expect, it } from "vitest";

import { getFetch, gzip, runtimeGlobals, timeoutSignal, utf8Decode, utf8Encode } from "./web.js";

describe("gzip", () => {
  it("round-trips through DecompressionStream", async () => {
    const text = "llmobs traces go brrr — with multi-byte characters 🎉".repeat(50);
    const compressed = await gzip(utf8Encode(text));
    const { Blob, DecompressionStream, Response } = runtimeGlobals();
    if (compressed === undefined) {
      throw new Error("this runtime has no CompressionStream, Response or Blob");
    }
    if (Blob === undefined || DecompressionStream === undefined || Response === undefined) {
      throw new Error("this runtime has no DecompressionStream");
    }
    expect(compressed[0]).toBe(0x1f); // gzip magic number
    expect(compressed[1]).toBe(0x8b);
    const stream = new Blob([compressed]).stream().pipeThrough(new DecompressionStream("gzip"));
    const decompressed = await new Response(stream).arrayBuffer();
    expect(utf8Decode(new Uint8Array(decompressed))).toBe(text);
  });

  it("compresses repeated input to less than its raw size", async () => {
    const text = "a".repeat(10_000);
    const compressed = await gzip(utf8Encode(text));
    expect(compressed?.length ?? Number.POSITIVE_INFINITY).toBeLessThan(text.length);
  });
});

describe("getFetch", () => {
  it("is undefined when globalThis.fetch is removed", () => {
    const previous = globalThis.fetch;
    Object.defineProperty(globalThis, "fetch", { value: undefined, configurable: true });
    try {
      expect(getFetch()).toBeUndefined();
    } finally {
      Object.defineProperty(globalThis, "fetch", { value: previous, configurable: true });
    }
  });

  it("returns the global fetch bound to globalThis", () => {
    const calls: string[] = [];
    const previous = globalThis.fetch;
    const stub = ((url: string) => {
      calls.push(url);
      return Promise.resolve({ status: 200, ok: true, headers: { get: () => null } });
    }) as unknown as typeof globalThis.fetch;
    Object.defineProperty(globalThis, "fetch", { value: stub, configurable: true });
    try {
      const fetch = getFetch();
      expect(typeof fetch).toBe("function");
      void fetch?.("http://127.0.0.1:9/v1/traces", { method: "POST", headers: {}, body: "" });
      expect(calls).toEqual(["http://127.0.0.1:9/v1/traces"]);
    } finally {
      Object.defineProperty(globalThis, "fetch", { value: previous, configurable: true });
    }
  });
});

describe("utf8", () => {
  it("round-trips text with multi-byte characters", () => {
    const text = "héllo 🎉 — tracing";
    expect(utf8Decode(utf8Encode(text))).toBe(text);
    expect(utf8Encode(text)).toBeInstanceOf(Uint8Array);
  });
});

describe("timeoutSignal", () => {
  it("returns a signal when AbortSignal.timeout exists", () => {
    expect(runtimeGlobals().AbortSignal?.timeout).toBeDefined();
    expect(timeoutSignal(60)).toBeDefined();
  });
});
