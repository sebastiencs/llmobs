import { type Attributes, diag, SpanKind, SpanStatusCode, TraceFlags } from "@opentelemetry/api";
import { type ExportResult, ExportResultCode } from "@opentelemetry/core";
import { emptyResource } from "@opentelemetry/resources";
import {
  InMemorySpanExporter,
  type ReadableSpan,
  type Span,
  TracerProvider,
} from "@opentelemetry/sdk-trace";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { applyMask, MASK_FAILED, MaskingExporter } from "./mask.js";
import { resetForTests } from "./runtime.js";
import { finished, startMemory } from "./testing.test-helpers.js";

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

/** A deep mask: every occurrence of the address, at any depth, is replaced. */
function redact(value: unknown): unknown {
  if (typeof value === "string") {
    return value.replaceAll("alice@example.com", "[redacted]");
  }
  if (Array.isArray(value)) {
    return value.map(redact);
  }
  if (typeof value === "object" && value !== null) {
    const out: Record<string, unknown> = {};
    for (const [key, item] of Object.entries(value)) {
      out[key] = redact(item);
    }
    return out;
  }
  return value;
}

const EMAIL = "alice@example.com";
const MESSAGES_JSON = JSON.stringify([{ role: "user", content: EMAIL }]);
const MESSAGES_REDACTED = JSON.stringify([{ role: "user", content: "[redacted]" }]);

/** A finished span from a processor-less provider: real spans, no pipeline. */
function instrumentorSpan(
  attributes: Attributes,
  events: { name: string; attributes: Attributes }[] = [],
): ReadableSpan {
  const provider = new TracerProvider();
  // The SDK returns its span object but types it as the API `Span`; cast to
  // the SDK `Span`, which also carries the ReadableSpan fields.
  const span = provider
    .getTracer("openinference.instrumentation.openai")
    .startSpan("chat", { attributes }) as Span;
  for (const event of events) {
    span.addEvent(event.name, event.attributes);
  }
  span.end();
  return span;
}

function masking(
  mask: ((value: unknown) => unknown) | undefined,
  captureContent = true,
): { inner: InMemorySpanExporter; exporter: MaskingExporter } {
  const inner = new InMemorySpanExporter();
  const exporter = new MaskingExporter(inner, { mask, captureContent });
  return { inner, exporter };
}

