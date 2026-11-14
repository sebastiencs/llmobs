import { expect, it } from "vitest";
import { parseSqlSearch, withoutView, withSqlRange } from "@/lib/search/sql";

const A = "2026-11-13T10:00:00.000Z";
const B = "2026-11-13T11:00:00.000Z";
const V = "01900000-0000-7000-8000-000000000211";

it("keeps a valid preset and an absolute pair, and drops the rest", () => {
  expect(parseSqlSearch({ range: "1h" })).toEqual({ range: "1h" });
  expect(parseSqlSearch({ from: A, to: B })).toEqual({ from: A, to: B });
  expect(parseSqlSearch({ range: "2h" })).toEqual({});
  expect(parseSqlSearch({ from: B, to: A })).toEqual({});
  expect(parseSqlSearch({ from: A })).toEqual({});
  expect(parseSqlSearch({ view: "x" })).toEqual({});
});

it("keeps a saved query id that is a UUID", () => {
  expect(parseSqlSearch({ view: V })).toEqual({ view: V });
  expect(parseSqlSearch({ range: "1h", view: V })).toEqual({ range: "1h", view: V });
});

it("replaces the range and keeps other keys", () => {
  expect(withSqlRange({ range: "1h" }, { kind: "absolute", from: A, to: B })).toEqual({
    from: A,
    to: B,
  });
  expect(withSqlRange({ from: A, to: B }, { kind: "preset", preset: "7d" })).toEqual({
    range: "7d",
  });
  expect(withSqlRange({ range: "1h", view: V }, { kind: "preset", preset: "7d" })).toEqual({
    view: V,
    range: "7d",
  });
});

it("removes the saved query id and keeps the range", () => {
  expect(withoutView({ range: "1h", view: V })).toEqual({ range: "1h" });
});
