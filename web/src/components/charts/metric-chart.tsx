import { type KeyboardEvent, useMemo, useState } from "react";
import type { BarShapeProps } from "recharts";
import {
  Bar,
  CartesianGrid,
  ComposedChart,
  Legend,
  Line,
  ReferenceLine,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import type { Schema } from "@/api/types";
import { CardHeading, NoData, TopModels } from "@/components/charts/top-models";
import { type ChartConfig, ChartContainer } from "@/components/ui/chart";
import { formatTimestamp } from "@/lib/format";
import {
  type ChartRow,
  foldGroups,
  formatCount,
  formatMetric,
  type MetricName,
  type MetricSeries,
  presentSum,
  seriesByMetric,
  toChartRows,
} from "@/lib/metrics";
import { type TimeZoneSetting, useTimeZone } from "@/lib/settings";

const DAY_MS = 24 * 60 * 60 * 1000;
const PALETTE = [
  "var(--chart-1)",
  "var(--chart-2)",
  "var(--chart-3)",
  "var(--chart-4)",
  "var(--chart-5)",
] as const;

const METRIC_LABEL: Record<MetricName, string> = {
  traces: "Traces",
  errors: "Errors",
  error_rate: "Error rate",
  latency_p50: "p50",
  latency_p95: "p95",
  latency_p99: "p99",
  cost: "Cost",
  input_tokens: "Input",
  output_tokens: "Output",
  cache_read_tokens: "Cache read",
  cache_write_tokens: "Cache write",
  ttft_p50: "p50",
  ttft_p95: "p95",
  llm_latency_p50: "p50",
  llm_latency_p95: "p95",
  cost_by_model: "Cost",
  score_avg: "Score",
  eval_cost: "Evaluation cost",
};

type ChartKind = "composed" | "line" | "bar" | "stacked";
type SummaryKind = "counts" | "sum" | "parts" | "series";

type ChartSpec = {
  title: string;
  unit: string;
  metrics: readonly MetricName[];
  kind: ChartKind;
  summary: SummaryKind;
};

const CHARTS: readonly ChartSpec[] = [
  {
    title: "Traces & errors",
    unit: "count",
    metrics: ["traces", "errors"],
    kind: "composed",
    summary: "counts",
  },
  { title: "Error rate", unit: "%", metrics: ["error_rate"], kind: "line", summary: "series" },
  {
    title: "Latency",
    unit: "duration",
    metrics: ["latency_p50", "latency_p95", "latency_p99"],
    kind: "line",
    summary: "series",
  },
  {
    title: "LLM latency by model",
    unit: "duration",
    metrics: ["llm_latency_p50", "llm_latency_p95"],
    kind: "line",
    summary: "series",
  },
  {
    title: "Cost by model",
    unit: "USD",
    metrics: ["cost_by_model"],
    kind: "stacked",
    summary: "sum",
  },
  {
    title: "Tokens by type",
    unit: "tokens",
    metrics: ["input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens"],
    kind: "stacked",
    summary: "parts",
  },
  {
    title: "TTFT",
    unit: "duration",
    metrics: ["ttft_p50", "ttft_p95"],
    kind: "line",
    summary: "series",
  },
  { title: "Scores", unit: "score", metrics: ["score_avg"], kind: "line", summary: "series" },
  { title: "Evaluation cost", unit: "USD", metrics: ["eval_cost"], kind: "bar", summary: "sum" },
];

type Column = {
  key: string;
  label: string;
  metric: MetricName;
  color: string;
  dashed?: string;
  mark: "bar" | "line";
  stacked: boolean;
  series: MetricSeries;
};

const AXIS_TICK = {
  fontSize: 11,
  fontFamily: "var(--font-mono)",
  fill: "var(--muted-foreground)",
} as const;

export function DashboardBody({
  response,
  onBucket,
}: {
  response: Schema<"MetricsResponse">;
  onBucket: (timeMs: number) => void;
}) {
  const byMetric = seriesByMetric(response);
  return (
    <div className="mt-3 grid grid-cols-1 gap-3 xl:grid-cols-2">
      {CHARTS.map((spec) => (
        <ChartCard
          key={spec.title}
          spec={spec}
          byMetric={byMetric}
          bucketNs={response.bucket_ns}
          onBucket={onBucket}
        />
      ))}
      <TopModels models={response.top_models} />
    </div>
  );
}

function ChartCard({
  spec,
  byMetric,
  bucketNs,
  onBucket,
}: {
  spec: ChartSpec;
  byMetric: Map<MetricName, MetricSeries[]>;
  bucketNs: number;
  onBucket: (timeMs: number) => void;
}) {
  const described = describe(spec, byMetric);
  const columns = described.hasData ? columnsFor(spec, byMetric) : [];
  const rows = toChartRows(
    columns.map((column) => ({ ...column.series, group: column.key })),
    bucketNs,
  );
  return (
    <section
      aria-label={described.label}
      className="flex h-[220px] min-w-0 flex-col rounded-md border p-3"
    >
      <CardHeading title={spec.title} unit={spec.unit} />
      {described.hasData ? (
        <MetricPlot title={spec.title} columns={columns} rows={rows} onBucket={onBucket} />
      ) : (
        <NoData />
      )}
    </section>
  );
}

function MetricPlot({
  title,
  columns,
  rows,
  onBucket,
}: {
  title: string;
  columns: Column[];
  rows: ChartRow[];
  onBucket: (timeMs: number) => void;
}) {
  const zone = useTimeZone();
  const style = axisStyle(rows);
  const config = useMemo(() => chartConfig(columns), [columns]);
  const yMetric = columns[0]?.metric ?? "traces";
  const [active, setActive] = useState<number | null>(null);
  const index = active !== null && active < rows.length ? active : null;
  const activeRow = index === null ? undefined : rows[index];

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (rows.length === 0) return;
    const last = rows.length - 1;
    let next: number | null | undefined;
    if (event.key === "ArrowRight") next = index === null ? 0 : Math.min(last, index + 1);
    else if (event.key === "ArrowLeft") next = index === null ? last : Math.max(0, index - 1);
    else if (event.key === "Home") next = 0;
    else if (event.key === "End") next = last;
    else if (event.key === "Escape") next = null;
    else if ((event.key === "Enter" || event.key === " ") && activeRow) {
      event.preventDefault();
      onBucket(activeRow.time);
      return;
    }
    if (next === undefined) return;
    event.preventDefault();
    setActive(next);
  }

  return (
    // biome-ignore lint/a11y/useSemanticElements: a chart is a focusable group with a custom keyboard model (D-C-3)
    <div
      role="group"
      aria-roledescription="chart"
      aria-label={`${title}. Arrow keys move between time buckets; Enter opens the traces of the bucket.`}
      // biome-ignore lint/a11y/noNoninteractiveTabindex: the chart is one keyboard stop (D-C-3)
      tabIndex={0}
      className="focus-ring mt-2 flex min-h-0 flex-1 flex-col rounded-sm"
      onKeyDown={onKeyDown}
      onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget)) setActive(null);
      }}
    >
      <ChartContainer
        config={config}
        data-chart-container=""
        initialDimension={{ width: 640, height: 160 }}
        className="aspect-auto min-h-0 w-full flex-1"
      >
        <ComposedChart
          data={rows}
          margin={{ top: 8, right: 12, left: 0, bottom: 0 }}
          accessibilityLayer={false}
        >
          <CartesianGrid stroke="var(--border)" strokeOpacity={0.7} vertical={false} />
          <XAxis
            dataKey="time"
            tick={AXIS_TICK}
            tickFormatter={(value) => formatTick(value, zone, style)}
            minTickGap={28}
            axisLine={false}
            tickLine={false}
          />
          <YAxis
            tick={AXIS_TICK}
            tickFormatter={(value) =>
              formatMetric(yMetric, typeof value === "number" ? value : null)
            }
            width={72}
            axisLine={false}
            tickLine={false}
          />
          <Tooltip
            content={<SeriesTooltip columns={columns} zone={zone} axisStyle={style} />}
            cursor={{ stroke: "var(--border)" }}
          />
          {columns.length > 1 ? <Legend iconSize={8} wrapperStyle={{ fontSize: 11 }} /> : null}
          {columns.map((column) =>
            column.mark === "bar" ? (
              <Bar
                key={column.key}
                dataKey={column.key}
                name={column.label}
                fill={column.color}
                stackId={column.stacked ? "stack" : undefined}
                isAnimationActive={false}
                shape={(props: BarShapeProps) => <BucketBar {...props} onBucket={onBucket} />}
              />
            ) : (
              <Line
                key={column.key}
                dataKey={column.key}
                name={column.label}
                stroke={column.color}
                strokeWidth={1.5}
                strokeDasharray={column.dashed}
                dot={lineDot(column.key, onBucket, solitary(rows, column.key))}
                activeDot={{ r: 3, strokeWidth: 1.5 }}
                connectNulls={false}
                isAnimationActive={false}
                type="linear"
              />
            ),
          )}
          {activeRow ? (
            <ReferenceLine
              x={activeRow.time}
              stroke="var(--ring)"
              strokeWidth={1.5}
              ifOverflow="extendDomain"
            />
          ) : null}
        </ComposedChart>
      </ChartContainer>
      <p className="sr-only" aria-live="polite">
        {activeRow ? bucketSummary(activeRow, columns, zone, style) : ""}
      </p>
    </div>
  );
}

