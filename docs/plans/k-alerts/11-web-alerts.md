# K11 — Web: the Alerts nav entry, the rules list and the rule detail page

**Commit subject:** `web: add the alerts list and rule pages`

Adds "Alerts" to the project sidebar, the rules list `/$org/$project/alerts`, and the rule page `/$org/$project/alerts/$ruleId` with its state, its condition, its last error and its event history with deliveries. The editor (new/edit) is K12, which also adds the "New rule" and "Edit" buttons.

## Read first

- README § Design, D-K-2, D-K-4, D-K-9.
- K10 (`web/src/api/queries-alerts.ts`, `web/src/test/fixtures.ts` ids, the page and test style).
- `web/src/components/shell/nav.ts` and `project-sidebar.tsx`, `web/src/routes/$org/$project/layout.test.tsx` (nav assertions).
- `web/src/routes/$org/$project/traces/$traceId.tsx` (a detail route with a loader that maps a 404 to `notFound()`), `web/src/routes/$org/$project/dashboard/index.tsx` (a page with a query).
- `web/src/api/queries-settings.ts` (`useInfiniteQuery` with `pageParamName: "cursor"` in `api-keys.tsx`).
- `web/src/components/time-text.tsx` (`TimeText`, styles `"time"` and `"datetime"`), `web/src/components/{confirm-dialog,forbidden,disabled-reason,empty-state}.tsx`, `web/src/components/form/form-error.tsx`.
- `docs/plans/l-admin-ui/README.md` § Conventions (States: loading, error thrown to the error boundary, empty, data).
- Fixtures `fixtures/api/static/alert-rules.json`, `alert-rule.json`, `alert-events.json`, `channels.json` (K5, K6).

## Files

| File | Change |
|---|---|
| `web/src/lib/alerts.ts` (+ `alerts.test.ts`) | formatting helpers |
| `web/src/api/queries-alerts.ts` | `alertRuleQuery`, `alertEventsInit` |
| `web/src/components/shell/nav.ts` | the Alerts entry |
| `web/src/routes/$org/$project/layout.test.tsx` | one assertion |
| `web/src/components/alerts/state-badge.tsx` | new |
| `web/src/routes/$org/$project/alerts/index.tsx` (+ `.test.tsx`) | the list |
| `web/src/routes/$org/$project/alerts/$ruleId/index.tsx` (+ `.test.tsx`) | the detail |

## 0. Queries and ids

Add to `web/src/api/queries-alerts.ts`:

```ts
export function alertRuleQuery(org: string, project: string, id: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/alert-rules/{id}", {
    params: { path: { project: projectRef(org, project), id } },
  });
}

export function alertEventsInit(org: string, project: string, id: string) {
  return { params: { path: { project: projectRef(org, project), id }, query: { limit: 50 } } };
}
```

and to `web/src/test/fixtures.ts`: `export const FIRING_RULE_ID = "01900000-0000-7000-8000-000000000510";` (tests use it instead of the literal). Role checks use `canManageAlerts` from K10.

## 1. `web/src/lib/alerts.ts` additions

```ts
import type { Schema } from "@/api/types";

type Condition = Schema<"AlertCondition">;

/** Metric label: "Error rate", "Cost", "p95 latency" (+ " of LLM spans" for llm_span), "Score <name>" or "Pass rate of <name>". */
export function metricLabel(condition: Condition): string;

/** A value in the metric's unit: error_rate and pass_rate as percent with 1 decimal ("12.5%"),
 *  cost as "$" with 2 decimals, latency as "820 ms" (0 decimals), score avg with 3 decimals.
 *  `undefined` → "—". */
export function formatAlertValue(condition: Condition, value: number | undefined): string;

/** "Error rate > 5.0% over 5m" — label, comparator symbol, threshold via formatAlertValue, window. */
export function conditionSummary(condition: Condition): string;

/** ">", "≥", "<", "≤" for gt, gte, lt, lte. */
export function comparatorSymbol(comparator: Schema<"Comparator">): string;
```

