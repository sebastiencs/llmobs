import { $api, api } from "./client";
import { projectRef } from "./project-ref";
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
          query: { limit: 50, cursor: pageParam ?? undefined },
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

export function projectInit(org: string, project: string) {
  return {
    params: {
      path: { project: projectRef(org, project) },
    },
  };
}

export function projectQuery(org: string, project: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}", projectInit(org, project));
}

export function traceDetailQuery(org: string, project: string, traceId: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/traces/{trace_id}", {
    params: {
      path: { project: projectRef(org, project), trace_id: traceId },
    },
  });
}

export function schemaQuery(org: string, project: string) {
  return {
    ...$api.queryOptions("get", "/api/v1/projects/{project}/schema", {
      params: { path: { project: projectRef(org, project) } },
    }),
    staleTime: Number.POSITIVE_INFINITY,
  };
}

export function spanDetailQuery(org: string, project: string, traceId: string, spanId: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/traces/{trace_id}/spans/{span_id}", {
    params: {
      path: {
        project: projectRef(org, project),
        trace_id: traceId,
        span_id: spanId,
      },
    },
  });
}
