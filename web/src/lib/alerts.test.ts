import { expect, it } from "vitest";
import type { Schema } from "@/api/types";
import rulesDocument from "../../../fixtures/api/static/alert-rules.json";
import {
  channelKindLabel,
  conditionChanged,
  conditionFromForm,
  conditionSummary,
  DEFAULT_RULE_FORM,
  formatAlertValue,
  formFromRule,
  previewCondition,
  previewParams,
  previewSeries,
  type RuleFormValues,
  thresholdInPercent,
  thresholdUnit,
} from "./alerts";

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

const rules = rulesDocument.data as Schema<"AlertRule">[];

function formWith(patch: Partial<RuleFormValues>): RuleFormValues {
  return { ...DEFAULT_RULE_FORM, ...patch };
}

function conditionOf(form: RuleFormValues): Condition {
  const result = conditionFromForm(form);
  if (!result.ok) {
    throw new Error(`unexpected errors: ${JSON.stringify(result.errors)}`);
  }
  return result.condition;
}

function errorsOf(form: RuleFormValues): Record<string, string> {
  const result = conditionFromForm(form);
  if (result.ok) {
    throw new Error("expected errors");
  }
  return result.errors;
}

it("knows which metrics are typed in percent and their units", () => {
  expect(thresholdInPercent("error_rate", "avg")).toBe(true);
  expect(thresholdInPercent("score", "pass_rate")).toBe(true);
  expect(thresholdInPercent("score", "avg")).toBe(false);
  expect(thresholdInPercent("cost", "avg")).toBe(false);
  expect(thresholdUnit("error_rate", "avg")).toBe("%");
  expect(thresholdUnit("score", "pass_rate")).toBe("%");
  expect(thresholdUnit("cost", "avg")).toBe("USD");
  expect(thresholdUnit("latency_p95", "avg")).toBe("ms");
  expect(thresholdUnit("score", "avg")).toBe("");
});

it("builds the default error rate condition exactly", () => {
  const result = conditionFromForm(DEFAULT_RULE_FORM);
  expect(result.ok).toBe(true);
  if (!result.ok) {
    return;
  }
  expect(JSON.stringify(result.condition)).toBe(
    '{"metric":"error_rate","window":"5m","comparator":"gt","threshold":0.05,"min_count":20}',
  );
  expect(result.cooldownS).toBe(3600);
});

it("converts a typed percentage to a fraction without float noise", () => {
  expect(conditionOf(formWith({ threshold: "7" })).threshold).toBe(0.07);
});

it("types a stored error rate threshold as a percentage", () => {
  const rule = rules[1];
  if (!rule) {
    throw new Error("fixture rule missing");
  }
  const form = formFromRule({
    ...rule,
    condition: { ...rule.condition, metric: "error_rate", threshold: 0.07 },
  });
  expect(form.threshold).toBe("7");
});

it("rejects a blank threshold as not a number", () => {
  expect(errorsOf(formWith({ threshold: "  " }))).toEqual({
    "condition.threshold": "Enter a number.",
  });
});

it("accepts 0 and 100 as percentage bounds", () => {
  expect(conditionOf(formWith({ threshold: "0" })).threshold).toBe(0);
  expect(conditionOf(formWith({ threshold: "100" })).threshold).toBe(1);
});

it("omits the filter when it is blank and sends it trimmed otherwise", () => {
  expect(conditionOf(formWith({ filter: "   " })).filter).toBeUndefined();
  expect(conditionOf(formWith({ filter: " error_count > 0 " })).filter).toBe("error_count > 0");
});

it("sets only the fields of each metric", () => {
  expect(conditionOf(formWith({ metric: "cost", threshold: "50" }))).toEqual({
    metric: "cost",
    window: "5m",
    comparator: "gt",
    threshold: 50,
  });
  expect(
    conditionOf(formWith({ metric: "latency_p95", threshold: "8000", target: "llm_span" })),
  ).toEqual({
    metric: "latency_p95",
    window: "5m",
    comparator: "gt",
    threshold: 8000,
    min_count: 20,
    target: "llm_span",
  });
  expect(
    conditionOf(
      formWith({
        metric: "score",
        aggregation: "pass_rate",
        comparator: "lt",
        threshold: "90",
        minCount: "10",
        scoreName: "ok",
      }),
    ),
  ).toEqual({
    metric: "score",
    window: "5m",
    comparator: "lt",
    threshold: 0.9,
    min_count: 10,
    score_name: "ok",
    aggregation: "pass_rate",
  });
});

