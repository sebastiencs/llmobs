import { expect, it } from "vitest";
import {
  paramsKey,
  parseSavedState,
  sameSavedState,
  savedStateOf,
  searchOfSavedState,
} from "@/lib/sql/saved-state";

const V = "01900000-0000-7000-8000-000000000211";
const OLD = "01900000-0000-7000-8000-0000000002aa";
const A = "2026-01-01T00:00:00Z";
const B = "2026-01-02T00:00:00Z";

it("reads a stored state leniently", () => {
  expect(parseSavedState({ query: "Q", params: [] })).toEqual({ query: "Q", params: {} });
  expect(
    parseSavedState({ query: "Q", params: { a: 1, b: true, c: "x", d: null }, range: "7d" }),
  ).toEqual({ query: "Q", params: { a: "1", b: "true", c: "x" }, range: "7d" });
  expect(parseSavedState("nope")).toEqual({ query: "", params: {} });
  expect(parseSavedState({ query: "Q", params: {}, range: "1h", from: A, to: B })).toEqual({
    query: "Q",
    params: {},
    from: A,
    to: B,
  });
  expect(parseSavedState({ query: "Q", params: {}, range: "1h", from: B, to: A })).toEqual({
    query: "Q",
    params: {},
    range: "1h",
  });
  expect(parseSavedState({ query: "Q", params: {}, from: A })).toEqual({ query: "Q", params: {} });
});

it("stores the used parameters and the effective range", () => {
  const draft = { query: "SELECT {a:Int}", params: { a: "1", old: "x" } };
  expect(savedStateOf(draft, { range: "1h", view: V })).toEqual({
    query: "SELECT {a:Int}",
    params: { a: "1" },
    range: "1h",
  });
  expect(savedStateOf(draft, { range: "1h", from: A, to: B })).toEqual({
    query: "SELECT {a:Int}",
    params: { a: "1" },
    from: A,
    to: B,
  });
  expect(savedStateOf({ query: "SELECT 1", params: {} }, {})).toEqual({
    query: "SELECT 1",
    params: {},
    range: "24h",
  });
});

it("compares saved and current states", () => {
  const current = { query: "Q", params: { a: "1", b: "2" }, range: "1h" as const };
  expect(sameSavedState(current, { query: "Q", params: { b: "2", a: "1" }, range: "1h" })).toBe(
    true,
  );
  expect(sameSavedState(current, { query: "Q", params: { a: "1", b: "2" } })).toBe(true);
  expect(sameSavedState(current, { query: "Q", params: { a: "1", b: "2" }, range: "7d" })).toBe(
    false,
  );
  const pair = { query: "Q", params: {}, from: A, to: B };
  expect(sameSavedState(pair, { ...pair })).toBe(true);
  expect(sameSavedState({ query: "Q", params: {}, range: "24h" }, pair)).toBe(false);
  expect(sameSavedState(current, { query: "R", params: { a: "1", b: "2" } })).toBe(false);
});

it("opens a loaded state with its range, or the current one when it has none", () => {
  expect(searchOfSavedState({ query: "Q", params: {}, range: "7d" }, {}, V)).toEqual({
    range: "7d",
    view: V,
  });
  expect(searchOfSavedState({ query: "Q", params: {}, from: A, to: B }, {}, V)).toEqual({
    from: A,
    to: B,
    view: V,
  });
  expect(searchOfSavedState({ query: "Q", params: {} }, { range: "1h", view: OLD }, V)).toEqual({
    range: "1h",
    view: V,
  });
});

it("keys params independently of their order", () => {
  expect(paramsKey({ b: "2", a: "1" })).toBe(paramsKey({ a: "1", b: "2" }));
});
