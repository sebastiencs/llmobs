import { useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, notFound, useNavigate } from "@tanstack/react-router";
import { problemOf } from "@/api/problem";
import { alertRuleQuery } from "@/api/queries-alerts";
import { RuleForm } from "@/components/alerts/rule-form";
import { Forbidden } from "@/components/forbidden";
import { useProject } from "@/components/shell/project-context";
import { canManageAlerts } from "@/lib/roles";

export const Route = createFileRoute("/$org/$project/alerts/$ruleId/edit")({
  loader: async ({ context, params }) => {
    try {
      await context.queryClient.ensureQueryData(
        alertRuleQuery(params.org, params.project, params.ruleId),
      );
    } catch (error) {
      if (problemOf(error)?.status === 404) {
        throw notFound();
      }
      throw error;
    }
  },
  component: EditRulePage,
});

function EditRulePage() {
  const { ruleId } = Route.useParams();
  const { org, project, data } = useProject();
  const navigate = useNavigate();
  const { data: rule } = useSuspenseQuery(alertRuleQuery(org, project, ruleId));

  if (!canManageAlerts(data.role)) {
    return <Forbidden description="Only project members and above can edit alert rules." />;
  }

  const toRule = () => {
    void navigate({
      to: "/$org/$project/alerts/$ruleId",
      params: { org, project, ruleId: rule.id },
    });
  };

  return (
    <div className="mx-auto flex w-full max-w-5xl flex-1 flex-col gap-6 overflow-auto px-4 py-6">
      <h1 className="text-lg font-medium">Edit {rule.name}</h1>
      <RuleForm org={org} project={project} rule={rule} onSaved={toRule} onCancel={toRule} />
    </div>
  );
}
