# K12 — Web: the rule editor with the preview chart, dev write mocks, closeout

**Commit subject:** `web: add the alert rule editor`

Adds `/$org/$project/alerts/new` and `/$org/$project/alerts/$ruleId/edit` with one form component, a preview chart of the metric with the threshold line (D-K-13), the "New rule" and "Edit" buttons on K11's pages, stateless dev write mocks for every alert and channel write (D-L-10), and marks track K implemented in the roadmap.

## Read first

- README § Design, D-K-2, D-K-3, D-K-4, D-K-13.
- K3's condition rules (`docs/plans/k-alerts/03-types.md` § 3) — the form mirrors them; the server stays the authority.
- K10 (`queries-alerts.ts`, `channel-dialog.tsx` form style, `test/fixtures.ts`), K11 (`lib/alerts.ts`, the two pages).
- `web/src/components/charts/metric-chart.tsx` (`ChartContainer`, `ComposedChart`, `Line`, `XAxis`, `YAxis`, `ReferenceLine`, `AXIS_TICK`), `web/src/components/ui/chart.tsx`, `web/src/lib/metrics.ts` (`seriesByMetric`, `toChartRows`, `ChartRow`, `formatMetric`), `web/src/api/queries.ts` (`metricsQuery`: it always sends `bucket: "auto"`, so the preview builds its own query).
- `web/src/components/ui/{select,switch,textarea,checkbox,label}.tsx`, `web/src/components/form/*`.
- `web/src/mocks/writes.ts` and `writes.test.ts` (the covered prefixes and `WRITE_OPERATIONS`), `web/src/mocks/browser.ts`.
- `docs/roadmap.md` (row K).

## Files

| File | Change |
|---|---|
| `web/src/components/alerts/rule-form.tsx` | new |
| `web/src/components/alerts/rule-preview.tsx` | new |
| `web/src/lib/alerts.ts` (+ test) | form ↔ condition conversion, preview parameters |
| `web/src/routes/$org/$project/alerts/new.tsx` (+ `.test.tsx`) | new |
| `web/src/routes/$org/$project/alerts/$ruleId/edit.tsx` (+ `.test.tsx`) | new |
| `web/src/routes/$org/$project/alerts/index.tsx`, `$ruleId/index.tsx` | "New rule" and "Edit" buttons |
| `web/src/mocks/writes.ts`, `writes.test.ts` | alert and channel writes |
| `docs/roadmap.md` | row K |

## 1. `lib/alerts.ts` additions

```ts
/** The editable form. Numbers are strings while typed. Rates are typed in percent. */
export type RuleFormValues = {
  name: string;
  metric: Schema<"AlertMetric">;
  window: Schema<"AlertWindow">;
  filter: string;
  comparator: Schema<"Comparator">;
  /** Percent for error_rate and pass_rate, USD for cost, ms for latency, raw for score avg. */
  threshold: string;
  minCount: string;
  target: Schema<"LatencyTarget">;
  scoreName: string;
  aggregation: Schema<"ScoreAggregation">;
  channelIds: string[];
  enabled: boolean;
  /** Minutes, 5–10080. */
  cooldownMinutes: string;
  notifyWhileFiring: boolean;
};

export const DEFAULT_RULE_FORM: RuleFormValues = {
  name: "", metric: "error_rate", window: "5m", filter: "", comparator: "gt",
  threshold: "5", minCount: "20", target: "trace", scoreName: "", aggregation: "avg",
  channelIds: [], enabled: true, cooldownMinutes: "60", notifyWhileFiring: false,
};

/** Whether the threshold is typed in percent. */
export function thresholdInPercent(metric: Schema<"AlertMetric">, aggregation: Schema<"ScoreAggregation">): boolean;

/** The unit shown after the threshold input: "%", "USD", "ms" or "" (score avg). */
export function thresholdUnit(metric: Schema<"AlertMetric">, aggregation: Schema<"ScoreAggregation">): string;

/** Form values from a stored rule (edit). Percent thresholds are multiplied by 100 and printed
 *  with `String(Math.round(value * 1e6) / 1e6)`; cooldown_s / 60. */
export function formFromRule(rule: Schema<"AlertRule">): RuleFormValues;

/** The condition to send, or field errors keyed like the server's (`name`, `condition.threshold`,
 *  `condition.min_count`, `condition.score_name`, `cooldown_s`). Only the fields of the metric are
 *  set: `min_count` not for cost, `target` only for latency, `score_name` and `aggregation` only for score.
 *  An empty filter is omitted. */
export function conditionFromForm(form: RuleFormValues):
  | { ok: true; condition: Schema<"AlertCondition">; cooldownS: number }
  | { ok: false; errors: Record<string, string> };
```

