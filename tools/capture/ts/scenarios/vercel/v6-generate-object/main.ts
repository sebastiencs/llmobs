import { createOpenAI } from "@ai-sdk/openai-v6";
import { generateText, jsonSchema, Output } from "ai-v6";

import { finish, mockUrl, setup } from "../../../harness.ts";

const MODEL = "gpt-4o-mini-2024-07-18";
const ALIASES = {
  ai: "ai-v6",
  "@ai-sdk/openai": "@ai-sdk/openai-v6",
};

async function main(): Promise<void> {
  const provider = setup();
  try {
    const openai = createOpenAI({
      baseURL: mockUrl("/v1"),
      apiKey: process.env.OPENAI_API_KEY,
    });
    await generateText({
      model: openai.chat(MODEL),
      maxRetries: 0,
      output: Output.object({
        schema: jsonSchema<{ summary: string }>({
          type: "object",
          properties: { summary: { type: "string" } },
          required: ["summary"],
          additionalProperties: false,
        }),
      }),
      prompt: "Describe the harbor as JSON.",
      experimental_telemetry: {
        isEnabled: true,
        functionId: "v6-generate-object",
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
