import type { InfiniteData } from "@tanstack/react-query";
import type { Schema } from "@/api/types";

export type TracePages = InfiniteData<Schema<"TracePage">, string | null>;

export const LIVE_BUFFER_MAX = 500;
export const LIVE_PAGE_MAX = 500;

/** Newest first. Keeps at most `max` traces; `overflowed` is true when one was dropped. */
export function pushBuffered(
  buffer: readonly Schema<"TraceSummary">[],
  trace: Schema<"TraceSummary">,
  max: number,
): { buffer: Schema<"TraceSummary">[]; overflowed: boolean } {
  const next = [trace, ...buffer];
  return next.length > max
    ? { buffer: next.slice(0, max), overflowed: true }
    : { buffer: next, overflowed: false };
}

/** True when adding `adding` rows to the first page would make it longer than `max`. */
export function firstPageWouldExceed(data: TracePages, adding: number, max: number): boolean {
  return (data.pages[0]?.data.length ?? 0) + adding > max;
}

export function traceInList(data: TracePages, traceId: string): boolean {
  return data.pages.some((page) => page.data.some((row) => row.trace_id === traceId));
}

export function replaceLiveTrace(data: TracePages, trace: Schema<"TraceSummary">): TracePages {
  let changed = false;
  const pages = data.pages.map((page) => {
    const index = page.data.findIndex((row) => row.trace_id === trace.trace_id);
    if (index < 0) {
      return page;
    }
    changed = true;
    const rows = page.data.slice();
    rows[index] = trace;
    return { ...page, data: rows };
  });
  return changed ? { ...data, pages } : data;
}

/** `traces` is newest first. They land on the first page, ahead of what is already there. */
export function prependLiveTraces(
  data: TracePages,
  traces: readonly Schema<"TraceSummary">[],
): TracePages {
  const first = data.pages[0];
  if (!first || traces.length === 0) {
    return data;
  }
  return {
    ...data,
    pages: [{ ...first, data: [...traces, ...first.data] }, ...data.pages.slice(1)],
  };
}

/**
 * Live mode is start time, newest first. A new row belongs at the top when the
 * first page is empty or this trace is at least as new as the current first row.
 * RFC 3339 timestamps in the same shape compare as strings.
 */
export function belongsAtTop(data: TracePages, trace: Schema<"TraceSummary">): boolean {
  const first = data.pages[0]?.data[0];
  if (!first) {
    return true;
  }
  return trace.start_time >= first.start_time;
}

export function firstPageOnly(data: TracePages): TracePages {
  if (data.pages.length <= 1) {
    return data;
  }
  return {
    pages: data.pages.slice(0, 1),
    pageParams: data.pageParams.slice(0, 1),
  };
}
