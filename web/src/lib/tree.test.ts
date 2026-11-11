import { expect, it } from "vitest";
import {
  ancestorsOf,
  defaultCollapsed,
  filterRows,
  hasChildren,
  insertSpan,
  subtreeEnds,
  visibleRows,
} from "@/lib/tree";

// Two roots and a detached root:
// 0 agent
// 1   llm
// 2     tool
// 3   chain
// 4 agent
// 5   retriever
// 6 detached
const tree = [
  node(0, "a"),
  node(1, "b"),
  node(2, "c"),
  node(1, "d"),
  node(0, "e"),
  node(1, "f"),
  node(0, "g", true),
];

function node(depth: number, spanId: string, detached = false) {
  return { depth, detached, span: { span_id: spanId } };
}

it("finds subtree ends on a tree with two roots and a detached root", () => {
  const ends = subtreeEnds(tree);
  expect(Array.from(ends)).toEqual([4, 3, 3, 4, 6, 6, 7]);
  expect(hasChildren(ends, 0)).toBe(true);
  expect(hasChildren(ends, 1)).toBe(true);
  expect(hasChildren(ends, 2)).toBe(false);
  expect(hasChildren(ends, 6)).toBe(false);
  expect(ancestorsOf(tree, 2)).toEqual([0, 1]);
  expect(ancestorsOf(tree, 5)).toEqual([4]);
  expect(ancestorsOf(tree, 6)).toEqual([]);
});

it("hides descendants of a collapsed span and keeps the span itself", () => {
  const ends = subtreeEnds(tree);
  expect(visibleRows(tree, ends, new Set(["b"]))).toEqual([0, 1, 3, 4, 5, 6]);
  expect(visibleRows(tree, ends, new Set(["a"]))).toEqual([0, 4, 5, 6]);
  expect(visibleRows(tree, ends, new Set())).toEqual([0, 1, 2, 3, 4, 5, 6]);
});

it("keeps matches and their ancestors", () => {
  const ends = subtreeEnds(tree);
  expect(filterRows(tree, ends, (index) => index === 2)).toEqual([0, 1, 2]);
  expect(filterRows(tree, ends, (index) => tree[index]?.span.span_id === "g")).toEqual([6]);
  expect(filterRows(tree, ends, () => false)).toEqual([]);
});

it("opens trees of up to 1 000 spans and collapses depth-2 parents after that", () => {
  const open = Array.from({ length: 1000 }, (_, index) =>
    node(index === 0 ? 0 : index === 1 ? 1 : index === 2 ? 2 : 3, `s${index}`),
  );
  expect(defaultCollapsed(open).size).toBe(0);

  const folded = Array.from({ length: 1001 }, (_, index) =>
    node(index === 0 ? 0 : index === 1 ? 1 : index === 2 ? 2 : 3, `s${index}`),
  );
  expect(defaultCollapsed(folded)).toEqual(new Set(["s2"]));
});

it("counts ends and visible rows for 10 000 generated spans", () => {
  const spans = Array.from({ length: 10_000 }, (_, index) =>
    node(index === 0 ? 0 : 1 + (index % 4), `s${index}`),
  );
  const ends = subtreeEnds(spans);
  expect(ends.length).toBe(10_000);
  for (let index = 0; index < spans.length; index++) {
    const span = spans[index];
    if (!span) {
      continue;
    }
    let end = index + 1;
    while (end < spans.length) {
      const next = spans[end];
      if (!next || next.depth <= span.depth) {
        break;
      }
      end += 1;
    }
    expect(ends[index]).toBe(end);
  }

  const collapsed = defaultCollapsed(spans);
  const visible = visibleRows(spans, ends, collapsed);
  const hidden = new Set<number>();
  for (let index = 0; index < spans.length; index++) {
    const span = spans[index];
    const end = ends[index];
    if (!span || end === undefined || !collapsed.has(span.span.span_id)) {
      continue;
    }
    for (let child = index + 1; child < end; child++) {
      hidden.add(child);
    }
  }
  const expected: number[] = [];
  for (let index = 0; index < spans.length; index++) {
    if (!hidden.has(index)) {
      expected.push(index);
    }
  }
  expect(visible).toEqual(expected);
  expect(visible.length).toBeGreaterThan(0);
  expect(visible.length).toBeLessThan(spans.length);
});

