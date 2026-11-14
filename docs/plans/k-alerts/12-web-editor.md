# K12 — Web: the rule editor with the preview chart, dev write mocks, closeout

**Commit subject:** `web: add the alert rule editor`

Adds `/$org/$project/alerts/new` and `/$org/$project/alerts/$ruleId/edit` with one form component, a preview chart of the metric with the threshold line (D-K-13), the "New rule" and "Edit" buttons on K11's pages, stateless dev write mocks for every alert and channel write (D-L-10), and marks track K implemented in the roadmap.

## Read first

- README § Design, D-K-2, D-K-3, D-K-4, D-K-13.
- K3's condition rules (`docs/plans/k-alerts/03-types.md` § 3) — the form mirrors them; the server stays the authority.
- K10 (`queries-alerts.ts`, `channel-dialog.tsx` form style, `test/fixtures.ts`), K11 (`lib/alerts.ts`, the two pages).
- `web/src/components/charts/metric-chart.tsx` (`ChartContainer`, `ComposedChart`, `Line`, `XAxis`, `YAxis`, `ReferenceLine`), `web/src/components/ui/chart.tsx`, `web/src/lib/metrics.ts` (`seriesByMetric`, `toChartRows`, `ChartRow`, `formatMetric`), `web/src/api/queries.ts` (`metricsQuery`: it always sends `bucket: "auto"`, so the preview builds its own query).
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
 *  with `String(Math.round(value * 1e6) / 1e6)`; `cooldownMinutes` is `String(Math.round(cooldown_s / 60))`.
 *  Fields the metric does not use take the defaults of DEFAULT_RULE_FORM (`target` "trace",
 *  `scoreName` "", `aggregation` "avg", `minCount` "20" for cost); `filter` is the stored filter or "". */
export function formFromRule(rule: Schema<"AlertRule">): RuleFormValues;

/** The condition to send, or field errors keyed like the server's (`condition.threshold`,
 *  `condition.min_count`, `condition.score_name`, `cooldown_s`). Only the fields of the metric are
 *  set: `min_count` not for cost, `target` only for latency, `score_name` and `aggregation` only for score.
 *  An empty filter is omitted. */
export function conditionFromForm(form: RuleFormValues):
  | { ok: true; condition: Schema<"AlertCondition">; cooldownS: number }
  | { ok: false; errors: Record<string, string> };
```

The client range checks below go beyond D-L-11's "presence only" on purpose: the percent ↔ fraction conversion has to parse the number before anything is sent, so the form reports what it already knows. The server stays the authority.

Values are trimmed first: `name.trim()`, `filter.trim()` (empty → omitted), `scoreName.trim()`.

Client checks in `conditionFromForm` (messages exact):
- `threshold`: `form.threshold.trim() === ""` or `!Number.isFinite(Number(form.threshold))` → `"Enter a number."` (`Number("")` is 0, so the blank check comes first); percent metrics outside 0–100 → `"Enter a percentage from 0 to 100."`; cost below 0 → `"Enter 0 or more."`; latency ≤ 0 → `"Enter a duration above 0."`. Percent values are divided with `Number((p / 100).toPrecision(12))` (so `0.07` gives `0.0007`, not `0.0007000000000000001`).
- `minCount` (not cost): an integer from 1 to 1 000 000, else `"Enter a whole number from 1 to 1,000,000."`.
- `scoreName` (score): blank → `"Enter a score name."`.
- `cooldownMinutes`: checked only when `notifyWhileFiring` is on: an integer from 5 to 10 080, else `"Enter 5 to 10,080 minutes."`; `cooldownS = minutes * 60`. When reminders are off, `cooldownS` is 3600 on create and is not sent on PATCH.

```ts
/** A full condition for labels and axes, also while the form is invalid: an unparsable
 *  threshold becomes 0, a blank score name stays blank. Never sent. */
export function previewCondition(form: RuleFormValues): Schema<"AlertCondition">;

/** The condition-related form fields of `a` and `b` differ (metric, window, trimmed filter,
 *  comparator, threshold, minCount, target, trimmed scoreName, aggregation, compared as
 *  strings after trimming). Used to decide whether a PATCH sends `condition`. */
