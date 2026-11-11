import { createFileRoute, notFound, Outlet } from "@tanstack/react-router";
import { meQuery } from "@/api/queries";
import { TopBar } from "@/components/shell/top-bar";

export const Route = createFileRoute("/$org")({
  loader: async ({ context, params }) => {
    const me = await context.queryClient.ensureQueryData(meQuery);
    if (!me.orgs.some((org) => org.slug === params.org)) {
      throw notFound();
    }
  },
  component: OrgLayout,
});

function OrgLayout() {
  return (
    <div className="flex h-dvh flex-col overflow-hidden">
      <TopBar />
      <div className="flex min-h-0 flex-1 flex-col">
        <Outlet />
      </div>
    </div>
  );
}
