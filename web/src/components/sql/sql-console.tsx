import { useQuery } from "@tanstack/react-query";
import { Play } from "lucide-react";
import { useState } from "react";
import { $api } from "@/api/client";
import { problemOf } from "@/api/problem";
import { schemaQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { useProject } from "@/components/shell/project-context";
import { SqlResultGrid } from "@/components/sql/result-grid";
import { SqlEditor } from "@/components/sql/sql-editor";
import { TimeRangePicker } from "@/components/time-range/time-range-picker";
import { Button } from "@/components/ui/button";
import { type SqlSearch, withSqlRange } from "@/lib/search/sql";
import { useSqlDraft } from "@/lib/sql/draft";
import {
  isSqlResult,
  RUN_FAILED,
  resultStatus,
  runShortcutLabel,
  sqlProblemMessage,
} from "@/lib/sql/format";
import { sqlErrorRange } from "@/lib/sql/position";
import { sqlRunBody } from "@/lib/sql/request";
import { rangeFromSearch, resolveRange } from "@/lib/time-range";

const ERROR_ID = "sql-error";

type LastRun = { result: Schema<"SqlQueryResult">; durationMs: number };

export function SqlConsole({
  search,
  onSearch,
}: {
  search: SqlSearch;
  /** Changes the URL search. Always given the function form, like `navigate({ search })`. */
  onSearch: (update: (previous: SqlSearch) => SqlSearch) => void;
}) {
  const { org, project, ref } = useProject();
  const schema = useQuery(schemaQuery(org, project));
  const [draft, updateDraft] = useSqlDraft(ref);
  const run = $api.useMutation("post", "/api/v1/projects/{project}/sql", { gcTime: 0 });
  const [lastRun, setLastRun] = useState<LastRun | undefined>();
  /** The last run "succeeded" without a result (an empty error body). */
  const [badAnswer, setBadAnswer] = useState(false);
  const range = rangeFromSearch(search);

  function execute(text: string) {
    if (text.trim().length === 0 || run.isPending) {
      return;
    }
    const span = resolveRange(range, Date.now());
    const started = performance.now();
    setLastRun(undefined);
    setBadAnswer(false);
    run.mutate(
      {
        params: { path: { project: ref } },
        body: sqlRunBody({ query: text, from: span.from, to: span.to }),
        headers: { Accept: "application/json" },
      },
      {
        onSuccess: (data) => {
          if (isSqlResult(data)) {
            setLastRun({ result: data, durationMs: Math.round(performance.now() - started) });
          } else {
            setBadAnswer(true);
          }
        },
      },
    );
  }

  const problem = run.isError ? problemOf(run.error) : undefined;
  const ranQuery = run.variables?.body.query;
  const position = problem?.errors?.find((error) => error.position)?.position;
  const failedHere = (run.isError || badAnswer) && ranQuery === draft.query;
  const errorRange = position && failedHere ? sqlErrorRange(draft.query, position) : null;
  const failure = run.isError
    ? problem
      ? sqlProblemMessage(problem)
      : RUN_FAILED
    : badAnswer
      ? RUN_FAILED
      : undefined;

  return (
    <div className="flex h-full min-h-0 flex-1 flex-col">
      <div className="flex min-h-9 shrink-0 items-center gap-2 overflow-x-auto border-b px-3">
        <h1 className="shrink-0 text-sm font-medium">SQL</h1>
        <TimeRangePicker
          state={range}
          onChange={(next) => onSearch((previous) => withSqlRange(previous, next))}
        />
        <div className="ml-auto flex shrink-0 items-center gap-2">
          <Button
            type="button"
            size="xs"
            title={`Run (${runShortcutLabel(navigator.userAgent)})`}
            disabled={draft.query.trim().length === 0 || run.isPending}
            onClick={() => execute(draft.query)}
          >
            <Play />
            Run
          </Button>
        </div>
      </div>
      <div className="flex min-h-0 flex-1 flex-col gap-2 p-3">
        <div className="h-56 shrink-0">
          <SqlEditor
            value={draft.query}
            onChange={(text) => updateDraft((previous) => ({ ...previous, query: text }))}
            onRun={execute}
            errorRange={errorRange}
            schema={schema.data}
            invalid={failedHere}
            describedBy={failedHere ? ERROR_ID : undefined}
          />
        </div>
        {failure ? (
          <p id={ERROR_ID} role="alert" className="text-destructive text-xs">
            {failure}
          </p>
        ) : null}
        <div className="flex min-h-0 flex-1 flex-col gap-1">
          {run.isPending ? (
            <p role="status" className="text-muted-foreground text-sm">
              Running…
            </p>
          ) : lastRun && !run.isError ? (
            <>
              <p role="status" className="text-muted-foreground text-xs">
                {resultStatus(
                  lastRun.result.rows.length,
                  lastRun.result.truncated,
                  lastRun.durationMs,
                )}
              </p>
              <div className="flex min-h-0 flex-1 flex-col">
                <SqlResultGrid result={lastRun.result} />
              </div>
            </>
          ) : run.isIdle ? (
            <p className="text-muted-foreground text-sm">Run a query to see results.</p>
          ) : null}
        </div>
      </div>
    </div>
  );
}
