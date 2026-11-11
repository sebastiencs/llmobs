import { $api, api } from "@/api/client";
import { projectRef } from "@/api/project-ref";
import type { Schema } from "@/api/types";

export function traceListOptions(
  org: string,
  project: string,
  query: {
    from: string;
    to: string;
    sort?: Schema<"TraceSort">;
    order?: Schema<"SortOrder">;
    search?: string;
    includeEval?: boolean;
    includeExperiments?: boolean;
  },
) {
  const path = { project: projectRef(org, project) };
  const listQuery = {
    limit: 100,
    from: query.from,
    to: query.to,
    ...(query.sort ? { sort: query.sort } : {}),
    ...(query.order ? { order: query.order } : {}),
    ...(query.search ? { search: query.search } : {}),
    ...(query.includeEval ? { include_eval: true } : {}),
    ...(query.includeExperiments ? { include_experiments: true } : {}),
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
