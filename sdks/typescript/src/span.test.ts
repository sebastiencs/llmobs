import { SpanStatusCode } from "@opentelemetry/api";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { resetForTests } from "./runtime.js";
import { currentSpanId, currentTraceId, type SpanHandle, type SpanKind, withSpan } from "./span.js";
import { finished, startMemory } from "./testing.test-helpers.js";

class ScenarioError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ScenarioError";
  }
}

const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

beforeEach(async () => {
  await resetForTests();
});

afterEach(async () => {
  await resetForTests();
});

describe("withSpan", () => {
  it("writes the span kind, defaulting to chain, for every valid kind", async () => {
    const { exporter } = startMemory();
    withSpan("default", () => {});
    const kinds: SpanKind[] = [
      "llm",
      "embedding",
      "tool",
      "agent",
      "chain",
      "retriever",
      "reranker",
      "guardrail",
      "evaluator",
      "span",
    ];
    for (const kind of kinds) {
      withSpan(`span-${kind}`, { kind }, () => {});
    }
    const spans = await finished(exporter);
    const byName = new Map(spans.map((span) => [span.name, span]));
    expect(byName.get("default")?.attributes["llmobs.span.kind"]).toBe("chain");
    for (const kind of kinds) {
      expect(byName.get(`span-${kind}`)?.attributes["llmobs.span.kind"]).toBe(kind);
    }
  });

  it("throws TypeError on an invalid kind before the body runs", () => {
    let ran = false;
    const body = () => {
      ran = true;
    };
    expect(() => withSpan("x", { kind: "workflow" as unknown as SpanKind }, body)).toThrowError(
      new TypeError(
        "kind: must be one of llm, embedding, tool, agent, chain, retriever, reranker, guardrail, evaluator, span",
      ),
    );
    expect(() => withSpan("x", { kind: 3 as unknown as SpanKind }, body)).toThrow(TypeError);
    expect(ran).toBe(false);
  });

  it("runs the body with a no-op handle when no runtime is installed", () => {
    let seen: SpanHandle | undefined;
    const result = withSpan("n", (span) => {
      seen = span;
      span.setInput("i");
      span.setOutput("o");
      span.setMetadata({ k: 1 });
      return 42;
    });
    expect(result).toBe(42);
    expect(seen?.traceId).toBeUndefined();
    expect(seen?.spanId).toBeUndefined();
  });

  it("traces the same call shape after init()", async () => {
    expect(withSpan("n", (span) => span.spanId)).toBeUndefined();
    const { exporter } = startMemory();
    const spanId = withSpan("n", (span) => span.spanId);
    const spans = await finished(exporter);
    expect(spans[0]?.spanContext().spanId).toBe(spanId);
    expect(spanId).toMatch(/^[0-9a-f]{16}$/);
  });

  it("records the option input and setOutput, never the return value", async () => {
    const { exporter } = startMemory();
    withSpan("io", { input: { q: "hi" } }, (span) => {
      span.setOutput(["b", "a"]);
      return "not recorded";
    });
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.input"]).toBe('{"q":"hi"}');
    expect(span?.attributes["llmobs.input.format"]).toBe("json");
    expect(span?.attributes["llmobs.output"]).toBe('["b","a"]');
    expect(span?.attributes["llmobs.output.format"]).toBe("json");
  });

  it("records setInput from the handle, a string as text", async () => {
    const { exporter } = startMemory();
    withSpan("s", (span) => {
      span.setInput("plain");
    });
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.input"]).toBe("plain");
    expect(span?.attributes["llmobs.input.format"]).toBe("text");
  });

  it("records metadata from options and the handle, masked per value", async () => {
    const { exporter } = startMemory({
      mask: (value) => (typeof value === "string" ? value.toUpperCase() : value),
    });
    withSpan("m", { metadata: { model: "bge" } }, (span) => {
      span.setMetadata({ seats: 3, obj: { a: 1 }, "bad key!": 1 });
    });
    const [span] = await finished(exporter);
    const attributes = span?.attributes ?? {};
    expect(attributes["llmobs.metadata.model"]).toBe("BGE");
    expect(attributes["llmobs.metadata.seats"]).toBe(3);
    expect(attributes["llmobs.metadata.obj"]).toBe('{"a":1}');
    expect(attributes["llmobs.metadata.bad key!"]).toBeUndefined();
  });

  it("keeps metadata with captureContent: false, drops the payloads", async () => {
    const { exporter } = startMemory({ captureContent: false });
    withSpan("c", { input: ["a"], metadata: { model: "bge" } }, (span) => {
      span.setOutput("out");
    });
    const [span] = await finished(exporter);
    const attributes = span?.attributes ?? {};
    expect(attributes["llmobs.span.kind"]).toBe("chain");
    expect(attributes["llmobs.input"]).toBeUndefined();
    expect(attributes["llmobs.output"]).toBeUndefined();
    expect(attributes["llmobs.metadata.model"]).toBe("bge");
  });

  it("masks inputs and outputs; a failing mask fails closed", async () => {
    const masked = startMemory({
      mask: (value) => (typeof value === "string" ? `<${value}>` : value),
    });
    withSpan("m", { input: "secret" }, (span) => {
      span.setOutput("out with secret");
    });
    const maskedAttributes = (await finished(masked.exporter))[0]?.attributes ?? {};
    expect(maskedAttributes["llmobs.input"]).toBe("<secret>");
    // The mask sees the whole value; this one replaces only whole strings.
    expect(maskedAttributes["llmobs.output"]).toBe("<out with secret>");
    const failing = startMemory({
      mask: () => {
        throw new Error("nope");
      },
    });
    withSpan("f", { input: "v" }, (span) => {
      span.setOutput("w");
    });
    const failedAttributes = (await finished(failing.exporter))[0]?.attributes ?? {};
    expect(failedAttributes["llmobs.input"]).toBe("[llmobs: mask failed]");
    expect(failedAttributes["llmobs.output"]).toBe("[llmobs: mask failed]");
  });

  it("records the exception, sets the error status and re-throws unchanged", async () => {
    const { exporter } = startMemory();
    const thrown = new ScenarioError("boom");
    let caught: unknown;
    try {
      withSpan("f", () => {
        throw thrown;
      });
    } catch (error) {
      caught = error;
    }
    expect(caught).toBe(thrown);
    const [span] = await finished(exporter);
    expect(span?.status.code).toBe(SpanStatusCode.ERROR);
    expect(span?.status.message).toBe("ScenarioError: boom");
    expect(span?.events).toHaveLength(1);
    const event = span?.events[0];
    expect(event?.name).toBe("exception");
    expect(event?.attributes?.["exception.type"]).toBe("ScenarioError");
    expect(event?.attributes?.["exception.message"]).toBe("boom");
    expect(String(event?.attributes?.["exception.stacktrace"])).toContain("ScenarioError");
  });

  it("records a non-Error throw as type Error with String(e)", async () => {
    const { exporter } = startMemory();
    expect(() =>
      withSpan("s", () => {
        throw "plain";
      }),
    ).toThrow("plain");
    const [span] = await finished(exporter);
    expect(span?.status.message).toBe("Error: plain");
    const event = span?.events[0];
    expect(event?.attributes?.["exception.type"]).toBe("Error");
    expect(event?.attributes?.["exception.message"]).toBe("plain");
    expect(event?.attributes?.["exception.stacktrace"]).toBe("");
  });

  it("ends the span when the returned promise settles, without recording its value", async () => {
    const { exporter } = startMemory();
    const value = await withSpan("a", async () => {
      await pause(5);
      return "v";
    });
    expect(value).toBe("v");
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.output"]).toBeUndefined();
    expect(span?.status.code).toBe(SpanStatusCode.UNSET);
  });

  it("records the error when the returned promise rejects, and rejects with it", async () => {
    const { exporter } = startMemory();
    const thrown = new Error("late");
    const promise = withSpan("r", () => Promise.reject(thrown));
    await expect(promise).rejects.toBe(thrown);
    const [span] = await finished(exporter);
    expect(span?.status.code).toBe(SpanStatusCode.ERROR);
    expect(span?.status.message).toBe("Error: late");
    expect(span?.events[0]?.attributes?.["exception.type"]).toBe("Error");
  });

  it("settles custom thenables like promises", async () => {
    const { exporter } = startMemory();
    const value = await withSpan("t", () => ({
      // biome-ignore lint/suspicious/noThenProperty: a custom thenable is the point of the test
      then: (resolve: (v: number) => void) => resolve(7),
    }));
    expect(value).toBe(7);
    const [span] = await finished(exporter);
    expect(span?.name).toBe("t");
  });

  it("ignores handle updates after the span ended", async () => {
    const { exporter } = startMemory();
    let handle: SpanHandle | undefined;
    withSpan("h", (span) => {
      handle = span;
    });
    handle?.setInput("late");
    handle?.setOutput("late");
    handle?.setMetadata({ late: 1 });
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.input"]).toBeUndefined();
    expect(span?.attributes["llmobs.output"]).toBeUndefined();
    expect(span?.attributes["llmobs.metadata.late"]).toBeUndefined();
  });

  it("nests a child under the running span across an await", async () => {
    const { exporter } = startMemory();
    await withSpan("outer", async () => {
      await pause(5);
      await withSpan("inner", () => {});
    });
    const spans = await finished(exporter);
    const byName = new Map(spans.map((span) => [span.name, span]));
    const outer = byName.get("outer");
    const inner = byName.get("inner");
    expect(inner?.parentSpanContext?.spanId).toBe(outer?.spanContext().spanId);
    expect(inner?.spanContext().traceId).toBe(outer?.spanContext().traceId);
  });
});

describe("currentTraceId and currentSpanId", () => {
  it("returns the running span's ids, undefined outside", async () => {
    const { exporter } = startMemory();
    expect(currentTraceId()).toBeUndefined();
    expect(currentSpanId()).toBeUndefined();
    let traceId: string | undefined;
    let spanId: string | undefined;
    withSpan("c", () => {
      traceId = currentTraceId();
      spanId = currentSpanId();
    });
    expect(traceId).toMatch(/^[0-9a-f]{32}$/);
    expect(spanId).toMatch(/^[0-9a-f]{16}$/);
    const [span] = await finished(exporter);
    expect(traceId).toBe(span?.spanContext().traceId);
    expect(spanId).toBe(span?.spanContext().spanId);
    expect(currentTraceId()).toBeUndefined();
    expect(currentSpanId()).toBeUndefined();
  });

  it("works for a third-party span of our provider too", () => {
    const { runtime } = startMemory();
    runtime.tracer.startActiveSpan("ext", (span) => {
      expect(currentSpanId()).toBe(span.spanContext().spanId);
      expect(currentTraceId()).toBe(span.spanContext().traceId);
      span.end();
    });
  });
});
