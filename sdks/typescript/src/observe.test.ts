import { SpanStatusCode } from "@opentelemetry/api";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { resetForTests } from "./runtime.js";
import { observe, type SpanKind, withSpan } from "./span.js";
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

describe("observe", () => {
  it("throws TypeError at wrap time on an invalid kind", () => {
    expect(() => observe(() => 1, { kind: "workflow" as unknown as SpanKind })).toThrow(TypeError);
    expect(() => observe(() => 1, { kind: "workflow" as unknown as SpanKind })).toThrow(
      "kind: must be one of llm, embedding, tool, agent, chain, retriever, reranker, guardrail, evaluator, span",
    );
  });

  it("writes the kind, defaulting to chain", async () => {
    const { exporter } = startMemory();
    observe(() => 1, { name: "d" })();
    observe(() => 1, { name: "t", kind: "tool" })();
    const spans = await finished(exporter);
    const byName = new Map(spans.map((span) => [span.name, span]));
    expect(byName.get("d")?.attributes["llmobs.span.kind"]).toBe("chain");
    expect(byName.get("t")?.attributes["llmobs.span.kind"]).toBe("tool");
  });

  it("passes through without a runtime and traces after init()", async () => {
    const double = observe((x: number) => x * 2, { name: "double" });
    expect(double(3)).toBe(6);
    const passthrough = Promise.resolve(1);
    expect(observe(() => passthrough, { name: "p" })()).toBe(passthrough);
    const { exporter } = startMemory();
    expect(double(4)).toBe(8);
    const [span] = await finished(exporter);
    expect(span?.name).toBe("double");
    expect(span?.attributes["llmobs.input"]).toBe("4");
  });

  it("names the span from the function, the options, or anonymous", async () => {
    const { exporter } = startMemory();
    observe(function named() {}, { name: "explicit" })();
    observe(function named() {})();
    observe(() => {})();
    const names = (await finished(exporter)).map((span) => span.name);
    expect(names).toEqual(["explicit", "named", "anonymous"]);
  });

  it("records no input for 0 arguments, the value for 1, an array for more", async () => {
    const { exporter } = startMemory();
    observe(() => 1, { name: "zero" })();
    observe((a: unknown) => a, { name: "one" })("solo");
    observe((a: unknown, b: unknown) => [a, b], { name: "two" })(1, "b");
    const byName = new Map((await finished(exporter)).map((span) => [span.name, span]));
    expect(byName.get("zero")?.attributes["llmobs.input"]).toBeUndefined();
    expect(byName.get("one")?.attributes["llmobs.input"]).toBe("solo");
    expect(byName.get("one")?.attributes["llmobs.input.format"]).toBe("text");
    expect(byName.get("two")?.attributes["llmobs.input"]).toBe('[1,"b"]');
    expect(byName.get("two")?.attributes["llmobs.input.format"]).toBe("json");
  });

  it("records outputs: strings as text, undefined as none, null and objects as json", async () => {
    const { exporter } = startMemory();
    observe(() => "text", { name: "a" })();
    observe(() => undefined, { name: "b" })();
    observe(() => null, { name: "c" })();
    observe(() => ({ n: 1 }), { name: "d" })();
    const byName = new Map((await finished(exporter)).map((span) => [span.name, span]));
    expect(byName.get("a")?.attributes["llmobs.output"]).toBe("text");
    expect(byName.get("a")?.attributes["llmobs.output.format"]).toBe("text");
    expect(byName.get("b")?.attributes["llmobs.output"]).toBeUndefined();
    expect(byName.get("c")?.attributes["llmobs.output"]).toBe("null");
    expect(byName.get("d")?.attributes["llmobs.output"]).toBe('{"n":1}');
  });

  it("preserves this on a method", async () => {
    const { exporter } = startMemory();
    let seen: unknown;
    class Tool {
      readonly id = "t-9";
      run = observe(
        function (this: Tool, _x: number) {
          seen = this;
          return this.id;
        },
        { name: "run" },
      );
    }
    const tool = new Tool();
    expect(tool.run(1)).toBe("t-9");
    expect(seen).toBe(tool);
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.input"]).toBe("1");
  });

  it("re-throws a sync error unchanged with the span in error", async () => {
    const { exporter } = startMemory();
    const thrown = new ScenarioError("bang");
    const wrapped = observe(
      (_x: number) => {
        throw thrown;
      },
      { name: "f" },
    );
    let caught: unknown;
    try {
      wrapped(1);
    } catch (error) {
      caught = error;
    }
    expect(caught).toBe(thrown);
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.input"]).toBe("1");
    expect(span?.attributes["llmobs.output"]).toBeUndefined();
    expect(span?.status.code).toBe(SpanStatusCode.ERROR);
    expect(span?.status.message).toBe("ScenarioError: bang");
    expect(span?.events[0]?.attributes?.["exception.type"]).toBe("ScenarioError");
  });

  it("rejects with the same error and records it when the promise rejects", async () => {
    const { exporter } = startMemory();
    const thrown = new Error("late");
    const wrapped = observe(
      async () => {
        throw thrown;
      },
      { name: "r" },
    );
    await expect(wrapped()).rejects.toBe(thrown);
    const [span] = await finished(exporter);
    expect(span?.status.code).toBe(SpanStatusCode.ERROR);
    expect(span?.status.message).toBe("Error: late");
    expect(span?.events).toHaveLength(1);
  });

  it("records the resolved value of a promise", async () => {
    const { exporter } = startMemory();
    const promise = observe(async () => ({ ok: true }), { name: "a" })();
    await expect(promise).resolves.toEqual({ ok: true });
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.output"]).toBe('{"ok":true}');
    expect(span?.status.code).toBe(SpanStatusCode.UNSET);
  });

  it("does not record or consume a returned generator object", async () => {
    const { exporter } = startMemory();
    function* numbers() {
      yield 1;
      yield 2;
    }
    const returned = observe(() => numbers(), { name: "g" })();
    expect(Object.prototype.toString.call(returned)).toBe("[object Generator]");
    expect([...returned]).toEqual([1, 2]);
    const byName = new Map((await finished(exporter)).map((span) => [span.name, span.attributes]));
    expect(byName.get("g")?.["llmobs.output"]).toBeUndefined();
  });

  it("does not record an async generator object resolved by an async function", async () => {
    const { exporter } = startMemory();
    async function* stream() {
      yield 1;
    }
    const returned = await observe(async () => stream(), { name: "s" })();
    expect(Object.prototype.toString.call(returned)).toBe("[object AsyncGenerator]");
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.output"]).toBeUndefined();
  });

  it("nests observe calls as parent and child", async () => {
    const { exporter } = startMemory();
    const retrieve = observe((_q: string) => [{ id: "d1" }], {
      kind: "retriever",
      name: "retrieve",
    });
    const answer = observe(
      (q: string) => {
        retrieve(q);
        return "It is 42.";
      },
      { name: "answer" },
    );
    answer("why?");
    const spans = await finished(exporter);
    const byName = new Map(spans.map((span) => [span.name, span]));
    const parent = byName.get("answer");
    const child = byName.get("retrieve");
    expect(child?.parentSpanContext?.spanId).toBe(parent?.spanContext().spanId);
    expect(child?.attributes["llmobs.span.kind"]).toBe("retriever");
    expect(child?.attributes["llmobs.input"]).toBe("why?");
    expect(parent?.attributes["llmobs.output"]).toBe("It is 42.");
  });

  it("gives concurrent async calls the same parent", async () => {
    const { exporter } = startMemory();
    const slow = observe(
      async () => {
        await pause(15);
        return "s";
      },
      { name: "slow" },
    );
    const fast = observe(
      async () => {
        await pause(1);
        return "f";
      },
      { name: "fast" },
    );
    await withSpan("parent", () => Promise.all([slow(), fast()]));
    const spans = await finished(exporter);
    const byName = new Map(spans.map((span) => [span.name, span]));
    const parentSpanId = byName.get("parent")?.spanContext().spanId;
    expect(byName.get("slow")?.parentSpanContext?.spanId).toBe(parentSpanId);
    expect(byName.get("fast")?.parentSpanContext?.spanId).toBe(parentSpanId);
    expect(byName.get("slow")?.spanContext().spanId).not.toBe(
      byName.get("fast")?.spanContext().spanId,
    );
  });

  it("skips input or output per capture options", async () => {
    const { exporter } = startMemory();
    observe((a: unknown) => a, { name: "no-in", captureInput: false })(1);
    observe(() => "v", { name: "no-out", captureOutput: false })();
    const byName = new Map((await finished(exporter)).map((span) => [span.name, span]));
    expect(byName.get("no-in")?.attributes["llmobs.input"]).toBeUndefined();
    expect(byName.get("no-in")?.attributes["llmobs.output"]).toBe("1");
    expect(byName.get("no-out")?.attributes["llmobs.input"]).toBeUndefined();
    expect(byName.get("no-out")?.attributes["llmobs.output"]).toBeUndefined();
  });

  it("writes only the kind with captureContent: false", async () => {
    const { exporter } = startMemory({ captureContent: false });
    observe((_q: { query: string }) => "x", { kind: "tool", name: "lookup" })({
      query: "q",
    });
    const [span] = await finished(exporter);
    expect(span?.attributes).toEqual({ "llmobs.span.kind": "tool" });
  });

  it("masks the input and the output at capture time", async () => {
    const { exporter } = startMemory({
      mask: (value) => (typeof value === "string" ? value.toUpperCase() : value),
    });
    observe((s: string) => `got ${s}`, { name: "m" })("quiet");
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.input"]).toBe("QUIET");
    expect(span?.attributes["llmobs.output"]).toBe("GOT QUIET");
  });
});
