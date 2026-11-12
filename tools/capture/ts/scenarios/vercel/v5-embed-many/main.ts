import { createOpenAI } from "@ai-sdk/openai-v5";
import { embedMany } from "ai-v5";

import { finish, mockUrl, setup } from "../../../harness.ts";

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
    await embedMany({
      model: openai.embedding("text-embedding-3-small"),
      values: ["alpha", "beta"],
      maxRetries: 0,
      providerOptions: { openai: { dimensions: 8 } },
      experimental_telemetry: {
        isEnabled: true,
        functionId: "v5-embed-many",
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
