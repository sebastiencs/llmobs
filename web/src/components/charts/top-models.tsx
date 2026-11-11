import type { Schema } from "@/api/types";
import { formatCost, formatTokens } from "@/lib/format";
import { formatCount } from "@/lib/metrics";

export function TopModels({ models }: { models: readonly Schema<"ModelUsage">[] }) {
  const label =
    models.length === 0
      ? "Top models by cost: no data in range"
      : `Top models by cost: ${models
          .map((model) => `${model.model} ${formatCost(model.cost_total)}`)
          .join(", ")} in range`;
  return (
    <section aria-label={label} className="flex h-[220px] min-w-0 flex-col rounded-md border p-3">
      <CardHeading title="Top models by cost" unit="USD" />
      {models.length === 0 ? (
        <NoData />
      ) : (
        <div className="mt-2 min-h-0 flex-1 overflow-auto">
          <table className="w-full text-left text-xs">
            <thead className="text-muted-foreground">
              <tr>
                <th className="py-1 font-medium">Model</th>
                <th className="py-1 text-right font-medium">LLM calls</th>
                <th className="py-1 text-right font-medium">Tokens</th>
                <th className="py-1 text-right font-medium">Cost</th>
              </tr>
            </thead>
            <tbody>
              {models.map((model) => (
                <tr key={model.model} className="border-t border-border/60">
                  <td className="max-w-0 truncate py-1 pr-2 font-mono">{model.model}</td>
                  <td className="py-1 text-right font-mono tabular-nums">
                    {formatCount(model.llm_count)}
                  </td>
                  <td className="py-1 text-right font-mono tabular-nums">
                    {formatTokens(model.total_tokens)}
                  </td>
                  <td className="py-1 text-right font-mono tabular-nums">
                    {formatCost(model.cost_total)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

export function CardHeading({ title, unit }: { title: string; unit: string }) {
  return (
    <header className="flex items-baseline justify-between gap-2">
      <h2 className="text-sm font-medium">{title}</h2>
      <span className="font-mono text-[11px] text-muted-foreground">{unit}</span>
    </header>
  );
}

export function NoData() {
  return (
    <p className="flex flex-1 items-center justify-center text-muted-foreground text-sm">No data</p>
  );
}
