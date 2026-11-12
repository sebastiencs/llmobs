import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useVirtualizer } from "@tanstack/react-virtual";
import { cn } from "cn";
import { ChevronDown, ChevronRight } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { spanDetailQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { KindIcon } from "@/components/kind-icon";
import { MessageView } from "@/components/payload/message-view";
import { ToolCallCard } from "@/components/payload/tool-call-card";
import { useProject } from "@/components/shell/project-context";
import { TreeToolbar } from "@/components/trace/tree-toolbar";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { formatCost, formatDuration, formatTokens } from "@/lib/format";
import type { TraceMode } from "@/lib/search/traces";
import {
  freshInput,
  messageText,
  payloadMessages,
  type TranscriptItem,
  transcriptSpans,
} from "@/lib/transcript";

const PAGE = 50;
const CONCURRENCY = 4;
const OVERSCAN = 6;

function rowEstimate(row: Row | undefined): number {
  return row?.kind === "section" ? 24 : 72;
}

const NO_LINKS = { callNames: new Map<string, string>(), responseIds: new Set<string>() };

type TurnItem = Extract<TranscriptItem, { kind: "turn" }>;
type SectionItem = Extract<TranscriptItem, { kind: "section" }>;

type Row =
  | { kind: "section"; item: SectionItem; key: string }
  | { kind: "turn"; item: TurnItem; previousLlmId?: string; key: string }
  | { kind: "more"; key: "more" };

export function TranscriptView({
  traceId,
  spans,
  selectedSpanId,
  onSelectSpan,
  mode,
  onMode,
}: {
  traceId: string;
  spans: readonly Schema<"TreeSpan">[];
  selectedSpanId?: string;
  onSelectSpan: (spanId: string) => void;
  mode: TraceMode;
  onMode: (mode: TraceMode) => void;
}) {
  const items = useMemo(() => transcriptSpans(spans), [spans]);
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(() => new Set());
  const [cap, setCap] = useState(PAGE);
  const rows = useMemo(() => visibleRows(items, collapsed, cap), [items, collapsed, cap]);
  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: rows.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: (index) => rowEstimate(rows[index]),
    measureElement: (element) => {
      const height = element.getBoundingClientRect().height;
      const index = Number(element.getAttribute("data-index"));
      return height > 0 ? height : rowEstimate(rows[index]);
    },
    overscan: OVERSCAN,
  });
  const visibleIds = virtualizer
    .getVirtualItems()
    .map((item) => rows[item.index])
    .flatMap((row) => (row?.kind === "turn" ? [row.item.span.span.span_id] : []));
  useDetailLoader(traceId, visibleIds);

  return (
    <section aria-label="Transcript" className="flex min-h-0 flex-1 flex-col">
      <TreeToolbar mode={mode} onMode={onMode} />
      {items.length === 0 ? (
        <p className="p-3 text-xs text-muted-foreground">
          No LLM or tool calls in this trace. Switch to Tree.
        </p>
      ) : (
        <div
          ref={scrollRef}
          data-scroll-container
          className="relative min-h-0 flex-1 overflow-auto"
        >
          <div className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
            {virtualizer.getVirtualItems().map((item) => {
              const row = rows[item.index];
              if (!row) return null;
              return (
                <div
                  key={row.key}
                  data-index={item.index}
                  ref={virtualizer.measureElement}
                  className="absolute top-0 left-0 w-full"
                  style={{ transform: `translateY(${item.start}px)` }}
                >
                  {row.kind === "section" ? (
                    <SectionRow
                      item={row.item}
                      collapsed={collapsed.has(row.item.span.span.span_id)}
                      onToggle={() => toggle(setCollapsed, row.item.span.span.span_id)}
                    />
                  ) : null}
                  {row.kind === "turn" ? (
                    <TurnRow
                      traceId={traceId}
                      item={row.item}
                      previousLlmId={row.previousLlmId}
                      selected={row.item.span.span.span_id === selectedSpanId}
                      onSelect={() => onSelectSpan(row.item.span.span.span_id)}
                    />
                  ) : null}
                  {row.kind === "more" ? (
                    <div className="p-2">
                      <Button
                        type="button"
                        variant="outline"
                        size="xs"
                        onClick={() => setCap((current) => current + PAGE)}
                      >
                        Load more
                      </Button>
                    </div>
                  ) : null}
                </div>
              );
            })}
          </div>
        </div>
      )}
    </section>
  );
}

