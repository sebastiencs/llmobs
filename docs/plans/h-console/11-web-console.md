# H11 — SQL console page

Commit subject: `web: add the SQL console page`.

See [README.md](README.md) for the scope, the decisions (D-H-7, D-H-8, D-H-13), § Contracts between sub-phases and the conventions. H1–H10 are done: `crates/server/openapi.json` and `web/src/api/schema.gen.ts` declare `run_sql` (`POST /api/v1/projects/{project}/sql`, `200` with `application/json` first, then `text/csv`, `application/x-ndjson`, `application/vnd.apache.parquet`) with the schemas `SqlQueryRequest`, `SqlQueryResult` and `SqlColumnInfo`, and `fixtures/api/requests/sql-query.json` and `fixtures/api/static/sql-result.json` exist. If `grep -n '"/api/v1/projects/{project}/sql"' web/src/api/schema.gen.ts` finds nothing, STOP: H4 is not merged.

H11 builds the page: the route, the sidebar entry, a multi-line CodeMirror editor with schema autocomplete and syntax colours, Run (button and ⌘↵ / Ctrl↵), the result grid, inline errors with the position underlined, the shared time-range picker, a per-project draft in localStorage, and the MSW mock for `POST /sql`. H12 adds parameters, saved queries (and with them the `view` search param, the draft's params and view, the `replace` navigation option and the `TooltipProvider`), examples and history. H13 adds exports, the chart tab and the schema browser. Build only what this file lists.

## Read first

- `web/src/routes/$org/$project/dashboard/index.tsx` (a page with the time-range picker and `validateSearch`) and its test.
- `web/src/components/filter-bar/editor.tsx` (the CodeMirror setup this editor follows) and `web/src/components/filter-bar/filter-bar.test.tsx` (how tests type into CodeMirror: `document.querySelector(".cm-content")`, `user.type`).
- `web/src/components/data-table/data-table.tsx` (`DataTable`, `DataTableColumn`) and `web/src/components/user-columns.tsx` (a column list).
- `web/src/components/time-range/time-range-picker.tsx`, `web/src/lib/time-range.ts`, `web/src/lib/search/common.ts`, `web/src/lib/search/dashboard.ts`.
- `web/src/components/shell/nav.ts`, `web/src/components/shell/project-sidebar.tsx`, `web/src/components/shell/project-context.tsx`, `web/src/components/command/palette.tsx` and its test (`openPalette`), `web/src/lib/shortcuts.ts` and its test.
- `web/src/api/client.ts`, `web/src/api/queries.ts` (`schemaQuery`), `web/src/api/problem.ts`, `web/src/api/types.ts`.
- `web/src/mocks/node.ts`, `web/src/mocks/browser.ts`, `web/src/mocks/saved-views.ts` (a mock in the house style), `web/src/test/setup.ts`, `web/src/test/render.tsx`.
- `fixtures/api/static/sql-result.json`, `fixtures/api/requests/sql-query.json`, `fixtures/api/static/schema.json`.

## Facts this phase relies on

- **Types.** Every API type comes from the generated file through `Schema<"…">` (`web/src/api/types.ts`): `Schema<"SqlQueryRequest">`, `Schema<"SqlQueryResult">`, `Schema<"SchemaResponse">`, `Schema<"Project">`. Never hand-write an API type. A JSON fixture import is not typed as an API type: cast it (`schemaDocument as Schema<"SchemaResponse">`), with `import type { Schema } from "@/api/types";`.
- `useProject()` returns `{ org, project, ref, data }`; `ref` is `projectRef(org, project)`, for example `"acme/support-bot"`.
- `schemaQuery(org, project)` loads `GET /schema`: `{ views: [{ name, description, columns: [{ name, column_type, description, … }] }], functions: [...] }`. The views are `traces`, `spans`, `sessions`, `scores`.
- **Query errors** (README § SQL console endpoint): an SQL text error is `400` `code: "invalid_filter"`, `detail: "invalid query"`, with one entry in `errors`: `{ field: "query", message, position?: { line, column } }`. `line` and `column` are 1-based; `column` counts Unicode characters within the line. The UI shows the first entry's `message` for `invalid_filter`. Other failures carry their message in `detail`: `query_failed` (400), `bad_request` (400), `query_timeout` and `query_too_large` (422), `busy` (503).
- **Empty error bodies.** `openapi-fetch` returns an empty error body as `error: ""`; `openapi-react-query` throws only a truthy error, so such a run *succeeds* with `data` undefined. The console treats any success that is not an `SqlQueryResult` as a failed run ("The query could not be run").
- `SqlQueryResult` is `{ columns: [{ name, type }], rows: [{ <column name>: <JSON value> }], truncated }`. `type` is the Arrow type as DataFusion prints it (`Int64`, `Float64`, `Utf8`, `Timestamp(Nanosecond, Some("UTC"))`, `Decimal128(38, 18)`, …). Column names are unique (spec 05). **Known limit:** an `Int64` above 2^53 arrives as a JSON number and loses precision in `JSON.parse`; the grid shows what the browser parsed. Cast to `VARCHAR` in SQL to see the exact value.
- **Accept.** `run_sql` serves several media types from one `200`, so the console always sends `Accept: application/json` for Run, and narrows the answer with `isSqlResult` (step 5) before using it.
- CodeMirror's `Mod` is Ctrl outside macOS. jsdom's user agent is not a Mac one, so tests press Ctrl+Enter and the Run button's title reads "Run (Ctrl↵)".
- `@codemirror/lang-sql`'s `sql({ schema })` registers completion sources (tables, `table.column`, keywords) as language data; `autocompletion()` uses them. No custom completion source is needed.
- `tsconfig.app.json` sets `noUncheckedIndexedAccess`: `array[0]` is `T | undefined`. Tests read list items through the `single` helper in § Tests, never as `bodies[0].x`.
- Biome formats at 100 columns with organized imports (packages first, then `@/…` aliases in path order). Some lines below may still be longer or out of order; `pnpm --dir web exec biome check --write .` (§ Commands) fixes formatting and import order, so run it before `just check-web`.
- Test setup (`web/src/test/setup.ts`) clears localStorage after each test and fails a test on any unhandled request. The manifest mocks answer `GET /schema` and `GET /api/v1/projects/{project}` for any project, including `acme/playground`.
- `DataTable` always persists column widths under `llmobs.table.<tableId>`; it has no switch to turn that off. The result grid uses `tableId="sql-results"` and column ids `c0`, `c1`, …, so a width the user drags carries over to the same column position of the next query. That limitation is accepted.
- **Use `user.click`**, never `fireEvent.click`: a `fireEvent` click on a router `Link` does not navigate.

## Edit only

- `web/src/components/shell/nav.ts`
- `web/src/lib/shortcuts.ts` and `web/src/lib/shortcuts.test.ts` (one help row)
- `web/src/components/filter-bar/editor.tsx` (export `ensureLayoutRects`, nothing else)
- `web/src/lib/search/sql.ts` (new) and `web/src/lib/search/sql.test.ts` (new)
- `web/src/lib/sql/position.ts` (new) and `web/src/lib/sql/position.test.ts` (new)
- `web/src/lib/sql/draft.ts` (new) and `web/src/lib/sql/draft.test.ts` (new)
- `web/src/lib/sql/request.ts` (new) and `web/src/lib/sql/request.test.ts` (new)
- `web/src/lib/sql/format.ts` (new) and `web/src/lib/sql/format.test.ts` (new)
- `web/src/lib/sql/types.ts` (new) and `web/src/lib/sql/types.test.ts` (new)
- `web/src/components/sql/sql-editor.tsx` (new) and `web/src/components/sql/sql-editor.test.tsx` (new)
- `web/src/components/sql/result-grid.tsx` (new) and `web/src/components/sql/result-grid.test.ts` (new)
- `web/src/components/sql/sql-console.tsx` (new)
- `web/src/routes/$org/$project/sql/index.tsx` (new) and `web/src/routes/$org/$project/sql/index.test.tsx` (new)
- `web/src/mocks/sql.ts` (new) and `web/src/mocks/sql.test.ts` (new)
- `web/src/mocks/node.ts`, `web/src/mocks/browser.ts`
- `web/src/routeTree.gen.ts` is regenerated by `pnpm gen:routes` (run by `just check-web`); commit it if it is tracked (`git ls-files web/src/routeTree.gen.ts`).

## Steps

### 1. Sidebar entry — `web/src/components/shell/nav.ts`

Add `Database` to the `lucide-react` import (keep the names sorted) and append one entry after `Users`:

```ts
  { to: "/$org/$project/sql", label: "SQL", icon: Database },
```

The project sidebar and the ⌘K palette both map `projectNav`, so both list the page. Do not add a `g` shortcut.

### 2. Help row — `web/src/lib/shortcuts.ts`

Insert into `SHORTCUTS`, right after the row whose `keys` is `"0"`:

```ts
  {
    keys: "⌘↵",
    description: "Run the query",
    scope: "SQL console",
    bindings: [],
  },
```

The editor handles the key itself (step 8); `bindings: []` keeps the global listener out of it, as the `g then …` row does. In `web/src/lib/shortcuts.test.ts`, test "lists every shortcut the help dialog shows", insert `"⌘↵"` after `"0"` in the expected list. The palette test "opens the shortcut help for every shortcut" then checks the new row too.

### 3. Search params — `web/src/lib/search/sql.ts`

```ts
import { parseEnum, parseRfc3339 } from "@/lib/search/common";
import { PRESETS, type Preset, type RangeState } from "@/lib/time-range";

export type SqlSearch = {
  range?: Preset;
  from?: string;
  to?: string;
};

export function parseSqlSearch(search: Record<string, unknown>): SqlSearch {
  const range = parseEnum(search.range, PRESETS);
  const from = parseRfc3339(search.from);
  const to = parseRfc3339(search.to);
  const absolute = from !== undefined && to !== undefined && Date.parse(from) < Date.parse(to);
  return {
    ...(range ? { range } : {}),
    ...(absolute ? { from, to } : {}),
  };
}

/** `search` with its range replaced by `state`. Other keys are kept. */
export function withSqlRange(search: SqlSearch, state: RangeState): SqlSearch {
  const { range: _range, from: _from, to: _to, ...rest } = search;
  if (state.kind === "preset") {
    return { ...rest, range: state.preset };
  }
  return { ...rest, from: state.from, to: state.to };
}
```

### 4. Error position — `web/src/lib/sql/position.ts`

```ts
/**
 * UTF-16 offset of a 1-based (line, column) where column counts Unicode characters.
 * Clamped to the text.
 */
export function offsetOf(text: string, line: number, column: number): number {
  const lines = text.split("\n");
  if (!Number.isInteger(line) || !Number.isInteger(column) || line < 1 || column < 1) {
    return 0;
  }
  if (line > lines.length) {
    return text.length;
  }
  let offset = 0;
  for (let index = 0; index < line - 1; index += 1) {
    offset += (lines[index] ?? "").length + 1;
  }
  const current = lines[line - 1] ?? "";
  const prefix = Array.from(current)
    .slice(0, column - 1)
    .join("");
  return Math.min(offset + prefix.length, text.length);
}

/**
 * The range to underline: from the position to the end of the word there
 * (`[A-Za-z0-9_]+`), else one character. At or past the end: the last character.
 * `null` for empty text.
 */
export function sqlErrorRange(
  text: string,
  position: { line: number; column: number },
): { from: number; to: number } | null {
  if (text.length === 0) {
    return null;
  }
  const offset = offsetOf(text, position.line, position.column);
  if (offset >= text.length) {
    return { from: text.length - 1, to: text.length };
  }
  const word = /[A-Za-z0-9_]+/y;
  word.lastIndex = offset;
  const match = word.exec(text);
  return { from: offset, to: match ? offset + match[0].length : offset + 1 };
}
```

### 5. Draft — `web/src/lib/sql/draft.ts`

The editor text of one project, kept in localStorage (D-H-13). H12 adds the parameter values and the saved-query id to this type. Every change is written at once: there is no debounce (a write is a few kilobytes at most, and losing the last keystrokes on a reload would be worse).

```ts
import { useCallback, useState } from "react";
import { STORAGE_PREFIX } from "@/product";

export type SqlDraft = {
  query: string;
};

/** Changes a draft. Always pass a function so two updates in one event both apply. */
export type DraftUpdate = (update: (previous: SqlDraft) => SqlDraft) => void;

export const DEFAULT_QUERY =
  "SELECT name, count(*) AS trace_count\nFROM traces\nGROUP BY name\nORDER BY trace_count DESC\nLIMIT 100";

export function draftKey(projectRef: string): string {
  return `${STORAGE_PREFIX}sql.draft.${projectRef}`;
}

export function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function readDraft(projectRef: string): SqlDraft {
  try {
    const raw = localStorage.getItem(draftKey(projectRef));
    if (raw === null) {
      return { query: DEFAULT_QUERY };
    }
    const parsed: unknown = JSON.parse(raw);
    if (!isRecord(parsed) || typeof parsed.query !== "string") {
      return { query: DEFAULT_QUERY };
    }
    return { query: parsed.query };
  } catch {
    return { query: DEFAULT_QUERY };
  }
}

export function writeDraft(projectRef: string, draft: SqlDraft): void {
  try {
    localStorage.setItem(draftKey(projectRef), JSON.stringify(draft));
  } catch {
    // Private mode or a full quota: the draft still lives in memory for this page.
  }
}

/** The draft of `projectRef`. Each update is written through to localStorage. */
export function useSqlDraft(projectRef: string): [SqlDraft, DraftUpdate] {
  const [draft, setDraft] = useState(() => readDraft(projectRef));
  const update = useCallback<DraftUpdate>(
    (change) => {
      setDraft((previous) => {
        const next = change(previous);
        writeDraft(projectRef, next);
        return next;
      });
    },
    [projectRef],
  );
  return [draft, update];
}
```

Callers use `updateDraft((previous) => …)` only; there is no setter that takes a value. React may call the updater twice in development (Strict Mode); the second write stores the same text, which is harmless. `isRecord` is exported for H12's parsers.

The route mounts the console with `key={ref}` (step 11), so switching projects re-reads the draft.

### 6. Format helpers — `web/src/lib/sql/format.ts`

```ts
import type { Problem } from "@/api/problem";
import type { Schema } from "@/api/types";

/** True for a JSON run answer. `run_sql` declares several media types, so narrow before use. */
export function isSqlResult(value: unknown): value is Schema<"SqlQueryResult"> {
  return (
    typeof value === "object" &&
    value !== null &&
    Array.isArray((value as { columns?: unknown }).columns) &&
    Array.isArray((value as { rows?: unknown }).rows)
  );
}

/** The message shown under the editor for a failed run. */
export function sqlProblemMessage(problem: Problem): string {
  const first = problem.errors?.[0];
  if (problem.code === "invalid_filter" && first) {
    return first.message;
  }
  return problem.detail ?? problem.title;
}

/** Shown when the failure is not a problem response (network error, empty or HTML body). */
export const RUN_FAILED = "The query could not be run";

/** `5 rows · 12 ms`, `1 row · 0 ms`, `10000 rows (truncated) · 3 ms`. */
export function resultStatus(count: number, truncated: boolean, durationMs: number): string {
  const rows = `${count} ${count === 1 ? "row" : "rows"}${truncated ? " (truncated)" : ""}`;
  return `${rows} · ${durationMs} ms`;
}

/** `⌘↵` on Apple platforms, `Ctrl↵` elsewhere, for the Run button's title. */
export function runShortcutLabel(userAgent: string): string {
  return /Mac|iPhone|iPad/.test(userAgent) ? "⌘↵" : "Ctrl↵";
}
```

### 7. Arrow types — `web/src/lib/sql/types.ts`

The grid (step 9) and H13's chart both classify columns by their Arrow type string, so the helpers live in `lib`, not in a component.

```ts
const NUMERIC = /^(U?Int(8|16|32|64)|Float(16|32|64)|Decimal(32|64|128|256)\()/;

/** `Int*`, `UInt*`, `Float*` and `Decimal*(…)` columns. */
export function isNumericType(type: string): boolean {
  return NUMERIC.test(type);
}
```

H13 adds `isTimestampType`, `isDateType` and `isTextType` to this file.

### 8. Request body — `web/src/lib/sql/request.ts`

```ts
import type { Schema } from "@/api/types";

/**
 * `POST /sql` body. `limit` and `timeout_ms` are left to the server defaults
 * (10,000 rows, 60 s).
 */
export function sqlRunBody(input: {
  query: string;
  from: string;
  to: string;
}): Schema<"SqlQueryRequest"> {
  return { query: input.query, from: input.from, to: input.to };
}
```

H12 adds `params`. Keep the function and its name.

### 9. Editor — `web/src/components/sql/sql-editor.tsx`

First, in `web/src/components/filter-bar/editor.tsx`, change `function ensureLayoutRects()` to `export function ensureLayoutRects()`. Change nothing else there.

The SQL editor is multi-line and long-lived: unlike `FilterEditor`, it is created once and never recreated on a keystroke. External value changes (a loaded saved query, an example) are dispatched into it; the schema and the accessibility attributes are swapped through `Compartment`s; the error underline is a `StateField` driven by a `StateEffect`, cleared by any edit.

```tsx
import { autocompletion, type Completion } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { type SQLNamespace, sql } from "@codemirror/lang-sql";
import { defaultHighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { Compartment, EditorState, Prec, StateEffect, StateField } from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  keymap,
  lineNumbers,
  placeholder,
} from "@codemirror/view";
import { useEffect, useRef } from "react";
import type { Schema } from "@/api/types";
import { ensureLayoutRects } from "@/components/filter-bar/editor";

const errorMark = Decoration.mark({ class: "cm-sql-error" });
const setError = StateEffect.define<{ from: number; to: number } | null>();

const errorField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(marks, tr) {
    let next = tr.docChanged ? Decoration.none : marks;
    for (const effect of tr.effects) {
      if (effect.is(setError)) {
        const range = effect.value;
        next =
          range && range.from < range.to
            ? Decoration.set([errorMark.range(range.from, range.to)])
            : Decoration.none;
      }
    }
    return next;
  },
  provide: (field) => EditorView.decorations.from(field),
});

const editorTheme = EditorView.theme({
  "&": { height: "100%", fontSize: "12px", backgroundColor: "transparent" },
  "&.cm-focused": { outline: "2px solid var(--ring)", outlineOffset: "1px" },
  ".cm-scroller": {
    fontFamily: "var(--font-mono), ui-monospace, monospace",
    overflow: "auto",
    lineHeight: "20px",
  },
  ".cm-content": { caretColor: "var(--foreground)" },
  ".cm-gutters": { backgroundColor: "transparent", borderRight: "1px solid var(--border)" },
  ".cm-tooltip.cm-tooltip-autocomplete": { fontSize: "12px" },
  ".cm-sql-error": { textDecoration: "underline wavy var(--destructive)" },
});

/** Tables and columns for `sql({ schema })`. */
export function sqlNamespace(schema: Schema<"SchemaResponse"> | undefined): SQLNamespace {
  const namespace: Record<string, readonly Completion[]> = {};
  for (const view of schema?.views ?? []) {
    namespace[view.name] = view.columns.map((column) => ({
      label: column.name,
      type: "property",
      detail: column.column_type,
      info: column.description,
    }));
  }
  return namespace;
}

/** The language extension for a schema. Exported for the completion tests. */
export function sqlLanguage(schema: Schema<"SchemaResponse"> | undefined) {
  return sql({ schema: sqlNamespace(schema), upperCaseKeywords: true });
}

function contentAttributes(invalid: boolean, describedBy: string | undefined) {
  return EditorView.contentAttributes.of({
    "aria-label": "SQL query",
    "aria-invalid": invalid ? "true" : "false",
    ...(describedBy ? { "aria-describedby": describedBy } : {}),
  });
}

export function SqlEditor({
  value,
  onChange,
  onRun,
  errorRange,
  schema,
  invalid,
  describedBy,
}: {
  value: string;
  onChange: (text: string) => void;
  /** Called by Mod-Enter with the editor's current text. */
  onRun: (text: string) => void;
  errorRange: { from: number; to: number } | null;
  schema: Schema<"SchemaResponse"> | undefined;
  /** The last run failed on this text. */
  invalid: boolean;
  /** Id of the element that explains the failure. */
  describedBy?: string;
}) {
  const parent = useRef<HTMLDivElement>(null);
  const viewRef = useRef<EditorView | null>(null);
  const language = useRef(new Compartment());
  const attributes = useRef(new Compartment());
  /** Texts the editor reported through `onChange` that have not come back as `value` yet. */
  const echoes = useRef<string[]>([]);
  const onChangeRef = useRef(onChange);
  const onRunRef = useRef(onRun);
  const initial = useRef({ value, schema, invalid, describedBy });
  onChangeRef.current = onChange;
  onRunRef.current = onRun;

  useEffect(() => {
    const host = parent.current;
    if (!host) {
      return;
    }
    ensureLayoutRects();
    const start = initial.current;
    const view = new EditorView({
      parent: host,
      state: EditorState.create({
        doc: start.value,
        extensions: [
          editorTheme,
          lineNumbers(),
          history(),
          syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
          language.current.of(sqlLanguage(start.schema)),
          autocompletion(),
          placeholder("SELECT … FROM traces"),
          errorField,
          attributes.current.of(contentAttributes(start.invalid, start.describedBy)),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) {
              const text = update.state.doc.toString();
              echoes.current.push(text);
              onChangeRef.current(text);
            }
          }),
          Prec.highest(
            keymap.of([
              {
                key: "Mod-Enter",
                run: (editor) => {
                  onRunRef.current(editor.state.doc.toString());
                  return true;
                },
              },
            ]),
          ),
          keymap.of([...historyKeymap, ...defaultKeymap]),
        ],
      }),
    });
    viewRef.current = view;
    return () => {
      viewRef.current = null;
      view.destroy();
    };
  }, []);

  useEffect(() => {
    const view = viewRef.current;
    const echoed = echoes.current.lastIndexOf(value);
    if (echoed >= 0) {
      echoes.current.splice(0, echoed + 1);
      return;
    }
    echoes.current = [];
    if (!view || view.state.doc.toString() === value) {
      return;
    }
    view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: value } });
  }, [value]);

  useEffect(() => {
    viewRef.current?.dispatch({ effects: language.current.reconfigure(sqlLanguage(schema)) });
  }, [schema]);

  useEffect(() => {
    viewRef.current?.dispatch({
      effects: attributes.current.reconfigure(contentAttributes(invalid, describedBy)),
    });
  }, [invalid, describedBy]);

  const errorFrom = errorRange?.from;
  const errorTo = errorRange?.to;
  useEffect(() => {
    const view = viewRef.current;
    if (!view) {
      return;
    }
    const length = view.state.doc.length;
    const range =
      errorFrom !== undefined && errorTo !== undefined
        ? { from: Math.min(errorFrom, length), to: Math.min(errorTo, length) }
        : null;
    view.dispatch({ effects: setError.of(range) });
  }, [errorFrom, errorTo]);

  return (
    <div
      ref={parent}
      className="h-full min-h-0 overflow-hidden rounded-md border border-input"
    />
  );
}
```

Notes:
- The empty dependency list of the first effect is deliberate: the effect reads only refs, so Biome accepts it. Do not add `value` or `schema` there; that would recreate the editor on every keystroke.
- **Why `echoes`.** The editor is the source of truth while the user types; `value` comes back from the parent one render later. When keystrokes arrive faster than React renders, a `value` can arrive that is older than the editor's text. Without `echoes` the sync effect would see a difference and overwrite the newer keystrokes. Every text the editor reports is recorded; when `value` equals one of them, that echo and every older one are dropped and nothing is dispatched. A `value` that is not an echo (a loaded query, an example, H13's insert) clears the list and replaces the document. That dispatch is itself reported through `onChange` and recorded, which is harmless: the next echo match removes it.
- The error effect is declared after the value effect, and React runs effects in declaration order, so an underline set together with a new text applies to that text. Ranges past the end of the document are clamped; an empty range draws nothing.
- There is no `indentWithTab`: Tab moves focus out of the editor.

