import { createFileRoute, Outlet } from "@tanstack/react-router";
import { SettingsSidebar } from "@/components/settings/settings-sidebar";

export const Route = createFileRoute("/$org/settings")({
  component: OrgSettingsLayout,
});

function OrgSettingsLayout() {
  const { org } = Route.useParams();
  return (
    <SettingsSidebar org={org}>
      <Outlet />
    </SettingsSidebar>
  );
}
