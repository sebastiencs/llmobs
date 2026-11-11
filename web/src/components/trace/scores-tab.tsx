import { useQuery } from "@tanstack/react-query";
import { problemOf } from "@/api/problem";
import { scoresQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { IdText } from "@/components/id-text";
import { useProject } from "@/components/shell/project-context";
import { TimeText } from "@/components/time-text";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

export function ScoresTab({ traceId, spanId }: { traceId: string; spanId: string }) {
  const { org, project } = useProject();
  const traceScores = useQuery(
    scoresQuery(org, project, { trace_id: traceId, target: "trace", limit: 1000 }),
  );
  const spanScores = useQuery(
    scoresQuery(org, project, {
      trace_id: traceId,
      span_id: spanId,
      target: "span",
      limit: 1000,
    }),
  );

  if (traceScores.isPending || spanScores.isPending) {
    return <Skeleton className="h-4 w-1/2" />;
  }
  if (traceScores.isError || spanScores.isError) {
    const error = traceScores.error ?? spanScores.error;
    return <p className="text-sm">{problemOf(error)?.title ?? "Could not load scores"}</p>;
  }

  const onTrace = byName(forTrace(traceScores.data.data, traceId));
  const onSpan = byName(forSpan(spanScores.data.data, spanId));
  if (onTrace.length === 0 && onSpan.length === 0) {
    return <p className="text-xs text-muted-foreground">No scores</p>;
  }

  return (
    <div className="flex flex-col gap-3">
      <ScoreGroup title="On this span" scores={onSpan} />
      <ScoreGroup title="On this trace" scores={onTrace} />
    </div>
  );
}

function ScoreGroup({ title, scores }: { title: string; scores: Schema<"Score">[] }) {
  return (
    <section className="flex flex-col gap-1">
      <h3 className="text-xs font-medium">{title}</h3>
      {scores.length > 0 ? <ScoreTable scores={scores} /> : null}
    </section>
  );
}

function ScoreTable({ scores }: { scores: Schema<"Score">[] }) {
  return (
    <table className="w-full text-xs">
      <thead className="text-left text-muted-foreground">
        <tr>
          <th className="py-1 pr-2 font-medium">Name</th>
          <th className="py-1 pr-2 font-medium">Value</th>
          <th className="py-1 pr-2 font-medium">Source</th>
          <th className="py-1 pr-2 font-medium">Reason</th>
          <th className="py-1 pr-2 font-medium">Author</th>
          <th className="py-1 font-medium">Updated</th>
        </tr>
      </thead>
      <tbody>
        {scores.map((score) => (
          <tr key={score.score_id} className="border-t">
            <td className="py-1 pr-2">{score.name}</td>
            <td className="py-1 pr-2">{scoreValue(score.value)}</td>
            <td className="py-1 pr-2">{score.source}</td>
            <td className="py-1 pr-2">
              <Reason reason={score.reason} />
            </td>
            <td className="py-1 pr-2">
              <Author score={score} />
            </td>
            <td className="py-1">
              <TimeText value={score.updated_at} style="datetime" />
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function Reason({ reason }: { reason: string | undefined }) {
  if (!reason) {
    return "—";
  }
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span className="block max-w-48 truncate">{reason}</span>
      </TooltipTrigger>
      <TooltipContent>{reason}</TooltipContent>
    </Tooltip>
  );
}

function Author({ score }: { score: Schema<"Score"> }) {
  if (score.author_user_id) {
    return <IdText value={score.author_user_id} kind="author" />;
  }
  if (score.evaluator_id) {
    return <IdText value={score.evaluator_id} kind="evaluator" />;
  }
  return "—";
}

function forTrace(scores: Schema<"Score">[], traceId: string): Schema<"Score">[] {
  return scores.filter(
    (score) => !score.deleted && score.target.type === "trace" && score.target.trace_id === traceId,
  );
}

function forSpan(scores: Schema<"Score">[], spanId: string): Schema<"Score">[] {
  return scores.filter(
    (score) => !score.deleted && score.target.type === "span" && score.target.span_id === spanId,
  );
}

function byName(scores: Schema<"Score">[]): Schema<"Score">[] {
  return [...scores].sort((left, right) => {
    const name = left.name.localeCompare(right.name);
    if (name !== 0) {
      return name;
    }
    return right.updated_at.localeCompare(left.updated_at);
  });
}

function scoreValue(value: Schema<"ScoreValue">): string {
  if (value.data_type === "numeric") {
    return value.value.toFixed(3);
  }
  if (value.data_type === "boolean") {
    return value.value ? "✓" : "✗";
  }
  if (value.data_type === "categorical") {
    return value.value.label;
  }
  return value.value;
}