export function conditionChanged(a: RuleFormValues, b: RuleFormValues): boolean;
```

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

Tests in `alerts.test.ts`: `thresholdUnit` and `thresholdInPercent` for each metric and aggregation; `previewParams` for each of the six windows; `conditionFromForm` for latency with target `llm_span` includes `target: "llm_span"`; `conditionFromForm` with threshold `""` and `"abc"` → `"Enter a number."`; `formFromRule` of a rule with threshold 0.07 (error rate) → threshold `"7"`; `conditionChanged` false for `formFromRule(rule)` against itself; `conditionFromForm` for each metric (defaults → `{"metric":"error_rate","window":"5m","comparator":"gt","threshold":0.05,"min_count":20}` exactly); each error message; `formFromRule` of each fixture rule round-trips through `conditionFromForm` to the same condition; `previewParams("6h", Date.parse("2026-01-04T00:00:00Z"))` → `{ bucket: "6h", from: "2026-01-01T00:00:00.000Z", to: "2026-01-04T00:00:00.000Z" }`; `previewSeries` for the five cases (score avg and score pass rate both use `score_avg` with the score name as group).

## 2. `components/alerts/rule-preview.tsx`

```tsx
export function RulePreview(props: {
  org: string;
  project: string;
  values: RuleFormValues;
})
```

No return type annotations on components anywhere in K12 (React 19 has no global `JSX` namespace).

- Hooks run unconditionally. `const series = previewSeries(values);` When it is undefined, the component renders a muted box: "No preview for LLM span latency. The dashboard shows it per model." (the query below is disabled).
- Debounce: `const [filter, setFilter] = useState(values.filter.trim());` and `useEffect(() => { const timer = setTimeout(() => setFilter(values.filter.trim()), 500); return () => clearTimeout(timer); }, [values.filter]);` (the first render does not wait). Then query `$api.useQuery("get", "/api/v1/projects/{project}/metrics", { params: { path: { project: projectRef(org, project) }, query: { from, to, bucket, ...(filter ? { filter } : {}) } } }, { staleTime: 60_000, placeholderData: keepPreviousData, enabled: series !== undefined })` (`keepPreviousData` from `@tanstack/react-query`). Compute `from`/`to` once per window change, `const params = useMemo(() => previewParams(values.window, Date.now()), [values.window]);`, so the query key does not change every render.
- Data: `seriesByMetric(response).get(series.metric)`, the entry whose `group` equals `series.group` when one is given (else the entry without a group). For latency, map its points to `{ ...point, value: point.value === undefined ? undefined : point.value / 1_000_000 }` (ns → ms; keep the point type of `MetricSeries`). Then `toChartRows([{ ...entry, group: undefined }], response.bucket_ns)`; without a group the row key is `all` (read `toChartRows` to confirm the key and use what it produces). Error rate and pass rate stay fractions.
- Threshold for the line: the `condition.threshold` of `conditionFromForm(values)` when it is `ok` (in the series unit: fraction, USD, ms, raw); none when the form has errors.
- Chart: `ChartContainer` with `data-chart-container=""` (the test layout sizes charts by this attribute), `config={{ all: { label: metricLabel(previewCondition(values)), color: "var(--chart-1)" } }}`, `initialDimension={{ width: 640, height: 160 }}`, `className="aspect-auto h-40 w-full"`, around a `ComposedChart` with `XAxis dataKey="time"` and `tickFormatter={(ms) => formatTimestamp(new Date(ms).toISOString(), zone, "datetime")}` (`const zone = useTimeZone();` at the top of the component), `YAxis` with `tickFormatter={(value) => formatAlertValue(previewCondition(values), typeof value === "number" ? value : undefined)}`, one `Line dataKey="all"`, `isAnimationActive={false}`, and, when there is a threshold, `<ReferenceLine y={threshold} stroke="var(--destructive)" strokeDasharray="4 4" ifOverflow="extendDomain" label={{ value: "Threshold", position: "insideTopRight", fontSize: 11 }} />`. Wrap it in `figure` with `aria-label="Preview of the metric over the last <24 hours|3 days|14 days>"`.
- Under the chart: "Each point is the metric over one window, as the rule sees it. Minimum samples are not shown." in `text-xs text-muted-foreground`.
- Query states: loading → `Skeleton className="h-40 w-full"`; a problem with `problemOf(error)?.code === "invalid_filter"` → `FormError` with `The filter is invalid: ${filterMessage(problem)}` (`filterMessage` is exported by `components/filter-bar/logic.ts`); other errors → "The preview could not be loaded."; no point with a value → "No data in this range."

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
})
```

A `<form noValidate>` (no nested forms: the filter is a plain `Textarea`, not the filter bar) with two columns on wide screens (fields left, `RulePreview` right; one column below `lg`). Sections and fields (ids prefixed `rule-`):

