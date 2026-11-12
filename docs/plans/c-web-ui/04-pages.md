# C · Sessions, users, dashboard and navigation (U21–U25)

See [README.md](README.md) for the decisions, boundaries, dependency versions and the rules that apply to every sub-phase. These pages reuse:
- `DataTable`;
- `ListToolbar`;
- `TimeRangePicker`;
- `FilterBar`;
- `SavedViewsMenu`;
- `ChatView`/`MessageView`;
- `TraceView`.

---

## U21 — Sessions

**Edit only:** `web/src/routes/$org/$project/sessions/**` (new), `web/src/components/session-columns.tsx` (new), `web/src/components/session/**` (new), `web/src/lib/search/sessions.ts` (new), `web/src/components/shell/nav.ts`, `web/src/components/trace/{span-header,trace-header}.tsx` (session links), `web/src/api/queries.ts` (or `queries-sessions.ts`), plus tests.

**Commit:** `web: sessions list and conversation view`

### List `/sessions`

- **Toolbar:** time range, filter bar on the `sessions` view, eval and experiment toggles, columns, saved views (kind `sessions`). There's no search box: `list_sessions` has no `search` in the contract.
- **Columns:**

  | Id | Shown by default | Content |
  |---|---|---|
  | `session` | yes | mono |
  | `start` | yes | |
  | `last` | yes | `end_time`, "last activity" |
  | `traces` | yes | `trace_count` |
  | `user` | yes | |
  | `duration` | yes | |
  | `tokens` | yes | |
  | `cost` | yes | |
  | `errors` | yes | `error_count`; red when > 0 |
  | `tags` | no | |
  | `score:<name>` | yes | as U10 |

  Sorts come from `SessionSort`: start, last, traces, cost.
- **Activate:** navigate to `/sessions/$sessionId`, keeping the list's search in the link state so Back restores it.
- **Nav entry:** "Sessions" (icon `MessagesSquare`).

### Detail `/sessions/$sessionId`

`get_session` (with `eval` and `exp` passed through).
- **Header:** the session ID (`IdText`), user (link to `/users/$userId` once U22 lands; text before), start → last activity, turns, total tokens, cost, errors, tags, latest scores as badges.
- **Body:** a two-pane split (D125 style, ratio persisted as `${STORAGE_PREFIX}split.session`).
  - **Left: the conversation.** One block per turn (`SessionTurn`), in order:
    - a turn header (index, `TimeText`, trace name, duration, cost, `StatusDot`), then the root's `input` and `output` rendered with `ChatView` when `messages`, else `PayloadView`;
    - a turn with no root payloads shows "No root input/output". This happens when the trace has no root;
    - turns are virtualized with dynamic heights (`measureElement`), because sessions can have up to 1 000 turns.
  - **Right: the trace of the selected turn**, in `TraceView`. Clicking a turn header selects it (`?trace=`); none is selected by default.
- `truncated: true`: a notice "Showing the first 1 000 turns".

`span-header` and `trace-header` now link `session_id` to this page.

**Tests:**
- the list renders `responses/sessions.json`;
- `eval=1` uses the include-eval fixture;
- sorting by cost sends `sort=cost_total`;
- the detail of `sess-001` renders its turns in order, with their chat;
- clicking a turn opens its trace on the right;
- the session link in a trace header navigates here.

---

## U22 — Users

**Edit only:** `web/src/routes/$org/$project/users/**` (new), `web/src/components/user-columns.tsx` (new), `web/src/lib/search/users.ts` (new), `web/src/components/shell/nav.ts`, `web/src/components/trace/{span-header,trace-header}.tsx`, `web/src/components/session/**` (user link), `web/src/api/queries.ts` (or `queries-users.ts`), plus tests.

**Commit:** `web: end users list and detail`

### List `/users`

- **Toolbar:** time range, filter bar on the **`traces`** view, toggles, columns, saved views (kind `users`). The user list aggregates traces (spec 05 § Trace and span APIs), so the filter applies to traces. The bar's placeholder says so: "Filter traces, e.g. environment = 'production'".
- **Columns:** `user` (mono), `traces`, `sessions`, `tokens`, `cost`, `errors`, `first_seen`, `last_seen`. Sorts come from `UserSort`; the default is `last_seen desc`.
- **Activate:** `/users/$userId`.
- **Nav entry:** "Users" (icon `Users`).

### Detail `/users/$userId`

`get_user`:
- **Header:** the user ID, first and last seen, totals (traces, sessions, tokens, cost, errors).
- **Body:** the user's sessions (`EndUserDetail.sessions`, up to 100) in a `DataTable` with the session columns from U21. Activating a row goes to the session detail.
- No GDPR action (D123).

