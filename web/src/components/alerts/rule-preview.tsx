import { keepPreviousData } from "@tanstack/react-query";
import { type ReactNode, useEffect, useMemo, useState } from "react";
import { CartesianGrid, ComposedChart, Line, ReferenceLine, XAxis, YAxis } from "recharts";
import { $api } from "@/api/client";
import { problemOf } from "@/api/problem";
import { projectRef } from "@/api/project-ref";
import { filterMessage } from "@/components/filter-bar/logic";
import { FormError } from "@/components/form/form-error";
import { ChartContainer } from "@/components/ui/chart";
import { Skeleton } from "@/components/ui/skeleton";
import {
  conditionFromForm,
  formatAlertValue,
  metricLabel,
  previewCondition,
  previewParams,
  previewSeries,
  type RuleFormValues,
} from "@/lib/alerts";
import { formatTimestamp } from "@/lib/format";
import { seriesByMetric, toChartRows } from "@/lib/metrics";
import { useTimeZone } from "@/lib/settings";

const NS_PER_MS = 1_000_000;

const RANGE_LABEL: Record<RuleFormValues["window"], string> = {
  "1m": "24 hours",
  "5m": "24 hours",
  "15m": "24 hours",
  "1h": "24 hours",
  "6h": "3 days",
  "24h": "14 days",
};

/** The metric over the rule's window-wide buckets, with the threshold as a line (D-K-13). */
export function RulePreview(props: { org: string; project: string; values: RuleFormValues }) {
  const { org, project, values } = props;
  const zone = useTimeZone();
  const series = previewSeries(values);
  const [filter, setFilter] = useState(values.filter.trim());
  useEffect(() => {
    const timer = setTimeout(() => {
      setFilter(values.filter.trim());
    }, 500);
    return () => {
      clearTimeout(timer);
    };
  }, [values.filter]);
  const params = useMemo(() => previewParams(values.window, Date.now()), [values.window]);
  const query = $api.useQuery(
    "get",
    "/api/v1/projects/{project}/metrics",
    {
      params: {
        path: { project: projectRef(org, project) },
        query: {
          from: params.from,
          to: params.to,
          bucket: params.bucket,
          ...(filter ? { filter } : {}),
        },
      },
    },
    {
      staleTime: 60_000,
      placeholderData: keepPreviousData,
      enabled: series !== undefined,
    },
  );

  if (series === undefined) {
    return (
      <div className="rounded-md border border-dashed p-4 text-sm text-muted-foreground">
        No preview for LLM span latency. The dashboard shows it per model.
      </div>
    );
  }

  const response = query.data;
  const entry = response
    ? seriesByMetric(response)
        .get(series.metric)
        ?.find((item) => (item.group ?? undefined) === series.group)
    : undefined;
  const points =
    entry?.points.map((point) =>
      series.metric === "latency_p95"
        ? { ...point, value: point.value === undefined ? undefined : point.value / NS_PER_MS }
        : point,
    ) ?? [];
  const rows =
    response && entry
      ? toChartRows([{ ...entry, group: undefined, points }], response.bucket_ns)
      : [];
  const hasValue = rows.some((row) => typeof row.all === "number");
  const built = conditionFromForm(values);
  const threshold = built.ok ? built.condition.threshold : undefined;
  const condition = previewCondition(values);

  let body: ReactNode;
  if (query.isPending) {
    body = <Skeleton className="h-40 w-full" />;
  } else if (query.error) {
    const problem = problemOf(query.error);
    body =
      problem?.code === "invalid_filter" ? (
        <FormError message={`The filter is invalid: ${filterMessage(problem)}`} />
      ) : (
        <p className="text-sm text-muted-foreground">The preview could not be loaded.</p>
      );
  } else if (!hasValue) {
    body = <p className="text-sm text-muted-foreground">No data in this range.</p>;
  } else {
    body = (
      <figure aria-label={`Preview of the metric over the last ${RANGE_LABEL[values.window]}`}>
        <ChartContainer
          config={{ all: { label: metricLabel(condition), color: "var(--chart-1)" } }}
          data-chart-container=""
          initialDimension={{ width: 640, height: 160 }}
          className="aspect-auto h-40 w-full"
        >
          <ComposedChart data={rows} margin={{ top: 8, right: 12, left: 0, bottom: 0 }}>
            <CartesianGrid stroke="var(--border)" strokeOpacity={0.7} vertical={false} />
            <XAxis
              dataKey="time"
              tickFormatter={(ms) => formatTimestamp(new Date(ms).toISOString(), zone, "datetime")}
              minTickGap={28}
              axisLine={false}
              tickLine={false}
            />
            <YAxis
              tickFormatter={(value) =>
                formatAlertValue(condition, typeof value === "number" ? value : undefined)
              }
              width={72}
              axisLine={false}
              tickLine={false}
            />
            <Line
              dataKey="all"
              stroke="var(--chart-1)"
              strokeWidth={1.5}
              dot={false}
              connectNulls={false}
              isAnimationActive={false}
              type="linear"
            />
            {threshold === undefined ? null : (
              <ReferenceLine
                y={threshold}
                stroke="var(--destructive)"
                strokeDasharray="4 4"
                ifOverflow="extendDomain"
                label={{ value: "Threshold", position: "insideTopRight", fontSize: 11 }}
              />
            )}
          </ComposedChart>
        </ChartContainer>
      </figure>
    );
  }

  return (
    <div className="flex flex-col gap-2">
      <h2 className="text-sm font-medium">Preview</h2>
      {body}
      <p className="text-xs text-muted-foreground">
        Each point is the metric over one window, as the rule sees it. Minimum samples are not
        shown.
      </p>
    </div>
  );
}