function SeriesTooltip({
  active,
  payload,
  columns,
  zone,
  axisStyle: style,
}: {
  active?: boolean;
  payload?: ReadonlyArray<{ payload?: ChartRow }>;
  columns: Column[];
  zone: TimeZoneSetting;
  axisStyle: "time" | "datetime";
}) {
  const row = payload?.[0]?.payload;
  if (!active || !row) {
    return null;
  }
  return (
    <div className="grid min-w-32 gap-1 rounded-md border bg-popover px-2.5 py-1.5 text-xs shadow-md">
      <div className="font-mono text-[11px] text-muted-foreground">
        {formatTimestamp(new Date(row.time).toISOString(), zone, style)}
      </div>
      {columns.map((column) => (
        <div key={column.key} className="flex items-center justify-between gap-3">
          <span className="flex items-center gap-1.5 text-muted-foreground">
            <span className="size-2 shrink-0 rounded-[2px]" style={{ background: column.color }} />
            {column.label}
          </span>
          <span className="font-mono text-foreground tabular-nums">
            {formatMetric(column.metric, row[column.key] ?? null)}
          </span>
        </div>
      ))}
    </div>
  );
}

function BucketBar({
  x,
  y,
  width,
  height,
  fill,
  payload,
  onBucket,
}: BarShapeProps & { onBucket: (timeMs: number) => void }) {
  const left = x ?? 0;
  const top = y ?? 0;
  const time = rowTime(payload);
  if (time === undefined || width <= 0 || height <= 0) {
    return (
      <rect x={left} y={top} width={Math.max(width, 0)} height={Math.max(height, 0)} fill={fill} />
    );
  }
  return (
    <g>
      <rect x={left} y={top} width={width} height={height} fill={fill} />
      {/* biome-ignore lint/a11y/noAriaHiddenOnFocusable: mouse-only bucket target with tabIndex -1; the chart group is the keyboard path (D-C-3) */}
      <foreignObject x={left} y={top} width={width} height={height} aria-hidden="true">
        <button
          type="button"
          tabIndex={-1}
          aria-label={bucketLabel(time)}
          className="focus-ring block h-full w-full cursor-pointer border-0 bg-transparent p-0"
          onClick={() => onBucket(time)}
        />
      </foreignObject>
    </g>
  );
}