function SectionRow({
  item,
  collapsed,
  onToggle,
}: {
  item: SectionItem;
  collapsed: boolean;
  onToggle: () => void;
}) {
  return (
    <button
      type="button"
      aria-expanded={!collapsed}
      className="flex h-6 w-full items-center gap-1 text-left text-xs"
      style={{ paddingLeft: 8 + item.depth * 12 }}
      onClick={onToggle}
    >
      {collapsed ? <ChevronRight className="size-3" /> : <ChevronDown className="size-3" />}
      <KindIcon kind={item.span.span.span_kind} />
      <span className="min-w-0 truncate">{item.span.span.name}</span>
    </button>
  );
}

function TurnRow({
  traceId,
  item,
  previousLlmId,
  selected,
  onSelect,
}: {
  traceId: string;
  item: TurnItem;
  previousLlmId?: string;
  selected: boolean;
  onSelect: () => void;
}) {
  const { org, project, ref } = useProject();
  const span = item.span.span;
  const detail = useQuery({
    ...spanDetailQuery(org, project, traceId, span.span_id),
    enabled: false,
  });
  const previous = useQuery({
    ...spanDetailQuery(org, project, traceId, previousLlmId ?? span.span_id),
    enabled: false,
  });
  const loaded = detail.data;
  return (
    <div
      className={cn(
        "flex flex-col gap-1 border-b px-2 py-1",
        selected ? "bg-selection shadow-[inset_2px_0_0_var(--primary)]" : "hover:bg-muted",
      )}
    >
      <button
        type="button"
        className="flex w-full items-center gap-2 text-left text-xs"
        onClick={onSelect}
      >
        <KindIcon kind={span.span_kind} />
        <span className="min-w-0 truncate font-medium">{span.model ?? span.name}</span>
        <span className="num shrink-0 text-muted-foreground">
          {formatDuration(span.duration_ns)}
        </span>
        {span.total_tokens !== undefined ? (
          <span className="num shrink-0 text-muted-foreground">
            {formatTokens(span.total_tokens)}
          </span>
        ) : null}
        {span.cost_total !== undefined ? (
          <span className="num shrink-0 text-muted-foreground">{formatCost(span.cost_total)}</span>
        ) : null}
      </button>
      {loaded ? (
        span.span_kind === "tool" ? (
          <ToolTurn detail={loaded} />
        ) : (
          <LlmTurn
            detail={loaded}
            previous={previousLlmId ? previous.data : undefined}
            projectRef={ref}
          />
        )
      ) : detail.isError ? (
        <p className="text-xs">Could not load this span</p>
      ) : (
        <Skeleton className="h-4 w-2/3" />
      )}
    </div>
  );
}

function LlmTurn({
  detail,
  previous,
  projectRef,
}: {
  detail: Schema<"SpanDetail">;
  previous: Schema<"SpanDetail"> | undefined;
  projectRef: string;
}) {
  const seen = previous
    ? [...payloadMessages(previous.input), ...payloadMessages(previous.output)]
    : undefined;
  const input = freshInput(seen, payloadMessages(detail.input));
  const output = payloadMessages(detail.output);
  return (
    <div className="flex flex-col gap-2">
      {input.map((message) => (
        <MessageView
          key={messageKey(message, "in")}
          message={message}
          projectRef={projectRef}
          links={NO_LINKS}
        />
      ))}
      {output.map((message) => (
        <MessageView
          key={messageKey(message, "out")}
          message={message}
          projectRef={projectRef}
          links={NO_LINKS}
        />
      ))}
    </div>
  );
}

