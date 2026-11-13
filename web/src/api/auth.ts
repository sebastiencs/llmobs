import type { QueryClient } from "@tanstack/react-query";
import { $api } from "./client";

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
