import { expect, it } from "vitest";
import { SQL_EXAMPLES } from "@/lib/sql/examples";
import { sqlParams } from "@/lib/sql/params";
import schemaFixture from "../../../../fixtures/api/static/schema.json";

const EXPECTED_NAMES = [
  "Cost per model per day",
  "LLM latency percentiles per model",
  "Traces with the most tool calls",
  "Average judge score per release",
  "Users by spend",
];

it("ships the five examples of spec 05, in order", () => {
  expect(SQL_EXAMPLES.map((example) => example.name)).toEqual(EXPECTED_NAMES);
});

it("writes each example as a plain SELECT without parameters", () => {
  for (const example of SQL_EXAMPLES) {
    expect(sqlParams(example.query), example.name).toEqual([]);
    expect(example.query.startsWith("SELECT"), example.name).toBe(true);
  }
});

it("reads only columns the schema lists", () => {
  const columnsOf = (view: string) =>
    schemaFixture.views.find((item) => item.name === view)?.columns.map((column) => column.name) ??
    [];
  const reads: [string, string, string[]][] = [
    ["Cost per model per day", "spans", ["start_time", "model", "cost_total", "span_kind"]],
    ["LLM latency percentiles per model", "spans", ["model", "duration_ms", "span_kind"]],
    ["Traces with the most tool calls", "spans", ["trace_id", "span_kind"]],
    ["Average judge score per release", "scores", ["trace_id", "name", "value_num", "source"]],
    ["Average judge score per release", "traces", ["trace_id", "release"]],
    ["Users by spend", "traces", ["user_id", "cost_total"]],
  ];
  for (const [example, view, columns] of reads) {
    const available = columnsOf(view);
    for (const column of columns) {
      expect(available, `${example}: ${view}.${column}`).toContain(column);
    }
  }
});
