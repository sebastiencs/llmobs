import { QueryClient } from "@tanstack/react-query";
import createClient from "openapi-fetch";
import createQueryHooks from "openapi-react-query";
import { problemOf } from "./problem";
import type { paths } from "./schema.gen";

export const api = createClient<paths>({
  baseUrl: window.location.origin,
  credentials: "same-origin",
});

export const $api = createQueryHooks(api);

/** Up to 2 retries. A problem response with status 400–499 is not retried. */
export function retryQuery(failureCount: number, error: unknown): boolean {
  const problem = problemOf(error);
  if (problem && problem.status >= 400 && problem.status <= 499) {
    return false;
  }
  return failureCount < 2;
}

export function createAppQueryClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: {
        staleTime: 30_000,
        refetchOnWindowFocus: false,
        retry: retryQuery,
      },
    },
  });
}
