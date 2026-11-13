import { createRootRouteWithContext, Outlet } from "@tanstack/react-router";
import { TooltipProvider } from "@/components/ui/tooltip";
import type { RouterContext } from "@/router";

export const Route = createRootRouteWithContext<RouterContext>()({
  component: RootLayout,
});

function RootLayout() {
  return (
    <TooltipProvider>
      <Outlet />
    </TooltipProvider>
  );
}
