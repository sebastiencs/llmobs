import { createFileRoute, notFound } from "@tanstack/react-router";
import { problemOf } from "@/api/problem";
import { traceDetailQuery } from "@/api/queries";
import { TraceView } from "@/components/trace/trace-view";
import { parseTracePageSearch } from "@/lib/search/traces";

export const Route = createFileRoute("/$org/$project/traces/$traceId")({
  validateSearch: (search) => parseTracePageSearch(search),
  loader: async ({ context, params }) => {
    try {
      await context.queryClient.ensureQueryData(
        traceDetailQuery(params.org, params.project, params.traceId),
      );
    } catch (error) {
      if (problemOf(error)?.status === 404) {
        throw notFound();
      }
      throw error;
    }
  },
  component: TracePage,
});

function TracePage() {
  const { traceId } = Route.useParams();
  const search = Route.useSearch();
  const navigate = Route.useNavigate();
  return (
    <div className="relative h-full min-h-0 flex-1">
      <div className="absolute inset-0 flex min-h-0 flex-col">
        <TraceView
          traceId={traceId}
          spanId={search.span}
          mode={search.mode}
          onSelectSpan={(spanId) => {
            void navigate({
              to: "/$org/$project/traces/$traceId",
              search: (prev) => ({ ...prev, span: spanId }),
              replace: true,
            });
          }}
        />
      </div>
    </div>
  );
}
