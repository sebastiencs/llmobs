import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { resolveConfig } from "./config.js";
import { withContext } from "./context.js";
import { flush, installRuntime, Runtime, resetForTests, shutdown } from "./runtime.js";
import { type ScoreBody, type ScoreOptions, score } from "./scores.js";
import type { FetchLike, ResponseLike } from "./web.js";

const BASE_URL = "http://127.0.0.1:9";
const SCORE_URL = `${BASE_URL}/api/v1/scores`;
const TRACE_ID = "0123456789abcdef0123456789abcdef";
const SPAN_ID = "0123456789abcdef";

interface RecordedCall {
  url: string;
  headers: Record<string, string>;
  body: string;
}

type ScriptedAnswer = ResponseLike | Error;

function response(status: number, headers: Record<string, string> = {}): ResponseLike {
  const lower = new Map(Object.entries(headers).map(([k, v]) => [k.toLowerCase(), v] as const));
  return {
    status,
    ok: status >= 200 && status < 300,
    headers: { get: (name: string) => lower.get(name.toLowerCase()) ?? null },
  };
}

/** A `fetch` answering from `script` (200 beyond it) and recording every call. */
function scriptedFetch(script: ReadonlyArray<ScriptedAnswer>): {
  fetch: FetchLike;
  calls: RecordedCall[];
} {
  const calls: RecordedCall[] = [];
  const fetch: FetchLike = (url, init) => {
    const answer = script[calls.length] ?? response(200);
    calls.push({ url, headers: { ...init.headers }, body: String(init.body) });
    return answer instanceof Error ? Promise.reject(answer) : Promise.resolve(answer);
  };
  return { fetch, calls };
}

/** A runtime whose score queue uses `fetch` and a recording sleep. */
function startScores(
  fetch: FetchLike,
  options?: { now?: () => number; mask?: (value: unknown) => unknown },
): { sleeps: number[]; runtime: Runtime } {
  const sleeps: number[] = [];
  const config = resolveConfig(
    { apiKey: "sk-llmobs-test", baseUrl: BASE_URL, mask: options?.mask },
    { OTEL_SERVICE_NAME: "conformance" },
  );
  const runtime = Runtime.create(config, {
    fetch,
    scoreSleep: (ms) => {
      sleeps.push(ms);
      return Promise.resolve();
    },
    scoreNow: options?.now,
  });
  installRuntime(runtime);
  return { sleeps, runtime };
}

/** The `scores` array of a recorded call's JSON body. */
function bodies(call: RecordedCall | undefined): ScoreBody[] {
  if (call === undefined) {
    throw new Error("no request was recorded");
  }
  return (JSON.parse(call.body) as { scores: ScoreBody[] }).scores;
}

function spyWarn() {
  return vi.spyOn(console, "warn").mockImplementation(() => {});
}

beforeEach(async () => {
  vi.useFakeTimers();
  await resetForTests();
});

afterEach(async () => {
  await resetForTests();
  vi.useRealTimers();
});