### 10. Result grid — `web/src/components/sql/result-grid.tsx`

`DataTable` fits: one row per result row, no paging (`hasNextPage: false`), no sorting, no activation. The columns and rows are memoized on the result object, and the grid is a `memo` component, so typing in the editor does not rebuild them.

```tsx
import { Rows3 } from "lucide-react";
import { memo, useMemo } from "react";
import type { Schema } from "@/api/types";
import { DataTable, type DataTableColumn } from "@/components/data-table/data-table";
import { EmptyState } from "@/components/empty-state";
import { isNumericType } from "@/lib/sql/types";

export const RESULT_TABLE_ID = "sql-results";

export type SqlRow = { index: number; values: Record<string, unknown> };

/** Display text of one JSON cell. `null` and a missing key read `NULL`; objects are JSON. */
export function formatSqlCell(value: unknown): string {
  if (value === null || value === undefined) {
    return "NULL";
  }
  if (typeof value === "string") {
    return value;
  }
  if (typeof value === "number" || typeof value === "boolean") {
    return String(value);
  }
  return JSON.stringify(value);
}

export function sqlColumns(result: Schema<"SqlQueryResult">): DataTableColumn<SqlRow>[] {
  return result.columns.map((column, index) => ({
    // Column names may hold spaces; DataTable builds DOM ids from the column id.
    id: `c${index}`,
    size: 160,
    header: column.name,
    enableSorting: false,
    meta: { mono: true, align: isNumericType(column.type) ? "right" : "left" },
    cell: ({ row }) => {
      const values = row.original.values;
      // Own keys only: a column named `__proto__` or `constructor` must not read the prototype.
      const value = Object.hasOwn(values, column.name) ? values[column.name] : undefined;
      const text = formatSqlCell(value);
      const isNull = value === null || value === undefined;
      return (
        <span title={text} className={isNull ? "text-muted-foreground" : undefined}>
          {text}
        </span>
      );
    },
  }));
}

function sqlRows(result: Schema<"SqlQueryResult">): SqlRow[] {
  return result.rows.map((values, index) => ({
    index,
    values: values as Record<string, unknown>,
  }));
}

export const SqlResultGrid = memo(function SqlResultGrid({
  result,
}: {
  result: Schema<"SqlQueryResult">;
}) {
  const columns = useMemo(() => sqlColumns(result), [result]);
  const rows = useMemo(() => sqlRows(result), [result]);
  return (
    <DataTable
      tableId={RESULT_TABLE_ID}
      ariaLabel="Query results"
      columns={columns}
      rows={rows}
      getRowId={(row) => String(row.index)}
      onActivate={() => {}}
      hasNextPage={false}
      isFetchingNextPage={false}
      fetchNextPage={() => {}}
      emptyState={<EmptyState icon={Rows3} title="No rows" />}
    />
  );
});
```

