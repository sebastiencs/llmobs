import { type QueryKey, useQueryClient } from "@tanstack/react-query";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import type { Schema } from "@/api/types";
import { ROW_HEIGHT } from "@/components/data-table/data-table";
import {
  belongsAtTop,
  firstPageOnly,
  firstPageWouldExceed,
  LIVE_BUFFER_MAX,
  LIVE_PAGE_MAX,
  prependLiveTraces,
  pushBuffered,
  replaceLiveTrace,
  type TracePages,
  traceInList,
} from "@/components/data-table/live-list";
import { type StreamEvent, type StreamStatus, useStream } from "@/lib/sse";

const HIGHLIGHT_MS = 1_500;

let limits = { buffer: LIVE_BUFFER_MAX, page: LIVE_PAGE_MAX };

/** Tests only: lower the live caps. Restore with `setLiveLimits({ buffer: LIVE_BUFFER_MAX, page: LIVE_PAGE_MAX })`. */
export function setLiveLimits(next: { buffer: number; page: number }): void {
  limits = next;
}

export function useLiveTraces({
  url,
  queryKey,
  selectedId,
  ready,
}: {
  url: string | undefined;
  queryKey: QueryKey;
  selectedId?: string;
  /** The traces grid is mounted, so its scroll container can be read. */
  ready: boolean;
}): {
  status: StreamStatus;
  pending: number;
  highlighted: ReadonlySet<string>;
  reveal: () => void;
  overflowed: boolean;
} {
  const queryClient = useQueryClient();
  const queryKeyRef = useRef(queryKey);
  queryKeyRef.current = queryKey;
  const selectedRef = useRef(selectedId);
  selectedRef.current = selectedId;
  const bufferRef = useRef<Schema<"TraceSummary">[]>([]);
  const [pending, setPending] = useState(0);
  const overflowRef = useRef(false);
  const [overflowed, setOverflowed] = useState(false);
  const [highlighted, setHighlighted] = useState<ReadonlySet<string>>(() => new Set());
  const timers = useRef<number[]>([]);
  const pinTop = useRef(false);
  const scrollAdjust = useRef(0);

  useEffect(() => {
    return () => {
      for (const timer of timers.current) {
        window.clearTimeout(timer);
      }
    };
  }, []);

  function mark(ids: readonly string[]) {
    if (ids.length === 0) {
      return;
    }
    setHighlighted((current) => {
      const next = new Set(current);
      for (const id of ids) {
        next.add(id);
      }
      return next;
    });
    const timer = window.setTimeout(() => {
      setHighlighted((current) => {
        const next = new Set(current);
        for (const id of ids) {
          next.delete(id);
        }
        return next;
      });
      timers.current = timers.current.filter((id) => id !== timer);
    }, HIGHLIGHT_MS);
    timers.current.push(timer);
  }

  function writeBuffer(next: Schema<"TraceSummary">[]) {
    bufferRef.current = next;
    setPending(next.length);
  }

  /** Drops every page after the first and refetches page 1 from the server, with a correct cursor. */
  function refetchFirstPage() {
    const key = queryKeyRef.current;
    queryClient.setQueryData<TracePages>(key, (current) =>
      current ? firstPageOnly(current) : current,
    );
    void queryClient.invalidateQueries({ queryKey: key, exact: true });
  }

  function insertBuffered(rows: readonly Schema<"TraceSummary">[]) {
    const current = queryClient.getQueryData<TracePages>(queryKeyRef.current);
    if (!current) {
      return;
    }
    const unseen = rows.filter((trace) => !traceInList(current, trace.trace_id));
    if (firstPageWouldExceed(current, unseen.length, limits.page)) {
      refetchFirstPage();
      mark(unseen.map((trace) => trace.trace_id));
      return;
    }
    queryClient.setQueryData<TracePages>(queryKeyRef.current, (data) => {
      if (!data) {
        return data;
      }
      let next = data;
      for (const trace of rows) {
        if (traceInList(next, trace.trace_id)) {
          next = replaceLiveTrace(next, trace);
        }
      }
      const pending = rows.filter((trace) => !traceInList(data, trace.trace_id));
      return pending.length > 0 ? prependLiveTraces(next, pending) : next;
    });
    mark(unseen.map((trace) => trace.trace_id));
  }

  function reveal() {
    const rows = bufferRef.current;
    if (rows.length === 0) {
      return;
    }
    writeBuffer([]);
    pinTop.current = true;
    scrollAdjust.current = 0;
    if (overflowRef.current) {
      overflowRef.current = false;
      setOverflowed(false);
      refetchFirstPage();
      mark(rows.map((trace) => trace.trace_id));
      return;
    }
    insertBuffered(rows);
  }

  const revealRef = useRef(reveal);
  revealRef.current = reveal;

  function onEvent(event: StreamEvent) {
    if (event.type === "resync") {
      refetchFirstPage();
      return;
    }
    if (event.type !== "trace") {
      return;
    }
    const trace = event.trace;
    const key = queryKeyRef.current;
    const current = queryClient.getQueryData<TracePages>(key);
    if (current && traceInList(current, trace.trace_id)) {
      queryClient.setQueryData<TracePages>(key, (data) =>
        data ? replaceLiveTrace(data, trace) : data,
      );
      return;
    }
    const buffered = bufferRef.current.findIndex((row) => row.trace_id === trace.trace_id);
    if (buffered >= 0) {
      const next = bufferRef.current.slice();
      next[buffered] = trace;
      bufferRef.current = next;
      return;
    }
    const node = tracesScroller();
    const atTop = (node?.scrollTop ?? 0) <= 0;
    if (!atTop) {
      const pushed = pushBuffered(bufferRef.current, trace, limits.buffer);
      if (pushed.overflowed && !overflowRef.current) {
        overflowRef.current = true;
        setOverflowed(true);
      }
      writeBuffer(pushed.buffer);
      return;
    }
    if (!current || !belongsAtTop(current, trace)) {
      return;
    }
    if (firstPageWouldExceed(current, 1, limits.page)) {
      refetchFirstPage();
      mark([trace.trace_id]);
      return;
    }
    queryClient.setQueryData<TracePages>(key, (data) =>
      data ? prependLiveTraces(data, [trace]) : data,
    );
    mark([trace.trace_id]);
    if (selectedRef.current) {
      scrollAdjust.current += ROW_HEIGHT;
    }
  }

  const { status } = useStream(url, onEvent);

  useEffect(() => {
    if (!ready) {
      return;
    }
    const node = tracesScroller();
    if (!node) {
      return;
    }
    const onScroll = () => {
      if (node.scrollTop <= 0) {
        revealRef.current();
      }
    };
    node.addEventListener("scroll", onScroll);
    return () => node.removeEventListener("scroll", onScroll);
  }, [ready]);

  useLayoutEffect(() => {
    const node = tracesScroller();
    if (!node) {
      return;
    }
    if (pinTop.current) {
      pinTop.current = false;
      scrollAdjust.current = 0;
      if (node.scrollTop !== 0) {
        node.scrollTop = 0;
        node.dispatchEvent(new Event("scroll"));
      }
      return;
    }
    const adjust = scrollAdjust.current;
    if (adjust === 0) {
      return;
    }
    scrollAdjust.current = 0;
    node.scrollTop += adjust;
    node.dispatchEvent(new Event("scroll"));
  });

  return { status, pending, highlighted, reveal, overflowed };
}

function tracesScroller(): HTMLElement | null {
  return document.querySelector<HTMLElement>('[aria-label="Traces"] [data-scroll-container]');
}
