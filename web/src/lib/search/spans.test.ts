import { expect, it } from "vitest";
import { parseSpansSearch } from "./spans";

it("keeps valid span search and drops the rest", () => {
  expect(
    parseSpansSearch({
      sort: "duration",
      order: "asc",
      trace: "a".repeat(32),
      span: "b".repeat(16),
      mode: "tree",
      live: "1",
      extra: "nope",
    }),
  ).toEqual({
    sort: "duration",
    order: "asc",
    trace: "a".repeat(32),
    span: "b".repeat(16),
  });
  expect(parseSpansSearch({ sort: "name", trace: "A".repeat(32), span: "zz" })).toEqual({});
});

it("keeps known column ids and drops the rest", () => {
  expect(parseSpansSearch({ cols: "nope, ttft, kind, ttft, score:helpfulness" })).toEqual({
    cols: "ttft,kind",
  });
  expect(parseSpansSearch({ cols: "nope" })).toEqual({});
});

it("keeps a filter and drops an empty or oversized one", () => {
  expect(parseSpansSearch({ filter: " status = 'error' " })).toEqual({
    filter: "status = 'error'",
  });
  expect(parseSpansSearch({ filter: "   " })).toEqual({});
  expect(parseSpansSearch({ filter: "x".repeat(4097) })).toEqual({});
});
