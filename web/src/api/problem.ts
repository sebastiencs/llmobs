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

/** The first message for each field of a `400` problem; `{}` for anything else. */
export function fieldErrors(error: unknown): Record<string, string> {
  const problem = problemOf(error);
  const result: Record<string, string> = {};
  for (const item of problem?.errors ?? []) {
    if (result[item.field] === undefined) {
      result[item.field] = item.message;
    }
  }
  return result;
}

/** Text for a form-level error: the problem's detail, else its title, else `fallback`. */
export function problemMessage(error: unknown, fallback: string): string {
  const problem = problemOf(error);
  return problem?.detail ?? problem?.title ?? fallback;
}
