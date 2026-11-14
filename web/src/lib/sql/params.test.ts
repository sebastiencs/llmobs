import { expect, it } from "vitest";
import {
  INT_RANGE,
  isServerTimestamp,
  paramValues,
  type SqlParam,
  sqlParams,
  usedParams,
} from "@/lib/sql/params";

it("lists distinct placeholders in order of first use", () => {
  expect(
    sqlParams("SELECT * FROM traces WHERE name = {name:String} AND cost_total > {min:float}"),
  ).toEqual([
    { name: "name", type: "String" },
    { name: "min", type: "Float" },
  ]);
  expect(sqlParams("{a:Int} + {a:Int} + {a:String}")).toEqual([{ name: "a", type: "Int" }]);
});

it("skips placeholders inside strings, identifiers, dollar quotes and comments", () => {
  const skipped = [
    "'{a:Int}'",
    "'it''s {a:Int}'",
    '"{a:Int}"',
    "`{a:Int}`",
    "-- {a:Int}",
    "/* {a:Int} */",
    "/* /* */ {a:Int} */",
    "$${a:Int}$$",
    `$q\${a:Int}$q$`,
    "'{a:Int}",
    "/* {a:Int}",
  ];
  for (const text of skipped) {
    expect(sqlParams(text), text).toEqual([]);
  }
});

it("counts a placeholder after a skipped region", () => {
  for (const text of ["'x' {b:Bool}", "/* /* */ */ {b:Bool}", "$$x$$ {b:Bool}", "-- c\n{b:Bool}"]) {
    expect(sqlParams(text), text).toEqual([{ name: "b", type: "Bool" }]);
  }
});

it("reads identifiers the way the server lexes them", () => {
  expect(sqlParams("{café:Int}")).toEqual([{ name: "café", type: "Int" }]);
  expect(sqlParams("{#a:Int} {@b:Int} {c$1:Int}").map((param) => param.name)).toEqual([
    "#a",
    "@b",
    "c$1",
  ]);
  expect(sqlParams("{select:Int}")).toEqual([{ name: "select", type: "Int" }]);
  expect(sqlParams("a$$b {x:Int}")).toEqual([{ name: "x", type: "Int" }]);
  expect(sqlParams("$1 {x:Int}")).toEqual([{ name: "x", type: "Int" }]);
  expect(sqlParams("😀{x:Int}")).toEqual([{ name: "x", type: "Int" }]);
  expect(sqlParams("'😀' {x:Int}")).toEqual([{ name: "x", type: "Int" }]);
});

it("ignores text that is not a placeholder", () => {
  for (const text of ["{ a:Int }", "{a :Int}", "{a: Int}", "{a:Date}", "{1a:Int}", "{a:Int"]) {
    expect(sqlParams(text), text).toEqual([]);
  }
});

it("converts typed text to JSON values by placeholder type", () => {
  const params: SqlParam[] = [
    { name: "n", type: "Int" },
    { name: "f", type: "Float" },
    { name: "b", type: "Bool" },
    { name: "t", type: "Timestamp" },
    { name: "e", type: "Timestamp" },
    { name: "s", type: "String" },
  ];
  expect(
    paramValues(params, {
      n: " 42 ",
      f: "-1.5e3",
      b: "true",
      t: "2026-01-01T00:00:00Z",
      e: "1767225600000",
      s: " x ",
    }),
  ).toEqual({
    values: { n: 42, f: -1500, b: true, t: "2026-01-01T00:00:00Z", e: 1767225600000, s: " x " },
    errors: {},
  });
});

it("reports invalid values per name", () => {
  const cases: [SqlParam, string, string][] = [
    [{ name: "n", type: "Int" }, "4.2", "Enter an integer"],
    [{ name: "n", type: "Int" }, "99999999999999999999", INT_RANGE],
    [{ name: "f", type: "Float" }, "", "Enter a number"],
    [{ name: "f", type: "Float" }, "Infinity", "Enter a number"],
    [{ name: "f", type: "Float" }, "0x10", "Enter a number"],
    [{ name: "f", type: "Float" }, ".5", "Enter a number"],
    [{ name: "b", type: "Bool" }, "", "Choose true or false"],
  ];
  for (const [param, typed, message] of cases) {
    expect(paramValues([param], { [param.name]: typed }).errors, typed).toEqual({
      [param.name]: message,
    });
  }
});

it("sends an untyped String parameter as an empty string", () => {
  expect(paramValues([{ name: "s", type: "String" }], {})).toEqual({
    values: { s: "" },
    errors: {},
  });
});

it("accepts only the server's RFC 3339 timestamps", () => {
  for (const text of [
    "2026-01-01T00:00:00Z",
    "2026-01-01t00:00:00z",
    "2026-01-01T00:00:00.123456789+02:00",
  ]) {
    expect(isServerTimestamp(text), text).toBe(true);
  }
  for (const text of [
    "2026-01-01",
    "2026-01-01 00:00:00Z",
    "2026-01-01T00:00:00.1234567890Z",
    "2026-02-30T00:00:00Z",
    "2026-01-01T24:00:00Z",
    "2026-01-01T00:00:60Z",
    "2026-01-01T00:00:00+24:00",
  ]) {
    expect(isServerTimestamp(text), text).toBe(false);
  }
});

it("keeps the typed value of each parameter the query uses", () => {
  expect(usedParams("SELECT {a:Int}", { a: "1", b: "2" })).toEqual({ a: "1" });
  expect(usedParams("SELECT {a:Int}", {})).toEqual({ a: "" });
});