function lineDot(dataKey: string, onBucket: (timeMs: number) => void, showMarker: boolean) {
  return (props: unknown) => {
    if (typeof props !== "object" || props === null) {
      return <g />;
    }
    const point = props as {
      cx?: number;
      cy?: number;
      payload?: ChartRow;
      value?: number | null;
      stroke?: string;
    };
    const time = point.payload?.time;
    const value = point.value ?? (point.payload ? point.payload[dataKey] : undefined);
    if (
      typeof point.cx !== "number" ||
      typeof point.cy !== "number" ||
      typeof time !== "number" ||
      value === null ||
      value === undefined
    ) {
      return <g />;
    }
    return (
      <g>
        {showMarker ? <circle cx={point.cx} cy={point.cy} r={3} fill={point.stroke} /> : null}
        {/* biome-ignore lint/a11y/noAriaHiddenOnFocusable: mouse-only bucket target with tabIndex -1; the chart group is the keyboard path (D-C-3) */}
        <foreignObject x={point.cx - 8} y={point.cy - 8} width={16} height={16} aria-hidden="true">
          <button
            type="button"
            tabIndex={-1}
            aria-label={bucketLabel(time)}
            className="focus-ring block size-full cursor-pointer border-0 bg-transparent p-0"
            onClick={() => onBucket(time)}
          />
        </foreignObject>
      </g>
    );
  };
}

