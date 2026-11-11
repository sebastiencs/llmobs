import { createOpenAI } from "@ai-sdk/openai-v5";
import { generateText, jsonSchema, stepCountIs, tool } from "ai-v5";

import { finish, mockUrl, setup } from "../../../harness.ts";

const MODEL = "gpt-4o-mini-2024-07-18";
const ALIASES = {
  ai: "ai-v5",
  "@ai-sdk/openai": "@ai-sdk/openai-v5",
};

async function main(): Promise<void> {
  const provider = setup();
  try {
    const openai = createOpenAI({
      baseURL: mockUrl("/v1"),
      apiKey: process.env.OPENAI_API_KEY,
    });
    await generateText({
      model: openai.responses(MODEL),
      maxRetries: 0,
      system: "You are a weather assistant.",
      prompt: "What's the weather in Paris?",
      tools: {
        get_weather: tool({
          description: "Look up the weather for a city.",
          inputSchema: jsonSchema<{ city: string }>({
            type: "object",
            properties: { city: { type: "string" } },
            required: ["city"],
            additionalProperties: false,
          }),
          execute: async (input) => ({
            temperature_c: 18,
            conditions: "sunny",
            city: input.city,
          }),
        }),
      },
      stopWhen: stepCountIs(2),
      experimental_telemetry: {
        isEnabled: true,
        functionId: "v5-generate-text-tool",
        metadata: {
          sessionId: "sess-weather",
          userId: "user-weather",
          feature: "forecast",
        },
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