`pass_rate` is `condition.metric === "score" && condition.aggregation === "pass_rate"`. Implement the formats directly: `` `${(v * 100).toFixed(1)}%` ``, `` `$${v.toFixed(2)}` ``, `` `${Math.round(v)} ms` `` and `v.toFixed(3)`. Do not call `formatPercent`, `formatCost` or `formatDuration` from `lib/metrics.ts`/`lib/format.ts`: their output differs (`5%`, `$1.5k`). No return type annotations on components anywhere in K11 (React 19 has no global `JSX` namespace). Tests in `alerts.test.ts`: one `expect` per metric for `formatAlertValue` and `conditionSummary` (e.g. the `High error rate` fixture condition → `Error rate > 5.0% over 5m`; `Low helpfulness` → `Score helpfulness < 0.700 over 1h`; `Daily cost` → `Cost > $50.00 over 24h`; a latency `llm_span` condition with threshold 8000 and `gte` → `p95 latency of LLM spans ≥ 8000 ms over 15m`), a `pass_rate` condition → `Pass rate of ok < 90.0% over 1h`, and `undefined` → `—`.

## 2. Nav and badge

- `nav.ts`: import `BellRing` from `lucide-react` and insert `{ to: "/$org/$project/alerts", label: "Alerts", icon: BellRing }` between the Users and SQL entries.
- `layout.test.tsx`: in its first test add `expect(screen.getByRole("link", { name: "Alerts" })).toBeInTheDocument();`.
- `components/alerts/state-badge.tsx`:

```tsx
/** The rule's state; a disabled rule shows "Disabled" whatever its state. */
import type { Schema } from "@/api/types";
import { Badge } from "@/components/ui/badge";

export function StateBadge({ state, enabled }: { state: Schema<"AlertState">; enabled: boolean })
```

`enabled` false → `Badge variant="ghost"` "Disabled"; `firing` → `variant="destructive"` "Firing"; `ok` → `variant="secondary"` "OK"; `no_data` → `variant="outline"` "No data".

## 3. The list (`alerts/index.tsx`)

`createFileRoute("/$org/$project/alerts/")({ component: AlertsPage })`.

- `const { org, project, data } = useProject(); const canEdit = atLeast(data.role, "member");`
- `useQuery(alertRulesQuery(org, project))` only (the list shows a channel count, not names).
- Page frame (the project layout clips overflow, so the page scrolls itself): `<div className="mx-auto flex w-full max-w-5xl flex-1 flex-col gap-6 overflow-auto px-4 py-6">` with an `h1 className="text-lg font-medium"` "Alerts" and `<p className="text-sm text-muted-foreground">Rules are evaluated every minute, or every 5 minutes for windows of 1 hour or more.</p>`. The detail page uses the same frame. The "New rule" button comes with the editor in K12; do not add it here.
- States: loading → three `Skeleton`s; `403` → `<Forbidden description="You don't have access to this project's alert rules." />`; any other error → `throw query.error`; empty; data.
- Empty → `EmptyState icon={BellRing} title="No alert rules yet" description="Alert on error rate, cost, latency or a score."`.
- `Table aria-label="Alert rules"` with columns **Name** (a `Link` to `/$org/$project/alerts/$ruleId`), **State** (`StateBadge`), **Condition** (`conditionSummary`), **Last value** (`formatAlertValue(rule.condition, rule.last_value)`), **Last evaluated** (`TimeText` with `style="datetime"`, or "Never"), **Channels** (`rule.channel_ids.length`). Rows in the order the API returns (by name).
- A rule with `last_error` shows, next to its state badge, `<span title={rule.last_error} role="img" aria-label="Evaluation failed"><TriangleAlert aria-hidden="true" className="size-4 text-destructive" /></span>` (lucide icons take no `title` prop).
- Links use typed params: `<Link to="/$org/$project/alerts/$ruleId" params={{ org, project, ruleId: rule.id }}>`.

## 4. The detail (`alerts/$ruleId/index.tsx`)

`createFileRoute("/$org/$project/alerts/$ruleId/")` with a loader like `traces/$traceId.tsx`: `ensureQueryData(alertRuleQuery(params.org, params.project, params.ruleId))`, a `404` problem → `throw notFound()`.

Data: `const { ruleId } = Route.useParams(); const { org, project, data } = useProject(); const { data: rule } = useSuspenseQuery(alertRuleQuery(org, project, ruleId));` plus `useQuery(channelsQuery(org, project))` and the events infinite query. Keep every hook above any early return. Any error of the channels or events query is thrown (readers have `View`, so a 403 there cannot happen).

