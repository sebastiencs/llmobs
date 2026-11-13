import {
  context,
  createContextKey,
  ProxyTracerProvider,
  ROOT_CONTEXT,
  TraceFlags,
  trace,
} from "@opentelemetry/api";
import { AsyncLocalStorageContextManager } from "@opentelemetry/context-async-hooks";
import { type ReadableSpan, TracerProvider } from "@opentelemetry/sdk-trace";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { resolveConfig } from "./config.js";
import { VERSION } from "./index.js";
import {
  currentRuntime,
  flush,
  init,
  installRuntime,
  Runtime,
  resetForTests,
  shutdown,
  spanProcessor,
  tracerProvider,
} from "./runtime.js";
import { finished, startMemory } from "./testing.test-helpers.js";
import { type FetchLike, runtimeGlobals } from "./web.js";

const INIT_OPTIONS = { apiKey: "sk-llmobs-test", baseUrl: "http://127.0.0.1:9" } as const;

const MISSING_KEY_WARNING =
  "llmobs: no API key; set LLMOBS_API_KEY, or LLMOBS_ENABLED=false to silence this; tracing is disabled";
const INIT_TWICE_WARNING = "llmobs: init() was already called; this call is ignored";
const GLOBAL_EXISTS_WARNING =
  "llmobs: a global tracer provider is already registered; llmobs keeps its own provider. Add spanProcessor() to your provider to send its spans to llmobs";

interface RecordedCall {
  url: string;
  headers: Record<string, string>;
  body: Uint8Array | string;
}

async function gunzip(body: Uint8Array | string | undefined): Promise<Uint8Array> {
  const { Blob, DecompressionStream, Response } = runtimeGlobals();
  if (
    !(body instanceof Uint8Array) ||
    Blob === undefined ||
    DecompressionStream === undefined ||
    Response === undefined
  ) {
    throw new Error("this runtime cannot gunzip");
  }
  const stream = new Blob([body]).stream().pipeThrough(new DecompressionStream("gzip"));
  const buffer = await new Response(stream).arrayBuffer();
  return new Uint8Array(buffer);
}

beforeEach(async () => {
  await resetForTests();
});

afterEach(async () => {
  await resetForTests();
});

describe("init", () => {
  it("registers one beforeExit listener across two init calls", () => {
    // Must stay the first init() of this file: the once-per-process flag is
    // never cleared by resetForTests (spec 10 § Lifecycle).
    const once = vi.spyOn(process, "once").mockImplementation(() => process);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      init(INIT_OPTIONS);
      init(INIT_OPTIONS);
      const beforeExit = once.mock.calls.filter((call) => String(call[0]) === "beforeExit");
      expect(beforeExit).toHaveLength(1);
    } finally {
      once.mockRestore();
      warn.mockRestore();
    }
  });

  it("installs nothing when disabled", () => {
    init({ enabled: false });
    expect(currentRuntime()).toBeUndefined();
    expect(spanProcessor()).toBeUndefined();
    expect(tracerProvider()).toBeInstanceOf(ProxyTracerProvider);
  });

  it("warns once and installs nothing without an API key", () => {
    vi.stubEnv("LLMOBS_API_KEY", ""); // empty counts as unset
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      init();
      expect(currentRuntime()).toBeUndefined();
      expect(spanProcessor()).toBeUndefined();
      const missing = warn.mock.calls.filter(([message]) => message === MISSING_KEY_WARNING);
      expect(missing).toHaveLength(1);
    } finally {
      warn.mockRestore();
      vi.unstubAllEnvs();
    }
  });

  it("keeps the provider private", () => {
    const globalBefore = trace.getTracerProvider();
    init(INIT_OPTIONS);
    const runtime = currentRuntime();
    expect(runtime).toBeDefined();
    // Own mode: the global provider is untouched (D-F-4).
    expect(trace.getTracerProvider()).toBe(globalBefore);
    expect(globalBefore).toBeInstanceOf(ProxyTracerProvider);
    expect(trace.getTracerProvider()).not.toBe(runtime?.provider);
    expect(tracerProvider()).toBe(runtime?.provider);
    expect(spanProcessor()).toBe(runtime?.processor);
  });

  it("warns and keeps the first runtime on a second init", () => {
    init(INIT_OPTIONS);
    const first = currentRuntime();
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      init({ apiKey: "sk-llmobs-test-2", baseUrl: "http://127.0.0.1:10" });
      expect(warn).toHaveBeenCalledTimes(1);
      expect(warn).toHaveBeenCalledWith(INIT_TWICE_WARNING);
      expect(currentRuntime()).toBe(first);
    } finally {
      warn.mockRestore();
    }
  });

  it("refuses a second init after a disabled first one", () => {
    init({ enabled: false });
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      init(INIT_OPTIONS);
      expect(warn).toHaveBeenCalledTimes(1);
      expect(warn).toHaveBeenCalledWith(INIT_TWICE_WARNING);
      expect(currentRuntime()).toBeUndefined();
    } finally {
      warn.mockRestore();
    }
  });
});

