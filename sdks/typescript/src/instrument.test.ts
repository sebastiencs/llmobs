import Anthropic from "@anthropic-ai/sdk";
import { isPatched } from "@arizeai/openinference-instrumentation-openai";
import * as callbacksManager from "@langchain/core/callbacks/manager";
import type { TracerProvider } from "@opentelemetry/api";
import type { ReadableSpan } from "@opentelemetry/sdk-trace";
import OpenAI from "openai";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { anthropic } from "./anthropic.js";
import { withContext } from "./context.js";
import { langchain } from "./langchain.js";
import { openai } from "./openai.js";
import { resetForTests, shutdown } from "./runtime.js";
import { observe } from "./span.js";
import { finished, startMemory } from "./testing.test-helpers.js";

/** P10's minimal provider responses, returned by the injected fetch. */
const CHAT_COMPLETION = {
  id: "chatcmpl-1",
  object: "chat.completion",
  created: 0,
  model: "gpt-4o-mini",
  choices: [{ index: 0, message: { role: "assistant", content: "hi" }, finish_reason: "stop" }],
  usage: { prompt_tokens: 5, completion_tokens: 1, total_tokens: 6 },
};

const MESSAGE = {
  id: "msg_1",
  type: "message",
  role: "assistant",
  model: "claude-sonnet-4-5",
  content: [{ type: "text", text: "hi" }],
  stop_reason: "end_turn",
  stop_sequence: null,
  usage: { input_tokens: 5, output_tokens: 1 },
};

function jsonResponse(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { "content-type": "application/json" },
  });
}

/** A hand-written instrumentation recording every call (T11). */
class FakeInstrumentation {
  readonly calls: string[] = [];
  provider: TracerProvider | undefined;
  failOnSetProvider = false;

  setTracerProvider(provider: TracerProvider): void {
    this.provider = provider;
    this.calls.push("setTracerProvider");
    if (this.failOnSetProvider) {
      throw new Error("boom");
    }
  }

  enable(): void {
    this.calls.push("enable");
  }

  disable(): void {
    this.calls.push("disable");
  }
}

function llmSpans(spans: ReadableSpan[]): ReadableSpan[] {
  return spans.filter((span) => span.attributes["openinference.span.kind"] === "LLM");
}

beforeEach(async () => {
  await resetForTests();
});

afterEach(async () => {
  await resetForTests();
});

describe("instrument wiring", () => {
  it("binds each instrumentation to the runtime provider, then enables it", async () => {
    const fake = new FakeInstrumentation();
    const { runtime } = startMemory({ instrument: [fake] });
    expect(fake.provider).toBe(runtime.provider);
    expect(fake.calls).toEqual(["setTracerProvider", "enable"]);
    await resetForTests();
    expect(fake.calls).toEqual(["setTracerProvider", "enable", "disable"]);
  });

  it("logs and skips a failing instrumentation, keeping the others", async () => {
    const failing = new FakeInstrumentation();
    failing.failOnSetProvider = true;
    const working = new FakeInstrumentation();
    startMemory({ instrument: [failing, working] });
    expect(failing.calls).toEqual(["setTracerProvider"]);
    expect(working.calls).toEqual(["setTracerProvider", "enable"]);
    await shutdown();
    // Never enabled, so never disabled either.
    expect(failing.calls).toEqual(["setTracerProvider"]);
    expect(working.calls).toEqual(["setTracerProvider", "enable", "disable"]);
  });
});

