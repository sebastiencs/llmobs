import { createFileRoute } from "@tanstack/react-router";
import { $api } from "@/api/client";
import { OrgInvites } from "@/components/settings/org-invites";
import { OrgMembers } from "@/components/settings/org-members";
import { SettingsPage } from "@/components/settings/settings-page";
import { atLeast } from "@/lib/roles";

export const Route = createFileRoute("/$org/settings/members")({
  component: OrgMembersPage,
});

function OrgMembersPage() {
  const { org } = Route.useParams();
  const { data: me } = $api.useSuspenseQuery("get", "/api/auth/me");
  const role = me.orgs.find((item) => item.slug === org)?.role;
  return (
    <SettingsPage title="Members" description="People in this organization and their roles.">
      <OrgMembers org={org} actorRole={role} me={me.user.id} />
      {atLeast(role, "admin") ? <OrgInvites org={org} actorRole={role} /> : null}
    </SettingsPage>
  );
}
