# C · Lists and filters (U9–U14)

See [README.md](README.md) for the decisions, boundaries, dependency versions and the rules that apply to every sub-phase. These sub-phases start only after the U8 review gate and any U8a… adjustments.

They complete the traces list (time range, search, columns, sorting, filter bar, saved views) and add the spans page. The pieces are written once and reused by sessions, users and the dashboard (U21–U23).

---

## U9 — Time range, search and toggles

**Edit only:** `web/src/lib/time-range.ts` (+ test), `web/src/components/time-range/**` (new), `web/src/components/list-toolbar.tsx` (new), `web/src/lib/search/{common,traces}.ts`, `web/src/routes/$org/$project/traces/index.tsx`, shadcn `popover`, `input`, `toggle`, `switch` and `label` (not `calendar`, see below), plus tests.

**Commit:** `web: time range, search and eval toggles on the traces list`

### `lib/time-range.ts` (pure)

```ts
type Preset = "15m" | "1h" | "6h" | "24h" | "7d" | "30d";
type RangeState = { kind: "preset"; preset: Preset } | { kind: "absolute"; from: string; to: string };
function rangeFromSearch(search: {range?: Preset; from?: string; to?: string}): RangeState; // absolute wins when both from and to are valid and from < to; else preset (default "24h")
function resolveRange(state: RangeState, nowMs: number): { from: string; to: string };    // RFC 3339 UTC, ms precision
function parseTypedRange(text: string): Preset | { durationMs: number } | undefined;      // "15m", "2h", "3d", "90s" → duration; presets map to Preset
function rangeLabel(state: RangeState, zone): string;                                      // "Last 24 hours" / "Jan 1 10:00 → Jan 1 12:00"
```

**The anchor.** A preset is resolved against an anchor time held in the list's state, not against "now" on every render. The anchor is set when the page mounts, when the range changes, and when the user presses **Refresh**. It's part of the query key, so pages of one infinite query stay coherent. The anchor is not in the URL; links share the preset.

### `TimeRangePicker`

A toolbar button showing `rangeLabel`. The popover holds:
- **presets** as a vertical list;
- **a text field** accepting typed durations (`parseTypedRange`; Enter applies; an invalid entry shows "Use 15m, 2h, 7d…");
- **"Custom"**: two `datetime-local` inputs (from, to) in the current time-zone setting, applied with a button. Don't add a calendar component; the native input is enough and keeps the bundle small;
- **a Refresh button** (icon) next to the picker, which re-anchors.

Changing the range writes `range` (and removes `from`/`to`) or writes `from`/`to` (and removes `range`).

### Toolbar (`list-toolbar.tsx`)

One row, 36 px, reused by every list page:

```
[title] [time range][⟳]  [filter bar — U12 fills this slot]  [search]  [eval ▢][exp ▢]  [columns — U10]  [views — U14]
```

In this sub-phase:
- the title, the time range, and the **search box** (`search` param);
- a 300 ms debounce, Enter applies immediately, Esc clears;
- `/` focuses it when focus is not in an editable element.
- **Toggles** "Eval" and "Experiments" (D123 default exclusions, spec 05): `eval=1` → `include_eval=true`, `exp=1` → `include_experiments=true`. Tooltips explain what they show.

`lib/search/traces.ts` gains `range`, `from`, `to`, `search`, `eval` and `exp`. The traces query passes `from`/`to` (resolved), `search`, `include_eval` and `include_experiments`.

**Tests:**
- `time-range.ts`: default; absolute wins; an invalid absolute pair (from ≥ to) falls back to the preset; `parseTypedRange` cases; `resolveRange` at a fixed now.
- **Page:**
  - choosing "1h" updates the URL and the request's `from` (inspect it in a `server.use` handler that records `request.url`);
  - Refresh re-anchors, so a new `from` is sent;
  - typing a search sends `search` after the debounce (use fake timers or `waitFor`);
  - `eval=1` sends `include_eval=true`, and the fixture `traces-include-eval.json` shows trace 5.

---

## U10 — Columns and sorting

**Edit only:** `web/src/components/data-table/**`, `web/src/lib/search/{common,traces}.ts`, `web/src/routes/$org/$project/traces/index.tsx`, `web/src/components/trace-columns.tsx` (new: the traces column defs moved out of the route), plus tests.

**Commit:** `web: column chooser, score columns and sorting`

- **Column registry.** Each list declares its columns with stable ids. For traces:
  - `status`, `start`, `name`, `input`, `output`, `duration`, `tokens`, `cost`, `models`, `session`, `user`, `tags`, `env`;
  - plus `release`, `spans` (`span_count`), `errors` (`error_count`), `llm_calls` (`llm_count`), `trace_id`, all hidden by default;
  - plus **one column per score name present** in the loaded rows: id `score:<name>`, header the name. Numeric values are formatted with 2 decimals, boolean as ✓/✗, categorical as text. The tooltip shows the source.
