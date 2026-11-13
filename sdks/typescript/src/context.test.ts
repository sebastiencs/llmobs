import { context, diag, propagation, ROOT_CONTEXT } from "@opentelemetry/api";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { CONTEXT_KEY, currentValues, withContext } from "./context.js";
import { resetForTests } from "./runtime.js";
import { finished, startMemory } from "./testing.test-helpers.js";

beforeEach(async () => {
  await resetForTests();
});

afterEach(async () => {
  await resetForTests();
});

describe("withContext", () => {
  it("stamps the values on spans inside the block, children included", async () => {
    const { exporter, runtime } = startMemory({ environment: "staging" });
    withContext(
      { sessionId: "s-1", userId: "u-1", tags: ["beta"], metadata: { plan: "pro" } },
      () => {
        runtime.tracer.startActiveSpan("parent", (parent) => {
          runtime.tracer.startSpan("child").end();
          parent.end();
        });
      },
    );
    const spans = await finished(exporter);
    expect(spans.map((span) => span.name).sort()).toEqual(["child", "parent"]);
    for (const span of spans) {
      expect(span.attributes["llmobs.session.id"]).toBe("s-1");
      expect(span.attributes["llmobs.user.id"]).toBe("u-1");
      expect(span.attributes["llmobs.tags"]).toEqual(["beta"]);
      expect(span.attributes["llmobs.metadata.plan"]).toBe("pro");
      expect(span.attributes["llmobs.environment"]).toBe("staging");
    }
  });

  it("stamps spans of another tracer of our provider", async () => {
    const { exporter, runtime } = startMemory();
    const thirdParty = runtime.provider.getTracer("thirdparty");
    withContext({ sessionId: "s-1" }, () => {
      thirdParty.startSpan("ext").end();
    });
    const [span] = await finished(exporter);
    expect(span?.name).toBe("ext");
    expect(span?.attributes["llmobs.session.id"]).toBe("s-1");
  });

  it("merges nested blocks and restores the outer values on exit", async () => {
    const { exporter, runtime } = startMemory();
    withContext(
      {
        sessionId: "outer",
        userId: "u-1",
        tags: ["a", "b"],
        metadata: { k: "outer", onlyOuter: 1 },
      },
      () => {
        withContext({ sessionId: "inner", tags: ["b", "c"], metadata: { k: "inner" } }, () => {
          runtime.tracer.startSpan("nested").end();
        });
        runtime.tracer.startSpan("outer-span").end();
      },
    );
    runtime.tracer.startSpan("after").end();
    const spans = await finished(exporter);
    const byName = new Map(spans.map((span) => [span.name, span]));
    const nested = byName.get("nested")?.attributes ?? {};
    expect(nested["llmobs.session.id"]).toBe("inner");
    expect(nested["llmobs.user.id"]).toBe("u-1");
    expect(nested["llmobs.tags"]).toEqual(["a", "b", "c"]);
    expect(nested["llmobs.metadata.k"]).toBe("inner");
    expect(nested["llmobs.metadata.onlyOuter"]).toBe(1);
    const outerSpan = byName.get("outer-span")?.attributes ?? {};
    expect(outerSpan["llmobs.session.id"]).toBe("outer");
    expect(outerSpan["llmobs.tags"]).toEqual(["a", "b"]);
    expect(outerSpan["llmobs.metadata.k"]).toBe("outer");
    const after = byName.get("after")?.attributes ?? {};
    expect(after["llmobs.session.id"]).toBeUndefined();
    expect(after["llmobs.metadata.k"]).toBeUndefined();
    expect(currentValues().sessionId).toBeUndefined();
  });

  it("isolates two concurrent async flows", async () => {
    const { exporter, runtime } = startMemory();
    const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
    await Promise.all([
      withContext({ sessionId: "one" }, async () => {
        await pause(15);
        runtime.tracer.startSpan("slow").end();
      }),
      withContext({ sessionId: "two" }, async () => {
        await pause(1);
        runtime.tracer.startSpan("fast").end();
      }),
    ]);
    const spans = await finished(exporter);
    const byName = new Map(spans.map((span) => [span.name, span]));
    expect(byName.get("slow")?.attributes["llmobs.session.id"]).toBe("one");
    expect(byName.get("fast")?.attributes["llmobs.session.id"]).toBe("two");
  });

  it("sets only the two baggage keys, only with propagate", () => {
    startMemory();
    // Without propagate the baggage is untouched.
    withContext({ sessionId: "s-1", userId: "u-1", tags: ["t"] }, () => {
      expect(propagation.getBaggage(context.active())).toBeUndefined();
    });
    // With propagate the existing baggage is kept and only our two keys added.
    const base = propagation.createBaggage().setEntry("correlation-id", { value: "c-1" });
    context.with(propagation.setBaggage(context.active(), base), () => {
      withContext(
        { sessionId: "s-1", userId: "u-1", tags: ["t"], metadata: { m: 1 }, propagate: true },
        () => {
          const entries =
            propagation
              .getBaggage(context.active())
              ?.getAllEntries()
              .sort(([a], [b]) => (a < b ? -1 : 1)) ?? [];
          expect(entries).toEqual([
            ["correlation-id", { value: "c-1" }],
            ["llmobs.session.id", { value: "s-1" }],
            ["llmobs.user.id", { value: "u-1" }],
          ]);
        },
      );
    });
  });

  it("reads the session and user from baggage alone, below the context values", async () => {
    const { exporter, runtime } = startMemory();
    const baggage = propagation
      .createBaggage()
      .setEntry("llmobs.session.id", { value: "bag-s" })
      .setEntry("llmobs.user.id", { value: "bag-u" });
    context.with(propagation.setBaggage(ROOT_CONTEXT, baggage), () => {
      runtime.tracer.startSpan("from-baggage").end();
      withContext({ sessionId: "ctx-s" }, () => {
        runtime.tracer.startSpan("wins").end();
      });
    });
    const spans = await finished(exporter);
    const byName = new Map(spans.map((span) => [span.name, span]));
    expect(byName.get("from-baggage")?.attributes["llmobs.session.id"]).toBe("bag-s");
    expect(byName.get("from-baggage")?.attributes["llmobs.user.id"]).toBe("bag-u");
    expect(byName.get("wins")?.attributes["llmobs.session.id"]).toBe("ctx-s");
  });

  it("drops invalid options silently, keeping a string tags as one tag", async () => {
    const { exporter, runtime } = startMemory();
    withContext(
      {
        sessionId: 42 as unknown as string,
        userId: null as unknown as string,
        tags: [1, "b", true] as unknown as readonly string[],
        metadata: "nope" as unknown as Readonly<Record<string, unknown>>,
      },
      () => {
        runtime.tracer.startSpan("junk").end();
      },
    );
    withContext({ tags: "solo" as unknown as readonly string[] }, () => {
      runtime.tracer.startSpan("one-tag").end();
    });
    const spans = await finished(exporter);
    const byName = new Map(spans.map((span) => [span.name, span]));
    const junk = byName.get("junk")?.attributes ?? {};
    expect(junk["llmobs.session.id"]).toBeUndefined();
    expect(junk["llmobs.user.id"]).toBeUndefined();
    expect(junk["llmobs.tags"]).toEqual(["b"]);
    expect(junk["llmobs.metadata.m"]).toBeUndefined();
    expect(byName.get("one-tag")?.attributes["llmobs.tags"]).toEqual(["solo"]);
  });

  it("runs fn without a runtime and returns its value, a promise included", () => {
    expect(withContext({ sessionId: "s-1" }, () => "ok")).toBe("ok");
    expect(withContext({}, () => 42)).toBe(42);
    const promise = Promise.resolve(7);
    expect(withContext({}, () => promise)).toBe(promise);
    expect(() =>
      withContext({}, () => {
        throw new Error("boom");
      }),
    ).toThrow("boom");
  });
});

