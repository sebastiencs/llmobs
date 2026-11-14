import type { Schema } from "@/api/types";
import { parseEnum, parseRfc3339 } from "@/lib/search/common";
import type { SqlSearch } from "@/lib/search/sql";
import { isRecord, type SqlDraft } from "@/lib/sql/draft";
import { usedParams } from "@/lib/sql/params";
import { PRESETS, type Preset } from "@/lib/time-range";

/** The picker's default when the URL has no range (`rangeFromSearch`). */
const DEFAULT_PRESET: Preset = "24h";

/** A `sql` saved view's state (D-H-7): a range or an absolute pair, never both. */
export type SqlSavedState = {
  query: string;
  params: Record<string, string>;
  range?: Preset;
  from?: string;
  to?: string;
};

/** What "Save" stores: the query, the parameters it uses, and the effective range. */
export function savedStateOf(draft: SqlDraft, search: SqlSearch): SqlSavedState {
  const base = { query: draft.query, params: usedParams(draft.query, draft.params) };
  if (search.from && search.to) {
    return { ...base, from: search.from, to: search.to };
  }
  return { ...base, range: search.range ?? DEFAULT_PRESET };
}

/** Reads a stored state leniently. Number and boolean params become strings. */
export function parseSavedState(state: unknown): SqlSavedState {
  const record = isRecord(state) ? state : {};
  const params: Record<string, string> = {};
  if (isRecord(record.params)) {
    for (const [name, value] of Object.entries(record.params)) {
      if (typeof value === "string") {
        params[name] = value;
      } else if (typeof value === "number" || typeof value === "boolean") {
        params[name] = String(value);
      }
    }
  }
  const query = typeof record.query === "string" ? record.query : "";
  const range = parseEnum(record.range, PRESETS);
  const from = parseRfc3339(record.from);
  const to = parseRfc3339(record.to);
  if (from !== undefined && to !== undefined && Date.parse(from) < Date.parse(to)) {
    return { query, params, from, to };
  }
  return range ? { query, params, range } : { query, params };
}

/** A stable key for a params map, independent of key order. */
export function paramsKey(params: Record<string, string>): string {
  return JSON.stringify(
    Object.keys(params)
      .sort()
      .map((name) => [name, params[name]]),
  );
}

/**
 * Whether `current` (from `savedStateOf`) matches `saved`. The params compare after
 * normalising `saved` to the ones its query uses, so a stored state with extra unused
 * keys or a missing used one is not "modified" right after loading. The range counts
 * only when `saved` stores one: an older query saved without a range matches under any
 * range.
 */
export function sameSavedState(current: SqlSavedState, saved: SqlSavedState): boolean {
  if (
    current.query !== saved.query ||
    paramsKey(current.params) !== paramsKey(usedParams(saved.query, saved.params))
  ) {
    return false;
  }
  if (!saved.range && !saved.from) {
    return true;
  }
  return current.range === saved.range && current.from === saved.from && current.to === saved.to;
}

/** The search a loaded state opens with. Without a stored range, the current one stays. */
export function searchOfSavedState(
  state: SqlSavedState,
  current: SqlSearch,
  viewId: string,
): SqlSearch {
  if (state.from && state.to) {
    return { from: state.from, to: state.to, view: viewId };
  }
  if (state.range) {
    return { range: state.range, view: viewId };
  }
  const { view: _view, ...rest } = current;
  return { ...rest, view: viewId };
}

/**
 * The state as the API's opaque object. `SqlSavedState` has no index signature, so a
 * direct cast is TS2352; going through `unknown` is the intended escape (the server only
 * checks that it is an object, D-H-7).
 */
export function storedState(state: SqlSavedState): Schema<"CreateSavedView">["state"] {
  return state as unknown as Schema<"CreateSavedView">["state"];
}
