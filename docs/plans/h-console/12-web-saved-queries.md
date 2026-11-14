# H12 — Parameters, saved queries, examples and history

Commit subject: `web: add saved queries, parameters and history`.

See [README.md](README.md) for the scope, the decisions (D-H-6, D-H-7, D-H-13) and the conventions. H11 is done. Read its files first; this phase extends them. The names this phase relies on, all from H11:

| Name | Where | What |
|---|---|---|
| `SqlConsole({ search, onSearch })` | `web/src/components/sql/sql-console.tsx` | `onSearch(update)`, `update: (previous: SqlSearch) => SqlSearch` |
| `draft`, `updateDraft` | same, from `useSqlDraft(ref)` | `SqlDraft = { query }`; `updateDraft((previous) => next)` writes through to localStorage |
| `run`, `execute(text)`, `lastRun`, `badAnswer`, `range`, `schema` | same | the run mutation, the run function, `{ result, durationMs }`, the empty-answer flag, `rangeFromSearch(search)`, the schema query |
| `sqlRunBody({ query, from, to })` | `web/src/lib/sql/request.ts` | the request body |
| `SqlSearch`, `parseSqlSearch`, `withSqlRange` | `web/src/lib/search/sql.ts` | URL search: `range`, `from`, `to` |
| `SqlDraft`, `readDraft`, `DEFAULT_QUERY`, `draftKey`, `isRecord` | `web/src/lib/sql/draft.ts` | draft type and storage |
| `isSqlResult` | `web/src/lib/sql/format.ts` | JSON result guard |
| `single`, `editorContent`, `storeDraft(query, ref?)`, `captureSql(answer?)`, `PROBLEM_JSON` | `web/src/routes/$org/$project/sql/index.test.tsx` | test helpers; `captureSql` returns `{ bodies, accepts }` |

If any of these is missing or has another signature, STOP and report: H11 deviated.

H12 adds to the console:
- the `view` search param, the draft's parameter values and saved-query id, the `replace` navigation option, and a `TooltipProvider` around the console (for `DisabledReason`);
- a **parameters bar**: one input per `{name:Type}` placeholder of the editor text, sent as typed JSON values in `params`;
- a **saved queries menu**: saved views of kind `sql` (private or shared), loaded into the editor, the parameters and the range;
- an **examples menu** with the five queries of spec 05 § Examples shipped in the UI;
- a **history menu**: the last 50 runs of this project, in localStorage.

## Read first

- The H11 files listed above, and `web/src/routes/$org/$project/sql/index.tsx`.
- `web/src/components/saved-views/saved-views-menu.tsx` and its test (the menu this one is modelled on: its `patch`, `refresh`, `problemText`, dialogs and `onSelect` handling), `web/src/mocks/saved-views.ts` (the stateful mock every test already has), `fixtures/api/static/saved-views.json`, `web/src/test/fixtures.ts`.
- `web/src/api/queries.ts` (`savedViewsQuery`, `meQuery`), `web/src/lib/roles.ts` (`atLeast`), `web/src/components/disabled-reason.tsx`, `web/src/components/ui/tooltip.tsx`, `web/src/lib/search/common.ts` (`parseEnum`, `parseRfc3339`, `parseViewId`).
- `docs/spec/05-query.md` § Parameters and § Examples shipped in the UI; `crates/storage/src/sql/params.rs` (the server's placeholder rule) and `crates/core/src/time_serde.rs` (`parse_rfc3339`).

## Facts this phase relies on

- **Server placeholder rule** (spec 05, `params.rs`): the tokens `{`, an unquoted word, `:`, an unquoted word, `}`, with nothing between them, after tokenizing with sqlparser's `GenericDialect`. The type is `String`, `Int`, `Float`, `Bool` or `Timestamp`, case-insensitive; another type is a `400`. One name used twice binds one value; two types for one name is a `400`. Values: `String` a JSON string; `Int` a JSON integer; `Float` a JSON number; `Bool` a JSON boolean; `Timestamp` an RFC 3339 string or an integer of epoch milliseconds. A missing or extra parameter is a `400`.
- **`GenericDialect` lexing** (sqlparser 0.62), which the UI scanner mirrors:
  - a word starts with a Unicode alphabetic character, `_`, `#` or `@`, and continues with alphabetic characters, ASCII digits, `@`, `$`, `#` or `_`; keywords are words too, so `{select:Int}` is a placeholder named `select`;
  - `'…'` strings with `''` escapes; `"…"` and `` `…` `` quoted identifiers with doubled-quote escapes;
  - `-- …` line comments; `/* … */` block comments, which **nest**;
  - `$$…$$` and `$tag$…$tag$` dollar-quoted strings (a tag is alphanumeric or `_`); `$name` without a closing `$` is a placeholder token, which the server rejects.
  Not modelled: backslash escapes in `E'…'` strings and the `Q'…'` / `N'…'` / `X'…'` prefixes beyond their quote. The scanner shows no input for a placeholder with an unknown type or written with spaces (`{ a:Int }`); the server reports those when the query runs.
- **Server timestamps** (`parse_rfc3339`): the character at index 10 is `T` or `t`, at most 9 fractional digits, `Z`, `z` or `±HH:MM`, a real calendar date, seconds below 60. `2026-01-01` and `2026-01-01 00:00:00Z` are refused.
- **Saved views mock.** `web/src/test/setup.ts` installs `savedViewHandlers(seedSavedViews())` before every test. After H3 the seed (`fixtures/api/static/saved-views.json`) has one `sql` view: id `01900000-0000-7000-8000-000000000211`, name `Cost per model per day`, owner Ada (`ADA_ID`, the user of `me.json`), not shared, `state.query` = `SELECT model, date_trunc('day', start_time) AS day, sum(cost_total) AS cost FROM spans WHERE span_kind = 'llm' GROUP BY model, day ORDER BY day, cost DESC`, `state.params` = `{}`, `state.range` = `"24h"`. If the file still has `"params": []` or no `range`, STOP: H3 is not merged. Unit tests never read the seed: they pass literal states. The mock's POST sets `owner_user_id` to Ada; its GET filters by `kind`, ignores `limit`, answers `next_cursor: null`, and hides other users' private views; PATCH and DELETE work on the in-memory store. A test replaces the store with `server.use(...savedViewHandlers([...]))`.
- **Listing (D-H-13).** The console lists saved queries with `kind=sql&limit=1000`, the per-owner cap, in one request. The menu shows them in the order the server returns them (by id, H3); it does not re-sort.
- **Project role.** `useProject().data.role`; the fixture says `owner`. Compare with `atLeast(role, "member")` / `atLeast(role, "admin")`, never `===`. Ada is `owner`, so she counts as admin.
- **D-H-6 rules the UI mirrors** (the server enforces them; the UI hides or disables): any role saves a private query; sharing, and any change to a query that stays shared, needs member; an owner may stop sharing and delete their own query at any role; only the owner saves over or renames; a project admin may stop sharing or delete any shared query; stopping sharing of a shared query whose owner was deleted is a `409`, so the UI hides "Stop sharing" for an ownerless query (Delete stays). There is no default saved query.
- **Saved state (D-H-7).** `{ query, params: { name: string }, range }` or `{ query, params, from, to }`: one of the two, never both. Parameter values are the strings the user typed; only the parameters the query uses are saved. With no range in the URL, Save stores the effective preset `"24h"` (the picker's default).
- **Opening `?view=` (D-H-13).** The draft records the saved query it came from (`draft.view`). A `?view=` link replaces the draft with the saved query unless `draft.view` already equals that id, so unsaved edits survive a reload. Loading writes the editor through a CodeMirror transaction, so Ctrl+Z in the editor brings back the replaced text.
- `tsconfig.app.json` sets `noUncheckedIndexedAccess`. Read list items through `single(...)` or a checked local.
- Use `user.click` / `user.keyboard` for Radix menus and dialogs, never `fireEvent`. Seed the editor through localStorage where a test is not about typing (H11's precedent); type only where typing is the point.

## Edit only

- `web/src/lib/search/sql.ts` and `web/src/lib/search/sql.test.ts`
- `web/src/lib/sql/draft.ts` and `web/src/lib/sql/draft.test.ts`
- `web/src/lib/sql/params.ts` (new) and `web/src/lib/sql/params.test.ts` (new)
- `web/src/lib/sql/saved-state.ts` (new) and `web/src/lib/sql/saved-state.test.ts` (new)
- `web/src/lib/sql/examples.ts` (new) and `web/src/lib/sql/examples.test.ts` (new)
- `web/src/lib/sql/history.ts` (new) and `web/src/lib/sql/history.test.ts` (new)
- `web/src/lib/sql/request.ts` and `web/src/lib/sql/request.test.ts`
- `web/src/api/queries.ts` (append `savedQueriesQuery`)
- `web/src/components/sql/params-bar.tsx` (new)
- `web/src/components/sql/saved-queries-menu.tsx` (new)
- `web/src/components/sql/examples-menu.tsx` (new)
- `web/src/components/sql/history-menu.tsx` (new)
- `web/src/components/sql/sql-console.tsx`
- `web/src/routes/$org/$project/sql/index.tsx` (the `replace` option)
- `web/src/components/saved-views/saved-views-menu.tsx` (add `export` to `NameField` and `Flag`, nothing else)
- `web/src/routes/$org/$project/sql/index.test.tsx` (helpers and tests)

## Steps

### 1. URL — `web/src/lib/search/sql.ts`

1. Add `view?: string;` to `SqlSearch` with the doc comment `/** Saved query id. */`.
2. In `parseSqlSearch`, import `parseViewId` from `@/lib/search/common`, read `const view = parseViewId(search.view);` and add `...(view ? { view } : {})` to the result.
3. Append:
   ```ts
   /** `search` without `view`. */
   export function withoutView(search: SqlSearch): SqlSearch {
     const { view: _view, ...rest } = search;
     return rest;
   }
   ```

`withSqlRange` already keeps `view` (it spreads the rest).

### 2. Draft — `web/src/lib/sql/draft.ts`

1. `SqlDraft` becomes:
   ```ts
   export type SqlDraft = {
     query: string;
     /** Parameter values as typed, by name (D-H-7). */
     params: Record<string, string>;
     /** The saved query this draft was loaded from (D-H-13). */
     view?: string;
   };
   ```
2. Every default `{ query: DEFAULT_QUERY }` becomes `{ query: DEFAULT_QUERY, params: {} }`.
3. In `readDraft`, after the `query` check, replace `return { query: parsed.query };` with:
   ```ts
   const params: Record<string, string> = {};
   if (isRecord(parsed.params)) {
     for (const [name, value] of Object.entries(parsed.params)) {
       if (typeof value === "string") {
         params[name] = value;
       }
     }
   }
   const view = typeof parsed.view === "string" ? parsed.view : undefined;
   return view ? { query: parsed.query, params, view } : { query: parsed.query, params };
   ```

H11's page test helper `storeDraft(query)` stores `{ query }`; `readDraft` reads that as `{ query, params: {} }`. Tests that need params or a view id write the JSON themselves. H11's page test "saves typing to the draft" now expects `{ query: "SELECT 7", params: {} }`; update that expectation in this phase.

### 3. Placeholders and values — `web/src/lib/sql/params.ts`

```ts
export const PARAM_TYPES = ["String", "Int", "Float", "Bool", "Timestamp"] as const;
export type ParamType = (typeof PARAM_TYPES)[number];
export type SqlParam = { name: string; type: ParamType };
export type ParamValue = string | number | boolean;

const WORD_START = /[\p{Alphabetic}_#@]/u;
const WORD_PART = /[\p{Alphabetic}0-9@$#_]/u;
const TAG_PART = /[\p{Alphabetic}\p{N}_]/u;
const WORD = "[\\p{Alphabetic}_#@][\\p{Alphabetic}0-9@$#_]*";
const PLACEHOLDER = new RegExp(`\\{(${WORD}):(${WORD})\\}`, "uy");
const FLOAT = /^-?\d+(\.\d+)?([eE][-+]?\d+)?$/;
const INTEGER = /^-?\d+$/;
const TIMESTAMP =
  /^(\d{4})-(\d{2})-(\d{2})[Tt](\d{2}):(\d{2}):(\d{2})(\.\d{1,9})?([Zz]|[+-](\d{2}):(\d{2}))$/;

/** The code point at `index` as a string (one or two UTF-16 units). */
function charAt(text: string, index: number): string {
  const code = text.codePointAt(index);
  return code === undefined ? "" : String.fromCodePoint(code);
}

function canonicalType(text: string): ParamType | undefined {
  return PARAM_TYPES.find((type) => type.toLowerCase() === text.toLowerCase());
}

/**
 * Distinct placeholders outside strings, quoted identifiers, dollar-quoted strings and
 * comments, in order of first use. The first type of a name wins.
 */
export function sqlParams(text: string): SqlParam[] {
  const found: SqlParam[] = [];
  let index = 0;
  while (index < text.length) {
    const char = charAt(text, index);
    const next = text[index + 1];
    if (char === "-" && next === "-") {
      const end = text.indexOf("\n", index + 2);
      index = end < 0 ? text.length : end + 1;
    } else if (char === "/" && next === "*") {
      index = skipBlockComment(text, index);
    } else if (char === "'" || char === '"' || char === "`") {
      index = skipQuoted(text, index, char);
    } else if (char === "$") {
      index = skipDollar(text, index);
    } else if (char === "{") {
      PLACEHOLDER.lastIndex = index;
      const match = PLACEHOLDER.exec(text);
      if (match) {
        const name = match[1] ?? "";
        const type = canonicalType(match[2] ?? "");
        if (type && !found.some((param) => param.name === name)) {
          found.push({ name, type });
        }
        index += match[0].length;
      } else {
        index += 1;
      }
    } else if (WORD_START.test(char)) {
      index = skipWhile(text, index + char.length, WORD_PART);
    } else {
      index += char.length;
    }
  }
  return found;
}

