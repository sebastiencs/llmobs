import { createFileRoute, Link } from "@tanstack/react-router";
import { Gauge, Loader2 } from "lucide-react";
import { $api } from "@/api/client";
import { evaluatorsInit } from "@/api/queries-evaluation";
import type { Schema } from "@/api/types";
import { DisabledReason } from "@/components/disabled-reason";
import { EmptyState } from "@/components/empty-state";
import { useProject } from "@/components/shell/project-context";
import { TimeText } from "@/components/time-text";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { canManageEvaluation } from "@/lib/roles";

export const Route = createFileRoute("/$org/$project/evaluators/")({
  component: EvaluatorsPage,
});

const KIND_LABELS: Record<Schema<"EvaluatorKind">, string> = {
  llm_judge: "LLM judge",
  check: "Check",
  webhook: "Webhook",
};

/** The one config field that identifies the evaluator: the model, the check or the webhook host. */
function detailOf(evaluator: Schema<"Evaluator">): string {
  const config: Record<string, unknown> = evaluator.config;
  const key =
    evaluator.kind === "llm_judge" ? "model" : evaluator.kind === "check" ? "check" : "url_host";
  const value = config[key];
  return typeof value === "string" ? value : "—";
}

function EvaluatorsPage() {
  const { org, project, data } = useProject();
  const canManage = canManageEvaluation(data.role);
  const query = $api.useInfiniteQuery(
    "get",
    "/api/v1/projects/{project}/evaluators",
    evaluatorsInit(org, project),
    {
      pageParamName: "cursor",
      initialPageParam: null,
      getNextPageParam: (page) => page.next_cursor ?? undefined,
    },
  );

  if (query.error) {
    throw query.error;
  }

  const rows = query.data?.pages.flatMap((page) => page.data) ?? [];

  return (
    <div className="mx-auto flex w-full max-w-6xl flex-1 flex-col gap-6 overflow-auto px-4 py-6">
      <div className="flex items-start justify-between gap-4">
        <h1 className="text-lg font-medium">Evaluators</h1>
        <DisabledReason reason={canManage ? undefined : "Only members can create evaluators"}>
          {canManage ? (
            <Button asChild>
              <Link to="/$org/$project/evaluators/new" params={{ org, project }}>
                New evaluator
              </Link>
            </Button>
          ) : (
            <Button disabled>New evaluator</Button>
          )}
        </DisabledReason>
      </div>
      {query.isPending ? (
        <>
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
          <Skeleton className="h-8 w-full" />
        </>
      ) : rows.length === 0 ? (
        <EmptyState
          icon={Gauge}
          title="No evaluators yet"
          description="Create one from a template to score traces with an LLM judge, a check or a webhook."
        />
      ) : (
        <Table aria-label="Evaluators">
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Kind</TableHead>
              <TableHead>Score</TableHead>
              <TableHead>Details</TableHead>
              <TableHead>Version</TableHead>
              <TableHead>Updated</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((evaluator) => (
              <TableRow key={evaluator.id}>
                <TableCell>
                  <Link
                    to="/$org/$project/evaluators/$evaluatorId"
                    params={{ org, project, evaluatorId: evaluator.id }}
                    className="rounded-sm underline-offset-4 outline-none hover:underline focus-visible:outline-2 focus-visible:outline-ring focus-visible:outline-offset-2 focus-visible:outline-solid"
                  >
                    {evaluator.name}
                  </Link>
                </TableCell>
                <TableCell>{KIND_LABELS[evaluator.kind]}</TableCell>
                <TableCell>
                  <div className="flex flex-col">
                    <span>{evaluator.output_score.name}</span>
                    <span className="text-xs text-muted-foreground">
                      {evaluator.output_score.data_type}
                    </span>
                  </div>
                </TableCell>
                <TableCell>
                  <span className="font-mono text-xs">{detailOf(evaluator)}</span>
                </TableCell>
                <TableCell>{evaluator.version}</TableCell>
                <TableCell>
                  <TimeText value={evaluator.updated_at} style="datetime" />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {query.hasNextPage ? (
        <Button
          type="button"
          variant="outline"
          className="w-fit"
          disabled={query.isFetchingNextPage}
          onClick={() => {
            void query.fetchNextPage();
          }}
        >
          {query.isFetchingNextPage ? <Loader2 className="animate-spin" /> : null}
          Load more
        </Button>
      ) : null}
    </div>
  );
}