function solitary(rows: readonly ChartRow[], key: string): boolean {
  let count = 0;
  for (const row of rows) {
    const value = row[key];
    if (value !== null && value !== undefined) {
      count += 1;
      if (count > 1) {
        return false;
      }
    }
  }
  return count === 1;
}

function bucketLabel(timeMs: number): string {
  return `Open bucket ${new Date(timeMs).toISOString()}`;
}

function bucketSummary(
  row: ChartRow,
  columns: readonly Column[],
  zone: TimeZoneSetting,
  style: "time" | "datetime",
): string {
  const values = columns
    .map((column) => `${column.label} ${formatMetric(column.metric, row[column.key] ?? null)}`)
    .join(", ");
  return `${formatTimestamp(new Date(row.time).toISOString(), zone, style)}: ${values}`;
}

function rowTime(payload: unknown): number | undefined {
  if (typeof payload !== "object" || payload === null || !("time" in payload)) {
    return undefined;
  }
  const time = payload.time;
  return typeof time === "number" ? time : undefined;
}

function columnsFor(spec: ChartSpec, byMetric: Map<MetricName, MetricSeries[]>): Column[] {
  const present = spec.metrics.filter((metric) => (byMetric.get(metric) ?? []).length > 0);
  const multi = present.length > 1;
  const colors = new Map<string, string>();
  let nextColor = 0;
  const columns: Column[] = [];
  for (const metric of present) {
    for (const series of foldGroups(byMetric.get(metric) ?? [])) {
      const group = series.group;
      const key = columnKey(metric, group, multi);
      const colorKey = group ?? metric;
      let color = colors.get(colorKey);
      if (!color) {
        color = PALETTE[nextColor % PALETTE.length] ?? PALETTE[0];
        nextColor += 1;
        colors.set(colorKey, color);
      }
      columns.push({
        key,
        label: columnLabel(metric, group, multi),
        metric,
        color,
        dashed: dashOf(metric),
        mark: markOf(spec.kind, metric),
        stacked: spec.kind === "stacked",
        series,
      });
    }
  }
  return columns;
}

function columnKey(metric: MetricName, group: string | undefined, multi: boolean): string {
  if (group) {
    return multi ? `${metric}\u001f${group}` : group;
  }
  return multi ? metric : "all";
}

function columnLabel(metric: MetricName, group: string | undefined, multi: boolean): string {
  const name = METRIC_LABEL[metric];
  if (!group) {
    return name;
  }
  return multi ? `${group} ${name}` : group;
}

function markOf(kind: ChartKind, metric: MetricName): "bar" | "line" {
  if (kind === "line") {
    return "line";
  }
  if (kind === "composed") {
    return metric === "traces" ? "bar" : "line";
  }
  return "bar";
}

