import { SpanKind, SpanStatusCode } from "@opentelemetry/api";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { type LlmSpanHandle, withLlmSpan } from "./llm.js";
import { resetForTests } from "./runtime.js";
import { withSpan } from "./span.js";
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

describe("withLlmSpan attributes", () => {
  it("writes the llm-span scenario's attribute set", async () => {
    const { exporter } = startMemory();
    withLlmSpan(
      "chat",
      {
        provider: "openai",
        model: "gpt-4o-mini",
        inputMessages: [
          { role: "system", content: "Be brief." },
          { role: "user", content: "hi" },
        ],
        params: { temperature: 0.2 },
      },
      (span) => {
        span.setOutputMessages([{ role: "assistant", content: "hello", finish_reason: "stop" }]);
        span.setResponseModel("gpt-4o-mini-2024-07-18");
        span.setUsage({ inputTokens: 12, outputTokens: 3 });
      },
    );
    const [span] = await finished(exporter);
    expect(span?.name).toBe("chat");
    expect(span?.kind).toBe(SpanKind.INTERNAL);
    expect(span?.status.code).toBe(SpanStatusCode.UNSET);
    const attributes = span?.attributes ?? {};
    expect(attributes["llmobs.span.kind"]).toBe("llm");
    expect(attributes["gen_ai.operation.name"]).toBe("chat");
    expect(attributes["gen_ai.provider.name"]).toBe("openai");
    expect(attributes["gen_ai.request.model"]).toBe("gpt-4o-mini");
    expect(attributes["gen_ai.request.temperature"]).toBe(0.2);
    expect(attributes["gen_ai.response.model"]).toBe("gpt-4o-mini-2024-07-18");
    expect(attributes["gen_ai.input.messages"]).toBe(
      '[{"role":"system","parts":[{"type":"text","content":"Be brief."}]},{"role":"user","parts":[{"type":"text","content":"hi"}]}]',
    );
    expect(attributes["gen_ai.output.messages"]).toBe(
      '[{"role":"assistant","finish_reason":"stop","parts":[{"type":"text","content":"hello"}]}]',
    );
    expect(attributes["gen_ai.usage.input_tokens"]).toBe(12);
    expect(attributes["gen_ai.usage.output_tokens"]).toBe(3);
    // The body's return value and the chat-less payloads are never written.
    expect(attributes["llmobs.input"]).toBeUndefined();
    expect(attributes["llmobs.output"]).toBeUndefined();
  });

  it("records system instructions from a string and from parts", async () => {
    const { exporter } = startMemory();
    withLlmSpan("a", { provider: "openai", model: "m", systemInstructions: "Be nice." }, () => {});
    withLlmSpan(
      "b",
      {
        provider: "openai",
        model: "m",
        systemInstructions: [
          { type: "text", content: "a" },
          { type: "text", content: "b" },
        ],
      },
      () => {},
    );
    const spans = await finished(exporter);
    expect(spans[0]?.attributes["gen_ai.system_instructions"]).toBe(
      '[{"type":"text","content":"Be nice."}]',
    );
    expect(spans[1]?.attributes["gen_ai.system_instructions"]).toBe(
      '[{"type":"text","content":"a"},{"type":"text","content":"b"}]',
    );
  });

  it("writes only the scalar params", async () => {
    const { exporter } = startMemory();
    withLlmSpan(
      "p",
      {
        provider: "openai",
        model: "m",
        params: {
          temperature: 0.2,
          topP: 1,
          stream: true,
          stop: "none",
          maxTokens: 7,
          object: { a: 1 },
          nothing: null,
          gone: undefined,
          nan: Number.NaN,
          infinite: Number.POSITIVE_INFINITY,
        },
      },
      () => {},
    );
    const attributes = (await finished(exporter))[0]?.attributes ?? {};
    const paramKeys = Object.keys(attributes)
      .filter((key) => key.startsWith("gen_ai.request."))
      .sort();
    expect(paramKeys).toEqual([
      "gen_ai.request.maxTokens",
      "gen_ai.request.model",
      "gen_ai.request.stop",
      "gen_ai.request.stream",
      "gen_ai.request.temperature",
      "gen_ai.request.topP",
    ]);
    expect(attributes["gen_ai.request.topP"]).toBe(1);
    expect(attributes["gen_ai.request.stream"]).toBe(true);
    expect(attributes["gen_ai.request.stop"]).toBe("none");
    expect(attributes["gen_ai.request.maxTokens"]).toBe(7);
  });

  it("writes every usage counter that is a safe integer >= 0, skips the rest", async () => {
    const { exporter } = startMemory();
    withLlmSpan("u", { provider: "openai", model: "m" }, (span) => {
      span.setUsage({
        inputTokens: 12,
        outputTokens: 0,
        cacheReadTokens: 4,
        cacheWriteTokens: 5,
        reasoningTokens: 6,
      });
    });
    withLlmSpan("v", { provider: "openai", model: "m" }, (span) => {
      span.setUsage({
        inputTokens: -1,
        outputTokens: 1.5,
        cacheReadTokens: 2 ** 53,
        cacheWriteTokens: Number.POSITIVE_INFINITY,
        reasoningTokens: true as unknown as number,
      });
    });
    const spans = await finished(exporter);
    const usageKeys = (attributes: Record<string, unknown>) =>
      Object.keys(attributes).filter((key) => key.startsWith("gen_ai.usage."));
    expect(usageKeys(spans[0]?.attributes ?? {}).sort()).toEqual([
      "gen_ai.usage.cache_creation.input_tokens",
      "gen_ai.usage.cache_read.input_tokens",
      "gen_ai.usage.input_tokens",
      "gen_ai.usage.output_tokens",
      "gen_ai.usage.reasoning.output_tokens",
    ]);
    expect(spans[0]?.attributes["gen_ai.usage.output_tokens"]).toBe(0);
    expect(spans[0]?.attributes["gen_ai.usage.cache_read.input_tokens"]).toBe(4);
    expect(usageKeys(spans[1]?.attributes ?? {})).toEqual([]);
  });

  it("writes a finite non-negative cost and skips anything else", async () => {
    const { exporter } = startMemory();
    withLlmSpan("c", { provider: "openai", model: "m" }, (span) => {
      span.setCost(0.0012);
    });
    withLlmSpan("d", { provider: "openai", model: "m" }, (span) => {
      span.setCost(-1);
      span.setCost(Number.NaN);
      span.setCost(Number.POSITIVE_INFINITY);
    });
    const spans = await finished(exporter);
    expect(spans[0]?.attributes["llmobs.cost.total"]).toBe(0.0012);
    expect(spans[1]?.attributes["llmobs.cost.total"]).toBeUndefined();
  });

  it("records the first token once, from the span start", async () => {
    const { exporter } = startMemory();
    await withLlmSpan("t", { provider: "openai", model: "m" }, async (span) => {
      span.recordFirstToken();
      await pause(10);
      span.recordFirstToken();
    });
    const [span] = await finished(exporter);
    const ttft = span?.attributes["llmobs.ttft_ns"];
    expect(typeof ttft).toBe("number");
    expect(Number.isSafeInteger(ttft)).toBe(true);
    // The second call must not overwrite: it ran >= 10 ms in, the first at ~0.
    expect(ttft as number).toBeLessThan(10_000_000);
  });

  it("converts short-form messages and keeps every other shape", async () => {
    const { exporter } = startMemory();
    withLlmSpan(
      "s",
      {
        provider: "openai",
        model: "m",
        inputMessages: [
          { role: "assistant", parts: [{ type: "text", content: "kept" }] },
          { role: "assistant", name: "bot", finish_reason: "stop", content: "hi" },
          { role: "user", content: 5 },
          { content: "no role" },
          "free text",
          42,
        ],
      },
      () => {},
    );
    const attributes = (await finished(exporter))[0]?.attributes ?? {};
    expect(attributes["gen_ai.input.messages"]).toBe(
      '[{"role":"assistant","parts":[{"type":"text","content":"kept"}]},' +
        '{"role":"assistant","name":"bot","finish_reason":"stop","parts":[{"type":"text","content":"hi"}]},' +
        '{"role":"user","content":5},' +
        '{"content":"no role"},' +
        '"free text",' +
        "42]",
    );
  });

  it("masks the message list before conversion", async () => {
    const redact = (value: unknown): unknown => {
      if (typeof value === "string") {
        return value === "alice@example.com" ? "[redacted]" : value;
      }
      if (Array.isArray(value)) {
        return value.map(redact);
      }
      if (typeof value === "object" && value !== null) {
        return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, redact(item)]));
      }
      return value;
    };
    const { exporter } = startMemory({ mask: redact });
    withLlmSpan(
      "m",
      {
        provider: "openai",
        model: "m",
        inputMessages: [{ role: "user", content: "alice@example.com" }],
      },
      (span) => {
        span.setOutputMessages([{ role: "assistant", content: "alice@example.com" }]);
      },
    );
    const attributes = (await finished(exporter))[0]?.attributes ?? {};
    expect(attributes["gen_ai.input.messages"]).toBe(
      '[{"role":"user","parts":[{"type":"text","content":"[redacted]"}]}]',
    );
    expect(attributes["gen_ai.output.messages"]).toBe(
      '[{"role":"assistant","parts":[{"type":"text","content":"[redacted]"}]}]',
    );
  });

  it("keeps model, params, usage, cost and ttft with captureContent: false", async () => {
    const { exporter } = startMemory({ captureContent: false });
    withLlmSpan(
      "o",
      {
        provider: "openai",
        model: "gpt-4o-mini",
        inputMessages: [{ role: "user", content: "hi" }],
        systemInstructions: "Be brief.",
        params: { temperature: 0.2 },
      },
      (span) => {
        span.setInputMessages([{ role: "user", content: "again" }]);
        span.setOutputMessages([{ role: "assistant", content: "hello" }]);
        span.setResponseModel("gpt-4o-mini-2024-07-18");
        span.setUsage({ inputTokens: 12, outputTokens: 3 });
        span.setCost(0.001);
        span.recordFirstToken();
      },
    );
    const attributes = (await finished(exporter))[0]?.attributes ?? {};
    expect(attributes["llmobs.span.kind"]).toBe("llm");
    expect(attributes["gen_ai.operation.name"]).toBe("chat");
    expect(attributes["gen_ai.provider.name"]).toBe("openai");
    expect(attributes["gen_ai.request.model"]).toBe("gpt-4o-mini");
    expect(attributes["gen_ai.request.temperature"]).toBe(0.2);
    expect(attributes["gen_ai.response.model"]).toBe("gpt-4o-mini-2024-07-18");
    expect(attributes["gen_ai.usage.input_tokens"]).toBe(12);
    expect(attributes["llmobs.cost.total"]).toBe(0.001);
    expect(typeof attributes["llmobs.ttft_ns"]).toBe("number");
    expect(attributes["gen_ai.input.messages"]).toBeUndefined();
    expect(attributes["gen_ai.output.messages"]).toBeUndefined();
    expect(attributes["gen_ai.system_instructions"]).toBeUndefined();
  });

  it("writes metadata from the options and the handle, masked per value", async () => {
    const { exporter } = startMemory({
      mask: (value) => (typeof value === "string" ? value.toUpperCase() : value),
    });
    withLlmSpan("x", { provider: "openai", model: "m", metadata: { plan: "pro" } }, (span) => {
      span.setMetadata({ seats: 3 });
    });
    const attributes = (await finished(exporter))[0]?.attributes ?? {};
    expect(attributes["llmobs.metadata.plan"]).toBe("PRO");
    expect(attributes["llmobs.metadata.seats"]).toBe(3);
  });
});

