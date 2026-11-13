import type { QueryClient } from "@tanstack/react-query";
import { redirect } from "@tanstack/react-router";
import { safeRedirect } from "@/lib/redirect";
import { $api } from "./client";
import { problemOf } from "./problem";
import { meQuery } from "./queries";

/** Login methods and setup state. Changes only with the server's config. */
export const authConfigQuery = {
  ...$api.queryOptions("get", "/api/auth/config"),
  staleTime: 60_000,
};

/**
 * Drops every cached query of the previous session, except the auth config, which is
 * not user data and which the public pages observe. Removing a query that a mounted
 * `useSuspenseQuery` observes would make that page suspend and refetch it.
 */
export function clearUserData(queryClient: QueryClient): void {
  queryClient.removeQueries({
    predicate: (query) => query.queryKey[1] !== "/api/auth/config",
  });
}

/** The auth gate (D-L-1). Throws a router redirect when there is no session. */
export async function requireSession(
  queryClient: QueryClient,
  location: { href: string },
): Promise<void> {
  try {
    await queryClient.ensureQueryData(meQuery);
  } catch (error) {
    if (problemOf(error)?.status !== 401) {
      throw error;
    }
    const config = await queryClient.fetchQuery(authConfigQuery);
    if (config.setup_required) {
      throw redirect({ to: "/setup" });
    }
    const target = safeRedirect(location.href);
    throw redirect({ to: "/login", search: target === undefined ? {} : { redirect: target } });
  }
}
