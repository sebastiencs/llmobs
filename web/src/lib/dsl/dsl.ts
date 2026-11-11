export type TokenKind =
  | "ident"
  | "string"
  | "number"
  | "op"
  | "keyword"
  | "lparen"
  | "rparen"
  | "lbracket"
  | "rbracket"
  | "comma"
  | "other";

export type Token = {
  kind: TokenKind;
  text: string;
  start: number;
  end: number;
};

export type ChipValue =
  | { kind: "string"; value: string }
  | { kind: "number"; value: number; text: string }
  | { kind: "boolean"; value: boolean };

export type CompareOp =
  | "="
  | "!="
  | "<"
  | "<="
  | ">"
  | ">="
  | "LIKE"
  | "ILIKE"
  | "NOT LIKE"
  | "NOT ILIKE";

export type Field =
  | { column: string }
  | { column: "metadata" | "attributes" | "resource_attributes"; key: string };

export type Chip =
  | { kind: "compare"; field: Field; op: CompareOp; value: ChipValue }
  | { kind: "null"; field: Field; negated: boolean }
  | { kind: "in"; field: Field; negated: boolean; values: ChipValue[] }
  | { kind: "contains"; field: Field; values: string[] }
  | { kind: "score"; name: string; fn: "score" | "score_label"; op: CompareOp; value: ChipValue }
  | { kind: "has_score"; name: string; negated: boolean }
  | { kind: "raw"; text: string };

export type Conjunct = { text: string; start: number; end: number };

const KEYWORDS = new Set([
  "and",
  "or",
  "not",
  "is",
  "in",
  "null",
  "like",
  "ilike",
  "true",
  "false",
]);

const MAP_COLUMNS = new Set(["metadata", "attributes", "resource_attributes"]);

const OPS = ["<>", "!=", "<=", ">=", "@>", "+", "-", "=", "<", ">"];

const COMPARE_OPS = new Set(["=", "!=", "<>", "<", "<=", ">", ">="]);

/** Never throws. An unterminated string is one `other` token through the end. */
export function tokenize(text: string): Token[] {
  const tokens: Token[] = [];
  let index = 0;
  while (index < text.length) {
    const char = text[index];
    if (char === undefined) {
      break;
    }
    if (isSpace(char)) {
      index += 1;
      continue;
    }
    if (char === "'") {
      const string = readString(text, index);
      tokens.push(string);
      index = string.end;
      if (string.kind === "other") {
        break;
      }
      continue;
    }
    if (isIdentStart(char)) {
      let end = index + 1;
      while (end < text.length && isIdentPart(text[end] ?? "")) {
        end += 1;
      }
      const word = text.slice(index, end);
      tokens.push({
        kind: KEYWORDS.has(word.toLowerCase()) ? "keyword" : "ident",
        text: word,
        start: index,
        end,
      });
      index = end;
      continue;
    }
    if (isNumberStart(text, index)) {
      const match = /^(\d+\.\d*|\.\d+|\d+)([eE][+-]?\d+)?/.exec(text.slice(index));
      const lexeme = match?.[0];
      if (lexeme) {
        tokens.push({ kind: "number", text: lexeme, start: index, end: index + lexeme.length });
        index += lexeme.length;
        continue;
      }
    }
    const op = OPS.find((candidate) => text.startsWith(candidate, index));
    if (op) {
      tokens.push({ kind: "op", text: op, start: index, end: index + op.length });
      index += op.length;
      continue;
    }
    const simple = simpleToken(char);
    if (simple) {
      tokens.push({ kind: simple, text: char, start: index, end: index + 1 });
      index += 1;
      continue;
    }
    tokens.push({ kind: "other", text: char, start: index, end: index + 1 });
    index += 1;
  }
  return tokens;
}

/** Splits on top-level AND. Trims each piece and drops empty ones. */
export function splitConjuncts(text: string): Conjunct[] {
  const cuts = [0];
  let depth = 0;
  for (const token of tokenize(text)) {
    if (token.kind === "lparen" || token.kind === "lbracket") {
      depth += 1;
    } else if (token.kind === "rparen" || token.kind === "rbracket") {
      depth = Math.max(0, depth - 1);
    } else if (depth === 0 && isKeyword(token, "and")) {
      cuts.push(token.start, token.end);
    }
  }
  cuts.push(text.length);
  const parts: Conjunct[] = [];
  for (let index = 0; index < cuts.length; index += 2) {
    const rawStart = cuts[index];
    const rawEnd = cuts[index + 1];
    if (rawStart === undefined || rawEnd === undefined) {
      continue;
    }
    const slice = text.slice(rawStart, rawEnd);
    const leading = slice.length - slice.trimStart().length;
    const trimmed = slice.trim();
    if (trimmed.length === 0) {
      continue;
    }
    const start = rawStart + leading;
    parts.push({ text: trimmed, start, end: start + trimmed.length });
  }
  return parts;
}

