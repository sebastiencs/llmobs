import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { Schema } from "@/api/types";
import { KindIcon } from "@/components/kind-icon";

const LABELS: Record<Schema<"SpanKind">, string> = {
  llm: "LLM",
  embedding: "Embedding",
  tool: "Tool",
  agent: "Agent",
  chain: "Chain",
  retriever: "Retriever",
  reranker: "Reranker",
  guardrail: "Guardrail",
  evaluator: "Evaluator",
  span: "Span",
};

describe("KindIcon", () => {
  it("labels every span kind", () => {
    for (const [kind, label] of Object.entries(LABELS)) {
      const { unmount } = render(<KindIcon kind={kind as Schema<"SpanKind">} />);
      expect(screen.getByLabelText(label)).toBeInTheDocument();
      unmount();
    }
  });
});
