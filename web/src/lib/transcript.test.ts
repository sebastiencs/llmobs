import { expect, it } from "vitest";
import type { Schema } from "@/api/types";
import { freshInput, transcriptSpans } from "./transcript";

const TRACE = "a".repeat(32);

it("keeps agent and chain sections only when they contain an LLM or tool span", () => {
  const items = transcriptSpans([
    row("b100000000000001", "agent", 0),
    row("b100000000000002", "chain", 1),
    row("b100000000000003", "llm", 2),
    row("b100000000000004", "retriever", 1),
    row("b100000000000005", "tool", 1),
    row("b100000000000006", "agent", 0),
    row("b100000000000007", "span", 1),
    row("b100000000000008", "llm", 0),
  ]);

  expect(items.map(label)).toEqual([
    "section:b100000000000001:0",
    "section:b100000000000002:1",
    "turn:b100000000000003",
    "turn:b100000000000005",
    "turn:b100000000000008",
  ]);
});

it("drops a trace with no LLM or tool spans", () => {
  expect(
    transcriptSpans([row("b100000000000001", "agent", 0), row("b100000000000002", "span", 1)]),
  ).toEqual([]);
});

it("shows only the messages past the previous LLM call's prefix", () => {
  const seen = [message("user", "Hello"), message("assistant", "")];
  expect(
    freshInput(seen, [message("user", "Hello"), message("assistant", ""), message("user", "Next")]),
  ).toEqual([message("user", "Next")]);
  expect(freshInput(seen, [message("user", "Different")])).toEqual([message("user", "Different")]);
  expect(freshInput(undefined, [message("user", "Hello")])).toEqual([message("user", "Hello")]);
});

function label(item: ReturnType<typeof transcriptSpans>[number]): string {
  if (item.kind === "section") {
    return `section:${item.span.span.span_id}:${item.depth}`;
  }
  return `turn:${item.span.span.span_id}`;
}

function message(role: string, text: string): Schema<"Message"> {
  return { role, parts: text.length > 0 ? [{ type: "text", content: text }] : [] };
}

function row(id: string, kind: Schema<"SpanKind">, depth: number): Schema<"TreeSpan"> {
  return {
    depth,
    detached: false,
    span: {
      trace_id: TRACE,
      span_id: id,
      name: id,
      span_kind: kind,
      start_time: "2026-01-01T00:00:00.000Z",
      end_time: "2026-01-01T00:00:01.000Z",
      duration_ns: 1_000_000_000,
      status: "ok",
      metadata: {},
      tags: [],
    },
  };
}