describe("openai helper", () => {
  it("traces a chat completion end to end as a child of our span", async () => {
    const { exporter } = startMemory({ instrument: [openai(OpenAI)] });
    const client = new OpenAI({
      apiKey: "test",
      baseURL: "http://127.0.0.1:9/v1",
      fetch: () => Promise.resolve(jsonResponse(CHAT_COMPLETION)),
    });
    await withContext({ sessionId: "s1" }, () =>
      observe(
        async () => {
          await client.chat.completions.create({
            model: "gpt-4o-mini",
            messages: [{ role: "user", content: "hello" }],
          });
        },
        { name: "call" },
      )(),
    );
    const spans = await finished(exporter);
    const ours = spans.find((span) => span.name === "call");
    const [llm] = llmSpans(spans);
    expect(llm).toBeDefined();
    expect(llm?.parentSpanContext?.spanId).toBe(ours?.spanContext().spanId);
    expect(llm?.attributes["llmobs.session.id"]).toBe("s1");
    expect(llm?.attributes["llm.model_name"]).toBe("gpt-4o-mini");
    expect(String(llm?.attributes["input.value"])).toContain("hello");
  });

  it("hides inputs and outputs with captureContent: false", async () => {
    const { exporter } = startMemory({
      instrument: [openai(OpenAI, { captureContent: false })],
    });
    const client = new OpenAI({
      apiKey: "test",
      baseURL: "http://127.0.0.1:9/v1",
      fetch: () => Promise.resolve(jsonResponse(CHAT_COMPLETION)),
    });
    await observe(
      async () => {
        await client.chat.completions.create({
          model: "gpt-4o-mini",
          messages: [{ role: "user", content: "hello" }],
        });
      },
      { name: "call" },
    )();
    const [llm] = llmSpans(await finished(exporter));
    expect(llm).toBeDefined();
    // The OpenInference trace config redacts the top-level payload values and
    // removes the message attributes entirely.
    expect(llm?.attributes["input.value"]).toBe("__REDACTED__");
    expect(llm?.attributes["output.value"]).toBe("__REDACTED__");
    for (const key of Object.keys(llm?.attributes ?? {})) {
      expect(key.startsWith("llm.input_messages")).toBe(false);
      expect(key.startsWith("llm.output_messages")).toBe(false);
    }
  });

  it("creates no span once the runtime has shut down", async () => {
    const first = startMemory({ instrument: [openai(OpenAI)] });
    const client = new OpenAI({
      apiKey: "test",
      baseURL: "http://127.0.0.1:9/v1",
      fetch: () => Promise.resolve(jsonResponse(CHAT_COMPLETION)),
    });
    await observe(
      async () => {
        await client.chat.completions.create({
          model: "gpt-4o-mini",
          messages: [{ role: "user", content: "hello" }],
        });
      },
      { name: "one" },
    )();
    expect(llmSpans(await finished(first.exporter))).toHaveLength(1);
    expect(isPatched()).toBe(true);
    await shutdown();
    expect(isPatched()).toBe(false);
    const second = startMemory();
    await observe(
      async () => {
        await client.chat.completions.create({
          model: "gpt-4o-mini",
          messages: [{ role: "user", content: "again" }],
        });
      },
      { name: "two" },
    )();
    const spans = await finished(second.exporter);
    expect(llmSpans(spans)).toHaveLength(0);
    expect(spans.map((span) => span.name)).toEqual(["two"]);
  });
});

describe("anthropic helper", () => {
  it("traces a message end to end as a child of our span", async () => {
    const { exporter } = startMemory({ instrument: [anthropic(Anthropic)] });
    const client = new Anthropic({
      apiKey: "test",
      baseURL: "http://127.0.0.1:9/v1",
      fetch: () => Promise.resolve(jsonResponse(MESSAGE)),
    });
    await withContext({ sessionId: "s2" }, () =>
      observe(
        async () => {
          await client.messages.create({
            model: "claude-sonnet-4-5",
            max_tokens: 16,
            messages: [{ role: "user", content: "hello" }],
          });
        },
        { name: "call" },
      )(),
    );
    const spans = await finished(exporter);
    const ours = spans.find((span) => span.name === "call");
    const [llm] = llmSpans(spans);
    expect(llm).toBeDefined();
    expect(llm?.parentSpanContext?.spanId).toBe(ours?.spanContext().spanId);
    expect(llm?.attributes["llmobs.session.id"]).toBe("s2");
    expect(llm?.attributes["llm.model_name"]).toBe("claude-sonnet-4-5");
    expect(String(llm?.attributes["input.value"])).toContain("hello");
  });
});

describe("langchain helper", () => {
  it("builds an instrumentation from the real callbacks module", () => {
    const instrumentation = langchain(callbacksManager);
    expect(typeof instrumentation.setTracerProvider).toBe("function");
    expect(typeof instrumentation.enable).toBe("function");
    expect(typeof instrumentation.disable).toBe("function");
  });
});
