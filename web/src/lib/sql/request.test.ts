import { expect, it } from "vitest";
import { sqlRunBody } from "@/lib/sql/request";

it("sends only the query and the range", () => {
  expect(
    sqlRunBody({
      query: "SELECT 1",
      from: "2026-11-13T10:00:00.000Z",
      to: "2026-11-13T11:00:00.000Z",
    }),
  ).toEqual({
    query: "SELECT 1",
    from: "2026-11-13T10:00:00.000Z",
    to: "2026-11-13T11:00:00.000Z",
  });
});
