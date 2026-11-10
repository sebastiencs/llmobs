import { createFileRoute, redirect } from "@tanstack/react-router";
import { Building2 } from "lucide-react";
import { meQuery } from "@/api/queries";
import { EmptyState } from "@/components/empty-state";

export const Route = createFileRoute("/")({
  loader: async ({ context }) => {
    const me = await context.queryClient.ensureQueryData(meQuery);
    const org = me.orgs[0];
    if (org) {
      throw redirect({ to: "/$org", params: { org: org.slug } });
    }
  },
  component: NoOrgs,
});

function NoOrgs() {
  return <EmptyState icon={Building2} title="You are not a member of any organization yet." />;
}