function skipWhile(text: string, start: number, pattern: RegExp): number {
  let index = start;
  while (index < text.length) {
    const char = charAt(text, index);
    if (!pattern.test(char)) {
      break;
    }
    index += char.length;
  }
  return index;
}

/** Nested block comments, as GenericDialect reads them. Unterminated: the end. */
function skipBlockComment(text: string, start: number): number {
  let depth = 1;
  let index = start + 2;
  while (index < text.length) {
    if (text.startsWith("/*", index)) {
      depth += 1;
      index += 2;
    } else if (text.startsWith("*/", index)) {
      depth -= 1;
      index += 2;
      if (depth === 0) {
        return index;
      }
    } else {
      index += 1;
    }
  }
  return text.length;
}

/** Index after the closing quote; a doubled quote is an escaped one. Unterminated: the end. */
function skipQuoted(text: string, start: number, quote: string): number {
  let index = start + 1;
  while (index < text.length) {
    if (text[index] === quote) {
      if (text[index + 1] === quote) {
        index += 2;
        continue;
      }
      return index + 1;
    }
    index += 1;
  }
  return text.length;
}

/** `$$…$$`, `$tag$…$tag$`; a `$name` with no closing `$` is a one-token placeholder. */
function skipDollar(text: string, start: number): number {
  const tagEnd = skipWhile(text, start + 1, TAG_PART);
  if (text[tagEnd] !== "$") {
    return tagEnd;
  }
  const delimiter = text.slice(start, tagEnd + 1);
  const close = text.indexOf(delimiter, tagEnd + 1);
  return close < 0 ? text.length : close + delimiter.length;
}

/** The server's `parse_rfc3339` rule (crates/core/src/time_serde.rs). */
export function isServerTimestamp(text: string): boolean {
  const match = TIMESTAMP.exec(text);
  if (!match) {
    return false;
  }
  const [year, month, day, hour, minute, second] = match.slice(1, 7).map(Number);
  const offsetHour = match[9] === undefined ? 0 : Number(match[9]);
  const offsetMinute = match[10] === undefined ? 0 : Number(match[10]);
  if (
    year === undefined ||
    month === undefined ||
    day === undefined ||
    hour === undefined ||
    minute === undefined ||
    second === undefined
  ) {
    return false;
  }
  if (month < 1 || month > 12 || hour > 23 || minute > 59 || second > 59) {
    return false;
  }
  if (offsetHour > 23 || offsetMinute > 59) {
    return false;
  }
  const date = new Date(Date.UTC(year, month - 1, day));
  return date.getUTCFullYear() === year && date.getUTCMonth() === month - 1 && date.getUTCDate() === day;
}

export const INT_RANGE = "Enter an integer between -9007199254740991 and 9007199254740991";

/** Typed JSON values for `params`, or a message per invalid name. */
export function paramValues(
  params: readonly SqlParam[],
  typed: Readonly<Record<string, string>>,
): { values: Record<string, ParamValue>; errors: Record<string, string> } {
  const values: Record<string, ParamValue> = {};
  const errors: Record<string, string> = {};
  for (const { name, type } of params) {
    const raw = typed[name] ?? "";
    const text = raw.trim();
    switch (type) {
      case "String":
        values[name] = raw;
        break;
      case "Int":
        if (!INTEGER.test(text)) {
          errors[name] = "Enter an integer";
        } else if (!Number.isSafeInteger(Number(text))) {
          errors[name] = INT_RANGE;
        } else {
          values[name] = Number(text);
        }
        break;
      case "Float":
        if (FLOAT.test(text) && Number.isFinite(Number(text))) {
          values[name] = Number(text);
        } else {
          errors[name] = "Enter a number";
        }
        break;
      case "Bool":
        if (text === "true" || text === "false") {
          values[name] = text === "true";
        } else {
          errors[name] = "Choose true or false";
        }
        break;
      case "Timestamp":
        if (INTEGER.test(text) && Number.isSafeInteger(Number(text))) {
          values[name] = Number(text);
        } else if (isServerTimestamp(text)) {
          values[name] = text;
        } else {
          errors[name] = "Enter an RFC 3339 time or epoch milliseconds";
        }
        break;
    }
  }
  return { values, errors };
}

/** The typed strings of the parameters `text` uses, `""` for one never typed. */
export function usedParams(
  text: string,
  typed: Readonly<Record<string, string>>,
): Record<string, string> {
  return Object.fromEntries(sqlParams(text).map((param) => [param.name, typed[param.name] ?? ""]));
}
```

A `String` parameter accepts any text, including the empty string, and is sent untrimmed.

### 4. Request body — `web/src/lib/sql/request.ts`

Replace the function with:

```ts
import type { Schema } from "@/api/types";
import type { ParamValue } from "@/lib/sql/params";

