import { createFileRoute } from "@tanstack/react-router";
import { $api } from "@/api/client";
import { traceListInit, traceListQuery } from "@/api/queries";
import { useProject } from "@/components/shell/project-context";
import { StatusDot } from "@/components/status-dot";
import { TimeText } from "@/components/time-text";
import { formatDuration } from "@/lib/format";

export const Route = createFileRoute("/$org/$project/traces/")({
  loader: ({ context, params }) =>
    context.queryClient.ensureQueryData(traceListQuery(params.org, params.project)),
  component: TracesPage,
});

function TracesPage() {
  const { org, project } = useProject();
  const { data } = $api.useSuspenseQuery(
    "get",
    "/api/v1/projects/{project}/traces",
    traceListInit(org, project),
  );

  return (
    <div className="flex h-full flex-col">
      <h1 className="flex h-10 shrink-0 items-center border-b px-3 text-sm font-medium">Traces</h1>
      <ul className="min-h-0 flex-1 overflow-auto" aria-label="Traces">
        {data.data.map((trace) => (
          <li key={trace.trace_id} className="flex h-row items-center gap-3 border-b px-3">
            <StatusDot status={trace.status} />
            <TimeText value={trace.start_time} style="datetime" />
            <span className="min-w-0 flex-1 truncate">{trace.name}</span>
            <span className="num">{formatDuration(trace.duration_ns)}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}
