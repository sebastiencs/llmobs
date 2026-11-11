# C · Foundations and the review gate (U1–U8)

See [README.md](README.md) for the decisions, boundaries, dependency versions and the rules that apply to every sub-phase.

By U8 the user can open `just web-dev` and judge the look and feel:
- the shell;
- the density;
- the table pattern;
- a trace opened beside the list, with the combined tree and timeline;

all on both the contract mocks and the large generated dataset.

---

## U1 — Spec and decisions

**Edit only:** `docs/spec/11-ui.md`, `docs/spec/04-metadata-db.md` (§ Saved views comment), `docs/mvp-decisions.md`, `docs/research/ui/README.md` (§ Open questions), and this plan's files if renumbering is needed.

**Commit:** `docs: record the web UI decisions`

1. **Decisions.** Check the highest D number on `main`. If D119–D130 are free, add them to the decision table as written in [README.md § Decisions](README.md#decisions), one row each, in the existing table style. Keep the topic column short: `UI scope`, `UI layout`, `Trace view`, `UI tokens`, `Charts`, `Filter bar`, `Times`, `UI mocks`, `UI tests`, `UI lists`, `Live mode`. If any number is taken, take the next free ones, renumber this plan's files in the same commit and report the mapping. D124 replaces ECharts: also edit D-row text elsewhere in `mvp-decisions.md` only if a row names ECharts. If one does, say so in the report and change only that word.
2. **Spec 11.** Edit in place, no "previously" notes:
   - **Stack line:** ECharts → Recharts through shadcn `chart` (D124). Add JetBrains Mono beside Inter (D123).
   - **New § Layout and density** after § Navigation:
     - the top bar and collapsible sidebar (D120);
     - the 13 px base and fixed 28 px rows;
     - mono for IDs and numbers;
     - dark by default;
     - times local with a UTC setting, RFC 3339 on hover (D126).
   - **§ Navigation:** add `/:org/:project/spans`. Mark the routes not yet built by this track as "(own plan, once its API contract exists; D119)". Keep them listed, because spec 11 remains the full product UI.
   - **§ Traces (list):**
     - row click → **resizable split** with `?trace=` (D121), replacing "wide drawer";
     - bulk actions marked as D119;
     - list the URL params from [README.md § Route and URL map](README.md#route-and-url-map-at-the-end-of-c).
   - **§ Trace view:**
     - the left pane is the **combined tree + timeline grid** (D122), replacing "toggle to a timeline";
     - add the **Transcript** toggle (`mode=transcript`);
     - Scores tab: read-only until `POST /scores` has a contract (D119);
     - trace header actions: copy link and copy ID now, the others D119.
   - **New § Spans (list):** same table, filter bar and split as Traces, over `GET /spans` (D129).
   - **§ Filter bar:** add a short subsection describing D125 (text is the truth, chip shapes, server validation, no facet sidebar yet).
   - **§ Dashboard:** "ECharts" → "Recharts"; empty buckets are gaps for rates and percentiles.
   - **§ Quality bar:** add "verified by bounded rendered-row counts in tests (D128)".
3. **Spec 04 § Saved views.** The `state` column comment becomes `-- JSON owned by the UI: the list page's search params (range|from+to, filter, search, sort, order, eval, exp, cols) or {query, params} for SQL`. That matches spec 09 § Response shapes ("its URL search params"). Keep the sentence about relative time ranges, rewritten as "Preset ranges are stored relative (`range: "24h"`)".
4. **Research README.** Replace § Open questions with "Answered (2026-11-11)": Transcript ships as a toggle (D122); no facet sidebar yet (D125, R3); traces and spans are separate pages (D129).

**Checks:** `just check` (docs only, but run it).

**Report:** the numbers used and any renumbering.

---

## U2 — Dependencies, tokens and theme

**Edit only:** `web/package.json`, `pnpm-lock.yaml`, `web/src/main.tsx`, `web/src/styles.css`, `web/src/components/theme-provider.tsx` and its test, `web/src/components/theme-toggle.tsx` and its test.

**Commit:** `web: dense tokens, mono font and dark default`

1. **Font.** Add `@fontsource-variable/jetbrains-mono` 5.3.0 and import it in `main.tsx` next to Inter. In `styles.css`, set `--font-mono: "JetBrains Mono Variable", ui-monospace, SFMono-Regular, Menlo, monospace;`.
2. **Density.** In `styles.css`:
   - `html { font-size: 13px; }` sets the base. Tailwind's `rem` scale then shrinks with it, which is intended. Check that the existing header, menus and the org home still look right, and fix any obvious overflow.
   - Add `--row-height: 28px` and expose it as a Tailwind theme value `--spacing-row: var(--row-height)` so `h-row` works.
   - `--radius: 0.375rem`: smaller radii suit a dense UI.
   - Numbers in tables use `font-variant-numeric: tabular-nums`. Add a utility class `.num` (mono + tabular nums + right aligned).
3. **Colour tokens.** Keep shadcn's neutral base. Add, in `:root` and `.dark`, using oklch like the rest:
   - **Status:** `--status-ok`, `--status-error`, `--status-unset`. Green, red and neutral grey; readable on both backgrounds.
   - **Span kinds:** `--kind-llm`, `--kind-embedding`, `--kind-tool`, `--kind-agent`, `--kind-chain`, `--kind-retriever`, `--kind-reranker`, `--kind-guardrail`, `--kind-evaluator`, `--kind-span`. Ten distinguishable hues at similar lightness. They are used for kind icons and timeline bars.
   - **Charts:** replace the grey `--chart-1…5` with five distinguishable hues (both themes).
   - **Accent:** one accent hue (blue) as `--primary` in dark and light, used for focus rings, the selected row and links. The selected row background is `--selection`, a low-alpha accent.

   Expose them under `@theme inline` (`--color-status-ok`, `--color-kind-llm`, …) so Tailwind classes exist.
4. **Dark by default.** `readStoredTheme()` returns `"dark"` when nothing is stored (it returned `"system"`). The toggle still offers light, dark and system. Update the existing tests and add one: with an empty localStorage and a system preference for light, the resolved theme is dark.

**Tests:** the theme tests above. `just check-web` (the build must still pass `check-dist`).

**Report:** a short note on anything the 13 px base broke and how it was fixed.

---

## U3 — Formatting and small primitives

**Edit only:** `web/src/lib/format.ts` (+ test), `web/src/lib/settings.ts` (+ test), `web/src/components/{status-dot,kind-icon,copy-button,id-text,time-text}.tsx` (+ tests), `web/src/components/user-menu.tsx` (+ its test), and `web/src/components/ui/tooltip.tsx` (shadcn `tooltip`).

**Commit:** `web: formatters, ids, status and kind primitives`

`lib/format.ts`. Pure functions, no React. All inputs are API values (RFC 3339 strings with 9 fractional digits, `*_ns` integers, USD floats):

| Function | Behaviour |
|---|---|
| `formatDuration(ns: number): string` | `< 1 ms` → `"850µs"`; `< 1 s` → `"412ms"`; `< 60 s` → `"4.20s"` (2 decimals below 10 s, 1 below 60 s); `< 1 h` → `"3m 05s"`; else `"2h 14m"`. 0 → `"0ms"`. |
| `formatTokens(n?: number): string` | `undefined` → `"—"`; `< 1000` as is; else `"12.3k"`, `"1.2M"`. |
| `formatCost(usd?: number): string` | `undefined` → `"—"` (spec 02: never `$0` for unknown); `0` → `"$0"`; `< 0.01` → 4 significant digits (`"$0.003060"`→`"$0.00306"`); else 2 decimals; `≥ 1000` → `"$1.2k"`. |
| `formatTimestamp(rfc3339: string, zone: "local" \| "utc", style: "time" \| "datetime"): string` | `"10:00:04.200"` or `"2026-01-01 10:00:04.200"`, with milliseconds, in the zone. Parse the instant with `Date` (ms precision); keep the original string for tooltips. |
| `formatRelative(rfc3339: string, now: number): string` | `"3m ago"`, `"2h ago"`, `"5d ago"`. |
| `shortId(hex: string): string` | the first 8 characters. |
| `nsBetween(a: string, b: string): number` | the difference in ns. Parse the fractional part as an integer so ns precision survives, because `Date` drops it. |
| `toNs(rfc3339: string): bigint`-free version: `instantNs(rfc3339): number` | ns since the epoch as a JS number. It's exact enough for layout: microsecond precision at 2026 magnitudes. Document that. |

`lib/settings.ts`: `type TimeZoneSetting = "local" | "utc"`, `useTimeZone()` and `setTimeZone()` backed by localStorage key `` `${STORAGE_PREFIX}time_zone` `` with a `useSyncExternalStore` subscription so every `TimeText` re-renders. A missing or invalid value means `"local"`. localStorage errors are swallowed, as in the theme provider.

Components:
- `TimeText({value, style})`: the formatted time. The tooltip shows the full RFC 3339 string, the other zone and the relative time.
- `IdText({value, kind})`: `shortId` in mono. The tooltip shows the full ID. A `CopyButton` appears on hover and focus.
- `CopyButton({value, label})`: `navigator.clipboard.writeText`, then a check icon for 1.5 s; `aria-label="Copy <label>"`.
- `StatusDot({status})`: a dot with the status colour plus visually hidden text (`"error"`, `"ok"`, `"unset"`). The error variant is a filled red dot with a ring, so it differs by shape too.
- `KindIcon({kind})`: one lucide icon per `SpanKind`, tinted with the kind token, with `aria-label` the kind name. Mapping:

  | Kind | Icon |
  |---|---|
  | `llm` | `Sparkles` |
  | `embedding` | `Binary` |
  | `tool` | `Wrench` |
  | `agent` | `Bot` |
  | `chain` | `Link` |
  | `retriever` | `Search` |
  | `reranker` | `ListOrdered` |
  | `guardrail` | `ShieldCheck` |
  | `evaluator` | `Gauge` |
  | `span` | `Circle` |

User menu: add a "Show times in UTC" checkbox item bound to the setting.

**Tests:**
- every row of the format table, including the boundaries (999 µs, 1 ms, 9.999 s, 59.9 s, 1 h);
- `nsBetween` keeping nanoseconds (`…00.000000001Z` vs `…00.000000000Z` → 1);
- the settings store, and that a `TimeText` re-renders on toggle;
- the user-menu item;
- `CopyButton` calling the clipboard (stub `navigator.clipboard` in the test).

---

## U4 — Seeded mock data

**Edit only:** `web/src/mocks/generated/**` (new), `web/src/mocks/browser.ts`, `web/src/main.tsx`, `web/scripts/check-dist.mjs`.

**Commit:** `web: seeded large mock dataset`

Purpose (D127): dense pages and the 10 000-span target need volume that the 6-trace example project doesn't have.

**Files:**
- `generated/random.ts`: `mulberry32(seed)` → `() => number`. No `Math.random` anywhere in `generated/`.
- `generated/dataset.ts`: `createDataset(options: {seed: number; now: number; traces: number}): Dataset`. Everything is typed with `Schema<…>` from `@/api/types`:
  - `traces: Schema<"TraceSummary">[]`. 5 000 traces by default, start times spread over the last 30 days relative to `now`, with denser activity in the last 24 h.
    - Names drawn from about 12 agent and workflow names; models from a list of 6.
    - About 8 % errors, some sessions (`sess-NNNN`) and users (`user-NNN`), tags, environments `production` / `staging`.
    - About 3 % `llmobs-eval`, which the default lists hide unless `include_eval`.
    - Latest scores for 2 score names on half the traces.
  - `traceDetail(traceId): Schema<"TraceDetail"> | undefined`: deterministic per trace (seeded by the trace index).
    - Span counts 3–60, with depth up to 6.
    - **The trace at index 0 is the big one: 10 000 spans**, depth up to 8, with a few `detached` roots.
    - Children start inside their parent's interval; some LLM spans overlap (parallel tool calls).
  - `spanDetail(traceId, spanId): Schema<"SpanDetail"> | undefined`: payloads by kind.
    - `llm` spans: `messages` input (system, user, assistant with a tool call, tool response) and output.
    - `tool` spans: `json` input and output.
    - Others: `text`.
    - `attributes`, `resource_attributes`, events (one `exception` event with `exception.stacktrace` on error spans), and `truncated` on a few.
  - `sessions`, `users`: aggregated from `traces` (count, cost, tokens, errors, first/last seen), consistent with the trace list.
  - `metrics(from, to, bucket)`: series computed from `traces` (counts, errors, error rate, p50/p95/p99 durations, cost, tokens by type, cost by model, `score_avg`, `top_models`, KPIs with `previous`).
- `generated/handlers.ts`: `largeHandlers(dataset): HttpHandler[]` for `list_traces`, `list_spans`, `get_trace`, `get_span`, `list_sessions`, `get_session`, `list_users`, `get_user`, `get_metrics`, and `get_project`, `get_schema` passing through to the fixture bodies. Lists implement:
  - `from`/`to` (both formats of `TimeParam`);
  - `sort`/`order` for the enum keys;
  - `limit` (default 50, 1–1000, else a 400 `Problem` `bad_request`);
  - `include_eval`/`include_experiments`;
  - `search`: a case-insensitive substring over `name`, `input_preview` and `output_preview`.
  - **cursor**: an opaque base64 of the offset. A cursor that doesn't decode is a 400.
  - **`filter` is not evaluated.** A filter that equals the literal text `bad` returns `400 invalid_filter` with `errors: [{field: "filter", message: "unknown column bad", position: {line: 1, column: 1}}]`, so the filter-error UI can be tried by hand. Any other filter is ignored, and the response header `X-Mock-Filter-Ignored: 1` says so.
  - `get_span` for unknown IDs is a 404 `Problem`.
  - `list_spans` flattens the summaries of all generated traces, built lazily: generate the detail of a trace only when a page reaches it, and cache it in a `Map`.
- **Dev switch** (`main.tsx` → `browser.ts`): when the page URL has `mock=large` (or sessionStorage `${STORAGE_PREFIX}mock` is `large`), the worker starts with `largeHandlers(createDataset({seed: 1, now: Date.now(), traces: 5000}))` **prepended** to the fixture handlers, and sessionStorage remembers the choice. `mock=fixtures` clears it. The module is loaded with a dynamic `import()` inside the existing DEV-only branch, so it never reaches the production bundle.
- **`check-dist.mjs`:** add the needle `mulberry32` to the forbidden list.

**Performance:** `createDataset` with 5 000 traces must not build span details eagerly. It takes under 200 ms in Vitest; assert < 1 s to avoid flakes.

**Tests (Vitest, node handlers through `server.use(...largeHandlers(ds))`):**
- the dataset is deterministic for a seed: the same first trace ID and the same trace-0 span count;
- trace 0 has 10 000 spans in valid pre-order: each span's depth is at most the previous depth + 1, and parent IDs are consistent;
- the list cursor pages through without duplicates or gaps (3 pages of 50);
- `sort=duration&order=asc` is sorted;
- `include_eval` toggles the eval rows;
- a bad cursor gives 400;
- `filter=bad` gives `invalid_filter` with a position;
- sessions and users totals match their traces.

---

## U5 — App shell and project layout

**Edit only:** `web/src/components/shell/**` (new), `web/src/components/app-header.tsx` (removed or folded into `shell/`), `web/src/routes/$org/route.tsx`, `web/src/routes/$org/index.tsx`, `web/src/routes/$org/$project/route.tsx` (new), `web/src/routes/$org/$project/index.tsx` (new), `web/src/routes/$org/$project/traces/index.tsx` (new, minimal), `web/src/api/queries.ts`, shadcn `sidebar` (and what it pulls in: `sheet`, `separator`, `input`, `skeleton`), plus tests.

**Commit:** `web: app shell with project sidebar`

- **Top bar** (`shell/top-bar.tsx`), 40 px high. Contents:
  - the product name, linking to `/`;
  - an **org / project breadcrumb switcher**. The org part reuses the existing org switcher. The project part is a dropdown of the org's projects (`list_projects`, first page of 50, with a "Load more" item when `next_cursor`), and choosing one navigates to `/$org/$project/traces`;
  - a flexible spacer;
  - `ThemeToggle`;
  - `UserMenu`.

  The ⌘K button comes with U24.
- **Sidebar** (`shell/project-sidebar.tsx`). The shadcn `sidebar` component, `collapsible="icon"`. Its open state is in localStorage `${STORAGE_PREFIX}sidebar` (shadcn's cookie default is replaced by localStorage). Items come from one array in `shell/nav.ts`: `{to, label, icon}`. **This sub-phase adds only "Traces"**. Each later page sub-phase appends its entry, so the sidebar never links to a missing page (D120). The active item is highlighted; `⌘B` / `Ctrl+B` toggles the sidebar (shadcn default).
- **Org layout** (`$org/route.tsx`): the top bar plus `<Outlet/>`. The `max-w-5xl` container moves into the org home route only, so project pages use the full width.
- **Project layout** (`$org/$project/route.tsx`):
  - the loader `ensureQueryData` of `get_project` with `projectRef(org, project)`. A 404 `Problem` means `notFound()`;
  - the sidebar plus a full-height content area (`h-[calc(100dvh-40px)]`, `overflow-hidden`; pages manage their own scroll);
  - the project is put in a React context `ProjectContext {org, project, ref, data}` (`shell/project-context.tsx`) so pages don't re-derive it.
- `$org/$project/index.tsx` redirects to `./traces`.
- `$org/$project/traces/index.tsx`, in this sub-phase: a page title row ("Traces") and, below it, a plain non-virtualized list of the first page of `list_traces`. Columns: status, start time, name and duration, using U3's primitives. U6 replaces this list with the real table.
- **Org home:** each project name links to `/$org/$project/traces`. Remove the comment about the traces track.

**Tests:**
- `/acme/support-bot` redirects to `/acme/support-bot/traces`, and the sidebar shows "Traces" as active;
- an unknown project (override `get_project` → 404) shows the not-found page;
- the project switcher lists projects and navigates;
- the sidebar collapse persists in localStorage across a re-render;
- the org home's project link targets the traces route;
- the existing shell tests still pass, updated to the new header.

---

## U6 — The dense table and the first traces list

**Edit only:** `web/package.json`, `pnpm-lock.yaml`, `web/src/components/data-table/**` (new), `web/src/lib/search/**` (new), `web/src/routes/$org/$project/traces/index.tsx`, `web/src/api/queries.ts` (or a new `queries-traces.ts`), `web/src/test/layout.ts` (new), `web/src/test/setup.ts`, plus tests.

**Commit:** `web: virtualized data table and traces list`

Add `@tanstack/react-table` 9.3.1 and `@tanstack/react-virtual` 3.14.26.

### `DataTable<Row>`

`components/data-table/data-table.tsx`:

```ts
interface DataTableProps<Row> {
  tableId: string;                       // localStorage key for column sizes: `${STORAGE_PREFIX}table.<tableId>`
  ariaLabel: string;
  columns: ColumnDef<Row>[];             // TanStack v9 column defs; `meta: {align?: "left"|"right"; mono?: boolean}`
  rows: Row[];
  getRowId: (row: Row) => string;
  selectedRowId?: string;
  onActivate: (row: Row) => void;        // click or Enter
  onSelectionMove?: (row: Row) => void;  // j/k ↑/↓ when a row is open (U7 uses it)
  hasNextPage: boolean;
  isFetchingNextPage: boolean;
  fetchNextPage: () => void;
  sorting?: { id: string; desc: boolean }; // controlled from the URL (U10)
  onSortingChange?: (next: { id: string; desc: boolean }) => void;
  emptyState: React.ReactNode;
}
```

- **Structure:** a `div` grid with `role="grid"`, `aria-label`, `aria-rowcount` (rows + header) and `aria-colcount`. The header row is sticky, 28 px, muted foreground, uppercase 11 px labels. Rows are `role="row"` with `aria-rowindex` (header is 1), cells `role="gridcell"`. Use CSS grid columns from the column sizes, not `<table>`, so virtualization and sticky headers are easy.
- **Rows:** 28 px fixed (`--row-height`), single line, `truncate` text.
  - Hover shows a subtle background, and the selected row `--selection` with a 2 px accent bar on the left.
  - Numbers use `.num`, and `meta.align` right-aligns.
- **Virtualization:** `useVirtualizer` with `estimateSize: () => 28`, overscan 12, on a scroll container marked `data-scroll-container`. Only visible rows are in the DOM.
- **Infinite loading:** when the last rendered index is within 20 rows of `rows.length` and `hasNextPage`, call `fetchNextPage` (once per page; guard with `isFetchingNextPage`). A 28 px loader row shows while fetching.
- **Column sizing:**
  - drag handles on header cells;
  - the minimum is 48 px;
  - sizes persist per `tableId` in localStorage;
  - a double-click on a handle resets the column.
- **Keyboard:** the grid container is focusable (`tabIndex=0`).
  - ↑/↓ and k/j move the focused row; Home/End go to the first and last loaded row; Enter activates.
  - The focused row scrolls into view (`virtualizer.scrollToIndex`).
  - When `selectedRowId` is set and the focus moves, `onSelectionMove` fires, so the open trace follows the focus (D121).
- **Empty state:** when `rows.length === 0` and not loading.

`components/data-table/use-infinite-list.ts`: a small helper around `useInfiniteQuery` + `api.GET`, shaped like `projectListOptions`. It flattens `pages[].data`, keeps the `next_cursor` logic, and is typed per operation. Keep it concrete for the operations that use it; don't build a generic over all paths if typing fights back. Two or three explicit option factories are fine.

### Search params

`lib/search/common.ts`. Plain validators:
- `parseEnum(value, allowed)`;
- `parseHex(value, length)` (lowercase, 32 or 16 characters);
- `parseFlag(value)` (`"1"` → `true`);
- `parseString(value, max)` (trim; drop when empty or longer than `max`; 4 KiB for `filter`, 1 KiB for `search`);
- `parseRfc3339(value)` (drop invalid).

Each returns `undefined` for invalid input. `lib/search/traces.ts`: `type TracesSearch = {sort?, order?, trace?, span?}` for now. Later sub-phases extend it.

### Traces list (replaces U5's plain list)

`routes/$org/$project/traces/index.tsx`:
- **Full height:** a 36 px toolbar row (title "Traces" and the row count "N loaded") and the `DataTable` below, filling the rest.
- **Query:** `list_traces` with `limit=100`, the sort and order from the URL, and **the last 24 h** (`from = now − 24 h`, computed once when the page mounts and kept in state; U9 replaces this with the time range).
- **Default columns, in order:**

  | Column | Width | Content |
  |---|---|---|
  | status | 28 px | `StatusDot` |
  | start | 132 px | `TimeText` "datetime" |
  | name | 200 px | |
  | input | 280 px | preview, muted, truncated |
  | output | 280 px | as input |
  | duration | 80 px | right-aligned |
  | tokens | 72 px | `total_tokens`, right-aligned; tooltip: input, output, cache read/write, reasoning |
  | cost | 80 px | right-aligned |
  | models | 140 px | first model + "+N" |
  | session | 120 px | `session_id`, mono |
  | user | 100 px | `user_id`, mono |
  | tags | 140 px | small badges, overflow "+N" |
  | env | 88 px | `environment` |

  U10 adds scores columns, the chooser and sorting.
- **Activate:** set `?trace=<trace_id>`. U7 uses it; in this sub-phase the selected row is just highlighted.

### `test/layout.ts`

jsdom has no layout, so the virtualizer would render nothing. `installLayout()`, called from `setup.ts`, defines getters on `HTMLElement.prototype` that return 800 (height) / 1200 (width) for `clientHeight`, `offsetHeight`, `clientWidth`, `offsetWidth` and `getBoundingClientRect` **only for elements with `data-scroll-container`**, and the default otherwise. Restore the originals in `afterAll`. If TanStack Virtual reads other measurements, cover them here and say which in the report. No module mocks.

**Tests:**
- the traces page renders the fixture traces with the default columns (6 fixture traces, 5 visible by default because trace 5 is eval; check against `responses/traces.json`);
- with `largeHandlers` (5 000 traces): **fewer than 60 rows in the DOM**; scrolling the container to the bottom (set `scrollTop` and dispatch `scroll`) fetches the next page (assert a second request with a `cursor`);
- j/k and Enter: Enter sets `?trace=` in the router location;
- column resize persists to localStorage (drag simulated with pointer events on the handle; or test the sizing reducer directly if pointer simulation is unreliable, and say so);
- the empty state renders for an empty page (`server.use` returning `{data: [], next_cursor: null}`).

---

## U7 — Trace view, first version

**Edit only:** `web/package.json`, `pnpm-lock.yaml`, shadcn `resizable` and `tabs`, `web/src/lib/tree.ts` (+ test), `web/src/lib/timeline.ts` (+ test), `web/src/components/trace/**` (new), `web/src/routes/$org/$project/traces/index.tsx`, `web/src/lib/search/traces.ts`, `web/src/api/queries.ts` (or `queries-traces.ts`), plus tests.

**Commit:** `web: trace view with combined tree and timeline`

Add `react-resizable-panels` 4.17.0 via shadcn `resizable`. Check the generated component against v4's API; it compiles with this version or the report says what was adapted.

### `lib/tree.ts` (pure)

Input: `TreeSpan[]` (pre-order with `depth`, spec 09 § Response shapes). All functions are O(n) and non-recursive (stack-based), so 20 000 spans are fine.
- `subtreeEnds(spans): Int32Array`: for each index `i`, the index one past its last descendant. A descendant run is the following spans with `depth > spans[i].depth`.
- `visibleRows(spans, ends, collapsed: Set<string>): number[]`: indices to render, skipping the descendants of collapsed span IDs.
- `defaultCollapsed(spans): Set<string>`: empty when `spans.length ≤ 1000`; otherwise the IDs of every span at depth ≥ 2 that has children, so the tree opens at depth 2.
- `ancestorsOf(spans, index): number[]`: so selecting a span expands its path.
- `hasChildren(ends, i)`.

### `lib/timeline.ts` (pure)

- `traceWindow(trace: TraceSummary, spans): {startNs, endNs}`: the min start and max end over the trace and its spans; at least 1 µs wide.
- `barGeometry(span, window): {leftPct, widthPct}`: clamped to [0, 100]; a minimum visible width of 0.15 %.
- `ticks(window, widthPx): {ns, label}[]`: 4–8 "nice" ticks (1, 2, 5 × 10^k ns), labelled with `formatDuration` relative to the window start.

### Components

- `trace/trace-view.tsx`: `TraceView({traceId, spanId, onSelectSpan})`. It loads `get_trace` (query key includes the trace ID) and renders:
  - **Header** (32 px): name, `StatusDot`, duration, total tokens, cost, `IdText` of the trace. U19 completes it.
  - **A horizontal resizable split.** On the left, the **tree grid** (default 55 %). On the right, the **span detail** (45 %). The ratio persists in localStorage `${STORAGE_PREFIX}split.trace`.
  - `truncated: true`: a warning bar "Showing the first 20 000 spans".
- `trace/tree-grid.tsx`: a virtualized grid like `DataTable`, but with its own row renderer. Each 28 px row has:
  - **name cell:** indentation `depth × 12 px`, a chevron when it has children (click toggles), `KindIcon`, the name, a red `StatusDot` only on errors, and a "detached" badge on detached roots;
  - **metrics cells:** duration, tokens (LLM and embedding only) and cost (when present), right-aligned mono;
  - **timeline cell:** fills the remaining width. A bar in the kind colour at `barGeometry`; error spans get a red outline. The header shows `ticks`.

  Behaviour:
  - Selection highlights the row; a click selects (`onSelectSpan`).
  - ↑/↓ moves the selection; → expands; ← collapses or goes to the parent; Enter toggles.
  - The default selection is the root span (the first row), or `?span=` when present, with its ancestors expanded.
- `trace/span-detail.tsx`: loads `get_span(traceId, spanId)`. It shows:
  - a header line: `KindIcon`, name, model, duration, `StatusDot` + `status_message`;
  - **tabs:** "Input / Output" only, in this sub-phase. It shows `input` and `output` as `<pre>` blocks of pretty JSON for `json` and `messages`, or the text for `text`, with a 1 MB rendering cap: beyond it, a "show raw" button. U16–U19 add the other tabs and the real viewers.
  - While loading, skeleton lines; on error, an inline `Problem` title.

### Traces page integration (D121)

- With `?trace=` set, the page becomes a horizontal resizable split: the list on the left (default 40 %) and `TraceView` on the right. The ratio persists in `${STORAGE_PREFIX}split.traces`.
- The list keeps its scroll and its selected row.
- **Esc** (when focus is not in an input) removes `trace` and `span` from the URL.
- **j/k in the list** moves the open trace (`onSelectionMove` → replace `?trace=`, drop `?span=`).
- Selecting a span sets `?span=` with `replace: true`, so history doesn't fill up.
- `lib/search/traces.ts` gains `trace` and `span`.

**Tests:**
- **`tree.ts`:** subtree ends on a hand-built 7-span tree with two roots and a detached root; `visibleRows` with collapse; `defaultCollapsed` threshold; **10 000 generated spans: `subtreeEnds` + `visibleRows` return correct counts** (no timing assertion).
- **`timeline.ts`:** geometry clamps, the zero-duration span minimum width, nice ticks.
- **Page:** clicking a fixture trace opens the split with the tree (trace 1 has 4 spans in pre-order), and the root is selected and its detail loaded (`get_span` fixture).
  - Clicking another span loads its detail and sets `?span=`.
  - Esc closes the split.
  - j in the list switches to the next trace.
  - **Large:** open the 10 000-span trace through `largeHandlers`; **fewer than 80 tree rows are in the DOM**, and expanding a collapsed node adds its children.

---

## U8 — Review gate

**No code. No commit.**

The implementer checks the list below, then **stops and hands over to the user**. The report is the checklist with notes and anything that looked wrong. The coordinating session turns the user's feedback into sub-phases U8a, U8b, … before U9.

Run `just web-dev`, then open:
1. `http://localhost:5173/acme/support-bot/traces`: the fixture project, in dark (default) and light.
2. The same with `?mock=large`: 5 000 traces. Scroll fast; the rows must keep up, and the infinite load must be seamless.
3. Open trace 1 of the fixtures (4 spans) and, in large mode, the first trace of the generated list (10 000 spans): expand, collapse, select spans, resize both splits, use j/k in the list.
4. Collapse the sidebar, switch org and project, toggle UTC in the user menu.

What to show the user, and ask about:
- **density:** 13 px text, 28 px rows, column widths, mono numbers;
- **colours:** status, span kinds, the accent, dark and light;
- **the split layout:** list | tree+timeline | span detail, and the default ratios;
- **the combined tree and timeline:** bar colours, tick labels, indentation;
- anything that feels slow.

The user may also ask for changes to this plan's later sub-phases. Those go into the plan files before U9 starts.
