import type { Completion, CompletionContext, CompletionResult } from "@codemirror/autocomplete";
import type { Problem } from "@/api/problem";
import type { Schema } from "@/api/types";
import {
  type Chip,
  type ChipValue,
  type CompareOp,
  type Field,
  parseChip,
  serializeChip,
  type Token,
  tokenize,
} from "@/lib/dsl/dsl";

type ColumnType = Schema<"ColumnType">;
type ViewName = Schema<"ViewName">;
type ColumnSchema = Schema<"ColumnSchema">;
type FunctionSchema = Schema<"FunctionSchema">;

const COMPARE_OPS = [
  "=",
  "!=",
  "<",
  "<=",
  ">",
  ">=",
  "LIKE",
  "ILIKE",
  "NOT LIKE",
  "NOT ILIKE",
] as const satisfies readonly CompareOp[];

const NULL_OPS = ["IS NULL", "IS NOT NULL"] as const;
const IN_OPS = ["IN", "NOT IN"] as const;
const SCORE_FNS = new Set(["score", "score_label", "has_score"]);
const SCORE_VIEWS = new Set<ViewName>(["traces", "spans", "sessions"]);
const MAP_COLUMNS = new Set(["metadata", "attributes", "resource_attributes"]);

export type Draft = {
  target: string;
  name: string;
  op: string;
  value: string;
};

export type TargetKind = ColumnType | "score" | "score_label" | "has_score";

/** Operators the popover and autocomplete offer for a column or score value. */
export function operatorsFor(type: ColumnType | "score" | "score_label"): readonly string[] {
  switch (type) {
    case "string":
    case "string_map":
    case "score_label":
      return [...COMPARE_OPS, ...NULL_OPS, ...IN_OPS];
    case "integer":
    case "float":
    case "timestamp":
    case "score":
      return ["=", "!=", "<", "<=", ">", ">=", ...NULL_OPS, ...IN_OPS];
    case "boolean":
      return ["=", "!=", ...NULL_OPS];
    case "string_list":
      return ["@>"];
    case "json":
      return ["=", "!=", ...NULL_OPS];
  }
}

/** Operators that still produce a recognised chip for a score call. */
export function chipOperators(type: TargetKind): readonly string[] {
  if (type === "has_score") {
    return ["has_score", "NOT has_score"];
  }
  if (type === "score") {
    return ["=", "!=", "<", "<=", ">", ">="];
  }
  if (type === "score_label") {
    return [...COMPARE_OPS];
  }
  if (type === "string_map") {
    return operatorsFor("string");
  }
  return operatorsFor(type);
}

export function needsValue(op: string): boolean {
  return op !== "IS NULL" && op !== "IS NOT NULL" && op !== "has_score" && op !== "NOT has_score";
}

export function draftFromChip(chip: Chip): Draft | null {
  switch (chip.kind) {
    case "raw":
      return null;
    case "compare":
      return {
        target: chip.field.column,
        name: keyOf(chip.field),
        op: chip.op,
        value: literalText(chip.value),
      };
    case "null":
      return {
        target: chip.field.column,
        name: keyOf(chip.field),
        op: chip.negated ? "IS NOT NULL" : "IS NULL",
        value: "",
      };
    case "in":
      return {
        target: chip.field.column,
        name: keyOf(chip.field),
        op: chip.negated ? "NOT IN" : "IN",
        value: chip.values.map(formatLiteral).join(", "),
      };
    case "contains":
      return {
        target: chip.field.column,
        name: keyOf(chip.field),
        op: "@>",
        value: chip.values.map((value) => formatLiteral({ kind: "string", value })).join(", "),
      };
    case "score":
      return { target: chip.fn, name: chip.name, op: chip.op, value: literalText(chip.value) };
    case "has_score":
      return {
        target: "has_score",
        name: chip.name,
        op: chip.negated ? "NOT has_score" : "has_score",
        value: "",
      };
  }
}