describe("metadata attributes", () => {
  it("writes scalar types and JSON strings, skipping bad keys", async () => {
    const { exporter, runtime } = startMemory();
    const debug = vi.spyOn(diag, "debug").mockImplementation(() => {});
    try {
      withContext(
        {
          metadata: {
            seats: 3,
            ratio: 0.5,
            flag: true,
            obj: { a: 1 },
            big: 2 ** 60,
            "bad key!": 1,
          },
        },
        () => {
          runtime.tracer.startSpan("md").end();
        },
      );
      const [span] = await finished(exporter);
      const attributes = span?.attributes ?? {};
      expect(attributes["llmobs.metadata.seats"]).toBe(3);
      expect(attributes["llmobs.metadata.ratio"]).toBe(0.5);
      expect(attributes["llmobs.metadata.flag"]).toBe(true);
      expect(attributes["llmobs.metadata.obj"]).toBe('{"a":1}');
      // Not a safe integer, so a JSON string; JS prints the shortest
      // round-trip decimal (Python would print the exact digits).
      expect(attributes["llmobs.metadata.big"]).toBe("1152921504606847000");
      expect(attributes["llmobs.metadata.bad key!"]).toBeUndefined();
      // Once per key, length only, never the key itself.
      expect(debug).toHaveBeenCalledWith("llmobs: metadata key of length 8 skipped");
      expect(debug).toHaveBeenCalledTimes(1);
    } finally {
      debug.mockRestore();
    }
  });

  it("masks metadata values when the block is entered", async () => {
    const { exporter, runtime } = startMemory({
      mask: (value) => (typeof value === "string" ? value.toUpperCase() : value),
    });
    withContext({ metadata: { note: "quiet" } }, () => {
      runtime.tracer.startSpan("m").end();
    });
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.metadata.note"]).toBe("QUIET");
  });
});

describe("the context key", () => {
  it("never throws on garbage under the key", async () => {
    const { exporter, runtime } = startMemory();
    context.with(context.active().setValue(CONTEXT_KEY, "garbage"), () => {
      runtime.tracer.startSpan("g1").end();
    });
    context.with(
      context.active().setValue(CONTEXT_KEY, { sessionId: 42, tags: "x", metadata: [1] }),
      () => {
        runtime.tracer.startSpan("g2").end();
      },
    );
    const spans = await finished(exporter);
    const byName = new Map(spans.map((span) => [span.name, span]));
    const g2 = byName.get("g2")?.attributes ?? {};
    expect(byName.has("g1")).toBe(true);
    expect(g2["llmobs.session.id"]).toBeUndefined();
    expect(g2["llmobs.tags"]).toBeUndefined();
    expect(g2["llmobs.metadata.length"]).toBeUndefined();
  });
});
