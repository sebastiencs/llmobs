import { expect, it } from "vitest";
import { csvOf, negotiate } from "@/mocks/sql";
import requestDocument from "../../../fixtures/api/requests/sql-query.json";
import resultDocument from "../../../fixtures/api/static/sql-result.json";

const url = new URL("/api/v1/projects/acme%2Fsupport-bot/sql", window.location.origin);

function post(body: unknown, accept?: string) {
  return fetch(url, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      ...(accept ? { Accept: accept } : {}),
    },
    body: JSON.stringify(body),
  });
}

it("answers the example request with the example result", async () => {
  const response = await post(requestDocument);
  expect(response.status).toBe(200);
  expect(await response.json()).toEqual(resultDocument);
});

it("refuses an empty query", async () => {
  const response = await post({ query: "  " });
  expect(response.status).toBe(400);
  expect(await response.json()).toMatchObject({ code: "bad_request" });
});

it("negotiates like the server", () => {
  expect(negotiate("text/csv")).toBe("csv");
  expect(negotiate("application/json, text/csv")).toBe("json");
  expect(negotiate("text/html;q=0.9, application/x-ndjson")).toBe("ndjson");
  expect(negotiate("*/*")).toBe("json");
  expect(negotiate("")).toBe("json");
  expect(negotiate("Application/Vnd.Apache.Parquet")).toBe("parquet");
});

it("renders CSV with quoting and CRLF", () => {
  expect(
    csvOf({
      columns: [{ name: "a" }, { name: "b" }],
      rows: [
        { a: "x,y", b: null },
        { a: 'q"q', b: { k: 1 } },
      ],
    }),
  ).toBe('a,b\r\n"x,y",\r\n"q""q","{""k"":1}"\r\n');
});

it("exports CSV with the negotiated Accept", async () => {
  const response = await post(requestDocument, "text/csv");
  expect(response.status).toBe(200);
  expect(response.headers.get("Content-Type")).toBe("text/csv");
  expect(response.headers.get("Content-Disposition")).toBe(
    'attachment; filename="query-20260101T100000Z.csv"',
  );
  const text = await response.text();
  const firstLine = text.split("\r\n")[0];
  expect(firstLine).toBe(resultDocument.columns.map((column) => column.name).join(","));
});

it("exports NDJSON with the negotiated Accept", async () => {
  const response = await post(requestDocument, "application/x-ndjson");
  expect(response.headers.get("Content-Type")).toBe("application/x-ndjson");
  const lines = (await response.text()).split("\n").filter((line) => line.length > 0);
  expect(lines).toHaveLength(resultDocument.rows.length);
  for (const [index, line] of lines.entries()) {
    expect(JSON.parse(line)).toEqual(resultDocument.rows[index]);
  }
});

it("exports Parquet with the negotiated Accept", async () => {
  const response = await post(requestDocument, "application/vnd.apache.parquet");
  expect(response.headers.get("Content-Type")).toBe("application/vnd.apache.parquet");
  expect(new TextDecoder().decode(await response.arrayBuffer())).toBe("PAR1PAR1");
});