User IDs in trace headers, span headers and session headers now link here.

**Tests:**
- the list renders `responses/users.json`;
- the default sort is `last_seen`;
- the detail of `user-42` lists its sessions;
- a link from a trace header navigates here;
- the filter is sent to `list_users`.

---

## U23 — Dashboard

**Edit only:** `web/package.json`, `pnpm-lock.yaml`, shadcn `chart` (and `card` if it pulls it), `web/src/components/charts/**` (new), `web/src/routes/$org/$project/dashboard/**` (new), `web/src/lib/search/dashboard.ts` (new), `web/src/lib/metrics.ts` (+ test), `web/src/components/shell/nav.ts`, `web/src/api/queries.ts` (or `queries-metrics.ts`), plus tests.

**Commit:** `web: dashboard with KPIs and charts`

Add `recharts` 3.10.1 through `pnpm dlx shadcn@4.23.0 add chart` (D128). It must load only with the dashboard route: autoCodeSplitting already splits routes. Check with `vite build` output that `recharts` is not in the entry chunk, and say so in the report.

### Data

`get_metrics` with:
- `from`/`to` (from the time range);
- `bucket=auto`;
- `filter`;
- `group_by` (`group` param);
- `include_eval` and `include_experiments`.

It refetches when any of these change. Results are cached server-side for 10 s (spec 05), and the client uses `staleTime: 10_000`.

### `lib/metrics.ts` (pure)

- `seriesByMetric(response): Map<MetricName, MetricSeries[]>`.
- `toChartRows(series[], bucketNs): Array<{time: number; [group]: number | null}>`: one row per bucket time, one key per group (`"all"` when ungrouped). A missing point is `null` (a gap), never 0 (D128).
- `kpiDelta(kpi): {value, previous, changePct, direction}`. `direction` is `"up"`, `"down"` or `"flat"`. `changePct` is `undefined` when `previous` is 0 or absent.
- **Formatters per metric:**
  - latencies and TTFT (ns) use `formatDuration`;
  - cost uses `formatCost`;
  - tokens use `formatTokens`;
  - `error_rate` is a percentage;
  - counts are integers.

### Layout (fixed, spec 11)

- **Toolbar:** title, time range, filter bar on the `traces` view, **Group by** (none / model / environment / release), eval and experiment toggles.
- **KPI row:** five tiles (traces, error rate, p95 latency, cost, tokens). Each is 64 px high and shows the value, the delta vs the previous period as a coloured arrow plus percentage (for error rate, latency and cost, "up" is red), and a tooltip with the previous value.
- **A grid of chart cards:** 2 columns at ≥ 1280 px, 1 below; each 220 px high, with a title and a unit.

  | Chart | Series | Kind |
  |---|---|---|
  | Traces & errors | `traces`, `errors` | bars (traces), line (errors) |
  | Error rate | `error_rate` | line, gaps |
  | Latency | `latency_p50`, `latency_p95`, `latency_p99` | lines |
  | LLM latency by model | `llm_latency_p50`, `llm_latency_p95` | lines per model |
  | Cost by model | `cost_by_model` | stacked bars |
  | Tokens by type | `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens` | stacked bars |
  | TTFT | `ttft_p50`, `ttft_p95` | lines |
  | Scores | `score_avg` | one line per score name |
  | Evaluation cost | `eval_cost` | bars |
  | Top models by cost | `top_models` | table: model, LLM calls, tokens, cost |

  With `group`, the series that the server splits by group draw one line or bar per group, at most 8 groups by total value; the rest are summed as "other" in the client. A series missing from the response shows "No data" in its card.
- **Chart style:**
  - thin 1.5 px lines, no dots except on hover;
  - muted grid lines, axis labels 11 px mono;
  - the chart palette tokens from U2;
  - tooltips with all series at that time, formatted per metric;
  - x labels in the current time-zone setting.
- **Click a bucket** (bar or line point) → navigate to `/traces` with `from`/`to` = that bucket, the same `filter`, `eval` and `exp`.
- **Nav entry:** "Dashboard" (icon `LayoutDashboard`), placed first in the sidebar. The project index still redirects to Traces (spec 11: traces is the default landing).