- **The `cols` param** (comma-separated ids, in order) sets visibility and order. Unknown ids are ignored, and score columns are allowed only if the name exists. With no `cols`, the defaults apply, plus the score columns: they're visible by default (spec 11) and come after `cost`. The user's last choice per table also goes into localStorage (`${STORAGE_PREFIX}table.<tableId>.cols`); the URL wins over it.
- **Column chooser:** a toolbar popover listing all columns with checkboxes, plus up/down buttons (no drag and drop) to reorder, and "Reset". `status` and `name` can't be hidden.
- **Sorting:** the sortable columns are those backed by the enum (`start` → `start_time`, `duration` → `duration`, `cost` → `cost_total`, `tokens` → `total_tokens`). Clicking a header cycles desc → asc → back to the default (`start_time desc`) and writes `sort` and `order`. The header shows an arrow and `aria-sort`. Other headers aren't clickable.

**Tests:**
- `cols` restores visibility and order;
- an unknown id is ignored;
- a score column appears for `helpfulness` from the fixtures;
- the chooser toggles a column and updates the URL;
- sorting by cost sends `sort=cost_total&order=desc`, then `asc`, then removes both;
- the localStorage default is used when the URL has no `cols`.

---

## U11 — The DSL chip parser

**Edit only:** `web/src/lib/dsl/**` (new), plus tests.

**Commit:** `web: parse filter conjuncts into chips`

Pure TypeScript, no CodeMirror. It doesn't validate the DSL (the server does, D129); it only splits and recognises the shapes the chips can edit.

```ts
type Token = { kind: "ident" | "string" | "number" | "op" | "keyword" | "lparen" | "rparen" | "lbracket" | "rbracket" | "comma" | "other"; text: string; start: number; end: number };
function tokenize(text: string): Token[];          // never throws; unterminated string → one "other" token to end
function splitConjuncts(text: string): { text: string; start: number; end: number }[];
//   splits on top-level AND (case-insensitive, outside parens/brackets/strings); trims; drops empty pieces

type ChipValue = { kind: "string"; value: string } | { kind: "number"; value: number } | { kind: "boolean"; value: boolean };
type Chip =
  | { kind: "compare"; field: Field; op: "=" | "!=" | "<" | "<=" | ">" | ">=" | "LIKE" | "ILIKE" | "NOT LIKE" | "NOT ILIKE"; value: ChipValue }
  | { kind: "null"; field: Field; negated: boolean }                         // IS [NOT] NULL
  | { kind: "in"; field: Field; negated: boolean; values: ChipValue[] }      // [NOT] IN (…)
  | { kind: "contains"; field: Field; values: string[] }                     // tags @> ['a','b']
  | { kind: "score"; name: string; fn: "score" | "score_label"; op: …; value: ChipValue }
  | { kind: "has_score"; name: string; negated: boolean }                    // [NOT] has_score('n')
  | { kind: "raw"; text: string };
type Field = { column: string } | { column: "metadata" | "attributes" | "resource_attributes"; key: string };

function parseChip(conjunct: string): Chip;          // falls back to { kind: "raw", text }
function serializeChip(chip: Chip): string;          // canonical text: identifiers lowercase, keywords uppercase,
                                                     // strings single-quoted with '' escaping, numbers as typed
function serializeFilter(chips: Chip[]): string;     // joins with " AND "; a raw chip whose text has a top-level OR is wrapped in parentheses
```

Rules:
- `<>` is read as `!=`.
- Identifiers are `[A-Za-z_][A-Za-z0-9_]*`, case-insensitive, and canonicalised to lowercase.
- Map access is `metadata['k']` (also `attributes` and `resource_attributes`) with a string key.
- The parser doesn't check that the column exists. The editor's autocomplete and the server do.
- A conjunct wrapped in a single pair of parentheses with nothing else inside but a recognisable shape is unwrapped. Anything with OR or NOT around a group, function calls other than the score functions, arithmetic, casts and intervals is **raw**.
- `parseChip(serializeChip(c))` deep-equals `c` for every non-raw chip (round trip).
- `serializeFilter(splitConjuncts(t).map(parseChip))` may differ from `t` only in whitespace and canonicalisation. It never changes meaning: raw chips keep their text byte for byte.

