import type { QueryClient } from "@tanstack/react-query";
import { createRouter, type RouterHistory } from "@tanstack/react-router";
import { clearUserData } from "@/api/auth";
import { setUnauthorizedHandler } from "@/api/session";
import { ErrorState } from "@/components/error-state";
import { NotFound } from "@/components/not-found";
import { isPublicPath, safeRedirect } from "@/lib/redirect";
import { routeTree } from "./routeTree.gen";

export interface RouterContext {
  queryClient: QueryClient;
}

export function createAppRouter(options: { queryClient: QueryClient; history?: RouterHistory }) {
  const router = createRouter({
    routeTree,
    context: { queryClient: options.queryClient },
    ...(options.history ? { history: options.history } : {}),
    defaultPreload: "intent",
    defaultPreloadStaleTime: 0,
    defaultNotFoundComponent: NotFound,
    defaultErrorComponent: ErrorState,
    scrollRestoration: true,
  });
  setUnauthorizedHandler((apiPath) => {
    // The gate's own `me` check runs while the router is loading; the gate redirects itself.
    if (apiPath === "/api/auth/me" && router.state.status === "pending") {
      return;
    }
    const location = router.state.location;
    if (isPublicPath(location.pathname)) {
      return;
    }
    const target = safeRedirect(location.href);
    void router
      .navigate({ to: "/login", search: target === undefined ? {} : { redirect: target } })
      .then(() => {
        clearUserData(options.queryClient);
      });
  });
  return router;
}

export type AppRouter = ReturnType<typeof createAppRouter>;

declare module "@tanstack/react-router" {
  interface Register {
    router: AppRouter;
  }
}
