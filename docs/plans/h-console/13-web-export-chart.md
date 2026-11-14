# H13 — Exports, chart tab and schema browser

Commit subject: `web: add console exports, chart and schema browser`.

See [README.md](README.md) for the scope, the decisions (D-H-5, D-H-8, D-H-13) and the conventions. H11 and H12 are done. Read their files first; this phase extends them. The names this phase relies on:

| Name | Where (phase) | What |
|---|---|---|
| `SqlConsole` | `web/src/components/sql/sql-console.tsx` (H11, H12) | the page component, wrapped in `TooltipProvider` (H12) |
| `schema` | same (H11) | `useQuery(schemaQuery(org, project))`; the response is `schema.data`, failure `schema.isError` |
| `draft`, `updateDraft` | same (H11, H12) | `useSqlDraft(ref)`; `SqlDraft = { query, params, view? }`; `updateDraft((previous) => next)` writes through to localStorage |
| `run`, `execute(text)`, `lastRun`, `badAnswer`, `range` | same (H11, H12) | the run mutation, the run function, `{ result, durationMs }`, the empty-answer flag, `rangeFromSearch(search)` |
| `buildBody(text)` | same (H12) | the request body (`sqlRunBody`, typed params, the resolved range), or `undefined` after showing parameter errors |
| `sqlRunBody` | `web/src/lib/sql/request.ts` (H11, H12) | omits `params` when the query has none |
| `isNumericType(type: string): boolean` | `web/src/lib/sql/types.ts` (H11) | Arrow numeric types |
| `sqlProblemMessage(problem)`, `isSqlResult` | `web/src/lib/sql/format.ts` (H11) | run messages, JSON result guard |
| `SqlEditor` props | `web/src/components/sql/sql-editor.tsx` (H11) | `value, onChange, onRun, errorRange, schema, invalid, describedBy?` |
| `single`, `editorContent`, `storeDraft(query, ref?)`, `captureSql(answer?)`, `PROBLEM_JSON` | `web/src/routes/$org/$project/sql/index.test.tsx` (H11) | test helpers |
| `asRole(role)`, `storeFullDraft(draft)` | same (H12) | serve the project with another role; store a draft with params |
| `Harness` | `web/src/components/sql/sql-editor.test.tsx` (H11) | editor test harness (`initial`, `schema`, `errorRange` props) |

If any of these is missing or has another signature, STOP and report: H11 or H12 deviated.

H13 adds:
- an **Export** menu (CSV, NDJSON, Parquet) that sends the same body as Run with an `Accept` header and saves the answer as a file;
- a **Table / Chart** tab pair over the result, the chart picked automatically (spec 11 § SQL console: x is the first time or text column; a line chart with exactly one numeric column is split by a second text column; at most 5 series and 1,000 points);
- a **schema browser** on the left of the console that lists the views, their columns and the SQL functions, and inserts a name into the editor;
- the roadmap status of track H.

## Read first

- The H11 and H12 files named above, and `web/src/mocks/sql.ts`.
- `web/src/components/charts/metric-chart.tsx` (how charts use `ChartContainer` and `ComposedChart`), `web/src/components/ui/chart.tsx` (`ChartContainer`, `ChartTooltip`, `ChartTooltipContent`), `web/src/components/ui/tabs.tsx`, `web/src/mocks/handlers.ts` (copying bytes into a fresh `ArrayBuffer`).
- `web/src/api/client.ts` (`api`, an `openapi-fetch` client with the 401 middleware; its `fetch` reads `globalThis.fetch` at call time), `web/src/api/problem.ts`, `web/src/lib/roles.ts`, `web/src/lib/settings.ts` (`useTimeZone`), `web/src/lib/format.ts` (`formatTimestamp`).
- README § Design → SQL console endpoint (format negotiation, `Content-Disposition`, mid-stream failure).

## Facts this phase relies on

- **Format (D-H-8).** The server reads `Accept` and picks the first listed media range that is one of `text/csv`, `application/x-ndjson`, `application/vnd.apache.parquet`, `application/json`; anything else is JSON. An export answers `200` with that content type, `Content-Disposition: attachment; filename="query-<yyyymmddThhmmssZ>.<csv|ndjson|parquet>"`, streamed, and ignores `limit`. Errors before the first byte are problems, as for Run. A failure after the first byte cuts the body, so reading it rejects.
- **Who may export (D-H-5).** Sessions and PATs need project role `member`; a viewer gets `403 forbidden`. The UI keeps the Export trigger enabled (so focus returns to it when the menu closes) and disables the menu items for a viewer, with the reason "Exports need the member role" as a second line in each item.
- **Blob download (D-H-13).** The browser holds the whole file in memory; the CLI is the tool for very large exports. The download is an `<a download>` click on an object URL, revoked on the next task.
- **`openapi-fetch` 0.17** accepts `parseAs: "blob"` and per-request `headers`. On a non-2xx answer it returns the parsed problem as `error`. On a `2xx` answer with `Content-Length: 0` it returns `data: undefined`; a streamed export has no `Content-Length`, but the code treats `undefined` as an empty file anyway. Reading a body that errors mid-stream rejects the `api.POST` promise.
- **What MSW can and cannot simulate.** A `ReadableStream` that errors inside an MSW response makes `fetch` itself reject, which is the same as a network error. A body that fails *after* `fetch` resolved can only be produced by replacing `fetch`: the unit test in `export.test.ts` does that with `vi.spyOn(globalThis, "fetch")`. That is a spy on a global function, not `vi.mock` of a module, and it is the only such spy in this phase.
- **Charts in jsdom.** Recharts' `ResponsiveContainer` draws nothing in jsdom. Chart tests assert the `section`'s accessible name, built from the chart spec (the dashboard tests do the same). `ChartContainer` turns its `config` keys into CSS; the chart passes `config={{}}` and sets colours on each `Line`/`Bar`, so a series named after a data value cannot inject CSS.
- **Arrow types.** Times are `Timestamp(…)` (JSON: RFC 3339 strings) and `Date32` / `Date64` (JSON: `YYYY-MM-DD` strings); text is `Utf8`, `LargeUtf8`, `Utf8View`.
- **Object URLs in tests.** Whether jsdom's `URL` has `createObjectURL` depends on the environment; tests replace `URL.createObjectURL` and `URL.revokeObjectURL`, and restore the originals afterwards (deleting a property that was absent). They spy on `HTMLAnchorElement.prototype.click` with `vi.spyOn` so no navigation happens.
- `tsconfig.app.json` sets `noUncheckedIndexedAccess`. Read list items through `single(...)` or a checked local.
- Use `user.click` / `user.keyboard` for Radix menus and tabs, never `fireEvent`.

