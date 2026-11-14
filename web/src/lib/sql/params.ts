export const PARAM_TYPES = ["String", "Int", "Float", "Bool", "Timestamp"] as const;
export type ParamType = (typeof PARAM_TYPES)[number];
export type SqlParam = { name: string; type: ParamType };
export type ParamValue = string | number | boolean;

const WORD_START = /[\p{Alphabetic}_#@]/u;
const WORD_PART = /[\p{Alphabetic}0-9@$#_]/u;
const TAG_PART = /[\p{Alphabetic}\p{N}_]/u;
const WORD = "[\\p{Alphabetic}_#@][\\p{Alphabetic}0-9@$#_]*";
const PLACEHOLDER = new RegExp(`\\{(${WORD}):(${WORD})\\}`, "uy");
const FLOAT = /^-?\d+(\.\d+)?([eE][-+]?\d+)?$/;
const INTEGER = /^-?\d+$/;
const TIMESTAMP =
  /^(\d{4})-(\d{2})-(\d{2})[Tt](\d{2}):(\d{2}):(\d{2})(\.\d{1,9})?([Zz]|[+-](\d{2}):(\d{2}))$/;

/** The code point at `index` as a string (one or two UTF-16 units). */
function charAt(text: string, index: number): string {
  const code = text.codePointAt(index);
  return code === undefined ? "" : String.fromCodePoint(code);
}

function canonicalType(text: string): ParamType | undefined {
  return PARAM_TYPES.find((type) => type.toLowerCase() === text.toLowerCase());
}

/**
 * Distinct placeholders outside strings, quoted identifiers, dollar-quoted strings and
 * comments, in order of first use. The first type of a name wins.
 */
export function sqlParams(text: string): SqlParam[] {
  const found: SqlParam[] = [];
  let index = 0;
  while (index < text.length) {
    const char = charAt(text, index);
    const next = text[index + 1];
    if (char === "-" && next === "-") {
      const end = text.indexOf("\n", index + 2);
      index = end < 0 ? text.length : end + 1;
    } else if (char === "/" && next === "*") {
      index = skipBlockComment(text, index);
    } else if (char === "'" || char === '"' || char === "`") {
      index = skipQuoted(text, index, char);
    } else if (char === "$") {
      index = skipDollar(text, index);
    } else if (char === "{") {
      PLACEHOLDER.lastIndex = index;
      const match = PLACEHOLDER.exec(text);
      if (match) {
        const name = match[1] ?? "";
        const type = canonicalType(match[2] ?? "");
        if (type && !found.some((param) => param.name === name)) {
          found.push({ name, type });
        }
        index += match[0].length;
      } else {
        index += 1;
      }
    } else if (WORD_START.test(char)) {
      index = skipWhile(text, index + char.length, WORD_PART);
    } else {
      index += char.length;
    }
  }
  return found;
}

function skipWhile(text: string, start: number, pattern: RegExp): number {
  let index = start;
  while (index < text.length) {
    const char = charAt(text, index);
    if (!pattern.test(char)) {
      break;
    }
    index += char.length;
  }
  return index;
}

/** Nested block comments, as GenericDialect reads them. Unterminated: the end. */
function skipBlockComment(text: string, start: number): number {
  let depth = 1;
  let index = start + 2;
  while (index < text.length) {
    if (text.startsWith("/*", index)) {
      depth += 1;
      index += 2;
    } else if (text.startsWith("*/", index)) {
      depth -= 1;
      index += 2;
      if (depth === 0) {
        return index;
      }
    } else {
      index += 1;
    }
  }
  return text.length;
}

/** Index after the closing quote; a doubled quote is an escaped one. Unterminated: the end. */
function skipQuoted(text: string, start: number, quote: string): number {
  let index = start + 1;
  while (index < text.length) {
    if (text[index] === quote) {
      if (text[index + 1] === quote) {
        index += 2;
        continue;
      }
      return index + 1;
    }
    index += 1;
  }
  return text.length;
}

/** `$$…$$`, `$tag$…$tag$`; a `$name` with no closing `$` is a one-token placeholder. */
function skipDollar(text: string, start: number): number {
  const tagEnd = skipWhile(text, start + 1, TAG_PART);
  if (text[tagEnd] !== "$") {
    return tagEnd;
  }
  const delimiter = text.slice(start, tagEnd + 1);
  const close = text.indexOf(delimiter, tagEnd + 1);
  return close < 0 ? text.length : close + delimiter.length;
}

/** The server's `parse_rfc3339` rule (crates/core/src/time_serde.rs). */
export function isServerTimestamp(text: string): boolean {
  const match = TIMESTAMP.exec(text);
  if (!match) {
    return false;
  }
  const [year, month, day, hour, minute, second] = match.slice(1, 7).map(Number);
  const offsetHour = match[9] === undefined ? 0 : Number(match[9]);
  const offsetMinute = match[10] === undefined ? 0 : Number(match[10]);
  if (
    year === undefined ||
    month === undefined ||
    day === undefined ||
    hour === undefined ||
    minute === undefined ||
    second === undefined
  ) {
    return false;
  }
  if (month < 1 || month > 12 || hour > 23 || minute > 59 || second > 59) {
    return false;
  }
  if (offsetHour > 23 || offsetMinute > 59) {
    return false;
  }
  const date = new Date(Date.UTC(year, month - 1, day));
  return (
    date.getUTCFullYear() === year && date.getUTCMonth() === month - 1 && date.getUTCDate() === day
  );
}

export const INT_RANGE = "Enter an integer between -9007199254740991 and 9007199254740991";

/** Typed JSON values for `params`, or a message per invalid name. */
export function paramValues(
  params: readonly SqlParam[],
  typed: Readonly<Record<string, string>>,
): { values: Record<string, ParamValue>; errors: Record<string, string> } {
  const values: Record<string, ParamValue> = {};
  const errors: Record<string, string> = {};
  for (const { name, type } of params) {
    const raw = typed[name] ?? "";
    const text = raw.trim();
    switch (type) {
      case "String":
        values[name] = raw;
        break;
      case "Int":
        if (!INTEGER.test(text)) {
          errors[name] = "Enter an integer";
        } else if (!Number.isSafeInteger(Number(text))) {
          errors[name] = INT_RANGE;
        } else {
          values[name] = Number(text);
        }
        break;
      case "Float":
        if (FLOAT.test(text) && Number.isFinite(Number(text))) {
          values[name] = Number(text);
        } else {
          errors[name] = "Enter a number";
        }
        break;
      case "Bool":
        if (text === "true" || text === "false") {
          values[name] = text === "true";
        } else {
          errors[name] = "Choose true or false";
        }
        break;
      case "Timestamp":
        if (INTEGER.test(text) && Number.isSafeInteger(Number(text))) {
          values[name] = Number(text);
        } else if (isServerTimestamp(text)) {
          values[name] = text;
        } else {
          errors[name] = "Enter an RFC 3339 time or epoch milliseconds";
        }
        break;
    }
  }
  return { values, errors };
}

/** The typed strings of the parameters `text` uses, `""` for one never typed. */
export function usedParams(
  text: string,
  typed: Readonly<Record<string, string>>,
): Record<string, string> {
  return Object.fromEntries(sqlParams(text).map((param) => [param.name, typed[param.name] ?? ""]));
}
