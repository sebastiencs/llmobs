import { expect, it } from "vitest";
import {
  type Chip,
  type ChipValue,
  type CompareOp,
  type Field,
  parseChip,
  serializeChip,
  serializeFilter,
  splitConjuncts,
  tokenize,
} from "@/lib/dsl/dsl";

const cases: { name: string; input: string; chips: Chip[]; canonical: string }[] = [
  { name: "empty", input: "", chips: [], canonical: "" },
  { name: "whitespace", input: "   \n", chips: [], canonical: "" },
  {
    name: "compare string",
    input: "name = 'refund'",
    chips: [compare("name", "=", str("refund"))],
    canonical: "name = 'refund'",
  },
  {
    name: "compare number",
    input: "cost_total > 0.10",
    chips: [compare("cost_total", ">", num(0.1, "0.10"))],
    canonical: "cost_total > 0.10",
  },
  {
    name: "not equal",
    input: "status != 'error'",
    chips: [compare("status", "!=", str("error"))],
    canonical: "status != 'error'",
  },
  {
    name: "diamond operator",
    input: "status <> 'error'",
    chips: [compare("status", "!=", str("error"))],
    canonical: "status != 'error'",
  },
  {
    name: "less or equal",
    input: "duration <= 10",
    chips: [compare("duration", "<=", num(10, "10"))],
    canonical: "duration <= 10",
  },
  {
    name: "greater or equal",
    input: "duration >= 10",
    chips: [compare("duration", ">=", num(10, "10"))],
    canonical: "duration >= 10",
  },
  {
    name: "less than",
    input: "duration < 10",
    chips: [compare("duration", "<", num(10, "10"))],
    canonical: "duration < 10",
  },
  {
    name: "like",
    input: "input LIKE '%a%'",
    chips: [compare("input", "LIKE", str("%a%"))],
    canonical: "input LIKE '%a%'",
  },
  {
    name: "ilike",
    input: "input ilike '%refund%'",
    chips: [compare("input", "ILIKE", str("%refund%"))],
    canonical: "input ILIKE '%refund%'",
  },
  {
    name: "not like",
    input: "input NOT LIKE '%a%'",
    chips: [compare("input", "NOT LIKE", str("%a%"))],
    canonical: "input NOT LIKE '%a%'",
  },
  {
    name: "not ilike",
    input: "output NOT ILIKE '%a%'",
    chips: [compare("output", "NOT ILIKE", str("%a%"))],
    canonical: "output NOT ILIKE '%a%'",
  },
  {
    name: "is null",
    input: "user_id IS NULL",
    chips: [{ kind: "null", field: { column: "user_id" }, negated: false }],
    canonical: "user_id IS NULL",
  },
  {
    name: "is not null",
    input: "Name is not null",
    chips: [{ kind: "null", field: { column: "name" }, negated: true }],
    canonical: "name IS NOT NULL",
  },
  {
    name: "in list",
    input: "status IN ('a', 2)",
    chips: [
      { kind: "in", field: { column: "status" }, negated: false, values: [str("a"), num(2, "2")] },
    ],
    canonical: "status IN ('a', 2)",
  },
  {
    name: "not in",
    input: "status NOT IN ('error')",
    chips: [{ kind: "in", field: { column: "status" }, negated: true, values: [str("error")] }],
    canonical: "status NOT IN ('error')",
  },
  {
    name: "in booleans",
    input: "flag IN (true, FALSE)",
    chips: [
      {
        kind: "in",
        field: { column: "flag" },
        negated: false,
        values: [
          { kind: "boolean", value: true },
          { kind: "boolean", value: false },
        ],
      },
    ],
    canonical: "flag IN (TRUE, FALSE)",
  },
  {
    name: "contains one",
    input: "tags @> ['beta']",
    chips: [{ kind: "contains", field: { column: "tags" }, values: ["beta"] }],
    canonical: "tags @> ['beta']",
  },
  {
    name: "contains many",
    input: "tags @> ['a', 'b']",
    chips: [{ kind: "contains", field: { column: "tags" }, values: ["a", "b"] }],
    canonical: "tags @> ['a', 'b']",
  },
  {
    name: "score",
    input: "score('hallucination') >= 0.5",
    chips: [
      {
        kind: "score",
        fn: "score",
        name: "hallucination",
        op: ">=",
        value: num(0.5, "0.5"),
      },
    ],
    canonical: "score('hallucination') >= 0.5",
  },
  {
    name: "score label",
    input: "score_label('mood') = 'happy'",
    chips: [{ kind: "score", fn: "score_label", name: "mood", op: "=", value: str("happy") }],
    canonical: "score_label('mood') = 'happy'",
  },
  {
    name: "has score",
    input: "has_score('x')",
    chips: [{ kind: "has_score", name: "x", negated: false }],
    canonical: "has_score('x')",
  },
  {
    name: "not has score",
    input: "NOT has_score('x')",
    chips: [{ kind: "has_score", name: "x", negated: true }],
    canonical: "NOT has_score('x')",
  },
  {
    name: "metadata key",
    input: "metadata['customer'] = 'acme'",
    chips: [compare({ column: "metadata", key: "customer" }, "=", str("acme"))],
    canonical: "metadata['customer'] = 'acme'",
  },
  {
    name: "attributes key",
    input: "ATTRIBUTES['k'] = 1",
    chips: [compare({ column: "attributes", key: "k" }, "=", num(1, "1"))],
    canonical: "attributes['k'] = 1",
  },
  {
    name: "resource attributes",
    input: "resource_attributes['service.name'] = 'api'",
    chips: [compare({ column: "resource_attributes", key: "service.name" }, "=", str("api"))],
    canonical: "resource_attributes['service.name'] = 'api'",
  },
  {
    name: "doubled quotes",
    input: "name = 'it''s'",
    chips: [compare("name", "=", str("it's"))],
    canonical: "name = 'it''s'",
  },
  {
    name: "exponent",
    input: "cost = 5e9",
    chips: [compare("cost", "=", num(5e9, "5e9"))],
    canonical: "cost = 5e9",
  },
  {
    name: "negative number",
    input: "duration < -2.5",
    chips: [compare("duration", "<", num(-2.5, "-2.5"))],
    canonical: "duration < -2.5",
  },
  {
    name: "negative with a space",
    input: "duration < - 2",
    chips: [compare("duration", "<", num(-2, "-2"))],
    canonical: "duration < -2",
  },
  {
    name: "dotted number",
    input: "n = .5",
    chips: [compare("n", "=", num(0.5, ".5"))],
    canonical: "n = .5",
  },
  {
    name: "lowercase and",
    input: "name = 'a' and status = 'b'",
    chips: [compare("name", "=", str("a")), compare("status", "=", str("b"))],
    canonical: "name = 'a' AND status = 'b'",
  },
  {
    name: "mixed-case and",
    input: "name = 'a' And status = 'b'",
    chips: [compare("name", "=", str("a")), compare("status", "=", str("b"))],
    canonical: "name = 'a' AND status = 'b'",
  },
  {
    name: "and inside a string",
    input: "name = 'a AND b'",
    chips: [compare("name", "=", str("a AND b"))],
    canonical: "name = 'a AND b'",
  },
  {
    name: "and inside parentheses",
    input: "(name = 'a' AND status = 'ok')",
    chips: [{ kind: "raw", text: "(name = 'a' AND status = 'ok')" }],
    canonical: "(name = 'a' AND status = 'ok')",
  },
  {
    name: "or group then compare",
    input: "(a = 1 OR b = 2) AND c = 3",
    chips: [{ kind: "raw", text: "(a = 1 OR b = 2)" }, compare("c", "=", num(3, "3"))],
    canonical: "(a = 1 OR b = 2) AND c = 3",
  },
  {
    name: "unterminated string",
    input: "name = 'ab",
    chips: [{ kind: "raw", text: "name = 'ab" }],
    canonical: "name = 'ab",
  },
  {
    name: "interval",
    input: "now() - INTERVAL '1 hour'",
    chips: [{ kind: "raw", text: "now() - INTERVAL '1 hour'" }],
    canonical: "now() - INTERVAL '1 hour'",
  },
  {
    name: "true",
    input: "ok = true",
    chips: [compare("ok", "=", { kind: "boolean", value: true })],
    canonical: "ok = TRUE",
  },
  {
    name: "false",
    input: "ok != false",
    chips: [compare("ok", "!=", { kind: "boolean", value: false })],
    canonical: "ok != FALSE",
  },
  {
    name: "unwrap one pair",
    input: "(name = 'a')",
    chips: [compare("name", "=", str("a"))],
    canonical: "name = 'a'",
  },
  {
    name: "unwrap nested pairs",
    input: "((name = 'a'))",
    chips: [compare("name", "=", str("a"))],
    canonical: "name = 'a'",
  },
  {
    name: "quoted map key",
    input: "metadata['o''brien'] = 1",
    chips: [compare({ column: "metadata", key: "o'brien" }, "=", num(1, "1"))],
    canonical: "metadata['o''brien'] = 1",
  },
  {
    name: "score case",
    input: "SCORE('Hallucination') >= 0.5",
    chips: [
      {
        kind: "score",
        fn: "score",
        name: "Hallucination",
        op: ">=",
        value: num(0.5, "0.5"),
      },
    ],
    canonical: "score('Hallucination') >= 0.5",
  },
  {
    name: "has score case",
    input: "not HAS_SCORE('x')",
    chips: [{ kind: "has_score", name: "x", negated: true }],
    canonical: "NOT has_score('x')",
  },
  {
    name: "spaced in list",
    input: "status IN ( 'a' , 2 )",
    chips: [
      { kind: "in", field: { column: "status" }, negated: false, values: [str("a"), num(2, "2")] },
    ],
    canonical: "status IN ('a', 2)",
  },
  {
    name: "two scores",
    input: "score('a') > 1 AND score('b') < 2",
    chips: [
      { kind: "score", fn: "score", name: "a", op: ">", value: num(1, "1") },
      { kind: "score", fn: "score", name: "b", op: "<", value: num(2, "2") },
    ],
    canonical: "score('a') > 1 AND score('b') < 2",
  },
  {
    name: "contains case",
    input: "Tags @> ['Beta']",
    chips: [{ kind: "contains", field: { column: "tags" }, values: ["Beta"] }],
    canonical: "tags @> ['Beta']",
  },
  {
    name: "leading and trailing and",
    input: "AND name = 'a' AND",
    chips: [compare("name", "=", str("a"))],
    canonical: "name = 'a'",
  },
  {
    name: "top-level or is wrapped",
    input: "a = 1 OR b = 2",
    chips: [{ kind: "raw", text: "a = 1 OR b = 2" }],
    canonical: "(a = 1 OR b = 2)",
  },
  {
    name: "unknown map",
    input: "foo['k'] = 1",
    chips: [{ kind: "raw", text: "foo['k'] = 1" }],
    canonical: "foo['k'] = 1",
  },
  {
    name: "not around a group",
    input: "NOT (name = 'a')",
    chips: [{ kind: "raw", text: "NOT (name = 'a')" }],
    canonical: "NOT (name = 'a')",
  },
  {
    name: "column compared to a column",
    input: "name = other",
    chips: [{ kind: "raw", text: "name = other" }],
    canonical: "name = other",
  },
  {
    name: "column named score",
    input: "score = 1",
    chips: [compare("score", "=", num(1, "1"))],
    canonical: "score = 1",
  },
  {
    name: "signed exponent",
    input: "n = -5E2",
    chips: [compare("n", "=", num(-500, "-5E2"))],
    canonical: "n = -5E2",
  },
  {
    name: "and inside brackets",
    input: "tags @> ['a AND b']",
    chips: [{ kind: "contains", field: { column: "tags" }, values: ["a AND b"] }],
    canonical: "tags @> ['a AND b']",
  },
  {
    name: "non-finite number",
    input: "n = 1e999",
    chips: [{ kind: "raw", text: "n = 1e999" }],
    canonical: "n = 1e999",
  },
];

