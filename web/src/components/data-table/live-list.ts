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

/**
 * Live mode is start time, newest first, but traces arrive in the order they
 * finish. Each trace goes before the first loaded row that started earlier; one
 * older than every loaded row goes last only when no page is left to load, and
 * is left for paging otherwise. Returns the ids that were inserted.
 * RFC 3339 timestamps in the same shape compare as strings.
 */
export function insertLiveTraces(
  data: TracePages,
  traces: readonly Schema<"TraceSummary">[],
): { data: TracePages; inserted: string[] } {
  let pages = data.pages;
  const inserted: string[] = [];
  for (const trace of traces) {
    const next = insertOne(pages, trace);
    if (next) {
      pages = next;
      inserted.push(trace.trace_id);
    }
  }
  return { data: inserted.length > 0 ? { ...data, pages } : data, inserted };
}

function insertOne(
  pages: TracePages["pages"],
  trace: Schema<"TraceSummary">,
): TracePages["pages"] | undefined {
  for (const [pageIndex, page] of pages.entries()) {
    const index = page.data.findIndex((row) => row.start_time < trace.start_time);
    if (index >= 0) {
      return withRow(pages, pageIndex, index, trace);
    }
  }
  const lastIndex = pages.length - 1;
  const last = pages[lastIndex];
  if (!last || last.next_cursor !== null) {
    return undefined;
  }
  return withRow(pages, lastIndex, last.data.length, trace);
}

function withRow(
  pages: TracePages["pages"],
  pageIndex: number,
  index: number,
  trace: Schema<"TraceSummary">,
): TracePages["pages"] {
  return pages.map((page, at) =>
    at === pageIndex
      ? { ...page, data: [...page.data.slice(0, index), trace, ...page.data.slice(index)] }
      : page,
  );
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