## Edit only

- `web/src/lib/sql/types.ts` and `web/src/lib/sql/types.test.ts` (add three predicates)
- `web/src/lib/sql/export.ts` (new) and `web/src/lib/sql/export.test.ts` (new)
- `web/src/lib/sql/chart.ts` (new) and `web/src/lib/sql/chart.test.ts` (new)
- `web/src/lib/sql/schema-panel.ts` (new) and `web/src/lib/sql/schema-panel.test.ts` (new)
- `web/src/components/sql/export-menu.tsx` (new)
- `web/src/components/sql/result-chart.tsx` (new)
- `web/src/components/sql/schema-browser.tsx` (new)
- `web/src/components/sql/sql-editor.tsx` and `web/src/components/sql/sql-editor.test.tsx` (the `insert` prop)
- `web/src/components/sql/sql-console.tsx`
- `web/src/mocks/sql.ts` and `web/src/mocks/sql.test.ts`
- `web/src/routes/$org/$project/sql/index.test.tsx` (append tests)
- `docs/roadmap.md` (row H, Status cell only)

## Steps

### 1. Type predicates — `web/src/lib/sql/types.ts`

Append:

```ts
export function isTimestampType(type: string): boolean {
  return type.startsWith("Timestamp(");
}

export function isDateType(type: string): boolean {
  return type === "Date32" || type === "Date64";
}

export function isTextType(type: string): boolean {
  return type === "Utf8" || type === "LargeUtf8" || type === "Utf8View";
}
```

### 2. Export helpers — `web/src/lib/sql/export.ts`

```ts
import { api } from "@/api/client";
import { type Problem, problemOf } from "@/api/problem";
import type { Schema } from "@/api/types";
import { sqlProblemMessage } from "@/lib/sql/format";

export type ExportFormat = "csv" | "ndjson" | "parquet";

export const EXPORT_FORMATS: Record<ExportFormat, { label: string; accept: string }> = {
  csv: { label: "CSV", accept: "text/csv" },
  ndjson: { label: "NDJSON", accept: "application/x-ndjson" },
  parquet: { label: "Parquet", accept: "application/vnd.apache.parquet" },
};

export const EXPORT_ORDER: readonly ExportFormat[] = ["csv", "ndjson", "parquet"];

export const EXPORT_ROLE_REASON = "Exports need the member role";
export const EXPORT_CUT = "The export failed before it finished";
export const EXPORT_NOT_SAVED = "The file could not be saved";

/** `query-20260101T093005Z.csv` for `now`, in UTC. The extension is the format id. */
export function defaultExportName(format: ExportFormat, now: Date): string {
  const stamp = now
    .toISOString()
    .replace(/[-:]/g, "")
    .replace(/\.\d{3}Z$/, "Z");
  return `query-${stamp}.${format}`;
}

/** The `filename="…"` of a `Content-Disposition`, else `fallback`. Only `[A-Za-z0-9._-]+`. */
export function exportName(disposition: string | null, fallback: string): string {
  const match = disposition ? /filename="([A-Za-z0-9._-]+)"/.exec(disposition) : null;
  const name = match?.[1];
  return name && !name.startsWith(".") ? name : fallback;
}

/** Saves `blob` as `name` through a temporary object URL, revoked on the next task. */
export function saveBlob(blob: Blob, name: string): void {
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.rel = "noopener";
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 0);
}

export function exportProblemMessage(problem: Problem | undefined): string {
  if (!problem) {
    return "The export could not be downloaded";
  }
  return problem.code === "forbidden" ? EXPORT_ROLE_REASON : sqlProblemMessage(problem);
}

export type ExportOutcome = { ok: true; name: string } | { ok: false; message: string };

/** Runs the export and saves the file. Never throws. */
export async function downloadSqlExport(input: {
  projectRef: string;
  body: Schema<"SqlQueryRequest">;
  format: ExportFormat;
}): Promise<ExportOutcome> {
  let blob: Blob;
  let disposition: string | null;
  try {
    const { data, error, response } = await api.POST("/api/v1/projects/{project}/sql", {
      params: { path: { project: input.projectRef } },
      body: input.body,
      headers: { Accept: EXPORT_FORMATS[input.format].accept },
      parseAs: "blob",
    });
    if (error !== undefined || !response.ok) {
      return { ok: false, message: exportProblemMessage(problemOf(error)) };
    }
    blob = data ?? new Blob([]);
    disposition = response.headers.get("Content-Disposition");
  } catch {
    return { ok: false, message: EXPORT_CUT };
  }
  const name = exportName(disposition, defaultExportName(input.format, new Date()));
  try {
    saveBlob(blob, name);
  } catch {
    return { ok: false, message: EXPORT_NOT_SAVED };
  }
  return { ok: true, name };
}
```

If `tsc` rejects `parseAs` or `headers` on this operation, report the exact error and STOP: do not switch to a raw `fetch` (the `api` client carries the 401 middleware). If `tsc` types `data` as something other than `Blob | undefined`, add the narrowest cast that compiles and say so in the report.

### 3. Mock — `web/src/mocks/sql.ts`

Add these, and use them in H11's handler after the empty-query check:

