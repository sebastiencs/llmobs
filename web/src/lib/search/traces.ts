import type { Schema } from "@/api/types";
import {
  FILTER_MAX,
  parseEnum,
  parseFlag,
  parseHex,
  parseRfc3339,
  parseString,
  parseViewId,
  SEARCH_MAX,
} from "@/lib/search/common";
import { PRESETS, type Preset } from "@/lib/time-range";

const TRACE_SORTS = ["start_time", "duration", "cost_total", "total_tokens"] as const;
const SORT_ORDERS = ["asc", "desc"] as const;
const TRACE_MODES = ["tree", "transcript"] as const;

export type TraceMode = (typeof TRACE_MODES)[number];

/** Search params of the full trace page. The list keeps its own URL. */
export type TracePageSearch = {
  span?: string;
  mode?: TraceMode;
};

export function parseTracePageSearch(search: Record<string, unknown>): TracePageSearch {
  const span = parseHex(search.span, 16);
  const mode = parseEnum(search.mode, TRACE_MODES);
  return {
    ...(span ? { span } : {}),
    ...(mode ? { mode } : {}),
  };
}

export const TRACE_COLUMN_IDS = [
  "status",
  "start",
  "name",
  "input",
  "output",
  "duration",
  "tokens",
  "cost",
  "models",
  "session",
  "user",
  "tags",
  "env",
  "release",
  "spans",
  "errors",
  "llm_calls",
  "trace_id",
] as const;

export type TraceColumnId = (typeof TRACE_COLUMN_IDS)[number];

const TRACE_COLUMN_ID_SET: ReadonlySet<string> = new Set(TRACE_COLUMN_IDS);

export type TracesSearch = {
  sort?: Schema<"TraceSort">;
  order?: Schema<"SortOrder">;
  trace?: string;
  span?: string;
  range?: Preset;
  from?: string;
  to?: string;
  search?: string;
  eval?: 1;
  exp?: 1;
  cols?: string;
  filter?: string;
  view?: string;
  mode?: TraceMode;
};

/** Fixed column ids, plus `score:<name>` for a non-empty name. */
export function isTraceColumnId(id: string): boolean {
  return TRACE_COLUMN_ID_SET.has(id) || (id.startsWith("score:") && id.length > "score:".length);
}

export function parseTracesSearch(search: Record<string, unknown>): TracesSearch {
  const sort = parseEnum(search.sort, TRACE_SORTS);
  const order = parseEnum(search.order, SORT_ORDERS);
  const trace = parseHex(search.trace, 32);
  const span = parseHex(search.span, 16);
  const range = parseEnum(search.range, PRESETS);
  const from = parseRfc3339(search.from);
  const to = parseRfc3339(search.to);
  const absolute = from !== undefined && to !== undefined && Date.parse(from) < Date.parse(to);
  const text = parseString(search.search, SEARCH_MAX);
  const evalOn = parseFlag(search.eval);
  const expOn = parseFlag(search.exp);
  const cols = parseCols(search.cols);
  const filter = parseString(search.filter, FILTER_MAX);
  const view = parseViewId(search.view);
  const mode = parseEnum(search.mode, TRACE_MODES);
  return {
    ...(sort ? { sort } : {}),
    ...(order ? { order } : {}),
    ...(trace ? { trace } : {}),
    ...(span ? { span } : {}),
    ...(range ? { range } : {}),
    ...(absolute ? { from, to } : {}),
    ...(text ? { search: text } : {}),
    ...(evalOn ? { eval: 1 } : {}),
    ...(expOn ? { exp: 1 } : {}),
    ...(cols ? { cols } : {}),
    ...(filter ? { filter } : {}),
    ...(view ? { view } : {}),
    ...(mode ? { mode } : {}),
  };
}

function parseCols(value: unknown): string | undefined {
  if (typeof value !== "string") {
    return undefined;
  }
  const ids: string[] = [];
  for (const part of value.split(",")) {
    const id = part.trim();
    if (!isTraceColumnId(id) || ids.includes(id)) {
      continue;
    }
    ids.push(id);
  }
  return ids.length > 0 ? ids.join(",") : undefined;
}