/** Recognises one conjunct. Anything else is returned unchanged as `raw`. */
export function parseChip(text: string): Chip {
  const tokens = tokenize(text);
  const chip = parseShape(tokens, 0, tokens.length);
  return chip ?? { kind: "raw", text };
}

export function serializeChip(chip: Chip): string {
  switch (chip.kind) {
    case "compare":
      return `${serializeField(chip.field)} ${chip.op} ${serializeValue(chip.value)}`;
    case "null":
      return `${serializeField(chip.field)} IS ${chip.negated ? "NOT " : ""}NULL`;
    case "in": {
      const values = chip.values.map(serializeValue).join(", ");
      return `${serializeField(chip.field)} ${chip.negated ? "NOT " : ""}IN (${values})`;
    }
    case "contains": {
      const values = chip.values
        .map((value) => serializeValue({ kind: "string", value }))
        .join(", ");
      return `${serializeField(chip.field)} @> [${values}]`;
    }
    case "score":
      return `${chip.fn}(${quote(chip.name)}) ${chip.op} ${serializeValue(chip.value)}`;
    case "has_score":
      return `${chip.negated ? "NOT " : ""}has_score(${quote(chip.name)})`;
    case "raw":
      return chip.text;
  }
}

/** Joins with ` AND `. A raw chip with a top-level OR is parenthesized. */
export function serializeFilter(chips: Chip[]): string {
  return chips
    .map((chip) => {
      const text = serializeChip(chip);
      return chip.kind === "raw" && hasTopLevelOr(chip.text) ? `(${text})` : text;
    })
    .join(" AND ");
}

function parseShape(tokens: Token[], from: number, to: number): Chip | undefined {
  if (from >= to) {
    return undefined;
  }
  const inner = wrapped(tokens, from, to);
  if (inner) {
    return parseShape(tokens, inner.from, inner.to);
  }
  return (
    parseHasScore(tokens, from, to) ??
    parseScore(tokens, from, to) ??
    parseFieldExpr(tokens, from, to)
  );
}

function parseHasScore(tokens: Token[], from: number, to: number): Chip | undefined {
  let index = from;
  let negated = false;
  if (isKeyword(tokens[index], "not")) {
    negated = true;
    index += 1;
  }
  if (!isName(tokens[index], "has_score") || !isKind(tokens[index + 1], "lparen")) {
    return undefined;
  }
  const name = decodeStringToken(tokens[index + 2]);
  if (name === undefined || !isKind(tokens[index + 3], "rparen") || index + 4 !== to) {
    return undefined;
  }
  return { kind: "has_score", name, negated };
}

function parseScore(tokens: Token[], from: number, to: number): Chip | undefined {
  const fn = tokenName(tokens[from]);
  if ((fn !== "score" && fn !== "score_label") || !isKind(tokens[from + 1], "lparen")) {
    return undefined;
  }
  const name = decodeStringToken(tokens[from + 2]);
  if (name === undefined || !isKind(tokens[from + 3], "rparen")) {
    return undefined;
  }
  const op = compareOp(tokens[from + 4]);
  const value = op === undefined ? undefined : parseValue(tokens, from + 5, to);
  if (op === undefined || value === undefined || value.next !== to) {
    return undefined;
  }
  return { kind: "score", name, fn, op, value: value.value };
}

