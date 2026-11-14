import { $api } from "@/api/client";
import { projectRef } from "@/api/project-ref";

export function channelsInit(org: string, project: string) {
  return { params: { path: { project: projectRef(org, project) } } };
}

export function channelsQuery(org: string, project: string) {
  return $api.queryOptions(
    "get",
    "/api/v1/projects/{project}/channels",
    channelsInit(org, project),
  );
}

export function alertRulesQuery(org: string, project: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/alert-rules", {
    params: { path: { project: projectRef(org, project) } },
  });
}

export function alertRuleQuery(org: string, project: string, id: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/alert-rules/{id}", {
    params: { path: { project: projectRef(org, project), id } },
  });
}

export function alertEventsInit(org: string, project: string, id: string) {
  return { params: { path: { project: projectRef(org, project), id }, query: { limit: 50 } } };
}
