import { expect, it } from "vitest";
import type { Problem } from "@/api/problem";
import { isSqlResult, resultStatus, runShortcutLabel, sqlProblemMessage } from "@/lib/sql/format";

it("narrows a run answer to a SQL result", () => {
  expect(isSqlResult({ columns: [], rows: [], truncated: false })).toBe(true);
  expect(
    isSqlResult({ columns: [{ name: "a", type: "Utf8" }], rows: [{ a: null }], truncated: true }),
  ).toBe(true);
  expect(isSqlResult("a,b")).toBe(false);
  expect(isSqlResult("")).toBe(false);
  expect(isSqlResult(undefined)).toBe(false);
  expect(isSqlResult(null)).toBe(false);
  expect(isSqlResult({ rows: [] })).toBe(false);
  expect(isSqlResult({ columns: [], rows: [] })).toBe(false);
  expect(isSqlResult({ columns: [], rows: [], truncated: "false" })).toBe(false);
  expect(isSqlResult({ columns: [], rows: [], truncated: 0 })).toBe(false);
  expect(isSqlResult({ columns: [], rows: ["a,b"], truncated: false })).toBe(false);
  expect(isSqlResult({ columns: [], rows: [null], truncated: false })).toBe(false);
  expect(isSqlResult({ columns: [], rows: [[]], truncated: false })).toBe(false);
});

it("shows the first field message for an invalid query, else the detail or title", () => {
  const invalid: Problem = {
    type: "about:blank",
    title: "Bad Request",
    status: 400,
    code: "invalid_filter",
    detail: "invalid query",
    errors: [{ field: "query", message: "m" }],
  };
  expect(sqlProblemMessage(invalid)).toBe("m");
  const timeout: Problem = {
    type: "about:blank",
    title: "Unprocessable Entity",
    status: 422,
    code: "query_timeout",
    detail: "d",
  };
  expect(sqlProblemMessage(timeout)).toBe("d");
  expect(sqlProblemMessage({ ...timeout, detail: undefined })).toBe("Unprocessable Entity");
});

it("formats the result status line", () => {
  expect(resultStatus(1, false, 0)).toBe("1 row · 0 ms");
  expect(resultStatus(5, false, 12)).toBe("5 rows · 12 ms");
  expect(resultStatus(10000, true, 3)).toBe("10000 rows (truncated) · 3 ms");
});

it("labels the Run shortcut for the platform", () => {
  expect(runShortcutLabel("Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)")).toBe("⌘↵");
  expect(runShortcutLabel(navigator.userAgent)).toBe("Ctrl↵");
  expect(runShortcutLabel("Mozilla/5.0 (X11; Linux x86_64)")).toBe("Ctrl↵");
});
