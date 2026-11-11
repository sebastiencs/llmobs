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
