import { useInfiniteQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { Waypoints } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { Schema } from "@/api/types";
import { DataTable, type DataTableColumn } from "@/components/data-table/data-table";
import { traceListOptions } from "@/components/data-table/use-infinite-list";
import { EmptyState } from "@/components/empty-state";
import { ListToolbar } from "@/components/list-toolbar";
import { useProject } from "@/components/shell/project-context";
import { StatusDot } from "@/components/status-dot";
import { TimeText } from "@/components/time-text";
import { SplitPane } from "@/components/trace/split-pane";
import { TraceView } from "@/components/trace/trace-view";
import { Badge } from "@/components/ui/badge";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { formatCost, formatDuration, formatTokens } from "@/lib/format";
import { parseTracesSearch, type TracesSearch } from "@/lib/search/traces";
import { type RangeState, rangeFromSearch, resolveRange } from "@/lib/time-range";

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
  const range = rangeFromSearch(search);
  const rangeKey = range.kind === "preset" ? range.preset : `${range.from}\n${range.to}`;
  const [anchor, setAnchor] = useState(() => Date.now());
  const seenRange = useRef(rangeKey);
  useEffect(() => {
    if (seenRange.current === rangeKey) {
      return;
    }
    seenRange.current = rangeKey;
    setAnchor(Date.now());
  }, [rangeKey]);
  const resolved = resolveRange(range, anchor);
  const list = useInfiniteQuery(
    traceListOptions(org, project, {
      from: resolved.from,
      to: resolved.to,
      sort: search.sort,
      order: search.order,
      search: search.search,
      includeEval: search.eval === 1,
      includeExperiments: search.exp === 1,
    }),
  );
  const fetchNextPage = useCallback(() => {
    void list.fetchNextPage();
  }, [list.fetchNextPage]);
  const rows = useMemo(() => list.data?.pages.flatMap((page) => page.data) ?? [], [list.data]);
  const traceOpen = search.trace !== undefined;
  const listScroll = useRef(0);
  const wasOpen = useRef(traceOpen);

  useLayoutEffect(() => {
    const node = document.querySelector<HTMLElement>(
      '[aria-label="Traces"] [data-scroll-container]',
    );
    if (node && wasOpen.current !== traceOpen) {
      node.scrollTop = listScroll.current;
      node.dispatchEvent(new Event("scroll"));
      wasOpen.current = traceOpen;
    }
    return () => {
      if (node) {
        listScroll.current = node.scrollTop;
      }
    };
  }, [traceOpen]);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      if (event.key !== "Escape" || !search.trace) {
        return;
      }
      const target = event.target;
      if (
        target instanceof HTMLElement &&
        (target.closest("input, textarea") !== null || target.isContentEditable)
      ) {
        return;
      }
      void navigate({
        to: "/$org/$project/traces",
        params: { org, project },
        search: (prev) => withoutTrace(prev),
      });
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [navigate, org, project, search.trace]);

  if (list.isError) {
    throw list.error;
  }

  return (
    <div className="relative h-full min-h-0 flex-1">
      <div className="absolute inset-0">
        <SplitPane
          name="traces"
          open={traceOpen}
          start={
            <div className="flex h-full min-h-0 flex-col">
              <TooltipProvider>
                <ListToolbar
                  title="Traces"
                  range={range}
                  onRange={(next) => {
                    seenRange.current =
                      next.kind === "preset" ? next.preset : `${next.from}\n${next.to}`;
                    setAnchor(Date.now());
                    void navigate({
                      to: "/$org/$project/traces",
                      params: { org, project },
                      search: (prev) => withRange(prev, next),
                    });
                  }}
                  onRefresh={() => setAnchor(Date.now())}
                  search={search.search ?? ""}
                  onSearch={(value) => {
                    void navigate({
                      to: "/$org/$project/traces",
                      params: { org, project },
                      search: (prev) => withSearch(prev, value),
                    });
                  }}
                  evalOn={search.eval === 1}
                  onEval={(on) => {
                    void navigate({
                      to: "/$org/$project/traces",
                      params: { org, project },
                      search: (prev) => withFlag(prev, "eval", on),
                    });
                  }}
                  expOn={search.exp === 1}
                  onExp={(on) => {
                    void navigate({
                      to: "/$org/$project/traces",
                      params: { org, project },
                      search: (prev) => withFlag(prev, "exp", on),
                    });
                  }}
                  loaded={list.data ? rows.length : undefined}
                />
                {list.data ? (
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
                        search: (prev) => ({ ...withoutSpan(prev), trace: row.trace_id }),
                      });
                    }}
                    onSelectionMove={(row) => {
                      void navigate({
                        to: "/$org/$project/traces",
                        params: { org, project },
                        search: (prev) => ({ ...withoutSpan(prev), trace: row.trace_id }),
                        replace: true,
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
                ) : null}
              </TooltipProvider>
            </div>
          }
          end={
            search.trace ? (
              <TraceView
                traceId={search.trace}
                spanId={search.span}
                onSelectSpan={(spanId) => {
                  void navigate({
                    to: "/$org/$project/traces",
                    params: { org, project },
                    search: (prev) => ({ ...prev, span: spanId }),
                    replace: true,
                  });
                }}
              />
            ) : null
          }
        />
      </div>
    </div>
  );
}

function withRange(search: TracesSearch, state: RangeState): TracesSearch {
  const { range: _range, from: _from, to: _to, ...rest } = search;
  if (state.kind === "preset") {
    return { ...rest, range: state.preset };
  }
  return { ...rest, from: state.from, to: state.to };
}

function withSearch(search: TracesSearch, value: string): TracesSearch {
  const { search: _search, ...rest } = search;
  const trimmed = value.trim();
  return trimmed.length > 0 ? { ...rest, search: trimmed } : rest;
}

function withFlag(search: TracesSearch, key: "eval" | "exp", on: boolean): TracesSearch {
  const { [key]: _flag, ...rest } = search;
  return on ? { ...rest, [key]: 1 } : rest;
}

function withoutSpan(search: TracesSearch): TracesSearch {
  const { span: _span, ...rest } = search;
  return rest;
}

function withoutTrace(search: TracesSearch): TracesSearch {
  const { trace: _trace, span: _span, ...rest } = search;
  return rest;
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
