import { expect, it } from "vitest";
import type { Schema } from "@/api/types";
import { CHART_MAX_POINTS, type ChartSpec, chartLabel, chartSpec } from "@/lib/sql/chart";

const TS = 'Timestamp(Nanosecond, Some("UTC"))';

function result(
  columns: { name: string; type: string }[],
  rows: Record<string, unknown>[],
): Schema<"SqlQueryResult"> {
  return { columns, rows: rows as Schema<"SqlQueryResult">["rows"], truncated: false };
}

function specOf(document: Schema<"SqlQueryResult">): ChartSpec {
  const spec = chartSpec(document);
  if (spec === null) {
    throw new Error("expected a chart");
  }
  return spec;
}

it("draws a line over the first timestamp column, sorted by time", () => {
  const spec = specOf(
    result(
      [
        { name: "day", type: TS },
        { name: "cost", type: "Float64" },
      ],
      [
        { day: "2026-01-02T00:00:00.000000000Z", cost: 2 },
        { day: "2026-01-01T00:00:00.000000000Z", cost: 1 },
      ],
    ),
  );
  expect(spec.kind).toBe("line");
  expect(spec.x).toEqual({ name: "day", kind: "timestamp" });
  expect(spec.series).toEqual(["cost"]);
  expect(spec.data).toEqual([
    { __x: "2026-01-01T00:00:00.000000000Z", cost: 1 },
    { __x: "2026-01-02T00:00:00.000000000Z", cost: 2 },
  ]);
  expect(spec.clipped).toBe(false);
  expect(chartLabel(spec)).toBe("Line chart of cost by day");
});

it("pivots a single numeric column by a text column", () => {
  const d1 = "2026-01-01T00:00:00.000000000Z";
  const d2 = "2026-01-02T00:00:00.000000000Z";
  const spec = specOf(
    result(
      [
        { name: "day", type: TS },
        { name: "model", type: "Utf8" },
        { name: "cost", type: "Float64" },
      ],
      [
        { day: d1, model: "a", cost: 1 },
        { day: d1, model: "b", cost: 2 },
        { day: d2, model: "a", cost: 3 },
      ],
    ),
  );
  expect(spec).toEqual({
    kind: "line",
    x: { name: "day", kind: "timestamp" },
    series: ["a", "b"],
    pivot: { column: "model", measure: "cost" },
    data: [
      { __x: d1, a: 1, b: 2 },
      { __x: d2, a: 3 },
    ],
    clipped: false,
  });
  expect(chartLabel(spec)).toBe("Line chart of cost by day, one series per model");
});

it("does not pivot with two numeric columns", () => {
  const spec = specOf(
    result(
      [
        { name: "day", type: TS },
        { name: "model", type: "Utf8" },
        { name: "a", type: "Int64" },
        { name: "b", type: "Int64" },
      ],
      [{ day: "2026-01-01T00:00:00.000000000Z", model: "m", a: 1, b: 2 }],
    ),
  );
  expect(spec.pivot).toBeUndefined();
  expect(spec.series).toEqual(["a", "b"]);
});

it("does not pivot when the second numeric column is only dropped as unsafe", () => {
  const spec = specOf(
    result(
      [
        { name: "day", type: TS },
        { name: "model", type: "Utf8" },
        { name: "cost", type: "Float64" },
        { name: "__proto__", type: "Float64" },
      ],
      [{ day: "2026-01-01T00:00:00.000000000Z", model: "m", cost: 1, ["__proto__"]: 9 }],
    ),
  );
  expect(spec.pivot).toBeUndefined();
  expect(spec.series).toEqual(["cost"]);
  expect(spec.clipped).toBe(true);
});

it("keeps a dotted pivot value as its own series key", () => {
  const spec = specOf(
    result(
      [
        { name: "day", type: TS },
        { name: "model", type: "Utf8" },
        { name: "cost", type: "Float64" },
      ],
      [{ day: "2026-01-01T00:00:00.000000000Z", model: "gpt-4.1", cost: 1 }],
    ),
  );
  expect(spec.pivot).toEqual({ column: "model", measure: "cost" });
  expect(spec.series).toEqual(["gpt-4.1"]);
  expect(spec.data).toEqual([{ __x: "2026-01-01T00:00:00.000000000Z", "gpt-4.1": 1 }]);
});

it("draws a bar chart over a text column", () => {
  const spec = specOf(
    result(
      [
        { name: "name", type: "Utf8" },
        { name: "traces", type: "Int64" },
        { name: "cost", type: "Float64" },
      ],
      [
        { name: "a", traces: 1, cost: 0.5 },
        { name: "b", traces: 2, cost: 1.5 },
      ],
    ),
  );
  expect(spec.kind).toBe("bar");
  expect(spec.series).toEqual(["traces", "cost"]);
  expect(chartLabel(spec)).toBe("Bar chart of traces, cost by name");
});

it("returns null without an x or a numeric column", () => {
  expect(chartSpec(result([{ name: "a", type: "Utf8" }], [{ a: "x" }]))).toBeNull();
  expect(chartSpec(result([{ name: "n", type: "Int64" }], [{ n: 1 }]))).toBeNull();
});

