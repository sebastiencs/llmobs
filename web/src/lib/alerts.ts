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

type Metric = Schema<"AlertMetric">;
type Aggregation = Schema<"ScoreAggregation">;
type Comparator = Schema<"Comparator">;
type Window = Schema<"AlertWindow">;

/** The editable form. Numbers are strings while typed. Rates are typed in percent. */
export type RuleFormValues = {
  name: string;
  metric: Metric;
  window: Window;
  filter: string;
  comparator: Comparator;
  /** Percent for error_rate and pass_rate, USD for cost, ms for latency, raw for score avg. */
  threshold: string;
  minCount: string;
  target: Schema<"LatencyTarget">;
  scoreName: string;
  aggregation: Aggregation;
  channelIds: string[];
  enabled: boolean;
  /** Minutes, 5–10080. */
  cooldownMinutes: string;
  notifyWhileFiring: boolean;
};

export const DEFAULT_RULE_FORM: RuleFormValues = {
  name: "",
  metric: "error_rate",
  window: "5m",
  filter: "",
  comparator: "gt",
  threshold: "5",
  minCount: "20",
  target: "trace",
  scoreName: "",
  aggregation: "avg",
  channelIds: [],
  enabled: true,
  cooldownMinutes: "60",
  notifyWhileFiring: false,
};

/** Whether the threshold is typed in percent. */
export function thresholdInPercent(metric: Metric, aggregation: Aggregation): boolean {
  return metric === "error_rate" || (metric === "score" && aggregation === "pass_rate");
}

/** The unit shown after the threshold input: "%", "USD", "ms" or "" (score avg). */
export function thresholdUnit(metric: Metric, aggregation: Aggregation): string {
  switch (metric) {
    case "error_rate":
      return "%";
    case "cost":
      return "USD";
    case "latency_p95":
      return "ms";
    case "score":
      return thresholdInPercent(metric, aggregation) ? "%" : "";
  }
}

/** The values a metric starts with when it is chosen (the form's reset on a metric change). */
export function metricDefaults(
  metric: Metric,
  aggregation: Aggregation,
): { threshold: string; comparator: Comparator; minCount: string } {
  switch (metric) {
    case "error_rate":
      return { threshold: "5", comparator: "gt", minCount: "20" };
    case "cost":
      return { threshold: "100", comparator: "gt", minCount: "20" };
    case "latency_p95":
      return { threshold: "5000", comparator: "gt", minCount: "20" };
    case "score":
      return aggregation === "pass_rate"
        ? { threshold: "90", comparator: "lt", minCount: "10" }
        : { threshold: "0.5", comparator: "lt", minCount: "10" };
  }
}

/** Form values from a stored rule (edit). Percent thresholds are multiplied by 100 and printed
 *  with `String(Math.round(value * 1e6) / 1e6)`; `cooldownMinutes` is `String(Math.round(cooldown_s / 60))`.
 *  Fields the metric does not use take the defaults of DEFAULT_RULE_FORM (`target` "trace",
 *  `scoreName` "", `aggregation` "avg", `minCount` "20" for cost); `filter` is the stored filter or "". */
export function formFromRule(rule: Schema<"AlertRule">): RuleFormValues {
  const condition = rule.condition;
  const aggregation = condition.aggregation ?? DEFAULT_RULE_FORM.aggregation;
  const threshold = thresholdInPercent(condition.metric, aggregation)
    ? String(Math.round(condition.threshold * 100 * 1e6) / 1e6)
    : String(condition.threshold);
  return {
    name: rule.name,
    metric: condition.metric,
    window: condition.window,
    filter: condition.filter ?? "",
    comparator: condition.comparator,
    threshold,
    minCount:
      condition.metric === "cost"
        ? DEFAULT_RULE_FORM.minCount
        : String(condition.min_count ?? DEFAULT_RULE_FORM.minCount),
    target: condition.target ?? DEFAULT_RULE_FORM.target,
    scoreName: condition.score_name ?? DEFAULT_RULE_FORM.scoreName,
    aggregation,
    channelIds: [...rule.channel_ids],
    enabled: rule.enabled,
    cooldownMinutes: String(Math.round(rule.cooldown_s / 60)),
    notifyWhileFiring: rule.notify_while_firing,
  };
}

/** The threshold in the unit the API stores: a percent becomes a fraction. */
function storedThreshold(form: RuleFormValues, value: number): number {
  return thresholdInPercent(form.metric, form.aggregation)
    ? Number((value / 100).toPrecision(12))
    : value;
}

function buildCondition(form: RuleFormValues, threshold: number, minCount: number): Condition {
  const condition: Condition = {
    metric: form.metric,
    window: form.window,
    comparator: form.comparator,
    threshold,
  };
  const filter = form.filter.trim();
  if (filter) {
    condition.filter = filter;
  }
  if (form.metric !== "cost") {
    condition.min_count = minCount;
  }
  if (form.metric === "latency_p95") {
    condition.target = form.target;
  }
  if (form.metric === "score") {
    condition.score_name = form.scoreName.trim();
    condition.aggregation = form.aggregation;
  }
  return condition;
}

