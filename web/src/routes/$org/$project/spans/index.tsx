import { useInfiniteQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { Rows3 } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { problemOf } from "@/api/problem";
import { spanListOptions } from "@/api/queries";
import type { Schema } from "@/api/types";
import { ColumnChooser } from "@/components/data-table/column-chooser";
import {
  readColumnChoice,
  sameColumnIds,
  visibleColumnIds,
  writeColumnChoice,
} from "@/components/data-table/column-layout";
import { DataTable } from "@/components/data-table/data-table";
import { EmptyState } from "@/components/empty-state";
import { FilterBar } from "@/components/filter-bar/filter-bar";
import { appendFilter, filterMessage } from "@/components/filter-bar/logic";
import { ListToolbar } from "@/components/list-toolbar";
import { useDefaultSavedView } from "@/components/saved-views/default-view";
import { SavedViewsMenu } from "@/components/saved-views/saved-views-menu";
import { useProject } from "@/components/shell/project-context";
import { orderedSpanColumns, spanColumnSpecs } from "@/components/span-columns";
import { SplitPane } from "@/components/trace/split-pane";
import { TraceView } from "@/components/trace/trace-view";
import { TooltipProvider } from "@/components/ui/tooltip";
import { parseSpansSearch, type SpansSearch } from "@/lib/search/spans";
import { type RangeState, rangeFromSearch, resolveRange } from "@/lib/time-range";

const TABLE_ID = "spans";

const SORT_COLUMN: Record<Schema<"SpanSort">, string> = {
  start_time: "start",
  duration: "duration",
  cost_total: "cost",
  total_tokens: "tokens",
};

const COLUMN_SORT: Record<string, Schema<"SpanSort">> = {
  start: "start_time",
  duration: "duration",
  cost: "cost_total",
  tokens: "total_tokens",
};

export const Route = createFileRoute("/$org/$project/spans/")({
  validateSearch: (search) => parseSpansSearch(search),
  component: SpansPage,
});

function SpansPage() {
  const { org, project } = useProject();
  const search = Route.useSearch();
  const navigate = Route.useNavigate();
  const applyView = useCallback(
    (next: Record<string, unknown>) => {
      void navigate({
        to: "/$org/$project/spans",
        params: { org, project },
        search: () => parseSpansSearch(next),
      });
    },
    [navigate, org, project],
  );
  const applyDefault = useCallback(
    (next: Record<string, unknown>) => {
      void navigate({
        to: "/$org/$project/spans",
        params: { org, project },
        search: () => parseSpansSearch(next),
        replace: true,
      });
    },
    [navigate, org, project],
  );
  const { hold } = useDefaultSavedView("spans", search, applyDefault);
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
  const list = useInfiniteQuery({
    ...spanListOptions(org, project, {
      from: resolved.from,
      to: resolved.to,
      sort: search.sort,
      order: search.order,
      search: search.search,
      filter: search.filter,
      includeEval: search.eval === 1,
      includeExperiments: search.exp === 1,
    }),
    enabled: !hold,
  });
  const fetchNextPage = useCallback(() => {
    void list.fetchNextPage();
  }, [list.fetchNextPage]);
  const rows = useMemo(() => list.data?.pages.flatMap((page) => page.data) ?? [], [list.data]);
  const specs = useMemo(() => spanColumnSpecs(), []);
  const defaults = useMemo(() => visibleColumnIds(specs, undefined), [specs]);
  const visible = useMemo(() => {
    const fromUrl = search.cols?.split(",");
    return visibleColumnIds(specs, fromUrl ?? readColumnChoice(TABLE_ID));
  }, [search.cols, specs]);
  const onFilter = useCallback(
    (conjunct: string) => {
      void navigate({
        to: "/$org/$project/spans",
        params: { org, project },
        search: (prev) => withFilter(prev, appendFilter(prev.filter ?? "", conjunct)),
      });
    },
    [navigate, org, project],
  );
  const tableColumns = useMemo(() => orderedSpanColumns(visible), [visible]);
  function onColumns(ids: string[]) {
    const choice = sameColumnIds(ids, defaults) ? undefined : ids;
    writeColumnChoice(TABLE_ID, choice);
    void navigate({
      to: "/$org/$project/spans",
      params: { org, project },
      search: (prev) => withCols(prev, choice?.join(",")),
    });
  }
  const traceOpen = search.trace !== undefined;
  const listScroll = useRef(0);
  const wasOpen = useRef(traceOpen);

  useLayoutEffect(() => {
    const node = document.querySelector<HTMLElement>(
      '[aria-label="Spans"] [data-scroll-container]',
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
        to: "/$org/$project/spans",
        params: { org, project },
        search: (prev) => withoutTrace(prev),
      });
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [navigate, org, project, search.trace]);

  const problem = list.isError ? problemOf(list.error) : undefined;
  const filterError = problem?.code === "invalid_filter" ? problem : undefined;
  if (problem && !filterError) {
    throw list.error;
  }

  const selectedRowId = search.trace && search.span ? `${search.trace}:${search.span}` : undefined;

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
                  title="Spans"
                  range={range}
                  onRange={(next) => {
                    seenRange.current =
                      next.kind === "preset" ? next.preset : `${next.from}\n${next.to}`;
                    setAnchor(Date.now());
                    void navigate({
                      to: "/$org/$project/spans",
                      params: { org, project },
                      search: (prev) => withRange(prev, next),
                    });
                  }}
                  onRefresh={() => setAnchor(Date.now())}
                  search={search.search ?? ""}
                  onSearch={(value) => {
                    void navigate({
                      to: "/$org/$project/spans",
                      params: { org, project },
                      search: (prev) => withSearch(prev, value),
                    });
                  }}
                  evalOn={search.eval === 1}
                  onEval={(on) => {
                    void navigate({
                      to: "/$org/$project/spans",
                      params: { org, project },
                      search: (prev) => withFlag(prev, "eval", on),
                    });
                  }}
                  expOn={search.exp === 1}
                  onExp={(on) => {
                    void navigate({
                      to: "/$org/$project/spans",
                      params: { org, project },
                      search: (prev) => withFlag(prev, "exp", on),
                    });
                  }}
                  filter={
                    <FilterBar
                      view="spans"
                      value={search.filter ?? ""}
                      error={filterError}
                      onApply={(text) => {
                        void navigate({
                          to: "/$org/$project/spans",
                          params: { org, project },
                          search: (prev) => withFilter(prev, text),
                        });
                      }}
                    />
                  }
                  columns={
                    <ColumnChooser columns={specs} visible={visible} onVisible={onColumns} />
                  }
                  views={<SavedViewsMenu kind="spans" search={search} onApply={applyView} />}
                  loaded={list.data ? rows.length : undefined}
                />
                {filterError ? (
                  <div className="mx-auto flex max-w-md flex-col gap-2 px-4 py-16">
                    <h2 className="text-sm font-medium">{filterError.title}</h2>
                    <p className="text-sm text-destructive">{filterMessage(filterError)}</p>
                  </div>
                ) : !hold && list.data ? (
                  <DataTable
                    tableId="spans"
                    ariaLabel="Spans"
                    columns={tableColumns}
                    rows={rows}
                    getRowId={(row) => `${row.trace_id}:${row.span_id}`}
                    selectedRowId={selectedRowId}
                    onActivate={(row) => {
                      void navigate({
                        to: "/$org/$project/spans",
                        params: { org, project },
                        search: (prev) => ({
                          ...prev,
                          trace: row.trace_id,
                          span: row.span_id,
                        }),
                      });
                    }}
                    onSelectionMove={(row) => {
                      void navigate({
                        to: "/$org/$project/spans",
                        params: { org, project },
                        search: (prev) => ({
                          ...prev,
                          trace: row.trace_id,
                          span: row.span_id,
                        }),
                        replace: true,
                      });
                    }}
                    hasNextPage={list.hasNextPage}
                    isFetchingNextPage={list.isFetchingNextPage}
                    fetchNextPage={fetchNextPage}
                    sorting={{
                      id: SORT_COLUMN[search.sort ?? "start_time"],
                      desc: (search.order ?? "desc") === "desc",
                    }}
                    onSortingChange={(next) => {
                      void navigate({
                        to: "/$org/$project/spans",
                        params: { org, project },
                        search: (prev) => withSort(prev, next),
                      });
                    }}
                    emptyState={<EmptyState icon={Rows3} title="No spans" />}
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
                onAddFilter={onFilter}
                onSelectSpan={(spanId) => {
                  void navigate({
                    to: "/$org/$project/spans",
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

function withRange(search: SpansSearch, state: RangeState): SpansSearch {
  const { range: _range, from: _from, to: _to, ...rest } = search;
  if (state.kind === "preset") {
    return { ...rest, range: state.preset };
  }
  return { ...rest, from: state.from, to: state.to };
}

function withSearch(search: SpansSearch, value: string): SpansSearch {
  const { search: _search, ...rest } = search;
  const trimmed = value.trim();
  return trimmed.length > 0 ? { ...rest, search: trimmed } : rest;
}

function withFilter(search: SpansSearch, value: string): SpansSearch {
  const { filter: _filter, ...rest } = search;
  const trimmed = value.trim();
  return trimmed.length > 0 ? { ...rest, filter: trimmed } : rest;
}

function withFlag(search: SpansSearch, key: "eval" | "exp", on: boolean): SpansSearch {
  const { [key]: _flag, ...rest } = search;
  return on ? { ...rest, [key]: 1 } : rest;
}

function withCols(search: SpansSearch, cols: string | undefined): SpansSearch {
  const { cols: _cols, ...rest } = search;
  return cols ? { ...rest, cols } : rest;
}

function withSort(search: SpansSearch, next: { id: string; desc: boolean } | null): SpansSearch {
  const { sort: _sort, order: _order, ...rest } = search;
  if (!next) {
    return rest;
  }
  const sort = COLUMN_SORT[next.id];
  if (!sort) {
    return rest;
  }
  return { ...rest, sort, order: next.desc ? "desc" : "asc" };
}

function withoutTrace(search: SpansSearch): SpansSearch {
  const { trace: _trace, span: _span, ...rest } = search;
  return rest;
}
