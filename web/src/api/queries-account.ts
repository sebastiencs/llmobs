import { $api } from "./client";

export function personalTokensInit() {
  return { params: { query: { limit: 50 } } };
}

/** Same key as the `useInfiniteQuery` that lists tokens, for invalidation. */
export function personalTokensKey() {
  return $api.queryOptions("get", "/api/v1/me/tokens", personalTokensInit()).queryKey;
}
