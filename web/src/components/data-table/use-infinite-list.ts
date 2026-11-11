import { $api, api } from "@/api/client";
import { projectRef } from "@/api/project-ref";
import type { Schema } from "@/api/types";

export function traceListOptions(
  org: string,
  project: string,
  query: { from: string; sort?: Schema<"TraceSort">; order?: Schema<"SortOrder"> },
) {
  const path = { project: projectRef(org, project) };
  const listQuery = {
    limit: 100,
    from: query.from,
    ...(query.sort ? { sort: query.sort } : {}),
    ...(query.order ? { order: query.order } : {}),
  };
  const { queryKey } = $api.queryOptions("get", "/api/v1/projects/{project}/traces", {
    params: { path, query: listQuery },
  });
  return {
    queryKey,
    initialPageParam: null as string | null,
    queryFn: async ({ pageParam, signal }: { pageParam: string | null; signal?: AbortSignal }) => {
      const { data, error } = await api.GET("/api/v1/projects/{project}/traces", {
        params: {
          path,
          query: { ...listQuery, ...(pageParam ? { cursor: pageParam } : {}) },
        },
        signal,
      });
      if (error) {
        throw error;
      }
      if (!data) {
        throw new Error("Empty trace list response");
      }
      return data;
    },
    getNextPageParam: (page: Schema<"TracePage">) => page.next_cursor ?? undefined,
  };
}