function parseFieldExpr(tokens: Token[], from: number, to: number): Chip | undefined {
  const field = parseField(tokens, from, to);
  if (!field) {
    return undefined;
  }
  const index = field.next;
  const token = tokens[index];
  if (isKeyword(token, "is")) {
    const negated = isKeyword(tokens[index + 1], "not");
    const nullAt = negated ? index + 2 : index + 1;
    if (!isKeyword(tokens[nullAt], "null") || nullAt + 1 !== to) {
      return undefined;
    }
    return { kind: "null", field: field.field, negated };
  }
  if (isKeyword(token, "not")) {
    if (isKeyword(tokens[index + 1], "in")) {
      return parseIn(tokens, index + 2, to, field.field, true);
    }
    const like = likeOp(tokens[index + 1]);
    if (like) {
      const value = parseValue(tokens, index + 2, to);
      if (value === undefined || value.next !== to) {
        return undefined;
      }
      return { kind: "compare", field: field.field, op: `NOT ${like}`, value: value.value };
    }
    return undefined;
  }
  if (isKeyword(token, "in")) {
    return parseIn(tokens, index + 1, to, field.field, false);
  }
  const like = likeOp(token);
  if (like) {
    const value = parseValue(tokens, index + 1, to);
    if (value === undefined || value.next !== to) {
      return undefined;
    }
    return { kind: "compare", field: field.field, op: like, value: value.value };
  }
  if (token?.kind === "op" && token.text === "@>") {
    return parseContains(tokens, index + 1, to, field.field);
  }
  const op = compareOp(token);
  if (op === undefined) {
    return undefined;
  }
  const value = parseValue(tokens, index + 1, to);
  if (value === undefined || value.next !== to) {
    return undefined;
  }
  return { kind: "compare", field: field.field, op, value: value.value };
}

function parseIn(
  tokens: Token[],
  index: number,
  to: number,
  field: Field,
  negated: boolean,
): Chip | undefined {
  if (!isKind(tokens[index], "lparen")) {
    return undefined;
  }
  const list = parseList(tokens, index + 1, to, "rparen", false);
  if (list === undefined || list.next !== to) {
    return undefined;
  }
  return { kind: "in", field, negated, values: list.values };
}

function parseContains(tokens: Token[], index: number, to: number, field: Field): Chip | undefined {
  if (!isKind(tokens[index], "lbracket")) {
    return undefined;
  }
  const list = parseList(tokens, index + 1, to, "rbracket", true);
  if (list === undefined || list.next !== to) {
    return undefined;
  }
  const values: string[] = [];
  for (const value of list.values) {
    if (value.kind !== "string") {
      return undefined;
    }
    values.push(value.value);
  }
  return { kind: "contains", field, values };
}

function parseList(
  tokens: Token[],
  index: number,
  to: number,
  close: "rparen" | "rbracket",
  stringsOnly: boolean,
): { values: ChipValue[]; next: number } | undefined {
  if (isKind(tokens[index], close)) {
    return { values: [], next: index + 1 };
  }
  const values: ChipValue[] = [];
  let cursor = index;
  while (cursor < to) {
    const value = parseValue(tokens, cursor, to);
    if (value === undefined || (stringsOnly && value.value.kind !== "string")) {
      return undefined;
    }
    values.push(value.value);
    cursor = value.next;
    if (isKind(tokens[cursor], close)) {
      return { values, next: cursor + 1 };
    }
    if (!isKind(tokens[cursor], "comma")) {
      return undefined;
    }
    cursor += 1;
  }
  return undefined;
}

function parseField(
  tokens: Token[],
  from: number,
  to: number,
): { field: Field; next: number } | undefined {
  const token = tokens[from];
  if (token?.kind !== "ident") {
    return undefined;
  }
  const column = token.text.toLowerCase();
  if (!isKind(tokens[from + 1], "lbracket")) {
    return { field: { column }, next: from + 1 };
  }
  const key = decodeStringToken(tokens[from + 2]);
  if (key === undefined || !isKind(tokens[from + 3], "rbracket") || !isMapColumn(column)) {
    return undefined;
  }
  if (from + 4 > to) {
    return undefined;
  }
  return { field: { column, key }, next: from + 4 };
}

function parseValue(
  tokens: Token[],
  index: number,
  to: number,
): { value: ChipValue; next: number } | undefined {
  const token = tokens[index];
  if (token === undefined || index >= to) {
    return undefined;
  }
  if (token.kind === "string") {
    const value = decodeStringToken(token);
    return value === undefined ? undefined : { value: { kind: "string", value }, next: index + 1 };
  }
  if (
    token.kind === "keyword" &&
    (token.text.toLowerCase() === "true" || token.text.toLowerCase() === "false")
  ) {
    return {
      value: { kind: "boolean", value: token.text.toLowerCase() === "true" },
      next: index + 1,
    };
  }
  let sign = "";
  let numberAt = index;
  if (token.kind === "op" && (token.text === "+" || token.text === "-")) {
    sign = token.text;
    numberAt += 1;
  }
  const number = tokens[numberAt];
  if (number?.kind !== "number" || numberAt >= to) {
    return undefined;
  }
  const text = `${sign}${number.text}`;
  const value = Number(text);
  if (!Number.isFinite(value)) {
    return undefined;
  }
  return { value: { kind: "number", value, text }, next: numberAt + 1 };
}

