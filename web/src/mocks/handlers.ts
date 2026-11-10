import { type HttpHandler, HttpResponse, http } from "msw";
import { type MockEntry, type MockOperation, operations } from "./manifest";

const NOT_FOUND = {
  type: "about:blank",
  title: "Not Found",
  status: 404,
  code: "not_found",
} as const;

export const handlers: HttpHandler[] = sortBySpecificity(operations).map((operation) =>
  http.get(operation.mswPath, ({ request, params }) => {
    const entry = selectEntry(operation, pathParamsOf(params), queryOf(request.url));
    if (!entry) {
      return HttpResponse.json(NOT_FOUND, {
        status: 404,
        headers: { "Content-Type": "application/problem+json" },
      });
    }
    if (entry.body instanceof Uint8Array) {
      const buffer = new ArrayBuffer(entry.body.byteLength);
      new Uint8Array(buffer).set(entry.body);
      return HttpResponse.arrayBuffer(buffer, {
        status: entry.status,
        headers: { "Content-Type": entry.contentType },
      });
    }
    return HttpResponse.json(jsonBody(entry.body), {
      status: entry.status,
      headers: { "Content-Type": entry.contentType },
    });
  }),
);

export function sortBySpecificity<T extends { mswPath: string }>(items: readonly T[]): T[] {
  return [...items].sort((left, right) => {
    const byLiterals = literalSegments(right.mswPath) - literalSegments(left.mswPath);
    if (byLiterals !== 0) {
      return byLiterals;
    }
    if (left.mswPath < right.mswPath) {
      return -1;
    }
    if (left.mswPath > right.mswPath) {
      return 1;
    }
    return 0;
  });
}

function literalSegments(path: string): number {
  return path.split("/").filter((segment) => segment.length > 0 && !segment.startsWith(":")).length;
}

function selectEntry(
  operation: MockOperation,
  pathParams: Record<string, string>,
  query: Record<string, string>,
): MockEntry | undefined {
  const candidates = operation.entries.filter((entry) =>
    Object.entries(entry.pathParams).every(([key, value]) => pathParams[key] === value),
  );
  const matching = candidates.filter((entry) =>
    Object.entries(entry.query).every(([key, value]) => query[key] === value),
  );
  let winners: MockEntry[] = [];
  let winnerCount = -1;
  for (const entry of matching) {
    const count = Object.keys(entry.query).length;
    if (count > winnerCount) {
      winners = [entry];
      winnerCount = count;
    } else if (count === winnerCount) {
      winners.push(entry);
    }
  }
  const winner = winners[0];
  if (!winner) {
    return undefined;
  }
  if (winners.length > 1) {
    const files = winners.map((entry) => entry.file).join(", ");
    throw new Error(`ambiguous fixture match for ${operation.operationId}: ${files}`);
  }
  return winner;
}

function jsonBody(body: unknown): Record<string, unknown> | string | number | boolean | null {
  if (
    body === null ||
    typeof body === "string" ||
    typeof body === "number" ||
    typeof body === "boolean"
  ) {
    return body;
  }
  if (typeof body === "object" && !(body instanceof Uint8Array)) {
    return body as Record<string, unknown>;
  }
  throw new Error("fixture body is not JSON");
}

function pathParamsOf(params: Record<string, unknown>): Record<string, string> {
  const pathParams: Record<string, string> = {};
  for (const [key, value] of Object.entries(params)) {
    if (typeof value === "string") {
      pathParams[key] = value;
    } else if (Array.isArray(value) && typeof value[0] === "string") {
      pathParams[key] = value[0];
    }
  }
  return pathParams;
}

function queryOf(url: string): Record<string, string> {
  const query: Record<string, string> = {};
  for (const [key, value] of new URL(url).searchParams) {
    if (query[key] === undefined) {
      query[key] = value;
    }
  }
  return query;
}
