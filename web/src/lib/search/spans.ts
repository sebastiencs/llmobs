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

const SPAN_SORTS = ["start_time", "duration", "cost_total", "total_tokens"] as const;
const SORT_ORDERS = ["asc", "desc"] as const;

export const SPAN_COLUMN_IDS = [
  "status",
  "start",
  "kind",
  "name",
  "model",
  "input",
  "output",
  "duration",
  "ttft",
  "tokens",
  "cost",
  "session",
  "user",
  "env",
  "provider",
  "service",
  "release",
  "tags",
  "trace_id",
  "span_id",
  "parent_span_id",
] as const;

export type SpanColumnId = (typeof SPAN_COLUMN_IDS)[number];

const SPAN_COLUMN_ID_SET: ReadonlySet<string> = new Set(SPAN_COLUMN_IDS);

export type SpansSearch = {
  sort?: Schema<"SpanSort">;
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
};

export function isSpanColumnId(id: string): id is SpanColumnId {
  return SPAN_COLUMN_ID_SET.has(id);
}

export function parseSpansSearch(search: Record<string, unknown>): SpansSearch {
  const sort = parseEnum(search.sort, SPAN_SORTS);
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
  };
}

function parseCols(value: unknown): string | undefined {
  if (typeof value !== "string") {
    return undefined;
  }
  const ids: string[] = [];
  for (const part of value.split(",")) {
    const id = part.trim();
    if (!isSpanColumnId(id) || ids.includes(id)) {
      continue;
    }
    ids.push(id);
  }
  return ids.length > 0 ? ids.join(",") : undefined;
}
