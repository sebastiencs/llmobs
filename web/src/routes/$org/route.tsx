import { createFileRoute, notFound, Outlet } from "@tanstack/react-router";
import { meQuery } from "@/api/queries";
import { TopBarLayout } from "@/components/shell/top-bar-layout";

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
    <TopBarLayout>
      <Outlet />
    </TopBarLayout>
  );
}