Client checks in `conditionFromForm` (messages exact):
- `threshold`: not a finite number → `"Enter a number."`; percent metrics outside 0–100 → `"Enter a percentage from 0 to 100."`; cost below 0 → `"Enter 0 or more."`; latency ≤ 0 → `"Enter a duration above 0."`. Percent values are divided by 100.
- `minCount` (not cost): an integer from 1 to 1 000 000, else `"Enter a whole number from 1 to 1,000,000."`.
- `scoreName` (score): blank → `"Enter a score name."`.
- `cooldownMinutes`: an integer from 5 to 10 080, else `"Enter 5 to 10,080 minutes."`; `cooldownS = minutes * 60`.

```ts
/** D-K-13: the bucket equal to the window, and the preview range. */
export function previewParams(window: Schema<"AlertWindow">, nowMs: number): { bucket: Schema<"MetricsBucket">; from: string; to: string };

/** Which series to draw: error_rate → "error_rate"; cost → "cost"; latency_p95 with target trace →
 *  "latency_p95" (ns, shown in ms); score → "score_avg" with group = the score name;
 *  latency_p95 with target llm_span → undefined (no preview). */
export function previewSeries(condition: Pick<RuleFormValues, "metric" | "target" | "scoreName">):
  | { metric: Schema<"MetricName">; group?: string }
  | undefined;
```

`previewParams`: bucket `1m`→`1m`, `5m`→`5m`, `15m`→`15m`, `1h`→`1h`, `6h`→`6h`, `24h`→`1d`; range `[now − 24 h, now)` for windows up to `1h`, `[now − 3 d, now)` for `6h`, `[now − 14 d, now)` for `24h`; `from`/`to` as `new Date(ms).toISOString()`.

Tests in `alerts.test.ts`: `conditionFromForm` for each metric (defaults → `{"metric":"error_rate","window":"5m","comparator":"gt","threshold":0.05,"min_count":20}` exactly); each error message; `formFromRule` of each fixture rule round-trips through `conditionFromForm` to the same condition; `previewParams("6h", Date.parse("2026-01-04T00:00:00Z"))` → `{ bucket: "6h", from: "2026-01-01T00:00:00.000Z", to: "2026-01-04T00:00:00.000Z" }`; `previewSeries` for the five cases.

## 2. `components/alerts/rule-preview.tsx`

```tsx
export function RulePreview(props: {
  org: string;
  project: string;
  values: RuleFormValues;
}): JSX.Element;
```

