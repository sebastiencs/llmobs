import { $api } from "./client";
import { projectRef } from "./project-ref";

export function evaluatorsInit(org: string, project: string) {
  return { params: { path: { project: projectRef(org, project) }, query: { limit: 100 } } };
}

/** Prefix of every evaluator list query of the project, for invalidation. */
export function evaluatorsKey(org: string, project: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/evaluators", {
    params: { path: { project: projectRef(org, project) } },
  }).queryKey;
}

export function evaluatorInit(org: string, project: string, evaluatorId: string) {
  return { params: { path: { project: projectRef(org, project), id: evaluatorId } } };
}

export function evaluatorQuery(org: string, project: string, evaluatorId: string) {
  return $api.queryOptions(
    "get",
    "/api/v1/projects/{project}/evaluators/{id}",
    evaluatorInit(org, project, evaluatorId),
  );
}

/** The key of one evaluator's detail query, for invalidation. */
export function evaluatorKey(org: string, project: string, evaluatorId: string) {
  return evaluatorQuery(org, project, evaluatorId).queryKey;
}

export function evaluatorTemplatesInit(org: string, project: string) {
  return { params: { path: { project: projectRef(org, project) } } };
}

export function scoreDefinitionsInit(org: string, project: string, includeArchived: boolean) {
  return {
    params: {
      path: { project: projectRef(org, project) },
      query: { limit: 100, include_archived: includeArchived },
    },
  };
}

/** Prefix of every score definition list query of the project, for invalidation. */
export function scoreDefinitionsKey(org: string, project: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/score-definitions", {
    params: { path: { project: projectRef(org, project) } },
  }).queryKey;
}
