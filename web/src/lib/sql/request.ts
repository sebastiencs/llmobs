import type { Schema } from "@/api/types";
import type { ParamValue } from "@/lib/sql/params";

/**
 * `POST /sql` body. `params` only when the query has placeholders; `limit` and
 * `timeout_ms` stay server defaults.
 */
export function sqlRunBody(input: {
  query: string;
  from: string;
  to: string;
  params?: Record<string, ParamValue>;
}): Schema<"SqlQueryRequest"> {
  const params = input.params ?? {};
  return {
    query: input.query,
    from: input.from,
    to: input.to,
    ...(Object.keys(params).length > 0
      ? { params: params as Schema<"SqlQueryRequest">["params"] }
      : {}),
  };
}