describe("the provider", () => {
  it("exports spans with the llmobs scope and the distro resource", async () => {
    const { exporter, runtime } = startMemory();
    runtime.tracer.startSpan("hello").end();
    const [span] = await finished(exporter);
    expect(span?.instrumentationScope.name).toBe("llmobs");
    expect(span?.instrumentationScope.version).toBe(VERSION);
    expect(span?.resource.attributes["telemetry.distro.name"]).toBe("llmobs");
    expect(span?.resource.attributes["telemetry.distro.version"]).toBe(VERSION);
    expect(span?.resource.attributes["service.name"]).toBe("conformance");
  });

  it("stamps environment and release on every span", async () => {
    const { exporter, runtime } = startMemory({ environment: "staging", release: "1.2.3" });
    runtime.tracer.startSpan("a").end();
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.environment"]).toBe("staging");
    expect(span?.attributes["llmobs.release"]).toBe("1.2.3");
  });

  it("stamps nothing when environment and release are not configured", async () => {
    const { exporter, runtime } = startMemory();
    runtime.tracer.startSpan("a").end();
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.environment"]).toBeUndefined();
    expect(span?.attributes["llmobs.release"]).toBeUndefined();
  });

  it("drops root spans at sampleRate 0 but keeps a sampled remote parent's children", async () => {
    const { exporter, runtime } = startMemory({ sampleRate: 0 });
    runtime.tracer.startSpan("root").end();
    const remoteParent = {
      traceId: "0123456789abcdef0123456789abcdef",
      spanId: "0123456789abcdef",
      isRemote: true,
      traceFlags: TraceFlags.SAMPLED,
    };
    const parentContext = trace.setSpanContext(ROOT_CONTEXT, remoteParent);
    runtime.tracer.startSpan("child", undefined, parentContext).end();
    const spans = await finished(exporter);
    expect(spans.map((span) => span.name)).toEqual(["child"]);
  });
});

describe("the context manager", () => {
  it("registers an AsyncLocalStorage context manager", () => {
    startMemory();
    const key = createContextKey("llmobs.test");
    const seen: unknown[] = [];
    context.with(context.active().setValue(key, "in-with"), () => {
      seen.push(context.active().getValue(key));
    });
    seen.push(context.active().getValue(key));
    // Without a real manager the first observation would be undefined too.
    expect(seen).toEqual(["in-with", undefined]);
  });

  it("keeps an already-registered context manager", () => {
    const manager = new AsyncLocalStorageContextManager();
    const withSpy = vi.spyOn(manager, "with");
    context.setGlobalContextManager(manager.enable());
    startMemory();
    context.with(context.active(), () => {});
    // Still the registered one; the manager init() built was dropped instead.
    expect(withSpy).toHaveBeenCalledTimes(1);
  });
});

describe("the lifecycle", () => {
  it("shutdown is idempotent and drops later spans", async () => {
    const { exporter, runtime } = startMemory();
    await shutdown();
    await shutdown();
    expect(currentRuntime()).toBeUndefined();
    runtime.tracer.startSpan("late").end();
    await expect(flush()).resolves.toBeUndefined();
    expect(exporter.getFinishedSpans()).toEqual([]);
  });

  it("exports through the fetch exporter with URL, auth and gzip", async () => {
    const calls: RecordedCall[] = [];
    const fetch: FetchLike = (url, request) => {
      calls.push({ url, headers: { ...request.headers }, body: request.body });
      return Promise.resolve({ status: 200, ok: true, headers: { get: () => null } });
    };
    const config = resolveConfig(INIT_OPTIONS, { OTEL_SERVICE_NAME: "conformance" });
    const runtime = Runtime.create(config, { fetch });
    installRuntime(runtime);
    runtime.tracer.startSpan("hello").end();
    await flush();
    expect(calls).toHaveLength(1);
    expect(calls[0]?.url).toBe("http://127.0.0.1:9/v1/traces");
    expect(calls[0]?.headers).toEqual({
      "Content-Type": "application/x-protobuf",
      Authorization: "Bearer sk-llmobs-test",
      "Content-Encoding": "gzip",
    });
    // The body really is a gzip stream.
    const restored = await gunzip(calls[0]?.body);
    expect(restored.length).toBeGreaterThan(0);
  });
});