/**
 * `POST /sql` body. `params` only when the query has placeholders; `limit` and
 * `timeout_ms` stay server defaults.
 */
export function sqlRunBody(input: {
  query: string;
  from: string;
  to: string;
  params?: Record<string, ParamValue>;
}): Schema<"SqlQueryRequest"> {
  const params = input.params ?? {};
  return {
    query: input.query,
    from: input.from,
    to: input.to,
    ...(Object.keys(params).length > 0
      ? { params: params as Schema<"SqlQueryRequest">["params"] }
      : {}),
  };
}
```

### 5. Saved state — `web/src/lib/sql/saved-state.ts`

```ts
import type { Schema } from "@/api/types";
import { parseEnum, parseRfc3339 } from "@/lib/search/common";
import type { SqlSearch } from "@/lib/search/sql";
import { isRecord, type SqlDraft } from "@/lib/sql/draft";
import { usedParams } from "@/lib/sql/params";
import { PRESETS, type Preset } from "@/lib/time-range";

/** The picker's default when the URL has no range (`rangeFromSearch`). */
const DEFAULT_PRESET: Preset = "24h";

/** A `sql` saved view's state (D-H-7): a range or an absolute pair, never both. */
export type SqlSavedState = {
  query: string;
  params: Record<string, string>;
  range?: Preset;
  from?: string;
  to?: string;
};

/** What "Save" stores: the query, the parameters it uses, and the effective range. */
export function savedStateOf(draft: SqlDraft, search: SqlSearch): SqlSavedState {
  const base = { query: draft.query, params: usedParams(draft.query, draft.params) };
  if (search.from && search.to) {
    return { ...base, from: search.from, to: search.to };
  }
  return { ...base, range: search.range ?? DEFAULT_PRESET };
}

/** Reads a stored state leniently. Number and boolean params become strings. */
export function parseSavedState(state: unknown): SqlSavedState {
  const record = isRecord(state) ? state : {};
  const params: Record<string, string> = {};
  if (isRecord(record.params)) {
    for (const [name, value] of Object.entries(record.params)) {
      if (typeof value === "string") {
        params[name] = value;
      } else if (typeof value === "number" || typeof value === "boolean") {
        params[name] = String(value);
      }
    }
  }
  const query = typeof record.query === "string" ? record.query : "";
  const range = parseEnum(record.range, PRESETS);
  const from = parseRfc3339(record.from);
  const to = parseRfc3339(record.to);
  if (from !== undefined && to !== undefined && Date.parse(from) < Date.parse(to)) {
    return { query, params, from, to };
  }
  return range ? { query, params, range } : { query, params };
}

/** A stable key for a params map, independent of key order. */
export function paramsKey(params: Record<string, string>): string {
  return JSON.stringify(
    Object.keys(params)
      .sort()
      .map((name) => [name, params[name]]),
  );
}

/**
 * Whether `current` (from `savedStateOf`) matches `saved`. The range counts only when
 * `saved` stores one: an older query saved without a range matches under any range.
 */
export function sameSavedState(current: SqlSavedState, saved: SqlSavedState): boolean {
  if (current.query !== saved.query || paramsKey(current.params) !== paramsKey(saved.params)) {
    return false;
  }
  if (!saved.range && !saved.from) {
    return true;
  }
  return (
    current.range === saved.range && current.from === saved.from && current.to === saved.to
  );
}

/** The search a loaded state opens with. Without a stored range, the current one stays. */
export function searchOfSavedState(
  state: SqlSavedState,
  current: SqlSearch,
  viewId: string,
): SqlSearch {
  if (state.from && state.to) {
    return { from: state.from, to: state.to, view: viewId };
  }
  if (state.range) {
    return { range: state.range, view: viewId };
  }
  const { view: _view, ...rest } = current;
  return { ...rest, view: viewId };
}

/**
 * The state as the API's opaque object. `SqlSavedState` has no index signature, so a
 * direct cast is TS2352; going through `unknown` is the intended escape (the server only
 * checks that it is an object, D-H-7).
 */
export function storedState(state: SqlSavedState): Schema<"CreateSavedView">["state"] {
  return state as unknown as Schema<"CreateSavedView">["state"];
}
```

If `Schema<"UpdateSavedView">["state"]` is not assignable from `Schema<"CreateSavedView">["state"]` in `tsc`, add `storedUpdateState` with the same body and that return type.

### 6. Examples — `web/src/lib/sql/examples.ts`

The five examples of spec 05, over the real columns of `fixtures/api/static/schema.json` (`scores.source` is `llm_judge` for LLM judges, `ScoreSource` in `llmobs-core`):

```ts
export type SqlExample = { name: string; query: string };

export const SQL_EXAMPLES: readonly SqlExample[] = [
  {
    name: "Cost per model per day",
    query: [
      "SELECT date_trunc('day', start_time) AS day, model, sum(cost_total) AS cost",
      "FROM spans",
      "WHERE span_kind = 'llm' AND model IS NOT NULL",
      "GROUP BY day, model",
      "ORDER BY day, cost DESC",
    ].join("\n"),
  },
  {
    name: "LLM latency percentiles per model",
    query: [
      "SELECT model,",
      "  approx_percentile_cont(duration_ms, 0.5) AS p50_ms,",
      "  approx_percentile_cont(duration_ms, 0.95) AS p95_ms,",
      "  approx_percentile_cont(duration_ms, 0.99) AS p99_ms,",
      "  count(*) AS calls",
      "FROM spans",
      "WHERE span_kind = 'llm' AND model IS NOT NULL",
      "GROUP BY model",
      "ORDER BY calls DESC",
    ].join("\n"),
  },
  {
    name: "Traces with the most tool calls",
    query: [
      "SELECT trace_id, count(*) AS tool_calls",
      "FROM spans",
      "WHERE span_kind = 'tool'",
      "GROUP BY trace_id",
      "ORDER BY tool_calls DESC",
      "LIMIT 50",
    ].join("\n"),
  },
  {
    name: "Average judge score per release",
    query: [
      "SELECT t.release, s.name, avg(s.value_num) AS avg_score, count(*) AS scores",
      "FROM scores s",
      "JOIN traces t ON s.trace_id = t.trace_id",
      "WHERE s.source = 'llm_judge' AND s.value_num IS NOT NULL",
      "GROUP BY t.release, s.name",
      "ORDER BY t.release, s.name",
    ].join("\n"),
  },
  {
    name: "Users by spend",
    query: [
      "SELECT user_id, count(*) AS traces, sum(cost_total) AS cost",
      "FROM traces",
      "WHERE user_id IS NOT NULL",
      "GROUP BY user_id",
      "ORDER BY cost DESC",
      "LIMIT 50",
    ].join("\n"),
  },
];
```

### 7. History — `web/src/lib/sql/history.ts`

```ts
import { STORAGE_PREFIX } from "@/product";
import { isRecord } from "@/lib/sql/draft";
import { paramsKey } from "@/lib/sql/saved-state";

export const HISTORY_MAX = 50;

export type HistoryEntry = {
  query: string;
  /** Typed strings of the parameters the query uses. */
  params: Record<string, string>;
  /** RFC 3339, when the run ended. */
  ranAt: string;
  /** Rows returned; absent when the run failed. */
  rows?: number;
};

export function historyKey(projectRef: string): string {
  return `${STORAGE_PREFIX}sql.history.${projectRef}`;
}

function entryOf(item: unknown): HistoryEntry[] {
  if (!isRecord(item) || typeof item.query !== "string" || typeof item.ranAt !== "string") {
    return [];
  }
  const params: Record<string, string> = {};
  if (isRecord(item.params)) {
    for (const [name, value] of Object.entries(item.params)) {
      if (typeof value === "string") {
        params[name] = value;
      }
    }
  }
  const rows = typeof item.rows === "number" ? { rows: item.rows } : {};
  return [{ query: item.query, params, ranAt: item.ranAt, ...rows }];
}

export function readHistory(projectRef: string): HistoryEntry[] {
  try {
    const raw = localStorage.getItem(historyKey(projectRef));
    const parsed: unknown = raw === null ? [] : JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.flatMap(entryOf) : [];
  } catch {
    return [];
  }
}

/** Newest first. A run equal to the newest entry replaces it. At most `HISTORY_MAX`. */
export function pushHistory(projectRef: string, entry: HistoryEntry): HistoryEntry[] {
  const current = readHistory(projectRef);
  const newest = current[0];
  const same =
    newest !== undefined &&
    newest.query === entry.query &&
    paramsKey(newest.params) === paramsKey(entry.params);
  const next = [entry, ...(same ? current.slice(1) : current)].slice(0, HISTORY_MAX);
  try {
    localStorage.setItem(historyKey(projectRef), JSON.stringify(next));
  } catch {
    // Private mode or a full quota: history is a convenience.
  }
  return next;
}

export function clearHistory(projectRef: string): void {
  try {
    localStorage.removeItem(historyKey(projectRef));
  } catch {
    // Nothing to clear.
  }
}

