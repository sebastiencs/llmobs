import { createOpenAI } from "@ai-sdk/openai-v7";
import { OpenTelemetry } from "@ai-sdk/otel-v7";
import { streamText } from "ai-v7";

import { finish, mockUrl, setup } from "../../../harness.ts";

const MODEL = "gpt-4o-mini-2024-07-18";
const ALIASES = {
  ai: "ai-v7",
  "@ai-sdk/openai": "@ai-sdk/openai-v7",
  "@ai-sdk/otel": "@ai-sdk/otel-v7",
};

async function main(): Promise<void> {
  const provider = setup();
  try {
    const openai = createOpenAI({
      baseURL: mockUrl("/v1"),
      apiKey: process.env.OPENAI_API_KEY,
    });
    const result = streamText({
      model: openai.chat(MODEL),
      maxRetries: 0,
      prompt: "Describe the harbor in a few words.",
      telemetry: {
        isEnabled: true,
        functionId: "v7-stream-text",
        integrations: new OpenTelemetry(),
      },
    });
    await result.text;
    await result.usage;
  } finally {
    await finish(provider, ALIASES);
  }
}

main().catch((error: unknown) => {
  const message = error instanceof Error ? error.name : "Error";
  process.stderr.write(`capture scenario failed: ${message}\n`);
  process.exitCode = 1;
});
