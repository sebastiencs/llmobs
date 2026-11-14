/**
 * UTF-16 offset of a 1-based (line, column) where column counts Unicode characters.
 * Clamped to the text.
 */
export function offsetOf(text: string, line: number, column: number): number {
  const lines = text.split("\n");
  if (!Number.isInteger(line) || !Number.isInteger(column) || line < 1 || column < 1) {
    return 0;
  }
  if (line > lines.length) {
    return text.length;
  }
  let offset = 0;
  for (let index = 0; index < line - 1; index += 1) {
    offset += (lines[index] ?? "").length + 1;
  }
  const current = lines[line - 1] ?? "";
  const prefix = Array.from(current)
    .slice(0, column - 1)
    .join("");
  return Math.min(offset + prefix.length, text.length);
}

/**
 * The range to underline: from the position to the end of the word there
 * (`[A-Za-z0-9_]+`), else one character. At or past the end: the last character.
 * `null` for empty text.
 */
export function sqlErrorRange(
  text: string,
  position: { line: number; column: number },
): { from: number; to: number } | null {
  if (text.length === 0) {
    return null;
  }
  const offset = offsetOf(text, position.line, position.column);
  if (offset >= text.length) {
    return { from: text.length - 1, to: text.length };
  }
  const word = /[A-Za-z0-9_]+/y;
  word.lastIndex = offset;
  const match = word.exec(text);
  return { from: offset, to: match ? offset + match[0].length : offset + 1 };
}
