import { useQuery } from "@tanstack/react-query";
import { createFileRoute, notFound, useNavigate } from "@tanstack/react-router";
import { MessagesSquare } from "lucide-react";
import { useMemo } from "react";
import { problemOf } from "@/api/problem";
import { userQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { visibleColumnIds } from "@/components/data-table/column-layout";
import { DataTable } from "@/components/data-table/data-table";
import { EmptyState } from "@/components/empty-state";
import { IdText } from "@/components/id-text";
import {
  orderedSessionColumns,
  sessionColumnSpecs,
  sessionScoreNames,
} from "@/components/session-columns";
import { useProject } from "@/components/shell/project-context";
import { TimeText } from "@/components/time-text";
import { TooltipProvider } from "@/components/ui/tooltip";
import { formatCost, formatTokens } from "@/lib/format";
import { parseUserPageSearch, type UserPageSearch } from "@/lib/search/users";

export const Route = createFileRoute("/$org/$project/users/$userId")({
  validateSearch: (search) => parseUserPageSearch(search),
  loaderDeps: ({ search }) => ({
    eval: search.eval,
    exp: search.exp,
  }),
  loader: async ({ context, params, deps }) => {
    try {
      await context.queryClient.ensureQueryData(
        userQuery(params.org, params.project, params.userId, {
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
  component: UserPage,
});

function UserPage() {
  const { org, project } = useProject();
  const { userId } = Route.useParams();
  const search = Route.useSearch();
  const navigate = useNavigate();
  const detail = useQuery(
    userQuery(org, project, userId, {
      includeEval: search.eval === 1,
      includeExperiments: search.exp === 1,
    }),
  );
  const sessions = detail.data?.sessions ?? [];
  const columns = useMemo(() => {
    const specs = sessionColumnSpecs(sessionScoreNames(sessions));
    return orderedSessionColumns(sessions, visibleColumnIds(specs, undefined));
  }, [sessions]);

  if (detail.isPending) {
    return null;
  }
  if (detail.isError) {
    throw detail.error;
  }

  return (
    <div className="flex h-full min-h-0 flex-1 flex-col">
      <UserHeader user={detail.data.user} />
      <TooltipProvider>
        <DataTable
          tableId="user-sessions"
          ariaLabel="Sessions"
          columns={columns}
          rows={sessions}
          getRowId={(row) => row.session_id}
          onActivate={(row) => {
            void navigate({
              to: "/$org/$project/sessions/$sessionId",
              params: { org, project, sessionId: row.session_id },
              search: pageFlags(search),
            });
          }}
          hasNextPage={false}
          isFetchingNextPage={false}
          fetchNextPage={() => undefined}
          emptyState={<EmptyState icon={MessagesSquare} title="No sessions" />}
        />
      </TooltipProvider>
    </div>
  );
}

function UserHeader({ user }: { user: Schema<"EndUserSummary"> }) {
  return (
    <header className="flex shrink-0 flex-col gap-1 border-b px-3 py-1.5 text-xs">
      <IdText value={user.user_id} kind="user" />
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-muted-foreground">
        <span>
          <TimeText value={user.first_seen} style="datetime" />
          <span aria-hidden="true"> → </span>
          <TimeText value={user.last_seen} style="datetime" />
        </span>
        <span>{count(user.trace_count, "trace")}</span>
        <span>{count(user.session_count, "session")}</span>
        {user.total_tokens !== undefined ? (
          <span className="num text-foreground">{formatTokens(user.total_tokens)}</span>
        ) : null}
        {user.cost_total !== undefined ? (
          <span className="num text-foreground">{formatCost(user.cost_total)}</span>
        ) : null}
        <span>{count(user.error_count, "error")}</span>
      </div>
    </header>
  );
}

function count(value: number, noun: string): string {
  return `${value} ${noun}${value === 1 ? "" : "s"}`;
}

function pageFlags(search: UserPageSearch): { eval?: 1; exp?: 1 } {
  return {
    ...(search.eval ? { eval: 1 as const } : {}),
    ...(search.exp ? { exp: 1 as const } : {}),
  };
}
