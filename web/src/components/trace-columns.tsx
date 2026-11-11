import { Plus } from "lucide-react";
import type { ReactNode } from "react";
import type { Schema } from "@/api/types";
import type { ColumnSpec } from "@/components/data-table/column-layout";
import type { DataTableColumn } from "@/components/data-table/data-table";
import { stringContains, stringEquals } from "@/components/filter-bar/logic";
import { StatusDot } from "@/components/status-dot";
import { TimeText } from "@/components/time-text";
import { Badge } from "@/components/ui/badge";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { formatCost, formatDuration, formatTokens } from "@/lib/format";
import { TRACE_COLUMN_IDS, type TraceColumnId } from "@/lib/search/traces";

const TAGS_SHOWN = 2;

const HIDDEN = new Set<TraceColumnId>(["release", "spans", "errors", "llm_calls", "trace_id"]);
const LOCKED = new Set<TraceColumnId>(["status", "name"]);

const LABELS: Record<TraceColumnId, string> = {
  status: "Status",
  start: "Start",
  name: "Name",
  input: "Input",
  output: "Output",
  duration: "Duration",
  tokens: "Tokens",
  cost: "Cost",
  models: "Models",
  session: "Session",
  user: "User",
  tags: "Tags",
  env: "Env",
  release: "Release",
  spans: "Spans",
  errors: "Errors",
  llm_calls: "LLM calls",
  trace_id: "Trace ID",
};

export type TraceColumn = ColumnSpec & { label: string };

const SOURCE_LABEL: Record<Schema<"ScoreSource">, string> = {
  api: "API",
  sdk: "SDK",
  human: "Human",
  end_user: "End user",
  llm_judge: "LLM judge",
  check: "Check",
  webhook: "Webhook",
};

export function scoreNames(rows: readonly Schema<"TraceSummary">[]): string[] {
  const names = new Set<string>();
  for (const row of rows) {
    for (const score of row.scores) {
      names.add(score.name);
    }
  }
  return [...names].sort((left, right) => left.localeCompare(right));
}

export function formatScore(value: Schema<"ScoreValue">): string {
  switch (value.data_type) {
    case "numeric":
      return value.value.toFixed(2);
    case "boolean":
      return value.value ? "✓" : "✗";
    case "categorical":
      return value.value.label;
    case "text":
      return value.value;
  }
}

export function traceColumnSpecs(names: readonly string[]): TraceColumn[] {
  const fixed = TRACE_COLUMN_IDS.map((id) => ({
    id,
    label: LABELS[id],
    ...(LOCKED.has(id) ? { locked: true } : {}),
    ...(!HIDDEN.has(id) ? { defaultVisible: true } : {}),
  }));
  const scores: TraceColumn[] = names.map((name) => ({
    id: `score:${name}`,
    label: name,
    defaultVisible: true,
  }));
  const costAt = fixed.findIndex((column) => column.id === "cost");
  return [...fixed.slice(0, costAt + 1), ...scores, ...fixed.slice(costAt + 1)];
}

export function orderedTraceColumns(
  rows: readonly Schema<"TraceSummary">[],
  visible: readonly string[],
  onFilter?: (conjunct: string) => void,
): DataTableColumn<Schema<"TraceSummary">>[] {
  const byId = new Map(
    traceColumnDefs(scoreNames(rows), onFilter).map((column) => [column.id, column]),
  );
  return visible.flatMap((id) => {
    const column = byId.get(id);
    return column ? [column] : [];
  });
}

function traceColumnDefs(
  names: readonly string[],
  onFilter?: (conjunct: string) => void,
): DataTableColumn<Schema<"TraceSummary">>[] {
  const fixed: DataTableColumn<Schema<"TraceSummary">>[] = [
    {
      id: "status",
      size: 28,
      header: () => <span className="sr-only">Status</span>,
      cell: ({ row }) => (
        <FilterHit
          label={`Filter by status ${row.original.status}`}
          conjunct={stringEquals("status", row.original.status)}
          onFilter={onFilter}
        >
          <StatusDot status={row.original.status} />
        </FilterHit>
      ),
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
      id: "name",
      size: 200,
      header: "Name",
      cell: ({ row }) => (
        <FilterHit
          label={`Filter by name ${row.original.name}`}
          conjunct={stringEquals("name", row.original.name)}
          onFilter={onFilter}
        >
          {row.original.name}
        </FilterHit>
      ),
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
      id: "tokens",
      accessorFn: (row) => row.total_tokens,
      size: 72,
      header: "Tokens",
      enableSorting: true,
      meta: { align: "right" },
      cell: ({ row }) => <TokenCount trace={row.original} />,
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
      id: "models",
      size: 140,
      header: "Models",
      cell: ({ row }) => {
        const first = row.original.models[0];
        const label = modelsLabel(row.original.models);
        if (!first) {
          return label;
        }
        return (
          <FilterHit
            label={`Filter by models ${first}`}
            conjunct={stringContains("models", first)}
            onFilter={onFilter}
          >
            {label}
          </FilterHit>
        );
      },
    },
    {
      id: "session",
      size: 120,
      header: "Session",
      meta: { mono: true },
      cell: ({ row }) => filterText(row.original.session_id, "session", "session_id", onFilter),
    },
    {
      id: "user",
      size: 100,
      header: "User",
      meta: { mono: true },
      cell: ({ row }) => filterText(row.original.user_id, "user", "user_id", onFilter),
    },
    {
      id: "tags",
      size: 140,
      header: "Tags",
      cell: ({ row }) => <Tags tags={row.original.tags} onFilter={onFilter} />,
    },
    {
      id: "env",
      size: 88,
      header: "Env",
      cell: ({ row }) => filterText(row.original.environment, "env", "environment", onFilter),
    },
    {
      id: "release",
      size: 88,
      header: "Release",
      cell: ({ row }) => row.original.release ?? "—",
    },
    {
      id: "spans",
      accessorFn: (row) => row.span_count,
      size: 64,
      header: "Spans",
      meta: { align: "right" },
      cell: ({ row }) => <span className="num">{row.original.span_count}</span>,
    },
    {
      id: "errors",
      accessorFn: (row) => row.error_count,
      size: 64,
      header: "Errors",
      meta: { align: "right" },
      cell: ({ row }) => <span className="num">{row.original.error_count}</span>,
    },
    {
      id: "llm_calls",
      accessorFn: (row) => row.llm_count,
      size: 72,
      header: "LLM calls",
      meta: { align: "right" },
      cell: ({ row }) => <span className="num">{row.original.llm_count}</span>,
    },
    {
      id: "trace_id",
      size: 220,
      header: "Trace ID",
      meta: { mono: true },
      cell: ({ row }) => row.original.trace_id,
    },
  ];
  const scores = names.map((name) => scoreColumn(name));
  const costAt = fixed.findIndex((column) => column.id === "cost");
  return [...fixed.slice(0, costAt + 1), ...scores, ...fixed.slice(costAt + 1)];
}

