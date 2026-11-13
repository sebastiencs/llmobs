import { $api } from "./client";

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
