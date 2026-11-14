# K11 — Web: the Alerts nav entry, the rules list and the rule detail page

**Commit subject:** `web: add the alerts list and rule pages`

Adds "Alerts" to the project sidebar, the rules list `/$org/$project/alerts`, and the rule page `/$org/$project/alerts/$ruleId` with its state, its condition, its last error and its event history with deliveries. The editor (new/edit) is K12, which also adds the "New rule" and "Edit" buttons.

## Read first

- README § Design, D-K-2, D-K-4, D-K-9.
- K10 (`web/src/api/queries-alerts.ts`, `web/src/test/fixtures.ts` ids, the page and test style).
- `web/src/components/shell/nav.ts` and `project-sidebar.tsx`, `web/src/routes/$org/$project/layout.test.tsx` (nav assertions).
- `web/src/routes/$org/$project/traces/$traceId.tsx` (a detail route with a loader that maps a 404 to `notFound()`), `web/src/routes/$org/$project/dashboard/index.tsx` (a page with a query).
- `web/src/api/queries-settings.ts` (`useInfiniteQuery` with `pageParamName: "cursor"` in `api-keys.tsx`).
- `web/src/lib/metrics.ts` (`formatPercent`, `formatMetric`), `web/src/lib/format.ts` (`formatCost`, `formatRelative`), `web/src/components/time-text.tsx`.
- Fixtures `fixtures/api/static/alert-rules.json`, `alert-rule.json`, `alert-events.json`, `channels.json` (K5, K6).

## Files

| File | Change |
|---|---|
| `web/src/lib/alerts.ts` (+ `alerts.test.ts`) | formatting helpers |
| `web/src/components/shell/nav.ts` | the Alerts entry |
| `web/src/routes/$org/$project/layout.test.tsx` | one assertion |
| `web/src/components/alerts/state-badge.tsx` | new |
| `web/src/routes/$org/$project/alerts/index.tsx` (+ `.test.tsx`) | the list |
| `web/src/routes/$org/$project/alerts/$ruleId/index.tsx` (+ `.test.tsx`) | the detail |

## 1. `web/src/lib/alerts.ts`

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

`pass_rate` is `condition.metric === "score" && condition.aggregation === "pass_rate"`. Tests in `alerts.test.ts`: one `expect` per metric for `formatAlertValue` and `conditionSummary` (e.g. the `High error rate` fixture condition → `Error rate > 5.0% over 5m`; `Low helpfulness` → `Score helpfulness < 0.700 over 1h`; `Daily cost` → `Cost > $50.00 over 24h`; a latency `llm_span` condition with threshold 8000 and `gte` → `p95 latency of LLM spans ≥ 8000 ms over 15m`), and `undefined` → `—`.

## 2. Nav and badge

- `nav.ts`: import `BellRing` from `lucide-react` and append `{ to: "/$org/$project/alerts", label: "Alerts", icon: BellRing }` after Users.
- `layout.test.tsx`: add an assertion that the sidebar has a link named `Alerts`.
- `components/alerts/state-badge.tsx`:

```tsx
/** The rule's state; a disabled rule shows "Disabled" whatever its state. */
export function StateBadge({ state, enabled }: { state: Schema<"AlertState">; enabled: boolean }): JSX.Element;
```

`enabled` false → `Badge variant="ghost"` "Disabled"; `firing` → `variant="destructive"` "Firing"; `ok` → `variant="secondary"` "OK"; `no_data` → `variant="outline"` "No data".

## 3. The list (`alerts/index.tsx`)

`createFileRoute("/$org/$project/alerts/")({ component: AlertsPage })`.

- `const { org, project, data } = useProject(); const canEdit = atLeast(data.role, "member");`
- `useQuery(alertRulesQuery(org, project))` only (the list shows a channel count, not names).
- Page layout: like the dashboard's page header (an `h1` "Alerts" and a short line "Rules are evaluated every minute over a sliding window."). The "New rule" button comes with the editor in K12; do not add it here.
- Loading skeleton; `403` → `Forbidden`; empty → `EmptyState icon={BellRing} title="No alert rules yet" description="Alert on error rate, cost, latency or a score."`.
- `Table aria-label="Alert rules"` with columns **Name** (a `Link` to `/$org/$project/alerts/$ruleId`), **State** (`StateBadge`), **Condition** (`conditionSummary`), **Last value** (`formatAlertValue(rule.condition, rule.last_value)`), **Last evaluated** (`TimeText` with `style="datetime"`, or "Never"), **Channels** (`rule.channel_ids.length`). Rows in the order the API returns (by name).
- A rule with `last_error` shows a small `AlertTriangle` icon with `title={rule.last_error}` and `aria-label="Evaluation failed"` next to its state badge.

## 4. The detail (`alerts/$ruleId/index.tsx`)

`createFileRoute("/$org/$project/alerts/$ruleId/")` with a loader like `traces/$traceId.tsx`: `ensureQueryData(alertRuleQuery(params.org, params.project, params.ruleId))`, a `404` problem → `throw notFound()`.

