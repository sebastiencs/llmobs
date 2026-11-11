import { useInfiniteQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { Waypoints } from "lucide-react";
import { useCallback, useMemo, useState } from "react";
import type { Schema } from "@/api/types";
import { DataTable, type DataTableColumn } from "@/components/data-table/data-table";
import { traceListOptions } from "@/components/data-table/use-infinite-list";
import { EmptyState } from "@/components/empty-state";
import { useProject } from "@/components/shell/project-context";
import { StatusDot } from "@/components/status-dot";
import { TimeText } from "@/components/time-text";
import { Badge } from "@/components/ui/badge";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { formatCost, formatDuration, formatTokens } from "@/lib/format";
import { parseTracesSearch } from "@/lib/search/traces";

const DAY_MS = 24 * 60 * 60 * 1000;
const TAGS_SHOWN = 2;

const columns: DataTableColumn<Schema<"TraceSummary">>[] = [
  {
    id: "status",
    size: 28,
    header: () => <span className="sr-only">Status</span>,
    cell: ({ row }) => <StatusDot status={row.original.status} />,
  },
  {
    id: "start_time",
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
    cell: ({ row }) => row.original.name,
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
    id: "total_tokens",
    accessorFn: (row) => row.total_tokens,
    size: 72,
    header: "Tokens",
    enableSorting: true,
    meta: { align: "right" },
    cell: ({ row }) => <TokenCount trace={row.original} />,
  },
  {
    id: "cost_total",
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
    cell: ({ row }) => modelsLabel(row.original.models),
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
    id: "tags",
    size: 140,
    header: "Tags",
    cell: ({ row }) => <Tags tags={row.original.tags} />,
  },
  {
    id: "env",
    size: 88,
    header: "Env",
    cell: ({ row }) => row.original.environment ?? "—",
  },
];

export const Route = createFileRoute("/$org/$project/traces/")({
  validateSearch: (search) => parseTracesSearch(search),
  component: TracesPage,
});

function TracesPage() {
  const { org, project } = useProject();
  const search = Route.useSearch();
  const navigate = Route.useNavigate();
  const [from] = useState(() => new Date(Date.now() - DAY_MS).toISOString());
  const list = useInfiniteQuery(
    traceListOptions(org, project, { from, sort: search.sort, order: search.order }),
  );
  const fetchNextPage = useCallback(() => {
    void list.fetchNextPage();
  }, [list.fetchNextPage]);
  const rows = useMemo(() => list.data?.pages.flatMap((page) => page.data) ?? [], [list.data]);

  if (list.isError) {
    throw list.error;
  }

  return (
    <div className="flex h-full min-h-0 flex-1 flex-col">
      <div className="flex h-[36px] shrink-0 items-center justify-between border-b px-3">
        <h1 className="text-sm font-medium">Traces</h1>
        {list.data ? (
          <span className="text-xs text-muted-foreground">{rows.length} loaded</span>
        ) : null}
      </div>
      {list.data ? (
        <TooltipProvider>
          <DataTable
            tableId="traces"
            ariaLabel="Traces"
            columns={columns}
            rows={rows}
            getRowId={(row) => row.trace_id}
            selectedRowId={search.trace}
            onActivate={(row) => {
              void navigate({
                to: "/$org/$project/traces",
                params: { org, project },
                search: (prev) => ({ ...prev, trace: row.trace_id }),
              });
            }}
            hasNextPage={list.hasNextPage}
            isFetchingNextPage={list.isFetchingNextPage}
            fetchNextPage={fetchNextPage}
            sorting={{
              id: search.sort ?? "start_time",
              desc: (search.order ?? "desc") === "desc",
            }}
            emptyState={<EmptyState icon={Waypoints} title="No traces" />}
          />
        </TooltipProvider>
      ) : null}
    </div>
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
