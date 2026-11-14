import { type HttpHandler, HttpResponse, http } from "msw";
import resultDocument from "../../../fixtures/api/static/sql-result.json";

export const SQL_PATH = "/api/v1/projects/:project/sql";

const EMPTY_QUERY = {
  type: "about:blank",
  title: "Bad Request",
  status: 400,
  code: "bad_request",
  detail: "query is empty",
};

/** `POST /sql`: the example result for any non-empty query; a 400 problem otherwise. */
export const sqlHandlers: HttpHandler[] = [
  http.post(SQL_PATH, async ({ request }) => {
    const body = await readJson(request);
    if (!body || typeof body.query !== "string" || body.query.trim().length === 0) {
      return HttpResponse.json(EMPTY_QUERY, {
        status: 400,
        headers: { "Content-Type": "application/problem+json" },
      });
    }
    return HttpResponse.json(resultDocument);
  }),
];

async function readJson(request: Request): Promise<Record<string, unknown> | undefined> {
  try {
    const body: unknown = await request.json();
    return typeof body === "object" && body !== null && !Array.isArray(body)
      ? (body as Record<string, unknown>)
      : undefined;
  } catch {
    return undefined;
  }
}
