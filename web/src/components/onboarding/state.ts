import type { RangeState } from "@/lib/time-range";

/** An empty list with no filter, search, custom range, or eval/experiment toggle. */
export function tracesNeedOnboarding(
  search: { filter?: string; search?: string; eval?: 1; exp?: 1 },
  range: RangeState,
): boolean {
  return (
    search.filter === undefined &&
    search.search === undefined &&
    search.eval === undefined &&
    search.exp === undefined &&
    range.kind === "preset"
  );
}