- `previewSeries(values)` undefined → a muted box: "No preview for LLM span latency. The dashboard shows it per model."
- Otherwise, debounce `values.filter` by 500 ms (`useEffect` + `setTimeout`) and query `$api.useQuery("get", "/api/v1/projects/{project}/metrics", { params: { path: { project: projectRef(org, project) }, query: { from, to, bucket, ...(filter ? { filter } : {}) } } }, { staleTime: 60_000, placeholderData: keepPreviousData })`. Compute `from`/`to` once per window change (`useMemo` on `values.window`, with `Date.now()`), so the query key does not change every render.
- Data: `seriesByMetric(response).get(metric)`, filtered to the entry whose `group` equals `previewSeries(...).group` when one is given (else the entry without a group); `toChartRows([series], response.bucket_ns)` (check `MetricsResponse`'s bucket field name in `schema.gen.ts`). Latency values are ns: divide by 1 000 000 for display; error rate and pass rate are fractions.
- Threshold for the line: the converted threshold of `conditionFromForm(values)` when it is `ok` (`condition.threshold`, in the series unit: fraction, USD, ms, raw); none when the form has errors.
- Chart: `ChartContainer` (`config={{ value: { label: metricLabel(…), color: "var(--chart-1)" } }}`, `initialDimension={{ width: 640, height: 160 }}`, `className="aspect-auto h-40 w-full"`) around a `ComposedChart` with `XAxis dataKey="time"` (ticks with `formatTimestamp`), `YAxis` (ticks with `formatAlertValue` of a condition built from the form), one `Line dataKey="value"` (rename the series key to `value` when building rows), `isAnimationActive={false}`, and, when there is a threshold, `<ReferenceLine y={threshold} stroke="var(--destructive)" strokeDasharray="4 4" ifOverflow="extendDomain" label={{ value: "Threshold", position: "insideTopRight", fontSize: 11 }} />`. Wrap it in `figure` with `aria-label="Preview of the metric over the last <24 hours|3 days|14 days>"`.
- Under the chart: "Each point is the metric over one window, as the rule sees it. Minimum samples are not shown." in `text-xs text-muted-foreground`.
- Query states: loading → `Skeleton className="h-40 w-full"`; `invalid_filter` problem → the filter message in `FormError` ("The filter is invalid: <message>", message from `filterMessage` in `components/filter-bar/logic.ts` if exported, else `problemMessage`); other errors → "The preview could not be loaded."; no point with a value → "No data in this range."

## 3. `components/alerts/rule-form.tsx`

```tsx
export function RuleForm(props: {
  org: string;
  project: string;
  /** Absent: create. */
  rule?: Schema<"AlertRule">;
  /** Called with the saved rule. */
  onSaved: (rule: Schema<"AlertRule">) => void;
  onCancel: () => void;
}): JSX.Element;
```

A `<form noValidate>` (no nested forms: the filter is a plain `Textarea`, not the filter bar) with two columns on wide screens (fields left, `RulePreview` right; one column below `lg`). Sections and fields (ids prefixed `rule-`):

1. **Name** (`TextField`, max 100).
2. **What to measure**: `Select` "Metric" (Error rate, Cost, p95 latency, Score); for latency a `Select` "Latency of" (Whole traces, LLM spans); for score a `TextField` "Score name" and a `Select` "Aggregation" (Average, Pass rate of a boolean score); `Select` "Window" (1 minute, 5 minutes, 15 minutes, 1 hour, 6 hours, 24 hours) with the description "Checked every minute for windows up to 15 minutes, every 5 minutes otherwise."
3. **Filter**: `Textarea` labelled "Filter" (`font-mono`, 2 rows, max 4096), description "A traces filter, e.g. environment = 'production'. Empty: every trace." Errors with field `filter` (the server's `invalid_filter`) show under it.
4. **Condition**: `Select` "Comparator" (above, at or above, below, at or below → gt, gte, lt, lte); `TextField` "Threshold" with the unit from `thresholdUnit` as a suffix; `TextField` "Minimum samples" (hidden for cost) with description "Fewer samples in the window gives No data, which never fires."
5. **Notifications**: a checkbox list of `useQuery(channelsQuery(org, project))` channels (`Checkbox` + label `<name> (<Kind>)`), at most 10 checked (disable the others at 10); no channels → "No channels yet." and a link "Add a channel" to `/$org/$project/settings/channels`. `Switch` "Remind while firing" and, when on, `TextField` "Remind every (minutes)".
6. **Enabled**: `Switch` "Enabled" (create only; the detail page toggles it for an existing rule).

Changing the metric resets `threshold` to the metric's default (`5` for error rate, `100` for cost, `5000` for latency, `0.5` for score avg, `90` for pass rate), `comparator` to `gt` (`lt` for score), and `minCount` to `"20"` (`"10"` for score).

Submit:
1. `conditionFromForm`; errors → shown on the fields, nothing sent. Name blank → `"Enter a name."`.
2. Create: `POST …/alert-rules` with `{ name, condition, channel_ids, enabled, cooldown_s, notify_while_firing }` (`Schema<"CreateAlertRuleRequest">`). Edit: `PATCH …/alert-rules/{id}` with only the changed top-level fields (`condition` is sent whole when any of its fields changed, compared with `JSON.stringify` of the stored and the new condition).
3. On success: invalidate `alertRulesQuery(org, project).queryKey`, set `alertRuleQuery(org, project, id)` data to the response, call `onSaved(rule)`.
4. Errors: `fieldErrors` mapped by field name (`name`, `filter`, `condition.threshold`, `condition.min_count`, `condition.score_name`, `condition.target`, `condition.aggregation`, `channel_ids`, `cooldown_s`); `409` → form error `problemMessage(error, "A rule with this name already exists.")`; `503` → `"The server is busy. Try again."`; else `problemMessage(error, "Could not save the rule.")`.
5. In edit mode, when the condition changed, show above the buttons: "Saving a new condition resets the rule to OK." (D-K-4).

Buttons: `Cancel` (`variant="outline"`, calls `onCancel`) and `SubmitButton` "Create rule" or "Save".

## 4. Routes and buttons

- `alerts/new.tsx`: `createFileRoute("/$org/$project/alerts/new")({ component: NewRulePage })`. Members and above: `h1` "New alert rule" and `RuleForm` with `onSaved={(rule) => navigate({ to: "/$org/$project/alerts/$ruleId", params: { org, project, ruleId: rule.id } })}` and `onCancel` back to `/$org/$project/alerts`. Viewers: `<Forbidden description="Only project members and above can create alert rules." />`.
- `alerts/$ruleId/edit.tsx`: `createFileRoute("/$org/$project/alerts/$ruleId/edit")` with the same loader as the detail page (404 → `notFound()`); viewers get `Forbidden`; `h1` "Edit <name>"; `RuleForm rule={…}`; saved or cancelled → the rule page.
- `alerts/index.tsx`: a "New rule" `Button` (a `Link` to `/$org/$project/alerts/new`, `Plus` icon) in the header, disabled with `DisabledReason` "Only project members and above can manage alerts" for viewers; the empty state gets the same button below it.
- `alerts/$ruleId/index.tsx`: an "Edit" `Button` (link to `…/edit`) next to "Delete rule", disabled the same way.

## 5. Dev write mocks (`mocks/writes.ts`)

Add to `WRITE_OPERATIONS`: `create_channel` (post `/api/v1/projects/{project}/channels`), `update_channel` (patch `…/channels/{id}`), `delete_channel` (delete `…/channels/{id}`), `test_channel` (post `…/channels/{id}/test`), `create_alert_rule` (post `/api/v1/projects/{project}/alert-rules`), `update_alert_rule` (patch `…/alert-rules/{id}`), `delete_alert_rule` (delete `…/alert-rules/{id}`). Responses (import `channels.json` and `alert-rules.json`):
- `create_channel`: `201` with `{ ...channels.data[2], name: trimmed(body.name) ?? channels.data[2].name }` (the Slack example; for `kind: "email"` use `data[0]`, for `"webhook"` `data[1]`).
- `update_channel`: the channel of `params.id` from the fixture (or `data[0]`) with the new name when given.
- `test_channel`: `{ ok: true }`.
- `create_alert_rule`: `201` with `{ ...alertRules.data[1], name: trimmed(body.name) ?? …, condition: body.condition ?? …, state: "ok" }` without the `last_*` fields.
- `update_alert_rule`: the rule of `params.id` (or `data[1]`) merged with the body's top-level fields.
- `delete_channel`, `delete_alert_rule`: `noContent()`.

`writes.test.ts`: extend `covered` with `path.startsWith("/api/v1/projects/{project}/channels") || path.startsWith("/api/v1/projects/{project}/alert-rules")`, and add one test like the existing ones that a create of each returns `201` with the submitted name.

## 6. Tests

`alerts/new.test.tsx` (`renderApp("/acme/support-bot/alerts/new")`):
1. `creates an error rate rule`: type the name, check `Team Slack`, submit → POST body `{ name: "Errors", condition: { metric: "error_rate", window: "5m", comparator: "gt", threshold: 0.05, min_count: 20 }, channel_ids: ["01900000-0000-7000-8000-000000000500"], enabled: true, cooldown_s: 3600, notify_while_firing: false }`; answer `201` with the `High error rate` fixture → the URL becomes the rule page.
2. `score fields`: choose Score, aggregation Pass rate, score name `ok`, threshold `90`, comparator below → body condition `{ metric: "score", window: "5m", comparator: "lt", threshold: 0.9, min_count: 10, score_name: "ok", aggregation: "pass_rate" }`.
3. `client errors`: clear the threshold → "Enter a number." and no POST (a `server.use` POST handler that fails the test if called, or a call counter that stays 0).
4. `server filter errors`: the POST answers `problemResponse(400, "invalid_filter", { errors: [{ field: "filter", message: "unexpected end of input" }] })` → the message shows under Filter.
5. `preview asks for window-wide buckets`: capture the `/metrics` request (`server.events.on("request:start", …)`) → `bucket=5m` and a 24 h range; choose window `24h` → a request with `bucket=1d` and a 14-day range.
6. `no preview for llm spans`: choose p95 latency, Latency of LLM spans → "No preview for LLM span latency." and no new `/metrics` request.
7. `viewers cannot create`: role `viewer` → Forbidden.

`alerts/$ruleId/edit.test.tsx` (`renderApp("/acme/support-bot/alerts/01900000-0000-7000-8000-000000000510/edit")`):
8. `edits the name only`: the form starts with the fixture values (threshold `5`, filter `environment = 'production'`, both channels checked, reminders on with `60` minutes); change the name → PATCH body exactly `{ name: "Errors 2" }`; answered with the rule → the URL becomes the rule page.
9. `warns that a new condition resets the state`: change the threshold → the warning text shows; submit → the PATCH body has `condition` with `threshold: 0.1` and no other top-level field but `condition`.

`alerts/index.test.tsx` and `$ruleId/index.test.tsx`: add `New rule is disabled for viewers` and `Edit links to the editor`.

## 7. Closeout

`docs/roadmap.md`, row K: set Plan to `[plans/k-alerts/](plans/k-alerts/) (K1–K12)`, Status to `**Implemented.** K1–K12.` (leave PR numbers for the coordinator), Decisions to `D-K-1–D-K-13`. Also set `Status:` at the top of `docs/plans/k-alerts/README.md` to `implemented on <date>`.

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
pnpm --dir web test -- writes
```

```bash
just check
```

## Done when

- A member can create, edit, enable/disable and delete a rule from the UI; the preview draws the metric with the threshold line; dev mocks answer every alert write.
- The new and updated tests pass; `just check` is green; the roadmap row says implemented.
