import type { Problem } from "@/api/problem";
import type { Schema } from "@/api/types";
import { isRecord } from "@/lib/sql/draft";

/** True for a JSON run answer. `run_sql` declares several media types, so narrow before use. */
export function isSqlResult(value: unknown): value is Schema<"SqlQueryResult"> {
  return (
    typeof value === "object" &&
    value !== null &&
    Array.isArray((value as { columns?: unknown }).columns) &&
    Array.isArray((value as { rows?: unknown }).rows) &&
    (value as { rows: unknown[] }).rows.every(isRecord) &&
    typeof (value as { truncated?: unknown }).truncated === "boolean"
  );
}

/** The message shown under the editor for a failed run. */
export function sqlProblemMessage(problem: Problem): string {
  const first = problem.errors?.[0];
  if (problem.code === "invalid_filter" && first) {
    return first.message;
  }
  return problem.detail ?? problem.title;
}

/** Shown when the failure is not a problem response (network error, empty or HTML body). */
export const RUN_FAILED = "The query could not be run";

/** `5 rows · 12 ms`, `1 row · 0 ms`, `10000 rows (truncated) · 3 ms`. */
export function resultStatus(count: number, truncated: boolean, durationMs: number): string {
  const rows = `${count} ${count === 1 ? "row" : "rows"}${truncated ? " (truncated)" : ""}`;
  return `${rows} · ${durationMs} ms`;
}

/** `⌘↵` on Apple platforms, `Ctrl↵` elsewhere, for the Run button's title. */
export function runShortcutLabel(userAgent: string): string {
  return /Mac|iPhone|iPad/.test(userAgent) ? "⌘↵" : "Ctrl↵";
}
