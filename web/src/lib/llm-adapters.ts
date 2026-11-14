import type { Schema } from "@/api/types";

/** LLM provider labels, shared by the connections table and the connection dialog (I1.7). */
export const LLM_ADAPTER_LABELS: Record<Schema<"LlmAdapter">, string> = {
  openai: "OpenAI-compatible",
  anthropic: "Anthropic",
  gemini: "Gemini",
};
