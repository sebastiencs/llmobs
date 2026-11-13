import { type ExportResult, ExportResultCode } from "@opentelemetry/core";
import { ProtobufTraceSerializer } from "@opentelemetry/otlp-transformer";
import {
  InMemorySpanExporter,
  type ReadableSpan,
  SimpleSpanProcessor,
  TracerProvider,
} from "@opentelemetry/sdk-trace";
import { describe, expect, it, vi } from "vitest";

import { FetchTraceExporter } from "./exporter.js";
import { resetWarningsForTests } from "./log.js";
import { type FetchLike, type ResponseLike, runtimeGlobals } from "./web.js";

const URL_UNDER_TEST = "http://127.0.0.1:9/v1/traces";
const API_KEY = "sk-llmobs-test";

interface RecordedCall {
  url: string;
  method: string;
  headers: Record<string, string>;
  body: Uint8Array | string;
  keepalive: boolean;
  signal: unknown;
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

function recordCall(url: string, init: Parameters<FetchLike>[1]): RecordedCall {
  return {
    url,
    method: init.method,
    headers: { ...init.headers },
    body: init.body,
    keepalive: "keepalive" in init,
    signal: init.signal,
  };
}

/** An exporter wired to a fake `fetch` answering from `script` and a fake sleep. */
function makeExporter(script: ReadonlyArray<ScriptedAnswer>): {
  calls: RecordedCall[];
  sleeps: number[];
  exporter: FetchTraceExporter;
} {
  const calls: RecordedCall[] = [];
  const sleeps: number[] = [];
  const fetch: FetchLike = (url, init) => {
    const answer = script[calls.length];
    if (answer === undefined) {
      return Promise.reject(new Error(`unexpected fetch call ${String(calls.length + 1)}`));
    }
    calls.push(recordCall(url, init));
    return answer instanceof Error ? Promise.reject(answer) : Promise.resolve(answer);
  };
  const exporter = new FetchTraceExporter({
    url: URL_UNDER_TEST,
    apiKey: API_KEY,
    fetch,
    sleep: (ms) => {
      sleeps.push(ms);
      return Promise.resolve();
    },
    now: () => 1_000_000,
  });
  return { calls, sleeps, exporter };
}

function exportSpans(exporter: FetchTraceExporter, spans: ReadableSpan[]): Promise<ExportResult> {
  return new Promise((resolve) => {
    exporter.export(spans, resolve);
  });
}

/** Finished spans from a throwaway provider, exported through memory. */
function finishedSpans(): ReadableSpan[] {
  const memory = new InMemorySpanExporter();
  const provider = new TracerProvider({
    spanProcessors: [new SimpleSpanProcessor({ exporter: memory })],
  });
  const tracer = provider.getTracer("llmobs", "0.0.0");
  const span = tracer.startSpan("hello", { attributes: { "llmobs.span.kind": "chain" } });
  span.setAttribute("llmobs.input", "hi");
  span.end();
  return memory.getFinishedSpans();
}

async function gunzip(bytes: Uint8Array): Promise<Uint8Array> {
  const { Blob, DecompressionStream, Response } = runtimeGlobals();
  if (Blob === undefined || DecompressionStream === undefined || Response === undefined) {
    throw new Error("this runtime has no DecompressionStream");
  }
  const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream("gzip"));
  const buffer = await new Response(stream).arrayBuffer();
  return new Uint8Array(buffer);
}

function asBytes(body: Uint8Array | string): Uint8Array {
  if (!(body instanceof Uint8Array)) {
    throw new Error("body is not bytes");
  }
  return body;
}

