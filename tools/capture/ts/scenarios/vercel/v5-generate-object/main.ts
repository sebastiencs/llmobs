import { createOpenAI } from "@ai-sdk/openai-v5";
import { generateObject, jsonSchema } from "ai-v5";

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
    await generateObject({
      model: openai.chat(MODEL),
      maxRetries: 0,
      schema: jsonSchema<{ summary: string }>({
        type: "object",
        properties: { summary: { type: "string" } },
        required: ["summary"],
        additionalProperties: false,
      }),
      prompt: "Describe the harbor as JSON.",
      experimental_telemetry: {
        isEnabled: true,
        functionId: "v5-generate-object",
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
