import type { ReactNode } from "react";
import type { Schema } from "@/api/types";
import { IdText } from "@/components/id-text";
import { KindIcon } from "@/components/kind-icon";
import { StatusDot } from "@/components/status-dot";
import { Badge } from "@/components/ui/badge";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { formatCost, formatDuration, formatTokens } from "@/lib/format";

export function SpanHeader({ span, detached }: { span: Schema<"SpanDetail">; detached: boolean }) {
  const tokens = tokenSummary(span);
  const modelsDiffer =
    span.request_model !== undefined &&
    span.response_model !== undefined &&
    span.request_model !== span.response_model;

  return (
    <header className="flex shrink-0 flex-col gap-1 border-b px-3 py-1.5 text-xs">
      <div className="flex min-w-0 items-center gap-2">
        <KindIcon kind={span.span_kind} />
        <span className="min-w-0 truncate select-text">{span.name}</span>
        <StatusDot status={span.status} />
        {span.status_message ? (
          <span className="min-w-0 truncate select-text">{span.status_message}</span>
        ) : null}
        <IdText value={span.span_id} kind="span" />
      </div>
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-muted-foreground">
        {span.model ? (
          <Pair label="model">
            {modelsDiffer ? (
              <Tooltip>
                <TooltipTrigger asChild>
                  <span className="text-foreground">{span.model}</span>
                </TooltipTrigger>
                <TooltipContent className="flex-col items-start">
                  <span>request {span.request_model}</span>
                  <span>response {span.response_model}</span>
                </TooltipContent>
              </Tooltip>
            ) : (
              <span className="text-foreground">{span.model}</span>
            )}
          </Pair>
        ) : null}
        {span.provider ? <Pair label="provider">{span.provider}</Pair> : null}
        {span.operation ? <Pair label="operation">{span.operation}</Pair> : null}
        <Pair label="duration">{formatDuration(span.duration_ns)}</Pair>
        {span.ttft_ns !== undefined ? (
          <Pair label="ttft">{formatDuration(span.ttft_ns)}</Pair>
        ) : null}
        {tokens ? <span className="text-foreground">{tokens}</span> : null}
        {span.cost_total !== undefined ? (
          <Pair label="cost">
            <Tooltip>
              <TooltipTrigger asChild>
                <span className="num text-foreground">{formatCost(span.cost_total)}</span>
              </TooltipTrigger>
              <TooltipContent className="flex-col items-start">
                <span>input {formatCost(span.cost_input)}</span>
                <span>output {formatCost(span.cost_output)}</span>
                <span>{span.cost_source ?? "none"}</span>
              </TooltipContent>
            </Tooltip>
          </Pair>
        ) : null}
        {span.service_name ? <Pair label="service">{span.service_name}</Pair> : null}
        {span.environment ? <Pair label="environment">{span.environment}</Pair> : null}
        {span.release ? <Pair label="release">{span.release}</Pair> : null}
        {span.session_id ? <Pair label="session">{span.session_id}</Pair> : null}
        {span.user_id ? <Pair label="user">{span.user_id}</Pair> : null}
        {span.truncated.length > 0 ? (
          <Tooltip>
            <TooltipTrigger asChild>
              <Badge variant="outline" className="h-4 px-1.5 text-[10px] text-muted-foreground">
                truncated at ingest
              </Badge>
            </TooltipTrigger>
            <TooltipContent>{span.truncated.join(", ")}</TooltipContent>
          </Tooltip>
        ) : null}
        {detached ? (
          <Badge variant="secondary" className="h-4 px-1.5 text-[10px]">
            detached
          </Badge>
        ) : null}
        {span.conventions.map((convention) => (
          <Badge
            key={convention}
            variant="outline"
            className="h-4 px-1.5 text-[10px] text-muted-foreground"
          >
            {convention}
          </Badge>
        ))}
      </div>
    </header>
  );
}

function Pair({ label, children }: { label: string; children: ReactNode }) {
  return (
    <span className="inline-flex items-center gap-1">
      <span>{label}</span>
      <span className="text-foreground">{children}</span>
    </span>
  );
}

/** `in 1.2k · out 40 · cache 1.0k`, with a part only when that count is present. */
function tokenSummary(span: Schema<"SpanDetail">): string | undefined {
  const parts: string[] = [];
  if (span.input_tokens !== undefined) {
    parts.push(`in ${formatTokens(span.input_tokens)}`);
  }
  if (span.output_tokens !== undefined) {
    parts.push(`out ${formatTokens(span.output_tokens)}`);
  }
  if (span.cache_read_tokens !== undefined) {
    parts.push(`cache ${formatTokens(span.cache_read_tokens)}`);
  }
  if (span.cache_write_tokens !== undefined) {
    parts.push(`cache write ${formatTokens(span.cache_write_tokens)}`);
  }
  if (span.reasoning_tokens !== undefined) {
    parts.push(`reasoning ${formatTokens(span.reasoning_tokens)}`);
  }
  return parts.length > 0 ? parts.join(" · ") : undefined;
}
