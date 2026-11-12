import { expect, it } from "vitest";
import { parseTracePageSearch, parseTracesSearch, traceLiveReason } from "./traces";

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

it("keeps a span and a mode on the trace page", () => {
  expect(
    parseTracePageSearch({
      span: "b".repeat(16),
      mode: "transcript",
      trace: "a".repeat(32),
    }),
  ).toEqual({ span: "b".repeat(16), mode: "transcript" });
  expect(parseTracePageSearch({ span: "B".repeat(16), mode: "chat" })).toEqual({});
});

it("keeps live=1 and drops any other value", () => {
  expect(parseTracesSearch({ live: 1 })).toEqual({ live: 1 });
  expect(parseTracesSearch({ live: "1" })).toEqual({ live: 1 });
  expect(parseTracesSearch({ live: "yes" })).toEqual({});
  expect(parseTracesSearch({ live: 0 })).toEqual({});
});

it("allows live for a preset range sorted by start time, newest first", () => {
  expect(traceLiveReason({})).toBeUndefined();
  expect(traceLiveReason({ sort: "start_time", order: "desc" })).toBeUndefined();
  expect(traceLiveReason({ sort: "cost_total" })).toBe("Live follows start time, newest first");
  expect(traceLiveReason({ order: "asc" })).toBe("Live follows start time, newest first");
  expect(
    traceLiveReason({
      from: "2026-01-01T00:00:00Z",
      to: "2026-01-02T00:00:00Z",
      sort: "cost_total",
    }),
  ).toBe("Live needs a relative time range");
});

it("keeps the trace list mode", () => {
  expect(parseTracesSearch({ mode: "transcript", trace: "a".repeat(32) })).toEqual({
    mode: "transcript",
    trace: "a".repeat(32),
  });
  expect(parseTracesSearch({ mode: "tree" })).toEqual({ mode: "tree" });
  expect(parseTracesSearch({ mode: "chat" })).toEqual({});
});
