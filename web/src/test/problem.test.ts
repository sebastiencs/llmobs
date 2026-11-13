import { expect, it } from "vitest";
import { problemResponse } from "./problem";

it("answers problem+json", async () => {
  const response = problemResponse(404, "not_found");
  expect(response.status).toBe(404);
  expect(response.headers.get("Content-Type")).toBe("application/problem+json");
  await expect(response.json()).resolves.toEqual({
    type: "about:blank",
    title: "Not Found",
    status: 404,
    code: "not_found",
  });
});

it("carries detail and errors", async () => {
  const response = problemResponse(400, "bad_request", {
    detail: "nope",
    errors: [{ field: "email", message: "invalid email" }],
  });
  const body = await response.json();
  expect(body.title).toBe("Bad Request");
  expect(body.detail).toBe("nope");
  expect(body.errors).toEqual([{ field: "email", message: "invalid email" }]);
});
