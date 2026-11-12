import type { Schema } from "@/api/types";
import type { ColumnSpec } from "@/components/data-table/column-layout";
import type { DataTableColumn } from "@/components/data-table/data-table";
import { KindIcon } from "@/components/kind-icon";
import { StatusDot } from "@/components/status-dot";
import { TimeText } from "@/components/time-text";
import { Badge } from "@/components/ui/badge";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { formatCost, formatDuration, formatTokens } from "@/lib/format";
import { SPAN_COLUMN_IDS, type SpanColumnId } from "@/lib/search/spans";

const TAGS_SHOWN = 2;

const HIDDEN = new Set<SpanColumnId>([
  "provider",
  "service",
  "release",
  "tags",
  "trace_id",
  "span_id",
  "parent_span_id",
]);
const LOCKED = new Set<SpanColumnId>(["status", "name"]);

const LABELS: Record<SpanColumnId, string> = {
  status: "Status",
  start: "Start",
  kind: "Kind",
  name: "Name",
  model: "Model",
  input: "Input",
  output: "Output",
  duration: "Duration",
  ttft: "TTFT",
  tokens: "Tokens",
  cost: "Cost",
  session: "Session",
  user: "User",
  env: "Env",
  provider: "Provider",
  service: "Service",
  release: "Release",
  tags: "Tags",
  trace_id: "Trace ID",
  span_id: "Span ID",
  parent_span_id: "Parent span ID",
};

const KIND_LABEL: Record<Schema<"SpanKind">, string> = {
  llm: "LLM",
  embedding: "Embedding",
  tool: "Tool",
  agent: "Agent",
  chain: "Chain",
  retriever: "Retriever",
  reranker: "Reranker",
  guardrail: "Guardrail",
  evaluator: "Evaluator",
  span: "Span",
};

export type SpanColumn = ColumnSpec & { label: string };

export function spanColumnSpecs(): SpanColumn[] {
  return SPAN_COLUMN_IDS.map((id) => ({
    id,
    label: LABELS[id],
    ...(LOCKED.has(id) ? { locked: true } : {}),
    ...(!HIDDEN.has(id) ? { defaultVisible: true } : {}),
  }));
}

export function orderedSpanColumns(
  visible: readonly string[],
): DataTableColumn<Schema<"SpanSummary">>[] {
  const byId = new Map(spanColumnDefs().map((column) => [column.id, column]));
  return visible.flatMap((id) => {
    const column = byId.get(id);
    return column ? [column] : [];
  });
}

