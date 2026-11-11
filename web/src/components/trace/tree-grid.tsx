// The plan requires a CSS grid with grid/row/gridcell roles so rows can be
// virtualized. The grid is the single tab stop; arrows move the selection.
// biome-ignore-all lint/a11y/useSemanticElements: virtualized CSS grid, not a table
// biome-ignore-all lint/a11y/useFocusableInteractive: one tab stop on the grid
// biome-ignore-all lint/a11y/useKeyWithClickEvents: arrows on the grid move the selection

import { useVirtualizer } from "@tanstack/react-virtual";
import { cn } from "cn";
import { ChevronDown, ChevronRight } from "lucide-react";
import { type KeyboardEvent, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Schema } from "@/api/types";
import { KindIcon } from "@/components/kind-icon";
import { StatusDot } from "@/components/status-dot";
import { Badge } from "@/components/ui/badge";
import { formatCost, formatDuration, formatTokens } from "@/lib/format";
import { barGeometry, ticks, traceWindow } from "@/lib/timeline";
import { ancestorsOf, defaultCollapsed, hasChildren, subtreeEnds, visibleRows } from "@/lib/tree";

const ROW_HEIGHT = 28;
const OVERSCAN = 12;
const TEMPLATE = "minmax(0, 1.4fr) 72px 64px 72px minmax(0, 1fr)";

const BAR: Record<Schema<"SpanKind">, string> = {
  llm: "bg-kind-llm",
  embedding: "bg-kind-embedding",
  tool: "bg-kind-tool",
  agent: "bg-kind-agent",
  chain: "bg-kind-chain",
  retriever: "bg-kind-retriever",
  reranker: "bg-kind-reranker",
  guardrail: "bg-kind-guardrail",
  evaluator: "bg-kind-evaluator",
  span: "bg-kind-span",
};

