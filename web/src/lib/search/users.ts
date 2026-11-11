import type { Schema } from "@/api/types";
import {
  FILTER_MAX,
  parseEnum,
  parseFlag,
  parseRfc3339,
  parseString,
  parseViewId,
} from "@/lib/search/common";
import { PRESETS, type Preset } from "@/lib/time-range";

const USER_SORTS = ["last_seen", "trace_count", "cost_total"] as const;
const SORT_ORDERS = ["asc", "desc"] as const;

export const USER_COLUMN_IDS = [
  "user",
  "traces",
  "sessions",
  "tokens",
  "cost",
  "errors",
  "first_seen",
  "last_seen",
] as const;

export type UserColumnId = (typeof USER_COLUMN_IDS)[number];

const USER_COLUMN_ID_SET: ReadonlySet<string> = new Set(USER_COLUMN_IDS);

export type UsersSearch = {
  sort?: Schema<"UserSort">;
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

/** Search params of the user page. The list keeps its own URL. */
export type UserPageSearch = {
  eval?: 1;
  exp?: 1;
};

export function isUserColumnId(id: string): boolean {
  return USER_COLUMN_ID_SET.has(id);
}

export function parseUsersSearch(search: Record<string, unknown>): UsersSearch {
  const sort = parseEnum(search.sort, USER_SORTS);
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

export function parseUserPageSearch(search: Record<string, unknown>): UserPageSearch {
  const evalOn = parseFlag(search.eval);
  const expOn = parseFlag(search.exp);
  return {
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
    if (!isUserColumnId(id) || ids.includes(id)) {
      continue;
    }
    ids.push(id);
  }
  return ids.length > 0 ? ids.join(",") : undefined;
}
