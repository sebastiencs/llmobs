import { expect, it } from "vitest";
import type { Schema } from "@/api/types";
import {
  belongsAtTop,
  firstPageOnly,
  firstPageWouldExceed,
  prependLiveTraces,
  pushBuffered,
  replaceLiveTrace,
  type TracePages,
} from "./live-list";

function trace(id: string, start: string): Schema<"TraceSummary"> {
  return {
    trace_id: id,
    name: id,
    start_time: start,
    end_time: start,
    duration_ns: 0,
    status: "ok",
    has_root: true,
    error_count: 0,
    llm_count: 0,
    span_count: 1,
    metadata: {},
    models: [],
    tags: [],
    scores: [],
  };
}

function list(ids: string[][]): TracePages {
  return {
    pages: ids.map((page, index) => ({
      data: page.map((id) => trace(id, `2026-01-01T00:00:0${index}.000000000Z`)),
      next_cursor: null,
    })),
    pageParams: ids.map(() => null),
  };
}

it("prepends onto the first page and replaces a known id wherever it sits", () => {
  const data = list([["a1000000000000000000000000000001"], ["a1000000000000000000000000000002"]]);
  const added = trace("c1000000000000000000000000000001", "2026-11-11T21:00:00.000000000Z");
  const prepended = prependLiveTraces(data, [added]);
  expect(prepended.pages[0]?.data.map((row) => row.trace_id)).toEqual([
    added.trace_id,
    "a1000000000000000000000000000001",
  ]);
  expect(prepended.pages[1]?.data).toHaveLength(1);

  const row = data.pages[1]?.data[0];
  if (!row) {
    throw new Error("missing second page");
  }
  const replaced = replaceLiveTrace(data, { ...row, name: "renamed" });
  expect(replaced.pages[1]?.data[0]?.name).toBe("renamed");
  expect(replaced.pages[0]).toBe(data.pages[0]);
});

it("treats an empty page as the top and an older trace as not", () => {
  const empty: TracePages = { pages: [{ data: [], next_cursor: null }], pageParams: [null] };
  const arriving = trace("c1000000000000000000000000000001", "2026-11-11T21:00:00.000000000Z");
  expect(belongsAtTop(empty, arriving)).toBe(true);
  const filled = prependLiveTraces(empty, [arriving]);
  const older = trace("c1000000000000000000000000000002", "2026-01-01T00:00:00.000000000Z");
  expect(belongsAtTop(filled, older)).toBe(false);
  expect(belongsAtTop(filled, arriving)).toBe(true);
});

it("drops every page after the first on resync", () => {
  const data = list([["a1000000000000000000000000000001"], ["a1000000000000000000000000000002"]]);
  const first = firstPageOnly(data);
  expect(first.pages).toHaveLength(1);
  expect(firstPageOnly(first)).toBe(first);
});

it("pushes onto the paused buffer and drops the oldest past the cap", () => {
  const older = trace("c1000000000000000000000000000001", "2026-11-11T21:00:00.000000000Z");
  const newer = trace("c1000000000000000000000000000002", "2026-11-11T21:00:01.000000000Z");
  const newest = trace("c1000000000000000000000000000003", "2026-11-11T21:00:02.000000000Z");
  const room = pushBuffered([older], newer, 2);
  expect(room).toEqual({ buffer: [newer, older], overflowed: false });
  const full = pushBuffered(room.buffer, newest, 2);
  expect(full.overflowed).toBe(true);
  expect(full.buffer.map((row) => row.trace_id)).toEqual([newest.trace_id, newer.trace_id]);
});

it("reports when adding rows would grow the first page past the cap", () => {
  const data = list([["a1000000000000000000000000000001", "a1000000000000000000000000000002"]]);
  expect(firstPageWouldExceed(data, 0, 2)).toBe(false);
  expect(firstPageWouldExceed(data, 1, 2)).toBe(true);
  const none: TracePages = { pages: [], pageParams: [] };
  expect(firstPageWouldExceed(none, 1, 0)).toBe(true);
});
