import { expect, it } from "vitest";
import { offsetOf, sqlErrorRange } from "@/lib/sql/position";

it("maps a 1-based line and column to an offset", () => {
  expect(offsetOf("SELECT 1\nFROM nme", 2, 6)).toBe(14);
  expect(offsetOf("abc", 1, 1)).toBe(0);
  expect(offsetOf("abc", 5, 1)).toBe(3);
  expect(offsetOf("abc", 0, 1)).toBe(0);
});

it("counts characters, not UTF-16 code units", () => {
  expect(offsetOf("'😀' x", 1, 5)).toBe(5);
});

it("underlines the word at the position, or one character", () => {
  expect(sqlErrorRange("SELECT 1\nFROM nme", { line: 2, column: 6 })).toEqual({
    from: 14,
    to: 17,
  });
  expect(sqlErrorRange("f(x)", { line: 1, column: 2 })).toEqual({ from: 1, to: 2 });
  expect(sqlErrorRange("ab", { line: 1, column: 9 })).toEqual({ from: 1, to: 2 });
  expect(sqlErrorRange("", { line: 1, column: 1 })).toBeNull();
});
