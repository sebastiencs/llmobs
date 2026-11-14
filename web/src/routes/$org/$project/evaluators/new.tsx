import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { EvaluatorForm } from "@/components/evaluators/evaluator-form";
import { useProject } from "@/components/shell/project-context";

export const Route = createFileRoute("/$org/$project/evaluators/new")({
  component: NewEvaluatorPage,
});

function NewEvaluatorPage() {
  const { org, project } = useProject();
  const navigate = useNavigate();
  return (
    <div className="mx-auto flex w-full max-w-3xl flex-1 flex-col gap-6 overflow-auto px-4 py-6">
      <h1 className="text-lg font-medium">New evaluator</h1>
      <EvaluatorForm
        org={org}
        project={project}
        onSaved={(saved) => {
          void navigate({
            to: "/$org/$project/evaluators/$evaluatorId",
            params: { org, project, evaluatorId: saved.id },
          });
        }}
      />
    </div>
  );
}