If `tsc` reports the `values as Record<string, unknown>` cast as unnecessary, drop it. If `tsconfig`'s `lib` lacks `Object.hasOwn`, use `Object.prototype.hasOwnProperty.call(values, column.name)`. If `DataTable` types `header` as something other than a string, pass `header: () => column.name` and adapt the unit test (§ Tests) to call it.

### 11. Console — `web/src/components/sql/sql-console.tsx`

```tsx
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
```

Notes:
- `lastRun` is set in `onSuccess` together with the duration, so the status line and the grid appear in the same render. A new run clears `lastRun` and `badAnswer` first.
- `run.variables?.body.query` reads the submitted text. If `tsc` types `body` as optional on `variables`, write `run.variables?.body?.query`.
- If `tsc` rejects `headers` in the `mutate` argument, report the exact error and STOP; do not drop the header.
- Every hook call comes before any early return; this component has none.

### 12. Route — `web/src/routes/$org/$project/sql/index.tsx`

```tsx
import { createFileRoute } from "@tanstack/react-router";
import { useProject } from "@/components/shell/project-context";
import { SqlConsole } from "@/components/sql/sql-console";
import { parseSqlSearch } from "@/lib/search/sql";

export const Route = createFileRoute("/$org/$project/sql/")({
  validateSearch: (search) => parseSqlSearch(search),
  component: SqlPage,
});

function SqlPage() {
  const { org, project, ref } = useProject();
  const search = Route.useSearch();
  const navigate = Route.useNavigate();
  return (
    <SqlConsole
      key={ref}
      search={search}
      onSearch={(update) => {
        void navigate({
          to: "/$org/$project/sql",
          params: { org, project },
          search: (previous) => update(previous),
        });
      }}
    />
  );
}
```

