# CF8 — Dashboard charts: one tab stop each (M8)

See [README.md](README.md) for the rules that apply to every phase.

**Edit only:**
- `web/src/components/charts/metric-chart.tsx`;
- `web/src/routes/$org/$project/dashboard/index.test.tsx`;
- `docs/spec/11-ui.md`;
- `docs/mvp-decisions.md`;
- the `Status:` line of `docs/plans/c-fixes/README.md`.

**Commit:** `web: make each dashboard chart one keyboard stop`

## Problem

`BucketBar` and `lineDot` in `metric-chart.tsx` draw every bar and every line point as a `<button>` inside a `<foreignObject>`, with the label "Open bucket <ISO time>".

Every one of them is a tab stop. On a 24 h range with `?mock=large`, the dashboard had 1 578 focusable buttons on 2026-11-13. The page cannot be used with the keyboard. U29 requires every page to be usable with the keyboard alone.

## Keyboard model (D-C-3)

- Each chart is **one** tab stop: a focusable `role="group"` with `aria-roledescription="chart"`.
- With the chart focused:
  - → selects the next bucket (from no selection: the first);
  - ← selects the previous one (from no selection: the last);
  - Home selects the first; End selects the last;
  - Enter or Space opens the traces of the selected bucket, exactly as a click on that bucket does (`onBucket(time)`);
  - Escape clears the selection.
- The selected bucket is:
  - announced through a polite live region, as the formatted time plus each series' value;
  - drawn as a vertical reference line.
- The selection clears when the chart loses focus.
- The per-point buttons stay for the mouse. They get `tabIndex={-1}`, and their `foreignObject` gets `aria-hidden="true"`.

## Changes — `web/src/components/charts/metric-chart.tsx`

1. **Imports.** Add `ReferenceLine` to the `recharts` import, and `useState` to the `react` import.

2. **Pass the title.** `ChartCard` renders `<MetricPlot columns={columns} rows={rows} onBucket={onBucket} />`. Add `title={spec.title}`, and add `title: string` to `MetricPlot`'s props.

3. **`MetricPlot`:**

   a. Add state, and clamp it whenever `rows` gets shorter:

   ```ts
   const [active, setActive] = useState<number | null>(null);
   const index = active !== null && active < rows.length ? active : null;
   const activeRow = index === null ? undefined : rows[index];
   ```

   b. Add a key handler:

   ```ts
   function onKeyDown(event: React.KeyboardEvent<HTMLDivElement>) {
     if (rows.length === 0) return;
     const last = rows.length - 1;
     let next: number | null | undefined;
     if (event.key === "ArrowRight") next = index === null ? 0 : Math.min(last, index + 1);
     else if (event.key === "ArrowLeft") next = index === null ? last : Math.max(0, index - 1);
     else if (event.key === "Home") next = 0;
     else if (event.key === "End") next = last;
     else if (event.key === "Escape") next = null;
     else if ((event.key === "Enter" || event.key === " ") && activeRow) {
       event.preventDefault();
       onBucket(activeRow.time);
       return;
     }
     if (next === undefined) return;
     event.preventDefault();
     setActive(next);
   }
   ```

   If the file has no `React` namespace import, import the type: `import type { KeyboardEvent } from "react"`. Then use `KeyboardEvent<HTMLDivElement>`.

   c. Wrap the returned `ChartContainer` in the focusable group, and move the container's outer spacing to it:

   ```tsx
   return (
     // biome-ignore lint/a11y/useSemanticElements: a chart is a focusable group with a custom keyboard model (D-C-3)
     // biome-ignore lint/a11y/noNoninteractiveTabindex: the chart is one keyboard stop (D-C-3)
     <div
       role="group"
       aria-roledescription="chart"
       aria-label={`${title}. Arrow keys move between time buckets; Enter opens the traces of the bucket.`}
       tabIndex={0}
       className="focus-ring mt-2 flex min-h-0 flex-1 flex-col rounded-sm"
       onKeyDown={onKeyDown}
       onBlur={(event) => {
         if (!event.currentTarget.contains(event.relatedTarget)) setActive(null);
       }}
     >
       <ChartContainer
         config={config}
         data-chart-container=""
         initialDimension={{ width: 640, height: 160 }}
         className="aspect-auto min-h-0 w-full flex-1"
       >
         {/* ComposedChart unchanged, plus the ReferenceLine below */}
       </ChartContainer>
       <p className="sr-only" aria-live="polite">
         {activeRow ? bucketSummary(activeRow, columns, zone, style) : ""}
       </p>
     </div>
   );
   ```

   - The container's old class string was `mt-2 aspect-auto min-h-0 w-full flex-1`. `mt-2` moves to the wrapper; the rest stays on the container.
   - Two Biome rules fire on this element; both were checked on 2026-11-13. Put both comments directly above `<div`:

     ```tsx
     // biome-ignore lint/a11y/useSemanticElements: a chart is a focusable group with a custom keyboard model (D-C-3)
     // biome-ignore lint/a11y/noNoninteractiveTabindex: the chart is one keyboard stop (D-C-3)
     ```

     The snippet above already has both. If any other rule fires, report it rather than suppressing it.
   - **Turn off Recharts' own keyboard layer.** On `<ComposedChart …>` add `accessibilityLayer={false}`.
     - Recharts 3 defaults it to `true`, which renders the chart `<svg>` with `tabindex="0"` and `role="application"`, with its own arrow-key handling.
     - Without this, each card keeps two tab stops and the new test fails.

   d. Inside `ComposedChart`, after the series `map`, add:

   ```tsx
   {activeRow ? (
     <ReferenceLine x={activeRow.time} stroke="var(--ring)" strokeWidth={1.5} ifOverflow="extendDomain" />
   ) : null}
   ```

