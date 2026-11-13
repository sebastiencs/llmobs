import { $api } from "./client";

export function orgInit(org: string) {
  return { params: { path: { org } } };
}

export function orgQuery(org: string) {
  return $api.queryOptions("get", "/api/v1/orgs/{org}", orgInit(org));
}