The sidebar and the palette navigate here with `applyDefaultView: true` in the history state. The SQL page ignores it: the console has no default saved query (spec 11 defaults are for list pages; H12 does not add one).

Run `pnpm --dir web gen:routes` (or let `just check-web` do it) so `routeTree.gen.ts` knows the route.

### 13. Mock — `web/src/mocks/sql.ts`

```ts
import { type HttpHandler, HttpResponse, http } from "msw";
import resultDocument from "../../../fixtures/api/static/sql-result.json";

export const SQL_PATH = "/api/v1/projects/:project/sql";

const EMPTY_QUERY = {
  type: "about:blank",
  title: "Bad Request",
  status: 400,
  code: "bad_request",
  detail: "query is empty",
};

/** `POST /sql`: the example result for any non-empty query; a 400 problem otherwise. */
export const sqlHandlers: HttpHandler[] = [
  http.post(SQL_PATH, async ({ request }) => {
    const body = await readJson(request);
    if (!body || typeof body.query !== "string" || body.query.trim().length === 0) {
      return HttpResponse.json(EMPTY_QUERY, {
        status: 400,
        headers: { "Content-Type": "application/problem+json" },
      });
    }
    return HttpResponse.json(resultDocument);
  }),
];

async function readJson(request: Request): Promise<Record<string, unknown> | undefined> {
  try {
    const body: unknown = await request.json();
    return typeof body === "object" && body !== null && !Array.isArray(body)
      ? (body as Record<string, unknown>)
      : undefined;
  } catch {
    return undefined;
  }
}
```