describe("FetchTraceExporter", () => {
  it("sends gzipped protobuf with auth, content type and URL", async () => {
    const { calls, sleeps, exporter } = makeExporter([response(200)]);
    const result = await exportSpans(exporter, finishedSpans());
    expect(result.code).toBe(ExportResultCode.SUCCESS);
    expect(result.error).toBeUndefined();
    expect(calls).toHaveLength(1);
    expect(calls[0]?.url).toBe(URL_UNDER_TEST);
    expect(calls[0]?.method).toBe("POST");
    expect(calls[0]?.headers).toEqual({
      "Content-Type": "application/x-protobuf",
      Authorization: `Bearer ${API_KEY}`,
      "Content-Encoding": "gzip",
    });
    expect(calls[0]?.keepalive).toBe(false);
    expect(calls[0]?.signal).toBeDefined();
    expect(sleeps).toEqual([]);
  });

  it("sends the serializer's bytes: the gunzipped body is byte-identical", async () => {
    const { calls, exporter } = makeExporter([response(200)]);
    const spans = finishedSpans();
    await exportSpans(exporter, spans);
    const expected = ProtobufTraceSerializer.serializeRequest(spans);
    if (expected === undefined) {
      throw new Error("the serializer returned nothing");
    }
    expect(await gunzip(asBytes(calls[0]?.body ?? ""))).toEqual(expected);
  });

  it("sends no Content-Encoding and the raw body when gzip is unavailable", async () => {
    const previous = globalThis.CompressionStream;
    Object.defineProperty(globalThis, "CompressionStream", {
      value: undefined,
      configurable: true,
    });
    try {
      const { calls, exporter } = makeExporter([response(200)]);
      const spans = finishedSpans();
      const result = await exportSpans(exporter, spans);
      expect(result.code).toBe(ExportResultCode.SUCCESS);
      expect(calls[0]?.headers["Content-Encoding"]).toBeUndefined();
      const expected = ProtobufTraceSerializer.serializeRequest(spans);
      if (expected === undefined) {
        throw new Error("the serializer returned nothing");
      }
      expect(asBytes(calls[0]?.body ?? "")).toEqual(expected);
    } finally {
      Object.defineProperty(globalThis, "CompressionStream", {
        value: previous,
        configurable: true,
      });
    }
  });

  it("retries 503 twice with 1 s and 2 s waits, then succeeds", async () => {
    const { calls, sleeps, exporter } = makeExporter([response(503), response(503), response(200)]);
    const result = await exportSpans(exporter, finishedSpans());
    expect(result.code).toBe(ExportResultCode.SUCCESS);
    expect(calls).toHaveLength(3);
    expect(sleeps).toEqual([1000, 2000]);
  });

  it("waits Retry-After seconds when the header is a non-negative integer", async () => {
    const { sleeps, exporter } = makeExporter([
      response(429, { "Retry-After": "3" }),
      response(200),
    ]);
    const result = await exportSpans(exporter, finishedSpans());
    expect(result.code).toBe(ExportResultCode.SUCCESS);
    expect(sleeps).toEqual([3000]);
  });

  it("caps a large Retry-After at 30 s", async () => {
    const { sleeps, exporter } = makeExporter([
      response(503, { "Retry-After": "120" }),
      response(200),
    ]);
    const result = await exportSpans(exporter, finishedSpans());
    expect(result.code).toBe(ExportResultCode.SUCCESS);
    expect(sleeps).toEqual([30_000]);
  });

  it("fails without retry on another non-2xx status", async () => {
    const { calls, sleeps, exporter } = makeExporter([response(400)]);
    const result = await exportSpans(exporter, finishedSpans());
    expect(result.code).toBe(ExportResultCode.FAILED);
    expect(result.error?.message).toBe("http 400");
    expect(calls).toHaveLength(1);
    expect(sleeps).toEqual([]);
  });

  it("retries a rejected fetch (network error)", async () => {
    const { calls, sleeps, exporter } = makeExporter([new Error("fetch failed"), response(200)]);
    const result = await exportSpans(exporter, finishedSpans());
    expect(result.code).toBe(ExportResultCode.SUCCESS);
    expect(calls).toHaveLength(2);
    expect(sleeps).toEqual([1000]);
  });

  it("fails after 4 attempts and calls the callback exactly once", async () => {
    const { calls, sleeps, exporter } = makeExporter([
      response(503),
      new Error("fetch failed"),
      response(502),
      response(504),
    ]);
    const seen: ExportResult[] = [];
    const done = new Promise<void>((resolve) => {
      exporter.export(finishedSpans(), (result) => {
        seen.push(result);
        resolve();
      });
    });
    await done;
    await Promise.resolve();
    expect(seen).toHaveLength(1);
    expect(seen[0]?.code).toBe(ExportResultCode.FAILED);
    expect(seen[0]?.error?.message).toBe("http 504");
    expect(calls).toHaveLength(4);
    expect(sleeps).toEqual([1000, 2000, 4000]);
  });

  it("does not retry when the wait would end past the 30 s deadline", async () => {
    let clock = 0;
    const calls: RecordedCall[] = [];
    const sleeps: number[] = [];
    const fetch: FetchLike = (url, init) => {
      calls.push(recordCall(url, init));
      return Promise.resolve(response(429, { "Retry-After": "29" }));
    };
    const exporter = new FetchTraceExporter({
      url: URL_UNDER_TEST,
      apiKey: API_KEY,
      fetch,
      sleep: (ms) => {
        sleeps.push(ms);
        clock += ms;
        return Promise.resolve();
      },
      now: () => clock,
    });
    const result = await exportSpans(exporter, finishedSpans());
    expect(result.code).toBe(ExportResultCode.FAILED);
    expect(result.error?.message).toBe("http 429");
    expect(calls).toHaveLength(2);
    expect(sleeps).toEqual([29_000]);
  });

  it("caps each attempt's timeout at the time left before the deadline", async () => {
    const timeouts: number[] = [];
    const previous = globalThis.AbortSignal;
    Object.defineProperty(globalThis, "AbortSignal", {
      value: {
        timeout: (ms: number) => {
          timeouts.push(ms);
          return { ms };
        },
      },
      configurable: true,
    });
    let clock = 0;
    const fetch: FetchLike = () => {
      clock += 25_000; // each attempt burns most of the 30 s budget
      return Promise.resolve(response(503));
    };
    try {
      const exporter = new FetchTraceExporter({
        url: URL_UNDER_TEST,
        apiKey: API_KEY,
        fetch,
        timeoutMillis: 10_000,
        sleep: (ms) => {
          clock += ms;
          return Promise.resolve();
        },
        now: () => clock,
      });
      const result = await exportSpans(exporter, finishedSpans());
      expect(result.code).toBe(ExportResultCode.FAILED);
      // Attempt 1 sees the full 10 s; attempt 2 only the 4 s left before the
      // deadline (25 s burned by attempt 1, 1 s of backoff).
      expect(timeouts).toEqual([10_000, 4_000]);
    } finally {
      Object.defineProperty(globalThis, "AbortSignal", {
        value: previous,
        configurable: true,
      });
    }
  });

  it("forceFlush resolves only after an in-flight export settles", async () => {
    const calls: RecordedCall[] = [];
    let release: (() => void) | undefined;
    const gate = new Promise<void>((resolve) => {
      release = resolve;
    });
    const fetch: FetchLike = (url, init) => {
      calls.push(recordCall(url, init));
      return gate.then(() => response(200));
    };
    const exporter = new FetchTraceExporter({
      url: URL_UNDER_TEST,
      apiKey: API_KEY,
      fetch,
      sleep: () => Promise.resolve(),
      now: () => 1_000_000,
    });
    let settled = false;
    exporter.export(finishedSpans(), () => {
      settled = true;
    });
    let flushed = false;
    const flushing = exporter.forceFlush().then(() => {
      flushed = true;
    });
    await vi.waitFor(() => expect(calls).toHaveLength(1));
    expect(flushed).toBe(false);
    expect(settled).toBe(false);
    release?.();
    await flushing;
    expect(flushed).toBe(true);
    expect(settled).toBe(true);
  });

  it("refuses exports after shutdown", async () => {
    const { calls, exporter } = makeExporter([response(200), response(200)]);
    const first = await exportSpans(exporter, finishedSpans());
    expect(first.code).toBe(ExportResultCode.SUCCESS);
    await exporter.shutdown();
    const second = await exportSpans(exporter, finishedSpans());
    expect(second.code).toBe(ExportResultCode.FAILED);
    expect(second.error?.message).toBe("shutdown");
    expect(calls).toHaveLength(1);
  });

  it("fails with one warning when the runtime has no fetch", async () => {
    resetWarningsForTests();
    const previous = globalThis.fetch;
    Object.defineProperty(globalThis, "fetch", { value: undefined, configurable: true });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const exporter = new FetchTraceExporter({ url: URL_UNDER_TEST, apiKey: API_KEY });
      const first = await exportSpans(exporter, finishedSpans());
      const second = await exportSpans(exporter, finishedSpans());
      expect(first.code).toBe(ExportResultCode.FAILED);
      expect(second.code).toBe(ExportResultCode.FAILED);
      expect(warn).toHaveBeenCalledTimes(1);
      expect(warn).toHaveBeenCalledWith("llmobs: fetch is not available; spans are not exported");
    } finally {
      warn.mockRestore();
      Object.defineProperty(globalThis, "fetch", { value: previous, configurable: true });
    }
  });
});