it("keeps the right rows for 10 000 spans", () => {
  const spans = Array.from({ length: 10_000 }, (_, index) =>
    node(index === 0 ? 0 : 1 + (index % 5), `s${index}`),
  );
  const ends = subtreeEnds(spans);
  const predicate = (index: number) => index !== 0 && index % 100 === 0;
  const rows = filterRows(spans, ends, predicate);
  const expected = new Set<number>();
  for (let index = 0; index < spans.length; index++) {
    if (!predicate(index)) {
      continue;
    }
    expected.add(index);
    for (const ancestor of ancestorsOf(spans, index)) {
      expected.add(ancestor);
    }
  }
  expect(rows).toEqual([...expected].sort((left, right) => left - right));
  expect(rows.length).toBeGreaterThan(0);
  expect(rows.length).toBeLessThan(spans.length);
});

function live(id: string, start: string, parent?: string, name?: string) {
  return {
    span_id: id,
    start_time: start,
    ...(parent ? { parent_span_id: parent } : {}),
    ...(name ? { name } : {}),
  };
}

function placed(id: string, start: string, depth: number, parent?: string, detached = false) {
  return { depth, detached, span: live(id, start, parent) };
}

it("inserts a child after earlier siblings and their descendants", () => {
  const spans = [
    placed("p", "t0", 0),
    placed("a", "t1", 1, "p"),
    placed("g", "t2", 2, "a"),
    placed("c", "t3", 1, "p"),
  ];
  const next = insertSpan(spans, live("b", "t2", "p"));
  expect(next.map((row) => row.span.span_id)).toEqual(["p", "a", "g", "b", "c"]);
  expect(next[3]).toEqual({ depth: 1, detached: false, span: live("b", "t2", "p") });
});

it("orders siblings with the same start by span id", () => {
  const spans = [placed("p", "t0", 0), placed("b", "t1", 1, "p"), placed("d", "t1", 1, "p")];
  const next = insertSpan(spans, live("c", "t1", "p"));
  expect(next.map((row) => row.span.span_id)).toEqual(["p", "b", "c", "d"]);
});

it("appends an unknown parent as a detached root and leaves it there", () => {
  const spans = [placed("p", "t0", 0)];
  const detached = insertSpan(spans, live("orphan", "t1", "missing"));
  expect(detached).toEqual([
    ...spans,
    { depth: 0, detached: true, span: live("orphan", "t1", "missing") },
  ]);
  const parent = insertSpan(detached, live("missing", "t0"));
  expect(parent.map((row) => row.span.span_id)).toEqual(["p", "orphan", "missing"]);
  expect(parent[1]?.detached).toBe(true);
});

it("replaces a duplicate span in place", () => {
  const spans = [placed("p", "t0", 0), placed("a", "t1", 1, "p")];
  const next = insertSpan(spans, live("a", "t9", "p", "updated"));
  expect(next.map((row) => row.span.span_id)).toEqual(["p", "a"]);
  expect(next[1]?.depth).toBe(1);
  expect(next[1]?.span).toMatchObject({ start_time: "t9", name: "updated" });
});

it("places one span in a 10 000-span tree", () => {
  const spans = [placed("p", stamp(0), 0)];
  for (let index = 0; index < 10_000; index++) {
    spans.push(placed(idOf(index), stamp(index * 2 + 2), 1, "p"));
  }
  const incoming = live(idOf(10_000), stamp(5000 * 2 + 1), "p");
  const next = insertSpan(spans, incoming);
  expect(next).toHaveLength(10_002);
  const at = next.findIndex((row) => row.span.span_id === incoming.span_id);
  expect(at).toBe(5001);
  expect(next[at]).toEqual({ depth: 1, detached: false, span: incoming });
  const before = next[at - 1]?.span.start_time;
  const after = next[at + 1]?.span.start_time;
  if (before === undefined || after === undefined) {
    throw new Error("missing neighbours");
  }
  expect(before < incoming.start_time).toBe(true);
  expect(after > incoming.start_time).toBe(true);
  expect(next.filter((row) => row.depth === 0).map((row) => row.span.span_id)).toEqual(["p"]);
});

function stamp(index: number): string {
  return `t${String(index).padStart(5, "0")}`;
}

function idOf(index: number): string {
  return `s${String(index).padStart(5, "0")}`;
}
