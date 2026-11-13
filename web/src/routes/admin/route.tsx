import { createFileRoute, Link, Outlet } from "@tanstack/react-router";
import { $api } from "@/api/client";
import { Forbidden } from "@/components/forbidden";
import { TopBarLayout } from "@/components/shell/top-bar-layout";

export const Route = createFileRoute("/admin")({
  component: AdminLayout,
});

function AdminLayout() {
  const { data: me } = $api.useSuspenseQuery("get", "/api/auth/me");
  if (!me.user.is_instance_admin) {
    return (
      <TopBarLayout>
        <main className="flex-1 overflow-auto">
          <Forbidden description="Only instance admins can open this page." />
        </main>
      </TopBarLayout>
    );
  }
  return (
    <TopBarLayout>
      <main className="mx-auto flex w-full max-w-5xl flex-1 flex-col gap-4 overflow-auto px-4 py-6">
        <h1 className="text-2xl font-medium">Instance admin</h1>
        <nav aria-label="Instance admin" className="flex gap-1 border-b">
          <AdminTab to="/admin/users" label="Users" />
        </nav>
        <Outlet />
      </main>
    </TopBarLayout>
  );
}

function AdminTab({ to, label }: { to: "/admin/users"; label: string }) {
  return (
    <Link
      to={to}
      className="-mb-px border-b-2 border-transparent px-3 py-1.5 text-sm text-muted-foreground hover:text-foreground focus-ring rounded-t-sm"
      activeProps={{ "aria-current": "page", className: "border-primary text-foreground" }}
    >
      {label}
    </Link>
  );
}
