import { createRootRouteWithContext, Outlet } from "@tanstack/react-router";
import { requireSession } from "@/api/auth";
import { TooltipProvider } from "@/components/ui/tooltip";
import { isPublicPath } from "@/lib/redirect";
import type { RouterContext } from "@/router";

export const Route = createRootRouteWithContext<RouterContext>()({
  beforeLoad: async ({ context, location }) => {
    if (isPublicPath(location.pathname)) {
      return;
    }
    await requireSession(context.queryClient, location);
  },
  component: RootLayout,
});

function RootLayout() {
  return (
    <TooltipProvider>
      <Outlet />
    </TooltipProvider>
  );
}
