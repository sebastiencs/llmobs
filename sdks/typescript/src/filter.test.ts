import { type Attributes, diag, SpanKind, SpanStatusCode, TraceFlags } from "@opentelemetry/api";
import { emptyResource } from "@opentelemetry/resources";
import { InMemorySpanExporter, type ReadableSpan, TracerProvider } from "@opentelemetry/sdk-trace";
import { describe, expect, it, vi } from "vitest";

import { defaultFilter } from "./filter.js";
import { LlmobsSpanProcessor } from "./processor.js";

/** A minimal `ReadableSpan`: only the scope and the attributes vary here. */
function spanOf(options: { scope?: string; attributes?: Attributes } = {}): ReadableSpan {
  return {
    name: "span",
    kind: SpanKind.INTERNAL,
    spanContext: () => ({
      traceId: "0".repeat(32),
      spanId: "1".repeat(16),
      traceFlags: TraceFlags.SAMPLED,
    }),
    startTime: [0, 0],
    endTime: [0, 0],
    status: { code: SpanStatusCode.UNSET },
    attributes: options.attributes ?? {},
    links: [],
    events: [],
    duration: [0, 0],
    ended: true,
    resource: emptyResource(),
    instrumentationScope: { name: options.scope ?? "" },
    droppedAttributesCount: 0,
    droppedEventsCount: 0,
    droppedLinksCount: 0,
  };
}

describe("defaultFilter (D-F-5)", () => {
  it.each(["llmobs", "ai"])("exports a span whose scope is %s", (scope) => {
    expect(defaultFilter(spanOf({ scope }))).toBe(true);
  });

  it.each([
    "llmobs.span.kind",
    "gen_ai.request.model",
    "llm.input_messages.0.message.role",
    "openinference.inference.usage",
    "ai.modelId",
    "traceloop.entity.name",
    "langfuse.trace.tags",
    "lmnr.span.type",
    "gcp.vertex.agent.token_count",
  ])("exports a span with the attribute %s", (key) => {
    expect(defaultFilter(spanOf({ scope: "other", attributes: { [key]: "v" } }))).toBe(true);
  });

  it.each(["input.value", "output.value"])("exports a span with the exact key %s", (key) => {
    expect(defaultFilter(spanOf({ scope: "other", attributes: { [key]: "v" } }))).toBe(true);
  });

  it.each([
    ["an HTTP-like span", { "http.request.method": "GET", "server.address": "example.com" }],
    ["a near-miss prefix", { llmobsx: "v", "input.values": "v", gen_ai: "v" }],
    ["no attributes at all", {}],
  ])("drops %s", (_name, attributes) => {
    expect(defaultFilter(spanOf({ scope: "http.client", attributes }))).toBe(false);
  });

  it("drops an OpenInference scope without matching attributes (scope is judged exactly)", () => {
    expect(defaultFilter(spanOf({ scope: "openinference.instrumentation.openai" }))).toBe(false);
    expect(
      defaultFilter(
        spanOf({ scope: "openinference.instrumentation.openai", attributes: { "llm.x": "v" } }),
      ),
    ).toBe(true);
  });
});

/** A provider whose spans flow through one llmobs processor into memory. */
function ruleProvider(options: {
  exportFilter?: (span: ReadableSpan) => boolean;
  defaultFilter: boolean;
}): { exporter: InMemorySpanExporter; provider: TracerProvider; processor: LlmobsSpanProcessor } {
  const exporter = new InMemorySpanExporter();
  const processor = new LlmobsSpanProcessor(exporter, options);
  const provider = new TracerProvider({ spanProcessors: [processor] });
  return { exporter, provider, processor };
}

function httpSpan(provider: TracerProvider): void {
  provider
    .getTracer("http.client")
    .startSpan("GET", { attributes: { "http.request.method": "GET" } })
    .end();
}

describe("the processor's export rule", () => {
  it("exports everything in own mode (defaultFilter false)", async () => {
    const { exporter, provider, processor } = ruleProvider({ defaultFilter: false });
    httpSpan(provider);
    provider.getTracer("llmobs").startSpan("work").end();
    await processor.forceFlush();
    expect(exporter.getFinishedSpans().map((span) => span.name)).toEqual(["GET", "work"]);
  });

  it("applies the default rule in global mode: the HTTP span is dropped, ours kept", async () => {
    const { exporter, provider, processor } = ruleProvider({ defaultFilter: true });
    httpSpan(provider);
    provider.getTracer("llmobs").startSpan("work").end();
    await processor.forceFlush();
    expect(exporter.getFinishedSpans().map((span) => span.name)).toEqual(["work"]);
  });

  it("exportFilter replaces the rule in own mode", async () => {
    const { exporter, provider, processor } = ruleProvider({
      defaultFilter: false,
      exportFilter: (span) => span.name === "GET",
    });
    httpSpan(provider);
    provider.getTracer("llmobs").startSpan("work").end();
    await processor.forceFlush();
    expect(exporter.getFinishedSpans().map((span) => span.name)).toEqual(["GET"]);
  });

  it("exportFilter replaces the default rule in global mode", async () => {
    const { exporter, provider, processor } = ruleProvider({
      defaultFilter: true,
      exportFilter: () => true,
    });
    httpSpan(provider);
    await processor.forceFlush();
    expect(exporter.getFinishedSpans().map((span) => span.name)).toEqual(["GET"]);
  });

  it("a raising exportFilter exports the span", async () => {
    const debug = vi.spyOn(diag, "debug").mockImplementation(() => {});
    try {
      const { exporter, provider, processor } = ruleProvider({
        defaultFilter: true,
        exportFilter: (): boolean => {
          throw new RangeError("no");
        },
      });
      httpSpan(provider);
      await processor.forceFlush();
      expect(exporter.getFinishedSpans().map((span) => span.name)).toEqual(["GET"]);
      expect(debug).toHaveBeenCalledWith("llmobs: exportFilter raised RangeError; span exported");
    } finally {
      debug.mockRestore();
    }
  });
});