describe("global mode (D-F-4)", () => {
  // beforeEach/afterEach resetForTests call trace.disable()/context.disable(),
  // the public resets, so every test registers what it needs from scratch.

  it("registers our provider when no app provider is set", async () => {
    const { exporter, runtime } = startMemory({ useGlobalProvider: true });
    const global = trace.getTracerProvider();
    expect(global).toBeInstanceOf(ProxyTracerProvider);
    // Identity is by delegation: the global slot holds the API's proxy.
    expect((global as ProxyTracerProvider).getDelegate()).toBe(runtime.provider);
    // Tracers from the global API are our provider's.
    expect(trace.getTracer("any")).toBe(runtime.provider.getTracer("any"));
    // The default rule applies: an HTTP-like span is dropped, ours is kept.
    trace
      .getTracer("http.client")
      .startSpan("GET", {
        attributes: { "http.request.method": "GET" },
      })
      .end();
    runtime.tracer.startSpan("work").end();
    const spans = await finished(exporter);
    expect(spans.map((span) => span.name)).toEqual(["work"]);
  });

  it("init honors useGlobalProvider", () => {
    init({ ...INIT_OPTIONS, useGlobalProvider: true });
    const runtime = currentRuntime();
    const global = trace.getTracerProvider();
    expect((global as ProxyTracerProvider).getDelegate()).toBe(runtime?.provider);
  });

  it("warns and stays private when an app provider is registered", async () => {
    trace.setGlobalTracerProvider(new TracerProvider());
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const { exporter, runtime } = startMemory({ useGlobalProvider: true });
      expect(warn).toHaveBeenCalledTimes(1);
      expect(warn).toHaveBeenCalledWith(GLOBAL_EXISTS_WARNING);
      // The app provider keeps the global slot; ours stays private.
      const global = trace.getTracerProvider();
      expect((global as ProxyTracerProvider).getDelegate()).not.toBe(runtime.provider);
      expect(tracerProvider()).toBe(runtime.provider);
      // Own mode: the default rule is off, so foreign spans are exported.
      runtime.provider
        .getTracer("http.client")
        .startSpan("GET", { attributes: { "http.request.method": "GET" } })
        .end();
      const spans = await finished(exporter);
      expect(spans.map((span) => span.name)).toEqual(["GET"]);
    } finally {
      warn.mockRestore();
    }
  });

  it("counts a proxied app provider as an app provider (identity, not names)", () => {
    const proxy = new ProxyTracerProvider();
    proxy.setDelegate(new TracerProvider());
    trace.setGlobalTracerProvider(proxy);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      startMemory({ useGlobalProvider: true });
      expect(warn).toHaveBeenCalledTimes(1);
      expect(warn).toHaveBeenCalledWith(GLOBAL_EXISTS_WARNING);
      const global = trace.getTracerProvider();
      expect((global as ProxyTracerProvider).getDelegate()).toBe(proxy);
    } finally {
      warn.mockRestore();
    }
  });

  it("exportFilter replaces the default rule in global mode", async () => {
    const keepHttp = (span: ReadableSpan) => span.name === "GET";
    const { exporter, runtime } = startMemory({ useGlobalProvider: true, exportFilter: keepHttp });
    trace
      .getTracer("http.client")
      .startSpan("GET", {
        attributes: { "http.request.method": "GET" },
      })
      .end();
    runtime.tracer.startSpan("work").end();
    const spans = await finished(exporter);
    expect(spans.map((span) => span.name)).toEqual(["GET"]);
  });

  it("exportFilter applies in own mode too", async () => {
    const keepHttp = (span: ReadableSpan) => span.name === "GET";
    const { exporter, runtime } = startMemory({ exportFilter: keepHttp });
    runtime.provider
      .getTracer("http.client")
      .startSpan("GET", { attributes: { "http.request.method": "GET" } })
      .end();
    runtime.tracer.startSpan("work").end();
    const spans = await finished(exporter);
    expect(spans.map((span) => span.name)).toEqual(["GET"]);
  });
});
