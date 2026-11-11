import { expect, it } from "vitest";
import { parseTracesSearch } from "./traces";

it("keeps valid trace search and drops the rest", () => {
  expect(
    parseTracesSearch({
      sort: "duration",
      order: "asc",
      trace: "a".repeat(32),
      span: "b".repeat(16),
      extra: "nope",
    }),
  ).toEqual({
    sort: "duration",
    order: "asc",
    trace: "a".repeat(32),
    span: "b".repeat(16),
  });
  expect(parseTracesSearch({ sort: "name", trace: "A".repeat(32), span: "zz" })).toEqual({});
});

it("keeps known column ids and score columns, and drops the rest", () => {
  expect(
    parseTracesSearch({
      cols: "nope, env, score:helpfulness, env, score:",
    }),
  ).toEqual({ cols: "env,score:helpfulness" });
  expect(parseTracesSearch({ cols: "nope" })).toEqual({});
});

it("keeps a filter and drops an empty or oversized one", () => {
  expect(parseTracesSearch({ filter: " status = 'error' " })).toEqual({
    filter: "status = 'error'",
  });
  expect(parseTracesSearch({ filter: "   " })).toEqual({});
  expect(parseTracesSearch({ filter: "x".repeat(4097) })).toEqual({});
});

it("keeps a saved-view id", () => {
  const id = "01900000-0000-7000-8000-00000000021a";
  expect(parseTracesSearch({ view: id })).toEqual({ view: id });
  expect(parseTracesSearch({ view: id.toUpperCase() })).toEqual({});
  expect(parseTracesSearch({ view: "errors" })).toEqual({});
});