it("reports the threshold, minimum samples, score name and reminder errors", () => {
  expect(errorsOf(formWith({ threshold: "" }))).toEqual({
    "condition.threshold": "Enter a number.",
  });
  expect(errorsOf(formWith({ threshold: "abc" }))["condition.threshold"]).toBe("Enter a number.");
  expect(errorsOf(formWith({ threshold: "120" }))["condition.threshold"]).toBe(
    "Enter a percentage from 0 to 100.",
  );
  expect(errorsOf(formWith({ metric: "cost", threshold: "-1" }))["condition.threshold"]).toBe(
    "Enter 0 or more.",
  );
  expect(errorsOf(formWith({ metric: "latency_p95", threshold: "0" }))["condition.threshold"]).toBe(
    "Enter a duration above 0.",
  );
  expect(errorsOf(formWith({ minCount: "0" }))["condition.min_count"]).toBe(
    "Enter a whole number from 1 to 1,000,000.",
  );
  expect(errorsOf(formWith({ metric: "score", scoreName: "  " }))["condition.score_name"]).toBe(
    "Enter a score name.",
  );
  expect(errorsOf(formWith({ notifyWhileFiring: true, cooldownMinutes: "3" }))).toEqual({
    cooldown_s: "Enter 5 to 10,080 minutes.",
  });
});

it("sends the reminder interval in seconds when reminders are on", () => {
  const result = conditionFromForm(formWith({ notifyWhileFiring: true, cooldownMinutes: "120" }));
  expect(result.ok && result.cooldownS).toBe(7200);
});

it("reads a stored rule into the form", () => {
  const rule = rules[1];
  if (!rule) {
    throw new Error("fixture rule missing");
  }
  const form = formFromRule(rule);
  expect(form.threshold).toBe("5");
  expect(form.filter).toBe("environment = 'production'");
  expect(form.cooldownMinutes).toBe("60");
  expect(form.channelIds).toEqual(rule.channel_ids);
});

it("round-trips every stored rule through the form", () => {
  for (const rule of rules) {
    expect(conditionOf(formFromRule(rule))).toEqual(rule.condition);
    expect(conditionChanged(formFromRule(rule), formFromRule(rule))).toBe(false);
  }
});

it("sees a changed threshold as a changed condition", () => {
  const rule = rules[1];
  if (!rule) {
    throw new Error("fixture rule missing");
  }
  const base = formFromRule(rule);
  expect(conditionChanged(base, { ...base, name: "Renamed" })).toBe(false);
  expect(conditionChanged(base, { ...base, threshold: "10" })).toBe(true);
});

it("previews over buckets as wide as the window", () => {
  const now = Date.parse("2026-01-04T00:00:00Z");
  expect(previewParams("6h", now)).toEqual({
    bucket: "6h",
    from: "2026-01-01T00:00:00.000Z",
    to: "2026-01-04T00:00:00.000Z",
  });
  expect(previewParams("24h", now)).toEqual({
    bucket: "1d",
    from: "2025-12-21T00:00:00.000Z",
    to: "2026-01-04T00:00:00.000Z",
  });
  expect(previewParams("5m", now)).toEqual({
    bucket: "5m",
    from: "2026-01-03T00:00:00.000Z",
    to: "2026-01-04T00:00:00.000Z",
  });
  expect(previewParams("1m", now).bucket).toBe("1m");
  expect(previewParams("15m", now).bucket).toBe("15m");
  expect(previewParams("1h", now).bucket).toBe("1h");
});

it("previews a pass rate score as the average series of the score", () => {
  expect(
    previewSeries(formWith({ metric: "score", aggregation: "pass_rate", scoreName: " ok " })),
  ).toEqual({ metric: "score_avg", group: "ok" });
});

it("picks the series to preview", () => {
  expect(previewSeries({ metric: "error_rate", target: "trace", scoreName: "" })).toEqual({
    metric: "error_rate",
  });
  expect(previewSeries({ metric: "cost", target: "trace", scoreName: "" })).toEqual({
    metric: "cost",
  });
  expect(previewSeries({ metric: "latency_p95", target: "trace", scoreName: "" })).toEqual({
    metric: "latency_p95",
  });
  expect(previewSeries({ metric: "latency_p95", target: "llm_span", scoreName: "" })).toBe(
    undefined,
  );
  expect(previewSeries({ metric: "score", target: "trace", scoreName: "ok" })).toEqual({
    metric: "score_avg",
    group: "ok",
  });
});

it("previews a zero threshold while the form is invalid", () => {
  const condition = previewCondition(formWith({ threshold: "abc" }));
  expect(condition.threshold).toBe(0);
  expect(condition.metric).toBe("error_rate");
});
