import { SpanStatusCode } from "@opentelemetry/api";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { flush, resetForTests } from "./runtime.js";
import { currentSpanId, observe, withSpan } from "./span.js";
import { finished, startMemory } from "./testing.test-helpers.js";

class ScenarioError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ScenarioError";
  }
}

beforeEach(async () => {
  await resetForTests();
});

afterEach(async () => {
  await resetForTests();
});

describe("generators", () => {
  it("creates no span at call time and starts it at the first next()", async () => {
    const { exporter } = startMemory();
    let currentAtStart: string | undefined;
    const traced = observe(
      function* gen() {
        currentAtStart = currentSpanId();
        yield 1;
      },
      { name: "gen" },
    );
    const wrapped = traced();
    await flush();
    expect(exporter.getFinishedSpans()).toHaveLength(0);
    // A span created between the call and the first next() is a sibling.
    withSpan("sibling", () => undefined);
    expect([...wrapped]).toEqual([1]);
    const byName = new Map((await finished(exporter)).map((span) => [span.name, span]));
    const genId = byName.get("gen")?.spanContext().spanId;
    const siblingId = byName.get("sibling")?.spanContext().spanId;
    expect(genId).toBeDefined();
    expect(siblingId).toBeDefined();
    if (genId === undefined || siblingId === undefined) {
      return;
    }
    expect(byName.get("gen")?.parentSpanContext).toBeUndefined();
    expect(byName.get("sibling")?.parentSpanContext).toBeUndefined();
    // Creation order (sequential ids): gen started after the sibling. OTel's
    // HrTime mixes clocks, so comparing raw start times within a tick is not.
    expect(Number.parseInt(genId, 16)).toBeGreaterThan(Number.parseInt(siblingId, 16));
    expect(currentAtStart).toBe(genId);
  });

  it("is the current span only while the body runs", async () => {
    const { exporter } = startMemory();
    const child = observe(() => "c", { name: "child" });
    const inBody: (string | undefined)[] = [];
    const inLoop: (string | undefined)[] = [];
    const traced = observe(
      function* gen() {
        inBody.push(currentSpanId());
        child();
        yield 1;
        inBody.push(currentSpanId());
        yield 2;
      },
      { name: "gen" },
    );
    const outerId = withSpan("outer", (outer) => {
      for (const _item of traced()) {
        inLoop.push(currentSpanId());
      }
      return outer.spanId;
    });
    const byName = new Map((await finished(exporter)).map((span) => [span.name, span]));
    const genSpan = byName.get("gen");
    const genId = genSpan?.spanContext().spanId;
    expect(inBody).toEqual([genId, genId]);
    expect(inLoop).toEqual([outerId, outerId]);
    expect(genSpan?.parentSpanContext?.spanId).toBe(byName.get("outer")?.spanContext().spanId);
    expect(byName.get("child")?.parentSpanContext?.spanId).toBe(genId);
  });

  it("ends with status unset and the joined text when every item is a string", async () => {
    const { exporter } = startMemory();
    const traced = observe(
      function* stream(_args: { prompt: string }) {
        yield "Hel";
        yield "lo";
      },
      { name: "stream" },
    );
    expect([...traced({ prompt: "hi" })]).toEqual(["Hel", "lo"]);
    const [span] = await finished(exporter);
    expect(span?.name).toBe("stream");
    expect(span?.attributes["llmobs.input"]).toBe('{"prompt":"hi"}');
    expect(span?.attributes["llmobs.input.format"]).toBe("json");
    expect(span?.attributes["llmobs.output"]).toBe("Hello");
    expect(span?.attributes["llmobs.output.format"]).toBe("text");
    expect(span?.status.code).toBe(SpanStatusCode.UNSET);
  });

  it("records the items as a JSON list when not every item is a string", async () => {
    const { exporter } = startMemory();
    const traced = observe(
      function* numbers() {
        yield { n: 1 };
        yield { n: 2 };
      },
      { name: "numbers" },
    );
    expect([...traced()]).toEqual([{ n: 1 }, { n: 2 }]);
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.input"]).toBeUndefined();
    expect(span?.attributes["llmobs.output"]).toBe('[{"n":1},{"n":2}]');
    expect(span?.attributes["llmobs.output.format"]).toBe("json");
    expect(span?.status.code).toBe(SpanStatusCode.UNSET);
  });

  it("ends with status ok and the partial output on break", async () => {
    const { exporter } = startMemory();
    const traced = observe(
      function* gen() {
        yield "a";
        yield "b";
        yield "c";
      },
      { name: "gen" },
    );
    const seen: string[] = [];
    for (const item of traced()) {
      seen.push(item);
      if (item === "b") {
        break;
      }
    }
    expect(seen).toEqual(["a", "b"]);
    const [span] = await finished(exporter);
    expect(span?.status.code).toBe(SpanStatusCode.OK);
    expect(span?.attributes["llmobs.output"]).toBe("ab");
  });

  it("records an exception from the body and re-throws it", async () => {
    const { exporter } = startMemory();
    const thrown = new ScenarioError("boom");
    const wrapped = observe(
      function* gen() {
        yield 1;
        throw thrown;
      },
      { name: "gen" },
    )();
    expect(wrapped.next()).toEqual({ value: 1, done: false });
    let caught: unknown;
    try {
      wrapped.next();
    } catch (error) {
      caught = error;
    }
    expect(caught).toBe(thrown);
    const [span] = await finished(exporter);
    expect(span?.status.code).toBe(SpanStatusCode.ERROR);
    expect(span?.status.message).toBe("ScenarioError: boom");
    expect(span?.events[0]?.attributes?.["exception.type"]).toBe("ScenarioError");
    expect(span?.attributes["llmobs.output"]).toBeUndefined();
  });

  it("forwards a consumer throw() and records the error", async () => {
    const { exporter } = startMemory();
    const injected = new ScenarioError("injected");
    const wrapped = observe(
      function* gen() {
        yield "a";
        yield "b";
      },
      { name: "gen" },
    )();
    expect(wrapped.next()).toEqual({ value: "a", done: false });
    let caught: unknown;
    try {
      wrapped.throw(injected);
    } catch (error) {
      caught = error;
    }
    expect(caught).toBe(injected);
    const [span] = await finished(exporter);
    expect(span?.status.code).toBe(SpanStatusCode.ERROR);
    expect(span?.status.message).toBe("ScenarioError: injected");
    expect(span?.attributes["llmobs.output"]).toBeUndefined();
  });

  it("keeps going when the body catches a consumer throw()", async () => {
    const { exporter } = startMemory();
    const wrapped = observe(
      function* gen() {
        try {
          yield "a";
        } catch {
          yield "caught";
        }
        return "end";
      },
      { name: "gen" },
    )();
    expect(wrapped.next()).toEqual({ value: "a", done: false });
    expect(wrapped.throw(new Error("x"))).toEqual({ value: "caught", done: false });
    expect(wrapped.next()).toEqual({ value: "end", done: true });
    const [span] = await finished(exporter);
    expect(span?.status.code).toBe(SpanStatusCode.UNSET);
    expect(span?.attributes["llmobs.output"]).toBe("acaught");
  });

  it("passes sent values through and returns the generator's return value", async () => {
    const { exporter } = startMemory();
    const wrapped = observe(
      function* echo() {
        const first: unknown = yield "start";
        const second: unknown = yield `got ${String(first)}`;
        return `done ${String(second)}`;
      },
      { name: "echo" },
    )();
    expect(wrapped.next()).toEqual({ value: "start", done: false });
    expect(wrapped.next(7)).toEqual({ value: "got 7", done: false });
    expect(wrapped.next("x")).toEqual({ value: "done x", done: true });
    await finished(exporter);
  });

  it("exports nothing for a generator that never runs", async () => {
    const { exporter } = startMemory();
    const traced = observe(
      function* gen() {
        yield 1;
      },
      { name: "gen" },
    );
    expect(traced().return(undefined)).toEqual({ value: undefined, done: true });
    traced(); // dropped without iteration
    expect(await finished(exporter)).toHaveLength(0);
  });

  it("never exports an abandoned generator", async () => {
    const { exporter } = startMemory();
    const wrapped = observe(
      function* gen() {
        yield 1;
        yield 2;
        yield 3;
      },
      { name: "gen" },
    )();
    expect(wrapped.next()).toEqual({ value: 1, done: false });
    expect(await finished(exporter)).toHaveLength(0);
  });

  it("returns the generator unwrapped without a runtime and traces after init", async () => {
    const traced = observe(
      function* double(x: number) {
        yield String(x * 2);
      },
      { name: "double" },
    );
    const before = traced(3);
    expect(before.next()).toEqual({ value: "6", done: false });
    expect(before.next()).toEqual({ value: undefined, done: true });
    const { exporter } = startMemory();
    expect([...traced(4)]).toEqual(["8"]);
    const [span] = await finished(exporter);
    expect(span?.name).toBe("double");
    expect(span?.attributes["llmobs.input"]).toBe("4");
    expect(span?.attributes["llmobs.output"]).toBe("8");
    expect(span?.attributes["llmobs.output.format"]).toBe("text");
  });

  it("records reduceOutput(items) instead of the default rule", async () => {
    const { exporter } = startMemory();
    const traced = observe(
      function* gen() {
        yield "a";
        yield "b";
        yield "c";
      },
      { name: "gen", reduceOutput: (items) => items.join("-") },
    );
    expect([...traced()]).toEqual(["a", "b", "c"]);
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.output"]).toBe("a-b-c");
    expect(span?.attributes["llmobs.output.format"]).toBe("text");
  });

  it("falls back to the default rule when reduceOutput throws", async () => {
    const { exporter } = startMemory();
    const traced = observe(
      function* gen() {
        yield "a";
        yield "b";
      },
      {
        name: "gen",
        reduceOutput: () => {
          throw new Error("nope");
        },
      },
    );
    expect([...traced()]).toEqual(["a", "b"]);
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.output"]).toBe("ab");
    expect(span?.status.code).toBe(SpanStatusCode.UNSET);
  });

  it("ignores reduceOutput for a non-generator function", async () => {
    const { exporter } = startMemory();
    observe(() => "plain", { name: "plain", reduceOutput: () => "reduced" })();
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.output"]).toBe("plain");
  });

  it("keeps no items while captureContent is false", async () => {
    const { exporter } = startMemory({ captureContent: false });
    const traced = observe(
      function* many() {
        for (let i = 0; i < 10_000; i += 1) {
          yield i;
        }
      },
      { name: "many" },
    );
    expect([...traced()]).toHaveLength(10_000);
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.output"]).toBeUndefined();
    expect(span?.attributes["llmobs.span.kind"]).toBe("chain");
  });

  it("keeps no items while captureOutput is false", async () => {
    const { exporter } = startMemory();
    const traced = observe(
      function* many() {
        for (let i = 0; i < 10_000; i += 1) {
          yield i;
        }
      },
      { name: "many", captureOutput: false },
    );
    expect([...traced()]).toHaveLength(10_000);
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.output"]).toBeUndefined();
  });

  it("skips the input per captureInput", async () => {
    const { exporter } = startMemory();
    const traced = observe(
      function* reply(question: string) {
        yield question;
      },
      { name: "reply", captureInput: false },
    );
    expect([...traced("hi")]).toEqual(["hi"]);
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.input"]).toBeUndefined();
    expect(span?.attributes["llmobs.output"]).toBe("hi");
  });
});

