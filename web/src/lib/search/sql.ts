import { parseEnum, parseRfc3339 } from "@/lib/search/common";
import { PRESETS, type Preset, type RangeState } from "@/lib/time-range";

export type SqlSearch = {
  range?: Preset;
  from?: string;
  to?: string;
};

export function parseSqlSearch(search: Record<string, unknown>): SqlSearch {
  const range = parseEnum(search.range, PRESETS);
  const from = parseRfc3339(search.from);
  const to = parseRfc3339(search.to);
  const absolute = from !== undefined && to !== undefined && Date.parse(from) < Date.parse(to);
  return {
    ...(range ? { range } : {}),
    ...(absolute ? { from, to } : {}),
  };
}

/** `search` with its range replaced by `state`. Other keys are kept. */
export function withSqlRange(search: SqlSearch, state: RangeState): SqlSearch {
  const { range: _range, from: _from, to: _to, ...rest } = search;
  if (state.kind === "preset") {
    return { ...rest, range: state.preset };
  }
  return { ...rest, from: state.from, to: state.to };
}
