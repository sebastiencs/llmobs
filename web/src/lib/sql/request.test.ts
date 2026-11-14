import { expect, it } from "vitest";
import { sqlRunBody } from "@/lib/sql/request";

const FROM = "2026-11-13T10:00:00.000Z";
const TO = "2026-11-13T11:00:00.000Z";

it("sends only the query and the range without parameters", () => {
  expect(sqlRunBody({ query: "SELECT 1", from: FROM, to: TO })).toEqual({
    query: "SELECT 1",
    from: FROM,
    to: TO,
  });
  expect(sqlRunBody({ query: "SELECT 1", from: FROM, to: TO, params: {} })).not.toHaveProperty(
    "params",
  );
});

it("sends typed parameter values", () => {
  expect(
    sqlRunBody({ query: "SELECT {n:Int}", from: FROM, to: TO, params: { n: 7, b: true } }),
  ).toEqual({ query: "SELECT {n:Int}", from: FROM, to: TO, params: { n: 7, b: true } });
});
