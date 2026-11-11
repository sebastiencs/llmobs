import { useQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { useVirtualizer } from "@tanstack/react-virtual";
import { cn } from "cn";
import { useRef, useState } from "react";
import { problemOf } from "@/api/problem";
import { sessionQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { IdText } from "@/components/id-text";
import { ChatView } from "@/components/payload/chat-view";
import { PayloadView } from "@/components/payload/payload-view";
import { useProject } from "@/components/shell/project-context";
import { StatusDot } from "@/components/status-dot";
import { TimeText } from "@/components/time-text";
import { SplitPane } from "@/components/trace/split-pane";
import { TraceView } from "@/components/trace/trace-view";
import { Badge } from "@/components/ui/badge";
import { Skeleton } from "@/components/ui/skeleton";
import { formatCost, formatDuration, formatTokens } from "@/lib/format";
import type { SessionPageSearch } from "@/lib/search/sessions";

const TURN_ESTIMATE = 96;
const OVERSCAN = 6;

export function SessionView({
  sessionId,
  search,
}: {
  sessionId: string;
  search: SessionPageSearch;
}) {
  const { org, project, ref } = useProject();
  const navigate = useNavigate();
  const detail = useQuery(
    sessionQuery(org, project, sessionId, {
      includeEval: search.eval === 1,
      includeExperiments: search.exp === 1,
    }),
  );
  const [span, setSpan] = useState<{ traceId?: string; spanId?: string }>({});
  const traceId = search.trace;
  const spanId = span.traceId === traceId ? span.spanId : undefined;

  if (detail.isPending) {
    return (
      <div className="flex flex-col gap-2 p-3">
        <Skeleton className="h-4 w-1/2" />
        <Skeleton className="h-4 w-2/3" />
      </div>
    );
  }
  if (detail.isError) {
    return (
      <p className="p-3 text-sm">
        {problemOf(detail.error)?.title ?? "Could not load this session"}
      </p>
    );
  }

  const { session, turns, truncated } = detail.data;
  function selectTurn(next: string) {
    void navigate({
      to: "/$org/$project/sessions/$sessionId",
      params: { org, project, sessionId },
      search: (prev: SessionPageSearch) => ({ ...prev, trace: next }),
      replace: true,
    });
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <SessionHeader session={session} />
      {truncated ? (
        <div className="shrink-0 border-b bg-muted px-3 py-1 text-xs">
          Showing the first 1 000 turns
        </div>
      ) : null}
      <div className="relative min-h-0 flex-1">
        <div className="absolute inset-0">
          <SplitPane
            name="session"
            start={
              <TurnList
                turns={turns}
                selectedTraceId={traceId}
                onSelect={selectTurn}
                projectRef={ref}
              />
            }
            end={
              traceId ? (
                <TraceView
                  key={traceId}
                  traceId={traceId}
                  spanId={spanId}
                  onSelectSpan={(next) => setSpan({ traceId, spanId: next })}
                  onClose={() => {
                    void navigate({
                      to: "/$org/$project/sessions/$sessionId",
                      params: { org, project, sessionId },
                      search: (prev: SessionPageSearch) => {
                        const { trace: _trace, ...rest } = prev;
                        return rest;
                      },
                      replace: true,
                    });
                  }}
                />
              ) : (
                <p className="p-3 text-xs text-muted-foreground">Select a turn</p>
              )
            }
          />
        </div>
      </div>
    </div>
  );
}

function SessionHeader({ session }: { session: Schema<"SessionSummary"> }) {
  const { org, project } = useProject();
  return (
    <header className="flex shrink-0 flex-col gap-1 border-b px-3 py-1.5 text-xs">
      <div className="flex min-w-0 items-center gap-2">
        <IdText value={session.session_id} kind="session" />
        {session.user_id ? (
          <Link
            to="/$org/$project/users/$userId"
            params={{ org, project, userId: session.user_id }}
            className="focus-ring rounded-sm font-mono underline-offset-2 hover:underline"
          >
            {session.user_id}
          </Link>
        ) : null}
      </div>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-muted-foreground">
        <span>
          <TimeText value={session.start_time} style="datetime" />
          <span aria-hidden="true"> → </span>
          <TimeText value={session.end_time} style="datetime" />
        </span>
        <span>{session.trace_count} turns</span>
        {session.total_tokens !== undefined ? (
          <span className="num text-foreground">{formatTokens(session.total_tokens)}</span>
        ) : null}
        {session.cost_total !== undefined ? (
          <span className="num text-foreground">{formatCost(session.cost_total)}</span>
        ) : null}
        <span className={cn(session.error_count > 0 && "text-destructive")}>
          {session.error_count} errors
        </span>
        {session.tags.map((tag) => (
          <Badge key={tag} variant="outline" className="h-4 px-1.5 text-[10px]">
            {tag}
          </Badge>
        ))}
        {[...session.scores]
          .sort((left, right) => left.name.localeCompare(right.name))
          .map((score) => (
            <Badge key={score.score_id} variant="secondary" className="h-4 px-1.5 text-[10px]">
              {score.name} {badgeValue(score.value)}
            </Badge>
          ))}
      </div>
    </header>
  );
}

function TurnList({
  turns,
  selectedTraceId,
  onSelect,
  projectRef,
}: {
  turns: readonly Schema<"SessionTurn">[];
  selectedTraceId?: string;
  onSelect: (traceId: string) => void;
  projectRef: string;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const virtualizer = useVirtualizer({
    count: turns.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => TURN_ESTIMATE,
    measureElement: (element) => {
      const height = element.getBoundingClientRect().height;
      return height > 0 ? height : TURN_ESTIMATE;
    },
    overscan: OVERSCAN,
  });

  return (
    <section aria-label="Conversation" className="flex min-h-0 flex-1 flex-col">
      <div ref={scrollRef} data-scroll-container className="min-h-0 flex-1 overflow-auto">
        <div className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
          {virtualizer.getVirtualItems().map((item) => {
            const turn = turns[item.index];
            if (!turn) {
              return null;
            }
            const traceId = turn.trace.trace_id;
            return (
              <div
                key={traceId}
                data-index={item.index}
                ref={virtualizer.measureElement}
                className="absolute top-0 left-0 w-full"
                style={{ transform: `translateY(${item.start}px)` }}
              >
                <Turn
                  index={item.index + 1}
                  turn={turn}
                  selected={traceId === selectedTraceId}
                  onSelect={() => onSelect(traceId)}
                  projectRef={projectRef}
                />
              </div>
            );
          })}
        </div>
      </div>
    </section>
  );
}

function Turn({
  index,
  turn,
  selected,
  onSelect,
  projectRef,
}: {
  index: number;
  turn: Schema<"SessionTurn">;
  selected: boolean;
  onSelect: () => void;
  projectRef: string;
}) {
  const trace = turn.trace;
  return (
    <div
      className={cn(
        "flex flex-col gap-2 border-b px-3 py-2",
        selected && "bg-selection shadow-[inset_2px_0_0_var(--primary)]",
      )}
    >
      <button
        type="button"
        className="focus-ring flex w-full items-center gap-2 rounded-sm text-left text-xs"
        aria-current={selected ? "true" : undefined}
        onClick={onSelect}
      >
        <span className="num w-6 shrink-0 text-muted-foreground">{index}</span>
        <TimeText value={trace.start_time} style="time" />
        <span className="min-w-0 truncate font-medium">{trace.name}</span>
        <span className="num shrink-0 text-muted-foreground">
          {formatDuration(trace.duration_ns)}
        </span>
        {trace.cost_total !== undefined ? (
          <span className="num shrink-0 text-muted-foreground">{formatCost(trace.cost_total)}</span>
        ) : null}
        <StatusDot status={trace.status} />
      </button>
      <TurnPayloads turn={turn} projectRef={projectRef} />
    </div>
  );
}

function TurnPayloads({ turn, projectRef }: { turn: Schema<"SessionTurn">; projectRef: string }) {
  if (turn.input === undefined && turn.output === undefined) {
    return <p className="text-xs text-muted-foreground">No root input/output</p>;
  }
  const input = messagesOf(turn.input);
  const output = messagesOf(turn.output);
  if (input && output) {
    return <ChatView input={input} output={output} projectRef={projectRef} />;
  }
  return (
    <div className="flex flex-col gap-2">
      <PayloadView payload={turn.input} label="Input" />
      <PayloadView payload={turn.output} label="Output" />
    </div>
  );
}

function messagesOf(payload: Schema<"Payload"> | undefined): Schema<"Message">[] | undefined {
  if (!payload) {
    return [];
  }
  if (payload.format === "messages") {
    return payload.value;
  }
  return undefined;
}

function badgeValue(value: Schema<"ScoreValue">): string {
  if (value.data_type === "numeric") {
    return value.value.toFixed(3).replace(/\.?0+$/, "");
  }
  if (value.data_type === "boolean") {
    return value.value ? "✓" : "✗";
  }
  if (value.data_type === "categorical") {
    return value.value.label;
  }
  return value.value;
}
