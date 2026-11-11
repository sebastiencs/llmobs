# C · Web UI on mocks

Track C of Wave 1 builds the observability pages of spec 11 in `web/`. They run against the MSW mocks from `fixtures/api/` (D72, D76) and against a seeded mock generator in `web/` (D129). A real server is not needed. The pages in scope are the ones whose API contract exists today in `crates/server/openapi.json`:
- the traces and spans lists;
- the trace view;
- sessions and users;
- the dashboard;
- saved views, the filter bar and the ⌘K palette;
- live mode.

The visual direction is a **dense developer tool**, closer to Laminar, Datadog, Linear and Grafana than to an airy SaaS UI.

The plan has 29 sub-phases, **U1–U29**. One agent does each one, in order, and each ends with `just check-web` green (and `just check` when the sub-phase touches docs only) and one commit. **U8 is a review gate:** nothing after it starts until the user has looked at the UI and said to continue.

**Read first, for every sub-phase:**
- `AGENTS.md`;
- `docs/spec/11-ui.md` (after U1, with this plan's decisions);
- `docs/spec/09-api.md` § Conventions and § Response shapes;
- `docs/spec/05-query.md` § Filter DSL;
- `fixtures/api/README.md`;
- `docs/research/ui/README.md`;
- the code this plan points at in `web/src/` (`api/`, `mocks/`, `test/`, `components/`);
- the sub-phase's own section.

## Files

| File | Sub-phases |
|---|---|
| [01-foundations.md](01-foundations.md) | U1 spec and decisions · U2 dependencies, tokens and theme · U3 formatting and small primitives · U4 seeded mock data · U5 app shell and project layout · U6 the dense table and the first traces list · U7 trace view, first version · **U8 review gate** |
| [02-lists-and-filters.md](02-lists-and-filters.md) | U9 time range, search and toggles · U10 columns and sorting · U11 the DSL chip parser · U12 the filter bar · U13 the spans page · U14 saved views |
| [03-trace-view.md](03-trace-view.md) | U15 timeline interactions and tree search · U16 span header and attributes · U17 the JSON viewer and the Input/Output tab · U18 the Chat tab and media · U19 scores, the trace header and the full page · U20 Transcript mode |
| [04-pages.md](04-pages.md) | U21 sessions · U22 users · U23 dashboard · U24 ⌘K palette and shortcuts · U25 onboarding empty state |
| [05-live-and-closeout.md](05-live-and-closeout.md) | U26 the SSE client · U27 live traces list · U28 live trace view · U29 quality bar and closeout |

## Decisions

These were settled with the user on 2026-11-11. U1 writes them into `docs/mvp-decisions.md` and spec 11.

**Numbering:** U1 checked on 2026-11-11. `main` ends at D113. This branch already had D118, and B2's plan has D114–D120, so the draft IDs D119–D130 moved to **D121–D132**: D119→D121, D120→D122, D121→D123, D122→D124, D123→D125, D124→D126, D125→D127, D126→D128, D127→D129, D128→D130, D129→D131, D130→D132.

| # | Decision | Source |
|---|---|---|
| D121 | **C's scope is the pages whose API contract exists.** The pages are traces, spans, the trace view, sessions, users, the dashboard, saved views, the filter bar, the ⌘K palette and live mode. Several pages and actions have no contract yet, and the tracks that own them add it; they get their own UI plans then. Those are: the SQL console (`/sql`), datasets, experiments, evaluators and online rules, alerts, settings, API keys, members, login/signup/setup, bulk actions (add to dataset, delete, export), the annotation form, "run evaluator on this trace", and GDPR deletion. C only requests contracts; it never writes them. | User |
| D122 | **Layout.** A thin top bar (product, org/project switcher, ⌘K, theme, user) and a **collapsible left sidebar** with the project pages. The sidebar collapses to icons and its state is in localStorage. It lists only pages that exist. | User |
| D123 | **A trace opens in a resizable split beside the list.** Clicking a row sets `?trace=<id>` and the list keeps its scroll and selection. j/k (or ↑/↓) move between rows and the open trace follows; Esc closes the split. The split ratio is in localStorage. The same trace view is also a full page at `/:org/:project/traces/:traceId` for links. Inside the trace view, the span tree and the span detail are a second resizable split. There is no drawer and no modal. | User |
| D124 | **The span tree and the timeline are one combined grid.** Each virtualized row has an indented name (kind icon, status), right-aligned duration, tokens and cost, and a bar on a shared time axis. It has the same rows, selection and scroll as the tree. A **Transcript** toggle shows the trace's LLM and tool spans in order as one conversation. The combined grid is the default, and the mode is remembered in the URL (`mode=transcript`). | User, research |
| D125 | **Visual tokens.** Inter (already bundled) for text and JetBrains Mono (bundled, `@fontsource-variable/jetbrains-mono`) for IDs, numbers in tables, JSON and code. The base text is 13 px. Table rows are a **fixed 28 px**, single-line, with previews truncated and full on hover. Theme: **dark by default**; light and system stay selectable. Status and span-kind colours are tokens (U2). No external network calls (spec 11 § Quality bar). | User |
| D126 | **Charts use Recharts through shadcn's `chart` component**, not ECharts. Recharts is in the dashboard route's chunk only. Empty buckets show as gaps for rates and percentiles. | User |
| D127 | **In the filter bar, the text is the truth.** One single-line CodeMirror editor holds the DSL (spec 05). A small client parser splits top-level `AND` conjuncts and recognises simple shapes (`column op literal`, `IS [NOT] NULL`, `[NOT] IN (…)`, `@>`, `score('n') op x`, `has_score('n')`) as editable chips. Everything else is a raw chip. The server stays the validator: a `400 invalid_filter` underlines the position and shows the message. A broken filter is never applied silently. Autocomplete comes from `GET /schema`. There is no facet sidebar in this plan. | User, research |
| D128 | **Times.** Local time zone by default, with a "Show times in UTC" setting in the user menu (localStorage). The full RFC 3339 with nanoseconds is in the tooltip and in what copy puts on the clipboard. | User |
| D129 | **Mock data.** `fixtures/api/` stays the small contract-checked set. A seeded, deterministic generator in `web/src/mocks/generated/` (typed by `schema.gen.ts`) produces thousands of traces and one 10 000-span trace. `just web-dev` switches to it with `?mock=large`, and tests use it through `server.use`. Saved views get stateful in-memory handlers in `web/src/mocks/` for POST, PATCH and DELETE. | User |
| D130 | **Tests are Vitest and Testing Library only** (D74). Page tests run on the MSW mocks with `server.use` overrides and never mock modules. Pure modules (tree rows, the timeline scale, the DSL chip parser, URL state, formatters) get unit tests. The quality-bar targets are checked by bounded rendered-row counts and by pure-function tests on 10 000 spans, not by timing. The U8 gate covers visuals. There is no Playwright. | User |
| D131 | **Main lists.** Traces (one row per trace) and Spans (one row per span) are separate pages with the same table and filter bar, as in spec 11. There is no spans-first default. | User |
| D132 | **Live mode is in this plan** (U26–U28). It uses `EventSource` on `/traces/stream` and `/traces/{id}/stream` (spec 09 § SSE), mocked with MSW's `sse()`. | User |

## Boundaries

```
web/  ── openapi-fetch / openapi-react-query ──▶  crates/server/openapi.json (contract, D69)
web/src/mocks/handlers.ts  ◀── fixtures/api/manifest.json + responses (D72, generated by `just api-examples`)
web/src/mocks/generated/   seeded large dataset (D129), dev + tests only, never in dist/
web/src/mocks/saved-views.ts  stateful POST/PATCH/DELETE for saved views (dev + tests)
```

- **The API contract** is owned by the server and by the tracks that implement endpoints (D69). C reads `openapi.json` and `schema.gen.ts` and never edits them. When C needs an operation or a field, it writes the request in its report.
- **`fixtures/api/`:** `responses/` and `manifest.json` are generated (`just api-examples`) and must not be edited by hand. `dataset/` and `static/` are hand-written. The only change in this plan is U14, which moves `static/saved-views.json` to the URL-state format. If a sub-phase finds a missing example that only the server-side generator can produce, it reports it instead.
- **Production build:** nothing under `web/src/mocks/` may reach `dist/`. `scripts/check-dist.mjs` already rejects `fixtures/api`, `setupWorker` and `ada@example.com`. U4 adds a needle for the generator.
- **No new `just` recipes.** C edits only `web/`, the hand-written parts of `fixtures/api/` (`static/saved-views.json` in U14) and docs. Large mocks are switched by a URL parameter, not a recipe.

### Requests for other tracks

None of these blocks this plan. They are recorded in U1's decision log and in the final report.

| # | For | Request |
|---|---|---|
| R1 | server / contract owners | Contracts for the pages excluded by D121: `POST /sql` (D98, D101), `POST /scores` and `/score-definitions` (annotation form), `/api-keys` (the onboarding snippet's key prefix), datasets, experiments, evaluators, online rules, alerts, channels, members, models, deletions and the auth forms. |
| R2 | server / A2 | **Batch span payloads for Transcript.** U20 fetches each LLM and tool span's detail separately (bounded concurrency, first 50). An endpoint returning the payloads of several spans of one trace (for example `GET /traces/{trace_id}/spans?ids=…&fields=input,output`) would turn N requests into one. |
| R3 | server / A2 | **Value suggestions for autocomplete** (future facet sidebar): distinct values with counts for low-cardinality columns (`status`, `model`, `environment`, `release`, `tags`, `span_kind`), scoped by the current filter and time range. |

## Shared rules for every sub-phase

- **Dependencies:** exact versions only, from the table below. Never add a dependency that isn't listed there. If one turns out to be needed, stop and report. `pnpm-workspace.yaml` enforces `minimumReleaseAge` (7 days), so `pnpm add` refuses anything younger.
- **shadcn components** are added with the shadcn CLI (`pnpm dlx shadcn@4.23.0 add <name>` from `web/`, same version as the pinned `shadcn` package) and committed as generated. Generated files under `components/ui/` may be restyled for density but are not rewritten.
- **URL state:** every view state that spec 11 lists (time range, filter, search, sort, columns, selection, mode) is a typed search param validated by the route's `validateSearch`. The validators are plain TypeScript functions in `web/src/lib/search/`, with no schema library. An invalid value is dropped, never thrown.
- **API calls** go through `$api` / `api` (`web/src/api/client.ts`). Query options live in `web/src/api/queries.ts`, or in a `queries-<area>.ts` next to it once that file passes about 300 lines. Paths use `projectRef(org, project)`.
- **Errors:** list and detail queries surface a `Problem` through the existing `ErrorState`, or inline for filter errors (U12). A 4xx is never retried (`retryQuery`).
- **Accessibility:** Radix primitives; every interactive element is reachable by keyboard; grids use `role="grid"` with `aria-rowcount` and `aria-rowindex`; status never relies on colour alone (icon or text too).
- **Tests:** `renderApp(url)` from `web/src/test/render.tsx`, MSW overrides with `server.use(...)`, and `userEvent`. No `vi.mock` of modules. Virtualized grids need a laid-out scroll container in jsdom; U6 adds `web/src/test/layout.ts` for that.
- **Privacy:** the UI logs nothing (`console.*` is not used in `src/`, except in the MSW worker start that already exists). Payloads never go into URLs, except the user's own filter and search text.
- **Done means:** `just check-web` is green, the commit is the sub-phase's only commit, and the report lists what was done, deviations, versions and anything unverified.

## Dependencies added by this plan

Looked up on 2026-11-11. Each is the newest release at least 7 days old.

| Package | Version | Added in | For |
|---|---|---|---|
| `@fontsource-variable/jetbrains-mono` | 5.3.0 | U2 | mono font (D125) |
| `@tanstack/react-table` | 9.3.1 | U6 | column model: visibility, sizing, order, sorting state. **v9 changed the API compared with v8**: follow the v9 types in `node_modules`, not v8 examples. |
| `@tanstack/react-virtual` | 3.14.26 | U6 | row virtualization |
| `react-resizable-panels` | 4.17.0 | U7 | splits (through shadcn `resizable`) |
| `@codemirror/state` | 6.7.6 | U12 | filter editor |
| `@codemirror/view` | 6.43.15 | U12 | |
| `@codemirror/commands` | 6.11.1 | U12 | |
| `@codemirror/language` | 6.13.1 | U12 | |
| `@codemirror/autocomplete` | 6.20.4 | U12 | |
| `@codemirror/lang-sql` | 6.10.0 | U12 | SQL highlighting of the DSL |
| `recharts` | 3.10.1 | U23 | charts (through shadcn `chart`) |
| `cmdk` | 1.1.1 | U24 | ⌘K palette (through shadcn `command`) |

If shadcn's generated component imports a package that isn't listed here (for example `react-is` for `chart`), pin the newest version at least 7 days old and say so in the report.

## The review gate (U8)

U8 is not code. The implementer starts `just web-dev`, checks the list in [01-foundations.md § U8](01-foundations.md#u8--review-gate) and hands the UI to the user. The user's feedback becomes a short list of adjustments, which the coordinating session turns into extra sub-phases (U8a, U8b, …) before U9. **No sub-phase after U8 starts before the user says so.**

## Route and URL map at the end of C

```
/$org                                       org home (projects; names link to traces)
/$org/$project                              → redirect to traces
/$org/$project/traces                       ?range|from&to ?filter ?search ?sort ?order ?eval ?exp ?cols ?view ?trace ?span ?mode ?live
/$org/$project/traces/$traceId              ?span ?mode
/$org/$project/spans                        ?range|from&to ?filter ?search ?sort ?order ?eval ?exp ?cols ?view ?trace ?span
/$org/$project/sessions                     ?range|from&to ?filter ?sort ?order ?eval ?exp ?view
/$org/$project/sessions/$sessionId          ?eval ?exp
/$org/$project/users                        ?range|from&to ?filter ?sort ?order ?eval ?exp ?view
/$org/$project/users/$userId                ?eval ?exp
/$org/$project/dashboard                    ?range|from&to ?filter ?group ?eval ?exp
```

Search params, everywhere they apply:

| Param | Values | Default |
|---|---|---|
| `range` | `15m` `1h` `6h` `24h` `7d` `30d` | `24h` |
| `from`, `to` | RFC 3339, both or neither, `from < to`; wins over `range` | — |
| `filter` | DSL text | — |
| `search` | free text (lists with `search` in the contract: traces, spans) | — |
| `sort` | the list's sort enum (`TraceSort`, `SpanSort`, `SessionSort`, `UserSort`) | `start_time` (traces, spans, sessions), `last_seen` (users) |
| `order` | `asc` `desc` | `desc` |
| `eval`, `exp` | `1` when on (→ `include_eval`, `include_experiments`) | off |
| `cols` | comma-separated column ids, in order | the table's defaults (U10) |
| `view` | saved view id | — |
| `trace` | 32 lowercase hex | — |
| `span` | 16 lowercase hex | — |
| `mode` | `tree` `transcript` | `tree` |
| `live` | `1` | off |
| `group` | `model` `environment` `release` | — |

## Module layout at the end of C

```
web/src/
  api/          client, problem, project-ref, types, queries*.ts
  lib/
    format.ts       time, duration, tokens, cost, ids (U3)
    time-range.ts   presets → {from, to} (U9)
    search/         validateSearch helpers per route (U6, U9, U10, …)
    dsl/            tokenizer, conjunct split, chip parse/serialize (U11)
    tree.ts         TreeSpan[] → subtree ends, visible rows (U7)
    timeline.ts     time scale, zoom window, ticks (U7, U15)
    sse.ts          EventSource wrapper (U26)
  components/
    ui/             shadcn (generated)
    shell/          top bar, sidebar, project switcher (U5)
    data-table/     DataTable, column helpers, infinite loader (U6, U10)
    filter-bar/     editor, chips, autocomplete (U12)
    time-range/     picker (U9)
    saved-views/    menu and dialogs (U14)
    trace/          trace view, tree grid, span detail, tabs, transcript (U7, U15–U20)
    payload/        JSON viewer, chat renderer, media (U17, U18)
    command/        ⌘K palette, shortcuts help (U24)
    charts/         KPI tile, chart wrappers (U23)
  mocks/
    manifest.ts handlers.ts browser.ts node.ts   (existing)
    saved-views.ts                               (U14)
    generated/      seeded dataset and handlers  (U4)
    sse.ts                                       (U26)
  routes/           file routes (TanStack Router)
  test/             setup, render, match-media, layout (U6)
```
