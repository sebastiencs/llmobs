import { useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { cn } from "cn";
import { SquareArrowOutUpRight, Trash2, X } from "lucide-react";
import { useState } from "react";
import { $api } from "@/api/client";
import { problemMessage } from "@/api/problem";
import { traceDetailQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { forgetTrace } from "@/components/command/recent-traces";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { CopyButton } from "@/components/copy-button";
import { DisabledReason } from "@/components/disabled-reason";
import { IdText } from "@/components/id-text";
import { useProject } from "@/components/shell/project-context";
import { StatusDot } from "@/components/status-dot";
import { TimeText } from "@/components/time-text";
import { Badge } from "@/components/ui/badge";
import { Button, buttonVariants } from "@/components/ui/button";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { invalidateProjectReads } from "@/lib/deletions";
import { formatCost, formatDuration, formatTokens } from "@/lib/format";
import { atLeast } from "@/lib/roles";
import type { TraceMode } from "@/lib/search/traces";
import type { StreamStatus } from "@/lib/sse";

const LIVE_DOT: Record<StreamStatus, string> = {
  connecting: "bg-muted-foreground",
  open: "bg-status-ok",
  retrying: "bg-status-error",
  closed: "bg-muted-foreground",
};

export function TraceHeader({
  trace,
  spanId,
  mode,
  onClose,
  live,
}: {
  trace: Schema<"TraceSummary">;
  spanId?: string;
  mode?: TraceMode;
  onClose?: () => void;
  live?: { status: StreamStatus; onStop: () => void };
}) {
  const { org, project, ref, data } = useProject();
  const search = pageSearch(spanId, mode);
  const href = traceHref(org, project, trace.trace_id, search);
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [confirmOpen, setConfirmOpen] = useState(false);
  const deletion = $api.useMutation("post", "/api/v1/projects/{project}/deletions");
  const canDelete = atLeast(data.role, "admin");

  function deleteTrace() {
    deletion.mutate(
      {
        params: { path: { project: ref } },
        body: { kind: "trace_ids", trace_ids: [trace.trace_id] },
      },
      {
        onSuccess: async () => {
          setConfirmOpen(false);
          forgetTrace(trace.trace_id);
          if (onClose) {
            onClose();
          } else {
            await navigate({ to: "/$org/$project/traces", params: { org, project } });
          }
          // Drop the cached detail so a view still mounted does not flash "Could not load this trace".
          queryClient.removeQueries({
            queryKey: traceDetailQuery(org, project, trace.trace_id).queryKey,
          });
          void invalidateProjectReads(queryClient);
        },
      },
    );
  }

  return (
    <TooltipProvider>
      <section
        aria-label="Trace"
        className="flex shrink-0 flex-col gap-1 border-b px-3 py-1.5 text-xs"
      >
        <div className="flex min-w-0 items-center gap-2">
          <StatusDot status={trace.status} />
          <span className="min-w-0 truncate font-medium">{trace.name}</span>
          <IdText value={trace.trace_id} kind="trace" />
          <TimeText value={trace.start_time} style="datetime" />
          <div className="ml-auto flex shrink-0 items-center gap-1">
            {live ? (
              <span className="inline-flex items-center gap-1">
                <span role="status" className="inline-flex">
                  <span
                    aria-hidden="true"
                    className={cn("size-2 shrink-0 rounded-full", LIVE_DOT[live.status])}
                  />
                  <span className="sr-only">{live.status}</span>
                </span>
                <Badge variant="secondary" className="h-4 px-1.5 text-[10px]">
                  Live
                </Badge>
                <Button type="button" variant="ghost" size="xs" onClick={live.onStop}>
                  Stop
                </Button>
              </span>
            ) : null}
            <DisabledReason
              reason={canDelete ? undefined : "Only project admins can delete traces"}
            >
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                aria-label="Delete trace"
                disabled={!canDelete}
                onClick={() => {
                  deletion.reset();
                  setConfirmOpen(true);
                }}
              >
                <Trash2 />
              </Button>
            </DisabledReason>
            <CopyButton value={href} label="link" />
            {onClose ? (
              <Link
                to="/$org/$project/traces/$traceId"
                params={{ org, project, traceId: trace.trace_id }}
                search={search}
                aria-label="Open full page"
                className={cn(buttonVariants({ variant: "ghost", size: "icon-xs" }))}
              >
                <SquareArrowOutUpRight />
              </Link>
            ) : null}
            {onClose ? (
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                aria-label="Close"
                onClick={onClose}
              >
                <X />
              </Button>
            ) : null}
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-muted-foreground">
          <span className="text-foreground">{formatDuration(trace.duration_ns)}</span>
          <span>{trace.span_count} spans</span>
          <span>{trace.llm_count} LLM calls</span>
          <Tokens trace={trace} />
          {trace.cost_total !== undefined ? (
            <span className="num text-foreground">{formatCost(trace.cost_total)}</span>
          ) : null}
          {trace.session_id ? (
            <span>
              session{" "}
              <Link
                to="/$org/$project/sessions/$sessionId"
                params={{ org, project, sessionId: trace.session_id }}
                className="focus-ring rounded-sm text-foreground underline-offset-2 hover:underline"
              >
                {trace.session_id}
              </Link>
            </span>
          ) : null}
          {trace.user_id ? (
            <span>
              user{" "}
              <Link
                to="/$org/$project/users/$userId"
                params={{ org, project, userId: trace.user_id }}
                className="focus-ring rounded-sm text-foreground underline-offset-2 hover:underline"
              >
                {trace.user_id}
              </Link>
            </span>
          ) : null}
          {trace.environment ? <span>{trace.environment}</span> : null}
          {trace.release ? <span>{trace.release}</span> : null}
          {trace.tags.map((tag) => (
            <Badge key={tag} variant="outline" className="h-4 px-1.5 text-[10px]">
              {tag}
            </Badge>
          ))}
          {scoreBadges(trace.scores).map((score) => (
            <Badge key={score.score_id} variant="secondary" className="h-4 px-1.5 text-[10px]">
              {score.name} {badgeValue(score.value)}
            </Badge>
          ))}
        </div>
      </section>
      <ConfirmDialog
        open={confirmOpen}
        onOpenChange={setConfirmOpen}
        title="Delete this trace?"
        description="Its spans, scores and evaluation traces are hidden within seconds and erased within 24 hours. This cannot be undone."
        confirmLabel="Delete trace"
        destructive
        pending={deletion.isPending}
        error={
          deletion.error ? problemMessage(deletion.error, "Could not delete the trace.") : undefined
        }
        onConfirm={deleteTrace}
      />
    </TooltipProvider>
  );
}

function Tokens({ trace }: { trace: Schema<"TraceSummary"> }) {
  if (trace.total_tokens === undefined) {
    return null;
  }
  const breakdown = tokenBreakdown(trace);
  const total = <span className="text-foreground">{formatTokens(trace.total_tokens)}</span>;
  if (!breakdown) {
    return total;
  }
  return (
    <Tooltip>
      <TooltipTrigger asChild>{total}</TooltipTrigger>
      <TooltipContent>{breakdown}</TooltipContent>
    </Tooltip>
  );
}

function tokenBreakdown(trace: Schema<"TraceSummary">): string | undefined {
  const parts: string[] = [];
  if (trace.input_tokens !== undefined) {
    parts.push(`in ${formatTokens(trace.input_tokens)}`);
  }
  if (trace.output_tokens !== undefined) {
    parts.push(`out ${formatTokens(trace.output_tokens)}`);
  }
  if (trace.cache_read_tokens !== undefined) {
    parts.push(`cache ${formatTokens(trace.cache_read_tokens)}`);
  }
  if (trace.cache_write_tokens !== undefined) {
    parts.push(`cache write ${formatTokens(trace.cache_write_tokens)}`);
  }
  if (trace.reasoning_tokens !== undefined) {
    parts.push(`reasoning ${formatTokens(trace.reasoning_tokens)}`);
  }
  return parts.length > 0 ? parts.join(" · ") : undefined;
}

function pageSearch(spanId: string | undefined, mode: TraceMode | undefined) {
  return {
    ...(spanId ? { span: spanId } : {}),
    ...(mode ? { mode } : {}),
  };
}

function traceHref(
  org: string,
  project: string,
  traceId: string,
  search: { span?: string; mode?: TraceMode },
): string {
  const params = new URLSearchParams();
  if (search.span) {
    params.set("span", search.span);
  }
  if (search.mode) {
    params.set("mode", search.mode);
  }
  const query = params.toString();
  return new URL(
    `/${encodeURIComponent(org)}/${encodeURIComponent(project)}/traces/${encodeURIComponent(traceId)}${query ? `?${query}` : ""}`,
    window.location.origin,
  ).href;
}

function scoreBadges(scores: Schema<"ScoreBrief">[]): Schema<"ScoreBrief">[] {
  return [...scores].sort((left, right) => left.name.localeCompare(right.name));
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