**Tests:**
- **`lib/metrics.ts`:** gaps stay `null`; grouped rows; the "other" folding beyond 8 groups; `kpiDelta` directions and the zero-previous case.
- **Page:**
  - renders the five KPIs from `static/metrics.json`;
  - every chart card title is present;
  - changing the range refetches with a new `from`;
  - group by model sends `group_by=model`;
  - clicking a bucket navigates to traces with that range.

  Recharts renders SVG in jsdom only with a size. Give the chart containers explicit test dimensions through `test/layout.ts` (`data-chart-container`), or assert on the data passed to the chart through accessible text (the cards' `aria-label` summaries), and say which in the report.
- **A11y:** each chart card has an `aria-label` summary such as "Traces & errors: 5 traces, 1 error in range", so screen readers get the gist.

---

## U24 — ⌘K palette and keyboard shortcuts

**Edit only:** `web/package.json`, `pnpm-lock.yaml`, shadcn `command` (with `cmdk` 1.1.1), `web/src/components/command/**` (new), `web/src/components/shell/top-bar.tsx`, `web/src/routes/$org/$project/route.tsx`, `web/src/lib/shortcuts.ts` (+ test), plus tests.

**Commit:** `web: command palette and shortcuts help`

### Palette

⌘K / Ctrl+K anywhere, or the search-style button in the top bar showing "⌘K". Groups:
- **Go to:** the current project's pages (from `nav.ts`), plus the org home.
- **Projects:** the org's projects (first 50 from `list_projects`). Choosing one goes to its traces.
- **Open trace:** when the typed text is 32 hex characters (any case, with or without dashes), the first item is "Open trace <id>". It navigates to the full page `/traces/$traceId`. 16 hex characters gives "Find span <id>": it navigates to the spans list with `filter=span_id = '<id>'`.
- **Recent traces:** the last 10 traces opened in this browser (localStorage `${STORAGE_PREFIX}recent-traces`, per project, IDs and names only, never payloads).
- **Theme:** light / dark / system.
- **Time zone:** toggle UTC.

### Shortcuts

`lib/shortcuts.ts` holds one table of `{keys, description, scope}`, used by both the handler and the help dialog. The listener ignores keystrokes when focus is in an `input`, a `textarea`, a `select`, `[contenteditable]` or a CodeMirror editor.

| Keys | Action | Where |
|---|---|---|
| `/` | focus search (or the filter bar where there's no search) | lists |
| `f` | edit the filter as text | lists |
| `j` / `k`, `↓` / `↑` | next / previous row | lists, tree |
| `Enter` | open | lists |
| `Esc` | close the panel, clear focus | anywhere |
| `t` | toggle Tree / Transcript | trace view |
| `0` | fit the timeline | trace view |
| `g` then `t`/`s`/`e`/`u`/`d` | go to traces / spans / sessions / users / dashboard | project |
| `⌘K` | palette | anywhere |
| `?` | shortcuts help | anywhere |

Before this sub-phase, some of these were wired locally (j/k in `DataTable`, Esc in the split, `/` in the toolbar, `f`, `0`). Move them onto the table so the help dialog is the single source. Behaviour stays the same.

**Tests:**
- ⌘K opens the palette;
- typing a project name and Enter navigates;
- pasting a 32-hex trace ID offers "Open trace" and navigates to the full page;
- a 16-hex ID goes to spans with the filter;
- recent traces appear after opening one;
- `?` opens the help listing every table row;
- `g` `s` goes to sessions;
- shortcuts are ignored while typing in the search box.

---

## U25 — Onboarding empty state

**Edit only:** `web/src/components/onboarding/**` (new), `web/src/routes/$org/$project/traces/index.tsx`, plus tests.

**Commit:** `web: onboarding snippets for an empty project`

When the traces list is empty **and** no filter, search or custom range is set, and the eval and experiment toggles are off, show an onboarding panel instead of the plain empty state. Otherwise the plain "No traces match" with a "Clear filters" button.

The panel has tabs **OTel env vars**, **Python**, **TypeScript** and **Vercel AI SDK**, with copyable snippets:
- the OTLP endpoint is built from `window.location.origin` (`<origin>/v1/traces`, spec 02);
- the SDK base URL from the same origin;
- the key is written as `sk-llmobs-…` with a note "Create a secret key in Settings → API keys". The key-prefix version of spec 11 waits for the API keys contract (D123, R1).

Snippet content follows spec 10 § Configuration: env var names and `init()` options. Read spec 10 and use the exact names. There's also a "Waiting for the first trace…" line that polls the list every 5 s while the panel is shown (`refetchInterval`), so the page switches to the table as soon as data arrives.

**Tests:**
- an empty project shows the panel with the origin in the OTel tab;
- copy works;
- with a filter set, the plain empty state shows instead, and "Clear filters" clears it;
- the polling refetch switches to the table when the next response has rows (fake timers or `waitFor` with a short interval injected through a prop).
