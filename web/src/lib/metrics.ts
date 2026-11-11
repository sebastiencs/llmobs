import type { Schema } from "@/api/types";
import { formatCost, formatDuration, formatTokens } from "@/lib/format";

export type MetricName = Schema<"MetricName">;
export type MetricSeries = Schema<"MetricSeries">;
export type KpiMetric = Schema<"KpiMetric">;

export type ChartRow = { time: number } & Record<string, number | null>;

export type KpiDirection = "up" | "down" | "flat";

export type KpiDelta = {
  value?: number;
  previous?: number;
  changePct?: number;
  direction: KpiDirection;
};

const NS_PER_MS = 1_000_000;
const MAX_BUCKETS = 10_000;
const GROUP_LIMIT = 8;

type Point = Schema<"MetricPoint">;

/** Group the response series by metric. Order inside a metric is the response order. */
export function seriesByMetric(
  response: Pick<Schema<"MetricsResponse">, "series">,
): Map<MetricName, MetricSeries[]> {
  const grouped = new Map<MetricName, MetricSeries[]>();
  for (const series of response.series) {
    const list = grouped.get(series.metric);
    if (list) {
      list.push(series);
    } else {
      grouped.set(series.metric, [series]);
    }
  }
  return grouped;
}

/**
 * Keep the `limit` groups with the largest totals. The rest are summed into
 * one series named `other`. A time where every folded series lacks a value
 * stays a gap.
 */
export function foldGroups(series: readonly MetricSeries[], limit = GROUP_LIMIT): MetricSeries[] {
  if (series.length <= limit) {
    return [...series];
  }
  const ranked = [...series].sort((left, right) => {
    const delta = total(right) - total(left);
    if (delta !== 0) {
      return delta;
    }
    return (left.group ?? "").localeCompare(right.group ?? "");
  });
  const kept = ranked.slice(0, limit);
  const rest = ranked.slice(limit);
  const first = rest[0];
  if (!first) {
    return kept;
  }
  return [...kept, sumSeries(rest, first.metric, "other")];
}

/**
 * One row per bucket, one column per group (`all` when a series has no group).
 * A point with no value is `null`. Buckets between the earliest and latest
 * time that no series mentioned are filled in steps of `bucketNs`.
 */
export function toChartRows(series: readonly MetricSeries[], bucketNs: number): ChartRow[] {
  const times = new Set<number>();
  for (const item of series) {
    for (const point of item.points) {
      const time = epochMs(point.time);
      if (time !== undefined) {
        times.add(time);
      }
    }
  }
  if (times.size === 0) {
    return [];
  }
  const ordered = [...times].sort((left, right) => left - right);
  const start = ordered[0];
  const end = ordered[ordered.length - 1];
  const stepMs = Math.round(bucketNs / NS_PER_MS);
  if (start !== undefined && end !== undefined && stepMs > 0) {
    const count = Math.round((end - start) / stepMs);
    if (count > 0 && count <= MAX_BUCKETS) {
      for (let index = 0; index <= count; index += 1) {
        times.add(start + index * stepMs);
      }
    }
  }
  const columns = series.map((item) => ({
    key: item.group && item.group.length > 0 ? item.group : "all",
    byTime: pointMap(item.points),
  }));
  return [...times]
    .sort((left, right) => left - right)
    .map((time) => {
      const row: ChartRow = { time };
      for (const column of columns) {
        const value = column.byTime.get(time);
        row[column.key] = value === undefined ? null : value;
      }
      return row;
    });
}

/** Delta of a KPI against the previous window of the same length. */
export function kpiDelta(kpi: { value?: number; previous?: number }): KpiDelta {
  const { value, previous } = kpi;
  if (value === undefined || previous === undefined) {
    return { value, previous, direction: "flat" };
  }
  const direction: KpiDirection = value > previous ? "up" : value < previous ? "down" : "flat";
  const changePct = previous === 0 ? undefined : ((value - previous) / previous) * 100;
  return { value, previous, changePct, direction };
}

