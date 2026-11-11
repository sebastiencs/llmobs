import type { Schema } from "@/api/types";
import {
  parseEnum,
  parseFlag,
  parseHex,
  parseRfc3339,
  parseString,
  SEARCH_MAX,
} from "@/lib/search/common";
import { PRESETS, type Preset } from "@/lib/time-range";

const TRACE_SORTS = ["start_time", "duration", "cost_total", "total_tokens"] as const;
const SORT_ORDERS = ["asc", "desc"] as const;

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
};

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
  };
}