describe("score()", () => {
  it("sends a span target with a numeric value, URL and headers", async () => {
    const { fetch, calls } = scriptedFetch([]);
    startScores(fetch);
    const id = score({ name: "accuracy", value: 0.8, traceId: TRACE_ID, spanId: SPAN_ID });
    expect(id).toMatch(/^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    await flush();
    expect(calls).toHaveLength(1);
    expect(calls[0]?.url).toBe(SCORE_URL);
    expect(calls[0]?.headers).toEqual({
      "Content-Type": "application/json",
      Authorization: "Bearer sk-llmobs-test",
    });
    expect(bodies(calls[0])).toEqual([
      {
        score_id: id,
        target: { type: "span", trace_id: TRACE_ID, span_id: SPAN_ID },
        name: "accuracy",
        value: { data_type: "numeric", value: 0.8 },
        source: "sdk",
      },
    ]);
  });

  it("sends a trace target with a boolean value, reason, user_id and metadata", async () => {
    const { fetch, calls } = scriptedFetch([]);
    startScores(fetch);
    score({
      name: "user_feedback",
      value: true,
      traceId: TRACE_ID,
      reason: "thumbs up",
      userId: "u-17",
      metadata: { k: "v", n: 3, b: true },
    });
    await flush();
    expect(bodies(calls[0])).toEqual([
      {
        score_id: expect.any(String),
        target: { type: "trace", trace_id: TRACE_ID },
        name: "user_feedback",
        value: { data_type: "boolean", value: true },
        source: "sdk",
        reason: "thumbs up",
        user_id: "u-17",
        metadata: { k: "v", n: "3", b: "true" },
      },
    ]);
  });

  it("sends a session target with a categorical value", async () => {
    const { fetch, calls } = scriptedFetch([]);
    startScores(fetch);
    score({ name: "quality", value: "good", sessionId: "chat-42" });
    await flush();
    expect(bodies(calls[0])).toEqual([
      {
        score_id: expect.any(String),
        target: { type: "session", session_id: "chat-42" },
        name: "quality",
        value: { data_type: "categorical", value: { label: "good" } },
        source: "sdk",
      },
    ]);
  });

  it("encodes text through dataType and forces numeric, boolean and categorical", async () => {
    const { fetch, calls } = scriptedFetch([]);
    startScores(fetch);
    score({ name: "note", value: "fix the math", sessionId: "s", dataType: "text" });
    score({ name: "len", value: 3, sessionId: "s", dataType: "numeric" });
    score({ name: "flag", value: true, sessionId: "s", dataType: "boolean" });
    score({ name: "grade", value: "good", sessionId: "s", dataType: "categorical" });
    await flush();
    expect(bodies(calls[0]).map((body) => body.value)).toEqual([
      { data_type: "text", value: "fix the math" },
      { data_type: "numeric", value: 3 },
      { data_type: "boolean", value: true },
      { data_type: "categorical", value: { label: "good" } },
    ]);
  });

  it("sends a passed score_id lowercased and returns it", async () => {
    const { fetch, calls } = scriptedFetch([]);
    startScores(fetch);
    const id = score({
      name: "accuracy",
      value: 1,
      sessionId: "s",
      scoreId: "01939DE1-8F0A-7A2B-9C3D-2E4F5A6B7C8D",
    });
    expect(id).toBe("01939de1-8f0a-7a2b-9c3d-2e4f5a6b7c8d");
    await flush();
    expect(bodies(calls[0])[0]?.score_id).toBe(id);
  });

  it("takes user_id from withContext unless passed, and omits it outside", async () => {
    const { fetch, calls } = scriptedFetch([]);
    startScores(fetch);
    withContext({ userId: "u-9" }, () => {
      score({ name: "a", value: 1, sessionId: "s" });
      score({ name: "b", value: 1, sessionId: "s", userId: "u-2" });
    });
    score({ name: "c", value: 1, sessionId: "s" });
    await flush();
    const sent = bodies(calls[0]);
    expect(sent.map((body) => body.user_id)).toEqual(["u-9", "u-2", undefined]);
  });

  it("masks reason and metadata values", async () => {
    const { fetch, calls } = scriptedFetch([]);
    const mask = (value: unknown): unknown =>
      typeof value === "string" ? value.replaceAll("alice@example.com", "[redacted]") : value;
    startScores(fetch, { mask });
    score({
      name: "m",
      value: 1,
      sessionId: "s",
      reason: "alice@example.com",
      metadata: { email: "alice@example.com", n: 1 },
    });
    await flush();
    const [body] = bodies(calls[0]);
    expect(body?.reason).toBe("[redacted]");
    expect(body?.metadata).toEqual({ email: "[redacted]", n: "1" });
  });

  it("serializes a mask result for reason that is not a string", async () => {
    // Python's test_mask_non_string_reason_is_serialized pins the same rule.
    const { fetch, calls } = scriptedFetch([]);
    startScores(fetch, { mask: () => 42 });
    score({ name: "m", value: 1, sessionId: "s", reason: "why" });
    await flush();
    expect(bodies(calls[0])[0]?.reason).toBe("42");
  });
});

describe("score() validation", () => {
  const INVALID: ReadonlyArray<[ScoreOptions, string]> = [
    [{ name: "", value: 1, sessionId: "s" }, "empty name"],
    [{ name: "a", value: 1 }, "no target"],
    [{ name: "a", value: 1, spanId: SPAN_ID }, "span_id without trace_id"],
    [{ name: "a", value: 1, traceId: "not-32-hex" }, "invalid trace_id"],
    [{ name: "a", value: 1, traceId: TRACE_ID.toUpperCase(), spanId: SPAN_ID }, "invalid trace_id"],
    [{ name: "a", value: 1, traceId: TRACE_ID, spanId: "nothex" }, "invalid span_id"],
    [{ name: "a", value: 1, sessionId: "" }, "invalid session_id"],
    [{ name: "a", value: 1, sessionId: "s", scoreId: "not-a-uuid" }, "invalid score_id"],
    [{ name: "a", value: Number.POSITIVE_INFINITY, sessionId: "s" }, "non-finite value"],
    [{ name: "a", value: Number.NaN, sessionId: "s" }, "non-finite value"],
    [{ name: "a", value: "x", sessionId: "s", dataType: "numeric" }, "data_type mismatch"],
    [{ name: "a", value: 1, sessionId: "s", dataType: "boolean" }, "data_type mismatch"],
    [{ name: "a", value: 1, sessionId: "s", dataType: "text" }, "data_type mismatch"],
  ];

  it.each(INVALID)("rejects with %s", async (options, reason) => {
    const { fetch, calls } = scriptedFetch([]);
    startScores(fetch);
    const warn = spyWarn();
    try {
      expect(score(options)).toBeUndefined();
      expect(warn).toHaveBeenCalledTimes(1);
      expect(warn).toHaveBeenCalledWith(`llmobs: score not queued: ${reason}`);
      await flush();
      expect(calls).toHaveLength(0);
    } finally {
      warn.mockRestore();
    }
  });

  it("checks the reasons in order", async () => {
    const { fetch } = scriptedFetch([]);
    startScores(fetch);
    const warn = spyWarn();
    try {
      score({ name: "", value: 1 });
      score({ name: "a", value: 1, spanId: "nothex" });
      score({ name: "a", value: Number.NaN, sessionId: "", scoreId: "bad" });
      expect(warn.mock.calls.map(([message]) => message)).toEqual([
        "llmobs: score not queued: empty name",
        "llmobs: score not queued: span_id without trace_id",
        "llmobs: score not queued: invalid session_id",
      ]);
    } finally {
      warn.mockRestore();
    }
  });
});

describe("score() lifecycle", () => {
  it("returns undefined without a runtime and stays silent", () => {
    const warn = spyWarn();
    try {
      expect(score({ name: "a", value: 1, sessionId: "s" })).toBeUndefined();
      expect(warn).not.toHaveBeenCalled();
    } finally {
      warn.mockRestore();
    }
  });

  it("returns undefined after shutdown", async () => {
    const { fetch, calls } = scriptedFetch([]);
    startScores(fetch);
    await shutdown();
    const warn = spyWarn();
    try {
      expect(score({ name: "a", value: 1, sessionId: "s" })).toBeUndefined();
      expect(warn).not.toHaveBeenCalled();
      await vi.advanceTimersByTimeAsync(10_000);
      expect(calls).toHaveLength(0);
    } finally {
      warn.mockRestore();
    }
  });

  it("warns once and returns undefined when crypto is missing", () => {
    const { fetch } = scriptedFetch([]);
    startScores(fetch);
    const previous = globalThis.crypto;
    Object.defineProperty(globalThis, "crypto", { value: undefined, configurable: true });
    const warn = spyWarn();
    try {
      expect(score({ name: "a", value: 1, sessionId: "s" })).toBeUndefined();
      expect(score({ name: "b", value: 1, sessionId: "s" })).toBeUndefined();
      expect(warn).toHaveBeenCalledTimes(1);
      expect(warn).toHaveBeenCalledWith("llmobs: crypto is not available; scores are not queued");
    } finally {
      warn.mockRestore();
      Object.defineProperty(globalThis, "crypto", { value: previous, configurable: true });
    }
  });
});

describe("the score queue", () => {
  it("batches 2500 scores as 1000, 1000 and 500", async () => {
    const { fetch, calls } = scriptedFetch([]);
    startScores(fetch);
    for (let i = 0; i < 2_500; i++) {
      score({ name: `n${String(i)}`, value: i, sessionId: "s" });
    }
    await flush();
    expect(calls.map((call) => bodies(call).length)).toEqual([1_000, 1_000, 500]);
    expect(calls[0]?.url).toBe(SCORE_URL);
  });

  it("sends one batch per tick, and none while one is in flight", async () => {
    const calls: RecordedCall[] = [];
    let release: (() => void) | undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const fetch: FetchLike = (url, init) => {
      calls.push({ url, headers: { ...init.headers }, body: String(init.body) });
      return calls.length === 1 ? gate.then(() => response(200)) : Promise.resolve(response(200));
    };
    startScores(fetch);
    for (let i = 0; i < 1_500; i++) {
      score({ name: `n${String(i)}`, value: i, sessionId: "s" });
    }
    await vi.advanceTimersByTimeAsync(1_000);
    expect(calls).toHaveLength(1);
    // Ticks at 2 s, 3 s and 4 s find a batch in flight and do nothing.
    await vi.advanceTimersByTimeAsync(3_000);
    expect(calls).toHaveLength(1);
    release?.();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(calls.map((call) => bodies(call).length)).toEqual([1_000, 500]);
  });

  it("retries a 503 batch and resends the same bodies", async () => {
    const { fetch, calls } = scriptedFetch([response(503), response(200)]);
    const { sleeps } = startScores(fetch);
    score({ name: "a", value: 1, sessionId: "s" });
    score({ name: "b", value: 2, sessionId: "s" });
    await flush();
    expect(calls).toHaveLength(2);
    expect(calls[1]?.body).toBe(calls[0]?.body);
    expect(sleeps).toEqual([1_000]);
  });

  it("retries a 429 with Retry-After seconds", async () => {
    const { fetch, calls } = scriptedFetch([response(429, { "Retry-After": "2" }), response(200)]);
    const { sleeps } = startScores(fetch);
    score({ name: "a", value: 1, sessionId: "s" });
    await flush();
    expect(calls).toHaveLength(2);
    expect(sleeps).toEqual([2_000]);
  });

  it("caps a large Retry-After at 30 s", async () => {
    const { fetch } = scriptedFetch([response(503, { "Retry-After": "120" }), response(200)]);
    const { sleeps } = startScores(fetch);
    score({ name: "a", value: 1, sessionId: "s" });
    await flush();
    expect(sleeps).toEqual([30_000]);
  });

  it("uses the default backoff for an HTTP-date Retry-After", async () => {
    const { fetch } = scriptedFetch([
      response(503, { "Retry-After": "Wed, 21 Oct 2015 07:28:00 GMT" }),
      response(200),
    ]);
    const { sleeps } = startScores(fetch);
    score({ name: "a", value: 1, sessionId: "s" });
    await flush();
    expect(sleeps).toEqual([1_000]);
  });

  it("retries a rejected fetch (network error)", async () => {
    const { fetch, calls } = scriptedFetch([new Error("fetch failed"), response(200)]);
    const { sleeps } = startScores(fetch);
    score({ name: "a", value: 1, sessionId: "s" });
    await flush();
    expect(calls).toHaveLength(2);
    expect(sleeps).toEqual([1_000]);
  });

  it("drops the batch after 4 attempts with one warning", async () => {
    const { fetch, calls } = scriptedFetch([
      response(500),
      response(500),
      response(500),
      response(500),
    ]);
    const { sleeps } = startScores(fetch);
    score({ name: "a", value: 1, sessionId: "s" });
    score({ name: "b", value: 2, sessionId: "s" });
    const warn = spyWarn();
    try {
      await flush();
      expect(calls).toHaveLength(4);
      expect(sleeps).toEqual([1_000, 2_000, 4_000]);
      expect(warn).toHaveBeenCalledTimes(1);
      expect(warn).toHaveBeenCalledWith(
        "llmobs: score batch failed after retries; dropped 2 scores",
      );
    } finally {
      warn.mockRestore();
    }
  });

  it("drops the batch at once on a 400 with one warning", async () => {
    const { fetch, calls } = scriptedFetch([response(400)]);
    const { sleeps } = startScores(fetch);
    score({ name: "a", value: 1, sessionId: "s" });
    const warn = spyWarn();
    try {
      await flush();
      expect(calls).toHaveLength(1);
      expect(sleeps).toEqual([]);
      expect(warn).toHaveBeenCalledTimes(1);
      expect(warn).toHaveBeenCalledWith(
        "llmobs: score batch rejected with status 400; dropped 1 scores",
      );
    } finally {
      warn.mockRestore();
    }
  });

  it("drops beyond 10 000 buffered scores, warning once per minute", () => {
    const { fetch } = scriptedFetch([]);
    let clock = 0;
    startScores(fetch, { now: () => clock });
    const warn = spyWarn();
    try {
      const ids: (string | undefined)[] = [];
      for (let i = 0; i < 10_000; i++) {
        ids.push(score({ name: `n${String(i)}`, value: i, sessionId: "s" }));
      }
      expect(ids[9_999]).toBeDefined();
      expect(ids.filter((id) => id === undefined)).toHaveLength(0);
      // The first drop warns at once; the three next ones only add to the count.
      for (let i = 0; i < 4; i++) {
        expect(score({ name: `x${String(i)}`, value: 0, sessionId: "s" })).toBeUndefined();
      }
      expect(warn).toHaveBeenCalledTimes(1);
      expect(warn).toHaveBeenCalledWith(
        "llmobs: score buffer full; dropped 1 scores in the last minute",
      );
      // A drop a minute later warns again, with everything dropped since.
      clock = 60_000;
      expect(score({ name: "one-more", value: 0, sessionId: "s" })).toBeUndefined();
      expect(warn).toHaveBeenCalledTimes(2);
      expect(warn).toHaveBeenNthCalledWith(
        2,
        "llmobs: score buffer full; dropped 4 scores in the last minute",
      );
    } finally {
      warn.mockRestore();
    }
  });

  it("shutdown flushes the buffer and stops the timer", async () => {
    const { fetch, calls } = scriptedFetch([]);
    startScores(fetch);
    score({ name: "a", value: 1, sessionId: "s" });
    await shutdown();
    expect(calls).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(calls).toHaveLength(1);
    expect(score({ name: "late", value: 1, sessionId: "s" })).toBeUndefined();
  });
});
