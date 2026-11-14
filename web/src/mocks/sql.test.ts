import { expect, it } from "vitest";
import requestDocument from "../../../fixtures/api/requests/sql-query.json";
import resultDocument from "../../../fixtures/api/static/sql-result.json";

const url = new URL("/api/v1/projects/acme%2Fsupport-bot/sql", window.location.origin);

function post(body: unknown) {
  return fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
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
