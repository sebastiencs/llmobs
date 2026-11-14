import { useQuery } from "@tanstack/react-query";
import { PanelLeft, Play } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { $api } from "@/api/client";
import { problemOf } from "@/api/problem";
import { savedQueriesQuery, schemaQuery } from "@/api/queries";
import type { Schema } from "@/api/types";
import { useProject } from "@/components/shell/project-context";
import { ExamplesMenu } from "@/components/sql/examples-menu";
import { ExportMenu } from "@/components/sql/export-menu";
import { HistoryMenu } from "@/components/sql/history-menu";
import { ParamsBar } from "@/components/sql/params-bar";
import { SqlResultChart } from "@/components/sql/result-chart";
import { SqlResultGrid } from "@/components/sql/result-grid";
import { SavedQueriesMenu } from "@/components/sql/saved-queries-menu";
import { SchemaBrowser } from "@/components/sql/schema-browser";
import { SqlEditor } from "@/components/sql/sql-editor";
import { TimeRangePicker } from "@/components/time-range/time-range-picker";
import { Button } from "@/components/ui/button";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { TooltipProvider } from "@/components/ui/tooltip";
import { atLeast } from "@/lib/roles";
import { type SqlSearch, withoutView, withSqlRange } from "@/lib/search/sql";
import { useSqlDraft } from "@/lib/sql/draft";
import { downloadSqlExport, type ExportFormat } from "@/lib/sql/export";
import {
  isSqlResult,
  RUN_FAILED,
  resultStatus,
  runShortcutLabel,
  sqlProblemMessage,
} from "@/lib/sql/format";
import { pushHistory } from "@/lib/sql/history";
import { paramValues, sqlParams, usedParams } from "@/lib/sql/params";
import { sqlErrorRange } from "@/lib/sql/position";
import { sqlRunBody } from "@/lib/sql/request";
import { parseSavedState, savedStateOf, searchOfSavedState } from "@/lib/sql/saved-state";
import { readSchemaPanelOpen, writeSchemaPanelOpen } from "@/lib/sql/schema-panel";
import { rangeFromSearch, resolveRange } from "@/lib/time-range";

const ERROR_ID = "sql-error";

type LastRun = { result: Schema<"SqlQueryResult">; durationMs: number };

