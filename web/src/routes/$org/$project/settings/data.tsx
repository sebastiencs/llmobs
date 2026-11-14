import { createFileRoute } from "@tanstack/react-router";
import { DeletionHistory } from "@/components/deletions/deletion-history";
import { UserDeletionForm } from "@/components/deletions/user-deletion-form";
import { Forbidden } from "@/components/forbidden";
import { SettingsPage } from "@/components/settings/settings-page";
import { useProject } from "@/components/shell/project-context";
import { atLeast } from "@/lib/roles";

export const Route = createFileRoute("/$org/$project/settings/data")({
  component: ProjectDataPage,
});

function ProjectDataPage() {
  const { data } = useProject();

  if (!atLeast(data.role, "admin")) {
    return <Forbidden description="Only project admins can delete data." />;
  }

  return (
    <SettingsPage
      title="Data"
      description="Delete end-user data and follow deletion requests. Retention is under General."
    >
      <div className="flex max-w-3xl flex-col gap-8">
        <UserDeletionForm />
        <DeletionHistory />
      </div>
    </SettingsPage>
  );
}
