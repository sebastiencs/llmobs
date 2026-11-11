import type { Schema } from "@/api/types";
import { parseEnum, parseHex } from "@/lib/search/common";

const TRACE_SORTS = ["start_time", "duration", "cost_total", "total_tokens"] as const;
const SORT_ORDERS = ["asc", "desc"] as const;

export type TracesSearch = {
  sort?: Schema<"TraceSort">;
  order?: Schema<"SortOrder">;
  trace?: string;
  span?: string;
};

export function parseTracesSearch(search: Record<string, unknown>): TracesSearch {
  const sort = parseEnum(search.sort, TRACE_SORTS);
  const order = parseEnum(search.order, SORT_ORDERS);
  const trace = parseHex(search.trace, 32);
  const span = parseHex(search.span, 16);
  return {
    ...(sort ? { sort } : {}),
    ...(order ? { order } : {}),
    ...(trace ? { trace } : {}),
    ...(span ? { span } : {}),
  };
}
