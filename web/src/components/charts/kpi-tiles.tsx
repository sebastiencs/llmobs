import { cn } from "cn";
import { ArrowDown, ArrowUp, Minus } from "lucide-react";
import type { Schema } from "@/api/types";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { formatKpi, type KpiDirection, type KpiMetric, kpiDelta } from "@/lib/metrics";

const TILES: { metric: KpiMetric; label: string; adverse: boolean }[] = [
  { metric: "traces", label: "Traces", adverse: false },
  { metric: "error_rate", label: "Error rate", adverse: true },
  { metric: "latency_p95", label: "p95 latency", adverse: true },
  { metric: "cost_total", label: "Cost", adverse: true },
  { metric: "total_tokens", label: "Tokens", adverse: false },
];

export function KpiRow({ kpis }: { kpis: readonly Schema<"Kpi">[] }) {
  return (
    <div className="grid grid-cols-2 gap-2 md:grid-cols-5">
      {TILES.map((tile) => (
        <KpiTile
          key={tile.metric}
          tile={tile}
          kpi={kpis.find((item) => item.metric === tile.metric)}
        />
      ))}
    </div>
  );
}

function KpiTile({ tile, kpi }: { tile: (typeof TILES)[number]; kpi?: Schema<"Kpi"> }) {
  const delta = kpiDelta(kpi ?? {});
  const previous = delta.previous === undefined ? "—" : formatKpi(tile.metric, delta.previous);
  return (
    <Tooltip>
      <TooltipTrigger
        type="button"
        className="flex h-16 flex-col justify-center rounded-md border px-3 text-left"
        aria-label={`${tile.label}: ${delta.value === undefined ? "—" : formatKpi(tile.metric, delta.value)}`}
      >
        <span className="text-muted-foreground text-xs">{tile.label}</span>
        <span className="flex items-baseline gap-2">
          <span className="font-mono text-sm tabular-nums">
            {delta.value === undefined ? "—" : formatKpi(tile.metric, delta.value)}
          </span>
          <Delta adverse={tile.adverse} direction={delta.direction} changePct={delta.changePct} />
        </span>
      </TooltipTrigger>
      <TooltipContent>Previous: {previous}</TooltipContent>
    </Tooltip>
  );
}

function Delta({
  adverse,
  direction,
  changePct,
}: {
  adverse: boolean;
  direction: KpiDirection;
  changePct?: number;
}) {
  const Icon = direction === "up" ? ArrowUp : direction === "down" ? ArrowDown : Minus;
  return (
    <span
      className={cn("inline-flex items-center gap-0.5 text-xs", deltaClass(adverse, direction))}
    >
      <Icon aria-hidden className="size-3" />
      {changePct === undefined ? null : `${formatChange(changePct)}%`}
    </span>
  );
}

function deltaClass(adverse: boolean, direction: KpiDirection): string {
  if (direction === "flat") {
    return "text-muted-foreground";
  }
  const improved = adverse ? direction === "down" : direction === "up";
  return improved ? "text-chart-2" : "text-destructive";
}

function formatChange(changePct: number): string {
  const abs = Math.abs(changePct);
  const rounded = abs >= 10 ? Math.round(abs) : Math.round(abs * 10) / 10;
  return Number.isInteger(rounded) ? String(rounded) : rounded.toFixed(1);
}