```ts
export type SqlFormat = "csv" | "ndjson" | "parquet" | "json";

const MEDIA: readonly [string, SqlFormat][] = [
  ["text/csv", "csv"],
  ["application/x-ndjson", "ndjson"],
  ["application/vnd.apache.parquet", "parquet"],
  ["application/json", "json"],
];

/** The server's rule (D-H-8): the first listed media range that is one of the four. */
export function negotiate(accept: string): SqlFormat {
  for (const part of accept.split(",")) {
    const media = part.split(";")[0]?.trim().toLowerCase() ?? "";
    const hit = MEDIA.find(([type]) => type === media);
    if (hit) {
      return hit[1];
    }
  }
  return "json";
}

/** RFC 4180 with CRLF; a field with `,`, `"`, CR or LF is quoted; NULL is empty. */
export function csvOf(result: {
  columns: { name: string }[];
  rows: Record<string, unknown>[];
}): string {
  const field = (value: unknown): string => {
    if (value === null || value === undefined) {
      return "";
    }
    const text =
      typeof value === "string"
        ? value
        : typeof value === "object"
          ? JSON.stringify(value)
          : String(value);
    return /[",\r\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
  };
  const lines = [result.columns.map((column) => field(column.name)).join(",")];
  for (const row of result.rows) {
    lines.push(result.columns.map((column) => field(row[column.name])).join(","));
  }
  return `${lines.join("\r\n")}\r\n`;
}

const STAMP = "query-20260101T100000Z";

function attachment(extension: string): string {
  return `attachment; filename="${STAMP}.${extension}"`;
}

/** A fresh `ArrayBuffer`, as `handlers.ts` does for binary fixtures. */
function parquetBytes(): ArrayBuffer {
  const bytes = new TextEncoder().encode("PAR1PAR1");
  const buffer = new ArrayBuffer(bytes.byteLength);
  new Uint8Array(buffer).set(bytes);
  return buffer;
}
```

In the handler, replace `return HttpResponse.json(resultDocument);` with:

```ts
const rows: Record<string, unknown>[] = resultDocument.rows;
const format = negotiate(request.headers.get("Accept") ?? "");
if (format === "csv") {
  return new HttpResponse(csvOf({ columns: resultDocument.columns, rows }), {
    headers: { "Content-Type": "text/csv", "Content-Disposition": attachment("csv") },
  });
}
if (format === "ndjson") {
  const text = rows.map((row) => `${JSON.stringify(row)}\n`).join("");
  return new HttpResponse(text, {
    headers: {
      "Content-Type": "application/x-ndjson",
      "Content-Disposition": attachment("ndjson"),
    },
  });
}
if (format === "parquet") {
  return HttpResponse.arrayBuffer(parquetBytes(), {
    headers: {
      "Content-Type": "application/vnd.apache.parquet",
      "Content-Disposition": attachment("parquet"),
    },
  });
}
return HttpResponse.json(resultDocument);
```

If `tsc` refuses the `rows` annotation, write `resultDocument.rows as Record<string, unknown>[]`.

### 4. Export menu — `web/src/components/sql/export-menu.tsx`

```tsx
import { Download } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  EXPORT_FORMATS,
  EXPORT_ORDER,
  EXPORT_ROLE_REASON,
  type ExportFormat,
} from "@/lib/sql/export";

