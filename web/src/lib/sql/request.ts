import type { Schema } from "@/api/types";

/**
 * `POST /sql` body. `limit` and `timeout_ms` are left to the server defaults
 * (10,000 rows, 60 s).
 */
export function sqlRunBody(input: {
  query: string;
  from: string;
  to: string;
}): Schema<"SqlQueryRequest"> {
  return { query: input.query, from: input.from, to: input.to };
}