describe("withLlmSpan control flow", () => {
  it("raises TypeError on an invalid provider or model before the body runs", () => {
    let ran = false;
    const body = () => {
      ran = true;
    };
    expect(() => withLlmSpan("x", { provider: "", model: "m" }, body)).toThrowError(
      new TypeError("provider: must be a non-empty string"),
    );
    expect(() =>
      withLlmSpan("x", { provider: "openai", model: 3 as unknown as string }, body),
    ).toThrowError(new TypeError("model: must be a non-empty string"));
    expect(ran).toBe(false);
  });

  it("runs the body with a no-op handle when no runtime is installed", () => {
    const result = withLlmSpan("n", { provider: "openai", model: "m" }, (span) => {
      expect(span.traceId).toBeUndefined();
      expect(span.spanId).toBeUndefined();
      span.setInputMessages([]);
      span.setOutputMessages([]);
      span.setResponseModel("r");
      span.setUsage({ inputTokens: 1 });
      span.setCost(1);
      span.recordFirstToken();
      span.setInput("i");
      span.setOutput("o");
      span.setMetadata({ k: 1 });
      return 42;
    });
    expect(result).toBe(42);
  });

  it("exposes the span ids on the handle", async () => {
    const { exporter } = startMemory();
    let spanId: string | undefined;
    withLlmSpan("i", { provider: "openai", model: "m" }, (span) => {
      spanId = span.spanId;
    });
    const [span] = await finished(exporter);
    expect(spanId).toMatch(/^[0-9a-f]{16}$/);
    expect(span?.spanContext().spanId).toBe(spanId);
  });

  it("records the exception, sets the error status and re-throws unchanged", async () => {
    const { exporter } = startMemory();
    const thrown = new ScenarioError("boom");
    let caught: unknown;
    try {
      withLlmSpan("f", { provider: "openai", model: "m" }, () => {
        throw thrown;
      });
    } catch (error) {
      caught = error;
    }
    expect(caught).toBe(thrown);
    const [span] = await finished(exporter);
    expect(span?.status.code).toBe(SpanStatusCode.ERROR);
    expect(span?.status.message).toBe("ScenarioError: boom");
    const event = span?.events[0];
    expect(event?.name).toBe("exception");
    expect(event?.attributes?.["exception.type"]).toBe("ScenarioError");
    expect(event?.attributes?.["exception.message"]).toBe("boom");
  });

  it("ends the span when the body's promise settles, without recording its value", async () => {
    const { exporter } = startMemory();
    const value = await withLlmSpan("a", { provider: "openai", model: "m" }, async () => {
      await pause(5);
      return "v";
    });
    expect(value).toBe("v");
    const [span] = await finished(exporter);
    expect(span?.attributes["llmobs.output"]).toBeUndefined();
    expect(span?.attributes["gen_ai.output.messages"]).toBeUndefined();
    expect(span?.status.code).toBe(SpanStatusCode.UNSET);
  });

  it("records the error when the body's promise rejects, and rejects with it", async () => {
    const { exporter } = startMemory();
    const thrown = new Error("late");
    const promise = withLlmSpan("r", { provider: "openai", model: "m" }, () =>
      Promise.reject(thrown),
    );
    await expect(promise).rejects.toBe(thrown);
    const [span] = await finished(exporter);
    expect(span?.status.code).toBe(SpanStatusCode.ERROR);
    expect(span?.status.message).toBe("Error: late");
  });

  it("ignores handle updates after the span ended", async () => {
    const { exporter } = startMemory();
    let handle: LlmSpanHandle | undefined;
    withLlmSpan("h", { provider: "openai", model: "m" }, (span) => {
      handle = span;
    });
    handle?.setInputMessages([{ role: "user", content: "late" }]);
    handle?.setOutputMessages([{ role: "user", content: "late" }]);
    handle?.setResponseModel("late");
    handle?.setUsage({ inputTokens: 1 });
    handle?.setCost(1);
    handle?.recordFirstToken();
    handle?.setOutput("late");
    const attributes = (await finished(exporter))[0]?.attributes ?? {};
    expect(attributes["gen_ai.input.messages"]).toBeUndefined();
    expect(attributes["gen_ai.output.messages"]).toBeUndefined();
    expect(attributes["gen_ai.response.model"]).toBeUndefined();
    expect(attributes["gen_ai.usage.input_tokens"]).toBeUndefined();
    expect(attributes["llmobs.cost.total"]).toBeUndefined();
    expect(attributes["llmobs.ttft_ns"]).toBeUndefined();
    expect(attributes["llmobs.output"]).toBeUndefined();
  });

  it("nests a child span under the LLM span", async () => {
    const { exporter } = startMemory();
    await withLlmSpan("outer", { provider: "openai", model: "m" }, async () => {
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