1. **Name** (`TextField`, max 100).
Every `Select` has a `Label htmlFor="rule-<field>"` and `SelectTrigger id="rule-<field>"` (fields: `metric`, `target`, `aggregation`, `window`, `comparator`), so tests reach it with `getByRole("combobox", { name: "<label>" })`; options are reached with `findByRole("option", { name })`.

2. **What to measure**: `Select` "Metric" (options exactly: `Error rate`, `Cost`, `p95 latency`, `Score`); for latency a `Select` "Latency of" (`Whole traces`, `LLM spans`); for score a `TextField` "Score name" and a `Select` "Aggregation" (`Average`, `Pass rate`; below it a `<p className="text-xs text-muted-foreground">Pass rate counts true among boolean scores.</p>`, as `Select` has no description slot); `Select` "Window" (`1 minute`, `5 minutes`, `15 minutes`, `1 hour`, `6 hours`, `24 hours`) with the line `<p className="text-xs text-muted-foreground">Checked every minute for windows up to 15 minutes, every 5 minutes otherwise.</p>` below it
3. **Filter**: a `Label htmlFor="rule-filter"` "Filter" and a `Textarea id="rule-filter"` (`font-mono`, 2 rows, max 4096), described by "A traces filter, e.g. environment = 'production'. Empty: every trace." Errors with field `filter` (the server's `invalid_filter`) show under it, wired like `text-field.tsx`.
4. **Condition**: `Select` "Comparator" (`Above`, `At or above`, `Below`, `At or below` → gt, gte, lt, lte); `TextField` "Threshold" (default text type) whose description is the unit: "Percent." (rates), "USD." (cost), "Milliseconds." (latency) or "Raw score." (score average) — `TextField` has no suffix slot; do not change it; `TextField` "Minimum samples" (hidden for cost) with description "Fewer samples in the window gives No data, which never fires."
5. **Notifications**: a checkbox list of `useQuery(channelsQuery(org, project))` channels (`Checkbox id="rule-channel-<id>"` + `Label` reading `` `${name} (${channelKindLabel(kind)})` ``, e.g. `Team Slack (Slack)`), at most 10 checked (disable the others at 10); no channels → "No channels yet." and a link "Add a channel" to `/$org/$project/settings/channels`. `Switch` "Remind while firing" and, when on, `TextField` "Remind every (minutes)".
6. **Enabled**: `Switch` "Enabled" (create only; the detail page toggles it for an existing rule).

Changing the aggregation sets `threshold` to `"90"` (pass rate) or `"0.5"` (average). Changing the metric resets `threshold` to the metric's default (`5` for error rate, `100` for cost, `5000` for latency, `0.5` for score avg, `90` for pass rate), `comparator` to `gt` (`lt` for score), and `minCount` to `"20"` (`"10"` for score).

Submit:
1. Name blank (after trimming) → `"Enter a name."` on the name field; then `conditionFromForm`; any error → shown on the fields, nothing sent. (The name check is not in `conditionFromForm`, which the preview also uses.)
2. Create: `POST …/alert-rules` with `{ name, condition, channel_ids, enabled, cooldown_s, notify_while_firing }` (`Schema<"CreateAlertRuleRequest">`). Edit: `PATCH …/alert-rules/{id}` with only the changed top-level fields, compared with `const initial = formFromRule(rule)`: `name` when the trimmed name differs; `condition` (sent whole) when `conditionChanged(initial, form)`; `channel_ids` when the sorted id lists differ (the server returns them sorted); `notify_while_firing` when it differs; `cooldown_s` when reminders are on and the minutes differ numerically. A PATCH with no field calls `onSaved(rule)` with the unchanged rule, without a request.
3. On success: invalidate `alertRulesQuery(org, project).queryKey`, set `alertRuleQuery(org, project, id)` data to the response, call `onSaved(rule)`.
4. Errors: `fieldErrors` mapped by field name (`name`, `filter`, `condition.filter`, `condition.threshold`, `condition.min_count`, `condition.score_name`, `channel_ids`, `cooldown_s`; `condition.target` and `condition.aggregation` have no own message slot and fall through to the form error); when any mapped field error exists, set no form error. A `403` whose problem `code` is `"plan_limit"` (D-K-14) → `FormError` with the problem's `detail`, followed by a separate `<p className="text-xs">Upgrade your plan, disable another rule, or save this one disabled.</p>`; `409` → form error `problemMessage(error, "A rule with this name already exists.")`; `503` → `"The server is busy. Try again."`; else `problemMessage(error, "Could not save the rule.")`.
5. In edit mode, when `conditionChanged(initial, form)`, show above the buttons: "Saving a new condition resets the rule to OK." (D-K-4).

Buttons: `Cancel` (`variant="outline"`, calls `onCancel`) and `SubmitButton` "Create rule" or "Save".

## 4. Routes and buttons

- Both pages use K11's frame (`<div className="mx-auto flex w-full max-w-5xl flex-1 flex-col gap-6 overflow-auto px-4 py-6">`). Role checks use `canManageAlerts`.
- `alerts/new.tsx`: `createFileRoute("/$org/$project/alerts/new")({ component: NewRulePage })`. Members and above: `h1` "New alert rule" and `RuleForm` with `onSaved={(rule) => navigate({ to: "/$org/$project/alerts/$ruleId", params: { org, project, ruleId: rule.id } })}` and `onCancel` back to `/$org/$project/alerts`. Viewers: `<Forbidden description="Only project members and above can create alert rules." />`.
- `alerts/$ruleId/edit.tsx`: `createFileRoute("/$org/$project/alerts/$ruleId/edit")` with the same loader as the detail page (404 → `notFound()`); viewers get `Forbidden`; `h1` "Edit <name>"; `RuleForm rule={…}`; saved or cancelled → the rule page.
- `alerts/index.tsx`: a "New rule" button in the header. Members and above: `<Button asChild><Link to="/$org/$project/alerts/new" params={{ org, project }}><Plus /> New rule</Link></Button>`. Viewers: `<DisabledReason reason="Only project members and above can manage alerts"><Button disabled><Plus /> New rule</Button></DisabledReason>` (a link cannot be disabled). `EmptyState` takes no children; on an empty list the header button is hidden and the same button is rendered in a centered `div` after the `EmptyState`, so there is always one "New rule" button.
- `alerts/$ruleId/index.tsx`: an "Edit" button next to "Delete rule", built the same way (a link to `/$org/$project/alerts/$ruleId/edit` for members, a disabled button for viewers).

## 5. Dev write mocks (`mocks/writes.ts`)

Add to `WRITE_OPERATIONS`: `create_channel` (post `/api/v1/projects/{project}/channels`), `update_channel` (patch `…/channels/{id}`), `delete_channel` (delete `…/channels/{id}`), `test_channel` (post `…/channels/{id}/test`), `create_alert_rule` (post `/api/v1/projects/{project}/alert-rules`), `update_alert_rule` (patch `…/alert-rules/{id}`), `delete_alert_rule` (delete `…/alert-rules/{id}`). Responses (import `channels.json` and `alert-rules.json`):
Fixture rows are `T | undefined` under `noUncheckedIndexedAccess`; pick them with `find`, never by index plus `!`:
- `create_channel`: `const sample = channels.data.find((row) => row.kind === body.kind) ?? channels.data.find(() => true);` then `201` with `{ ...sample, name: trimmed(body.name) ?? sample?.name }`.
- `update_channel`: the channel whose `id` is `params.id` (else the first row via `find(() => true)`) with the new name when given.
- `test_channel`: `{ ok: true }`.
- `create_alert_rule`: take the rule named `High error rate` with `find`, drop its `last_*` fields by destructuring them into `_`-prefixed names (as `writes.ts` does for `patch_me`), and answer `201` with `{ ...rest, name: trimmed(body.name) ?? rest.name, condition: body.condition ?? rest.condition, state: "ok" }`.
- `update_alert_rule`: the rule whose `id` is `params.id` (else the first row via `find(() => true)`) merged with the body's top-level fields.
- `delete_channel`, `delete_alert_rule`: `noContent()`.

`writes.test.ts`: extend `covered` with `path.startsWith("/api/v1/projects/{project}/channels") || path.startsWith("/api/v1/projects/{project}/alert-rules")` (`LATER_TRACKS` stays empty), and add one test like the existing ones, using the file's `PROJECT` constant (`/api/v1/projects/acme%2Fsupport-bot`): a create of a channel and of a rule each return `201` with the submitted name, an update of each returns `200` with the new name, each delete returns `204`, and `test_channel` returns `200 { ok: true }`.

## 6. Tests

`alerts/new.test.tsx` (`renderApp("/acme/support-bot/alerts/new")`):
Viewers: `http.get("/api/v1/projects/:project", () => HttpResponse.json({ ...projectFixture, role: "viewer" }))`. URL checks: `await waitFor(() => expect(router.state.location.pathname).toBe(…))`. Use real timers (MSW and user-event hang with fake timers).

1. `creates an error rate rule`: type the name, check `getByRole("checkbox", { name: "Team Slack (Slack)" })`, submit → POST body `{ name: "Errors", condition: { metric: "error_rate", window: "5m", comparator: "gt", threshold: 0.05, min_count: 20 }, channel_ids: [SLACK_CHANNEL_ID], enabled: true, cooldown_s: 3600, notify_while_firing: false }`; answer `201` with the `High error rate` fixture → the pathname becomes `/acme/support-bot/alerts/01900000-0000-7000-8000-000000000510`.
2. `score fields`: choose Score, aggregation Pass rate, score name `ok`, threshold `90`, comparator below → body condition `{ metric: "score", window: "5m", comparator: "lt", threshold: 0.9, min_count: 10, score_name: "ok", aggregation: "pass_rate" }`.
3. `client errors`: clear the threshold → "Enter a number." and no POST (a call counter on a `server.use` POST handler stays 0).
3b. `plan limit`: the POST answers `problemResponse(403, "plan_limit", { detail: "Your plan allows 1 enabled alert rule." })` → `findByText("Your plan allows 1 enabled alert rule.")` and `getByText("Upgrade your plan, disable another rule, or save this one disabled.")`.
3c. `reminders`: turn on "Remind while firing", type `120` → the body has `notify_while_firing: true, cooldown_s: 7200`; `3` → "Enter 5 to 10,080 minutes." and no POST.
3d. `other server errors`: a `409` with detail `an alert rule with this name already exists` shows that detail; a `503` shows "The server is busy. Try again."
3e. `channels`: the channels list answering 11 channels (ids `01900000-0000-7000-8000-0000000006NN`) → after checking 10, the 11th checkbox is disabled; an empty list → "No channels yet." and the link "Add a channel".
4. `server filter errors`: the POST answers `problemResponse(400, "invalid_filter", { errors: [{ field: "filter", message: "unexpected end of input" }] })` → the message shows under Filter.
5. `preview asks for window-wide buckets`: capture the `/metrics` request (`server.events.on("request:start", …)`) → `bucket=5m` and a 24 h range; choose window `24h` → a request with `bucket=1d` and a 14-day range.
6. `no preview for llm spans`: wait for the first `/metrics` request to settle and record the count; choose p95 latency, then Latency of LLM spans → `getByText(/No preview for LLM span latency/)` and the count is unchanged.
6b. `debounces the filter`: type `error_count > 0` into Filter → `waitFor` a `/metrics` request whose `filter` is `error_count > 0`; exactly one request carries a filter.
6c. `preview states` (the default `/metrics` fixture answers any query, so the default state renders the chart): first assert the figure labelled "Preview of the metric over the last 24 hours"; then `/metrics` answering `problemResponse(400, "invalid_filter", { errors: [{ field: "filter", message: "bad" }] })` → "The filter is invalid: bad"; a response with no points → "No data in this range." (the figure is rendered only when there is data).
7. `viewers cannot create`: role `viewer` → Forbidden.

`alerts/$ruleId/edit.test.tsx` (`renderApp("/acme/support-bot/alerts/01900000-0000-7000-8000-000000000510/edit")`):
8. `edits the name only`: the form starts with the fixture values (threshold `5`, filter `environment = 'production'`, both channels checked, reminders on with `60` minutes); change the name → PATCH body exactly `{ name: "Errors 2" }`; answered with the rule → the URL becomes the rule page.
10. `edit without changes sends nothing`: submit at once → no PATCH (counter 0) and the pathname becomes the rule page.
11. `edit as a viewer or of an unknown rule`: `viewer` → Forbidden; the rule GET answering `problemResponse(404, "not_found")` → `findByRole("heading", { name: "Page not found" })`.
9. `warns that a new condition resets the state`: change only the name → no warning; then change the threshold to `10` → the warning text shows; submit → the PATCH body has `name` and `condition` (with `threshold: 0.1`) and no other field.

`alerts/index.test.tsx` and `$ruleId/index.test.tsx`: add `New rule is disabled for viewers` (`getByRole("button", { name: "New rule" })` is disabled and there is no link of that name) and `Edit links to the editor`.

## 7. Closeout

`docs/roadmap.md`, row K: Status becomes `**Done.** K1–K12.` (leave PR numbers for the coordinator); if the "Order" paragraph below the table still lists K as next, drop it from there. The first line of `docs/plans/k-alerts/README.md` becomes `Status: K1–K12 done on <date +%F>`.

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
pnpm --dir web test -- writes
```

```bash
just check
```

## Done when

- A member can create, edit, enable/disable and delete a rule from the UI; the preview draws the metric with the threshold line; dev mocks answer every alert write.
- The new and updated tests pass; `just check` is green; the roadmap row says implemented.
