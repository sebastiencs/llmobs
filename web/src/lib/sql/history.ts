import { isRecord } from "@/lib/sql/draft";
import { paramsKey } from "@/lib/sql/saved-state";
import { STORAGE_PREFIX } from "@/product";

export const HISTORY_MAX = 50;

export type HistoryEntry = {
  query: string;
  /** Typed strings of the parameters the query uses. */
  params: Record<string, string>;
  /** RFC 3339, when the run ended. */
  ranAt: string;
  /** Rows returned; absent when the run failed. */
  rows?: number;
};

export function historyKey(projectRef: string): string {
  return `${STORAGE_PREFIX}sql.history.${projectRef}`;
}

function entryOf(item: unknown): HistoryEntry[] {
  if (!isRecord(item) || typeof item.query !== "string" || typeof item.ranAt !== "string") {
    return [];
  }
  const params: Record<string, string> = {};
  if (isRecord(item.params)) {
    for (const [name, value] of Object.entries(item.params)) {
      if (typeof value === "string") {
        params[name] = value;
      }
    }
  }
  const rows = typeof item.rows === "number" ? { rows: item.rows } : {};
  return [{ query: item.query, params, ranAt: item.ranAt, ...rows }];
}

export function readHistory(projectRef: string): HistoryEntry[] {
  try {
    const raw = localStorage.getItem(historyKey(projectRef));
    const parsed: unknown = raw === null ? [] : JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.flatMap(entryOf) : [];
  } catch {
    return [];
  }
}

/** Newest first. A run equal to the newest entry replaces it. At most `HISTORY_MAX`. */
export function pushHistory(projectRef: string, entry: HistoryEntry): HistoryEntry[] {
  const current = readHistory(projectRef);
  const newest = current[0];
  const same =
    newest !== undefined &&
    newest.query === entry.query &&
    paramsKey(newest.params) === paramsKey(entry.params);
  const next = [entry, ...(same ? current.slice(1) : current)].slice(0, HISTORY_MAX);
  try {
    localStorage.setItem(historyKey(projectRef), JSON.stringify(next));
  } catch {
    // Private mode or a full quota: history is a convenience.
  }
  return next;
}

export function clearHistory(projectRef: string): void {
  try {
    localStorage.removeItem(historyKey(projectRef));
  } catch {
    // Nothing to clear.
  }
}

/** The first non-empty line, cut to 60 characters with an ellipsis. */
export function historyLabel(query: string): string {
  const line =
    query
      .split("\n")
      .find((item) => item.trim().length > 0)
      ?.trim() ?? "";
  return line.length > 60 ? `${line.slice(0, 59)}…` : line;
}