describe("async generators", () => {
  it("ends with status unset and the joined text on exhaustion", async () => {
    const { exporter } = startMemory();
    const traced = observe(
      async function* stream(_args: { prompt: string }) {
        yield "Hel";
        yield "lo";
      },
      { name: "stream" },
    );
    const seen: string[] = [];
    for await (const chunk of traced({ prompt: "hi" })) {
      seen.push(chunk);
    }
    expect(seen).toEqual(["Hel", "lo"]);
    const [span] = await finished(exporter);
    expect(span?.name).toBe("stream");
    expect(span?.attributes["llmobs.input"]).toBe('{"prompt":"hi"}');
    expect(span?.attributes["llmobs.output"]).toBe("Hello");
    expect(span?.attributes["llmobs.output.format"]).toBe("text");
    expect(span?.status.code).toBe(SpanStatusCode.UNSET);
  });

  it("is the current span only while the body runs", async () => {
    const { exporter } = startMemory();
    const child = observe(async () => "c", { name: "child" });
    const inBody: (string | undefined)[] = [];
    const inLoop: (string | undefined)[] = [];
    const traced = observe(
      async function* gen() {
        inBody.push(currentSpanId());
        await child();
        yield 1;
        inBody.push(currentSpanId());
        yield 2;
      },
      { name: "gen" },
    );
    const outerId = await withSpan("outer", async (outer) => {
      for await (const _item of traced()) {
        inLoop.push(currentSpanId());
      }
      return outer.spanId;
    });
    const byName = new Map((await finished(exporter)).map((span) => [span.name, span]));
    const genId = byName.get("gen")?.spanContext().spanId;
    expect(inBody).toEqual([genId, genId]);
    expect(inLoop).toEqual([outerId, outerId]);
    expect(byName.get("gen")?.parentSpanContext?.spanId).toBe(
      byName.get("outer")?.spanContext().spanId,
    );
    expect(byName.get("child")?.parentSpanContext?.spanId).toBe(genId);
  });

  it("ends with status ok and the partial output on break in for await…of", async () => {
    const { exporter } = startMemory();
    const traced = observe(
      async function* gen() {
        yield "a";
        yield "b";
        yield "c";
      },
      { name: "gen" },
    );
    const seen: string[] = [];
    for await (const item of traced()) {
      seen.push(item);
      if (item === "b") {
        break;
      }
    }
    expect(seen).toEqual(["a", "b"]);
    const [span] = await finished(exporter);
    expect(span?.status.code).toBe(SpanStatusCode.OK);
    expect(span?.attributes["llmobs.output"]).toBe("ab");
  });

  it("records an exception from the body and rejects with it", async () => {
    const { exporter } = startMemory();
    const thrown = new ScenarioError("late");
    const wrapped = observe(
      async function* gen() {
        yield 1;
        throw thrown;
      },
      { name: "gen" },
    )();
    await expect(wrapped.next()).resolves.toEqual({ value: 1, done: false });
    await expect(wrapped.next()).rejects.toBe(thrown);
    const [span] = await finished(exporter);
    expect(span?.status.code).toBe(SpanStatusCode.ERROR);
    expect(span?.status.message).toBe("ScenarioError: late");
    expect(span?.events).toHaveLength(1);
    expect(span?.attributes["llmobs.output"]).toBeUndefined();
  });

  it("forwards a consumer throw() and records the error", async () => {
    const { exporter } = startMemory();
    const injected = new ScenarioError("injected");
    const wrapped = observe(
      async function* gen() {
        yield "a";
        yield "b";
      },
      { name: "gen" },
    )();
    await expect(wrapped.next()).resolves.toEqual({ value: "a", done: false });
    await expect(wrapped.throw(injected)).rejects.toBe(injected);
    const [span] = await finished(exporter);
    expect(span?.status.code).toBe(SpanStatusCode.ERROR);
    expect(span?.status.message).toBe("ScenarioError: injected");
  });

  it("records the items as a JSON list when not every item is a string", async () => {
    const { exporter } = startMemory();
    const traced = observe(
      async function* numbers() {
        yield { n: 1 };
        yield { n: 2 };
      },
      { name: "numbers" },
    );
    const items: unknown[] = [];
    for await (const item of traced()) {
      items.push(item);
    }
    expect(items).toEqual([{ n: 1 }, { n: 2 }]);
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.output"]).toBe('[{"n":1},{"n":2}]');
    expect(span?.attributes["llmobs.output.format"]).toBe("json");
  });

  it("never exports an abandoned async generator", async () => {
    const { exporter } = startMemory();
    const wrapped = observe(
      async function* gen() {
        yield 1;
        yield 2;
      },
      { name: "gen" },
    )();
    await expect(wrapped.next()).resolves.toEqual({ value: 1, done: false });
    expect(await finished(exporter)).toHaveLength(0);
  });
});