describe("MaskingExporter", () => {
  it("masks every payload key of an instrumentor span, in span and event attributes", () => {
    const calls: unknown[] = [];
    const mask = (value: unknown): unknown => {
      calls.push(value);
      return redact(value);
    };
    const { inner, exporter } = masking(mask);
    const span = instrumentorSpan(
      {
        "input.value": `hello ${EMAIL}`,
        "llm.input_messages.0.message.content": `hi ${EMAIL}`,
        "gen_ai.input.messages": MESSAGES_JSON,
        "gen_ai.request.model": "gpt-4o-mini",
      },
      [{ name: "choice", attributes: { "gen_ai.output.messages": MESSAGES_JSON } }],
    );
    exporter.export([span], () => {});
    const [out] = inner.getFinishedSpans();
    expect(out?.attributes).toEqual({
      "input.value": "hello [redacted]",
      "llm.input_messages.0.message.content": "hi [redacted]",
      "gen_ai.input.messages": MESSAGES_REDACTED,
      "gen_ai.request.model": "gpt-4o-mini",
    });
    expect(out?.events[0]?.attributes).toEqual({ "gen_ai.output.messages": MESSAGES_REDACTED });
    // A JSON payload is decoded before the mask sees it.
    expect(calls).toContainEqual([{ role: "user", content: EMAIL }]);
  });

  it("passes llmobs spans through untouched", () => {
    const mask = vi.fn(redact);
    const { inner, exporter } = masking(mask);
    const provider = new TracerProvider();
    const ours = provider.getTracer("llmobs").startSpan("notify", {
      attributes: { "llmobs.input": JSON.stringify({ email: EMAIL }) },
    }) as Span;
    ours.end();
    const other = provider
      .getTracer("openinference.instrumentation.openai")
      .startSpan("chat", { attributes: { "input.value": EMAIL } }) as Span;
    other.end();
    exporter.export([ours, other], () => {});
    const spans = inner.getFinishedSpans();
    expect(spans.length).toBe(2);
    // Already masked at capture: not even copied, and the mask is not called.
    expect(spans[0]).toBe(ours);
    expect(spans[1]).not.toBe(other);
    expect(mask).toHaveBeenCalledTimes(1);
  });

  it("passes a span without payload keys by reference", () => {
    const mask = vi.fn(redact);
    const { inner, exporter } = masking(mask);
    const span = instrumentorSpan({ "gen_ai.request.model": "gpt-4o-mini" });
    exporter.export([span], () => {});
    expect(inner.getFinishedSpans()[0]).toBe(span);
    expect(mask).not.toHaveBeenCalled();
  });

  it("drops the payload keys when captureContent is false", () => {
    const { inner, exporter } = masking(redact, false);
    const span = instrumentorSpan(
      { "input.value": "v", "output.value": 2, "gen_ai.request.model": "m" },
      [{ name: "choice", attributes: { "ai.prompt": "p" } }],
    );
    exporter.export([span], () => {});
    const [out] = inner.getFinishedSpans();
    expect(out?.attributes).toEqual({ "gen_ai.request.model": "m" });
    expect(out?.events[0]?.attributes).toEqual({});
  });

  it("drops non-string payload values (only strings are masked)", () => {
    const { inner, exporter } = masking(redact);
    const span = instrumentorSpan({
      "input.value": 42,
      "ai.response.object": [1, 2],
      "lmnr.span.input": true,
      "llmobs.output": `sent to ${EMAIL}`,
    });
    exporter.export([span], () => {});
    expect(inner.getFinishedSpans()[0]?.attributes).toEqual({
      "llmobs.output": "sent to [redacted]",
    });
  });

  it("fails closed to the marker when the mask raises", () => {
    const debug = vi.spyOn(diag, "debug").mockImplementation(() => {});
    try {
      const mask = (): unknown => {
        throw new RangeError("no");
      };
      const { inner, exporter } = masking(mask);
      const span = instrumentorSpan({
        "input.value": "s",
        "gen_ai.output.messages": MESSAGES_JSON,
      });
      exporter.export([span], () => {});
      expect(inner.getFinishedSpans()[0]?.attributes).toEqual({
        "input.value": MASK_FAILED,
        "gen_ai.output.messages": MASK_FAILED,
      });
      expect(debug).toHaveBeenCalledWith("llmobs: mask raised RangeError");
    } finally {
      debug.mockRestore();
    }
  });

  it("treats unparsable JSON as a plain string and keeps a string result as is", () => {
    const { inner, exporter } = masking((value) =>
      typeof value === "string" ? value.toUpperCase() : value,
    );
    const span = instrumentorSpan({
      "input.value": "{not json",
      "llmobs.input": MESSAGES_JSON,
    });
    exporter.export([span], () => {});
    expect(inner.getFinishedSpans()[0]?.attributes).toEqual({
      "input.value": "{NOT JSON",
      "llmobs.input": MESSAGES_JSON,
    });
  });

  it("rewrites every ReadableSpan field", () => {
    const { inner, exporter } = masking(redact);
    const provider = new TracerProvider();
    const span = provider.getTracer("openinference.instrumentation.openai").startSpan("chat", {
      attributes: { "input.value": EMAIL, "gen_ai.request.model": "m" },
    }) as Span;
    span.addEvent("kept", { other: "v" });
    span.setStatus({ code: SpanStatusCode.ERROR, message: "boom" });
    span.end();
    exporter.export([span], () => {});
    const [out] = inner.getFinishedSpans();
    expect(out).toBeDefined();
    expect(out?.name).toBe("chat");
    expect(out?.kind).toBe(span.kind);
    expect(out?.spanContext()).toEqual(span.spanContext());
    expect(out?.parentSpanContext).toEqual(span.parentSpanContext);
    expect(out?.startTime).toBe(span.startTime);
    expect(out?.endTime).toBe(span.endTime);
    expect(out?.duration).toBe(span.duration);
    expect(out?.status).toEqual({ code: SpanStatusCode.ERROR, message: "boom" });
    expect(out?.ended).toBe(true);
    expect(out?.resource).toBe(span.resource);
    expect(out?.instrumentationScope).toBe(span.instrumentationScope);
    expect(out?.links).toBe(span.links);
    expect(out?.droppedAttributesCount).toBe(span.droppedAttributesCount);
    expect(out?.droppedEventsCount).toBe(span.droppedEventsCount);
    expect(out?.droppedLinksCount).toBe(span.droppedLinksCount);
    expect(out?.events[0]?.name).toBe("kept");
    expect(out?.events[0]?.time).toBe(span.events[0]?.time);
    expect(out?.events[0]?.attributes).toEqual({ other: "v" });
    expect(out?.attributes).toEqual({ "input.value": "[redacted]", "gen_ai.request.model": "m" });
  });

  it("fails closed on an unexpected error: drops the batch, FAILED, warns", () => {
    const { inner, exporter } = masking(redact);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    try {
      const batch = [
        instrumentorSpan({ "input.value": "v" }),
        {
          ...stubSpan("openinference.instrumentation.openai"),
          get attributes(): Attributes {
            throw new Error("boom");
          },
        },
      ];
      let result: ExportResult | undefined;
      exporter.export(batch, (settled) => {
        result = settled;
      });
      expect(result?.code).toBe(ExportResultCode.FAILED);
      expect(warn).toHaveBeenCalledTimes(1);
      expect(warn).toHaveBeenCalledWith("llmobs: masking failed; dropped 2 spans");
      expect(inner.getFinishedSpans()).toEqual([]);
    } finally {
      warn.mockRestore();
    }
  });

  it("delegates shutdown and forceFlush to the inner exporter", async () => {
    const { exporter } = masking(redact);
    await exporter.forceFlush();
    await exporter.shutdown();
    let result: ExportResult | undefined;
    exporter.export([], (settled) => {
      result = settled;
    });
    // The inner exporter was stopped by the delegated shutdown.
    expect(result?.code).toBe(ExportResultCode.FAILED);
  });
});

