import { expect, it } from "vitest";
import { ancestorPaths, jsonRows } from "@/lib/json-rows";

it("flattens nested objects and arrays in pre-order", () => {
  const rows = jsonRows({ a: 1, b: ["x", { c: true }] });
  expect(rows.map((row) => [row.depth, row.key, row.kind, row.childCount, row.end])).toEqual([
    [0, "", "object", 2, 6],
    [1, "a", "number", 0, 2],
    [1, "b", "array", 2, 6],
    [2, "0", "string", 0, 4],
    [2, "1", "object", 1, 6],
    [3, "c", "boolean", 0, 6],
  ]);
  expect(rows[1]?.preview).toBe("1");
  expect(rows[3]?.preview).toBe("x");
  expect(rows[5]?.preview).toBe("true");
  expect(rows[2]?.preview).toBe("[…] 2 items");
  expect(ancestorPaths(rows[5]?.path ?? "")).toEqual(["", "b", "b\u00001"]);
});

it("keeps a long string whole", () => {
  const text = "a".repeat(500);
  const rows = jsonRows({ note: text });
  expect(rows[1]?.preview).toBe(text);
});

it("walks a depth of 5000 without throwing", () => {
  let value: unknown = "leaf";
  for (let depth = 0; depth < 5000; depth++) {
    value = [value];
  }
  const rows = jsonRows(value);
  expect(rows).toHaveLength(5001);
  expect(rows[0]?.kind).toBe("array");
  expect(rows[0]?.depth).toBe(0);
  expect(rows[5000]?.kind).toBe("string");
  expect(rows[5000]?.depth).toBe(5000);
  expect(rows[5000]?.preview).toBe("leaf");
});
