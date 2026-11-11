import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useRouterState } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import { problemOf } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import { traceDetailQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { useShortcut } from "@/components/command/use-shortcut";
import { useProject } from "@/components/shell/project-context";
import { SpanDetail } from "@/components/trace/span-detail";
import { SplitPane } from "@/components/trace/split-pane";
import { TraceHeader } from "@/components/trace/trace-header";
import { TranscriptView } from "@/components/trace/transcript-view";
import { TreeGrid } from "@/components/trace/tree-grid";
import { Skeleton } from "@/components/ui/skeleton";
import { instantNs, nsBetween } from "@/lib/format";
import type { TraceMode } from "@/lib/search/traces";
import { useStream } from "@/lib/sse";
import { insertSpan } from "@/lib/tree";

const RECENT_MS = 10 * 60 * 1000;
const HIGHLIGHT_MS = 1_500;
const EMPTY_HIGHLIGHTS: ReadonlySet<string> = new Set();

/** A trace that ended within the last 10 minutes still receives live spans. */
export function traceIsRecent(endTime: string, now: number): boolean {
  const end = Date.parse(endTime);
  return Number.isFinite(end) && now - end <= RECENT_MS;
}

export function TraceView({
  traceId,
  spanId,
  mode,
  onModeChange,
  onSelectSpan,
  onAddFilter,
  onClose,
}: {
  traceId: string;
  spanId?: string;
  mode?: TraceMode;
  onModeChange?: (mode: TraceMode) => void;
  onSelectSpan: (spanId: string) => void;
  onAddFilter?: (conjunct: string) => void;
  onClose?: () => void;
}) {
  const { org, project } = useProject();
  const detailOptions = traceDetailQuery(org, project, traceId);
  const trace = useQuery(detailOptions);
  const queryClient = useQueryClient();
  const listLive = useRouterState({
    select: (state) => {
      const search = state.location.search as { live?: unknown };
      return search?.live === 1 || search?.live === "1";
    },
  });
  const [storedMode, setStoredMode] = useState<TraceMode>("tree");
  const [stoppedId, setStoppedId] = useState<string | null>(null);
  const [highlightState, setHighlightState] = useState<{
    traceId: string;
    ids: ReadonlySet<string>;
  }>({ traceId, ids: EMPTY_HIGHLIGHTS });
  const timers = useRef<number[]>([]);
  const stopped = stoppedId === traceId;
  const highlighted = highlightState.traceId === traceId ? highlightState.ids : EMPTY_HIGHLIGHTS;
  const active = onModeChange ? (mode ?? "tree") : storedMode;
  function chooseMode(next: TraceMode) {
    if (onModeChange) onModeChange(next);
    else setStoredMode(next);
  }

  useEffect(() => {
    return () => {
      for (const timer of timers.current) {
        window.clearTimeout(timer);
      }
    };
  }, []);

  const detail = trace.data;
  const follow =
    detail !== undefined &&
    !stopped &&
    (listLive || traceIsRecent(detail.trace.end_time, Date.now()));
  const queryKeyRef = useRef(detailOptions.queryKey);
  queryKeyRef.current = detailOptions.queryKey;

  function mark(spanIdToMark: string) {
    setHighlightState((current) => {
      const ids = current.traceId === traceId ? new Set(current.ids) : new Set<string>();
      ids.add(spanIdToMark);
      return { traceId, ids };
    });
    const timer = window.setTimeout(() => {
      setHighlightState((current) => {
        if (current.traceId !== traceId) {
          return current;
        }
        const ids = new Set(current.ids);
        ids.delete(spanIdToMark);
        return { traceId, ids };
      });
    }, HIGHLIGHT_MS);
    timers.current.push(timer);
  }

  const { status } = useStream(
    follow ? spanStreamUrl(projectRef(org, project), traceId) : undefined,
    (event) => {
      const key = queryKeyRef.current;
      if (event.type === "resync") {
        void queryClient.invalidateQueries({ queryKey: key, exact: true });
        return;
      }
      if (event.type === "trace") {
        queryClient.setQueryData<Schema<"TraceDetail">>(key, (current) =>
          current ? { ...current, trace: event.trace } : current,
        );
        return;
      }
      if (event.type !== "span") {
        return;
      }
      const current = queryClient.getQueryData<Schema<"TraceDetail">>(key);
      if (!current) {
        return;
      }
      const inserted = current.spans.every((row) => row.span.span_id !== event.span.span_id);
      queryClient.setQueryData<Schema<"TraceDetail">>(key, (data) => {
        if (!data) {
          return data;
        }
        const spans = insertSpan(data.spans, event.span);
        return { ...data, spans, trace: withSpanTotals(data.trace, spans) };
      });
      if (inserted) {
        mark(event.span.span_id);
      }
    },
  );

  useShortcut("toggle-mode", () => {
    chooseMode(active === "transcript" ? "tree" : "transcript");
  });

  if (trace.isPending) {
    return (
      <div className="flex flex-col gap-2 p-3">
        <Skeleton className="h-4 w-1/2" />
        <Skeleton className="h-4 w-2/3" />
        <Skeleton className="h-4 w-1/3" />
      </div>
    );
  }
  if (trace.isError) {
    return (
      <p className="p-3 text-sm">{problemOf(trace.error)?.title ?? "Could not load this trace"}</p>
    );
  }

  const loaded = trace.data;
  const selected = spanId ?? loaded.spans[0]?.span.span_id;
  const detached = loaded.spans.find((row) => row.span.span_id === selected)?.detached ?? false;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <TraceHeader
        trace={loaded.trace}
        spanId={selected}
        mode={active === "transcript" ? active : undefined}
        onClose={onClose}
        live={follow ? { status, onStop: () => setStoppedId(traceId) } : undefined}
      />
      {loaded.truncated ? (
        <div className="shrink-0 border-b bg-muted px-3 py-1 text-xs">
          Showing the first 20 000 spans
        </div>
      ) : null}
      <div className="relative min-h-0 flex-1">
        <div className="absolute inset-0">
          <SplitPane
            name="trace"
            start={
              active === "transcript" ? (
                <TranscriptView
                  key={loaded.trace.trace_id}
                  traceId={traceId}
                  spans={loaded.spans}
                  selectedSpanId={selected}
                  onSelectSpan={onSelectSpan}
                  mode={active}
                  onMode={chooseMode}
                />
              ) : (
                <TreeGrid
                  key={loaded.trace.trace_id}
                  spans={loaded.spans}
                  trace={loaded.trace}
                  selectedSpanId={selected}
                  onSelectSpan={onSelectSpan}
                  mode={active}
                  onMode={chooseMode}
                  highlightedSpanIds={highlighted}
                />
              )
            }
            end={
              selected ? (
                <SpanDetail
                  traceId={traceId}
                  spanId={selected}
                  detached={detached}
                  onAddFilter={onAddFilter}
                />
              ) : null
            }
          />
        </div>
      </div>
    </div>
  );
}

function spanStreamUrl(project: string, traceId: string): string {
  return `/api/v1/projects/${encodeURIComponent(project)}/traces/${encodeURIComponent(traceId)}/stream`;
}

/** Span count and duration follow the cached tree when a live span arrives without a trace event. */
function withSpanTotals(
  trace: Schema<"TraceSummary">,
  spans: readonly Schema<"TreeSpan">[],
): Schema<"TraceSummary"> {
  let endTime = trace.end_time;
  let endNs = instantNs(endTime);
  for (const row of spans) {
    const ns = instantNs(row.span.end_time);
    if (ns > endNs) {
      endNs = ns;
      endTime = row.span.end_time;
    }
  }
  return {
    ...trace,
    span_count: spans.length,
    duration_ns: Math.max(0, nsBetween(trace.start_time, endTime)),
    end_time: endTime,
  };
}
