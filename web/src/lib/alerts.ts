import type { Schema } from "@/api/types";

const KIND_LABELS: Record<Schema<"ChannelKind">, string> = {
  slack: "Slack",
  webhook: "Webhook",
  email: "Email",
};

/** "Slack", "Webhook" or "Email". */
export function channelKindLabel(kind: Schema<"ChannelKind">): string {
  return KIND_LABELS[kind];
}

type Condition = Schema<"AlertCondition">;

const COMPARATOR_SYMBOLS: Record<Schema<"Comparator">, string> = {
  gt: ">",
  gte: "≥",
  lt: "<",
  lte: "≤",
};

/** ">", "≥", "<", "≤" for gt, gte, lt, lte. */
export function comparatorSymbol(comparator: Schema<"Comparator">): string {
  return COMPARATOR_SYMBOLS[comparator];
}

function isPassRate(condition: Condition): boolean {
  return condition.metric === "score" && condition.aggregation === "pass_rate";
}

/** Metric label: "Error rate", "Cost", "p95 latency" (+ " of LLM spans" for llm_span), "Score <name>" or "Pass rate of <name>". */
export function metricLabel(condition: Condition): string {
  switch (condition.metric) {
    case "error_rate":
      return "Error rate";
    case "cost":
      return "Cost";
    case "latency_p95":
      return condition.target === "llm_span" ? "p95 latency of LLM spans" : "p95 latency";
    case "score":
      return isPassRate(condition)
        ? `Pass rate of ${condition.score_name ?? ""}`
        : `Score ${condition.score_name ?? ""}`;
  }
}

/** A value in the metric's unit: error_rate and pass_rate as percent with 1 decimal ("12.5%"),
 *  cost as "$" with 2 decimals, latency as "820 ms" (0 decimals), score avg with 3 decimals.
 *  `undefined` → "—". */
export function formatAlertValue(condition: Condition, value: number | undefined): string {
  if (value === undefined) {
    return "—";
  }
  switch (condition.metric) {
    case "error_rate":
      return `${(value * 100).toFixed(1)}%`;
    case "cost":
      return `$${value.toFixed(2)}`;
    case "latency_p95":
      return `${Math.round(value)} ms`;
    case "score":
      return isPassRate(condition) ? `${(value * 100).toFixed(1)}%` : value.toFixed(3);
  }
}

/** "Error rate > 5.0% over 5m" — label, comparator symbol, threshold via formatAlertValue, window. */
export function conditionSummary(condition: Condition): string {
  return `${metricLabel(condition)} ${comparatorSymbol(condition.comparator)} ${formatAlertValue(condition, condition.threshold)} over ${condition.window}`;
}