function scoreColumn(name: string): DataTableColumn<Schema<"TraceSummary">> {
  return {
    id: `score:${name}`,
    size: 96,
    header: name,
    cell: ({ row }) => <ScoreCell trace={row.original} name={name} />,
  };
}

function ScoreCell({ trace, name }: { trace: Schema<"TraceSummary">; name: string }) {
  const score = trace.scores.find((item) => item.name === name);
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

function TokenCount({ trace }: { trace: Schema<"TraceSummary"> }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span className="num">{formatTokens(trace.total_tokens)}</span>
      </TooltipTrigger>
      <TooltipContent className="flex-col items-start">
        <span>Input {formatTokens(trace.input_tokens)}</span>
        <span>Output {formatTokens(trace.output_tokens)}</span>
        <span>Cache read {formatTokens(trace.cache_read_tokens)}</span>
        <span>Cache write {formatTokens(trace.cache_write_tokens)}</span>
        <span>Reasoning {formatTokens(trace.reasoning_tokens)}</span>
      </TooltipContent>
    </Tooltip>
  );
}

function modelsLabel(models: string[]): string {
  const first = models[0];
  if (!first) {
    return "—";
  }
  if (models.length === 1) {
    return first;
  }
  return `${first} +${models.length - 1}`;
}

function Tags({ tags, onFilter }: { tags: string[]; onFilter?: (conjunct: string) => void }) {
  if (tags.length === 0) {
    return "—";
  }
  const shown = tags.slice(0, TAGS_SHOWN);
  const rest = tags.length - shown.length;
  return (
    <span className="flex min-w-0 items-center gap-1">
      {shown.map((tag) => (
        <span key={tag} className="group/tag inline-flex min-w-0 items-center">
          <Badge variant="secondary" className="h-4 px-1.5 text-[10px]">
            {tag}
          </Badge>
          {onFilter ? (
            <button
              type="button"
              aria-label={`Filter by tag ${tag}`}
              className="inline-flex size-4 shrink-0 items-center justify-center text-muted-foreground opacity-0 group-hover/tag:opacity-100 focus-visible:opacity-100"
              onClick={(event) => {
                event.stopPropagation();
                onFilter(stringContains("tags", tag));
              }}
            >
              <Plus className="size-3" />
            </button>
          ) : null}
        </span>
      ))}
      {rest > 0 ? <span className="text-muted-foreground">+{rest}</span> : null}
    </span>
  );
}

function filterText(
  value: string | undefined,
  label: string,
  column: string,
  onFilter?: (conjunct: string) => void,
): ReactNode {
  if (!value) {
    return "—";
  }
  return (
    <FilterHit
      label={`Filter by ${label} ${value}`}
      conjunct={stringEquals(column, value)}
      onFilter={onFilter}
    >
      {value}
    </FilterHit>
  );
}

function FilterHit({
  label,
  conjunct,
  onFilter,
  children,
}: {
  label: string;
  conjunct: string;
  onFilter?: (conjunct: string) => void;
  children: ReactNode;
}) {
  return (
    <span className="group flex min-w-0 items-center gap-1">
      <span className="min-w-0 truncate">{children}</span>
      {onFilter ? (
        <button
          type="button"
          aria-label={label}
          className="inline-flex size-4 shrink-0 items-center justify-center text-muted-foreground opacity-0 group-hover:opacity-100 focus-visible:opacity-100"
          onClick={(event) => {
            event.stopPropagation();
            onFilter(conjunct);
          }}
        >
          <Plus className="size-3" />
        </button>
      ) : null}
    </span>
  );
}