it.each(cases)("$name", ({ input, chips, canonical }) => {
  const parts = splitConjuncts(input);
  for (const part of parts) {
    expect(input.slice(part.start, part.end)).toBe(part.text);
    expect(part.text).toBe(part.text.trim());
  }
  const parsed = parts.map((part) => parseChip(part.text));
  expect(parsed).toEqual(chips);
  expect(serializeFilter(parsed)).toBe(canonical);
  for (const chip of parsed) {
    if (chip.kind === "raw") {
      continue;
    }
    expect(parseChip(serializeChip(chip))).toEqual(chip);
  }
});

it("keeps an unterminated string as one other token", () => {
  const tokens = tokenize("a = 'ab");
  expect(tokens.at(-1)).toMatchObject({ kind: "other", text: "'ab" });
});

it("records conjunct offsets", () => {
  const input = "  a = 1  AND  b = 2";
  expect(splitConjuncts(input)).toEqual([
    { text: "a = 1", start: 2, end: 7 },
    { text: "b = 2", start: 14, end: 19 },
  ]);
});

function compare(field: string | Field, op: CompareOp, value: ChipValue): Chip {
  return {
    kind: "compare",
    field: typeof field === "string" ? { column: field } : field,
    op,
    value,
  };
}

function str(value: string): { kind: "string"; value: string } {
  return { kind: "string", value };
}

function num(value: number, text: string): { kind: "number"; value: number; text: string } {
  return { kind: "number", value, text };
}
