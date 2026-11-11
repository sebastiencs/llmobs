import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import { problemOf } from "@/api/problem";
import { metricsQuery } from "@/api/queries";
import { KpiRow } from "@/components/charts/kpi-tiles";
import { DashboardBody } from "@/components/charts/metric-chart";
import { FilterBar } from "@/components/filter-bar/filter-bar";
import { filterMessage } from "@/components/filter-bar/logic";
import { ListToolbar } from "@/components/list-toolbar";
import { useProject } from "@/components/shell/project-context";
import { TooltipProvider } from "@/components/ui/tooltip";
import { bucketRange } from "@/lib/metrics";
import { type DashboardSearch, parseDashboardSearch } from "@/lib/search/dashboard";
import { type RangeState, rangeFromSearch, resolveRange } from "@/lib/time-range";

export const Route = createFileRoute("/$org/$project/dashboard/")({
  validateSearch: (search) => parseDashboardSearch(search),
  component: DashboardPage,
});

function DashboardPage() {
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
  const metrics = useQuery({
    ...metricsQuery(org, project, {
      from: resolved.from,
      to: resolved.to,
      filter: search.filter,
      groupBy: search.group,
      includeEval: search.eval === 1,
      includeExperiments: search.exp === 1,
    }),
    staleTime: 10_000,
    placeholderData: keepPreviousData,
  });
  const problem = metrics.isError ? problemOf(metrics.error) : undefined;
  const filterError = problem?.code === "invalid_filter" ? problem : undefined;
  if (problem && !filterError) {
    throw metrics.error;
  }
  const data = filterError ? undefined : metrics.data;

  function openBucket(timeMs: number) {
    if (!data) {
      return;
    }
    const bucket = bucketRange(timeMs, data.bucket_ns);
    void navigate({
      to: "/$org/$project/traces",
      params: { org, project },
      search: {
        from: bucket.from,
        to: bucket.to,
        ...(search.filter ? { filter: search.filter } : {}),
        ...(search.eval ? { eval: 1 as const } : {}),
        ...(search.exp ? { exp: 1 as const } : {}),
      },
    });
  }

  return (
    <div className="flex h-full min-h-0 flex-1 flex-col">
      <TooltipProvider>
        <ListToolbar
          title="Dashboard"
          idPrefix="dashboard"
          range={range}
          onRange={(next) => {
            seenRange.current = next.kind === "preset" ? next.preset : `${next.from}\n${next.to}`;
            setAnchor(Date.now());
            void navigate({
              to: "/$org/$project/dashboard",
              params: { org, project },
              search: (prev) => withRange(prev, next),
            });
          }}
          onRefresh={() => setAnchor(Date.now())}
          evalOn={search.eval === 1}
          onEval={(on) => {
            void navigate({
              to: "/$org/$project/dashboard",
              params: { org, project },
              search: (prev) => withFlag(prev, "eval", on),
            });
          }}
          expOn={search.exp === 1}
          onExp={(on) => {
            void navigate({
              to: "/$org/$project/dashboard",
              params: { org, project },
              search: (prev) => withFlag(prev, "exp", on),
            });
          }}
          filter={
            <div className="flex min-w-0 items-center gap-2">
              <div className="min-w-0 flex-1">
                <FilterBar
                  view="traces"
                  value={search.filter ?? ""}
                  error={filterError}
                  onApply={(text) => {
                    void navigate({
                      to: "/$org/$project/dashboard",
                      params: { org, project },
                      search: (prev) => withFilter(prev, text),
                    });
                  }}
                />
              </div>
              <GroupBy
                value={search.group}
                onChange={(group) => {
                  void navigate({
                    to: "/$org/$project/dashboard",
                    params: { org, project },
                    search: (prev) => withGroup(prev, group),
                  });
                }}
              />
            </div>
          }
        />
        <div className="min-h-0 flex-1 overflow-auto p-3">
          {filterError ? (
            <div className="mx-auto flex max-w-md flex-col gap-2 px-4 py-16">
              <h2 className="text-sm font-medium">{filterError.title}</h2>
              <p className="text-destructive text-sm">{filterMessage(filterError)}</p>
            </div>
          ) : data ? (
            <>
              <KpiRow kpis={data.kpis} />
              <DashboardBody response={data} onBucket={openBucket} />
            </>
          ) : (
            <p className="text-muted-foreground text-sm">Loading…</p>
          )}
        </div>
      </TooltipProvider>
    </div>
  );
}

function GroupBy({
  value,
  onChange,
}: {
  value: DashboardSearch["group"];
  onChange: (group: DashboardSearch["group"]) => void;
}) {
  return (
    <label className="flex shrink-0 items-center gap-1.5 text-muted-foreground text-xs">
      <span id="dashboard-group-label">Group by</span>
      <select
        aria-labelledby="dashboard-group-label"
        className="focus-ring h-6 rounded-md border border-border bg-background px-1.5 text-foreground text-xs"
        value={value ?? ""}
        onChange={(event) => {
          const next = event.target.value;
          onChange(
            next === "model" || next === "environment" || next === "release" ? next : undefined,
          );
        }}
      >
        <option value="">None</option>
        <option value="model">Model</option>
        <option value="environment">Environment</option>
        <option value="release">Release</option>
      </select>
    </label>
  );
}

function withRange(search: DashboardSearch, state: RangeState): DashboardSearch {
  const { range: _range, from: _from, to: _to, ...rest } = search;
  if (state.kind === "preset") {
    return { ...rest, range: state.preset };
  }
  return { ...rest, from: state.from, to: state.to };
}

function withFilter(search: DashboardSearch, value: string): DashboardSearch {
  const { filter: _filter, ...rest } = search;
  const trimmed = value.trim();
  return trimmed.length > 0 ? { ...rest, filter: trimmed } : rest;
}

function withFlag(search: DashboardSearch, key: "eval" | "exp", on: boolean): DashboardSearch {
  const { [key]: _flag, ...rest } = search;
  return on ? { ...rest, [key]: 1 } : rest;
}

function withGroup(search: DashboardSearch, group: DashboardSearch["group"]): DashboardSearch {
  const { group: _group, ...rest } = search;
  return group ? { ...rest, group } : rest;
}