/** The condition to send, or field errors keyed like the server's (`condition.threshold`,
 *  `condition.min_count`, `condition.score_name`, `cooldown_s`). Only the fields of the metric are
 *  set: `min_count` not for cost, `target` only for latency, `score_name` and `aggregation` only for score.
 *  An empty filter is omitted. */
export function conditionFromForm(
  form: RuleFormValues,
):
  | { ok: true; condition: Condition; cooldownS: number }
  | { ok: false; errors: Record<string, string> } {
  const errors: Record<string, string> = {};
  const raw = form.threshold.trim();
  const number = Number(raw);
  const percent = thresholdInPercent(form.metric, form.aggregation);
  let threshold = 0;
  if (raw === "" || !Number.isFinite(number)) {
    errors["condition.threshold"] = "Enter a number.";
  } else if (percent && (number < 0 || number > 100)) {
    errors["condition.threshold"] = "Enter a percentage from 0 to 100.";
  } else if (form.metric === "cost" && number < 0) {
    errors["condition.threshold"] = "Enter 0 or more.";
  } else if (form.metric === "latency_p95" && number <= 0) {
    errors["condition.threshold"] = "Enter a duration above 0.";
  } else {
    threshold = storedThreshold(form, number);
  }

  let minCount = 0;
  if (form.metric !== "cost") {
    const text = form.minCount.trim();
    const parsed = Number(text);
    if (text !== "" && Number.isInteger(parsed) && parsed >= 1 && parsed <= 1_000_000) {
      minCount = parsed;
    } else {
      errors["condition.min_count"] = "Enter a whole number from 1 to 1,000,000.";
    }
  }

  if (form.metric === "score" && form.scoreName.trim() === "") {
    errors["condition.score_name"] = "Enter a score name.";
  }

  let cooldownS = 3600;
  if (form.notifyWhileFiring) {
    const text = form.cooldownMinutes.trim();
    const minutes = Number(text);
    if (text !== "" && Number.isInteger(minutes) && minutes >= 5 && minutes <= 10_080) {
      cooldownS = minutes * 60;
    } else {
      errors.cooldown_s = "Enter 5 to 10,080 minutes.";
    }
  }

  if (Object.keys(errors).length > 0) {
    return { ok: false, errors };
  }
  return { ok: true, condition: buildCondition(form, threshold, minCount), cooldownS };
}

/** A full condition for labels and axes, also while the form is invalid: an unparsable
 *  threshold becomes 0, a blank score name stays blank. Never sent. */
export function previewCondition(form: RuleFormValues): Condition {
  const raw = form.threshold.trim();
  const number = Number(raw);
  const threshold = raw === "" || !Number.isFinite(number) ? 0 : storedThreshold(form, number);
  const minCount = Number.parseInt(form.minCount, 10);
  return buildCondition(form, threshold, Number.isFinite(minCount) ? minCount : 0);
}

/** The condition-related form fields of `a` and `b` differ (metric, window, trimmed filter,
 *  comparator, threshold, minCount, target, trimmed scoreName, aggregation, compared as
 *  strings after trimming). Used to decide whether a PATCH sends `condition`. */
export function conditionChanged(a: RuleFormValues, b: RuleFormValues): boolean {
  return (
    a.metric !== b.metric ||
    a.window !== b.window ||
    a.filter.trim() !== b.filter.trim() ||
    a.comparator !== b.comparator ||
    a.threshold.trim() !== b.threshold.trim() ||
    a.minCount.trim() !== b.minCount.trim() ||
    a.target !== b.target ||
    a.scoreName.trim() !== b.scoreName.trim() ||
    a.aggregation !== b.aggregation
  );
}

const DAY_MS = 24 * 60 * 60 * 1000;

/** D-K-13: the bucket equal to the window, and the preview range. */
export function previewParams(
  window: Window,
  nowMs: number,
): { bucket: Schema<"MetricsBucket">; from: string; to: string } {
  const plan: Record<Window, { bucket: Schema<"MetricsBucket">; spanMs: number }> = {
    "1m": { bucket: "1m", spanMs: DAY_MS },
    "5m": { bucket: "5m", spanMs: DAY_MS },
    "15m": { bucket: "15m", spanMs: DAY_MS },
    "1h": { bucket: "1h", spanMs: DAY_MS },
    "6h": { bucket: "6h", spanMs: 3 * DAY_MS },
    "24h": { bucket: "1d", spanMs: 14 * DAY_MS },
  };
  const { bucket, spanMs } = plan[window];
  return {
    bucket,
    from: new Date(nowMs - spanMs).toISOString(),
    to: new Date(nowMs).toISOString(),
  };
}

/** Which series to draw: error_rate → "error_rate"; cost → "cost"; latency_p95 with target trace →
 *  "latency_p95" (ns, shown in ms); score → "score_avg" with group = the score name;
 *  latency_p95 with target llm_span → undefined (no preview). */
export function previewSeries(
  condition: Pick<RuleFormValues, "metric" | "target" | "scoreName">,
): { metric: Schema<"MetricName">; group?: string } | undefined {
  switch (condition.metric) {
    case "error_rate":
      return { metric: "error_rate" };
    case "cost":
      return { metric: "cost" };
    case "latency_p95":
      return condition.target === "trace" ? { metric: "latency_p95" } : undefined;
    case "score":
      return { metric: "score_avg", group: condition.scoreName.trim() };
  }
}