function spanColumnDefs(): DataTableColumn<Schema<"SpanSummary">>[] {
  return [
    {
      id: "status",
      size: 28,
      header: () => <span className="sr-only">Status</span>,
      cell: ({ row }) => <StatusDot status={row.original.status} />,
    },
    {
      id: "start",
      accessorFn: (row) => row.start_time,
      size: 132,
      header: "Start",
      enableSorting: true,
      cell: ({ row }) => <TimeText value={row.original.start_time} style="datetime" />,
    },
    {
      id: "kind",
      size: 104,
      header: "Kind",
      cell: ({ row }) => (
        <span className="flex min-w-0 items-center gap-1">
          <KindIcon kind={row.original.span_kind} />
          <span className="truncate">{KIND_LABEL[row.original.span_kind]}</span>
        </span>
      ),
    },
    {
      id: "name",
      size: 200,
      header: "Name",
      cell: ({ row }) => row.original.name,
    },
    {
      id: "model",
      size: 140,
      header: "Model",
      cell: ({ row }) => row.original.model ?? "—",
    },
    {
      id: "input",
      size: 280,
      header: "Input",
      cell: ({ row }) => <Preview text={row.original.input_preview} />,
    },
    {
      id: "output",
      size: 280,
      header: "Output",
      cell: ({ row }) => <Preview text={row.original.output_preview} />,
    },
    {
      id: "duration",
      accessorFn: (row) => row.duration_ns,
      size: 80,
      header: "Duration",
      enableSorting: true,
      meta: { align: "right" },
      cell: ({ row }) => <span className="num">{formatDuration(row.original.duration_ns)}</span>,
    },
    {
      id: "ttft",
      accessorFn: (row) => row.ttft_ns,
      size: 72,
      header: "TTFT",
      meta: { align: "right" },
      cell: ({ row }) => <span className="num">{formatDurationOrDash(row.original.ttft_ns)}</span>,
    },
    {
      id: "tokens",
      accessorFn: (row) => row.total_tokens,
      size: 72,
      header: "Tokens",
      enableSorting: true,
      meta: { align: "right" },
      cell: ({ row }) => <TokenCount span={row.original} />,
    },
    {
      id: "cost",
      accessorFn: (row) => row.cost_total,
      size: 80,
      header: "Cost",
      enableSorting: true,
      meta: { align: "right" },
      cell: ({ row }) => <span className="num">{formatCost(row.original.cost_total)}</span>,
    },
    {
      id: "session",
      size: 120,
      header: "Session",
      meta: { mono: true },
      cell: ({ row }) => row.original.session_id ?? "—",
    },
    {
      id: "user",
      size: 100,
      header: "User",
      meta: { mono: true },
      cell: ({ row }) => row.original.user_id ?? "—",
    },
    {
      id: "env",
      size: 88,
      header: "Env",
      cell: ({ row }) => row.original.environment ?? "—",
    },
    {
      id: "provider",
      size: 100,
      header: "Provider",
      cell: ({ row }) => row.original.provider ?? "—",
    },
    {
      id: "service",
      size: 120,
      header: "Service",
      cell: ({ row }) => row.original.service_name ?? "—",
    },
    {
      id: "release",
      size: 88,
      header: "Release",
      cell: ({ row }) => row.original.release ?? "—",
    },
    {
      id: "tags",
      size: 140,
      header: "Tags",
      cell: ({ row }) => <Tags tags={row.original.tags} />,
    },
    {
      id: "trace_id",
      size: 220,
      header: "Trace ID",
      meta: { mono: true },
      cell: ({ row }) => row.original.trace_id,
    },
    {
      id: "span_id",
      size: 140,
      header: "Span ID",
      meta: { mono: true },
      cell: ({ row }) => row.original.span_id,
    },
    {
      id: "parent_span_id",
      size: 140,
      header: "Parent span ID",
      meta: { mono: true },
      cell: ({ row }) => row.original.parent_span_id ?? "—",
    },
  ];
}

function formatDurationOrDash(ns: number | undefined): string {
  if (ns === undefined) {
    return "—";
  }
  return formatDuration(ns);
}

function Preview({ text }: { text?: string }) {
  if (!text) {
    return <span className="text-muted-foreground">—</span>;
  }
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span className="block truncate text-muted-foreground">{text}</span>
      </TooltipTrigger>
      <TooltipContent>{text}</TooltipContent>
    </Tooltip>
  );
}

function TokenCount({ span }: { span: Schema<"SpanSummary"> }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span className="num">{formatTokens(span.total_tokens)}</span>
      </TooltipTrigger>
      <TooltipContent className="flex-col items-start">
        <span>Input {formatTokens(span.input_tokens)}</span>
        <span>Output {formatTokens(span.output_tokens)}</span>
        <span>Cache read {formatTokens(span.cache_read_tokens)}</span>
        <span>Cache write {formatTokens(span.cache_write_tokens)}</span>
        <span>Reasoning {formatTokens(span.reasoning_tokens)}</span>
      </TooltipContent>
    </Tooltip>
  );
}

function Tags({ tags }: { tags: string[] }) {
  if (tags.length === 0) {
    return "—";
  }
  const shown = tags.slice(0, TAGS_SHOWN);
  const rest = tags.length - shown.length;
  return (
    <span className="flex min-w-0 items-center gap-1">
      {shown.map((tag) => (
        <Badge key={tag} variant="secondary" className="h-4 px-1.5 text-[10px]">
          {tag}
        </Badge>
      ))}
      {rest > 0 ? <span className="text-muted-foreground">+{rest}</span> : null}
    </span>
  );
}
