import { type HttpHandler, HttpResponse, http } from "msw";
import resultDocument from "../../../fixtures/api/static/sql-result.json";

export const SQL_PATH = "/api/v1/projects/:project/sql";

export type SqlFormat = "csv" | "ndjson" | "parquet" | "json";

const MEDIA: readonly [string, SqlFormat][] = [
  ["text/csv", "csv"],
  ["application/x-ndjson", "ndjson"],
  ["application/vnd.apache.parquet", "parquet"],
  ["application/json", "json"],
];

/** The server's rule (D-H-8): the first listed media range that is one of the four. */
export function negotiate(accept: string): SqlFormat {
  for (const part of accept.split(",")) {
    const media = part.split(";")[0]?.trim().toLowerCase() ?? "";
    const hit = MEDIA.find(([type]) => type === media);
    if (hit) {
      return hit[1];
    }
  }
  return "json";
}

/** RFC 4180 with CRLF; a field with `,`, `"`, CR or LF is quoted; NULL is empty. */
export function csvOf(result: {
  columns: { name: string }[];
  rows: Record<string, unknown>[];
}): string {
  const field = (value: unknown): string => {
    if (value === null || value === undefined) {
      return "";
    }
    const text =
      typeof value === "string"
        ? value
        : typeof value === "object"
          ? JSON.stringify(value)
          : String(value);
    return /[",\r\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
  };
  const lines = [result.columns.map((column) => field(column.name)).join(",")];
  for (const row of result.rows) {
    lines.push(result.columns.map((column) => field(row[column.name])).join(","));
  }
  return `${lines.join("\r\n")}\r\n`;
}

const STAMP = "query-20260101T100000Z";

function attachment(extension: string): string {
  return `attachment; filename="${STAMP}.${extension}"`;
}

/** A fresh `ArrayBuffer`, as `handlers.ts` does for binary fixtures. */
function parquetBytes(): ArrayBuffer {
  const bytes = new TextEncoder().encode("PAR1PAR1");
  const buffer = new ArrayBuffer(bytes.byteLength);
  new Uint8Array(buffer).set(bytes);
  return buffer;
}

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
    const rows: Record<string, unknown>[] = resultDocument.rows;
    const format = negotiate(request.headers.get("Accept") ?? "");
    if (format === "csv") {
      return new HttpResponse(csvOf({ columns: resultDocument.columns, rows }), {
        headers: { "Content-Type": "text/csv", "Content-Disposition": attachment("csv") },
      });
    }
    if (format === "ndjson") {
      const text = rows.map((row) => `${JSON.stringify(row)}\n`).join("");
      return new HttpResponse(text, {
        headers: {
          "Content-Type": "application/x-ndjson",
          "Content-Disposition": attachment("ndjson"),
        },
      });
    }
    if (format === "parquet") {
      return HttpResponse.arrayBuffer(parquetBytes(), {
        headers: {
          "Content-Type": "application/vnd.apache.parquet",
          "Content-Disposition": attachment("parquet"),
        },
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