export function chipFromDraft(draft: Draft, type: TargetKind): Chip | null {
  if (draft.target === "has_score" || type === "has_score") {
    const name = draft.name.trim();
    if (name.length === 0) {
      return null;
    }
    return { kind: "has_score", name, negated: draft.op === "NOT has_score" };
  }
  if (draft.target === "score" || draft.target === "score_label") {
    const name = draft.name.trim();
    if (name.length === 0 || !isCompareOp(draft.op)) {
      return null;
    }
    const value = literal(draft.value, draft.target);
    if (!value) {
      return null;
    }
    return { kind: "score", fn: draft.target, name, op: draft.op, value };
  }
  const field = fieldOf(draft.target, draft.name, type);
  if (!field) {
    return null;
  }
  if (draft.op === "IS NULL" || draft.op === "IS NOT NULL") {
    return { kind: "null", field, negated: draft.op === "IS NOT NULL" };
  }
  if (draft.op === "IN" || draft.op === "NOT IN") {
    const values = inValues(draft.value);
    if (!values) {
      return null;
    }
    return { kind: "in", field, negated: draft.op === "NOT IN", values };
  }
  if (draft.op === "@>") {
    const values = containsValues(draft.value);
    if (!values) {
      return null;
    }
    return { kind: "contains", field, values };
  }
  if (!isCompareOp(draft.op)) {
    return null;
  }
  const value = literal(draft.value, type === "string_map" ? "string" : type);
  if (!value) {
    return null;
  }
  return { kind: "compare", field, op: draft.op, value };
}

export function stringEquals(column: string, value: string): string {
  return serializeChip({
    kind: "compare",
    field: { column },
    op: "=",
    value: { kind: "string", value },
  });
}

export function stringContains(column: string, value: string): string {
  return serializeChip({ kind: "contains", field: { column }, values: [value] });
}

export function appendFilter(current: string, conjunct: string): string {
  const base = current.trim();
  return base.length === 0 ? conjunct : `${base} AND ${conjunct}`;
}

export function filterMessage(problem: Problem): string {
  const field = problem.errors?.find((error) => error.field === "filter");
  return field?.message ?? problem.detail ?? problem.title;
}

/** Underline from the 1-based column to the end of that token, or one character. */
export function errorMarkRange(text: string, column: number): { from: number; to: number } | null {
  if (!Number.isInteger(column) || column < 1 || text.length === 0) {
    return null;
  }
  const index = column - 1;
  if (index >= text.length) {
    return { from: text.length - 1, to: text.length };
  }
  const token = tokenize(text).find((item) => index >= item.start && index < item.end);
  if (!token) {
    return { from: index, to: index + 1 };
  }
  return { from: index, to: token.end };
}

export function filterErrorRange(
  text: string,
  problem: Problem,
): { from: number; to: number } | null {
  const column = problem.errors?.find((error) => error.field === "filter")?.position?.column;
  return column === undefined ? null : errorMarkRange(text, column);
}

