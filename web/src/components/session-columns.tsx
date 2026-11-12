import { cn } from "cn";
import type { Schema } from "@/api/types";
import type { ColumnSpec } from "@/components/data-table/column-layout";
import type { DataTableColumn } from "@/components/data-table/data-table";
import { TimeText } from "@/components/time-text";
import { formatScore } from "@/components/trace-columns";
import { Badge } from "@/components/ui/badge";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { formatCost, formatDuration, formatTokens } from "@/lib/format";
import { SESSION_COLUMN_IDS, type SessionColumnId } from "@/lib/search/sessions";

const HIDDEN = new Set<SessionColumnId>(["tags"]);

const LABELS: Record<SessionColumnId, string> = {
  session: "Session",
  start: "Start",
  last: "Last activity",
  traces: "Traces",
  user: "User",
  duration: "Duration",
  tokens: "Tokens",
  cost: "Cost",
  errors: "Errors",
  tags: "Tags",
};

const SOURCE_LABEL: Record<Schema<"ScoreSource">, string> = {
  api: "API",
  sdk: "SDK",
  human: "Human",
  end_user: "End user",
  llm_judge: "LLM judge",
  check: "Check",
  webhook: "Webhook",
};

export type SessionColumn = ColumnSpec & { label: string };

export function sessionScoreNames(rows: readonly Schema<"SessionSummary">[]): string[] {
  const names = new Set<string>();
  for (const row of rows) {
    for (const score of row.scores) {
      names.add(score.name);
    }
  }
  return [...names].sort((left, right) => left.localeCompare(right));
}

export function sessionColumnSpecs(names: readonly string[]): SessionColumn[] {
  const fixed = SESSION_COLUMN_IDS.map((id) => ({
    id,
    label: LABELS[id],
    ...(!HIDDEN.has(id) ? { defaultVisible: true } : {}),
  }));
  const scores: SessionColumn[] = names.map((name) => ({
    id: `score:${name}`,
    label: name,
    defaultVisible: true,
  }));
  return [...fixed, ...scores];
}

export function orderedSessionColumns(
  rows: readonly Schema<"SessionSummary">[],
  visible: readonly string[],
): DataTableColumn<Schema<"SessionSummary">>[] {
  const byId = new Map(
    sessionColumnDefs(sessionScoreNames(rows)).map((column) => [column.id, column]),
  );
  return visible.flatMap((id) => {
    const column = byId.get(id);
    return column ? [column] : [];
  });
}

function sessionColumnDefs(names: readonly string[]): DataTableColumn<Schema<"SessionSummary">>[] {
  const fixed: DataTableColumn<Schema<"SessionSummary">>[] = [
    {
      id: "session",
      size: 160,
      header: "Session",
      enableSorting: false,
      meta: { mono: true },
      cell: ({ row }) => row.original.session_id,
    },
    {
      id: "start",
      accessorFn: (row) => row.start_time,
      size: 168,
      header: "Start",
      enableSorting: true,
      cell: ({ row }) => <TimeText value={row.original.start_time} style="datetime" />,
    },
    {
      id: "last",
      accessorFn: (row) => row.end_time,
      size: 168,
      header: "Last activity",
      enableSorting: true,
      cell: ({ row }) => <TimeText value={row.original.end_time} style="datetime" />,
    },
    {
      id: "traces",
      accessorFn: (row) => row.trace_count,
      size: 72,
      header: "Traces",
      enableSorting: true,
      meta: { align: "right" },
      cell: ({ row }) => <span className="num">{row.original.trace_count}</span>,
    },
    {
      id: "user",
      size: 140,
      header: "User",
      enableSorting: false,
      meta: { mono: true },
      cell: ({ row }) => row.original.user_id ?? "—",
    },
    {
      id: "duration",
      accessorFn: (row) => row.duration_ns,
      size: 88,
      header: "Duration",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => <span className="num">{formatDuration(row.original.duration_ns)}</span>,
    },
    {
      id: "tokens",
      accessorFn: (row) => row.total_tokens,
      size: 80,
      header: "Tokens",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => <span className="num">{formatTokens(row.original.total_tokens)}</span>,
    },
    {
      id: "cost",
      accessorFn: (row) => row.cost_total,
      size: 88,
      header: "Cost",
      enableSorting: true,
      meta: { align: "right" },
      cell: ({ row }) => <span className="num">{formatCost(row.original.cost_total)}</span>,
    },
    {
      id: "errors",
      accessorFn: (row) => row.error_count,
      size: 72,
      header: "Errors",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => (
        <span className={cn("num", row.original.error_count > 0 && "text-destructive")}>
          {row.original.error_count}
        </span>
      ),
    },
    {
      id: "tags",
      size: 140,
      header: "Tags",
      enableSorting: false,
      cell: ({ row }) => <Tags tags={row.original.tags} />,
    },
  ];
  return [...fixed, ...names.map((name) => scoreColumn(name))];
}

function scoreColumn(name: string): DataTableColumn<Schema<"SessionSummary">> {
  return {
    id: `score:${name}`,
    size: 96,
    header: name,
    enableSorting: false,
    cell: ({ row }) => <ScoreCell session={row.original} name={name} />,
  };
}

function ScoreCell({ session, name }: { session: Schema<"SessionSummary">; name: string }) {
  const score = session.scores.find((item) => item.name === name);
  if (!score) {
    return <span className="text-muted-foreground">—</span>;
  }
  const numeric = score.value.data_type === "numeric" || score.value.data_type === "boolean";
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span className={numeric ? "num" : "block truncate"}>{formatScore(score.value)}</span>
      </TooltipTrigger>
      <TooltipContent>{SOURCE_LABEL[score.source]}</TooltipContent>
    </Tooltip>
  );
}

function Tags({ tags }: { tags: string[] }) {
  if (tags.length === 0) {
    return "—";
  }
  const shown = tags.slice(0, 2);
  const rest = tags.length - shown.length;
  return (
    <span className="flex min-w-0 items-center gap-1">
      {shown.map((tag) => (
        <Badge key={tag} variant="secondary" className="h-4 max-w-24 truncate px-1.5 text-[10px]">
          {tag}
        </Badge>
      ))}
      {rest > 0 ? <span className="text-[10px] text-muted-foreground">+{rest}</span> : null}
    </span>
  );
}
