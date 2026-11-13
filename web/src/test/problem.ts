import { HttpResponse } from "msw";

const TITLES: Record<number, string> = {
  400: "Bad Request",
  401: "Unauthorized",
  403: "Forbidden",
  404: "Not Found",
  409: "Conflict",
  429: "Too Many Requests",
  500: "Internal Server Error",
  503: "Service Unavailable",
};

/** A problem+json response like the server's (spec 09 § Conventions). */
export function problemResponse(
  status: number,
  code: string,
  options: { detail?: string; errors?: { field: string; message: string }[]; title?: string } = {},
): Response {
  return HttpResponse.json(
    {
      type: "about:blank",
      title: options.title ?? TITLES[status] ?? "Error",
      status,
      code,
      ...(options.detail !== undefined ? { detail: options.detail } : {}),
      ...(options.errors !== undefined ? { errors: options.errors } : {}),
    },
    { status, headers: { "Content-Type": "application/problem+json" } },
  );
}
