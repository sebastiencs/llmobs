import { createFileRoute, Link, redirect } from "@tanstack/react-router";
import { Building2 } from "lucide-react";
import { meQuery } from "@/api/queries";
import { useSignOut } from "@/components/auth/use-sign-out";
import { EmptyState } from "@/components/empty-state";
import { Button } from "@/components/ui/button";

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
  const { signOut, pending } = useSignOut();
  return (
    <div className="flex flex-col items-center gap-4">
      <EmptyState icon={Building2} title="You are not a member of any organization yet." />
      <Button asChild>
        <Link to="/setup/workspace">Create an organization</Link>
      </Button>
      <Button type="button" variant="ghost" disabled={pending} onClick={signOut}>
        Sign out
      </Button>
    </div>
  );
}
