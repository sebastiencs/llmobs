import { PRESETS, type Preset } from "@/lib/time-range";

/** Maximum length of a filter expression kept in the URL. */
export const FILTER_MAX = 4096;
/** Maximum length of a full-text search kept in the URL. */
export const SEARCH_MAX = 1024;

const RFC3339 =
  /^(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?(Z|[+-]\d{2}:\d{2})$/;

export function parseEnum<T extends string>(value: unknown, allowed: readonly T[]): T | undefined {
  if (typeof value !== "string") {
    return undefined;
  }
  return allowed.includes(value as T) ? (value as T) : undefined;
}

/** Lowercase hex of exactly `length` characters. Anything else is dropped. */
export function parseHex(value: unknown, length: number): string | undefined {
  if (typeof value !== "string" || value.length !== length) {
    return undefined;
  }
  return /^[0-9a-f]+$/.test(value) ? value : undefined;
}

/** `1` is on, whether the router parsed it as a number or left it as a string. */
export function parseFlag(value: unknown): true | undefined {
  return value === 1 || value === "1" ? true : undefined;
}

/** Trimmed text. Empty or longer than `max` is dropped. */
export function parseString(value: unknown, max: number): string | undefined {
  if (typeof value !== "string") {
    return undefined;
  }
  const trimmed = value.trim();
  if (trimmed.length === 0 || trimmed.length > max) {
    return undefined;
  }
  return trimmed;
}

export function parseRfc3339(value: unknown): string | undefined {
  if (typeof value !== "string" || !RFC3339.test(value)) {
    return undefined;
  }
  return Number.isNaN(Date.parse(value)) ? undefined : value;
}

const VIEW_ID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

const VIEW_SORTS = [
  "start_time",
  "end_time",
  "duration",
  "cost_total",
  "total_tokens",
  "trace_count",
  "last_seen",
] as const;

const COLS_MAX = 1024;

export type ViewSearch = {
  range?: Preset;
  from?: string;
  to?: string;
  filter?: string;
  search?: string;
  sort?: (typeof VIEW_SORTS)[number];
  order?: "asc" | "desc";
  eval?: 1;
  exp?: 1;
  cols?: string;
};

/** Saved-view id. Anything that is not a UUID is dropped. */
export function parseViewId(value: unknown): string | undefined {
  if (typeof value !== "string" || !VIEW_ID.test(value)) {
    return undefined;
  }
  return value;
}

/**
 * List-page search params stored on a saved view.
 * Selection (`trace`, `span`, `view`, `live`) is left out. Invalid keys are dropped.
 */
export function searchFromViewState(state: unknown): ViewSearch {
  if (typeof state !== "object" || state === null || Array.isArray(state)) {
    return {};
  }
  const record = state as Record<string, unknown>;
  const range = parseEnum(record.range, PRESETS);
  const from = parseRfc3339(record.from);
  const to = parseRfc3339(record.to);
  const absolute = from !== undefined && to !== undefined && Date.parse(from) < Date.parse(to);
  const filter = parseString(record.filter, FILTER_MAX);
  const text = parseString(record.search, SEARCH_MAX);
  const sort = parseEnum(record.sort, VIEW_SORTS);
  const order = parseEnum(record.order, ["asc", "desc"] as const);
  const cols = parseString(record.cols, COLS_MAX);
  return {
    ...(range ? { range } : {}),
    ...(absolute ? { from, to } : {}),
    ...(filter ? { filter } : {}),
    ...(text ? { search: text } : {}),
    ...(sort ? { sort } : {}),
    ...(order ? { order } : {}),
    ...(parseFlag(record.eval) ? { eval: 1 } : {}),
    ...(parseFlag(record.exp) ? { exp: 1 } : {}),
    ...(cols ? { cols } : {}),
  };
}

/** The current URL search, in the shape a saved view stores. */
export function viewStateOf(search: unknown): Record<string, string> {
  const parsed = searchFromViewState(search);
  const state: Record<string, string> = {};
  if (parsed.range) {
    state.range = parsed.range;
  }
  if (parsed.from) {
    state.from = parsed.from;
  }
  if (parsed.to) {
    state.to = parsed.to;
  }
  if (parsed.filter) {
    state.filter = parsed.filter;
  }
  if (parsed.search) {
    state.search = parsed.search;
  }
  if (parsed.sort) {
    state.sort = parsed.sort;
  }
  if (parsed.order) {
    state.order = parsed.order;
  }
  if (parsed.eval) {
    state.eval = "1";
  }
  if (parsed.exp) {
    state.exp = "1";
  }
  if (parsed.cols) {
    state.cols = parsed.cols;
  }
  return state;
}

export function sameViewState(
  left: Record<string, string>,
  right: Record<string, string>,
): boolean {
  const keys = new Set([...Object.keys(left), ...Object.keys(right)]);
  for (const key of keys) {
    if (left[key] !== right[key]) {
      return false;
    }
  }
  return true;
}