Content:
- Header: the rule name (`h1`), `StateBadge`, and a "Back to alerts" link to `/$org/$project/alerts`.
- A summary block (`dl`): **Condition** (`conditionSummary`), **Filter** (`condition.filter` in `font-mono`, or "All traces"), **Minimum samples** (`condition.min_count`, hidden for cost), **Last value** (`formatAlertValue`) with **Samples** (`last_count`), **Last evaluated** (`TimeText`), **Last fired** (`TimeText` or "Never"), **Channels** (names from `useQuery(channelsQuery(org, project))`, comma separated; an id not in the list shows "Deleted channel"), **Reminders** ("Every N minutes while firing" when `notify_while_firing`, else "Off"; N = `cooldown_s / 60`).
- `last_error` present → an alert box (`role="alert"`, destructive text): "The last evaluation failed: <last_error>".
- **Delete**: a `Button variant="outline"` "Delete rule" (disabled with `DisabledReason` "Only project members and above can manage alerts" when `!canEdit`) → `ConfirmDialog` "Delete alert rule?", description "Its history is deleted too.", destructive; `$api.useMutation("delete", "/api/v1/projects/{project}/alert-rules/{id}")`; on success invalidate `alertRulesQuery(org, project).queryKey` and navigate to `/$org/$project/alerts`.
- **Enable/disable**: a `Switch` labelled "Enabled" (`id="rule-enabled"`, `Label htmlFor`), disabled for viewers; toggling sends `PATCH …/alert-rules/{id}` with `{ enabled }` and on success sets the query data of `alertRuleQuery(...)` to the response and invalidates `alertRulesQuery`. Under it, the line "Disabling or changing the condition resets the state to OK." in `text-xs text-muted-foreground`.
- **History** (`h2` "History"): `$api.useInfiniteQuery("get", "/api/v1/projects/{project}/alert-rules/{id}/events", alertEventsInit(org, project, ruleId), { pageParamName: "cursor", initialPageParam: null, getNextPageParam: (page) => page.next_cursor ?? undefined })`. A `Table aria-label="Alert events"` with columns **Time** (`TimeText` of `created_at`), **Event** (`Fired` destructive badge, `Resolved` secondary, `Reminder` outline), **Value** (`formatAlertValue(rule.condition, event.value)`, with `count` samples in muted text: `12.5% · 48 samples`; `—` when absent), **Window** (`TimeText` of `window_from` with `style="time"` "–" `window_to`), **Notifications**: one line per delivery: the channel name, a badge (`sent` → secondary "Sent", `failed` → destructive "Failed", `pending` → outline "Pending"), `attempts` when > 1 ("3 attempts"), and `last_error` in muted text. No deliveries → "No channels". A "Load more" button when `hasNextPage`. Empty → "No events yet. Events appear when the rule fires or resolves."

The value formatting uses the rule's current condition; the event's own `metric` can differ after an edit, so when `event.metric !== rule.condition.metric`, show the raw number with 3 decimals instead.

## 5. Tests

`alerts/index.test.tsx` (`renderApp("/acme/support-bot/alerts")`):
1. `lists the rules with their state`: three rows in fixture order; `High error rate` shows `Firing`, `Error rate > 5.0% over 5m`, `12.5%`, channels `2`; `Daily cost` shows `Disabled`; `Low helpfulness` shows `No data`.
2. `links to the rule page`: the `High error rate` link points to `/acme/support-bot/alerts/01900000-0000-7000-8000-000000000510`.
3. `empty state`: override the list with `{ data: [], next_cursor: null }` → "No alert rules yet".
4. `the sidebar shows Alerts as current` (`aria-current="page"`).

`alerts/$ruleId/index.test.tsx` (`renderApp("/acme/support-bot/alerts/01900000-0000-7000-8000-000000000510")`):
5. `shows the rule and its history`: the summary shows `environment = 'production'`, the channel names `Team Slack, Pager webhook`, "Every 60 minutes while firing"; the events table has 3 rows; the newest shows `Fired`, `12.5% · 48 samples`, and `Pager webhook` `Failed` with `the endpoint answered 500`.
6. `shows the last error`: override the rule with `last_error: "query timed out"` → the alert text.
7. `pages the history`: override the events with a first page whose `next_cursor` is `"c1"` and a second page for `?cursor=c1` (check `new URL(request.url).searchParams.get("cursor")` in one handler) → "Load more" fetches and shows the second page's row.
8. `disables the rule`: toggle "Enabled" → the PATCH body is `{ enabled: false }`; answer with the rule `{ ...fixture, enabled: false, state: "ok" }` → the badge reads `Disabled`.
9. `deletes the rule`: confirm → DELETE answered `204` → the URL becomes `/acme/support-bot/alerts`.
10. `unknown rule is not found`: override `GET …/alert-rules/:id` with `problemResponse(404, "not_found")` → the not-found page.
11. `viewers cannot change the rule`: project role `viewer` → the switch and "Delete rule" are disabled; no write is sent.

## Commands

```bash
export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
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

- Both pages work on the mocks; the alerts helpers, the 11 page tests and the nav test pass; `just check` is green.