Content:
- Header: the rule name (`h1`), `StateBadge`, and a "Back to alerts" link to `/$org/$project/alerts`.
- A summary block (`dl`): **Condition** (`conditionSummary`), **Filter** (`condition.filter` in `font-mono`, or "All traces"), **Minimum samples** (`condition.min_count`; the row is hidden when it is undefined), **Last value** (`formatAlertValue`) with **Samples** (`last_count`, or `—`), **Last evaluated** (`TimeText`, or "Never"), **Last fired** (`TimeText`, or "Never"), **Channels** (names from the channels query, in the order of `rule.channel_ids`, comma separated; `—` while it loads; an id not in the list shows "Deleted channel"; "None" for an empty list), **Reminders** ("Every N minutes while firing" when `notify_while_firing`, else "Off"; N = `Math.round(cooldown_s / 60)`).
- `last_error` present → an alert box (`role="alert"`, destructive text): "The last evaluation failed: <last_error>".
- **Delete**: a `Button variant="outline"` "Delete rule" (disabled with `DisabledReason` "Only project members and above can manage alerts" when `!canEdit`) → `ConfirmDialog` titled "Delete alert rule?", description "Its history is deleted too.", `confirmLabel="Delete"`, `destructive`, `pending={remove.isPending}`, `error={remove.error ? problemMessage(remove.error, "Could not delete the rule.") : undefined}`. `const remove = $api.useMutation("delete", "/api/v1/projects/{project}/alert-rules/{id}")`, called as `remove.mutate({ params: { path: { project: projectRef(org, project), id: rule.id } } }, { onSuccess })`; on success close, `remove.reset()`, invalidate `alertRulesQuery(org, project).queryKey` and `navigate({ to: "/$org/$project/alerts", params: { org, project } })`.
- **Enable/disable**: a `Switch` labelled "Enabled" (`id="rule-enabled"`, `Label htmlFor`); for viewers it is disabled and wrapped in `DisabledReason` with the same reason as Delete (D-L-4). Toggling sends `PATCH …/alert-rules/{id}` with `{ enabled }`; on success it sets the query data of `alertRuleQuery(org, project, ruleId)` to the response and invalidates `alertRulesQuery(org, project).queryKey`. On failure it shows, next to the switch, a `FormError` with: for a `403` whose problem `code` is `"plan_limit"` (D-K-14), the problem's `detail`, followed by a separate `<p className="text-xs">Upgrade your plan or disable another rule.</p>`; otherwise `problemMessage(error, "Could not update the rule.")`. Under the switch, the line "Disabling or changing the condition resets the state to OK." in `text-xs text-muted-foreground`.
- **History** (`h2` "History"): `$api.useInfiniteQuery("get", "/api/v1/projects/{project}/alert-rules/{id}/events", alertEventsInit(org, project, ruleId), { pageParamName: "cursor", initialPageParam: null, getNextPageParam: (page) => page.next_cursor ?? undefined })`. A `Table aria-label="Alert events"` with columns **Time** (`TimeText` of `created_at`), **Event** (`Fired` destructive badge, `Resolved` secondary, `Reminder` outline), **Value** (`<span>{formatAlertValue(rule.condition, event.value)}</span>` then, when `count` is present, `<span className="text-muted-foreground"> · {count} {count === 1 ? "sample" : "samples"}</span>`; `—` when the value is absent), **Window** (`TimeText` of `window_from` with `style="time"` "–" `window_to`), **Notifications**: one line per delivery: the channel name, a badge (`sent` → secondary "Sent", `failed` → destructive "Failed", `pending` → outline "Pending"), `attempts` when > 1 ("3 attempts"), and `last_error` in muted text. No deliveries → "No channels". A "Load more" button when `hasNextPage`. Empty → "No events yet. Events appear when the rule fires or resolves."

The value formatting uses the rule's current condition; the event's own `metric` can differ after an edit, so when `event.metric !== rule.condition.metric`, show the raw number with 3 decimals instead.

## 5. Tests

`alerts/index.test.tsx` (`renderApp("/acme/support-bot/alerts")`):
Tests import fixtures with relative paths as `api-keys.test.tsx` does (`alerts/index.test.tsx` is at the same depth; `alerts/$ruleId/index.test.tsx` is one `../` deeper), e.g. `alert-rules.json`, `alert-rule.json`, `alert-events.json`, `project.json`. Table rows: find a row by its accessible name (`within(table).getByRole("row", { name: /^High error rate\b/ })`, as `api-keys.test.tsx` does) rather than by index: indexed access is `T | undefined` under `noUncheckedIndexedAccess` and Biome forbids `!`. For the events table, rows have no unique name, so use a small helper `function rowAt(table: HTMLElement, index: number): HTMLElement { const row = within(table).getAllByRole("row")[index]; if (!row) throw new Error(`no row ${index}`); return row; }` (index 0 is the header). Writes are overridden per test (`server.use`); `test/setup.ts` fails a test on an unhandled request.

