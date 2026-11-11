import type { Schema } from "@/api/types";
import {
  FILTER_MAX,
  parseEnum,
  parseFlag,
  parseHex,
  parseRfc3339,
  parseString,
  parseViewId,
} from "@/lib/search/common";
import { PRESETS, type Preset } from "@/lib/time-range";

const SESSION_SORTS = ["start_time", "end_time", "trace_count", "cost_total"] as const;
const SORT_ORDERS = ["asc", "desc"] as const;

export const SESSION_COLUMN_IDS = [
  "session",
  "start",
  "last",
  "traces",
  "user",
  "duration",
  "tokens",
  "cost",
  "errors",
  "tags",
] as const;

export type SessionColumnId = (typeof SESSION_COLUMN_IDS)[number];

const SESSION_COLUMN_ID_SET: ReadonlySet<string> = new Set(SESSION_COLUMN_IDS);

export type SessionsSearch = {
  sort?: Schema<"SessionSort">;
  order?: Schema<"SortOrder">;
  range?: Preset;
  from?: string;
  to?: string;
  eval?: 1;
  exp?: 1;
  cols?: string;
  filter?: string;
  view?: string;
};

/** Search params of the session page. The list keeps its own URL. */
export type SessionPageSearch = {
  trace?: string;
  eval?: 1;
  exp?: 1;
};

declare module "@tanstack/react-router" {
  interface HistoryState {
    sessionsSearch?: SessionsSearch;
  }
}

/** Fixed column ids, plus `score:<name>` for a non-empty name. */
export function isSessionColumnId(id: string): boolean {
  return SESSION_COLUMN_ID_SET.has(id) || (id.startsWith("score:") && id.length > "score:".length);
}

export function parseSessionsSearch(search: Record<string, unknown>): SessionsSearch {
  const sort = parseEnum(search.sort, SESSION_SORTS);
  const order = parseEnum(search.order, SORT_ORDERS);
  const range = parseEnum(search.range, PRESETS);
  const from = parseRfc3339(search.from);
  const to = parseRfc3339(search.to);
  const absolute = from !== undefined && to !== undefined && Date.parse(from) < Date.parse(to);
  const evalOn = parseFlag(search.eval);
  const expOn = parseFlag(search.exp);
  const cols = parseCols(search.cols);
  const filter = parseString(search.filter, FILTER_MAX);
  const view = parseViewId(search.view);
  return {
    ...(sort ? { sort } : {}),
    ...(order ? { order } : {}),
    ...(range ? { range } : {}),
    ...(absolute ? { from, to } : {}),
    ...(evalOn ? { eval: 1 } : {}),
    ...(expOn ? { exp: 1 } : {}),
    ...(cols ? { cols } : {}),
    ...(filter ? { filter } : {}),
    ...(view ? { view } : {}),
  };
}

export function parseSessionPageSearch(search: Record<string, unknown>): SessionPageSearch {
  const trace = parseHex(search.trace, 32);
  const evalOn = parseFlag(search.eval);
  const expOn = parseFlag(search.exp);
  return {
    ...(trace ? { trace } : {}),
    ...(evalOn ? { eval: 1 } : {}),
    ...(expOn ? { exp: 1 } : {}),
  };
}

function parseCols(value: unknown): string | undefined {
  if (typeof value !== "string") {
    return undefined;
  }
  const ids: string[] = [];
  for (const part of value.split(",")) {
    const id = part.trim();
    if (!isSessionColumnId(id) || ids.includes(id)) {
      continue;
    }
    ids.push(id);
  }
  return ids.length > 0 ? ids.join(",") : undefined;
}