/** The first non-empty line, cut to 60 characters with an ellipsis. */
export function historyLabel(query: string): string {
  const line = query.split("\n").find((item) => item.trim().length > 0)?.trim() ?? "";
  return line.length > 60 ? `${line.slice(0, 59)}…` : line;
}
```

### 8. Saved queries list — `web/src/api/queries.ts`

Append next to `savedViewsQuery`:

```ts
/** Saved SQL queries, all in one page (D-H-13: 1,000 is the per-owner cap). */
export function savedQueriesQuery(org: string, project: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/saved-views", {
    params: {
      path: { project: projectRef(org, project) },
      query: { kind: "sql", limit: 1000 },
    },
  });
}
```

### 9. Parameters bar — `web/src/components/sql/params-bar.tsx`

```tsx
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import type { SqlParam } from "@/lib/sql/params";

const PLACEHOLDERS: Partial<Record<SqlParam["type"], string>> = {
  Int: "42",
  Float: "0.5",
  Timestamp: "2026-01-01T00:00:00Z",
};

export function ParamsBar({
  params,
  values,
  errors,
  onChange,
}: {
  params: readonly SqlParam[];
  values: Readonly<Record<string, string>>;
  /** Set by a run attempt; the console clears a name's error when it changes. */
  errors: Readonly<Record<string, string>>;
  onChange: (name: string, value: string) => void;
}) {
  if (params.length === 0) {
    return null;
  }
  return (
    <fieldset className="flex shrink-0 flex-wrap items-start gap-3">
      <legend className="sr-only">Parameters</legend>
      {params.map((param) => {
        const id = `sql-param-${param.name}`;
        const error = errors[param.name];
        const value = values[param.name] ?? "";
        const described = error
          ? { "aria-invalid": true, "aria-describedby": `${id}-error` }
          : {};
        return (
          <div key={param.name} className="flex flex-col gap-1">
            <Label htmlFor={id} className="font-mono text-xs">
              {`${param.name} (${param.type})`}
            </Label>
            {param.type === "Bool" ? (
              <select
                id={id}
                {...described}
                className="focus-ring h-7 rounded-md border border-border bg-background px-1.5 text-xs"
                value={value}
                onChange={(event) => onChange(param.name, event.target.value)}
              >
                <option value="">—</option>
                <option value="true">true</option>
                <option value="false">false</option>
              </select>
            ) : (
              <Input
                id={id}
                {...described}
                value={value}
                className="h-7 w-48 font-mono text-xs"
                placeholder={PLACEHOLDERS[param.type] ?? ""}
                onChange={(event) => onChange(param.name, event.target.value)}
              />
            )}
            {error ? (
              <p id={`${id}-error`} className="text-destructive text-xs">
                {error}
              </p>
            ) : null}
          </div>
        );
      })}
    </fieldset>
  );
}
```

A parameter id is built from its name; names with `#` or `@` still make valid ids, and `htmlFor` matches them exactly.

### 10. Shared menu pieces — `web/src/components/saved-views/saved-views-menu.tsx`

Change `function NameField(` to `export function NameField(` and `function Flag(` to `export function Flag(`. Nothing else in that file changes.

### 11. Saved queries menu — `web/src/components/sql/saved-queries-menu.tsx`

Imports: `useQuery`, `useQueryClient` from `@tanstack/react-query`; `Bookmark`, `Check`, `Users` from `lucide-react`; `useState` from `react`; `api` from `@/api/client`; `problemOf` from `@/api/problem`; `projectRef` from `@/api/project-ref`; `meQuery`, `savedQueriesQuery` from `@/api/queries`; `type Schema` from `@/api/types`; `DisabledReason` from `@/components/disabled-reason`; `Flag`, `NameField` from `@/components/saved-views/saved-views-menu`; `useProject`; `Button`; `Dialog`, `DialogContent`, `DialogDescription`, `DialogFooter`, `DialogHeader`, `DialogTitle`; `DropdownMenu`, `DropdownMenuContent`, `DropdownMenuItem`, `DropdownMenuSeparator`, `DropdownMenuTrigger`; `Label`; `Switch`; `atLeast` from `@/lib/roles`; `parseSavedState`, `sameSavedState`, `type SqlSavedState`, `storedState` from `@/lib/sql/saved-state`.

Props:

```tsx
export function SavedQueriesMenu({
  viewId,
  current,
  onLoad,
  onSaved,
  onCleared,
}: {
  /** `search.view`. */
  viewId: string | undefined;
  /** What "Save" would store now: `savedStateOf(draft, search)`. */
  current: SqlSavedState;
  /** Load a saved query into the console. */
  onLoad: (view: Schema<"SavedView">) => void;
  /** A query was created from the current draft; the console records its id. */
  onSaved: (view: Schema<"SavedView">) => void;
  /** The loaded query was deleted. */
  onCleared: () => void;
})
```

Inside, in this order (every hook before any return):

```ts
const { org, project, data } = useProject();
const queryClient = useQueryClient();
const views = useQuery(savedQueriesQuery(org, project));
const me = useQuery(meQuery);
const [dialog, setDialog] = useState<"save-as" | "rename" | "delete" | null>(null);
const [name, setName] = useState("");
const [shared, setShared] = useState(false);
const [problem, setProblem] = useState<string | undefined>();

const loaded = views.data?.data.find((view) => view.id === viewId);
const owned = loaded !== undefined && loaded.owner_user_id === me.data?.user.id;
const canShare = atLeast(data.role, "member");
const isAdmin = atLeast(data.role, "admin");
/** A viewer who owns a shared query cannot change it (D-H-6). */
const editBlocked = owned && loaded?.shared === true && !canShare;
const ownerless = loaded !== undefined && loaded.owner_user_id === undefined;
const modified = loaded !== undefined && !sameSavedState(current, parseSavedState(loaded.state));

async function refresh(): Promise<void> {
  await queryClient.invalidateQueries({ queryKey: savedQueriesQuery(org, project).queryKey });
}

async function patch(
  id: string,
  body: { name?: string; state?: Schema<"UpdateSavedView">["state"]; shared?: boolean },
): Promise<boolean> {
  setProblem(undefined);
  const { error } = await api.PATCH("/api/v1/projects/{project}/saved-views/{id}", {
    params: { path: { project: projectRef(org, project), id } },
    body,
  });
  if (error) {
    setProblem(problemText(error));
    return false;
  }
  await refresh();
  return true;
}

function openDialog(next: "save-as" | "rename" | "delete") {
  setProblem(undefined);
  setName(next === "rename" ? (loaded?.name ?? "") : "");
  setShared(false);
  setDialog(next);
}

function closeDialog() {
  setDialog(null);
  setProblem(undefined);
}
```

`problemText(error)` is a module function like `SavedViewsMenu`'s: the problem's `detail`, else its `title`, else `"Could not save the query"`. Every `Dialog` gets `onOpenChange={(open) => { if (!open) closeDialog(); }}` and every "Cancel" button calls `closeDialog()`, so a dialog's problem never survives into the menu.

Trigger:

```tsx
<Button type="button" variant="outline" size="xs" className="gap-1.5">
  <Bookmark />
  {loaded?.name ?? "Saved queries"}
  {modified ? (
    <span
      role="status"
      aria-label="Unsaved changes"
      className="size-1.5 shrink-0 rounded-full bg-primary"
    />
  ) : null}
</Button>
```

Menu content (`<DropdownMenuContent align="end" className="min-w-56">`), in order. "Keeps the menu open" means `onSelect={(event) => { event.preventDefault(); void …; }}`, as `SavedViewsMenu` does for Save, so a failure shows in the menu.
1. `problem` as `<p role="alert" className="px-2 py-1 text-xs text-destructive">` when set.
2. One `DropdownMenuItem` per view of `views.data?.data`, in the server's order, `onSelect={() => onLoad(view)}`: the name in `<span className="min-w-0 flex-1 truncate">`, `<Users role="img" aria-hidden={false} aria-label="Shared" className="size-3" />` when shared, `<Check className="size-3" />` when `view.id === viewId`. When the list is empty: `<DropdownMenuItem disabled>No saved queries</DropdownMenuItem>`.
3. `DropdownMenuSeparator`.
4. "Save" — when `owned && loaded`; keeps the menu open; `patch(loaded.id, { state: storedState(current) })`. When `editBlocked` it is `disabled` and holds a second line `<span className="text-muted-foreground text-xs">Changing a shared query needs the member role</span>`.
5. "Save as…" — always; `openDialog("save-as")`.
6. "Rename…" — when `owned && loaded`; `openDialog("rename")`; disabled with the same second line when `editBlocked`.
7. "Share with project" — when `owned && loaded && !loaded.shared`; keeps the menu open; `patch(loaded.id, { shared: true })`. When `!canShare`: `disabled`, second line `Needs the member role`.
8. "Stop sharing" — when `loaded?.shared && !ownerless && (owned || isAdmin)`; keeps the menu open; `patch(loaded.id, { shared: false })`.
9. "Delete" — when `loaded && (owned || (isAdmin && loaded.shared))` (an owner may delete at any role); `openDialog("delete")`.
10. "Discard changes" — when `modified && loaded`; `onLoad(loaded)`.

If `tsc` reports that `storedState`'s return type is not assignable to `Schema<"UpdateSavedView">["state"]`, use the `storedUpdateState` variant of step 5.