4. **`bucketSummary`** — a new function next to `bucketLabel`:

   ```ts
   function bucketSummary(
     row: ChartRow,
     columns: readonly Column[],
     zone: TimeZoneSetting,
     style: "time" | "datetime",
   ): string {
     const values = columns
       .map((column) => `${column.label} ${formatMetric(column.metric, row[column.key] ?? null)}`)
       .join(", ");
     return `${formatTimestamp(new Date(row.time).toISOString(), zone, style)}: ${values}`;
   }
   ```

   `formatTimestamp`, `formatMetric`, `ChartRow`, `Column` and `TimeZoneSetting` are already imported or defined in the file. `style` is the value `MetricPlot` already computes with `axisStyle(rows)`.

5. **Mouse-only point targets.** In `BucketBar`, the `<foreignObject …>` wrapping the button gets `aria-hidden="true"`, and the `<button>` gets `tabIndex={-1}`. Do the same in the `lineDot` renderer. Keep their `aria-label` and `onClick`: the existing click test finds the target with `querySelector('[aria-label="Open bucket …"]')`.

## Tests — `web/src/routes/$org/$project/dashboard/index.test.tsx`

The `get_metrics` fixture (`fixtures/api/static/metrics.json`) has `bucket_ns` 600 000 000 000 (10 min). Its `traces` series has buckets from `2026-01-01T10:00:00Z` to `2026-01-01T10:50:00Z`, six in all.

Add `fireEvent`, `within` and `waitFor` to the `@testing-library/react` import if they are missing.

```ts
it("gives each chart one tab stop", async () => {
  await renderApp("/acme/support-bot/dashboard");
  const card = await screen.findByRole("region", { name: /Traces & errors/ });
  await waitFor(() => {
    expect(card.querySelector('[aria-label^="Open bucket"]')).toBeInstanceOf(Element);
  });
  const stops = [...card.querySelectorAll<HTMLElement>("button, [tabindex]")].filter(
    (element) => element.tabIndex >= 0,
  );
  expect(stops).toHaveLength(1);
  expect(stops[0]).toHaveAttribute("role", "group");
  for (const point of document.querySelectorAll<HTMLElement>("foreignObject button")) {
    expect(point.tabIndex).toBe(-1);
  }
});

it("opens a bucket from the keyboard", async () => {
  const { router } = await renderApp(
    "/acme/support-bot/dashboard?filter=status%20%3D%20'error'&eval=1&exp=1",
  );
  const card = await screen.findByRole("region", { name: /Traces & errors/ });
  const chart = await within(card).findByRole("group", { name: /^Traces & errors\./ });
  chart.focus();
  fireEvent.keyDown(chart, { key: "ArrowRight" });
  fireEvent.keyDown(chart, { key: "ArrowRight" });
  const live = card.querySelector('[aria-live="polite"]');
  expect(live?.textContent).toContain("Traces");
  fireEvent.keyDown(chart, { key: "Enter" });
  await waitFor(() => {
    expect(router.state.location.pathname).toContain("/traces");
  });
  expect(router.state.location.search).toMatchObject({
    from: "2026-01-01T10:10:00.000Z",
    to: "2026-01-01T10:20:00.000Z",
    filter: "status = 'error'",
    eval: 1,
    exp: 1,
  });
});

it("jumps to the last bucket with End", async () => {
  const { router } = await renderApp("/acme/support-bot/dashboard");
  const card = await screen.findByRole("region", { name: /Traces & errors/ });
  const chart = await within(card).findByRole("group", { name: /^Traces & errors\./ });
  chart.focus();
  fireEvent.keyDown(chart, { key: "End" });
  fireEvent.keyDown(chart, { key: "Enter" });
  await waitFor(() => {
    expect(router.state.location.search).toMatchObject({
      from: "2026-01-01T10:50:00.000Z",
      to: "2026-01-01T11:00:00.000Z",
    });
  });
});
```

Notes on these tests:
- If the existing test `"opens traces for the clicked bucket"` shows that the card's region name is not matched by `/Traces & errors/`, use the form that test uses.
- If the `ChartRow` key that holds the traces count gives a summary whose label is not "Traces", adjust the `toContain` to the label `columnLabel` produces for the `traces` metric, and report it. `METRIC_LABEL.traces` is "Traces".
- `/acme/support-bot/dashboard` with no params is not a list page, so no default saved view applies.

The existing test `"opens traces for the clicked bucket"` must pass unchanged.

## Docs

**`docs/spec/11-ui.md` § Dashboard.** After "Clicking a chart point opens the trace list filtered to that bucket.", insert:

> Each chart is one keyboard stop: ←/→ move between buckets (announced, and marked with a line), Home/End jump to the ends, Enter opens the bucket's traces (D-C-3).

**`docs/mvp-decisions.md`, under `## Track C — Web UI`.** Add:

```markdown
| D-C-3 | Chart keyboard | **Each dashboard chart is one tab stop** (`role="group"`, `aria-roledescription="chart"`). ←/→ move between buckets (from none: the first or the last), Home/End jump, Enter/Space opens the bucket's traces like a click, Escape clears; the bucket is announced in a polite live region (time and each series' value) and marked with a reference line. The per-point click targets stay for the mouse with `tabIndex=-1` inside an `aria-hidden` `foreignObject`, and Recharts' own keyboard layer is off (`accessibilityLayer={false}`). |
```

## Done when

- `just check-web` is green.
- The three new tests pass.
- The click test is unchanged.