/** `[from, to)` for the bucket that starts at `timeMs`. Both ends are RFC 3339 UTC. */
export function bucketRange(timeMs: number, bucketNs: number): { from: string; to: string } {
  return {
    from: new Date(timeMs).toISOString(),
    to: new Date(timeMs + bucketNs / NS_PER_MS).toISOString(),
  };
}

export function formatKpi(metric: KpiMetric, value: number): string {
  switch (metric) {
    case "traces":
      return formatCount(value);
    case "error_rate":
      return formatPercent(value);
    case "latency_p95":
      return formatDuration(value);
    case "cost_total":
      return formatCost(value);
    case "total_tokens":
      return formatTokens(value);
  }
}

/** Format one chart point. `null` is a gap. */
export function formatMetric(metric: MetricName, value: number | null): string {
  if (value === null || Number.isNaN(value)) {
    return "—";
  }
  switch (metric) {
    case "error_rate":
      return formatPercent(value);
    case "latency_p50":
    case "latency_p95":
    case "latency_p99":
    case "ttft_p50":
    case "ttft_p95":
    case "llm_latency_p50":
    case "llm_latency_p95":
      return formatDuration(value);
    case "cost":
    case "cost_by_model":
    case "eval_cost":
      return formatCost(value);
    case "score_avg":
      return new Intl.NumberFormat("en-US", { maximumFractionDigits: 2 }).format(value);
    case "input_tokens":
    case "output_tokens":
    case "cache_read_tokens":
    case "cache_write_tokens":
      return formatTokens(value);
    default:
      return formatCount(value);
  }
}

export function formatPercent(rate: number): string {
  const percent = rate * 100;
  const rounded = Math.round(percent * 10) / 10;
  return Number.isInteger(rounded) ? `${rounded}%` : `${rounded.toFixed(1)}%`;
}

export function formatCount(value: number): string {
  return new Intl.NumberFormat("en-US", { maximumFractionDigits: 0 }).format(Math.round(value));
}

/** Sum of present points. `undefined` when every point is a gap. */
export function presentSum(series: readonly MetricSeries[]): number | undefined {
  let sum = 0;
  let any = false;
  for (const item of series) {
    for (const point of item.points) {
      if (point.value !== undefined) {
        sum += point.value;
        any = true;
      }
    }
  }
  return any ? sum : undefined;
}

function total(series: MetricSeries): number {
  return presentSum([series]) ?? 0;
}

function sumSeries(
  series: readonly MetricSeries[],
  metric: MetricName,
  group: string,
): MetricSeries {
  const times: string[] = [];
  const seen = new Set<string>();
  for (const item of series) {
    for (const point of item.points) {
      if (!seen.has(point.time)) {
        seen.add(point.time);
        times.push(point.time);
      }
    }
  }
  times.sort();
  const points: Point[] = times.map((time) => {
    let sum = 0;
    let any = false;
    for (const item of series) {
      const point = item.points.find((candidate) => candidate.time === time);
      if (point?.value !== undefined) {
        sum += point.value;
        any = true;
      }
    }
    return any ? { time, value: sum } : { time };
  });
  return { metric, group, points };
}

function pointMap(points: readonly Point[]): Map<number, number | undefined> {
  const byTime = new Map<number, number | undefined>();
  for (const point of points) {
    const time = epochMs(point.time);
    if (time !== undefined) {
      byTime.set(time, point.value);
    }
  }
  return byTime;
}

function epochMs(rfc3339: string): number | undefined {
  const parsed = Date.parse(rfc3339);
  if (!Number.isNaN(parsed)) {
    return parsed;
  }
  const trimmed = Date.parse(rfc3339.replace(/\.(\d{3})\d+/, ".$1"));
  return Number.isNaN(trimmed) ? undefined : trimmed;
}
