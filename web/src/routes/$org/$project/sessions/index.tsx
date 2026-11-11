import { useInfiniteQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { MessagesSquare } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { problemOf } from "@/api/problem";
import { sessionListOptions } from "@/api/queries";
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
import { filterMessage } from "@/components/filter-bar/logic";
import { ListToolbar } from "@/components/list-toolbar";
import { useDefaultSavedView } from "@/components/saved-views/default-view";
import { SavedViewsMenu } from "@/components/saved-views/saved-views-menu";
import {
  orderedSessionColumns,
  sessionColumnSpecs,
  sessionScoreNames,
} from "@/components/session-columns";
import { useProject } from "@/components/shell/project-context";
import { TooltipProvider } from "@/components/ui/tooltip";
import { parseSessionsSearch, type SessionsSearch } from "@/lib/search/sessions";
import { type RangeState, rangeFromSearch, resolveRange } from "@/lib/time-range";

const TABLE_ID = "sessions";

const SORT_COLUMN: Record<Schema<"SessionSort">, string> = {
  start_time: "start",
  end_time: "last",
  trace_count: "traces",
  cost_total: "cost",
};

const COLUMN_SORT: Record<string, Schema<"SessionSort">> = {
  start: "start_time",
  last: "end_time",
  traces: "trace_count",
  cost: "cost_total",
};

export const Route = createFileRoute("/$org/$project/sessions/")({
  validateSearch: (search) => parseSessionsSearch(search),
  component: SessionsPage,
});

function SessionsPage() {
  const { org, project } = useProject();
  const search = Route.useSearch();
  const navigate = Route.useNavigate();
  const applyView = useCallback(
    (next: Record<string, unknown>) => {
      void navigate({
        to: "/$org/$project/sessions",
        params: { org, project },
        search: () => parseSessionsSearch(next),
      });
    },
    [navigate, org, project],
  );
  const applyDefault = useCallback(
    (next: Record<string, unknown>) => {
      void navigate({
        to: "/$org/$project/sessions",
        params: { org, project },
        search: () => parseSessionsSearch(next),
        replace: true,
      });
    },
    [navigate, org, project],
  );
  const { hold } = useDefaultSavedView("sessions", search, applyDefault);
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
    ...sessionListOptions(org, project, {
      from: resolved.from,
      to: resolved.to,
      sort: search.sort,
      order: search.order,
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
  const specs = useMemo(() => sessionColumnSpecs(sessionScoreNames(rows)), [rows]);
  const defaults = useMemo(() => visibleColumnIds(specs, undefined), [specs]);
  const visible = useMemo(() => {
    const fromUrl = search.cols?.split(",");
    return visibleColumnIds(specs, fromUrl ?? readColumnChoice(TABLE_ID));
  }, [search.cols, specs]);
  const tableColumns = useMemo(() => orderedSessionColumns(rows, visible), [rows, visible]);

  function onColumns(ids: string[]) {
    const choice = sameColumnIds(ids, defaults) ? undefined : ids;
    writeColumnChoice(TABLE_ID, choice);
    void navigate({
      to: "/$org/$project/sessions",
      params: { org, project },
      search: (prev) => withCols(prev, choice?.join(",")),
    });
  }

  const problem = list.isError ? problemOf(list.error) : undefined;
  const filterError = problem?.code === "invalid_filter" ? problem : undefined;
  if (problem && !filterError) {
    throw list.error;
  }

  return (
    <div className="flex h-full min-h-0 flex-1 flex-col">
      <TooltipProvider>
        <ListToolbar
          title="Sessions"
          idPrefix="sessions"
          range={range}
          onRange={(next) => {
            seenRange.current = next.kind === "preset" ? next.preset : `${next.from}\n${next.to}`;
            setAnchor(Date.now());
            void navigate({
              to: "/$org/$project/sessions",
              params: { org, project },
              search: (prev) => withRange(prev, next),
            });
          }}
          onRefresh={() => setAnchor(Date.now())}
          evalOn={search.eval === 1}
          onEval={(on) => {
            void navigate({
              to: "/$org/$project/sessions",
              params: { org, project },
              search: (prev) => withFlag(prev, "eval", on),
            });
          }}
          expOn={search.exp === 1}
          onExp={(on) => {
            void navigate({
              to: "/$org/$project/sessions",
              params: { org, project },
              search: (prev) => withFlag(prev, "exp", on),
            });
          }}
          filter={
            <FilterBar
              view="sessions"
              value={search.filter ?? ""}
              error={filterError}
              onApply={(text) => {
                void navigate({
                  to: "/$org/$project/sessions",
                  params: { org, project },
                  search: (prev) => withFilter(prev, text),
                });
              }}
            />
          }
          columns={<ColumnChooser columns={specs} visible={visible} onVisible={onColumns} />}
          views={<SavedViewsMenu kind="sessions" search={search} onApply={applyView} />}
          loaded={list.data ? rows.length : undefined}
        />
        {filterError ? (
          <div className="mx-auto flex max-w-md flex-col gap-2 px-4 py-16">
            <h2 className="text-sm font-medium">{filterError.title}</h2>
            <p className="text-sm text-destructive">{filterMessage(filterError)}</p>
          </div>
        ) : !hold && list.data ? (
          <DataTable
            tableId="sessions"
            ariaLabel="Sessions"
            columns={tableColumns}
            rows={rows}
            getRowId={(row) => row.session_id}
            onActivate={(row) => {
              void navigate({
                to: "/$org/$project/sessions/$sessionId",
                params: { org, project, sessionId: row.session_id },
                search: pageFlags(search),
                state: (current) => ({ ...current, sessionsSearch: search }),
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
                to: "/$org/$project/sessions",
                params: { org, project },
                search: (prev) => withSort(prev, next),
              });
            }}
            emptyState={<EmptyState icon={MessagesSquare} title="No sessions" />}
          />
        ) : null}
      </TooltipProvider>
    </div>
  );
}

function pageFlags(search: SessionsSearch): { eval?: 1; exp?: 1 } {
  return {
    ...(search.eval ? { eval: 1 as const } : {}),
    ...(search.exp ? { exp: 1 as const } : {}),
  };
}

function withRange(search: SessionsSearch, state: RangeState): SessionsSearch {
  const { range: _range, from: _from, to: _to, ...rest } = search;
  if (state.kind === "preset") {
    return { ...rest, range: state.preset };
  }
  return { ...rest, from: state.from, to: state.to };
}

function withFilter(search: SessionsSearch, value: string): SessionsSearch {
  const { filter: _filter, ...rest } = search;
  const trimmed = value.trim();
  return trimmed.length > 0 ? { ...rest, filter: trimmed } : rest;
}

function withFlag(search: SessionsSearch, key: "eval" | "exp", on: boolean): SessionsSearch {
  const { [key]: _flag, ...rest } = search;
  return on ? { ...rest, [key]: 1 } : rest;
}

function withCols(search: SessionsSearch, cols: string | undefined): SessionsSearch {
  const { cols: _cols, ...rest } = search;
  return cols ? { ...rest, cols } : rest;
}

function withSort(
  search: SessionsSearch,
  next: { id: string; desc: boolean } | null,
): SessionsSearch {
  const { sort: _sort, order: _order, ...rest } = search;
  if (!next) {
    return rest;
  }
  const sort = COLUMN_SORT[next.id];
  if (!sort) {
    return rest;
  }
  if (sort === "start_time" && next.desc) {
    return rest;
  }
  return { ...rest, sort, order: next.desc ? "desc" : "asc" };
}