it("clips series, points and pivot values", () => {
  const seven = specOf(
    result(
      [
        { name: "name", type: "Utf8" },
        ...["a", "b", "c", "d", "e", "f", "g"].map((name) => ({ name, type: "Int64" })),
      ],
      [{ name: "x", a: 1, b: 1, c: 1, d: 1, e: 1, f: 1, g: 1 }],
    ),
  );
  expect(seven.series).toEqual(["a", "b", "c", "d", "e"]);
  expect(seven.clipped).toBe(true);

  const many = specOf(
    result(
      [
        { name: "day", type: TS },
        { name: "n", type: "Int64" },
      ],
      Array.from({ length: 1001 }, (_, index) => ({
        day: new Date(Date.UTC(2026, 0, 1, 0, 0, index)).toISOString(),
        n: index,
      })),
    ),
  );
  expect(many.data).toHaveLength(CHART_MAX_POINTS);
  expect(many.clipped).toBe(true);

  const pivot = specOf(
    result(
      [
        { name: "day", type: TS },
        { name: "model", type: "Utf8" },
        { name: "cost", type: "Float64" },
      ],
      ["a", "b", "c", "d", "e", "f"].map((model, index) => ({
        day: "2026-01-01T00:00:00.000000000Z",
        model,
        cost: index,
      })),
    ),
  );
  expect(pivot.series).toEqual(["a", "b", "c", "d", "e"]);
  expect(pivot.clipped).toBe(true);
});

it("reads decimal strings and keeps null", () => {
  const spec = specOf(
    result(
      [
        { name: "day", type: TS },
        { name: "amount", type: "Decimal128(38, 18)" },
      ],
      [
        { day: "2026-01-01T00:00:00.000000000Z", amount: "0.25" },
        { day: "2026-01-02T00:00:00.000000000Z", amount: null },
      ],
    ),
  );
  expect(spec.data).toEqual([
    { __x: "2026-01-01T00:00:00.000000000Z", amount: 0.25 },
    { __x: "2026-01-02T00:00:00.000000000Z", amount: null },
  ]);
});

it("treats a date column as a date axis and sorts it", () => {
  const spec = specOf(
    result(
      [
        { name: "d", type: "Date32" },
        { name: "n", type: "Int64" },
      ],
      [
        { d: "2026-01-02", n: 2 },
        { d: "2026-01-01", n: 1 },
      ],
    ),
  );
  expect(spec.x).toEqual({ name: "d", kind: "date" });
  expect(spec.kind).toBe("line");
  expect(spec.data.map((point) => point.n)).toEqual([1, 2]);
});

it("sorts an unparseable timestamp last", () => {
  const spec = specOf(
    result(
      [
        { name: "day", type: TS },
        { name: "n", type: "Int64" },
      ],
      [
        { day: "not a time", n: 3 },
        { day: "2026-01-01T00:00:00.000000000Z", n: 1 },
      ],
    ),
  );
  expect(spec.data.map((point) => point.n)).toEqual([1, 3]);
});

it("keeps odd names as series keys and drops unsafe ones", () => {
  const odd = specOf(
    result(
      [
        { name: "day", type: TS },
        { name: "model", type: "Utf8" },
        { name: "cost", type: "Float64" },
      ],
      [{ day: "2026-01-01T00:00:00.000000000Z", model: "a;}b{", cost: 1 }],
    ),
  );
  expect(odd.series).toEqual(["a;}b{"]);
  expect(odd.data).toEqual([{ __x: "2026-01-01T00:00:00.000000000Z", "a;}b{": 1 }]);

  const plainX = specOf(
    result(
      [
        { name: "day", type: TS },
        { name: "x", type: "Int64" },
      ],
      [{ day: "2026-01-01T00:00:00.000000000Z", x: 4 }],
    ),
  );
  expect(plainX.series).toEqual(["x"]);
  expect(plainX.data).toEqual([{ __x: "2026-01-01T00:00:00.000000000Z", x: 4 }]);

  const unsafe = specOf(
    result(
      [
        { name: "day", type: TS },
        { name: "model", type: "Utf8" },
        { name: "cost", type: "Float64" },
      ],
      [
        { day: "2026-01-01T00:00:00.000000000Z", model: "__proto__", cost: 1 },
        { day: "2026-01-01T00:00:00.000000000Z", model: "safe", cost: 2 },
      ],
    ),
  );
  expect(unsafe.series).toEqual(["safe"]);
  expect(unsafe.clipped).toBe(true);

  const nullPivot = specOf(
    result(
      [
        { name: "day", type: TS },
        { name: "model", type: "Utf8" },
        { name: "cost", type: "Float64" },
      ],
      [{ day: "2026-01-01T00:00:00.000000000Z", model: null, cost: 1 }],
    ),
  );
  expect(nullPivot.series).toEqual(["NULL"]);
  expect(nullPivot.data).toEqual([{ __x: "2026-01-01T00:00:00.000000000Z", NULL: 1 }]);
});
