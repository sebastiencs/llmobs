import { useQuery } from "@tanstack/react-query";
import { problemOf } from "@/api/problem";
import { traceDetailQuery } from "@/api/queries";
import { IdText } from "@/components/id-text";
import { useProject } from "@/components/shell/project-context";
import { StatusDot } from "@/components/status-dot";
import { SpanDetail } from "@/components/trace/span-detail";
import { SplitPane } from "@/components/trace/split-pane";
import { TreeGrid } from "@/components/trace/tree-grid";
import { Skeleton } from "@/components/ui/skeleton";
import { formatCost, formatDuration, formatTokens } from "@/lib/format";

export function TraceView({
  traceId,
  spanId,
  onSelectSpan,
  onAddFilter,
}: {
  traceId: string;
  spanId?: string;
  onSelectSpan: (spanId: string) => void;
  onAddFilter?: (conjunct: string) => void;
}) {
  const { org, project } = useProject();
  const trace = useQuery(traceDetailQuery(org, project, traceId));

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
      <div className="flex h-[32px] shrink-0 items-center gap-2 border-b px-3">
        <span className="min-w-0 truncate text-sm">{detail.trace.name}</span>
        <StatusDot status={detail.trace.status} />
        <span className="num shrink-0 text-xs">{formatDuration(detail.trace.duration_ns)}</span>
        <span className="num shrink-0 text-xs">{formatTokens(detail.trace.total_tokens)}</span>
        <span className="num shrink-0 text-xs">{formatCost(detail.trace.cost_total)}</span>
        <IdText value={detail.trace.trace_id} kind="trace" />
      </div>
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
              <TreeGrid
                key={detail.trace.trace_id}
                spans={detail.spans}
                trace={detail.trace}
                selectedSpanId={selected}
                onSelectSpan={onSelectSpan}
              />
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
