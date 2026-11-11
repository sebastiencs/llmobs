import {
  Binary,
  Bot,
  Circle,
  Gauge,
  Link,
  ListOrdered,
  type LucideIcon,
  Search,
  ShieldCheck,
  Sparkles,
  Wrench,
} from "lucide-react";
import type { Schema } from "@/api/types";

const KINDS = {
  llm: { icon: Sparkles, label: "LLM", className: "text-kind-llm" },
  embedding: { icon: Binary, label: "Embedding", className: "text-kind-embedding" },
  tool: { icon: Wrench, label: "Tool", className: "text-kind-tool" },
  agent: { icon: Bot, label: "Agent", className: "text-kind-agent" },
  chain: { icon: Link, label: "Chain", className: "text-kind-chain" },
  retriever: { icon: Search, label: "Retriever", className: "text-kind-retriever" },
  reranker: { icon: ListOrdered, label: "Reranker", className: "text-kind-reranker" },
  guardrail: { icon: ShieldCheck, label: "Guardrail", className: "text-kind-guardrail" },
  evaluator: { icon: Gauge, label: "Evaluator", className: "text-kind-evaluator" },
  span: { icon: Circle, label: "Span", className: "text-kind-span" },
} as const satisfies Record<
  Schema<"SpanKind">,
  { icon: LucideIcon; label: string; className: string }
>;

export function KindIcon({ kind }: { kind: Schema<"SpanKind"> }) {
  const entry = KINDS[kind] ?? { icon: Circle, label: kind, className: "text-kind-span" };
  const Icon = entry.icon;
  return <Icon aria-label={entry.label} className={`size-3.5 ${entry.className}`} />;
}
