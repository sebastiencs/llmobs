import { useInfiniteQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { Waypoints } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { problemOf } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import type { Schema } from "@/api/types";
import { useShortcut } from "@/components/command/use-shortcut";
import { ColumnChooser } from "@/components/data-table/column-chooser";
import {
  readColumnChoice,
  sameColumnIds,
  visibleColumnIds,
  writeColumnChoice,
} from "@/components/data-table/column-layout";
import { DataTable } from "@/components/data-table/data-table";
import { traceListOptions } from "@/components/data-table/use-infinite-list";
import { useLiveTraces } from "@/components/data-table/use-live-traces";
import { FilterBar } from "@/components/filter-bar/filter-bar";
import { appendFilter, filterMessage } from "@/components/filter-bar/logic";
import { ListToolbar } from "@/components/list-toolbar";
import { LiveToggle } from "@/components/live-toggle";
import { OnboardingPanel } from "@/components/onboarding/panel";
import { onboardingPollMs } from "@/components/onboarding/poll";
import { tracesNeedOnboarding } from "@/components/onboarding/state";
import { useDefaultSavedView } from "@/components/saved-views/default-view";
import { SavedViewsMenu } from "@/components/saved-views/saved-views-menu";
import { useProject } from "@/components/shell/project-context";
import { SplitPane } from "@/components/trace/split-pane";
import { TraceView } from "@/components/trace/trace-view";
import { orderedTraceColumns, scoreNames, traceColumnSpecs } from "@/components/trace-columns";
import { Button } from "@/components/ui/button";
import { TooltipProvider } from "@/components/ui/tooltip";
import {
  parseTracesSearch,
  type TraceMode,
  type TracesSearch,
  traceLiveReason,
} from "@/lib/search/traces";
import { traceStreamUrl } from "@/lib/sse";
import { type RangeState, rangeFromSearch, resolveRange } from "@/lib/time-range";

const TABLE_ID = "traces";

const SORT_COLUMN: Record<Schema<"TraceSort">, string> = {
  start_time: "start",
  duration: "duration",
  cost_total: "cost",
  total_tokens: "tokens",
};

const COLUMN_SORT: Record<string, Schema<"TraceSort">> = {
  start: "start_time",
  duration: "duration",
  cost: "cost_total",
  tokens: "total_tokens",
};

export const Route = createFileRoute("/$org/$project/traces/")({
  validateSearch: (search) => parseTracesSearch(search),
  component: TracesPage,
});

function TracesPage() {
  const { org, project } = useProject();
  const search = Route.useSearch();
  const navigate = Route.useNavigate();
  const applyView = useCallback(
    (next: Record<string, unknown>) => {
      void navigate({
        to: "/$org/$project/traces",
        params: { org, project },
        search: () => parseTracesSearch(next),
      });
    },
    [navigate, org, project],
  );
  const applyDefault = useCallback(
    (next: Record<string, unknown>) => {
      void navigate({
        to: "/$org/$project/traces",
        params: { org, project },
        search: () => parseTracesSearch(next),
        replace: true,
      });
    },
    [navigate, org, project],
  );
  const { hold } = useDefaultSavedView("traces", search, applyDefault);
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
  const listOptions = traceListOptions(org, project, {
    from: resolved.from,
    to: resolved.to,
    sort: search.sort,
    order: search.order,
    search: search.search,
    filter: search.filter,
    includeEval: search.eval === 1,
    includeExperiments: search.exp === 1,
  });
  const list = useInfiniteQuery({
    ...listOptions,
    enabled: !hold,
    refetchInterval: (query) => {
      if (hold || !tracesNeedOnboarding(search, range)) {
        return false;
      }
      const data = query.state.data;
      if (!data || data.pages.some((page) => page.data.length > 0)) {
        return false;
      }
      return onboardingPollMs();
    },
  });
  const fetchNextPage = useCallback(() => {
    void list.fetchNextPage();
  }, [list.fetchNextPage]);
  const rows = useMemo(() => list.data?.pages.flatMap((page) => page.data) ?? [], [list.data]);
  const specs = useMemo(() => traceColumnSpecs(scoreNames(rows)), [rows]);
  const defaults = useMemo(() => visibleColumnIds(specs, undefined), [specs]);
  const visible = useMemo(() => {
    const fromUrl = search.cols?.split(",");
    return visibleColumnIds(specs, fromUrl ?? readColumnChoice(TABLE_ID));
  }, [search.cols, specs]);
  const onFilter = useCallback(
    (conjunct: string) => {
      void navigate({
        to: "/$org/$project/traces",
        params: { org, project },
        search: (prev) => withFilter(prev, appendFilter(prev.filter ?? "", conjunct)),
      });
    },
    [navigate, org, project],
  );
  const tableColumns = useMemo(
    () => orderedTraceColumns(rows, visible, onFilter),
    [rows, visible, onFilter],
  );
  function onColumns(ids: string[]) {
    const choice = sameColumnIds(ids, defaults) ? undefined : ids;
    writeColumnChoice(TABLE_ID, choice);
    void navigate({
      to: "/$org/$project/traces",
      params: { org, project },
      search: (prev) => withCols(prev, choice?.join(",")),
    });
  }
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

  useShortcut("dismiss", () => {
    if (!search.trace) {
      return;
    }
    void navigate({
      to: "/$org/$project/traces",
      params: { org, project },
      search: (prev) => withoutTrace(prev),
    });
  });

  const problem = list.isError ? problemOf(list.error) : undefined;
  const filterError = problem?.code === "invalid_filter" ? problem : undefined;
  const onboarding =
    !hold && list.data !== undefined && rows.length === 0 && tracesNeedOnboarding(search, range);
  const reason = traceLiveReason(search);
  const liveOn = search.live === 1 && reason === undefined && !hold;
  const live = useLiveTraces({
    url: liveOn
      ? traceStreamUrl(projectRef(org, project), {
          filter: search.filter,
          includeEval: search.eval === 1,
          includeExperiments: search.exp === 1,
        })
      : undefined,
    queryKey: listOptions.queryKey,
    selectedId: search.trace,
    ready: !hold && list.data !== undefined && !filterError && !onboarding,
  });
  useEffect(() => {
    if (search.live !== 1 || reason === undefined) {
      return;
    }
    void navigate({
      to: "/$org/$project/traces",
      params: { org, project },
      search: (prev) => withoutLive(prev),
      replace: true,
    });
  }, [search.live, reason, navigate, org, project]);
  if (problem && !filterError) {
    throw list.error;
  }
  function clearConstraints() {
    void navigate({
      to: "/$org/$project/traces",
      params: { org, project },
      search: (prev) => withoutConstraints(prev),
    });
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
                      search: (prev) => {
                        const ranged = withRange(prev, next);
                        return traceLiveReason(ranged) ? withoutLive(ranged) : ranged;
                      },
                    });
                  }}
                  onRefresh={() => setAnchor(Date.now())}
                  live={
                    <LiveToggle
                      pressed={liveOn}
                      disabled={reason !== undefined}
                      reason={reason}
                      status={live.status}
                      onPressedChange={(on) => {
                        if (on) {
                          setAnchor(Date.now());
                        }
                        void navigate({
                          to: "/$org/$project/traces",
                          params: { org, project },
                          search: (prev) => withLive(prev, on),
                        });
                      }}
                    />
                  }
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
                  filter={
                    <FilterBar
                      view="traces"
                      value={search.filter ?? ""}
                      error={filterError}
                      onApply={(text) => {
                        void navigate({
                          to: "/$org/$project/traces",
                          params: { org, project },
                          search: (prev) => withFilter(prev, text),
                        });
                      }}
                    />
                  }
                  columns={
                    <ColumnChooser columns={specs} visible={visible} onVisible={onColumns} />
                  }
                  views={<SavedViewsMenu kind="traces" search={search} onApply={applyView} />}
                  loaded={list.data ? rows.length : undefined}
                />
                {filterError ? (
                  <div className="mx-auto flex max-w-md flex-col gap-2 px-4 py-16">
                    <h2 className="text-sm font-medium">{filterError.title}</h2>
                    <p className="text-sm text-destructive">{filterMessage(filterError)}</p>
                  </div>
                ) : onboarding ? (
                  <div className="min-h-0 flex-1 overflow-auto">
                    <OnboardingPanel origin={window.location.origin} />
                  </div>
                ) : !hold && list.data ? (
                  <DataTable
                    tableId="traces"
                    ariaLabel="Traces"
                    columns={tableColumns}
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
                      id: SORT_COLUMN[search.sort ?? "start_time"],
                      desc: (search.order ?? "desc") === "desc",
                    }}
                    onSortingChange={(next) => {
                      void navigate({
                        to: "/$org/$project/traces",
                        params: { org, project },
                        search: (prev) => {
                          const sorted = withSort(prev, next);
                          return traceLiveReason(sorted) ? withoutLive(sorted) : sorted;
                        },
                      });
                    }}
                    highlightedRowIds={live.highlighted}
                    notice={
                      live.pending > 0 ? (
                        <Button type="button" size="xs" variant="secondary" onClick={live.reveal}>
                          {live.overflowed ? `${live.pending}+` : live.pending} new traces ↑
                        </Button>
                      ) : null
                    }
                    emptyState={
                      <div className="flex flex-col items-center gap-3 px-4 py-16 text-center">
                        <Waypoints className="size-8 text-muted-foreground" />
                        <h2 className="text-lg font-medium">No traces match</h2>
                        <Button
                          type="button"
                          variant="outline"
                          size="sm"
                          onClick={clearConstraints}
                        >
                          Clear filters
                        </Button>
                      </div>
                    }
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
                mode={search.mode}
                onModeChange={(next) => {
                  void navigate({
                    to: "/$org/$project/traces",
                    params: { org, project },
                    search: (prev) => withMode(prev, next),
                    replace: true,
                  });
                }}
                onAddFilter={onFilter}
                onClose={() => {
                  void navigate({
                    to: "/$org/$project/traces",
                    params: { org, project },
                    search: (prev) => withoutTrace(prev),
                  });
                }}
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