Dialogs (same structure and classes as `SavedViewsMenu`'s):
- **Save as** — `DialogTitle` "Save query", `DialogDescription` "Save the query, its parameters and the time range.", `<NameField name={name} onName={setName} />`, then the share control: when `canShare`, `<Flag id="query-share" label="Share with project" checked={shared} onChecked={setShared} />`; otherwise
  ```tsx
  <DisabledReason reason="Sharing needs the member role">
    <span className="flex items-center gap-2">
      <Switch id="query-share" size="sm" checked={false} disabled />
      <Label htmlFor="query-share">Share with project</Label>
    </span>
  </DisabledReason>
  ```
  then `{problem ? <p className="text-xs text-destructive">{problem}</p> : null}`, and the buttons "Cancel" and "Save query" (disabled while `name.trim()` is empty). "Save query":
  ```ts
  setProblem(undefined);
  const { data: created, error } = await api.POST("/api/v1/projects/{project}/saved-views", {
    params: { path: { project: projectRef(org, project) } },
    body: { kind: "sql", name: name.trim(), state: storedState(current), shared, is_default: false },
  });
  if (error || !created) {
    setProblem(problemText(error));
    return;
  }
  await refresh();
  setDialog(null);
  onSaved(created);
  ```
  A `409 conflict` (the 1,000 cap) or a `400` shows its `detail` in the dialog, which stays open.
- **Rename** — title "Rename query", description "Change the name shown in the saved queries menu.", `NameField`, buttons "Cancel" / "Rename" (disabled while the trimmed name is empty). On success (`patch` returned `true`) the dialog closes and the trigger shows the new name after the refresh; on failure it stays open with the problem.
- **Delete** — title "Delete query", description `Delete ${loaded?.name ?? "this query"}? This cannot be undone.`, buttons "Cancel" / "Delete query" (`variant="destructive"`); `api.DELETE("/api/v1/projects/{project}/saved-views/{id}", { params: { path: { project: projectRef(org, project), id: loaded.id } } })`; on success `await refresh()`, close, `onCleared()`; on failure the problem shows in the dialog, which stays open.

### 12. Examples menu — `web/src/components/sql/examples-menu.tsx`

```tsx
export function ExamplesMenu({ onPick }: { onPick: (example: SqlExample) => void })
```

A `DropdownMenu` whose trigger is `<Button type="button" variant="outline" size="xs"><BookOpen />Examples</Button>` and whose items are `SQL_EXAMPLES` by `name`, each `onSelect={() => onPick(example)}`.

### 13. History menu — `web/src/components/sql/history-menu.tsx`

```tsx
export function HistoryMenu({
  projectRef,
  onPick,
}: {
  projectRef: string;
  onPick: (entry: HistoryEntry) => void;
})
```

- `const [entries, setEntries] = useState<HistoryEntry[]>([]);`. The `DropdownMenu` gets `onOpenChange={(open) => { if (open) { setEntries(readHistory(projectRef)); } }}`, so it reads localStorage each time it opens.
- Trigger: `<Button type="button" variant="outline" size="xs"><Clock />History</Button>`.
- Items: one per entry, `key={\`${entry.ranAt}-${index}\`}`, `title={entry.query}`, `onSelect={() => onPick(entry)}`, with `<span className="min-w-0 flex-1 truncate font-mono">{historyLabel(entry.query)}</span>` and `<span className="text-muted-foreground">{entry.rows === undefined ? "failed" : \`${entry.rows} ${entry.rows === 1 ? "row" : "rows"}\`}</span>`.
- Empty: `<DropdownMenuItem disabled>No runs yet</DropdownMenuItem>`.
- With entries: a separator, then "Clear history": `clearHistory(projectRef); setEntries([]);`.

### 14. Route — `web/src/routes/$org/$project/sql/index.tsx`

`onSearch` takes a second argument and passes `replace`:

```tsx
      onSearch={(update, options) => {
        void navigate({
          to: "/$org/$project/sql",
          params: { org, project },
          search: (previous) => update(previous),
          replace: options?.replace === true,
        });
      }}
```

### 15. Console — `web/src/components/sql/sql-console.tsx`

Keep H11's code; make these changes.

1. The `onSearch` prop type becomes:
   ```ts
   /**
    * Changes the URL search. Always given the function form, like `navigate({ search })`.
    * `replace` replaces the history entry instead of pushing one.
    */
   onSearch: (
     update: (previous: SqlSearch) => SqlSearch,
     options?: { replace?: boolean },
   ) => void;
   ```
2. Wrap the returned tree in `<TooltipProvider>…</TooltipProvider>` (import from `@/components/ui/tooltip`).
3. Imports to add: `useEffect`, `useMemo`, `useRef` from `react`; `savedQueriesQuery` from `@/api/queries`; `ParamsBar`, `SavedQueriesMenu`, `ExamplesMenu`, `HistoryMenu`; `paramValues`, `sqlParams`, `usedParams` from `@/lib/sql/params`; `parseSavedState`, `savedStateOf`, `searchOfSavedState` from `@/lib/sql/saved-state`; `pushHistory` from `@/lib/sql/history`; `withoutView` from `@/lib/search/sql`.
4. New hooks, after H11's:
   ```ts
   const params = useMemo(() => sqlParams(draft.query), [draft.query]);
   const [paramErrors, setParamErrors] = useState<Record<string, string>>({});
   const savedQueries = useQuery(savedQueriesQuery(org, project));
   const listed = savedQueries.data?.data;
   const complete = savedQueries.isSuccess && savedQueries.data.next_cursor === null;
   /** The draft's saved-query id, read by the `?view=` effect without re-running it. */
   const draftView = useRef(draft.view);
   draftView.current = draft.view;
   ```
5. Loading:
   ```ts
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
   ```
6. `?view=` handling (D-H-13). The effect depends on the URL and the list only, and reads `draft.view` through the ref: if it depended on `draft.view`, a Save as (which sets `draft.view` to the new id one render before the URL catches up) would reload the old query over the user's text.
   ```ts
   // biome-ignore lint/correctness/useExhaustiveDependencies: load and forgetView are recreated every render; draftView is a ref
   useEffect(() => {
     const id = search.view;
     if (!id || !savedQueries.isSuccess) {
       return;
     }
     const view = listed?.find((item) => item.id === id);
     if (!view) {
       if (complete) {
         forgetView();
       }
       return;
     }
     if (draftView.current !== id) {
       load(view);
     }
   }, [search.view, savedQueries.isSuccess, listed, complete]);
   ```
   The comment sits directly above the `useEffect`, as written. A view deleted elsewhere disappears from the refetched list, so this effect also drops it from the URL. A list that is not complete (a `next_cursor`) never drops `view`: the query may be on a later page.
7. Below the toolbar, before the body: `{savedQueries.isError ? <p role="alert" className="px-3 py-1 text-destructive text-xs">Saved queries could not be loaded.</p> : null}`. A failed list keeps `?view=` and the draft untouched.
8. Body building, shared by Run (and by H13's Export):
   ```ts
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
   ```
9. `execute(text)` becomes:
   ```ts
   function execute(text: string) {
     if (text.trim().length === 0 || run.isPending) {
       return;
     }
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
   ```
10. The editor's `onChange` also drops parameter errors whose name is gone:
    ```ts
    onChange={(text) => {
      updateDraft((previous) => ({ ...previous, query: text }));
      const names = new Set(sqlParams(text).map((param) => param.name));
      setParamErrors((previous) =>
        Object.fromEntries(Object.entries(previous).filter(([name]) => names.has(name))),
      );
    }}
    ```
11. Toolbar: after the `TimeRangePicker`, before the `ml-auto` group:
    ```tsx
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
        updateDraft((previous) => ({ ...previous, query: entry.query, params: entry.params }));
      }}
    />
    ```
    Examples and history keep `draft.view`: the result is an edit of the loaded query, and the trigger's "Unsaved changes" dot says so. H13 puts its export menu after these three.
12. Above the editor:
    ```tsx
    <ParamsBar
      params={params}
      values={draft.params}
      errors={paramErrors}
      onChange={(name, value) => {
        updateDraft((previous) => ({ ...previous, params: { ...previous.params, [name]: value } }));
        setParamErrors((previous) => {
          const { [name]: _cleared, ...rest } = previous;
          return rest;
        });
      }}
    />
    ```
13. Loading a saved query, an example or a history entry while the editor holds unsaved edits replaces them without asking; Ctrl+Z in the editor brings the previous text back (the replacement is a CodeMirror transaction).

## States

| State | What shows |
|---|---|
| No placeholders | No parameters bar |
| Placeholders | One labelled input per distinct name; errors after Run with invalid values; changing a value, or removing its placeholder, clears its error |
| Saved query loaded | Trigger shows its name; "Unsaved changes" dot when the query, the used parameters or (when the saved query has one) the effective range differ |
| `?view=` not in a complete list | `view` dropped from the URL (replace); `draft.view` cleared; the editor keeps its text |
| `?view=` with a draft from that view | The draft stays (unsaved edits survive a reload) |
| List fetch failed | Alert "Saved queries could not be loaded."; `?view=` and the draft untouched |
| Viewer | "Share with project" disabled (menu and dialog); as owner of a shared query, Save and Rename disabled (Stop sharing and Delete stay) |
| Ownerless shared query, admin | "Delete" shown, "Stop sharing" hidden |
| localStorage unavailable | Everything works for the page; nothing is remembered |

## Tests

New unit test files import `{ expect, it }` (and `describe` if used) from `vitest` and the module under test. The page test adds to H11's imports: `import { vi } from "vitest";` (merge into the existing `vitest` import), `import { savedViewHandlers } from "@/mocks/saved-views";`, `import { ADA_ID, BOB_ID } from "@/test/fixtures";`, `import projectFixture from "../../../../../../fixtures/api/static/project.json";`. `DEFAULT_QUERY` and `resultDocument` are already imported by H11.

### `web/src/lib/search/sql.test.ts` (extend)

1. `parseSqlSearch` keeps a UUID `view` and drops `view: "x"`; `withSqlRange({ range: "1h", view: V }, { kind: "preset", preset: "7d" })` is `{ view: V, range: "7d" }`; `withoutView({ range: "1h", view: V })` is `{ range: "1h" }`.

### `web/src/lib/sql/draft.test.ts` (adapt H11's)

1. H11's expectations gain `params: {}` (`readDraft` of nothing is `{ query: DEFAULT_QUERY, params: {} }`; of stored `{ query: "SELECT 1" }` is `{ query: "SELECT 1", params: {} }`).
2. Stored `'{"query":"Q","params":{"a":"1","b":2},"view":"V"}'` reads `{ query: "Q", params: { a: "1" }, view: "V" }`; a non-string `view` is dropped; `params: []` reads `{}`.
3. H11's `useSqlDraft` test keeps passing with `params: {}` added to its expected value.

### `web/src/lib/sql/params.test.ts`

1. `sqlParams("SELECT * FROM traces WHERE name = {name:String} AND cost_total > {min:float}")` → `[{ name: "name", type: "String" }, { name: "min", type: "Float" }]`.
2. `"{a:Int} + {a:Int} + {a:String}"` → `[{ name: "a", type: "Int" }]`.
3. Skipped regions, each alone → `[]`: `"'{a:Int}'"`, `"'it''s {a:Int}'"`, `"\"{a:Int}\""`, ``"`{a:Int}`"``, `"-- {a:Int}"`, `"/* {a:Int} */"`, `"/* /* */ {a:Int} */"`, `"$${a:Int}$$"`, `"$q${a:Int}$q$"`, `"'{a:Int}"`, `"/* {a:Int}"`.
4. After a skipped region, a placeholder counts: `"'x' {b:Bool}"`, `"/* /* */ */ {b:Bool}"`, `"$$x$$ {b:Bool}"`, `"-- c\n{b:Bool}"` → `[{ name: "b", type: "Bool" }]` each.
5. Identifiers: `"{café:Int}"` → name `café`; `"{#a:Int} {@b:Int} {c$1:Int}"` → names `#a`, `@b`, `c$1`; `"{select:Int}"` → name `select`; `"a$$b {x:Int}"` → `[{ name: "x", type: "Int" }]`; `"$1 {x:Int}"` → `[{ name: "x", type: "Int" }]`; `"😀{x:Int}"` and `"'😀' {x:Int}"` → `[{ name: "x", type: "Int" }]`.
6. Not placeholders: `"{ a:Int }"`, `"{a :Int}"`, `"{a: Int}"`, `"{a:Date}"`, `"{1a:Int}"`, `"{a:Int"` → `[]` each.
7. `paramValues` of `[Int n, Float f, Bool b, Timestamp t, Timestamp e, String s]` with `{ n: " 42 ", f: "-1.5e3", b: "true", t: "2026-01-01T00:00:00Z", e: "1767225600000", s: " x " }` → `values` `{ n: 42, f: -1500, b: true, t: "2026-01-01T00:00:00Z", e: 1767225600000, s: " x " }`, `errors` `{}`.
8. Errors: Int `"4.2"` → `"Enter an integer"`; Int `"99999999999999999999"` → `INT_RANGE`; Float `""`, `"Infinity"`, `"0x10"`, `".5"` → `"Enter a number"`; Bool `""` → `"Choose true or false"`; a `String` param never typed → value `""`, no error.
9. `isServerTimestamp`: true for `"2026-01-01T00:00:00Z"`, `"2026-01-01t00:00:00z"`, `"2026-01-01T00:00:00.123456789+02:00"`; false for `"2026-01-01"`, `"2026-01-01 00:00:00Z"`, `"2026-01-01T00:00:00.1234567890Z"` (10 digits), `"2026-02-30T00:00:00Z"`, `"2026-01-01T24:00:00Z"`, `"2026-01-01T00:00:60Z"`, `"2026-01-01T00:00:00+24:00"`.
10. `usedParams("SELECT {a:Int}", { a: "1", b: "2" })` is `{ a: "1" }`; `usedParams("SELECT {a:Int}", {})` is `{ a: "" }`.

### `web/src/lib/sql/request.test.ts` (extend H11's)

1. Without `params`, and with `params: {}`: no `params` key.
2. With `params: { n: 7, b: true }`: `params` deep-equals `{ n: 7, b: true }`.

### `web/src/lib/sql/saved-state.test.ts`

1. `parseSavedState({ query: "Q", params: [] })` → `{ query: "Q", params: {} }`; `parseSavedState({ query: "Q", params: { a: 1, b: true, c: "x", d: null }, range: "7d" })` → `{ query: "Q", params: { a: "1", b: "true", c: "x" }, range: "7d" }`; `parseSavedState("nope")` → `{ query: "", params: {} }`; `range: "1h"` with a valid `from < to` pair → the pair, no `range`; `range: "1h"` with `from >= to` → `{ …, range: "1h" }`; `from` without `to` → no pair.
2. `savedStateOf({ query: "SELECT {a:Int}", params: { a: "1", old: "x" } }, { range: "1h", view: V })` → `{ query: "SELECT {a:Int}", params: { a: "1" }, range: "1h" }`; with `{ range: "1h", from: A, to: B }` → `from`/`to`, no `range`; with `{}` → `range: "24h"`.
3. `sameSavedState`: params in another key order match; a saved state without a range matches any current range; saved `range: "7d"` vs current `range: "1h"` does not match; saved pair vs the same current pair matches; saved pair vs current `range: "24h"` does not match; a different query does not match.
4. `searchOfSavedState`: stored `range: "7d"` → `{ range: "7d", view: V }`; stored pair → `{ from, to, view: V }`; neither, with current `{ range: "1h", view: OLD }` → `{ range: "1h", view: V }`.
5. `paramsKey({ b: "2", a: "1" })` equals `paramsKey({ a: "1", b: "2" })`.

### `web/src/lib/sql/examples.test.ts`

1. Five examples named, in order, `Cost per model per day`, `LLM latency percentiles per model`, `Traces with the most tool calls`, `Average judge score per release`, `Users by spend`.
2. Each has `sqlParams(query)` equal to `[]` and starts with `SELECT`.
3. The columns each example reads exist in `fixtures/api/static/schema.json` (import it as `schemaFixture` with 4 × `..` from `web/src/lib/sql/`): for every `[view, columns]` pair of this table, every column is in that view's `columns[].name`:
   - Cost per model per day: `spans` → `start_time`, `model`, `cost_total`, `span_kind`;
   - LLM latency percentiles per model: `spans` → `model`, `duration_ms`, `span_kind`;
   - Traces with the most tool calls: `spans` → `trace_id`, `span_kind`;
   - Average judge score per release: `scores` → `trace_id`, `name`, `value_num`, `source`; `traces` → `trace_id`, `release`;
   - Users by spend: `traces` → `user_id`, `cost_total`.

### `web/src/lib/sql/history.test.ts`

`REF = "acme/support-bot"`.
1. `pushHistory(REF, { query: "A", params: {}, ranAt: "2026-01-01T00:00:00.000Z", rows: 1 })` then `{ query: "B", params: {}, ranAt: "2026-01-01T00:00:01.000Z" }` → `readHistory(REF).map((e) => e.query)` is `["B", "A"]`; `localStorage.getItem("llmobs.sql.history.acme/support-bot")` is not null.
2. Pushing `{ query: "B", params: {}, ranAt: "2026-01-01T00:00:02.000Z" }` again leaves two entries, the newest with `ranAt` `"2026-01-01T00:00:02.000Z"` and no `rows`; pushing `B` with `params: { a: "1" }` adds a third.
3. 55 pushes of `Q0`…`Q54` (each `ranAt` `"2026-01-01T00:00:00.000Z"`) keep 50, the first being `Q54`.
4. Stored `"x"` or `'{"a":1}'` reads `[]`; an entry without `query` is skipped; `clearHistory(REF)` empties it.
5. `historyLabel("\n  SELECT 1  \nFROM t")` is `"SELECT 1"`; a 70-character first line becomes its first 59 characters plus `…`.

### Page tests — append to `web/src/routes/$org/$project/sql/index.test.tsx`

Constants and helpers:

```ts
const COST_VIEW = "01900000-0000-7000-8000-000000000211";
const SHARED_VIEW = "01900000-0000-7000-8000-000000000212";
const SEED_QUERY =
  "SELECT model, date_trunc('day', start_time) AS day, sum(cost_total) AS cost FROM spans WHERE span_kind = 'llm' GROUP BY model, day ORDER BY day, cost DESC";

function sqlView(overrides: Record<string, unknown>) {
  return {
    id: SHARED_VIEW,
    kind: "sql",
    name: "Shared one",
    state: { query: "SELECT 1", params: {}, range: "24h" },
    owner_user_id: ADA_ID,
    shared: true,
    is_default: false,
    created_at: "2026-01-01T09:00:00.000000000Z",
    updated_at: "2026-01-01T09:00:00.000000000Z",
    ...overrides,
  };
}

function storeFullDraft(draft: { query: string; params?: Record<string, string>; view?: string }) {
  localStorage.setItem(
    "llmobs.sql.draft.acme/support-bot",
    JSON.stringify({ params: {}, ...draft }),
  );
}

function storedDraft(): unknown {
  return JSON.parse(localStorage.getItem("llmobs.sql.draft.acme/support-bot") ?? "null");
}

function storedHistory(): Record<string, unknown>[] {
  return JSON.parse(localStorage.getItem("llmobs.sql.history.acme/support-bot") ?? "[]");
}

function asRole(role: string) {
  server.use(
    http.get("*/api/v1/projects/:project", () => HttpResponse.json({ ...projectFixture, role })),
  );
}

function captureWrites(method: "POST" | "PATCH") {
  const bodies: unknown[] = [];
  const onStart = ({ request }: { request: Request }) => {
    if (request.method === method && request.url.includes("/saved-views")) {
      void request
        .clone()
        .json()
        .then((body) => bodies.push(body));
    }
  };
  server.events.on("request:start", onStart);
  return { bodies, stop: () => server.events.removeListener("request:start", onStart) };
}

function problemAnswer(status: number, code: string, detail: string) {
  return () =>
    HttpResponse.json(
      { type: "about:blank", title: "Error", status, code, detail },
      { status, headers: PROBLEM_JSON },
    );
}
```

`savedViewHandlers` objects are shaped like `fixtures/api/static/saved-views.json` entries; if `tsc` complains about `sqlView`'s return type there, pass `sqlView({…}) as Parameters<typeof savedViewHandlers>[0][number]`. Wrap `captureWrites` users in `try { … } finally { capture.stop(); }`. Items that depend on ownership (Save, Rename, Share, Stop sharing, Delete) appear only once `me` has loaded: find them with `findByRole`. Read localStorage inside `waitFor`. Open the menu with `await user.click(await screen.findByRole("button", { name: /Saved queries|<loaded name>/ }))`.

1. **"sends typed parameters"**: `storeFullDraft({ query: "SELECT {n:Int} AS n, {flag:Bool} AS b" })`; `const sql = captureSql();` render `/acme/support-bot/sql`; `await user.type(await screen.findByLabelText("n (Int)"), "7")`; `await user.selectOptions(screen.getByLabelText("flag (Bool)"), "true")`; click Run; `waitFor` `single(sql.bodies).params` deep-equals `{ n: 7, flag: true }`.
2. **"blocks a run with an invalid parameter, then clears the error"**: same draft; `const sql = captureSql();` type `x` into `n (Int)`; click Run; `findByText("Enter an integer")`; `getByLabelText("n (Int)")` has `aria-invalid="true"`; after `await new Promise((resolve) => setTimeout(resolve, 50))`, `sql.bodies` is empty; `await user.type(screen.getByLabelText("n (Int)"), "{Backspace}1")`: `queryByText("Enter an integer")` is null.
3. **"clears a parameter error when its placeholder goes"**: `storeFullDraft({ query: "SELECT {n:Int}" })`; click Run; `findByText("Enter an integer")`; focus the editor, `await user.keyboard("{Control>}a{/Control}SELECT 1")`; `waitFor` `queryByText("Enter an integer")` is null and `queryByLabelText("n (Int)")` is null.
4. **"lists saved queries and loads one"**: render `/acme/support-bot/sql?range=1h`; open "Saved queries"; click `getByRole("menuitem", { name: /Cost per model per day/ })`; `waitFor` `router.state.location.search` equals `{ range: "24h", view: COST_VIEW }` (the seed's range replaces the URL's); `waitFor` `editorContent().textContent` is `SEED_QUERY`; `findByRole("button", { name: /Cost per model per day/ })`; `queryByRole("status", { name: "Unsaved changes" })` is null; `waitFor` `storedDraft()` has `view: COST_VIEW`.
5. **"does not list another user's private query"**: `server.use(...savedViewHandlers([sqlView({ owner_user_id: BOB_ID, shared: false, name: "Bob's" })]))`; render; open the menu; `getByRole("menuitem", { name: "No saved queries" })`.
6. **"opens a ?view= link over another draft"**: `storeDraft("SELECT 'old'")`; render `/acme/support-bot/sql?view=${COST_VIEW}`; `waitFor` the editor text is `SEED_QUERY`.
7. **"keeps unsaved edits of the same view on reload"**: `storeFullDraft({ query: "SELECT 'edited'", view: COST_VIEW })`; render `?view=${COST_VIEW}&range=24h`; `findByRole("status", { name: "Unsaved changes" })`; the editor text is `SELECT 'edited'`.
8. **"shows no dot for a view link without range"**: render `?view=${COST_VIEW}`; `waitFor` the editor text is `SEED_QUERY`; `findByRole("button", { name: /Cost per model per day/ })`; `queryByRole("status", { name: "Unsaved changes" })` is null.
9. **"drops an unknown ?view="**: render `/acme/support-bot/sql?range=1h&view=01900000-0000-7000-8000-0000000002ff`; `waitFor` search equals `{ range: "1h" }`.
10. **"keeps an unknown ?view= when the list is incomplete"**: `server.use(http.get("*/api/v1/projects/:project/saved-views", () => HttpResponse.json({ data: [], next_cursor: "more" })))`; render `?range=1h&view=01900000-0000-7000-8000-0000000002ff`; `await screen.findByRole("button", { name: "Saved queries" })`; after `await new Promise((resolve) => setTimeout(resolve, 50))`, search still has that `view`.
11. **"keeps ?view= when the list fails"**: `server.use(http.get("*/api/v1/projects/:project/saved-views", problemAnswer(503, "busy", "busy")))`; `storeDraft("SELECT 'mine'")`; render `?view=${COST_VIEW}`; `findByText("Saved queries could not be loaded.")`; search still has `view: COST_VIEW`; the editor text is `SELECT 'mine'`.
12. **"saves the current query"**: `const capture = captureWrites("POST")`; `storeFullDraft({ query: "SELECT {a:Int}", params: { a: "1", old: "x" } })`; render `/acme/support-bot/sql?range=6h`; open the menu; click "Save as…"; type `Mine` in `getByLabelText("Name")`; click "Save query"; `waitFor` `single(capture.bodies)` deep-equals `{ kind: "sql", name: "Mine", state: { query: "SELECT {a:Int}", params: { a: "1" }, range: "6h" }, shared: false, is_default: false }`; `waitFor` `router.state.location.search.view` matches `/^[0-9a-f-]{36}$/`; `findByRole("button", { name: /Mine/ })`.
13. **"stores the default range when the URL has none"**: `const capture = captureWrites("POST")`; render `/acme/support-bot/sql`; Save as… `Plain`; `waitFor` the body's `state` deep-equals `{ query: DEFAULT_QUERY, params: {}, range: "24h" }`.
14. **"Save as on a loaded query keeps the text and switches the id"**: render `?view=${COST_VIEW}`; `waitFor` the editor text is `SEED_QUERY`; focus the editor; `await user.keyboard("{Control>}{End}{/Control} LIMIT 9")`; Save as… `Copy`; `waitFor` search `view` is a UUID other than `COST_VIEW`; the editor text is still `${SEED_QUERY} LIMIT 9`; after `await new Promise((resolve) => setTimeout(resolve, 50))` it is unchanged; `findByRole("button", { name: /Copy/ })`.
15. **"shows a refused save in the dialog and clears it on close"**: `server.use(http.post("*/api/v1/projects/:project/saved-views", problemAnswer(409, "conflict", "too many saved views")))`; Save as… `Mine`; `findByText("too many saved views")`; `getByRole("dialog", { name: "Save query" })` is open; click "Cancel"; open the menu: `queryByRole("alert")` is null.
16. **"marks edits and saves over the loaded query"**: `const capture = captureWrites("PATCH")`; render `?view=${COST_VIEW}`; `waitFor` the editor text is `SEED_QUERY`; focus; `await user.keyboard("{Control>}{End}{/Control} LIMIT 5")`; `findByRole("status", { name: "Unsaved changes" })`; open the menu; click `await screen.findByRole("menuitem", { name: "Save" })`; `waitFor` `single(capture.bodies)` deep-equals `{ state: { query: \`${SEED_QUERY} LIMIT 5\`, params: {}, range: "24h" } }`; `waitFor` the status is gone.
17. **"shows a failed save in the menu"**: render `?view=${COST_VIEW}`; edit as in 16; `server.use(http.patch("*/api/v1/projects/:project/saved-views/:id", problemAnswer(400, "bad_request", "state is too large")))`; open the menu; click Save; `within(screen.getByRole("menu")).findByRole("alert")` has text `state is too large`.
18. **"discards changes"**: as 16 up to the status; open the menu; click "Discard changes"; `waitFor` the editor text is `SEED_QUERY` and the status is gone.
19. **"renames the loaded query"**: render `?view=${COST_VIEW}`; open the menu; click `await screen.findByRole("menuitem", { name: "Rename…" })`; `await user.clear(screen.getByLabelText("Name"))`; type `Costs`; click "Rename"; `findByRole("button", { name: /Costs/ })`.
20. **"shows a refused rename in the dialog"**: `server.use(http.patch("*/api/v1/projects/:project/saved-views/:id", problemAnswer(400, "bad_request", "name is too long")))`; render `?view=${COST_VIEW}`; open the menu; Rename… `Costs`; `findByText("name is too long")`; the dialog `Rename query` is still open.
21. **"shares a query as a member"**: `asRole("member")`; `const capture = captureWrites("PATCH")`; render `?view=${COST_VIEW}`; open the menu; click `await screen.findByRole("menuitem", { name: "Share with project" })`; `waitFor` `single(capture.bodies)` deep-equals `{ shared: true }`; `findByRole("img", { name: "Shared" })`.
22. **"stops sharing an owned query"**: `server.use(...savedViewHandlers([sqlView({})]))`; `const capture = captureWrites("PATCH")`; render `?view=${SHARED_VIEW}`; open the menu (`/Shared one/`); click `await screen.findByRole("menuitem", { name: "Stop sharing" })`; `waitFor` the body is `{ shared: false }`.
23. **"lets an admin moderate someone else's shared query"**: `server.use(...savedViewHandlers([sqlView({ owner_user_id: BOB_ID })]))`; render `?view=${SHARED_VIEW}`; open the menu; `await screen.findByRole("menuitem", { name: "Stop sharing" })` and `getByRole("menuitem", { name: "Delete" })` exist; `queryByRole("menuitem", { name: "Save" })` and `queryByRole("menuitem", { name: "Rename…" })` are null; `getByRole("img", { name: "Shared" })` exists.
24. **"hides Stop sharing for an ownerless shared query"**: `const orphan = sqlView({}); delete (orphan as { owner_user_id?: string }).owner_user_id;` `server.use(...savedViewHandlers([orphan]))`; render `?view=${SHARED_VIEW}`; open the menu; `await screen.findByRole("menuitem", { name: "Delete" })`; `queryByRole("menuitem", { name: "Stop sharing" })` is null.
25. **"viewers cannot share"**: `asRole("viewer")`; render `?view=${COST_VIEW}`; open the menu; `await screen.findByRole("menuitem", { name: /^Share with project/ })` has `aria-disabled="true"` and contains `Needs the member role`; `await user.keyboard("{Escape}")`; open the menu again and click "Save as…"; `getByRole("switch", { name: "Share with project" })` is disabled.
26. **"a viewer cannot change their shared query but may delete it"**: `asRole("viewer")`; `server.use(...savedViewHandlers([sqlView({})]))`; render `?view=${SHARED_VIEW}`; open the menu; `await screen.findByRole("menuitem", { name: /^Rename…/ })`; `const items = screen.getAllByRole("menuitem");` `const save = items.find((item) => item.textContent?.startsWith("Save") && !item.textContent.startsWith("Save as"));` and `const rename = items.find((item) => item.textContent?.startsWith("Rename…"));` are defined, have `aria-disabled="true"` and contain `Changing a shared query needs the member role`; `getByRole("menuitem", { name: "Delete" })` and `getByRole("menuitem", { name: "Stop sharing" })` are enabled.
27. **"deletes the loaded query"**: render `?view=${COST_VIEW}`; `waitFor` search equals `{ range: "24h", view: COST_VIEW }`; open the menu; click `await screen.findByRole("menuitem", { name: "Delete" })`; click `getByRole("button", { name: "Delete query" })`; `waitFor` search equals `{ range: "24h" }`; open the menu: `getByRole("menuitem", { name: "No saved queries" })`.
28. **"shows a refused delete in the dialog"**: `server.use(http.delete("*/api/v1/projects/:project/saved-views/:id", problemAnswer(409, "conflict", "cannot delete")))`; render `?view=${COST_VIEW}`; open the menu; Delete; click "Delete query"; `findByText("cannot delete")`; the dialog `Delete query` is still open.
29. **"saves and reloads an absolute range"**: `const capture = captureWrites("POST")`; render `/acme/support-bot/sql?from=2026-01-01T00:00:00Z&to=2026-01-02T00:00:00Z`; Save as… `Day`; `waitFor` `single(capture.bodies)` has `state` deep-equal to `{ query: DEFAULT_QUERY, params: {}, from: "2026-01-01T00:00:00Z", to: "2026-01-02T00:00:00Z" }`; `waitFor` search `view` is defined; `const created = String(router.state.location.search.view)`. Then `await router.navigate({ to: "/$org/$project/sql", params: { org: "acme", project: "support-bot" }, search: { range: "1h" } })`; open the menu ("Saved queries"); click `getByRole("menuitem", { name: /Day/ })`; `waitFor` search equals `{ from: "2026-01-01T00:00:00Z", to: "2026-01-02T00:00:00Z", view: created }`.
30. **"loads saved params and runs them with Ctrl+Enter"**: `server.use(...savedViewHandlers([sqlView({ shared: false, state: { query: "SELECT {n:Int}", params: { n: "5" }, range: "24h" } })]))`; `const sql = captureSql();` render `?view=${SHARED_VIEW}`; `expect(await screen.findByLabelText("n (Int)")).toHaveValue("5")`; focus the editor; Ctrl+Enter; `waitFor` `single(sql.bodies).params` deep-equals `{ n: 5 }`.
31. **"loading another query replaces unsaved edits"**: `server.use(...savedViewHandlers([sqlView({ shared: false }), sqlView({ id: COST_VIEW, name: "Other", shared: false, state: { query: "SELECT 2", params: {}, range: "24h" } })]))`; render `?view=${SHARED_VIEW}`; focus; `await user.keyboard("{Control>}{End}{/Control} + 1")`; open the menu; click `Other`; `waitFor` the editor text is `SELECT 2` and search `view` is `COST_VIEW`.
32. **"an example clears the parameters and keeps the view"**: `storeFullDraft({ query: "SELECT {n:Int}", params: { n: "3" }, view: COST_VIEW })`; render `?view=${COST_VIEW}&range=24h`; `findByLabelText("n (Int)")`; click `getByRole("button", { name: "Examples" })`; click `getByRole("menuitem", { name: "Users by spend" })`; `waitFor` the editor text contains `ORDER BY cost DESC`; `queryByLabelText("n (Int)")` is null; `waitFor` `storedDraft()` has `params: {}` and `view: COST_VIEW`; `findByRole("status", { name: "Unsaved changes" })`; after `await new Promise((resolve) => setTimeout(resolve, 50))`, the editor text still contains `ORDER BY cost DESC`.
33. **"a history pick keeps a stale ?view= from reloading"**: store history `[{ query: "SELECT 9", params: {}, ranAt: "2026-01-01T00:00:00.000Z", rows: 1 }]` under `llmobs.sql.history.acme/support-bot`; render `?view=${COST_VIEW}`; `waitFor` the editor text is `SEED_QUERY`; open History; click `getByRole("menuitem", { name: /^SELECT 9/ })`; `waitFor` the editor text is `SELECT 9`; after `await new Promise((resolve) => setTimeout(resolve, 50))` it is still `SELECT 9`.
34. **"records runs with only the used params and restores them"**: `storeFullDraft({ query: "SELECT {n:Int}", params: { n: "4", stale: "x" } })`; `captureSql()`; render; click Run; `findByRole("grid", { name: "Query results" })`; `waitFor` `storedHistory()[0]` matches `{ query: "SELECT {n:Int}", params: { n: "4" }, rows: resultDocument.rows.length }`; focus the editor, `await user.keyboard("{Control>}a{/Control}SELECT 2")`; open History; click `getByRole("menuitem", { name: /^SELECT \{n:Int\}/ })`; `waitFor` the editor text is `SELECT {n:Int}` and `getByLabelText("n (Int)")` has value `4`. Then open History, click "Clear history", open it again: `getByRole("menuitem", { name: "No runs yet" })`.
35. **"keeps one history entry for a repeated run"**: `storeDraft("SELECT 1")`; `captureSql()`; click Run; wait for the grid; click Run again; wait for the status line to reappear; `waitFor` `storedHistory()` has length 1.
36. **"records a failed run"**: `storeDraft("SELECT nope")`; `captureSql(() => HttpResponse.json({ type: "about:blank", title: "Bad Request", status: 400, code: "invalid_filter", detail: "invalid query", errors: [{ field: "query", message: "unknown column nope" }] }, { status: 400, headers: PROBLEM_JSON }))`; click Run; `findByRole("alert")`; `waitFor` `storedHistory()[0]` has no `rows`; open History: the item for `SELECT nope` contains `failed`.
37. **"works when localStorage throws"**: `const setItem = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("quota"); });` `try {` `const sql = captureSql();` render; focus the editor; `await user.keyboard("{Control>}a{/Control}SELECT 3{Control>}{Enter}{/Control}")`; `waitFor` `single(sql.bodies).query` is `"SELECT 3"`; open History: `getByRole("menuitem", { name: "No runs yet" })`. `} finally { setItem.mockRestore(); }`

## Commands

```bash
export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH"
pnpm --dir web exec biome check --write .
just check-web
```

While iterating: `pnpm --dir web exec vitest run src/lib/sql src/lib/search/sql.test.ts src/routes/\$org/\$project/sql src/components/saved-views`. Before committing: the same `PATH` line, `pnpm --dir web exec biome check --write .`, then `just check`.

## Done when

- Parameters, saved queries (private and shared), examples and history work against the mocks under `just web-dev`.
- The existing saved-views menu tests still pass (only `export` was added there).
- Every test above passes; `just check` passes; no `vi.mock`.
- One commit, subject `web: add saved queries, parameters and history`, no trailer lines. Report "no new dependency".