describe("call mode around generator-like values", () => {
  it("traces a plain function returning a generator as a call without output", async () => {
    const { exporter } = startMemory();
    function* numbers() {
      yield 1;
      yield 2;
    }
    const gen = observe(() => numbers(), { name: "make" })();
    expect(Object.prototype.toString.call(gen)).toBe("[object Generator]");
    expect([...gen]).toEqual([1, 2]);
    const [span] = await finished(exporter);
    expect(span?.name).toBe("make");
    expect(span?.attributes["llmobs.output"]).toBeUndefined();
  });

  it("traces an async function resolving to an async generator as a call without output", async () => {
    const { exporter } = startMemory();
    async function* stream() {
      yield 1;
    }
    const gen = await observe(async () => stream(), { name: "make" })();
    expect(Object.prototype.toString.call(gen)).toBe("[object AsyncGenerator]");
    const seen: number[] = [];
    for await (const item of gen) {
      seen.push(item);
    }
    expect(seen).toEqual([1]);
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.output"]).toBeUndefined();
  });

  it("returns an AsyncIterable unchanged, unconsumed, recorded as a value", async () => {
    const { exporter } = startMemory();
    const source = {
      brand: "custom",
      async *[Symbol.asyncIterator]() {
        yield 1;
        yield 2;
      },
    };
    expect(observe(() => source, { name: "give" })()).toBe(source);
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.output"]).toBe('{"brand":"custom"}');
    expect(span?.attributes["llmobs.output.format"]).toBe("json");
    const seen: number[] = [];
    for await (const item of source) {
      seen.push(item);
    }
    expect(seen).toEqual([1, 2]);
  });

  it("returns a ReadableStream unchanged, unconsumed, recorded as {}", async () => {
    const { exporter } = startMemory();
    const stream = new ReadableStream<string>({
      start(controller) {
        controller.enqueue("chunk-1");
        controller.enqueue("chunk-2");
        controller.close();
      },
    });
    expect(observe(() => stream, { name: "give" })()).toBe(stream);
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.output"]).toBe("{}");
    expect(span?.attributes["llmobs.output.format"]).toBe("json");
    const reader = stream.getReader();
    expect(await reader.read()).toEqual({ value: "chunk-1", done: false });
    expect(await reader.read()).toEqual({ value: "chunk-2", done: false });
  });
});
