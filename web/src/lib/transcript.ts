import type { Schema } from "@/api/types";

export type TranscriptItem =
  | { kind: "section"; span: Schema<"TreeSpan">; depth: number }
  | { kind: "turn"; span: Schema<"TreeSpan"> };

const SECTION_KINDS = new Set<Schema<"SpanKind">>(["agent", "chain"]);
const TURN_KINDS = new Set<Schema<"SpanKind">>(["llm", "tool"]);

/** LLM and tool spans in pre-order, with agent and chain headers that contain one. */
export function transcriptSpans(spans: readonly Schema<"TreeSpan">[]): TranscriptItem[] {
  const parent = parentIndexes(spans);
  const keptDescendant = new Array<boolean>(spans.length).fill(false);
  for (let index = spans.length - 1; index >= 0; index -= 1) {
    const row = spans[index];
    if (!row) continue;
    const kept = TURN_KINDS.has(row.span.span_kind) || keptDescendant[index] === true;
    const ancestor = parent[index];
    if (kept && ancestor !== undefined && ancestor >= 0) {
      keptDescendant[ancestor] = true;
    }
  }

  const items: TranscriptItem[] = [];
  const open: number[] = [];
  for (let index = 0; index < spans.length; index += 1) {
    const row = spans[index];
    if (!row) continue;
    while (open.length > 0 && (open[open.length - 1] ?? -1) >= row.depth) {
      open.pop();
    }
    if (SECTION_KINDS.has(row.span.span_kind) && keptDescendant[index] === true) {
      items.push({ kind: "section", span: row, depth: open.length });
      open.push(row.depth);
      continue;
    }
    if (TURN_KINDS.has(row.span.span_kind)) {
      items.push({ kind: "turn", span: row });
    }
  }
  return items;
}

/** Messages of one payload. An empty list when the payload is not a conversation. */
export function payloadMessages(payload: Schema<"Payload"> | undefined): Schema<"Message">[] {
  if (payload?.format === "messages") return payload.value;
  return [];
}

/**
 * Input messages that were not already shown on the previous LLM call.
 * A shared prefix is role plus the text parts. No shared prefix shows the whole input.
 */
export function freshInput(
  previous: readonly Schema<"Message">[] | undefined,
  input: readonly Schema<"Message">[],
): Schema<"Message">[] {
  if (!previous || previous.length === 0) return [...input];
  let index = 0;
  while (index < input.length && index < previous.length) {
    const next = input[index];
    const seen = previous[index];
    if (!next || !seen || !sameMessage(next, seen)) break;
    index += 1;
  }
  return input.slice(index);
}

export function messageText(message: Schema<"Message">): string {
  const parts: string[] = [];
  for (const part of message.parts) {
    if (part.type === "text" && typeof part.content === "string") {
      parts.push(part.content);
    }
  }
  return parts.join("\n");
}

function sameMessage(left: Schema<"Message">, right: Schema<"Message">): boolean {
  return left.role === right.role && messageText(left) === messageText(right);
}

function parentIndexes(spans: readonly Schema<"TreeSpan">[]): number[] {
  const parent = new Array<number>(spans.length).fill(-1);
  const stack: number[] = [];
  for (let index = 0; index < spans.length; index += 1) {
    const depth = spans[index]?.depth ?? 0;
    while (stack.length > 0) {
      const top = stack[stack.length - 1];
      if (top === undefined) break;
      if ((spans[top]?.depth ?? 0) < depth) break;
      stack.pop();
    }
    parent[index] = stack[stack.length - 1] ?? -1;
    stack.push(index);
  }
  return parent;
}