describe("the masking pass is installed only with a mask or captureContent false", () => {
  it("exports the very same span objects otherwise", async () => {
    const { exporter, runtime } = startMemory();
    const spy = vi.spyOn(exporter, "export");
    const span = runtime.provider
      .getTracer("openinference.instrumentation.openai")
      .startSpan("chat", { attributes: { "input.value": EMAIL } });
    span.end();
    await finished(exporter);
    expect(spy).toHaveBeenCalledTimes(1);
    expect(spy.mock.calls[0]?.[0]).toContain(span);
  });

  it("rewrites instrumentor payloads when a mask is configured", async () => {
    const { exporter, runtime } = startMemory({ mask: redact });
    runtime.provider
      .getTracer("openinference.instrumentation.openai")
      .startSpan("chat", { attributes: { "input.value": EMAIL } })
      .end();
    const [span] = await finished(exporter);
    expect(span?.attributes["input.value"]).toBe("[redacted]");
  });

  it("drops payload keys when captureContent is false", async () => {
    const { exporter, runtime } = startMemory({ captureContent: false });
    runtime.provider
      .getTracer("openinference.instrumentation.openai")
      .startSpan("chat", { attributes: { "input.value": "v", "gen_ai.request.model": "m" } })
      .end();
    const [span] = await finished(exporter);
    expect(span?.attributes).toEqual({ "gen_ai.request.model": "m" });
  });
});

/** A stub `ReadableSpan`; the exploding-attributes case overrides `attributes`. */
function stubSpan(scope: string): ReadableSpan {
  return {
    name: "stub",
    kind: SpanKind.INTERNAL,
    spanContext: () => ({
      traceId: "0".repeat(32),
      spanId: "1".repeat(16),
      traceFlags: TraceFlags.SAMPLED,
    }),
    startTime: [0, 0],
    endTime: [0, 0],
    status: { code: SpanStatusCode.UNSET },
    attributes: {},
    links: [],
    events: [],
    duration: [0, 0],
    ended: true,
    resource: emptyResource(),
    instrumentationScope: { name: scope },
    droppedAttributesCount: 0,
    droppedEventsCount: 0,
    droppedLinksCount: 0,
  };
}