function dashOf(metric: MetricName): string | undefined {
  if (metric.endsWith("p99")) {
    return "1.5 3";
  }
  if (metric.endsWith("p95")) {
    return "4 3";
  }
  return undefined;
}

function chartConfig(columns: Column[]): ChartConfig {
  const config: ChartConfig = {};
  for (const column of columns) {
    config[column.key] = { label: column.label };
  }
  return config;
}

function describe(
  spec: ChartSpec,
  byMetric: Map<MetricName, MetricSeries[]>,
): { label: string; hasData: boolean } {
  const folded = spec.metrics.flatMap((metric) => foldGroups(byMetric.get(metric) ?? []));
  const hasData = folded.some((item) => item.points.some((point) => point.value !== undefined));
  if (!hasData) {
    return { label: `${spec.title}: no data in range`, hasData: false };
  }
  const clauses = summaryClauses(spec, byMetric, folded);
  if (clauses.length === 0) {
    return { label: `${spec.title}: no data in range`, hasData: false };
  }
  return { label: `${spec.title}: ${clauses.join(", ")} in range`, hasData: true };
}

function summaryClauses(
  spec: ChartSpec,
  byMetric: Map<MetricName, MetricSeries[]>,
  folded: readonly MetricSeries[],
): string[] {
  if (spec.summary === "counts") {
    const clauses: string[] = [];
    const traces = presentSum(byMetric.get("traces") ?? []);
    const errors = presentSum(byMetric.get("errors") ?? []);
    if (traces !== undefined) {
      clauses.push(countClause(traces, "trace", "traces"));
    }
    if (errors !== undefined) {
      clauses.push(countClause(errors, "error", "errors"));
    }
    return clauses;
  }
  if (spec.summary === "sum") {
    const metric = spec.metrics[0];
    const sum = presentSum(folded);
    return metric && sum !== undefined ? [formatMetric(metric, sum)] : [];
  }
  if (spec.summary === "parts") {
    const clauses: string[] = [];
    for (const metric of spec.metrics) {
      const sum = presentSum(byMetric.get(metric) ?? []);
      if (sum !== undefined) {
        clauses.push(`${METRIC_LABEL[metric]} ${formatMetric(metric, sum)}`);
      }
    }
    return clauses;
  }
  const clauses: string[] = [];
  for (const item of folded) {
    const last = lastPresent(item);
    if (last === undefined) {
      continue;
    }
    const text = formatMetric(item.metric, last);
    clauses.push(
      folded.length === 1 && !item.group
        ? text
        : `${item.group ?? METRIC_LABEL[item.metric]} ${text}`,
    );
  }
  return clauses;
}

function countClause(value: number, singular: string, plural: string): string {
  const rounded = Math.round(value);
  return `${formatCount(rounded)} ${rounded === 1 ? singular : plural}`;
}

function lastPresent(series: MetricSeries): number | undefined {
  let last: number | undefined;
  for (const point of series.points) {
    if (point.value !== undefined) {
      last = point.value;
    }
  }
  return last;
}

function axisStyle(rows: ChartRow[]): "time" | "datetime" {
  const first = rows[0]?.time;
  const last = rows[rows.length - 1]?.time;
  if (first === undefined || last === undefined || last - first < DAY_MS) {
    return "time";
  }
  return "datetime";
}

function formatTick(value: unknown, zone: TimeZoneSetting, style: "time" | "datetime"): string {
  const ms = typeof value === "number" ? value : Number(value);
  if (Number.isNaN(ms)) {
    return "";
  }
  const date = new Date(ms);
  const utc = zone === "utc";
  const hours = pad(utc ? date.getUTCHours() : date.getHours());
  const minutes = pad(utc ? date.getUTCMinutes() : date.getMinutes());
  const clock = `${hours}:${minutes}`;
  if (style === "time") {
    return clock;
  }
  const month = pad((utc ? date.getUTCMonth() : date.getMonth()) + 1);
  const day = pad(utc ? date.getUTCDate() : date.getDate());
  return `${month}-${day} ${clock}`;
}

function pad(value: number): string {
  return String(value).padStart(2, "0");
}
