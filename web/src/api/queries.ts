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

export function spanListOptions(
  org: string,
  project: string,
  query: {
    from: string;
    to: string;
    sort?: Schema<"SpanSort">;
    order?: Schema<"SortOrder">;
    search?: string;
    filter?: string;
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
    ...(query.filter ? { filter: query.filter } : {}),
    ...(query.includeEval ? { include_eval: true } : {}),
    ...(query.includeExperiments ? { include_experiments: true } : {}),
  };
  const { queryKey } = $api.queryOptions("get", "/api/v1/projects/{project}/spans", {
    params: { path, query: listQuery },
  });
  return {
    queryKey,
    initialPageParam: null as string | null,
    queryFn: async ({ pageParam, signal }: { pageParam: string | null; signal?: AbortSignal }) => {
      const { data, error } = await api.GET("/api/v1/projects/{project}/spans", {
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
        throw new Error("Empty span list response");
      }
      return data;
    },
    getNextPageParam: (page: Schema<"SpanPage">) => page.next_cursor ?? undefined,
  };
}

export function scoresQuery(
  org: string,
  project: string,
  query: {
    trace_id: string;
    span_id?: string;
    target?: Schema<"ScoreTargetKind">;
    limit?: number;
  },
) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/scores", {
    params: {
      path: { project: projectRef(org, project) },
      query,
    },
  });
}

export function sessionListOptions(
  org: string,
  project: string,
  query: {
    from: string;
    to: string;
    sort?: Schema<"SessionSort">;
    order?: Schema<"SortOrder">;
    filter?: string;
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
    ...(query.filter ? { filter: query.filter } : {}),
    ...(query.includeEval ? { include_eval: true } : {}),
    ...(query.includeExperiments ? { include_experiments: true } : {}),
  };
  const { queryKey } = $api.queryOptions("get", "/api/v1/projects/{project}/sessions", {
    params: { path, query: listQuery },
  });
  return {
    queryKey,
    initialPageParam: null as string | null,
    queryFn: async ({ pageParam, signal }: { pageParam: string | null; signal?: AbortSignal }) => {
      const { data, error } = await api.GET("/api/v1/projects/{project}/sessions", {
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
        throw new Error("Empty session list response");
      }
      return data;
    },
    getNextPageParam: (page: Schema<"SessionPage">) => page.next_cursor ?? undefined,
  };
}

export function sessionQuery(
  org: string,
  project: string,
  sessionId: string,
  flags: { includeEval?: boolean; includeExperiments?: boolean },
) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/sessions/{session_id}", {
    params: {
      path: { project: projectRef(org, project), session_id: sessionId },
      query: {
        ...(flags.includeEval ? { include_eval: true } : {}),
        ...(flags.includeExperiments ? { include_experiments: true } : {}),
      },
    },
  });
}

export function savedViewsQuery(org: string, project: string, kind: Schema<"SavedViewKind">) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/saved-views", {
    params: {
      path: { project: projectRef(org, project) },
      query: { kind },
    },
  });
}
