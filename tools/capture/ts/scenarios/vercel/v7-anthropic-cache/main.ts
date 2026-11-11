import { createAnthropic } from "@ai-sdk/anthropic-v7";
import { OpenTelemetry } from "@ai-sdk/otel-v7";
import { generateText } from "ai-v7";

import { finish, mockUrl, setup } from "../../../harness.ts";

const ALIASES = {
  ai: "ai-v7",
  "@ai-sdk/anthropic": "@ai-sdk/anthropic-v7",
  "@ai-sdk/otel": "@ai-sdk/otel-v7",
};

async function main(): Promise<void> {
  const provider = setup();
  try {
    const anthropic = createAnthropic({
      baseURL: mockUrl("/v1"),
      apiKey: process.env.ANTHROPIC_API_KEY,
    });
    await generateText({
      model: anthropic("claude-sonnet-4-5"),
      maxRetries: 0,
      allowSystemInMessages: true,
      messages: [
        {
          role: "system",
          content: "You are a weather assistant.",
          providerOptions: {
            anthropic: { cacheControl: { type: "ephemeral" } },
          },
        },
        { role: "user", content: "What's the weather in Paris?" },
      ],
      telemetry: {
        isEnabled: true,
        functionId: "v7-anthropic-cache",
        integrations: new OpenTelemetry(),
      },
    });
  } finally {
    await finish(provider, ALIASES);
  }
}

main().catch((error: unknown) => {
  const message = error instanceof Error ? error.name : "Error";
  process.stderr.write(`capture scenario failed: ${message}\n`);
  process.exitCode = 1;
});
