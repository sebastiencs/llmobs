import type { Schema } from "./types";

export type Problem = Schema<"Problem">;

export function isProblem(value: unknown): value is Problem {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const record = value as Record<string, unknown>;
  return (
    typeof record.status === "number" &&
    typeof record.title === "string" &&
    typeof record.code === "string"
  );
}

/** The problem when `error` is one (openapi-react-query throws the parsed body). */
export function problemOf(error: unknown): Problem | undefined {
  return isProblem(error) ? error : undefined;
}