export function TreeGrid({
  spans,
  trace,
  selectedSpanId,
  onSelectSpan,
}: {
  spans: readonly Schema<"TreeSpan">[];
  trace: Schema<"TraceSummary">;
  selectedSpanId?: string;
  onSelectSpan: (spanId: string) => void;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const gridRef = useRef<HTMLDivElement>(null);
  const timelineRef = useRef<HTMLDivElement>(null);
  const [timelineWidth, setTimelineWidth] = useState(480);
  const [collapsed, setCollapsed] = useState(() => collapsedWithPath(spans, selectedSpanId));

  useEffect(() => {
    const index = spans.findIndex((row) => row.span.span_id === selectedSpanId);
    if (index < 0) {
      return;
    }
    setCollapsed((current) => expandAncestors(spans, current, index));
  }, [selectedSpanId, spans]);

  useLayoutEffect(() => {
    const node = timelineRef.current;
    if (!node) {
      return;
    }
    const update = () => {
      if (node.clientWidth > 0) {
        setTimelineWidth(node.clientWidth);
      }
    };
    update();
    const observer = new ResizeObserver(update);
    observer.observe(node);
    return () => observer.disconnect();
  }, []);

  const ends = useMemo(() => subtreeEnds(spans), [spans]);
  const rows = useMemo(() => visibleRows(spans, ends, collapsed), [spans, ends, collapsed]);
  const timeWindow = useMemo(() => traceWindow(trace, spans), [trace, spans]);
  const marks = useMemo(() => ticks(timeWindow, timelineWidth), [timeWindow, timelineWidth]);
  const selectedIndex = spans.findIndex((row) => row.span.span_id === selectedSpanId);

  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: OVERSCAN,
  });

  useEffect(() => {
    const visibleIndex = rows.indexOf(selectedIndex);
    if (visibleIndex >= 0) {
      virtualizer.scrollToIndex(visibleIndex, { align: "auto" });
    }
  }, [rows, selectedIndex, virtualizer]);

  function toggle(spanId: string) {
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(spanId)) {
        next.delete(spanId);
      } else {
        next.add(spanId);
      }
      return next;
    });
  }

  function selectAt(spanIndex: number) {
    const row = spans[spanIndex];
    if (!row) {
      return;
    }
    onSelectSpan(row.span.span_id);
  }

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (rows.length === 0) {
      return;
    }
    const visiblePos = Math.max(0, rows.indexOf(selectedIndex));
    if (event.key === "ArrowDown") {
      event.preventDefault();
      selectAt(rows[Math.min(rows.length - 1, visiblePos + 1)] ?? 0);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      selectAt(rows[Math.max(0, visiblePos - 1)] ?? 0);
    } else if (event.key === "ArrowRight") {
      event.preventDefault();
      const id = spans[selectedIndex]?.span.span_id;
      if (id && hasChildren(ends, selectedIndex) && collapsed.has(id)) {
        toggle(id);
      }
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      const id = spans[selectedIndex]?.span.span_id;
      if (id && hasChildren(ends, selectedIndex) && !collapsed.has(id)) {
        toggle(id);
        return;
      }
      const parent = ancestorsOf(spans, selectedIndex).at(-1);
      if (parent !== undefined) {
        selectAt(parent);
      }
    } else if (event.key === "Enter") {
      event.preventDefault();
      const id = spans[selectedIndex]?.span.span_id;
      if (id && hasChildren(ends, selectedIndex)) {
        toggle(id);
      }
    }
  }

  const virtualRows = virtualizer.getVirtualItems();

  return (
    <div
      ref={gridRef}
      role="grid"
      aria-label="Trace"
      aria-rowcount={rows.length + 1}
      aria-colcount={5}
      tabIndex={0}
      className="flex min-h-0 flex-1 flex-col outline-none"
      onKeyDown={onKeyDown}
    >
      <div
        role="row"
        aria-rowindex={1}
        className="grid h-row shrink-0 items-center border-b bg-background text-[11px] font-medium text-muted-foreground uppercase"
        style={{ gridTemplateColumns: TEMPLATE }}
      >
        <div role="columnheader" className="min-w-0 truncate px-2">
          Name
        </div>
        <div role="columnheader" className="px-2 text-right">
          Duration
        </div>
        <div role="columnheader" className="px-2 text-right">
          Tokens
        </div>
        <div role="columnheader" className="px-2 text-right">
          Cost
        </div>
        <div
          role="columnheader"
          aria-label="Timeline"
          className="relative h-full"
          ref={timelineRef}
        >
          <span className="sr-only">Timeline</span>
          {marks.map((mark) => (
            <span
              key={mark.ns}
              aria-hidden="true"
              className="absolute top-1/2 -translate-y-1/2 text-[10px] normal-case"
              style={{
                left: `${((mark.ns - timeWindow.startNs) / (timeWindow.endNs - timeWindow.startNs)) * 100}%`,
              }}
            >
              {mark.label}
            </span>
          ))}
        </div>
      </div>
      <div ref={scrollRef} data-scroll-container className="relative min-h-0 flex-1 overflow-auto">
        <div className="relative" style={{ height: virtualizer.getTotalSize() }}>
          {virtualRows.map((item) => {
            const spanIndex = rows[item.index];
            const row = spanIndex === undefined ? undefined : spans[spanIndex];
            if (spanIndex === undefined || !row) {
              return null;
            }
            const span = row.span;
            const selected = span.span_id === selectedSpanId;
            const expandable = hasChildren(ends, spanIndex);
            const isCollapsed = collapsed.has(span.span_id);
            const bar = barGeometry(span, timeWindow);
            const showTokens = span.span_kind === "llm" || span.span_kind === "embedding";
            return (
              <div
                key={span.span_id}
                role="row"
                aria-rowindex={item.index + 2}
                aria-selected={selected}
                className={cn(
                  "absolute top-0 grid w-full items-center border-b",
                  selected
                    ? "bg-selection shadow-[inset_2px_0_0_var(--primary)]"
                    : "hover:bg-muted",
                )}
                style={{
                  height: ROW_HEIGHT,
                  transform: `translateY(${item.start}px)`,
                  gridTemplateColumns: TEMPLATE,
                }}
                onClick={() => {
                  gridRef.current?.focus();
                  onSelectSpan(span.span_id);
                }}
              >
                <div role="gridcell" className="min-w-0">
                  <div
                    className="flex min-w-0 items-center gap-1 pr-2"
                    style={{ paddingLeft: row.depth * 12 + 4 }}
                  >
                    {expandable ? (
                      <button
                        type="button"
                        tabIndex={-1}
                        aria-expanded={!isCollapsed}
                        aria-label={isCollapsed ? "Expand" : "Collapse"}
                        className="inline-flex size-4 shrink-0 items-center justify-center"
                        onClick={(event) => {
                          event.stopPropagation();
                          toggle(span.span_id);
                        }}
                      >
                        {isCollapsed ? (
                          <ChevronRight className="size-3" />
                        ) : (
                          <ChevronDown className="size-3" />
                        )}
                      </button>
                    ) : (
                      <span className="inline-block size-4 shrink-0" />
                    )}
                    <KindIcon kind={span.span_kind} />
                    <span className="min-w-0 truncate">{span.name}</span>
                    {span.status === "error" ? <StatusDot status="error" /> : null}
                    {row.detached ? (
                      <Badge variant="secondary" className="h-4 px-1.5 text-[10px]">
                        detached
                      </Badge>
                    ) : null}
                  </div>
                </div>
                <div role="gridcell" className="num truncate px-2 text-xs">
                  {formatDuration(span.duration_ns)}
                </div>
                <div role="gridcell" className="num truncate px-2 text-xs">
                  {showTokens ? formatTokens(span.total_tokens) : ""}
                </div>
                <div role="gridcell" className="num truncate px-2 text-xs">
                  {span.cost_total === undefined ? "" : formatCost(span.cost_total)}
                </div>
                <div role="gridcell" className="relative h-full">
                  <div
                    className={cn(
                      "absolute top-1/2 h-3 -translate-y-1/2 rounded-sm",
                      BAR[span.span_kind],
                      span.status === "error" && "outline outline-1 outline-status-error",
                    )}
                    style={{ left: `${bar.leftPct}%`, width: `${bar.widthPct}%` }}
                  />
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

function collapsedWithPath(
  spans: readonly Schema<"TreeSpan">[],
  spanId: string | undefined,
): Set<string> {
  const collapsed = defaultCollapsed(spans);
  const index = spans.findIndex((row) => row.span.span_id === spanId);
  if (index < 0) {
    return collapsed;
  }
  return expandAncestors(spans, collapsed, index);
}

function expandAncestors(
  spans: readonly Schema<"TreeSpan">[],
  collapsed: Set<string>,
  index: number,
): Set<string> {
  let changed = false;
  const next = new Set(collapsed);
  for (const ancestorIndex of ancestorsOf(spans, index)) {
    const ancestor = spans[ancestorIndex];
    if (ancestor && next.delete(ancestor.span.span_id)) {
      changed = true;
    }
  }
  return changed ? next : collapsed;
}