H13 extends this handler for `Accept`. Register it:
- `web/src/mocks/node.ts`: `import { sqlHandlers } from "./sql";` and `export const server = setupServer(...sqlHandlers, ...handlers);`.
- `web/src/mocks/browser.ts`: import it and add `...sqlHandlers,` right after `...savedViewHandlers(seedSavedViews()),`.

`POST /sql` is a read, so it is a default handler of the test server (unlike the writes of `mocks/writes.ts`, which tests install themselves). `mocks/writes.test.ts` only covers auth, account, admin, org and project-settings paths, so it does not need `run_sql`.

## States

| State | What shows |
|---|---|
| Before the first run | Editor with the draft (or `DEFAULT_QUERY`), "Run a query to see results." |
| Running | `role="status"` "Running…"; Run disabled |
| Success | Status line (`resultStatus`), then the grid `Query results`; zero rows shows "No rows" inside the grid |
| Query error | `role="alert"` (`id="sql-error"`) under the editor; the editor has `aria-invalid="true"` and `aria-describedby="sql-error"` while it holds the failed text; the position, when given, is underlined; no grid |
| Non-problem failure (network, HTML, empty body) | The alert reads "The query could not be run" |
| Empty editor | Run disabled; Ctrl+Enter does nothing |

## Keyboard

- ⌘↵ / Ctrl↵ in the editor runs the editor's text. The help dialog (`?`) lists it.
- Tab moves focus out of the editor (no `indentWithTab`).
- The Run button's title is "Run (⌘↵)" on Apple platforms and "Run (Ctrl↵)" elsewhere.

## Tests

Use these imports, adding only what a file uses:

```ts
// web/src/routes/$org/$project/sql/index.test.tsx
import { screen, waitFor, within } from "@testing-library/react";
import { HttpResponse, http } from "msw";
import { expect, it } from "vitest";
import { DEFAULT_QUERY } from "@/lib/sql/draft";
import { server } from "@/mocks/node";
import { renderApp } from "@/test/render";
import resultDocument from "../../../../../../fixtures/api/static/sql-result.json";

// web/src/mocks/sql.test.ts
import { expect, it } from "vitest";
import requestDocument from "../../../fixtures/api/requests/sql-query.json";
import resultDocument from "../../../fixtures/api/static/sql-result.json";

// web/src/lib/sql/format.test.ts
import { expect, it } from "vitest";
import type { Problem } from "@/api/problem";
import { isSqlResult, resultStatus, runShortcutLabel, sqlProblemMessage } from "@/lib/sql/format";

// web/src/components/sql/sql-editor.test.tsx
import { CompletionContext, type CompletionSource } from "@codemirror/autocomplete";
import { ensureSyntaxTree } from "@codemirror/language";
import { EditorState } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { expect, it } from "vitest";
import type { Schema } from "@/api/types";
import { SqlEditor, sqlLanguage, sqlNamespace } from "@/components/sql/sql-editor";
import schemaFixture from "../../../../fixtures/api/static/schema.json";

const schemaDocument = schemaFixture as Schema<"SchemaResponse">;
```

The cast is required: a JSON import's `column_type` is `string`, not the generated union. If `tsc` refuses the direct cast, write `schemaFixture as unknown as Schema<"SchemaResponse">`.

Helpers in the page test:

```ts
const PROBLEM_JSON = { "Content-Type": "application/problem+json" };

function single<T>(items: readonly T[]): T {
  expect(items).toHaveLength(1);
  const [item] = items;
  if (item === undefined) {
    throw new Error("expected one item");
  }
  return item;
}

function editorContent(): HTMLElement {
  const content = document.querySelector(".cm-content");
  if (!(content instanceof HTMLElement)) {
    throw new Error("SQL editor did not mount");
  }
  return content;
}

function storeDraft(query: string, ref = "acme/support-bot") {
  localStorage.setItem(`llmobs.sql.draft.${ref}`, JSON.stringify({ query }));
}

function captureSql(
  answer: () => Response | Promise<Response> = () => HttpResponse.json(resultDocument),
) {
  const bodies: Record<string, unknown>[] = [];
  const accepts: (string | null)[] = [];
  server.use(
    http.post("*/api/v1/projects/:project/sql", async ({ request }) => {
      accepts.push(request.headers.get("Accept"));
      bodies.push((await request.json()) as Record<string, unknown>);
      return answer();
    }),
  );
  return { bodies, accepts };
}
```

Read a body as `const body = single(sql.bodies);` inside a `waitFor`, then use `body.query`. Never index `bodies[0]`. Prefer seeding the editor through `storeDraft` over typing into CodeMirror; type only where a test is about typing.

### `web/src/lib/shortcuts.test.ts`

The existing test's expected list gains `"⌘↵"` after `"0"` (step 2).

### `web/src/lib/search/sql.test.ts`

1. `parseSqlSearch` keeps `range: "1h"` and an absolute pair with `from < to`; drops `range: "2h"`, a pair with `from >= to`, `from` alone and an unknown key `view: "x"`.
2. `withSqlRange({ range: "1h" }, { kind: "absolute", from: A, to: B })` equals `{ from: A, to: B }`; `withSqlRange({ from: A, to: B }, { kind: "preset", preset: "7d" })` equals `{ range: "7d" }`.

### `web/src/lib/sql/position.test.ts`

1. `offsetOf("SELECT 1\nFROM nme", 2, 6)` is `14`; `offsetOf("abc", 1, 1)` is `0`; `offsetOf("abc", 5, 1)` is `3`; `offsetOf("abc", 0, 1)` is `0`.
2. Characters, not code units: `offsetOf("'😀' x", 1, 5)` is `5`.
3. `sqlErrorRange("SELECT 1\nFROM nme", { line: 2, column: 6 })` is `{ from: 14, to: 17 }`; `sqlErrorRange("f(x)", { line: 1, column: 2 })` is `{ from: 1, to: 2 }`; `sqlErrorRange("ab", { line: 1, column: 9 })` is `{ from: 1, to: 2 }`; `sqlErrorRange("", { line: 1, column: 1 })` is `null`.

### `web/src/lib/sql/draft.test.ts`

1. `readDraft("acme/support-bot")` with nothing stored is `{ query: DEFAULT_QUERY }`.
2. After `writeDraft("acme/support-bot", { query: "SELECT 1" })`, `localStorage.getItem("llmobs.sql.draft.acme/support-bot")` is `'{"query":"SELECT 1"}'` and `readDraft` returns `{ query: "SELECT 1" }`; `readDraft("acme/playground")` is still the default.
3. Stored `"not json"` and stored `'{"query":3}'` read as the default.
4. `useSqlDraft` (with `renderHook` and `act` from `@testing-library/react`): two updates in one `act` — `update((d) => ({ query: \`${d.query}A\` }))` twice, starting from a stored `{ query: "" }` — leave `result.current[0]` equal to `{ query: "AA" }` and the same JSON in localStorage.

### `web/src/lib/sql/request.test.ts`

1. `sqlRunBody({ query: "SELECT 1", from: A, to: B })` deep-equals `{ query: "SELECT 1", from: A, to: B }` (no `limit`, no `timeout_ms`).

### `web/src/lib/sql/format.test.ts`

1. `isSqlResult({ columns: [], rows: [], truncated: false })` is true; `isSqlResult("a,b")`, `isSqlResult("")`, `isSqlResult(undefined)`, `isSqlResult(null)`, `isSqlResult({ rows: [] })` are false.
2. With `const invalid: Problem = { type: "about:blank", title: "Bad Request", status: 400, code: "invalid_filter", detail: "invalid query", errors: [{ field: "query", message: "m" }] };`, `sqlProblemMessage(invalid)` is `"m"`; a `Problem` with `code: "query_timeout"` and `detail: "d"` gives `"d"`; one with no `detail` gives its `title`.
3. `resultStatus(1, false, 0)` is `"1 row · 0 ms"`; `resultStatus(5, false, 12)` is `"5 rows · 12 ms"`; `resultStatus(10000, true, 3)` is `"10000 rows (truncated) · 3 ms"`.
4. `runShortcutLabel("Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)")` is `"⌘↵"`; `runShortcutLabel(navigator.userAgent)` (jsdom) and `runShortcutLabel("Mozilla/5.0 (X11; Linux x86_64)")` are `"Ctrl↵"`.

### `web/src/lib/sql/types.test.ts`

