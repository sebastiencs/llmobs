import { $api } from "./client";
import { projectRef } from "./project-ref";

export function orgInit(org: string) {
  return { params: { path: { org } } };
}

export function orgQuery(org: string) {
  return $api.queryOptions("get", "/api/v1/orgs/{org}", orgInit(org));
}

export function orgMembersInit(org: string) {
  return { params: { path: { org }, query: { limit: 50 } } };
}

export function orgMembersKey(org: string) {
  return $api.queryOptions("get", "/api/v1/orgs/{org}/members", orgMembersInit(org)).queryKey;
}

export function orgInvitesInit(org: string) {
  return { params: { path: { org }, query: { limit: 50 } } };
}

export function orgInvitesKey(org: string) {
  return $api.queryOptions("get", "/api/v1/orgs/{org}/invites", orgInvitesInit(org)).queryKey;
}

export function apiKeysInit(org: string, project: string) {
  return { params: { path: { project: projectRef(org, project) }, query: { limit: 50 } } };
}

export function apiKeysKey(org: string, project: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/api-keys", apiKeysInit(org, project))
    .queryKey;
}