**Tests:** table-driven, at least 40 cases, including:
- every chip kind, both negations, `<>`;
- doubled quotes in strings (`'it''s'`);
- `5e9` numbers, negative numbers;
- `metadata['customer']`;
- `tags @> ['beta']`;
- `score('hallucination') >= 0.5`;
- `has_score('x')`, `NOT has_score('x')`;
- case variants (`and`, `And`);
- AND inside a string (`name = 'a AND b'`) or inside parentheses;
- `(a = 1 OR b = 2) AND c = 3` (raw + compare);
- unterminated string;
- an empty filter;
- `now() - INTERVAL '1 hour'` (raw);
- round trips.

---

## U12 — The filter bar

**Edit only:** `web/package.json`, `pnpm-lock.yaml`, `web/src/components/filter-bar/**` (new), `web/src/components/list-toolbar.tsx`, `web/src/lib/search/{common,traces}.ts`, `web/src/routes/$org/$project/traces/index.tsx`, `web/src/api/queries.ts` (schema query), plus tests.

**Commit:** `web: filter bar with chips, autocomplete and inline errors`

Add the CodeMirror packages listed in the README.

### Components

- `FilterBar({view, value, onApply, error})`. `view` is a `ViewName` (`traces`, `spans`, `sessions`, `scores`); `value` is the applied filter text from the URL; `error` is the `Problem` of the last list request when its `code` is `invalid_filter`.
- **Chips row:** the applied filter, split with `splitConjuncts` + `parseChip`, rendered as compact chips (24 px):
  - text like `model = 'gpt-4o'`;
  - a × removes the conjunct and applies;
  - clicking a non-raw chip opens a popover editor (field select from the schema columns, operator select filtered by column type, value input) that replaces the conjunct;
  - clicking a raw chip puts the whole filter into the text editor with that conjunct selected.
- **Text editor:** a single-line CodeMirror 6 editor (`EditorView`).
  - Extensions: `sql()` highlighting from `@codemirror/lang-sql`, a placeholder ("Filter, e.g. status = 'error' AND cost_total > 0.1"), `history`, the default keymap minus Enter, and Enter = apply, Esc = revert to the applied value and blur.
  - Single line: newlines are replaced by spaces in a transaction filter.
  - It is shown when the user clicks the empty area of the bar, presses `f`, or clicks "Edit as text". When the editor is open, the chips are hidden and the editor holds the whole text.
- **Autocomplete** (`@codemirror/autocomplete`) from `GET /schema` (`get_schema`, cached for the session, `staleTime: Infinity`):
  - column names of the current view, with the type as detail and the description as info;
  - the `in_filter` functions with signature and description;
  - operators valid for the column type before the cursor (`=`, `!=`, `<`…, `LIKE`, `IS NULL`, `IN (…)`; `@>` for `string_list`);
  - `AND`, `OR`, `NOT`;
  - for `metadata[` and `attributes[`, a `'` snippet.

  There are no value suggestions yet (R3).
- **Apply:** `onApply(text)` writes `filter` (trimmed; an empty value removes the param). It replaces the URL entry (`replace: false`, so Back undoes a filter).
- **Errors (D129):** when the list query fails with `invalid_filter`:
  - the bar gets a red border;
  - the chips row is replaced by the text editor holding the applied text, with a red wavy underline (a CodeMirror `Decoration.mark`) from the error position's column (1-based, in characters) to the end of that token, or one character when the token can't be found;
  - below the bar, the message (`errors[].message` for `field: "filter"`, else `detail`, else `title`) in red, 12 px;
  - **the table shows the error state, not stale rows**: rows never appear filtered when the filter failed.
- **Click-to-filter.** Add a small `+` filter action on hover to these cells: `status`, `name`, `models` (first model), `session`, `user`, `tags` (each tag), `env`. Clicking it appends `column = 'value'` (`tags @> ['value']`, `models @> ['value']`) and applies. Keep it to these columns.

The traces page wires the bar into the toolbar slot and passes `filter` to `list_traces`.