1. `isNumericType`: true for `Int8`, `Int64`, `UInt8`, `UInt64`, `Float32`, `Float64`, `Decimal128(38, 18)`, `Decimal256(76, 10)`; false for `Utf8`, `Timestamp(Nanosecond, Some("UTC"))`, `Boolean`, `Date32`, `List(Int64)`, `Interval(MonthDayNano)`.

### `web/src/components/sql/result-grid.test.ts`

1. `formatSqlCell`: `null` → `"NULL"`, `undefined` → `"NULL"`, `"x"` → `"x"`, `3.5` → `"3.5"`, `false` → `"false"`, `{ k: "v" }` → `'{"k":"v"}'`, `[1, 2]` → `"[1,2]"`.
2. `const columns = sqlColumns({ columns: [{ name: "a b", type: "Utf8" }, { name: "n", type: "Int64" }], rows: [], truncated: false });` then `columns.map((column) => column.id)` is `["c0", "c1"]`, `columns.map((column) => column.header)` is `["a b", "n"]`, and `columns.map((column) => column.meta?.align)` is `["left", "right"]`.

### `web/src/components/sql/sql-editor.test.tsx`

Shared completion helper:

```ts
async function labelsAt(state: EditorState, pos: number): Promise<{ label: string; detail?: string }[]> {
  ensureSyntaxTree(state, state.doc.length, 1000);
  const sources = state.languageDataAt<CompletionSource>("autocomplete", pos);
  const results = await Promise.all(
    sources.map((source) => source(new CompletionContext(state, pos, true))),
  );
  return results.flatMap((result) => result?.options ?? []);
}
```

Harness (used by tests 4–7); the "After" button and the `Value` output follow the editor:

```tsx
function Harness({
  initial = "SELECT 1",
  schema,
  errorRange = null,
}: {
  initial?: string;
  schema?: Schema<"SchemaResponse">;
  errorRange?: { from: number; to: number } | null;
}) {
  const [value, setValue] = useState(initial);
  return (
    <>
      <button type="button" onClick={() => setValue("SELECT 2")}>
        Load
      </button>
      <SqlEditor
        value={value}
        onChange={setValue}
        onRun={() => {}}
        errorRange={errorRange}
        schema={schema}
        invalid={false}
      />
      <button type="button">After</button>
      <output aria-label="Value">{value}</output>
    </>
  );
}
```

1. **`sqlNamespace`**: `Object.keys(sqlNamespace(schemaDocument))` is `["traces", "spans", "sessions", "scores"]`; the `traces` entry's first item is `{ label: "trace_id", type: "property", detail: "string", info: <its description from the fixture> }`; `sqlNamespace(undefined)` is `{}`.
2. **tables complete only with a schema**: `const doc = "SELECT * FROM tr";` with `EditorState.create({ doc, extensions: [sqlLanguage(undefined)] })`, `(await labelsAt(state, doc.length)).map((o) => o.label)` does not contain `"traces"`; with `sqlLanguage(schemaDocument)` it does.
3. **columns complete after a table name**: `const doc = "SELECT traces.tr";` with `sqlLanguage(schemaDocument)`: the options contain `{ label: "trace_id", detail: "string" }` (check with `expect.objectContaining`). If `languageDataAt` returns no source in jsdom, STOP and report; do not replace these tests with weaker ones.
4. **the schema arriving reconfigures the editor**: `const { rerender } = render(<Harness />);` `const host = document.querySelector(".cm-editor");` and `const view = host instanceof HTMLElement ? EditorView.findFromDOM(host) : null;` (`expect(view).not.toBeNull()`); `view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: "SELECT * FROM tr" } })`; `(await labelsAt(view.state, view.state.doc.length)).map((o) => o.label)` does not contain `"traces"`; `rerender(<Harness schema={schemaDocument} />)`; `waitFor` (async) the same labels contain `"traces"`.
5. **external value sync and typing**: `render(<Harness />)`; `document.querySelector(".cm-content")?.textContent` is `"SELECT 1"`; click "Load"; `waitFor` it is `"SELECT 2"`. Then `const user = userEvent.setup();` focus `.cm-content`; `await user.keyboard("{Control>}{End}{/Control}345")`; the editor text is `"SELECT 2345"` and `screen.getByLabelText("Value")` has the text `"SELECT 2345"` (no keystroke lost, and the sync does not undo them).
6. **error ranges past the end draw nothing**: `render(<Harness errorRange={{ from: 50, to: 60 }} />)`: it renders, and `document.querySelector(".cm-sql-error")` is null.
6b. **error ranges are clamped to the end** (a separate `it`): `render(<Harness errorRange={{ from: 2, to: 50 }} />)`: `waitFor` `.cm-sql-error` has the text `"LECT 1"`.
7. **Tab leaves the editor**: `render(<Harness />)`; focus `.cm-content`; `await userEvent.setup().tab()`; `screen.getByRole("button", { name: "After" })` has focus.

### `web/src/mocks/sql.test.ts`

1. "answers the example request with the example result": `fetch(new URL("/api/v1/projects/acme%2Fsupport-bot/sql", window.location.origin), { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(requestDocument) })` answers `200` and its JSON deep-equals `resultDocument`.
2. "refuses an empty query": the same with `{ query: "  " }` answers `400` with `code: "bad_request"`.

### `web/src/routes/$org/$project/sql/index.test.tsx`

