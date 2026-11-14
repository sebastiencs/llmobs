import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, notFound, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { $api } from "@/api/client";
import { problemMessage, problemOf } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import { evaluatorQuery, evaluatorsKey } from "@/api/queries-evaluation";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { DisabledReason } from "@/components/disabled-reason";
import { EvaluatorForm } from "@/components/evaluators/evaluator-form";
import { useProject } from "@/components/shell/project-context";
import { Button } from "@/components/ui/button";
import { canManageEvaluation } from "@/lib/roles";

export const Route = createFileRoute("/$org/$project/evaluators/$evaluatorId")({
  loader: async ({ context, params }) => {
    try {
      await context.queryClient.ensureQueryData(
        evaluatorQuery(params.org, params.project, params.evaluatorId),
      );
    } catch (error) {
      if (problemOf(error)?.status === 404) {
        throw notFound();
      }
      throw error;
    }
  },
  component: EvaluatorPage,
});

const KIND_LABELS = {
  llm_judge: "LLM judge",
  check: "Check",
  webhook: "Webhook",
} as const;

function EvaluatorPage() {
  const { org, project, data } = useProject();
  const { evaluatorId } = Route.useParams();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const detail = useQuery(evaluatorQuery(org, project, evaluatorId));
  const remove = $api.useMutation("delete", "/api/v1/projects/{project}/evaluators/{id}");
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [deleteError, setDeleteError] = useState<string | undefined>();
  const canManage = canManageEvaluation(data.role);

  if (detail.isPending) {
    return null;
  }
  if (detail.isError) {
    throw detail.error;
  }
  const evaluator = detail.data;

  function onDelete() {
    setDeleteError(undefined);
    remove.mutate(
      { params: { path: { project: projectRef(org, project), id: evaluator.id } } },
      {
        onSuccess: () => {
          void queryClient.invalidateQueries({ queryKey: evaluatorsKey(org, project) });
          void navigate({ to: "/$org/$project/evaluators", params: { org, project } });
        },
        onError: (caught) => {
          setDeleteError(problemMessage(caught, "Could not delete the evaluator."));
        },
      },
    );
  }

  return (
    <div className="mx-auto flex w-full max-w-3xl flex-1 flex-col gap-6 overflow-auto px-4 py-6">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div className="flex flex-col gap-1">
          <h1 className="text-lg font-medium">{evaluator.name}</h1>
          <p className="text-xs text-muted-foreground">
            {KIND_LABELS[evaluator.kind]} · version {evaluator.version}
          </p>
        </div>
        <DisabledReason reason={canManage ? undefined : "Only members can edit evaluators"}>
          <Button
            type="button"
            variant="outline"
            disabled={!canManage}
            onClick={() => {
              setDeleteError(undefined);
              setConfirmOpen(true);
            }}
          >
            Delete
          </Button>
        </DisabledReason>
      </div>
      {/* Keyed by version: a saved change remounts the form, which clears any secret it still holds. */}
      <EvaluatorForm
        key={`${evaluator.id}-${evaluator.version}`}
        org={org}
        project={project}
        evaluator={evaluator}
        onSaved={() => undefined}
      />
      <ConfirmDialog
        open={confirmOpen}
        onOpenChange={setConfirmOpen}
        title={`Delete ${evaluator.name}?`}
        description="Its test history is kept as evaluation traces."
        confirmLabel="Delete"
        destructive
        pending={remove.isPending}
        error={deleteError}
        onConfirm={onDelete}
      />
    </div>
  );
}