export function completeFilter(
  context: CompletionContext,
  view: ViewName,
  schema: Schema<"SchemaResponse"> | undefined,
): CompletionResult | null {
  const before = context.state.doc.sliceString(0, context.pos);
  if (/(?:^|[^A-Za-z0-9_])(?:metadata|attributes|resource_attributes)\[$/i.test(before)) {
    return { from: context.pos, options: [{ label: "'", detail: "string key", apply: "'" }] };
  }
  const columns = columnMap(schema, view);
  const word = context.matchBefore(/[A-Za-z_][A-Za-z0-9_]*|!=|<>|<=|>=|@>|[=!<>]/);
  const prefix = word?.text ?? "";
  const head = tokenize(context.state.doc.sliceString(0, word?.from ?? context.pos));
  const headLast = head.at(-1);
  if (headLast?.kind === "other" && headLast.text.startsWith("'")) {
    return null;
  }
  const where = slot(head, columns);
  if (where === "op") {
    const type = opType(head, columns);
    return listing(
      word?.from ?? context.pos,
      operatorsFor(type === "string_map" ? "string" : type)
        .filter((op) => starts(op, prefix))
        .map((op) => ({ label: op, apply: applyOperator(op) })),
      /^[A-Za-z!=<>@ ]*$/,
    );
  }
  if (where === "logic") {
    if (prefix.length === 0 && !context.explicit) {
      return null;
    }
    return listing(
      word?.from ?? context.pos,
      ["AND", "OR"].filter((word) => starts(word, prefix)).map(keywordOption),
      /^[A-Za-z]*$/,
    );
  }
  if (where !== "expr" || (prefix.length === 0 && !context.explicit)) {
    return null;
  }
  const options: Completion[] = [];
  for (const column of columns.values()) {
    if (starts(column.name, prefix)) {
      options.push({
        label: column.name,
        detail: column.column_type,
        info: column.description,
        boost: 2,
      });
    }
  }
  for (const fn of schema?.functions ?? []) {
    if (allowFunction(fn, view) && starts(fn.name, prefix)) {
      options.push({ label: fn.name, detail: fn.signature, info: fn.description, boost: 1 });
    }
  }
  for (const word of ["AND", "OR", "NOT"]) {
    if (starts(word, prefix)) {
      options.push(keywordOption(word));
    }
  }
  return listing(word?.from ?? context.pos, options, /^[A-Za-z_][A-Za-z0-9_]*$/);
}

export function targetKind(target: string, columns: readonly ColumnSchema[]): TargetKind {
  if (target === "score" || target === "score_label" || target === "has_score") {
    return target;
  }
  const found = columns.find((column) => column.name === target)?.column_type;
  if (found) {
    return found;
  }
  return MAP_COLUMNS.has(target) ? "string_map" : "string";
}

export function keepsName(from: string, to: string): boolean {
  return (
    (SCORE_FNS.has(from) && SCORE_FNS.has(to)) || (MAP_COLUMNS.has(from) && MAP_COLUMNS.has(to))
  );
}

function keywordOption(label: string): Completion {
  return { label, detail: "keyword" };
}

function listing(
  from: number,
  options: readonly Completion[],
  validFor: RegExp,
): CompletionResult | null {
  if (options.length === 0) {
    return null;
  }
  return { from, options: [...options], validFor };
}

function slot(
  tokens: readonly Token[],
  columns: ReadonlyMap<string, ColumnSchema>,
): "expr" | "op" | "logic" | "none" {
  const last = tokens.at(-1);
  if (!last) {
    return "expr";
  }
  if (last.kind === "lparen") {
    return "expr";
  }
  if (last.kind === "keyword") {
    const word = last.text.toLowerCase();
    if (word === "and" || word === "or" || word === "not") {
      return "expr";
    }
    return word === "true" || word === "false" || word === "null" ? "logic" : "none";
  }
  if (last.kind === "ident") {
    return columns.has(last.text.toLowerCase()) ? "op" : "none";
  }
  if (last.kind === "rbracket") {
    return mapAccess(tokens) ? "op" : "logic";
  }
  if (last.kind === "string" || last.kind === "number" || last.kind === "rparen") {
    return "logic";
  }
  return "none";
}

function opType(tokens: readonly Token[], columns: ReadonlyMap<string, ColumnSchema>): ColumnType {
  if (mapAccess(tokens)) {
    return "string";
  }
  const last = tokens.at(-1);
  if (last?.kind === "ident") {
    return columns.get(last.text.toLowerCase())?.column_type ?? "string";
  }
  return "string";
}

function mapAccess(tokens: readonly Token[]): boolean {
  const column = tokens.at(-4);
  const open = tokens.at(-3);
  const key = tokens.at(-2);
  const close = tokens.at(-1);
  return (
    close?.kind === "rbracket" &&
    key?.kind === "string" &&
    open?.kind === "lbracket" &&
    column?.kind === "ident" &&
    MAP_COLUMNS.has(column.text.toLowerCase())
  );
}

function columnMap(
  schema: Schema<"SchemaResponse"> | undefined,
  view: ViewName,
): Map<string, ColumnSchema> {
  const map = new Map<string, ColumnSchema>();
  const found = schema?.views.find((item) => item.name === view);
  for (const column of found?.columns ?? []) {
    map.set(column.name.toLowerCase(), column);
  }
  return map;
}

function allowFunction(fn: FunctionSchema, view: ViewName): boolean {
  if (!fn.in_filter) {
    return false;
  }
  return !(SCORE_FNS.has(fn.name) && !SCORE_VIEWS.has(view));
}

function starts(label: string, prefix: string): boolean {
  return label.toLowerCase().startsWith(prefix.toLowerCase());
}

function applyOperator(op: string): string {
  if (op === "IN" || op === "NOT IN") {
    return `${op} ()`;
  }
  if (op === "@>") {
    return "@> []";
  }
  return `${op} `;
}

function keyOf(field: Field): string {
  return "key" in field ? field.key : "";
}

function literalText(value: ChipValue): string {
  switch (value.kind) {
    case "string":
      return value.value;
    case "number":
      return value.text;
    case "boolean":
      return value.value ? "true" : "false";
  }
}

function formatLiteral(value: ChipValue): string {
  switch (value.kind) {
    case "string":
      return `'${value.value.replaceAll("'", "''")}'`;
    case "number":
      return value.text;
    case "boolean":
      return value.value ? "TRUE" : "FALSE";
  }
}

function fieldOf(target: string, name: string, type: TargetKind): Field | null {
  if (type === "string_map") {
    if (!isMapColumn(target) || name.length === 0) {
      return null;
    }
    return { column: target, key: name };
  }
  if (target.length === 0) {
    return null;
  }
  return { column: target };
}

function isMapColumn(name: string): name is "metadata" | "attributes" | "resource_attributes" {
  return name === "metadata" || name === "attributes" || name === "resource_attributes";
}

function isCompareOp(op: string): op is CompareOp {
  return (COMPARE_OPS as readonly string[]).includes(op);
}

function literal(text: string, type: ColumnType | "score" | "score_label"): ChipValue | null {
  const trimmed = text.trim();
  if (type === "boolean") {
    if (trimmed === "true") {
      return { kind: "boolean", value: true };
    }
    if (trimmed === "false") {
      return { kind: "boolean", value: false };
    }
    return null;
  }
  if (type === "integer" || type === "float" || type === "score" || type === "timestamp") {
    if (/^[+-]?(?:\d+\.\d*|\.\d+|\d+)(?:[eE][+-]?\d+)?$/.test(trimmed)) {
      const value = Number(trimmed);
      if (!Number.isFinite(value)) {
        return null;
      }
      return { kind: "number", value, text: trimmed.startsWith("+") ? trimmed.slice(1) : trimmed };
    }
    if (type === "timestamp") {
      return { kind: "string", value: text };
    }
    return null;
  }
  return { kind: "string", value: text };
}

function inValues(input: string): ChipValue[] | null {
  if (input.trim().length === 0) {
    return [];
  }
  const direct = parseChip(`name IN (${input})`);
  if (direct.kind === "in") {
    return direct.values;
  }
  const parts = input.split(",");
  const values: ChipValue[] = [];
  for (const part of parts) {
    const trimmed = part.trim();
    if (trimmed.length === 0) {
      return null;
    }
    const parsed = parseChip(`name = ${trimmed}`);
    if (parsed.kind === "compare") {
      values.push(parsed.value);
      continue;
    }
    values.push({ kind: "string", value: stripQuotes(trimmed) });
  }
  return values;
}

function containsValues(input: string): string[] | null {
  const direct = parseChip(`name @> [${input}]`);
  if (direct.kind === "contains") {
    return direct.values;
  }
  const parts = input
    .split(",")
    .map((part) => stripQuotes(part))
    .filter((part) => part.length > 0);
  return parts.length > 0 ? parts : null;
}

function stripQuotes(part: string): string {
  const trimmed = part.trim();
  if (trimmed.startsWith("'") && trimmed.endsWith("'") && trimmed.length >= 2) {
    return trimmed.slice(1, -1).replaceAll("''", "'");
  }
  return trimmed;
}