1. **"lists SQL in the sidebar and opens the console"**: `const { router, user } = await renderApp("/acme/support-bot/traces")`; `await user.click(screen.getByRole("link", { name: "SQL" }))`; `waitFor` `router.state.location.pathname` is `/acme/support-bot/sql`; `findByRole("heading", { name: "SQL" })`; `getByText("Run a query to see results.")`; `getByRole("link", { name: "SQL" })` has `aria-current="page"`; the Run button has `title="Run (Ctrl↵)"`.
2. **"opens the console from the palette"**: `renderApp("/acme/support-bot/traces")`; `await screen.findByRole("grid", { name: "Traces" })`; `await user.keyboard("{Meta>}k{/Meta}")`; `const dialog = await screen.findByRole("dialog", { name: /Command palette/ })`; `await user.click(within(dialog).getByRole("option", { name: "SQL" }))`; `waitFor` the pathname is `/acme/support-bot/sql`.
3. **"runs the draft over the picked range"**: `const sql = captureSql();` `renderApp("/acme/support-bot/sql?range=1h")`; `await user.click(screen.getByRole("button", { name: "Run" }))`; in `waitFor`: `const body = single(sql.bodies)`; `body.query` is `DEFAULT_QUERY`; `Date.parse(String(body.to)) - Date.parse(String(body.from))` is `3_600_000`; `Object.keys(body).sort()` is `["from", "query", "to"]`; `single(sql.accepts)` is `"application/json"`. Then `findByRole("grid", { name: "Query results" })`; `getAllByRole("columnheader").map((header) => header.textContent?.trim())` equals `resultDocument.columns.map((column) => column.name)`; `await screen.findByText(new RegExp(\`^${resultDocument.rows.length} rows? · \\d+ ms$\`))`.
4. **"runs the stored draft with Ctrl+Enter"**: `storeDraft("SELECT 1")`; `const sql = captureSql();` render `/acme/support-bot/sql`; `const content = await waitFor(editorContent)`; `content.focus()`; `await user.keyboard("{Control>}{Enter}{/Control}")`; `waitFor` `single(sql.bodies).query` is `"SELECT 1"`.
5. **"saves typing to the draft"**: `storeDraft("")`; render; `const content = await waitFor(editorContent)`; `await user.type(content, "SELECT 7")`; `waitFor` `JSON.parse(localStorage.getItem("llmobs.sql.draft.acme/support-bot") ?? "null")` equals `{ query: "SELECT 7" }`.
6. **"restores the draft from localStorage"**: `storeDraft("SELECT 42")`; render; `waitFor` `editorContent().textContent` is `"SELECT 42"`.
7. **"keeps one draft per project"**: `storeDraft("SELECT 'bot'")`; `storeDraft("SELECT 'play'", "acme/playground")`; `const { router } = await renderApp("/acme/support-bot/sql")`; `waitFor` the editor text is `SELECT 'bot'`; `await router.navigate({ to: "/$org/$project/sql", params: { org: "acme", project: "playground" } })`; `waitFor` the editor text is `SELECT 'play'`.
8. **"does nothing for an empty query"**: `storeDraft("   ")`; `const sql = captureSql();` render; the Run button is disabled; focus the editor and press Ctrl+Enter; after `await new Promise((resolve) => setTimeout(resolve, 50))`: `sql.bodies` is empty, the Run button is still disabled, and `screen.queryByText("Running…")` is null.
9. **"shows Running… and blocks a second run"**: `let release = () => {}; const gate = new Promise<void>((resolve) => { release = resolve; });` `captureSql(async () => { await gate; return HttpResponse.json(resultDocument); })`; render; click Run; `findByRole("status")` has text `Running…`; the Run button is disabled; `release()`; `findByRole("grid", { name: "Query results" })`; `queryByText("Running…")` is null.
10. **"underlines the reported position"**: `storeDraft("SELECT 1\nFROM nme")`; `captureSql(() => HttpResponse.json({ type: "about:blank", title: "Bad Request", status: 400, code: "invalid_filter", detail: "invalid query", errors: [{ field: "query", message: "table 'nme' not found", position: { line: 2, column: 6 } }] }, { status: 400, headers: PROBLEM_JSON }))`; click Run; `findByRole("alert")` has text `table 'nme' not found` and `id="sql-error"`; `waitFor` `document.querySelector(".cm-sql-error")?.textContent` is `"nme"`; `editorContent()` has `aria-invalid="true"` and `aria-describedby="sql-error"`; `queryByRole("grid", { name: "Query results" })` is null. Then `editorContent().focus()`, `await user.keyboard("{Control>}{End}{/Control}x")`: `waitFor` `.cm-sql-error` is gone and `aria-invalid` is `"false"`.
11. **"shows an error without a position"**: answer `400` with the same problem but the `errors` entry `{ field: "query", message: "unknown function nope" }`; click Run; `findByRole("alert")` has text `unknown function nope`; `document.querySelector(".cm-sql-error")` is null.
12. **"shows a timeout message"**: answer `422` `{ type: "about:blank", title: "Unprocessable Entity", status: 422, code: "query_timeout", detail: "the query exceeded its time limit" }`; click Run; `findByRole("alert")` has text `the query exceeded its time limit`.
13. **"shows a generic message for a non-problem failure"**: answer `new HttpResponse("oops", { status: 500, headers: { "Content-Type": "text/plain" } })`; click Run; `findByRole("alert")` has text `The query could not be run`.
14. **"shows a generic message for an empty error body"**: answer `new HttpResponse(null, { status: 503 })`; click Run; `findByRole("alert")` has text `The query could not be run`; no grid.
15. **"replaces the result and clears the error on the next run"**: a gate as in test 9 and a counter handler: the first call answers the `query_timeout` problem of test 12; the second awaits the gate, then answers `resultDocument`. Click Run; `findByRole("alert")`; click Run again; while pending: `findByText("Running…")`, `queryByRole("alert")` is null and `queryByRole("grid", { name: "Query results" })` is null; `release()`; `findByRole("grid", { name: "Query results" })`; `queryByRole("alert")` is null.
16. **"renders NULL and JSON cells"**: answer `HttpResponse.json({ columns: [{ name: "a", type: "Utf8" }, { name: "n", type: "Int64" }, { name: "m", type: "Map(Utf8, Utf8)" }], rows: [{ a: null, n: 3, m: { k: "v" } }], truncated: true })`; click Run; `within(await screen.findByRole("grid", { name: "Query results" })).getAllByRole("gridcell").map((cell) => cell.textContent)` equals `["NULL", "3", '{"k":"v"}']`; `await screen.findByText(/^1 row \(truncated\) · \d+ ms$/)`.
17. **"changes the range in the URL"**: render `/acme/support-bot/sql`; `await user.click(screen.getByRole("button", { name: "Last 24 hours" }))`, then `await user.click(await screen.findByRole("button", { name: "Last 7 days" }))`; `waitFor` `router.state.location.search` equals `{ range: "7d" }`.

## Commands

```bash
export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH"
pnpm --dir web exec biome check --write .
just check-web
```

While iterating: `pnpm --dir web exec vitest run src/routes/\$org/\$project/sql src/lib/sql src/lib/search/sql.test.ts src/lib/shortcuts.test.ts src/components/sql src/components/command src/mocks/sql.test.ts`. Before committing: the same `PATH` line, `pnpm --dir web exec biome check --write .`, then `just check`.

## Done when

- Under `just web-dev`, `/acme/support-bot/sql` renders with the sidebar entry `SQL` highlighted, runs the default query against the browser worker's mock (this is the check that `browser.ts` registers `sqlHandlers`) and shows the example result.
- Every test above passes; `just check` passes; no `vi.mock` anywhere.
- The only change to `filter-bar/editor.tsx` is the `export` keyword.
- One commit, subject `web: add the SQL console page`, no trailer lines. Report "no new dependency".
