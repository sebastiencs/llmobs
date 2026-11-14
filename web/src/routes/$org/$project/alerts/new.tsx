import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { RuleForm } from "@/components/alerts/rule-form";
import { Forbidden } from "@/components/forbidden";
import { useProject } from "@/components/shell/project-context";
import { canManageAlerts } from "@/lib/roles";

export const Route = createFileRoute("/$org/$project/alerts/new")({
  component: NewRulePage,
});

function NewRulePage() {
  const { org, project, data } = useProject();
  const navigate = useNavigate();

  if (!canManageAlerts(data.role)) {
    return <Forbidden description="Only project members and above can create alert rules." />;
  }

  return (
    <div className="mx-auto flex w-full max-w-5xl flex-1 flex-col gap-6 overflow-auto px-4 py-6">
      <h1 className="text-lg font-medium">New alert rule</h1>
      <RuleForm
        org={org}
        project={project}
        onSaved={(rule) => {
          void navigate({
            to: "/$org/$project/alerts/$ruleId",
            params: { org, project, ruleId: rule.id },
          });
        }}
        onCancel={() => {
          void navigate({ to: "/$org/$project/alerts", params: { org, project } });
        }}
      />
    </div>
  );
}
