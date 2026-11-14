import { beforeEach, expect, it } from "vitest";
import {
  clearHistory,
  HISTORY_MAX,
  historyLabel,
  pushHistory,
  readHistory,
} from "@/lib/sql/history";

const REF = "acme/support-bot";
const T0 = "2026-01-01T00:00:00.000Z";

beforeEach(() => {
  localStorage.clear();
});

it("keeps the newest run first", () => {
  pushHistory(REF, { query: "A", params: {}, ranAt: T0, rows: 1 });
  pushHistory(REF, { query: "B", params: {}, ranAt: "2026-01-01T00:00:01.000Z" });
  expect(readHistory(REF).map((entry) => entry.query)).toEqual(["B", "A"]);
  expect(localStorage.getItem("llmobs.sql.history.acme/support-bot")).not.toBeNull();
});

it("replaces the newest entry when the same query and parameters run again", () => {
  pushHistory(REF, { query: "A", params: {}, ranAt: T0, rows: 1 });
  pushHistory(REF, { query: "B", params: {}, ranAt: "2026-01-01T00:00:01.000Z" });
  pushHistory(REF, { query: "B", params: {}, ranAt: "2026-01-01T00:00:02.000Z" });
  const entries = readHistory(REF);
  expect(entries).toHaveLength(2);
  expect(entries[0]).toEqual({ query: "B", params: {}, ranAt: "2026-01-01T00:00:02.000Z" });
  pushHistory(REF, { query: "B", params: { a: "1" }, ranAt: "2026-01-01T00:00:03.000Z" });
  expect(readHistory(REF)).toHaveLength(3);
});

it("keeps at most fifty runs", () => {
  for (let index = 0; index < 55; index += 1) {
    pushHistory(REF, { query: `Q${index}`, params: {}, ranAt: T0 });
  }
  const entries = readHistory(REF);
  expect(entries).toHaveLength(HISTORY_MAX);
  expect(entries[0]?.query).toBe("Q54");
});

it("reads malformed storage as no history and skips broken entries", () => {
  localStorage.setItem("llmobs.sql.history.acme/support-bot", '"x"');
  expect(readHistory(REF)).toEqual([]);
  localStorage.setItem("llmobs.sql.history.acme/support-bot", '{"a":1}');
  expect(readHistory(REF)).toEqual([]);
  localStorage.setItem(
    "llmobs.sql.history.acme/support-bot",
    JSON.stringify([
      { params: {}, ranAt: T0 },
      { query: "K", ranAt: T0, params: { a: 1 } },
    ]),
  );
  expect(readHistory(REF)).toEqual([{ query: "K", params: {}, ranAt: T0 }]);
});

it("clears the history of one project", () => {
  pushHistory(REF, { query: "A", params: {}, ranAt: T0 });
  clearHistory(REF);
  expect(readHistory(REF)).toEqual([]);
});

it("labels a run by its first non-empty line", () => {
  expect(historyLabel("\n  SELECT 1  \nFROM t")).toBe("SELECT 1");
  const long = "x".repeat(70);
  expect(historyLabel(long)).toBe(`${"x".repeat(59)}…`);
});
