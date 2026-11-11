import { expect, it } from "vitest";
import { parseSessionPageSearch, parseSessionsSearch } from "./sessions";

it("keeps valid session search and drops the rest", () => {
  expect(
    parseSessionsSearch({
      sort: "cost_total",
      order: "asc",
      eval: "1",
      extra: "nope",
      search: "refund",
    }),
  ).toEqual({
    sort: "cost_total",
    order: "asc",
    eval: 1,
  });
  expect(parseSessionsSearch({ sort: "duration" })).toEqual({});
});

it("keeps known column ids and score columns, and drops the rest", () => {
  expect(parseSessionsSearch({ cols: "nope, user, score:resolved, user, score:" })).toEqual({
    cols: "user,score:resolved",
  });
  expect(parseSessionsSearch({ cols: "nope" })).toEqual({});
});

it("keeps a trace and the exclusion flags on the session page", () => {
  expect(
    parseSessionPageSearch({
      trace: "a".repeat(32),
      eval: 1,
      exp: "1",
      sort: "cost_total",
    }),
  ).toEqual({ trace: "a".repeat(32), eval: 1, exp: 1 });
  expect(parseSessionPageSearch({ trace: "A".repeat(32) })).toEqual({});
});