function ToolTurn({ detail }: { detail: Schema<"SpanDetail"> }) {
  const callId = `tool-call-${detail.span_id}`;
  const responseId = `tool-response-${detail.span_id}`;
  return (
    <div className="flex flex-col gap-1">
      <ToolCallCard
        anchorId={callId}
        title={detail.name}
        id={detail.span_id}
        body={detail.input?.value}
        forwardTargetId={responseId}
      />
      <ToolCallCard
        anchorId={responseId}
        title="response"
        titleTargetId={callId}
        body={detail.output?.value}
      />
    </div>
  );
}

function useDetailLoader(traceId: string, visibleIds: readonly string[]) {
  const { org, project } = useProject();
  const client = useQueryClient();
  const inflight = useRef(new Set<string>());
  const [tick, setTick] = useState(0);

  useEffect(() => {
    // `tick` changes when a prefetch settles, which frees a concurrency slot.
    void tick;
    const room = CONCURRENCY - inflight.current.size;
    if (room <= 0) return;
    const next: string[] = [];
    for (const id of visibleIds) {
      if (next.length >= room) break;
      if (inflight.current.has(id)) continue;
      const state = client.getQueryState(spanDetailQuery(org, project, traceId, id).queryKey);
      if (
        state?.status === "success" ||
        state?.status === "error" ||
        state?.fetchStatus === "fetching"
      ) {
        continue;
      }
      next.push(id);
    }
    for (const id of next) {
      inflight.current.add(id);
      // A finished prefetch frees a slot. prefetchQuery swallows cancellation.
      void client.prefetchQuery(spanDetailQuery(org, project, traceId, id)).finally(() => {
        inflight.current.delete(id);
        setTick((current) => current + 1);
      });
    }
  }, [client, org, project, traceId, tick, visibleIds]);
}

const messageCounts = new WeakMap<Schema<"Message">, string>();
let messageCount = 0;

function messageKey(message: Schema<"Message">, side: string): string {
  const cached = messageCounts.get(message);
  if (cached) return cached;
  messageCount += 1;
  const key = `${side}:${message.role}:${messageText(message)}:${messageCount}`;
  messageCounts.set(message, key);
  return key;
}

function visibleRows(
  items: readonly TranscriptItem[],
  collapsed: ReadonlySet<string>,
  cap: number,
): Row[] {
  const rows: Row[] = [];
  let hiddenDepth: number | null = null;
  let turnIndex = 0;
  let previousLlmId: string | undefined;
  let more = false;
  for (const item of items) {
    if (item.kind === "section") {
      if (hiddenDepth !== null && item.depth <= hiddenDepth) hiddenDepth = null;
      if (hiddenDepth !== null) continue;
      rows.push({ kind: "section", item, key: item.span.span.span_id });
      if (collapsed.has(item.span.span.span_id)) hiddenDepth = item.depth;
      continue;
    }
    const ordinal = turnIndex;
    turnIndex += 1;
    const spanId = item.span.span.span_id;
    const llm = item.span.span.span_kind === "llm";
    if (hiddenDepth !== null) {
      if (llm) previousLlmId = spanId;
      continue;
    }
    if (ordinal >= cap) {
      more = true;
      if (llm) previousLlmId = spanId;
      continue;
    }
    rows.push({
      kind: "turn",
      item,
      key: spanId,
      ...(llm && previousLlmId ? { previousLlmId } : {}),
    });
    if (llm) previousLlmId = spanId;
  }
  if (more) rows.push({ kind: "more", key: "more" });
  return rows;
}

function toggle(
  setCollapsed: (update: (current: ReadonlySet<string>) => ReadonlySet<string>) => void,
  spanId: string,
) {
  setCollapsed((current) => {
    const next = new Set(current);
    if (next.has(spanId)) next.delete(spanId);
    else next.add(spanId);
    return next;
  });
}
