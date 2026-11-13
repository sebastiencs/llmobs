import { expect, it } from "vitest";
import { passwordProblems } from "./password-fields";

it("checks the length and the confirmation", () => {
  expect(passwordProblems("short", "short")).toEqual({
    password: "Use at least 10 characters.",
  });
  expect(passwordProblems("a".repeat(129), "a".repeat(129))).toEqual({
    password: "Use at most 128 characters.",
  });
  expect(passwordProblems("correct horse battery", "correct horse batter")).toEqual({
    confirm: "The passwords do not match.",
  });
  expect(passwordProblems("correct horse battery", "correct horse battery")).toEqual({});
  expect(passwordProblems("😀".repeat(10), "😀".repeat(10))).toEqual({});
  expect(passwordProblems(`${"é".repeat(9)}x`, `${"é".repeat(9)}x`)).toEqual({});
  expect(passwordProblems("é".repeat(9), "é".repeat(9))).toEqual({
    password: "Use at least 10 characters.",
  });
});
