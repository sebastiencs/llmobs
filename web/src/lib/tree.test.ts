import { expect, it } from "vitest";
import {
  ancestorsOf,
  defaultCollapsed,
  filterRows,
  hasChildren,
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
