import { expect, it } from "vitest";
import type { Schema } from "@/api/types";
import { channelKindLabel, conditionSummary, formatAlertValue } from "./alerts";

type Condition = Schema<"AlertCondition">;

const HIGH_ERROR_RATE: Condition = {
  metric: "error_rate",
  window: "5m",
  filter: "environment = 'production'",
  comparator: "gt",
  threshold: 0.05,
  min_count: 20,
};
const LOW_HELPFULNESS: Condition = {
  metric: "score",
  window: "1h",
  comparator: "lt",
  threshold: 0.7,
  min_count: 10,
  score_name: "helpfulness",
  aggregation: "avg",
};
const DAILY_COST: Condition = { metric: "cost", window: "24h", comparator: "gt", threshold: 50 };
const LLM_LATENCY: Condition = {
  metric: "latency_p95",
  window: "15m",
  target: "llm_span",
  comparator: "gte",
  threshold: 8000,
};
const PASS_RATE: Condition = {
  metric: "score",
  window: "1h",
  comparator: "lt",
  threshold: 0.9,
  score_name: "ok",
  aggregation: "pass_rate",
};

it("labels channel kinds", () => {
  expect(channelKindLabel("slack")).toBe("Slack");
  expect(channelKindLabel("webhook")).toBe("Webhook");
  expect(channelKindLabel("email")).toBe("Email");
});

it("formats a value in its metric's unit", () => {
  expect(formatAlertValue(HIGH_ERROR_RATE, 0.125)).toBe("12.5%");
  expect(formatAlertValue(LOW_HELPFULNESS, 0.7)).toBe("0.700");
  expect(formatAlertValue(DAILY_COST, 1.5)).toBe("$1.50");
  expect(formatAlertValue(LLM_LATENCY, 820.4)).toBe("820 ms");
  expect(formatAlertValue(PASS_RATE, 0.9)).toBe("90.0%");
});

it("shows a missing value as a dash", () => {
  expect(formatAlertValue(HIGH_ERROR_RATE, undefined)).toBe("—");
});

it("summarises an error rate condition", () => {
  expect(conditionSummary(HIGH_ERROR_RATE)).toBe("Error rate > 5.0% over 5m");
});

it("summarises a score condition", () => {
  expect(conditionSummary(LOW_HELPFULNESS)).toBe("Score helpfulness < 0.700 over 1h");
});

it("summarises a cost condition", () => {
  expect(conditionSummary(DAILY_COST)).toBe("Cost > $50.00 over 24h");
});

it("summarises an LLM span latency condition", () => {
  expect(conditionSummary(LLM_LATENCY)).toBe("p95 latency of LLM spans ≥ 8000 ms over 15m");
});

it("summarises a pass rate condition", () => {
  expect(conditionSummary(PASS_RATE)).toBe("Pass rate of ok < 90.0% over 1h");
});
