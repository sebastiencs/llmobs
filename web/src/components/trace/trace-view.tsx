import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { problemOf } from "@/api/problem";
import { traceDetailQuery } from "@/api/queries";
import { useProject } from "@/components/shell/project-context";
import { SpanDetail } from "@/components/trace/span-detail";
import { SplitPane } from "@/components/trace/split-pane";
import { TraceHeader } from "@/components/trace/trace-header";
import { TranscriptView } from "@/components/trace/transcript-view";
import { TreeGrid } from "@/components/trace/tree-grid";
import { Skeleton } from "@/components/ui/skeleton";
import type { TraceMode } from "@/lib/search/traces";

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
  const trace = useQuery(traceDetailQuery(org, project, traceId));
  const [storedMode, setStoredMode] = useState<TraceMode>("tree");
  const active = onModeChange ? (mode ?? "tree") : storedMode;
  function chooseMode(next: TraceMode) {
    if (onModeChange) onModeChange(next);
    else setStoredMode(next);
  }

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

  const detail = trace.data;
  const selected = spanId ?? detail.spans[0]?.span.span_id;
  const detached = detail.spans.find((row) => row.span.span_id === selected)?.detached ?? false;

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <TraceHeader
        trace={detail.trace}
        spanId={selected}
        mode={active === "transcript" ? active : undefined}
        onClose={onClose}
      />
      {detail.truncated ? (
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
                  key={detail.trace.trace_id}
                  traceId={traceId}
                  spans={detail.spans}
                  selectedSpanId={selected}
                  onSelectSpan={onSelectSpan}
                  mode={active}
                  onMode={chooseMode}
                />
              ) : (
                <TreeGrid
                  key={detail.trace.trace_id}
                  spans={detail.spans}
                  trace={detail.trace}
                  selectedSpanId={selected}
                  onSelectSpan={onSelectSpan}
                  mode={active}
                  onMode={chooseMode}
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
