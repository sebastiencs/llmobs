import { $api, api } from "./client";
import type { Schema } from "./types";

export const meQuery = $api.queryOptions("get", "/api/auth/me");

export function projectListInit(org: string) {
  return {
    params: {
      path: { org },
      query: { limit: 50 },
    },
  };
}

export function projectListOptions(org: string) {
  const { queryKey } = $api.queryOptions(
    "get",
    "/api/v1/orgs/{org}/projects",
    projectListInit(org),
  );
  return {
    queryKey,
    initialPageParam: null as string | null,
    queryFn: async ({ pageParam, signal }: { pageParam: string | null; signal?: AbortSignal }) => {
      const { data, error } = await api.GET("/api/v1/orgs/{org}/projects", {
        params: {
          path: { org },
          query: { limit: 50, cursor: pageParam },
        },
        signal,
      });
      if (error) {
        throw error;
      }
      if (!data) {
        throw new Error("Empty project list response");
      }
      return data;
    },
    getNextPageParam: (page: Schema<"ProjectPage">) => page.next_cursor ?? undefined,
  };
}
