import { $api } from "./client";

export function adminUsersInit() {
  return { params: { query: { limit: 50 } } };
}

/** The key `useInfiniteQuery` uses for the admin user list. Invalidate it after a write. */
export function adminUsersKey() {
  return $api.queryOptions("get", "/api/v1/admin/users", adminUsersInit()).queryKey;
}

export const adminSettingsQuery = $api.queryOptions("get", "/api/v1/admin/settings");
