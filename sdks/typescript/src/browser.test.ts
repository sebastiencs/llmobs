import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

import { type FeedbackOptions, feedback } from "./browser.js";
import type { FetchLike, ResponseLike } from "./web.js";

const BASE_URL = "http://127.0.0.1:9";
const FEEDBACK_URL = `${BASE_URL}/api/public/feedback`;
const TRACE_ID = "0123456789abcdef0123456789abcdef";
const SPAN_ID = "0123456789abcdef";
const PUBLIC_KEY = "pk-llmobs-public";

interface RecordedCall {
  url: string;
  headers: Record<string, string>;
  body: string;
  keepalive: boolean | undefined;
}

function response(status: number): ResponseLike {
  return { status, ok: status >= 200 && status < 300, headers: { get: () => null } };
}

/** A `fetch` answering `first` once (then 200) and recording every call. */
function recordingFetch(first?: ResponseLike | Error): { fetch: FetchLike; calls: RecordedCall[] } {
  const calls: RecordedCall[] = [];
  const fetch: FetchLike = (url, init) => {
    const answer = (calls.length === 0 ? first : undefined) ?? response(200);
    calls.push({
      url,
      headers: { ...init.headers },
      body: String(init.body),
      keepalive: init.keepalive,
    });
    return answer instanceof Error ? Promise.reject(answer) : Promise.resolve(answer);
  };
  return { fetch, calls };
}

function options(overrides?: Partial<FeedbackOptions>): FeedbackOptions {
  return { baseUrl: BASE_URL, publicKey: PUBLIC_KEY, traceId: TRACE_ID, value: 5, ...overrides };
}

describe("feedback", () => {
  it("posts the spec 06 body with auth headers and keepalive", async () => {
    const { fetch, calls } = recordingFetch();
    const result = await feedback(
      options({ fetch, spanId: SPAN_ID, name: "thumbs", comment: "great answer" }),
    );
    expect(result).toEqual({ ok: true });
    expect(calls).toEqual([
      {
        url: FEEDBACK_URL,
        headers: {
          "Content-Type": "application/json",
          Authorization: `Bearer ${PUBLIC_KEY}`,
        },
        body: JSON.stringify({
          trace_id: TRACE_ID,
          span_id: SPAN_ID,
          name: "thumbs",
          value: 5,
          comment: "great answer",
        }),
        keepalive: true,
      },
    ]);
  });

  it("omits absent keys", async () => {
    const { fetch, calls } = recordingFetch();
    await feedback(options({ fetch, value: "good" }));
    expect(calls[0]?.body).toBe(`{"trace_id":"${TRACE_ID}","value":"good"}`);
  });

  it("removes trailing slashes from the base URL", async () => {
    const { fetch, calls } = recordingFetch();
    await feedback(options({ fetch, baseUrl: "https://host/prefix///" }));
    expect(calls[0]?.url).toBe("https://host/prefix/api/public/feedback");
  });

  it.each([
    ["a relative baseUrl", { baseUrl: "localhost:8080" }],
    ["a non-http baseUrl", { baseUrl: "ftp://example.com" }],
    ["a baseUrl without a host", { baseUrl: "http://:8080" }],
    ["a baseUrl with a query", { baseUrl: "http://host/?q=1" }],
    ["a baseUrl with a fragment", { baseUrl: "http://host/#f" }],
    ["a baseUrl with a bare query delimiter", { baseUrl: "http://host/?" }],
    ["a baseUrl with a bare fragment delimiter", { baseUrl: "http://host/#" }],
    ["a public key without the pk prefix", { publicKey: "sk-llmobs-test" }],
    ["a too-short trace id", { traceId: "0123456789abcdef0123456789abcd" }],
    ["an uppercase trace id", { traceId: TRACE_ID.toUpperCase() }],
    ["a non-hex trace id", { traceId: `g${TRACE_ID.slice(1)}` }],
    ["a too-short span id", { spanId: "0123456789abcde" }],
    ["an uppercase span id", { spanId: SPAN_ID.toUpperCase() }],
    ["an empty span id", { spanId: "" }],
    ["a NaN value", { value: Number.NaN }],
    ["an Infinity value", { value: Number.POSITIVE_INFINITY }],
    ["a negative Infinity value", { value: Number.NEGATIVE_INFINITY }],
  ])("resolves ok:false without a request for %s", async (_label, override) => {
    const { fetch, calls } = recordingFetch();
    expect(await feedback(options({ ...override, fetch }))).toEqual({ ok: false });
    expect(calls).toEqual([]);
  });

  it("accepts boolean and string values", async () => {
    const first = recordingFetch();
    expect(await feedback(options({ fetch: first.fetch, value: true }))).toEqual({ ok: true });
    const second = recordingFetch();
    expect(await feedback(options({ fetch: second.fetch, value: "good" }))).toEqual({ ok: true });
  });

  it("posts a body of exactly 4096 bytes but not one byte more", async () => {
    // {"trace_id":"…","value":"v","comment":"<comment>"}: each comment char is one byte.
    const overhead = JSON.stringify({ trace_id: TRACE_ID, value: "v", comment: "" }).length;
    const exact = recordingFetch();
    expect(
      await feedback(
        options({ fetch: exact.fetch, value: "v", comment: "a".repeat(4096 - overhead) }),
      ),
    ).toEqual({ ok: true });
    const over = recordingFetch();
    expect(
      await feedback(
        options({ fetch: over.fetch, value: "v", comment: "a".repeat(4097 - overhead) }),
      ),
    ).toEqual({ ok: false });
    expect(over.calls).toEqual([]);
  });

  it("measures the body in bytes, not characters", async () => {
    const { fetch, calls } = recordingFetch();
    // 2100 two-byte characters: 4200 body bytes, well under 4096 characters.
    expect(await feedback(options({ fetch, comment: "é".repeat(2100) }))).toEqual({ ok: false });
    expect(calls).toEqual([]);
  });

  it("resolves ok:false on a non-2xx status", async () => {
    const { fetch, calls } = recordingFetch(response(404));
    expect(await feedback(options({ fetch }))).toEqual({ ok: false });
    expect(calls.length).toBe(1);
  });

  it("resolves ok:false when fetch rejects", async () => {
    const { fetch, calls } = recordingFetch(new Error("network down"));
    expect(await feedback(options({ fetch }))).toEqual({ ok: false });
    expect(calls.length).toBe(1);
  });

  it("resolves ok:false with no global fetch and no injected fetch", async () => {
    const previous = globalThis.fetch;
    Object.defineProperty(globalThis, "fetch", { value: undefined, configurable: true });
    try {
      expect(await feedback(options())).toEqual({ ok: false });
    } finally {
      Object.defineProperty(globalThis, "fetch", { value: previous, configurable: true });
    }
  });
});

describe("browser imports", () => {
  it("imports only ./web.js", () => {
    const source = readFileSync(new URL("./browser.ts", import.meta.url), "utf8");
    const specifiers = [...source.matchAll(/import[\s\S]*?from\s+"([^"]+)";/g)].map((m) => m[1]);
    const sideEffectImports = [...source.matchAll(/^import\s+"[^"]+";$/gm)];
    expect(specifiers).toEqual(["./web.js"]);
    expect(sideEffectImports).toEqual([]);
    expect(source).not.toMatch(/import\s*\(/);
  });
});