1. `lists the rules with their state`: three body rows in fixture order; `High error rate` shows `Firing`, `Error rate > 5.0% over 5m`, `12.5%`, channels `2`; `Daily cost` shows `Disabled`; `Low helpfulness` shows `No data`.
2. `links to the rule page`: the `High error rate` link points to `/acme/support-bot/alerts/01900000-0000-7000-8000-000000000510`.
3. `empty state`: override the list with `{ data: [], next_cursor: null }` → "No alert rules yet".
4. `the sidebar shows Alerts as current` (`aria-current="page"`).
4b. `flags a failed evaluation`: the list answers the fixture with `last_error: "query timed out"` on the first rule → `getByLabelText("Evaluation failed")`.
4c. `forbidden`: the list answers `problemResponse(403, "forbidden")` → "You don't have access to this project's alert rules."

`alerts/$ruleId/index.test.tsx` (`renderApp("/acme/support-bot/alerts/01900000-0000-7000-8000-000000000510")`):
5. `shows the rule and its history`: the summary shows `environment = 'production'`, the channel names `Team Slack, Pager webhook` (`findByText`: they appear when the channels query resolves), "Every 60 minutes while firing"; the events table has 3 body rows; within `rowAt(table, 1)`: `Fired`, text content containing `12.5% · 48 samples`, and `Pager webhook` with `Failed` and `the endpoint answered 500`; within `rowAt(table, 3)`: `2 attempts`. `Fired` appears in two rows, so always scope with `within(row)`.
6. `shows the last error`: override the rule with `last_error: "query timed out"` → the alert text.
7. `pages the history`: one handler `http.get("/api/v1/projects/:project/alert-rules/:id/events", …)` answers `{ ...eventsFixture, next_cursor: "c1" }` without a cursor and, when `new URL(request.url).searchParams.get("cursor") === "c1"`, a page with one event `{ ...eventsFixture.data[0], id: "01900000-0000-7000-8000-000000000529", kind: "reminder" }` and `next_cursor: null` → "Load more" fetches and shows a `Reminder` badge.
8. `disables the rule`: `http.patch("/api/v1/projects/:project/alert-rules/:id", async ({ request }) => { body = await request.json(); return HttpResponse.json({ ...ruleFixture, enabled: false, state: "ok" }); })`; toggle the `switch` named "Enabled" → `body` is `{ enabled: false }` and the badge reads `Disabled`.
8b. `shows the plan limit when enabling`: the rule answers `{ ...ruleFixture, enabled: false }`; the PATCH answers `problemResponse(403, "plan_limit", { detail: "Your plan allows 1 enabled alert rule." })`; toggle → `findByText("Your plan allows 1 enabled alert rule.")` and `getByText("Upgrade your plan or disable another rule.")` (two elements).
9. `deletes the rule`: click "Delete rule", then the `button` "Delete" inside the `alertdialog`; DELETE answered `new HttpResponse(null, { status: 204 })` → `await waitFor(() => expect(router.state.location.pathname).toBe("/acme/support-bot/alerts"))`.
10. `unknown rule is not found`: override `GET …/alert-rules/:id` with `problemResponse(404, "not_found")` → `findByRole("heading", { name: "Page not found" })`.
11. `viewers cannot change the rule`: `http.get("/api/v1/projects/:project", () => HttpResponse.json({ ...projectFixture, role: "viewer" }))` → the switch and "Delete rule" are disabled, and both have the accessible description "Only project members and above can manage alerts" (`toHaveAccessibleDescription`; `getByText` would find one hidden span per control).
13. `delete failure keeps the dialog open`: DELETE answers `problemResponse(500, "internal")` → the alertdialog stays open with the problem's title.
14. `other toggle errors`: the PATCH answers `problemResponse(500, "internal")` → the problem's title shows next to the switch.
15. `never evaluated`: the list answers the `Daily cost` rule (no `last_evaluated_at`) → its row shows "Never".
12. `edge values`: override the events with one event whose `metric` is `"cost"` and `value` 3.14159 → its value cell shows `3.142`; an event with `deliveries: []` → "No channels"; an empty page → "No events yet. Events appear when the rule fires or resolves."; the rule with `channel_ids` containing an unknown id → "Deleted channel".

## Commands

```bash
export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
```

```bash
pnpm --dir web gen:routes
```

```bash
just fmt
```

```bash
pnpm --dir web test -- alerts
```

```bash
just check
```

## Done when

- Both pages work on the mocks; the alerts helpers, the page tests and the nav test pass; `just check` is green.
