import type { Schema } from "@/api/types";
import { isDateType, isNumericType, isTextType, isTimestampType } from "@/lib/sql/types";

export const CHART_MAX_POINTS = 1000;
export const CHART_MAX_SERIES = 5;
/** Key of the x value in each point; series keys are column names or pivot values. */
export const X_KEY = "__x";

export type ChartPoint = Record<string, string | number | null>;

export type ChartSpec = {
  kind: "line" | "bar";
  /** The x column and how its values read. */
  x: { name: string; kind: "timestamp" | "date" | "text" };
  /** Series keys, in order. */
  series: string[];
  /** Set when the series are the values of a text column and `measure` is the one numeric column. */
  pivot?: { column: string; measure: string };
  data: ChartPoint[];
  /** True when rows or series were dropped. */
  clipped: boolean;
};

function numberOf(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === "string" && value.trim().length > 0 && Number.isFinite(Number(value))) {
    return Number(value);
  }
  return null;
}

/** Milliseconds for sorting; unparseable values sort last. */
function timeOf(point: ChartPoint): number {
  const time = Date.parse(String(point[X_KEY]));
  return Number.isNaN(time) ? Number.POSITIVE_INFINITY : time;
}

function byTime(left: ChartPoint, right: ChartPoint): number {
  const a = timeOf(left);
  const b = timeOf(right);
  return a === b ? 0 : a < b ? -1 : 1;
}

/** Keys that would not round-trip as plain data properties. */
const UNSAFE_KEYS = new Set(["__proto__", X_KEY]);

/**
 * x: the first timestamp or date column (line chart), else the first text column (bar chart).
 * y: the numeric columns other than x, at most 5.
 * Pivot (spec 11): a line chart with exactly one numeric column and another text column gets
 * one series per value of the first such column, at most 5, first seen first.
 * Points: the first 1,000 rows; a line chart is sorted by time.
 * No x or no numeric column: null.
 */
export function chartSpec(result: Schema<"SqlQueryResult">): ChartSpec | null {
  const columns = result.columns;
  const time = columns.find((column) => isTimestampType(column.type) || isDateType(column.type));
  const text = columns.find((column) => isTextType(column.type));
  const xColumn = time ?? text;
  if (!xColumn) {
    return null;
  }
  const numericColumns = columns.filter(
    (column) => column.name !== xColumn.name && isNumericType(column.type),
  );
  const numeric = numericColumns.filter((column) => !UNSAFE_KEYS.has(column.name));
  const measure = numeric[0];
  if (!measure) {
    return null;
  }
  const xKind: ChartSpec["x"]["kind"] = time
    ? isDateType(xColumn.type)
      ? "date"
      : "timestamp"
    : "text";
  const x = { name: xColumn.name, kind: xKind };
  const kind = time ? "line" : "bar";
  const rows = (result.rows as Record<string, unknown>[]).slice(0, CHART_MAX_POINTS);
  let clipped = result.rows.length > CHART_MAX_POINTS || numericColumns.length > numeric.length;
  // The pivot needs exactly one numeric column among all of them, not among the safe ones.
  const pivotColumn =
    kind === "line" && numericColumns.length === 1
      ? columns.find((column) => column.name !== xColumn.name && isTextType(column.type))
      : undefined;
  if (pivotColumn) {
    const series: string[] = [];
    const byX = new Map<string, ChartPoint>();
    for (const row of rows) {
      const xValue = String(row[xColumn.name] ?? "");
      const raw = row[pivotColumn.name];
      const key = raw === null || raw === undefined ? "NULL" : String(raw);
      if (UNSAFE_KEYS.has(key)) {
        clipped = true;
        continue;
      }
      if (!series.includes(key)) {
        if (series.length >= CHART_MAX_SERIES) {
          clipped = true;
          continue;
        }
        series.push(key);
      }
      const point: ChartPoint = byX.get(xValue) ?? { [X_KEY]: xValue };
      point[key] = numberOf(row[measure.name]);
      byX.set(xValue, point);
    }
    const data = [...byX.values()].sort(byTime);
    const pivot = { column: pivotColumn.name, measure: measure.name };
    return { kind, x, series, pivot, data, clipped };
  }
  if (numeric.length > CHART_MAX_SERIES) {
    clipped = true;
  }
  const series = numeric.slice(0, CHART_MAX_SERIES).map((column) => column.name);
  const data = rows.map((row) => {
    const point: ChartPoint = { [X_KEY]: String(row[xColumn.name] ?? "") };
    for (const name of series) {
      point[name] = numberOf(row[name]);
    }
    return point;
  });
  if (kind === "line") {
    data.sort(byTime);
  }
  return { kind, x, series, data, clipped };
}

/** The chart's accessible name. */
export function chartLabel(spec: ChartSpec): string {
  const kind = spec.kind === "line" ? "Line chart" : "Bar chart";
  if (spec.pivot) {
    return `${kind} of ${spec.pivot.measure} by ${spec.x.name}, one series per ${spec.pivot.column}`;
  }
  return `${kind} of ${spec.series.join(", ")} by ${spec.x.name}`;
}
