import { CompletionContext } from "@codemirror/autocomplete";
import { EditorState } from "@codemirror/state";
import { expect, it } from "vitest";
import type { Schema } from "@/api/types";
import { appendFilter, completeFilter, errorMarkRange } from "@/components/filter-bar/logic";

const schema = {
  views: [
    {
      name: "traces",
      description: "Traces.",
      columns: [
        {
          name: "status",
          column_type: "string",
          nullable: false,
          description: "Span status.",
        },
        {
          name: "cost_total",
          column_type: "float",
          nullable: true,
          description: "Sum of span costs, USD.",
        },
        {
          name: "tags",
          column_type: "string_list",
          nullable: false,
          description: "Tags.",
        },
        {
          name: "metadata",
          column_type: "string_map",
          nullable: false,
          description: "Metadata.",
        },
      ],
    },
  ],
  functions: [
    {
      name: "score",
      signature: "score(name: string) -> float",
      description: "Latest numeric score.",
      in_filter: true,
      in_sql: false,
    },
    {
      name: "lower",
      signature: "lower(value: string) -> string",
      description: "Lowercase a string.",
      in_filter: true,
      in_sql: true,
    },
    {
      name: "now",
      signature: "now() -> timestamp",
      description: "Current time.",
      in_filter: false,
      in_sql: true,
    },
  ],
} as Schema<"SchemaResponse">;

function at(text: string) {
  return new CompletionContext(EditorState.create({ doc: text }), text.length, false);
}

function labels(text: string): string[] {
  return completeFilter(at(text), "traces", schema)?.options.map((option) => option.label) ?? [];
}

it("offers columns and in-filter functions from the schema", () => {
  const result = completeFilter(at("cos"), "traces", schema);
  const cost = result?.options.find((option) => option.label === "cost_total");
  expect(cost?.detail).toBe("float");
  expect(cost?.info).toBe("Sum of span costs, USD.");
  const score = completeFilter(at("sc"), "traces", schema)?.options.find(
    (option) => option.label === "score",
  );
  expect(score?.detail).toBe("score(name: string) -> float");
  expect(labels("sc")).not.toContain("now");
});

it("offers operators for the column before the cursor", () => {
  expect(labels("status ")).toEqual(expect.arrayContaining(["=", "LIKE", "IS NULL", "IN"]));
  expect(labels("status ")).not.toContain("@>");
  expect(labels("tags ")).toEqual(["@>"]);
  expect(labels("metadata[")).toEqual(["'"]);
});

it("marks a filter error from the reported column through the token", () => {
  expect(errorMarkRange("nme = 1", 1)).toEqual({ from: 0, to: 3 });
  expect(errorMarkRange("nme = 1", 4)).toEqual({ from: 3, to: 4 });
});

it("appends a conjunct and parenthesizes a top-level OR", () => {
  expect(appendFilter("", "x = 1")).toBe("x = 1");
  expect(appendFilter("  ", "x = 1")).toBe("x = 1");
  expect(appendFilter("a = 1", "b = 2")).toBe("a = 1 AND b = 2");
  expect(appendFilter("a = 1 OR b = 2", "status = 'error'")).toBe(
    "(a = 1 OR b = 2) AND status = 'error'",
  );
  expect(appendFilter("(a = 1 OR b = 2)", "c = 3")).toBe("(a = 1 OR b = 2) AND c = 3");
});