function wrapped(
  tokens: Token[],
  from: number,
  to: number,
): { from: number; to: number } | undefined {
  if (!isKind(tokens[from], "lparen")) {
    return undefined;
  }
  let depth = 0;
  for (let index = from; index < to; index += 1) {
    const token = tokens[index];
    if (token?.kind === "lparen") {
      depth += 1;
    } else if (token?.kind === "rparen") {
      depth -= 1;
      if (depth === 0) {
        return index === to - 1 && index > from + 1 ? { from: from + 1, to: index } : undefined;
      }
    }
  }
  return undefined;
}

function serializeField(field: Field): string {
  if ("key" in field) {
    return `${field.column.toLowerCase()}[${quote(field.key)}]`;
  }
  return field.column.toLowerCase();
}

function serializeValue(value: ChipValue): string {
  switch (value.kind) {
    case "string":
      return quote(value.value);
    case "number":
      return value.text;
    case "boolean":
      return value.value ? "TRUE" : "FALSE";
  }
}

function quote(value: string): string {
  return `'${value.replaceAll("'", "''")}'`;
}

function hasTopLevelOr(text: string): boolean {
  let depth = 0;
  for (const token of tokenize(text)) {
    if (token.kind === "lparen" || token.kind === "lbracket") {
      depth += 1;
    } else if (token.kind === "rparen" || token.kind === "rbracket") {
      depth = Math.max(0, depth - 1);
    } else if (depth === 0 && isKeyword(token, "or")) {
      return true;
    }
  }
  return false;
}

function readString(text: string, start: number): Token {
  let index = start + 1;
  while (index < text.length) {
    if (text[index] !== "'") {
      index += 1;
      continue;
    }
    if (text[index + 1] === "'") {
      index += 2;
      continue;
    }
    return { kind: "string", text: text.slice(start, index + 1), start, end: index + 1 };
  }
  return { kind: "other", text: text.slice(start), start, end: text.length };
}

function decodeStringToken(token: Token | undefined): string | undefined {
  if (token?.kind !== "string" || token.text.length < 2) {
    return undefined;
  }
  let value = "";
  const body = token.text.slice(1, -1);
  for (let index = 0; index < body.length; index += 1) {
    const char = body[index];
    if (char === "'") {
      if (body[index + 1] !== "'") {
        return undefined;
      }
      value += "'";
      index += 1;
      continue;
    }
    value += char ?? "";
  }
  return value;
}

function compareOp(token: Token | undefined): CompareOp | undefined {
  if (token?.kind !== "op" || !COMPARE_OPS.has(token.text)) {
    return undefined;
  }
  return token.text === "<>" ? "!=" : (token.text as CompareOp);
}

function likeOp(token: Token | undefined): "LIKE" | "ILIKE" | undefined {
  if (!isKeyword(token, "like") && !isKeyword(token, "ilike")) {
    return undefined;
  }
  return token?.text.toLowerCase() === "ilike" ? "ILIKE" : "LIKE";
}

function isMapColumn(column: string): column is "metadata" | "attributes" | "resource_attributes" {
  return MAP_COLUMNS.has(column);
}

function isKeyword(token: Token | undefined, word: string): boolean {
  return token?.kind === "keyword" && token.text.toLowerCase() === word;
}

function isName(token: Token | undefined, word: string): boolean {
  return token?.kind === "ident" && token.text.toLowerCase() === word;
}

function tokenName(token: Token | undefined): string | undefined {
  return token?.kind === "ident" ? token.text.toLowerCase() : undefined;
}

function isKind(token: Token | undefined, kind: TokenKind): boolean {
  return token?.kind === kind;
}

function simpleToken(char: string): TokenKind | undefined {
  switch (char) {
    case "(":
      return "lparen";
    case ")":
      return "rparen";
    case "[":
      return "lbracket";
    case "]":
      return "rbracket";
    case ",":
      return "comma";
    default:
      return undefined;
  }
}

function isSpace(char: string): boolean {
  return char === " " || char === "\n" || char === "\t" || char === "\r" || char === "\f";
}

function isIdentStart(char: string): boolean {
  return (char >= "A" && char <= "Z") || (char >= "a" && char <= "z") || char === "_";
}

function isIdentPart(char: string): boolean {
  return isIdentStart(char) || (char >= "0" && char <= "9");
}

function isNumberStart(text: string, index: number): boolean {
  const char = text[index];
  if (char !== undefined && char >= "0" && char <= "9") {
    return true;
  }
  const next = text[index + 1];
  return char === "." && next !== undefined && next >= "0" && next <= "9";
}