function withFilter(search: TracesSearch, value: string): TracesSearch {
  const { filter: _filter, ...rest } = search;
  const trimmed = value.trim();
  return trimmed.length > 0 ? { ...rest, filter: trimmed } : rest;
}

function withLive(search: TracesSearch, on: boolean): TracesSearch {
  const rest = withoutLive(search);
  return on ? { ...rest, live: 1 } : rest;
}

function withoutLive(search: TracesSearch): TracesSearch {
  const { live: _live, ...rest } = search;
  return rest;
}

function withFlag(search: TracesSearch, key: "eval" | "exp", on: boolean): TracesSearch {
  const { [key]: _flag, ...rest } = search;
  return on ? { ...rest, [key]: 1 } : rest;
}

function withCols(search: TracesSearch, cols: string | undefined): TracesSearch {
  const { cols: _cols, ...rest } = search;
  return cols ? { ...rest, cols } : rest;
}

function withSort(search: TracesSearch, next: { id: string; desc: boolean } | null): TracesSearch {
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

function withoutSpan(search: TracesSearch): TracesSearch {
  const { span: _span, ...rest } = search;
  return rest;
}

function withoutConstraints(search: TracesSearch): TracesSearch {
  const {
    filter: _filter,
    search: _query,
    eval: _eval,
    exp: _exp,
    from: _from,
    to: _to,
    view: _view,
    ...rest
  } = search;
  // An empty search would reapply the default saved view and put the filter back.
  return Object.keys(rest).length === 0 ? { range: "24h" } : rest;
}

function withoutTrace(search: TracesSearch): TracesSearch {
  const { trace: _trace, span: _span, ...rest } = search;
  return rest;
}

function withMode(search: TracesSearch, mode: TraceMode): TracesSearch {
  const { mode: _mode, ...rest } = search;
  return mode === "tree" ? rest : { ...rest, mode };
}