export function ExportMenu({
  canExport,
  disabled,
  onExport,
}: {
  /** Role member or above (D-H-5). */
  canExport: boolean;
  /** No query, or a run or an export in flight. */
  disabled: boolean;
  onExport: (format: ExportFormat) => void;
}) {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button type="button" variant="outline" size="xs">
          <Download />
          Export
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end">
        {EXPORT_ORDER.map((format) => (
          <DropdownMenuItem
            key={format}
            disabled={disabled || !canExport}
            onSelect={() => onExport(format)}
          >
            <span className="flex flex-col">
              {EXPORT_FORMATS[format].label}
              {canExport ? null : (
                <span className="text-muted-foreground text-xs">{EXPORT_ROLE_REASON}</span>
              )}
            </span>
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
```

The trigger stays enabled so Radix returns focus to it when the menu closes; the items carry the disabled state.

### 5. Chart spec — `web/src/lib/sql/chart.ts`

```ts
import type { Schema } from "@/api/types";
import { isDateType, isNumericType, isTextType, isTimestampType } from "@/lib/sql/types";

export const CHART_MAX_POINTS = 1000;
export const CHART_MAX_SERIES = 5;
/** Key of the x value in each point; series keys are column names or pivot values. */
export const X_KEY = "__x";

export type ChartPoint = Record<string, string | number | null>;

export type ChartSpec = {
  kind: "line" | "bar";
  /** The x column and how its values read. */
  x: { name: string; kind: "timestamp" | "date" | "text" };
  /** Series keys, in order. */
  series: string[];
  /** Set when the series are the values of a text column and `measure` is the one numeric column. */
  pivot?: { column: string; measure: string };
  data: ChartPoint[];
  /** True when rows or series were dropped. */
  clipped: boolean;
};

function numberOf(value: unknown): number | null {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value;
  }
  if (typeof value === "string" && value.trim().length > 0 && Number.isFinite(Number(value))) {
    return Number(value);
  }
  return null;
}

/** Milliseconds for sorting; unparseable values sort last. */
function timeOf(point: ChartPoint): number {
  const time = Date.parse(String(point[X_KEY]));
  return Number.isNaN(time) ? Number.POSITIVE_INFINITY : time;
}

function byTime(left: ChartPoint, right: ChartPoint): number {
  const a = timeOf(left);
  const b = timeOf(right);
  return a === b ? 0 : a < b ? -1 : 1;
}

/** Keys that would not round-trip as plain data properties. */
const UNSAFE_KEYS = new Set(["__proto__", X_KEY]);

/**
 * x: the first timestamp or date column (line chart), else the first text column (bar chart).
 * y: the numeric columns other than x, at most 5.
 * Pivot (spec 11): a line chart with exactly one numeric column and another text column gets
 * one series per value of the first such column, at most 5, first seen first.
 * Points: the first 1,000 rows; a line chart is sorted by time.
 * No x or no numeric column: null.
 */
export function chartSpec(result: Schema<"SqlQueryResult">): ChartSpec | null {
  const columns = result.columns;
  const time = columns.find((column) => isTimestampType(column.type) || isDateType(column.type));
  const text = columns.find((column) => isTextType(column.type));
  const xColumn = time ?? text;
  if (!xColumn) {
    return null;
  }
  const numeric = columns.filter(
    (column) =>
      column.name !== xColumn.name && isNumericType(column.type) && !UNSAFE_KEYS.has(column.name),
  );
  const measure = numeric[0];
  if (!measure) {
    return null;
  }
  const xKind: ChartSpec["x"]["kind"] = time
    ? isDateType(xColumn.type)
      ? "date"
      : "timestamp"
    : "text";
  const x = { name: xColumn.name, kind: xKind };
  const kind = time ? "line" : "bar";
  const rows = (result.rows as Record<string, unknown>[]).slice(0, CHART_MAX_POINTS);
  let clipped = result.rows.length > CHART_MAX_POINTS;
  const pivotColumn =
    kind === "line" && numeric.length === 1
      ? columns.find((column) => column.name !== xColumn.name && isTextType(column.type))
      : undefined;
  if (pivotColumn) {
    const series: string[] = [];
    const byX = new Map<string, ChartPoint>();
    for (const row of rows) {
      const xValue = String(row[xColumn.name] ?? "");
      const raw = row[pivotColumn.name];
      const key = raw === null || raw === undefined ? "NULL" : String(raw);
      if (UNSAFE_KEYS.has(key)) {
        clipped = true;
        continue;
      }
      if (!series.includes(key)) {
        if (series.length >= CHART_MAX_SERIES) {
          clipped = true;
          continue;
        }
        series.push(key);
      }
      const point: ChartPoint = byX.get(xValue) ?? { [X_KEY]: xValue };
      point[key] = numberOf(row[measure.name]);
      byX.set(xValue, point);
    }
    const data = [...byX.values()].sort(byTime);
    const pivot = { column: pivotColumn.name, measure: measure.name };
    return { kind, x, series, pivot, data, clipped };
  }
  if (numeric.length > CHART_MAX_SERIES) {
    clipped = true;
  }
  const series = numeric.slice(0, CHART_MAX_SERIES).map((column) => column.name);
  const data = rows.map((row) => {
    const point: ChartPoint = { [X_KEY]: String(row[xColumn.name] ?? "") };
    for (const name of series) {
      point[name] = numberOf(row[name]);
    }
    return point;
  });
  if (kind === "line") {
    data.sort(byTime);
  }
  return { kind, x, series, data, clipped };
}

/** The chart's accessible name. */
export function chartLabel(spec: ChartSpec): string {
  const kind = spec.kind === "line" ? "Line chart" : "Bar chart";
  if (spec.pivot) {
    return `${kind} of ${spec.pivot.measure} by ${spec.x.name}, one series per ${spec.pivot.column}`;
  }
  return `${kind} of ${spec.series.join(", ")} by ${spec.x.name}`;
}
```

Documented choices: a `null` pivot value is the series `NULL` (it merges with a literal `"NULL"` text value); a pivot value or numeric column named `__proto__` or `__x` is left out and marks the chart clipped. A column named `x` is an ordinary series.

### 6. Chart — `web/src/components/sql/result-chart.tsx`

```tsx
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
```

`useTimeZone` returns `"local" | "utc"`, the type `formatTimestamp` takes. A `date` axis shows the `YYYY-MM-DD` strings as they are. Both hooks come before the early return. If `ChartTooltipContent`'s `labelFormatter` has another signature in `ui/chart.tsx`, adapt the call to it.

### 7. Schema panel state — `web/src/lib/sql/schema-panel.ts`

```ts
import { STORAGE_PREFIX } from "@/product";

export const SCHEMA_PANEL_KEY = `${STORAGE_PREFIX}sql.schema`;

/** Open unless the user closed it. */
export function readSchemaPanelOpen(): boolean {
  try {
    return localStorage.getItem(SCHEMA_PANEL_KEY) !== "closed";
  } catch {
    return true;
  }
}

export function writeSchemaPanelOpen(open: boolean): void {
  try {
    localStorage.setItem(SCHEMA_PANEL_KEY, open ? "open" : "closed");
  } catch {
    // Private mode: the choice still applies for this page.
  }
}
```

### 8. Editor insertion — `web/src/components/sql/sql-editor.tsx`

1. Add one optional prop:
   ```ts
     /** Each `seq` greater than the last one inserts `text` and focuses the editor. */
     insert?: { text: string; seq: number };
   ```
2. With the other `useRef` calls at the top: `const lastInsert = useRef(0);` and `const touched = useRef(false);`.
3. In the editor's extension list (first effect), add:
   ```ts
   EditorView.domEventHandlers({
     focus: () => {
       touched.current = true;
       return false;
     },
   }),
   ```
4. After the other effects:
   ```ts
   const insertSeq = insert?.seq;
   const insertText = insert?.text;
   useEffect(() => {
     const view = viewRef.current;
     if (!view || insertSeq === undefined || insertText === undefined) {
       return;
     }
     if (insertSeq <= lastInsert.current) {
       return;
     }
     lastInsert.current = insertSeq;
     // An editor never focused has its cursor at 0; append instead.
     const length = view.state.doc.length;
     const target = touched.current ? view.state.selection.main : { from: length, to: length };
     const before = view.state.sliceDoc(Math.max(0, target.from - 1), target.from);
     const text = before !== "" && !/[\s(,.]/.test(before) ? ` ${insertText}` : insertText;
     view.dispatch({
       changes: { from: target.from, to: target.to, insert: text },
       selection: { anchor: target.from + text.length },
     });
     view.focus();
   }, [insertSeq, insertText]);
   ```

The dispatch fires the update listener, so `onChange` reports the new text as for typing; H11's `echoes` handle it like any edit. A leading space is added when the character before the insertion point is not whitespace, `(`, `,` or `.`.

### 9. Schema browser — `web/src/components/sql/schema-browser.tsx`

```tsx
import { ChevronRight, Plus } from "lucide-react";
import { useState } from "react";
import type { Schema } from "@/api/types";
import { Button } from "@/components/ui/button";

function toggled(set: ReadonlySet<string>, key: string): ReadonlySet<string> {
  const next = new Set(set);
  if (next.has(key)) {
    next.delete(key);
  } else {
    next.add(key);
  }
  return next;
}

export function SchemaBrowser({
  schema,
  failed,
  onInsert,
}: {
  schema: Schema<"SchemaResponse"> | undefined;
  /** The schema request failed. */
  failed: boolean;
  onInsert: (text: string) => void;
}) {
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set(["traces"]));
  // render below
}
```

Render:
- Root: `<nav aria-label="Schema" className="flex w-56 shrink-0 flex-col gap-2 overflow-auto border-r p-2 text-xs">`.
- When `failed`: only `<p className="text-destructive">Schema unavailable</p>`. Else while `schema` is `undefined`: only `<p className="text-muted-foreground">Loading…</p>`.
- One `<div key={view.name}>` per view of `schema.views`, in order. `const open = expanded.has(view.name)`:
  - a header row `<div className="flex items-center gap-1">` with the toggle `<button type="button" aria-expanded={open} aria-controls={\`schema-${view.name}\`} title={view.description} className="flex flex-1 items-center gap-1 font-mono" onClick={() => setExpanded((previous) => toggled(previous, view.name))}><ChevronRight className={open ? "size-3 rotate-90" : "size-3"} />{view.name}</button>` and `<Button type="button" variant="ghost" size="icon-xs" aria-label={\`Insert ${view.name}\`} onClick={() => onInsert(view.name)}><Plus /></Button>`;
  - when open, `<ul id={\`schema-${view.name}\`} aria-label={\`${view.name} columns\`}>` with one `<li key={column.name}>` per column: `<button type="button" className="flex w-full justify-between gap-2 rounded px-1 hover:bg-muted" title={column.description} aria-label={\`Insert ${column.name}\`} onClick={() => onInsert(column.name)}><span className="font-mono">{column.name}</span><span className="text-muted-foreground">{column.column_type}</span></button>`.
- A last `<div key="functions">`, closed by default, with the same toggle (text `Functions`, `aria-controls="schema-functions"`, toggling the key `"functions"`); when open, `<ul id="schema-functions" aria-label="Functions">` lists `schema.functions.filter((fn) => fn.in_sql)`, each `<li key={fn.name}>` holding `<button type="button" aria-label={\`Insert ${fn.name}(\`} title={fn.signature} onClick={() => onInsert(\`${fn.name}(\`)}>` with the name in `font-mono`.

Each button's accessible name is "Insert " plus exactly the text it inserts (without the leading space step 8 may add). Column names repeat across views (`trace_id`), so tests look a column up inside its view's list.

### 10. Console — `web/src/components/sql/sql-console.tsx`

1. Imports to add: `PanelLeft` from `lucide-react`; `ExportMenu`; `SqlResultChart`; `SchemaBrowser`; `Tabs`, `TabsContent`, `TabsList`, `TabsTrigger` from `@/components/ui/tabs`; `atLeast` from `@/lib/roles`; `downloadSqlExport`, `type ExportFormat` from `@/lib/sql/export`; `readSchemaPanelOpen`, `writeSchemaPanelOpen` from `@/lib/sql/schema-panel`.
2. `useProject()` also destructures `data` (the project, for `data.role`).
3. New state, with the other hooks:
   ```ts
   const [exporting, setExporting] = useState(false);
   const [exportError, setExportError] = useState<string | undefined>();
   const [schemaOpen, setSchemaOpen] = useState(readSchemaPanelOpen);
   const [insert, setInsert] = useState<{ text: string; seq: number } | undefined>();
   const [tab, setTab] = useState<"table" | "chart">("table");
   const busy = run.isPending || exporting;
   ```
4. `execute(text)`: change its first guard to `if (text.trim().length === 0 || busy) { return; }` and add `setExportError(undefined);` right after the guard. Nothing else in `execute` changes.
5. Export, next to `execute`:
   ```ts
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
   ```
   A parameter error during an export shows in the parameters bar (through `buildBody`), and no request is sent.
6. Toolbar:
   - first element after the `h1`: `<Button type="button" variant="ghost" size="icon-xs" aria-label="Schema" aria-pressed={schemaOpen} onClick={() => { const next = !schemaOpen; setSchemaOpen(next); writeSchemaPanelOpen(next); }}><PanelLeft /></Button>`;
   - after H12's three menus: `<ExportMenu canExport={atLeast(data.role, "member")} disabled={draft.query.trim().length === 0 || busy} onExport={(format) => void exportAs(format)} />`;
   - the Run button's `disabled` becomes `draft.query.trim().length === 0 || busy`.
7. Under the run failure line: `{exportError ? <p role="alert" className="text-destructive text-xs">{exportError}</p> : null}` and `{exporting ? <p role="status" className="text-muted-foreground text-xs">Exporting…</p> : null}`.
8. Body: wrap the `p-3` column in a row, and pass `insert` to the editor:
   ```tsx
   <div className="flex min-h-0 flex-1">
     {schemaOpen ? (
       <SchemaBrowser
         schema={schema.data}
         failed={schema.isError}
         onInsert={(text) => setInsert((previous) => ({ text, seq: (previous?.seq ?? 0) + 1 }))}
       />
     ) : null}
     <div className="flex min-h-0 flex-1 flex-col gap-2 p-3">{/* H11/H12 content */}</div>
   </div>
   ```
   and `insert={insert}` on `SqlEditor`.
9. The success branch (`lastRun && !run.isError`) keeps the status line, then replaces H11's grid wrapper `<div className="flex min-h-0 flex-1 flex-col"><SqlResultGrid … /></div>` with a controlled tab pair, so a re-run stays on the tab the user picked:
   ```tsx
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
   ```

### 11. Roadmap — `docs/roadmap.md`

In the table row that starts with `| **H** Console and agents |`, replace the Status cell `**Planned.**` with `**Implemented.** H1–H13 are done; the SQL console page, saved queries, exports, MCP and the CLI query commands run on the real API.` Change nothing else in the file.

## States

| State | What shows |
|---|---|
| Viewer | Export opens; its items are disabled with "Exports need the member role" |
| Exporting | `role="status"` "Exporting…"; Run and the export items disabled |
| Running | Export items disabled |
| Export refused (problem) | `role="alert"` with the problem's message |
| Export cut mid-stream, or network error | `role="alert"` "The export failed before it finished" |
| File cannot be saved | `role="alert"` "The file could not be saved" |
| Result without a time/text or numeric column | Chart tab shows `NO_CHART` |
| More than 1,000 rows or 5 series | Chart shows `CLIPPED` above it |
| Re-run on the Chart tab | Stays on the Chart tab |
| Schema loading / failed | "Loading…" / "Schema unavailable" in the panel |
| Schema panel closed | Toggle `aria-pressed="false"`; `llmobs.sql.schema` is `closed` and is read on the next visit |

## Tests

Imports: add `vi`, `afterEach` to the page test's `vitest` import. New unit test files import from `vitest` what they use (`afterEach`, `describe`, `expect`, `it`, `vi`) and the module under test; `export.test.ts` also imports `{ HttpResponse, http } from "msw"`, `{ server } from "@/mocks/node"`, `type { Schema } from "@/api/types"`. The page test imports `NO_CHART`, `CLIPPED` from `@/components/sql/result-chart`, `EXPORT_ROLE_REASON` from `@/lib/sql/export`, and the schema fixture `import schemaFixture from "../../../../../../fixtures/api/static/schema.json";`.

Download helper, in the page test and in `export.test.ts`, with its own `afterEach` in each file:

```ts
let restoreDownloads: (() => void) | undefined;
afterEach(() => {
  restoreDownloads?.();
  restoreDownloads = undefined;
});

function captureDownloads() {
  const saved: { href: string; download: string; rel: string }[] = [];
  const revoked: string[] = [];
  const target = URL as unknown as Record<string, unknown>;
  const original = {
    createObjectURL: target.createObjectURL,
    revokeObjectURL: target.revokeObjectURL,
  };
  target.createObjectURL = () => "blob:test";
  target.revokeObjectURL = (url: string) => {
    revoked.push(url);
  };
  const click = vi
    .spyOn(HTMLAnchorElement.prototype, "click")
    .mockImplementation(function (this: HTMLAnchorElement) {
      saved.push({ href: this.getAttribute("href") ?? "", download: this.download, rel: this.rel });
    });
  restoreDownloads = () => {
    click.mockRestore();
    for (const [key, value] of Object.entries(original)) {
      if (value === undefined) {
        delete target[key];
      } else {
        target[key] = value;
      }
    }
  };
  return { saved, revoked };
}
```

Request watcher for the page test (the resolver returns nothing, so the default `POST /sql` mock still answers):

```ts
function watchSql() {
  const seen: { accept: string | null; body: Record<string, unknown> }[] = [];
  server.use(
    http.post("*/api/v1/projects/:project/sql", async ({ request }) => {
      const body = (await request.clone().json()) as Record<string, unknown>;
      seen.push({ accept: request.headers.get("Accept"), body });
    }),
  );
  return seen;
}

async function exportAs(user: { click: (element: Element) => Promise<void> }, label: string) {
  await user.click(await screen.findByRole("button", { name: "Export" }));
  await user.click(await screen.findByRole("menuitem", { name: label }));
}
```

A gate for "in flight" states: `let release = () => {}; const gate = new Promise<void>((resolve) => { release = resolve; });`.

### `web/src/lib/sql/types.test.ts` (extend)

1. `isTimestampType('Timestamp(Nanosecond, Some("UTC"))')` true, `isTimestampType("Date32")` false; `isDateType` true for `Date32` and `Date64`, false for `Utf8`; `isTextType` true for `Utf8`, `LargeUtf8`, `Utf8View`, false for `Int64`.

### `web/src/lib/sql/export.test.ts`

`const BODY: Schema<"SqlQueryRequest"> = { query: "SELECT 1", from: "2026-01-01T00:00:00Z", to: "2026-01-02T00:00:00Z" };`
1. `defaultExportName("csv", new Date("2026-01-01T09:30:05.123Z"))` is `"query-20260101T093005Z.csv"`; `"parquet"` and `"ndjson"` end in `.parquet` and `.ndjson`.
2. `exportName('attachment; filename="query-20260101T100000Z.csv"', "f")` is the file name; `exportName(null, "f")`, `exportName('attachment; filename="../x"', "f")` and `exportName('attachment; filename=".env"', "f")` are `"f"`.
3. `exportProblemMessage(undefined)` is `"The export could not be downloaded"`; a `forbidden` problem gives `EXPORT_ROLE_REASON`; an `invalid_filter` problem with `errors: [{ field: "query", message: "m" }]` gives `"m"`; a `query_timeout` problem with `detail: "d"` gives `"d"`.
4. `saveBlob(new Blob(["x"]), "a.csv")` with `captureDownloads()`: `saved` is `[{ href: "blob:test", download: "a.csv", rel: "noopener" }]`; `document.querySelector("a[download]")` is null afterwards; `revoked` is empty right after the call and `["blob:test"]` after `await new Promise((resolve) => setTimeout(resolve, 0))`.
5. `downloadSqlExport` (each case with `captureDownloads()`):
   - against the default mock with `format: "csv"`: `{ ok: true, name: "query-20260101T100000Z.csv" }`;
   - `server.use` answering `200` text `a\n` with `Content-Type: text/csv` and no `Content-Disposition`: the name matches `/^query-\d{8}T\d{6}Z\.csv$/`;
   - a `403` `forbidden` problem: `{ ok: false, message: EXPORT_ROLE_REASON }`;
   - `HttpResponse.error()`: `{ ok: false, message: EXPORT_CUT }`;
   - **a body cut after the headers**: `const fetchSpy = vi.spyOn(globalThis, "fetch").mockResolvedValue(new Response(new ReadableStream({ start(controller) { controller.enqueue(new TextEncoder().encode("a,b\r\n")); setTimeout(() => controller.error(new Error("cut")), 10); } }), { status: 200, headers: { "Content-Type": "text/csv" } }));` then `{ ok: false, message: EXPORT_CUT }` and nothing saved; `fetchSpy.mockRestore()` in `finally`;
   - `createObjectURL` throwing (replace it after `captureDownloads()` with `() => { throw new Error("no"); }`): `{ ok: false, message: EXPORT_NOT_SAVED }`.

### `web/src/mocks/sql.test.ts` (append)

1. `negotiate("text/csv")` is `"csv"`; `negotiate("application/json, text/csv")` is `"json"`; `negotiate("text/html;q=0.9, application/x-ndjson")` is `"ndjson"`; `negotiate("*/*")` and `negotiate("")` are `"json"`; `negotiate("Application/Vnd.Apache.Parquet")` is `"parquet"`.
2. `csvOf({ columns: [{ name: "a" }, { name: "b" }], rows: [{ a: "x,y", b: null }, { a: 'q"q', b: { k: 1 } }] })` is `'a,b\r\n"x,y",\r\n"q""q","{""k"":1}"\r\n'`.
3. POSTing `requestDocument` with `Accept: text/csv` answers `200`, `Content-Type` `text/csv`, `Content-Disposition` `attachment; filename="query-20260101T100000Z.csv"`, and a body whose first line is `resultDocument.columns.map((column) => column.name).join(",")`.
4. With `Accept: application/x-ndjson`: `Content-Type` `application/x-ndjson`; the body has `resultDocument.rows.length` non-empty lines, each parsing to the matching row.
5. With `Accept: application/vnd.apache.parquet`: `Content-Type` `application/vnd.apache.parquet`; `new TextDecoder().decode(await response.arrayBuffer())` is `"PAR1PAR1"`.

### `web/src/lib/sql/chart.test.ts`

Results are built inline (`{ columns, rows, truncated: false }`); `TS = 'Timestamp(Nanosecond, Some("UTC"))'`.
1. `day` (`TS`) + `cost` (`Float64`), rows out of time order → `kind: "line"`, `x: { name: "day", kind: "timestamp" }`, `series: ["cost"]`, data sorted by time with `__x` holding the strings; `chartLabel` `"Line chart of cost by day"`.
2. `day` (`TS`) + `model` (`Utf8`) + `cost` (`Float64`), rows `(d1, "a", 1)`, `(d1, "b", 2)`, `(d2, "a", 3)` → `series: ["a", "b"]`, `data: [{ __x: d1, a: 1, b: 2 }, { __x: d2, a: 3 }]`, label `"Line chart of cost by day, one series per model"`.
3. Two numeric columns and a text column with a timestamp x → no pivot: `series` are the two numeric names.
4. `name` (`Utf8`) + `traces` (`Int64`) + `cost` (`Float64`) → `kind: "bar"`, `series: ["traces", "cost"]`, label `"Bar chart of traces, cost by name"`.
5. Only `Utf8` columns, or only numeric columns → `null`.
6. Seven numeric columns → five series and `clipped: true`; 1,001 rows → 1,000 points and `clipped: true`; a pivot with six distinct values → five series and `clipped: true`.
7. A `Decimal128(38, 18)` value sent as the string `"0.25"` becomes `0.25`; `null` stays `null`.
8. `d` (`Date32`) + `n` (`Int64`) with `"2026-01-02"`, `"2026-01-01"` → `x.kind: "date"`, sorted ascending.
9. A timestamp value `"not a time"` sorts that point last, without throwing.
10. Odd names: a pivot value `a;}b{` is a series key as is; a numeric column named `x` is a series and `__x` still holds the x value; a pivot value `__proto__` is left out and `clipped` is true; a `null` pivot value is the series `"NULL"`.

### `web/src/lib/sql/schema-panel.test.ts`

1. `readSchemaPanelOpen()` is `true` with nothing stored; after `writeSchemaPanelOpen(false)`, `localStorage.getItem("llmobs.sql.schema")` is `"closed"` and `readSchemaPanelOpen()` is `false`.
2. With `vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => { throw new Error("denied"); })`, `readSchemaPanelOpen()` is `true`; with `setItem` throwing, `writeSchemaPanelOpen(false)` does not throw. Restore the spies in `finally`.

### `web/src/components/sql/sql-editor.test.tsx` (append)

Add an `InsertHarness`:

```tsx
function InsertHarness({ initial = "" }: { initial?: string }) {
  const [value, setValue] = useState(initial);
  const [insert, setInsert] = useState<{ text: string; seq: number }>();
  const [tick, setTick] = useState(0);
  return (
    <>
      <button type="button" onClick={() => setInsert({ text: "x", seq: (insert?.seq ?? 0) + 1 })}>
        Insert
      </button>
      <button type="button" onClick={() => setTick(tick + 1)}>
        Rerender
      </button>
      <span>{tick}</span>
      <SqlEditor
        value={value}
        onChange={setValue}
        onRun={() => {}}
        errorRange={null}
        schema={undefined}
        invalid={false}
        insert={insert}
      />
    </>
  );
}
```

1. **inserts once per seq, with a separating space**: render it; click "Insert": the editor text is `"x"`; click "Rerender" twice: still `"x"`; click "Insert": `"x x"`.
2. **appends when never focused, inserts at the cursor once focused**: `render(<InsertHarness initial="SELECT  FROM t" />)`; click "Insert" (the editor was never focused): the text is `"SELECT  FROM t x"`; focus `.cm-content`, `await user.keyboard("{Control>}{Home}{/Control}{ArrowRight>7/}")` (the cursor sits after `SELECT `); click "Insert": the text is `"SELECT x FROM t x"` (a space precedes the cursor, so none is added).

### Page tests — append to `web/src/routes/$org/$project/sql/index.test.tsx`

1. **"exports CSV with the run's body"**: `const downloads = captureDownloads(); const seen = watchSql();` render `/acme/support-bot/sql?range=1h`; `await exportAs(user, "CSV")`; `waitFor` `downloads.saved` deep-equals `[{ href: "blob:test", download: "query-20260101T100000Z.csv", rel: "noopener" }]`; `const request = single(seen)`: `request.accept` is `"text/csv"`, `Object.keys(request.body).sort()` is `["from", "query", "to"]`, `Date.parse(String(request.body.to)) - Date.parse(String(request.body.from))` is `3_600_000`.
2. **"exports NDJSON and Parquet"**: `captureDownloads()`; `watchSql()`; `exportAs(user, "NDJSON")`; `waitFor` one saved file `query-20260101T100000Z.ndjson`; `waitFor` `queryByText("Exporting…")` is null; `exportAs(user, "Parquet")`; `waitFor` the second saved file is `query-20260101T100000Z.parquet`; the two accepts are `application/x-ndjson` and `application/vnd.apache.parquet`.
3. **"shows Exporting… and blocks Run"**: `captureDownloads()`; `server.use(http.post("*/api/v1/projects/:project/sql", async () => { await gate; return new HttpResponse("a\r\n", { headers: { "Content-Type": "text/csv" } }); }))`; `exportAs(user, "CSV")`; `findByText("Exporting…")`; `getByRole("button", { name: "Run" })` is disabled; open Export: each item has `aria-disabled="true"`; `await user.keyboard("{Escape}")`; `release()`; `waitFor` `queryByText("Exporting…")` is null and Run is enabled.
4. **"blocks export while a run is pending"**: `captureSql(async () => { await gate; return HttpResponse.json(resultDocument); })`; click Run; open Export: the `CSV` item has `aria-disabled="true"`; Escape; `release()`.
5. **"blocks an export with an invalid parameter"**: `storeFullDraft({ query: "SELECT {n:Int}", params: { n: "x" } })`; `const seen = watchSql()`; `exportAs(user, "CSV")`; `findByText("Enter an integer")`; after `await new Promise((resolve) => setTimeout(resolve, 50))`, `seen` is empty.
6. **"disables export items on an empty query"**: `storeDraft("  ")`; open Export: the `CSV` item has `aria-disabled="true"`.
7. **"disables export for viewers"**: `asRole("viewer")`; open Export (the trigger is enabled); `const item = await screen.findByRole("menuitem", { name: /^CSV/ })` has `aria-disabled="true"` and contains the text `EXPORT_ROLE_REASON`.
8. **"shows a refused export"**: `const downloads = captureDownloads();` `server.use(http.post("*/api/v1/projects/:project/sql", () => HttpResponse.json({ type: "about:blank", title: "Forbidden", status: 403, code: "forbidden" }, { status: 403, headers: PROBLEM_JSON })))`; `exportAs(user, "CSV")`; `findByRole("alert")` has text `EXPORT_ROLE_REASON`; `downloads.saved` is empty.
9. **"reports a network failure"**: `const downloads = captureDownloads();` `server.use(http.post("*/api/v1/projects/:project/sql", () => HttpResponse.error()))`; `exportAs(user, "CSV")`; `findByRole("alert")` has text `The export failed before it finished`; `downloads.saved` is empty. (The cut-after-headers case is the `export.test.ts` unit test.)
10. **"Run clears an export error"**: the setup of test 8 (same `server.use` answering `403` to every `POST /sql`), then `exportAs(user, "CSV")` and `findByText(EXPORT_ROLE_REASON)`; then `server.use(http.post("*/api/v1/projects/:project/sql", () => HttpResponse.json(resultDocument)))`; click Run; `waitFor` `queryByText(EXPORT_ROLE_REASON)` is null.
11. **"draws a line chart with one series per value and keeps the tab"**: `captureSql(() => HttpResponse.json({ columns: [{ name: "day", type: 'Timestamp(Nanosecond, Some("UTC"))' }, { name: "model", type: "Utf8" }, { name: "cost", type: "Float64" }], rows: [{ day: "2026-01-01T00:00:00.000000000Z", model: "a;}b{", cost: 1 }, { day: "2026-01-02T00:00:00.000000000Z", model: "b", cost: 2 }], truncated: false }))`; click Run; `findByRole("grid", { name: "Query results" })`; `await user.click(screen.getByRole("tab", { name: "Chart" }))`; `findByRole("region", { name: "Line chart of cost by day, one series per model" })`; click Run again; `waitFor` the tab `Chart` has `aria-selected="true"` and the region is shown again.
12. **"draws a bar chart"**: answer columns `name` (`Utf8`) and `traces` (`Int64`) with two rows; run; click the Chart tab; `findByRole("region", { name: "Bar chart of traces by name" })`.
13. **"says when no chart fits"**: answer one `Utf8` column; run; Chart tab; `findByText(NO_CHART)`.
14. **"notes a clipped chart"**: answer `name` (`Utf8`) and six `Int64` columns `a`…`f` with one row; run; Chart tab; `findByText(CLIPPED)`.
15. **"inserts names from the schema browser"**: `storeDraft("")`; render; `const traces = await screen.findByRole("list", { name: "traces columns" })`; `await user.click(within(traces).getByRole("button", { name: "Insert cost_total" }))`; `waitFor` `editorContent().textContent` is `"cost_total"`; `await user.click(screen.getByRole("button", { name: "spans", expanded: false }))`; `await user.click(within(screen.getByRole("list", { name: "spans columns" })).getByRole("button", { name: "Insert model" }))`; `waitFor` the text is `"cost_total model"`.
16. **"lists SQL functions on demand"**: `storeDraft("")`; render; `await screen.findByRole("button", { name: "Insert traces" })` (the schema has loaded); `getByRole("button", { name: "Functions", expanded: false })`; `queryByRole("list", { name: "Functions" })` is null; click the toggle; `within(getByRole("list", { name: "Functions" })).getAllByRole("button").map((button) => button.getAttribute("aria-label"))` equals `schemaFixture.functions.filter((fn) => fn.in_sql).map((fn) => \`Insert ${fn.name}(\`)`; click `Insert now(`; `waitFor` the editor text is `"now("`.
17. **"shows Loading… until the schema arrives"**: `server.use(http.get("*/api/v1/projects/:project/schema", async () => { await gate; return HttpResponse.json(schemaFixture); }))`; render; `within(await screen.findByRole("navigation", { name: "Schema" })).getByText("Loading…")`; `release()`; `findByRole("button", { name: "Insert traces" })`.
18. **"shows Schema unavailable when the schema fails"**: `server.use(http.get("*/api/v1/projects/:project/schema", () => HttpResponse.json({ type: "about:blank", title: "Service Unavailable", status: 503, code: "busy" }, { status: 503, headers: PROBLEM_JSON })))`; render; `within(await screen.findByRole("navigation", { name: "Schema" })).findByText("Schema unavailable")`.
19. **"hides the schema browser and remembers it"**: render; `await user.click(await screen.findByRole("button", { name: "Schema" }))`; `queryByRole("navigation", { name: "Schema" })` is null; the button has `aria-pressed="false"`; `localStorage.getItem("llmobs.sql.schema")` is `"closed"`.
20. **"starts closed when it was closed"**: `localStorage.setItem("llmobs.sql.schema", "closed")`; render; `findByRole("button", { name: "Schema" })` has `aria-pressed="false"`; no `navigation` "Schema".

## Commands

```bash
export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH"
pnpm --dir web exec biome check --write .
just check-web
```

While iterating: `pnpm --dir web exec vitest run src/lib/sql src/mocks/sql.test.ts src/components/sql src/routes/\$org/\$project/sql`. Before committing: the same `PATH` line, `pnpm --dir web exec biome check --write .`, then `just check`. Check the roadmap edit with `git diff docs/roadmap.md`: one changed line.

## Done when

- Under `just web-dev`: Export downloads the mock files, the Chart tab draws a chart for results with a time or text column, the schema browser inserts names.
- Every test above passes; `just check` passes; no `vi.mock` (the one `vi.spyOn(globalThis, "fetch")` in `export.test.ts` is the documented exception).
- `docs/roadmap.md` row H says **Implemented.** as given in step 11.
- One commit, subject `web: add console exports, chart and schema browser`, no trailer lines. Report "no new dependency".