**Tests:**
- chips render from `?filter=status = 'error' AND model LIKE 'gpt%'`; removing one chip updates the URL;
- editing a compare chip through the popover changes the value;
- typing in the text editor and pressing Enter applies (CodeMirror in jsdom: type with `userEvent` into the `.cm-content` element; if jsdom can't drive it, dispatch transactions through the `EditorView` found via `EditorView.findFromDOM` and say so in the report);
- autocomplete offers `cost_total` after typing `cos` (call the completion source function directly with a fake context if the popup can't be driven in jsdom);
- `invalid_filter` from `server.use` (status 400, `errors: [{field: "filter", message: "unknown column nme", position: {line: 1, column: 1}}]`) shows the message, marks the editor, and shows no rows;
- click-to-filter on a status cell appends `status = 'error'`.

---

## U13 — The spans page

**Edit only:** `web/src/routes/$org/$project/spans/**` (new), `web/src/components/span-columns.tsx` (new), `web/src/lib/search/spans.ts` (new), `web/src/components/shell/nav.ts`, `web/src/api/queries.ts` (or `queries-spans.ts`), plus tests.

**Commit:** `web: spans list`

- **The `/spans` route.** It has the same toolbar (time range, filter on the `spans` view, search, toggles, columns) and the same split behaviour as traces: `?trace=` and `?span=` open `TraceView` with that span selected (D125, D133).
- **Columns** (ids):

  | Shown by default | Hidden by default |
  |---|---|
  | `status`, `start`, `kind` (`KindIcon` + text), `name`, `model`, `input`, `output`, `duration`, `ttft`, `tokens`, `cost`, `session`, `user`, `env` | `provider`, `service`, `release`, `tags`, `trace_id`, `span_id`, `parent_span_id` |

  Sort keys come from `SpanSort`.
- **Activate:** a row sets `trace=<trace_id>&span=<span_id>`.
- **Nav entry:** "Spans" after "Traces" (icon `Rows3`).

**Tests:**
- the fixture spans render;
- activating a span opens the split with that span selected (its `get_span` fixture is requested);
- the filter is sent to `list_spans`;
- the nav shows Spans.

---

## U14 — Saved views

**Edit only:** `web/src/mocks/saved-views.ts` (new), `web/src/mocks/browser.ts`, `web/src/test/setup.ts`, `fixtures/api/static/saved-views.json`, `web/src/components/saved-views/**` (new), `web/src/components/list-toolbar.tsx`, `web/src/lib/search/common.ts`, the traces and spans routes, `web/src/api/queries.ts`, shadcn `dialog`, plus tests.

**Commit:** `web: saved views for lists`

### Mock handlers (`mocks/saved-views.ts`)

- `savedViewHandlers(initial: SavedView[])`: an in-memory store seeded from `fixtures/api/static/saved-views.json`. It implements:
  - `GET` with `kind` filtering and cursor paging (one page is enough, `next_cursor: null`);
  - `POST` (201 with a generated id, `owner_user_id` = the fixture user, timestamps now);
  - `PATCH` (200);
  - `DELETE` (204).

  Unknown ids give 404 `Problem`s. Setting `is_default: true` clears `is_default` on the user's other views of the same kind (spec 04 semantics: one default per user per kind; check spec 04 § saved_views and follow it if it says otherwise).
- These handlers are prepended in the browser worker and in the test server (`setup.ts`). `server.resetHandlers()` restores the initial state, and the store is re-created per test.

### State format

A saved view's `state` is the page's search params as an object of strings, **excluding** `trace`, `span`, `view` and `live` (selection is not part of a view). It holds `range`/`from`/`to`, `filter`, `search`, `sort`, `order`, `eval`, `exp` and `cols`. A preset range stays relative (`range: "24h"`), as spec 04 requires. `lib/search/common.ts` gains `viewStateOf(search)` and `searchFromViewState(state)`, which validates and drops unknown or invalid keys.

U1 aligned spec 04's `saved_views.state` comment with this format. Update the hand-written `fixtures/api/static/saved-views.json` to match: the `traces` view's state becomes `{"filter": "status = 'error'", "range": "24h"}`; the `sql` view keeps `{query, params}`. Run `just api-examples` to check that the fixture test still passes; it compares only that `state` is an object. If the regenerated `manifest.json` or `responses/` change, the generator depends on the static file: commit what it regenerates and say so.

**Saved filters that no longer validate** (spec 04): the view still loads; the filter bar shows the server's `invalid_filter` error (U12).

### UI

`SavedViewsMenu({kind})` in the toolbar: a dropdown with the current view's name (or "Default view") and a **modified dot** when `viewStateOf(current) ≠ the loaded view's state`. Items:
- the list of views (shared ones marked with an icon, and the default marked);
- **Save** (PATCH `state`; only for the owner);
- **Save as…** (dialog: name, "Share with project", "Make my default"; POST);
- **Rename…**;
- **Delete** (a confirm dialog);
- **Set as my default** / **Unset default**;
- **Discard changes** (reloads the view's state).

Behaviour:
- Loading a view replaces the URL search with `searchFromViewState(state)` + `view=<id>`.
- **Default view:** when a list page opens with no search params at all, it loads the user's default view for that kind, if any (`is_default` and owned by the user, or shared with the project when the user has none).
- Errors (`Problem`) show as an inline toast-like message in the menu (no new toast dependency).

**Tests:**
- the menu lists the fixture views of kind `traces` (the `sql` one is not shown);
- loading "Errors" sets `filter=status = 'error'` and `view=…`;
- changing the filter shows the modified dot, and Save sends a PATCH with the new state;
- Save as creates a view and selects it;
- delete removes it;
- opening `/traces` with no params applies the default view;
- a PATCH 404 shows the error.
