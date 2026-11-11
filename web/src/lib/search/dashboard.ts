import { FILTER_MAX, parseEnum, parseFlag, parseRfc3339, parseString } from "@/lib/search/common";
import { PRESETS, type Preset } from "@/lib/time-range";

const GROUPS = ["model", "environment", "release"] as const;

export type DashboardGroup = (typeof GROUPS)[number];

export type DashboardSearch = {
  range?: Preset;
  from?: string;
  to?: string;
  filter?: string;
  group?: DashboardGroup;
  eval?: 1;
  exp?: 1;
};

export function parseDashboardSearch(search: Record<string, unknown>): DashboardSearch {
  const range = parseEnum(search.range, PRESETS);
  const from = parseRfc3339(search.from);
  const to = parseRfc3339(search.to);
  const absolute = from !== undefined && to !== undefined && Date.parse(from) < Date.parse(to);
  const filter = parseString(search.filter, FILTER_MAX);
  const group = parseEnum(search.group, GROUPS);
  const evalOn = parseFlag(search.eval);
  const expOn = parseFlag(search.exp);
  return {
    ...(range ? { range } : {}),
    ...(absolute ? { from, to } : {}),
    ...(filter ? { filter } : {}),
    ...(group ? { group } : {}),
    ...(evalOn ? { eval: 1 } : {}),
    ...(expOn ? { exp: 1 } : {}),
  };
}
