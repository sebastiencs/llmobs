import { memo, useMemo } from "react";
import { Bar, CartesianGrid, ComposedChart, Legend, Line, XAxis, YAxis } from "recharts";
import type { Schema } from "@/api/types";
import { ChartContainer, ChartTooltip, ChartTooltipContent } from "@/components/ui/chart";
import { formatTimestamp } from "@/lib/format";
import { useTimeZone } from "@/lib/settings";
import {
  CHART_MAX_POINTS,
  CHART_MAX_SERIES,
  type ChartSpec,
  chartLabel,
  chartSpec,
  X_KEY,
} from "@/lib/sql/chart";

const COLORS = [
  "var(--chart-1)",
  "var(--chart-2)",
  "var(--chart-3)",
  "var(--chart-4)",
  "var(--chart-5)",
];
const DAY_MS = 86_400_000;

export const NO_CHART =
  "No chart for this result: it needs a time or text column and a numeric column.";
export const CLIPPED = `Showing the first ${CHART_MAX_POINTS} rows and at most ${CHART_MAX_SERIES} series.`;

/** `time` for a timestamp axis under a day long, `datetime` otherwise. */
function tickStyle(spec: ChartSpec): "time" | "datetime" {
  const first = Date.parse(String(spec.data[0]?.[X_KEY] ?? ""));
  const last = Date.parse(String(spec.data.at(-1)?.[X_KEY] ?? ""));
  return Number.isFinite(first) && Number.isFinite(last) && last - first < DAY_MS
    ? "time"
    : "datetime";
}

export const SqlResultChart = memo(function SqlResultChart({
  result,
}: {
  result: Schema<"SqlQueryResult">;
}) {
  const zone = useTimeZone();
  const spec = useMemo(() => chartSpec(result), [result]);
  if (!spec) {
    return <p className="text-muted-foreground text-sm">{NO_CHART}</p>;
  }
  const style = tickStyle(spec);
  const tick = (value: unknown) =>
    spec.x.kind === "timestamp" ? formatTimestamp(String(value), zone, style) : String(value);
  return (
    <section aria-label={chartLabel(spec)} className="flex min-h-0 flex-1 flex-col gap-1">
      {spec.clipped ? <p className="text-muted-foreground text-xs">{CLIPPED}</p> : null}
      <ChartContainer config={{}} className="aspect-auto h-72 w-full">
        <ComposedChart data={spec.data} margin={{ left: 8, right: 8, top: 8, bottom: 8 }}>
          <CartesianGrid vertical={false} />
          <XAxis dataKey={X_KEY} tickFormatter={tick} minTickGap={24} />
          <YAxis width={56} />
          <ChartTooltip content={<ChartTooltipContent labelFormatter={(value) => tick(value)} />} />
          <Legend />
          {spec.series.map((name, index) => {
            const color = COLORS[index % COLORS.length];
            return spec.kind === "line" ? (
              <Line
                key={name}
                name={name}
                dataKey={name}
                type="monotone"
                dot={false}
                stroke={color}
                isAnimationActive={false}
                connectNulls
              />
            ) : (
              <Bar key={name} name={name} dataKey={name} fill={color} isAnimationActive={false} />
            );
          })}
        </ComposedChart>
      </ChartContainer>
    </section>
  );
});
