import { expect, it } from "vitest";
import { formatSqlCell, sqlColumns } from "@/components/sql/result-grid";

it("formats one JSON cell for display", () => {
  expect(formatSqlCell(null)).toBe("NULL");
  expect(formatSqlCell(undefined)).toBe("NULL");
  expect(formatSqlCell("x")).toBe("x");
  expect(formatSqlCell(3.5)).toBe("3.5");
  expect(formatSqlCell(false)).toBe("false");
  expect(formatSqlCell({ k: "v" })).toBe('{"k":"v"}');
  expect(formatSqlCell([1, 2])).toBe("[1,2]");
});

it("builds one column per result column, aligned by type", () => {
  const columns = sqlColumns({
    columns: [
      { name: "a b", type: "Utf8" },
      { name: "n", type: "Int64" },
    ],
    rows: [],
    truncated: false,
  });
  expect(columns.map((column) => column.id)).toEqual(["c0", "c1"]);
  expect(columns.map((column) => column.header)).toEqual(["a b", "n"]);
  expect(columns.map((column) => column.meta?.align)).toEqual(["left", "right"]);
});
