import { createFileRoute, notFound, Outlet } from "@tanstack/react-router";
import { meQuery } from "@/api/queries";
import { AppHeader } from "@/components/app-header";

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
    <>
      <AppHeader />
      <main className="mx-auto flex w-full max-w-5xl flex-1 flex-col px-4 py-6">
        <Outlet />
      </main>
    </>
  );
}