export function SqlConsole({
  search,
  onSearch,
}: {
  search: SqlSearch;
  /**
   * Changes the URL search. Always given the function form, like `navigate({ search })`.
   * `replace` replaces the history entry instead of pushing one.
   */
  onSearch: (update: (previous: SqlSearch) => SqlSearch, options?: { replace?: boolean }) => void;
}) {
  const { org, project, ref, data } = useProject();
  const schema = useQuery(schemaQuery(org, project));
  const [draft, updateDraft] = useSqlDraft(ref);
  const run = $api.useMutation("post", "/api/v1/projects/{project}/sql", { gcTime: 0 });
  const [lastRun, setLastRun] = useState<LastRun | undefined>();
  /** The last run "succeeded" without a result (an empty error body). */
  const [badAnswer, setBadAnswer] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [exportError, setExportError] = useState<string | undefined>();
  const [schemaOpen, setSchemaOpen] = useState(readSchemaPanelOpen);
  const [insert, setInsert] = useState<{ text: string; seq: number } | undefined>();
  const [tab, setTab] = useState<"table" | "chart">("table");
  const busy = run.isPending || exporting;
  const range = rangeFromSearch(search);
  const params = useMemo(() => sqlParams(draft.query), [draft.query]);
  const [paramErrors, setParamErrors] = useState<Record<string, string>>({});
  const savedQueries = useQuery(savedQueriesQuery(org, project));
  const listed = savedQueries.data?.data;
  const complete = savedQueries.isSuccess && savedQueries.data.next_cursor === null;
  /** The draft's saved-query id, read by the `?view=` effect without re-running it. */
  const draftView = useRef(draft.view);
  draftView.current = draft.view;
  /** The `?view=` id the effect below has already handled. */
  const handledView = useRef<string | undefined>(undefined);

  function load(view: Schema<"SavedView">) {
    const state = parseSavedState(view.state);
    draftView.current = view.id;
    setParamErrors({});
    updateDraft(() => ({ query: state.query, params: state.params, view: view.id }));
    onSearch((previous) => searchOfSavedState(state, previous, view.id), { replace: true });
  }

  function forgetView() {
    draftView.current = undefined;
    updateDraft((previous) => ({ query: previous.query, params: previous.params }));
    onSearch(withoutView, { replace: true });
  }

  // biome-ignore lint/correctness/useExhaustiveDependencies: load and forgetView are recreated every render; draftView and handledView are refs
  useEffect(() => {
    const id = search.view;
    if (!id) {
      handledView.current = undefined;
      return;
    }
    if (!savedQueries.isSuccess) {
      return;
    }
    const view = listed?.find((item) => item.id === id);
    if (!view) {
      handledView.current = undefined;
      if (complete) {
        forgetView();
      }
      return;
    }
    // A list refetch that lands before the URL catches up (Save as) must not reload the old view.
    if (handledView.current === id) {
      return;
    }
    handledView.current = id;
    if (draftView.current !== id) {
      load(view);
    }
  }, [search.view, savedQueries.isSuccess, listed, complete]);

  /** The request for `text`, or undefined when a parameter is invalid (errors are shown). */
  function buildBody(text: string): Schema<"SqlQueryRequest"> | undefined {
    const typed = paramValues(sqlParams(text), draft.params);
    if (Object.keys(typed.errors).length > 0) {
      setParamErrors(typed.errors);
      return undefined;
    }
    setParamErrors({});
    const span = resolveRange(range, Date.now());
    return sqlRunBody({ query: text, from: span.from, to: span.to, params: typed.values });
  }

  function execute(text: string) {
    if (text.trim().length === 0 || busy) {
      return;
    }
    setExportError(undefined);
    const body = buildBody(text);
    if (!body) {
      return;
    }
    const used = usedParams(text, draft.params);
    const started = performance.now();
    setLastRun(undefined);
    setBadAnswer(false);
    run.mutate(
      { params: { path: { project: ref } }, body, headers: { Accept: "application/json" } },
      {
        onSuccess: (data) => {
          const ranAt = new Date().toISOString();
          if (!isSqlResult(data)) {
            setBadAnswer(true);
            pushHistory(ref, { query: text, params: used, ranAt });
            return;
          }
          setLastRun({ result: data, durationMs: Math.round(performance.now() - started) });
          pushHistory(ref, { query: text, params: used, ranAt, rows: data.rows.length });
        },
        onError: () => {
          pushHistory(ref, { query: text, params: used, ranAt: new Date().toISOString() });
        },
      },
    );
  }

  /** Sends the same body as Run with the format's `Accept` and saves the file. */
  async function exportAs(format: ExportFormat) {
    if (draft.query.trim().length === 0 || busy) {
      return;
    }
    const body = buildBody(draft.query);
    if (!body) {
      return;
    }
    setExporting(true);
    setExportError(undefined);
    const outcome = await downloadSqlExport({ projectRef: ref, body, format });
    setExporting(false);
    if (!outcome.ok) {
      setExportError(outcome.message);
    }
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
    <TooltipProvider>
      <div className="flex h-full min-h-0 flex-1 flex-col">
        <div className="flex min-h-9 shrink-0 items-center gap-2 overflow-x-auto border-b px-3">
          <h1 className="shrink-0 text-sm font-medium">SQL</h1>
          <Button
            type="button"
            variant="ghost"
            size="icon-xs"
            aria-label="Schema"
            aria-pressed={schemaOpen}
            onClick={() => {
              const next = !schemaOpen;
              setSchemaOpen(next);
              writeSchemaPanelOpen(next);
            }}
          >
            <PanelLeft />
          </Button>
          <TimeRangePicker
            state={range}
            onChange={(next) => onSearch((previous) => withSqlRange(previous, next))}
          />
          <SavedQueriesMenu
            viewId={search.view}
            current={savedStateOf(draft, search)}
            onLoad={load}
            onSaved={(view) => {
              draftView.current = view.id;
              updateDraft((previous) => ({ ...previous, view: view.id }));
              onSearch((previous) => ({ ...previous, view: view.id }), { replace: true });
            }}
            onCleared={forgetView}
          />
          <ExamplesMenu
            onPick={(example) => {
              setParamErrors({});
              updateDraft((previous) => ({ ...previous, query: example.query, params: {} }));
            }}
          />
          <HistoryMenu
            projectRef={ref}
            onPick={(entry) => {
              setParamErrors({});
              updateDraft((previous) => ({
                ...previous,
                query: entry.query,
                params: entry.params,
              }));
            }}
          />
          <ExportMenu
            canExport={atLeast(data.role, "member")}
            disabled={draft.query.trim().length === 0 || busy}
            onExport={(format) => void exportAs(format)}
          />
          <div className="ml-auto flex shrink-0 items-center gap-2">
            <Button
              type="button"
              size="xs"
              title={`Run (${runShortcutLabel(navigator.userAgent)})`}
              disabled={draft.query.trim().length === 0 || busy}
              onClick={() => execute(draft.query)}
            >
              <Play />
              Run
            </Button>
          </div>
        </div>
        {savedQueries.isError ? (
          <p role="alert" className="px-3 py-1 text-destructive text-xs">
            Saved queries could not be loaded.
          </p>
        ) : null}
        <div className="flex min-h-0 flex-1">
          {schemaOpen ? (
            <SchemaBrowser
              schema={schema.data}
              failed={schema.isError}
              onInsert={(text) =>
                setInsert((previous) => ({ text, seq: (previous?.seq ?? 0) + 1 }))
              }
            />
          ) : null}
          <div className="flex min-h-0 flex-1 flex-col gap-2 p-3">
            <ParamsBar
              params={params}
              values={draft.params}
              errors={paramErrors}
              onChange={(name, value) => {
                updateDraft((previous) => ({
                  ...previous,
                  params: { ...previous.params, [name]: value },
                }));
                setParamErrors((previous) => {
                  const { [name]: _cleared, ...rest } = previous;
                  return rest;
                });
              }}
            />
            <div className="h-56 shrink-0">
              <SqlEditor
                value={draft.query}
                onChange={(text) => {
                  updateDraft((previous) => ({ ...previous, query: text }));
                  const names = new Set(sqlParams(text).map((param) => param.name));
                  setParamErrors((previous) =>
                    Object.fromEntries(
                      Object.entries(previous).filter(([name]) => names.has(name)),
                    ),
                  );
                }}
                onRun={execute}
                errorRange={errorRange}
                schema={schema.data}
                invalid={failedHere}
                describedBy={failedHere ? ERROR_ID : undefined}
                insert={insert}
              />
            </div>
            {failure ? (
              <p id={ERROR_ID} role="alert" className="text-destructive text-xs">
                {failure}
              </p>
            ) : null}
            {exportError ? (
              <p role="alert" className="text-destructive text-xs">
                {exportError}
              </p>
            ) : null}
            {exporting ? (
              <p role="status" className="text-muted-foreground text-xs">
                Exporting…
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
                  <Tabs
                    value={tab}
                    onValueChange={(value) => setTab(value === "chart" ? "chart" : "table")}
                    className="flex min-h-0 flex-1 flex-col"
                  >
                    <TabsList>
                      <TabsTrigger value="table">Table</TabsTrigger>
                      <TabsTrigger value="chart">Chart</TabsTrigger>
                    </TabsList>
                    <TabsContent value="table" className="flex min-h-0 flex-1 flex-col">
                      <SqlResultGrid result={lastRun.result} />
                    </TabsContent>
                    <TabsContent value="chart" className="flex min-h-0 flex-1 flex-col">
                      <SqlResultChart result={lastRun.result} />
                    </TabsContent>
                  </Tabs>
                </>
              ) : run.isIdle ? (
                <p className="text-muted-foreground text-sm">Run a query to see results.</p>
              ) : null}
            </div>
          </div>
        </div>
      </div>
    </TooltipProvider>
  );
}
