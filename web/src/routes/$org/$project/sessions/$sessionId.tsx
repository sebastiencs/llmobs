import { createFileRoute, notFound } from "@tanstack/react-router";
import { problemOf } from "@/api/problem";
import { sessionQuery } from "@/api/queries";
import { SessionView } from "@/components/session/session-view";
import { parseSessionPageSearch } from "@/lib/search/sessions";

export const Route = createFileRoute("/$org/$project/sessions/$sessionId")({
  validateSearch: (search) => parseSessionPageSearch(search),
  loaderDeps: ({ search }) => ({
    eval: search.eval,
    exp: search.exp,
  }),
  loader: async ({ context, params, deps }) => {
    try {
      await context.queryClient.ensureQueryData(
        sessionQuery(params.org, params.project, params.sessionId, {
          includeEval: deps.eval === 1,
          includeExperiments: deps.exp === 1,
        }),
      );
    } catch (error) {
      if (problemOf(error)?.status === 404) {
        throw notFound();
      }
      throw error;
    }
  },
  component: SessionPage,
});

function SessionPage() {
  const { sessionId } = Route.useParams();
  const search = Route.useSearch();
  return (
    <div className="relative h-full min-h-0 flex-1">
      <div className="absolute inset-0 flex min-h-0 flex-col">
        <SessionView sessionId={sessionId} search={search} />
      </div>
    </div>
  );
}
