# Review · Track C (web UI on mocks)

Reviewed on 2026-11-13 against `main` at `8022dc8`.

**Scope:**
- the plan `docs/plans/c-web-ui/` (README and 01–05, U1–U29);
- spec `docs/spec/11-ui.md`;
- decisions D123–D134;
- the `AGENTS.md` conventions;
- the code in `web/`.

**Method:**
- Four read-only subagents each reviewed a group of sub-phases: U1–U8, U9–U14, U15–U20 and U21–U29.
- I checked every finding kept below against the code. The ones marked *(browser)* I also reproduced in the browser on `just web-dev`.
- I ran the web suite 13 times under CPU load (§ Flaky tests).
- I checked contract alignment against `crates/server/openapi.json` and `fixtures/api/manifest.json`.

## Summary

Track C is complete and mostly faithful to its plan:
- All 29 sub-phases landed.
- Every API call the pages make exists in the OpenAPI document.
- URL state goes through plain `validateSearch` helpers.
- The quality-bar row-count tests exist.
- There is no `console.*` and no `vi.mock`.

Track G should know about three groups of problems before it wires the real API:

1. **The filter bar can change what a filter means** (U11/U12):
   - A top-level `OR`, a `BETWEEN … AND …`, or a click-to-filter on an `OR` filter is split or joined with a bare `AND`.
   - The chips then show a different filter from the one applied, and editing a chip applies a different one again.
   - This goes against D129 ("a broken filter is never applied silently").
2. **List and keyboard bugs** that the mocks hide:
   - The table header does not scroll horizontally with the body.
   - `j`/`k` restart at row 1 after a deep link.
   - The default saved view comes back whenever the search becomes empty.
   - Live mode ignores `search`, and its buffers have no bound.
3. **The external-media gate can be bypassed** (U18). `HTTPS://…` (upper case), `//host/x.png` and other schemes load without the "Load" click. This breaks the "no external network calls" rule in spec 11.

The flaky `just check-web` failures come from load, not from logic. Tests that take about 2 s at idle exceed Testing Library's 1 s `findBy*` default, or Vitest's 5 s test timeout, once the CPU is contended. Raising both made 3 out of 3 loaded runs pass (§ Flaky tests).

**Counts of confirmed findings:** 0 blocker · 14 major · 27 minor.

**The U8 review gate is not on record** (see Open questions).

## Status per sub-phase

| Sub-phase | Status | Note |
|---|---|---|
| U1 Spec and decisions | as planned | D123–D134 are in `mvp-decisions.md`; spec 11, 04 and 05 are updated |
| U2 Tokens and theme | as planned | Dark is applied by JS only, so the first paint can flash light (m) |
| U3 Formatting and primitives | as planned | Rounding at bucket boundaries; the tooltip and `IdText` are untested (m) |
| U4 Seeded mock data | as planned | The `check-dist` needle `mulberry32` is unlikely to survive minification (m) |
| U5 App shell | as planned | — |
| U6 Dense table and traces list | deviates | The header does not scroll horizontally (M). j/k start at row 1 after a deep link (M). No loading state (m) |
| U7 Trace view v1 | as planned | Tree keyboard and split-ratio persistence are untested (M) |
| U8 Review gate | **unclear** | No record of the user review, and no U8a/U8b |
| U9 Time range, search, toggles | as planned | Stale debounce timer after Back (m) |
| U10 Columns and sorting | as planned | The sort cycle writes default values to the URL (m) |
| U11 DSL chip parser | **deviates** | OR precedence and BETWEEN are split (M) |
| U12 Filter bar | deviates | `appendFilter` ignores OR (M). The error message shows twice (m). The editor remounts on any prop change (m) |
| U13 Spans page | as planned | Thin tests: no sort, cols, search or eval tests (m) |
| U14 Saved views | deviates | The default view is re-applied on every empty search (M). Modified detection is noisy (m) |
| U15 Timeline and tree search | as planned | Search costs 150–360 ms per keystroke on 10 000 spans (m). Wheel, drag, `0` and Shift+Enter are untested (M) |
| U16 Span header, attributes | as planned | Unguarded `localStorage` (m). Duplicate React keys (m) |
| U17 JSON viewer, I/O tab | deviates | The viewer resets its state on parent re-render (m). The 1 MB cap does not offer JSON/Raw first (m). Path collision (m) |
| U18 Chat tab and media | **deviates** | The external-URL gate can be bypassed (M) |
| U19 Scores, header, full page | as planned | Errors are an inline `<p>` (m) |
| U20 Transcript | deviates | The dedupe relies on an unfetched previous span (M). A `?span=` beyond the cap is not shown (m) |
| U21 Sessions | as planned | Dead `sessionsSearch` router state (m) |
| U22 Users | as planned | — |
| U23 Dashboard | deviates | 1 578 tab stops in the charts (M). The colour cycle repeats after 5 series (m) |
| U24 ⌘K and shortcuts | as planned | Shortcuts still fire under open overlays (m) |
| U25 Onboarding | as planned (plan gap) | Shows for "no traces in the preset range", not "no traces ever" (m, open question) |
| U26 SSE client | as planned | Weak backoff and unmount tests (m) |
| U27 Live traces list | deviates | Ignores `search` (M). Unbounded buffer and rows (M). Retries forever on an invalid filter (m) |
| U28 Live trace view | as planned | A root span is inserted as `detached` (m). The `follow` window is recomputed on every render (m) |
| U29 Quality bar | mostly | Spec 11 keyboard and palette lines are not synced (m). `check-dist` scans only `index.html` for external URLs (m). Light `chart-2` text contrast is 4.0:1 (m) |

M = major, m = minor.

## Confirmed findings

### Major

**M1 · U18: the external-media gate can be bypassed.**
- Where: `web/src/components/payload/media-view.tsx:26`, `:83-87` and `:97-99`; `web/src/lib/media.ts:10`.
- What happens:
  - `isExternal` is `uri.startsWith("https:") || uri.startsWith("http:")`, which is case-sensitive.
  - Every other URI goes through `sourceUrl` → `mediaUrl`, which returns it unchanged.
  - The result is rendered straight into `<img src>` or `<audio src>`.
  - So `HTTPS://evil/x.png`, `//evil/x.png` and other schemes load as soon as the span is opened. This leaks the viewer's IP and works as a tracking pixel.
- Ref: plan U18 § Media ("External URLs are never loaded automatically"); spec 11 § Quality bar.
- Fix:
  - Load automatically only `media://` (through `mediaUrl`) and `data:image|audio/*`.
  - Treat everything else as external and put it behind the Load chip. Parse the scheme case-insensitively.
  - In `MediaChip` hrefs, allow only `media:`, `http(s):` and `data:`.
  - Add tests for `HTTPS://`, `//host/` and `javascript:`.

**M2 · U11/U12: the chips can misrepresent a filter with a top-level OR, and editing a chip changes its meaning.**
- Where: `web/src/lib/dsl/dsl.ts:137-167` (`splitConjuncts`) and `:202-209` (`serializeFilter`); `web/src/components/filter-bar/filter-bar.tsx:123-128`.
- What happens:
  - `a = 1 OR b = 2 AND c = 3` means `a=1 OR (b=2 AND c=3)`.
  - It is split into the chips `[a = 1 OR b = 2]` and `[c = 3]`.
  - Editing `c` serializes to `(a = 1 OR b = 2) AND c = 4`, which is a different predicate.
  - The test at `dsl.test.ts:339` covers only a lone OR.
- Ref: D129; plan U11 ("never changes meaning").
- Fix: if the text has any top-level `OR`, return the whole text as one raw conjunct. Add a test for `x OR y AND z`.

**M3 · U11: `BETWEEN a AND b` is split in the middle.**
- Where: `web/src/lib/dsl/dsl.ts:145`.
- What happens: `cost_total BETWEEN 1 AND 2` becomes the raw chips `cost_total BETWEEN 1` and `2`. Removing either one produces invalid SQL. `BETWEEN` is in the spec 05 grammar, and no test covers it.
- Fix: in `splitConjuncts`, after a `BETWEEN` token at the current depth, skip the next `AND`. Add a test.

**M4 · U12: click-to-filter and `onAddFilter` append with a bare `AND`.**
- Where: `web/src/components/filter-bar/logic.ts:197-200`; callers at `routes/$org/$project/traces/index.tsx:141` and `spans/index.tsx:118`.
- What happens: with `a = 1 OR b = 2`, clicking a status cell produces `a = 1 OR b = 2 AND status = 'error'`. That silently widens the result instead of narrowing it.
- Fix: parenthesize the base when `hasTopLevelOr(base)`, or rebuild it as `serializeFilter([...chips, new])`. Add a test.

**M5 · U14: the default saved view is re-applied whenever the search becomes empty.**
- Where: `web/src/components/saved-views/default-view.ts:32-45`.
- What happens:
  - The check is `empty = Object.keys(search).length === 0`, evaluated on every render, not once when the page opens.
  - So opening `/traces?eval=1` and toggling Eval off brings back the default view's filter.
  - Deleting the loaded view has the same effect.
  - `withoutConstraints` (`traces/index.tsx:490`) works around this only for "Clear filters", by forcing `range=24h`.
- Ref: plan U14 ("when a list page opens with no search params").
- Fix: decide once per page entry, for example with a ref keyed on the initial location. Add a test that toggles off the last param.

**M6 · U6: the table header does not scroll horizontally with the body** *(browser)*.
- Where: `web/src/components/data-table/data-table.tsx:204-218` (header) and `:283` (the `overflow-auto` body).
- What happens:
  - The header row sits outside the scroll container.
  - In the split at 800 px, the body's `scrollWidth` is 2 028 px against a `clientWidth` of 326 px.
  - After `scrollLeft = 300`, the body cells moved 300 px and the header did not, so the columns no longer line up.
  - This happens on every list as soon as a trace is open (D125).
- Fix: put the header inside the scroll container, either as a sticky top row or with `scrollLeft` mirrored onto it. Add a test that the header and the rows share a container.

**M7 · U6/U7: `j`/`k` start from row 1, not from the open trace, after a deep link** *(browser)*.
- Where: `web/src/components/data-table/data-table.tsx:156` (`const origin = focusedIndex ?? -1`).
- What happens:
  - With `?trace=<5th row>` open, pressing `j` selected row 1 and replaced `?trace=`.
  - Enter does nothing until a move has happened.
- Ref: D125; plan U6 § Keyboard.
- Fix: when `focusedIndex` is null, start from the index of `selectedRowId`. Add a test that loads `?trace=<third row>` and presses `j`.

**M8 · U23/U29: every chart point and bar is a tab stop** *(browser)*.
- Where: `web/src/components/charts/metric-chart.tsx:315-321` and `:352-359`.
- What happens:
  - With `?mock=large`, the dashboard has 1 578 focusable `<button>`s inside `foreignObject`, all labelled "Open bucket …".
  - That makes the page unusable with the keyboard, against U29 ("every page usable with the keyboard alone").
  - `keyboard.test.tsx` stops tabbing before the charts, so it does not catch this.
- Fix: give the per-point buttons `tabIndex={-1}`. Add one keyboard path per card, for example a bucket list or arrow keys on a single focusable chart. Add a test that counts tab stops.

**M9 · U27: live mode ignores `search`.**
- Where: `web/src/lib/sse.ts:19-40`; `web/src/lib/search/traces.ts:80-88`.
- What happens:
  - `stream_traces` has no `search` parameter in `openapi.json`.
  - `traceLiveReason` does not disable live mode when `search` is set.
  - So traces that do not match the search are prepended to a searched list.
- Fix: disable live while `search` is set, with the tooltip "Live does not support search". Or request a `search` param on the stream from A2/G. Add a test.

**M10 · U27: live rows and the paused buffer have no bound.**
- Where: `web/src/components/data-table/live-list.ts:27-38`; `web/src/components/data-table/use-live-traces.ts:149`.
- What happens: `prependLiveTraces` grows page 1 without limit. The paused buffer is copied in full on every event (`[trace, ...bufferRef.current]`). On a busy project, a tab left open grows without limit.
- Fix:
  - Cap the buffer (for example 500, shown as "500+ new"), and refetch on reveal.
  - Trim page 1, or refetch once it passes a threshold.
  - Record the cap in spec 11.

**M11 · U20: the Transcript dedupe depends on the previous LLM span's detail, which may never have been fetched.**
- Where: `web/src/components/trace/transcript-view.tsx:186-189` (`enabled: false`, cache read only).
- What happens:
  - If the previous LLM turn is outside the loaded window, still in flight, or failed, `freshInput(undefined, input)` shows the whole history.
  - The turn then jumps once the previous detail arrives.
  - The test passes only because both turns are visible and loaded in order.
- Ref: plan U20 ("show only the messages not seen in the previous call's input + output").
- Fix: have the loader also fetch each visible turn's `previousLlmId` (it counts toward the cap). Show a skeleton until it settles. Add a test with a slow or out-of-window previous span.

**M12 · U7/U15: named tree interactions have no test.**
- What the plan names but no test covers:
  - tree keyboard: ↑/↓, → expand, ← collapse or go to the parent, Enter toggle (`tree-grid.tsx:252-292`);
  - `0` to fit, Shift+Enter to the previous match;
  - Ctrl/⌘+wheel zoom, Shift+wheel pan, header drag-zoom, the hover needle.
- Evidence: `grep ArrowRight|ArrowLeft` over `*.test.*` finds nothing. `tree-grid.test.tsx` has 4 tests: search, errors-only, zoom, and next match.
- Ref: plan README § Tests ("every behaviour named has a test").
- Fix: add page tests on the 4-span fixture trace and on the 10 000-span trace.

**M13 · U7: split ratios and their persistence are untested.**
- Where: `web/src/components/trace/split-pane.tsx:8-12` and `:48-53`.
- What happens: no test reads or seeds `llmobs.split.traces` or `llmobs.split.trace`, or asserts the 40/55 defaults or the fallback for a malformed value.
- Fix: add one test that seeds the values in `localStorage` and asserts the panel sizes.

**M14 · Cross-cutting: `just check-web` fails under CPU load** (see § Flaky tests). The suite's timing assumptions are too tight. This is a CI-reliability defect, not a product bug.

### Minor

1. **U25: onboarding shows when the current preset range is empty, not when the project has no traces at all.** A quiet project, or one whose traces are older than 24 h, sees "Send your first trace" and polls every 5 s forever. The code follows the plan literally (`components/onboarding/state.ts:4-14`), so this is a plan gap; see Open questions.
2. **U17: `JsonView` resets its expand, search and string state on any parent re-render.** `attributes-tab.tsx:183-198` re-parses JSON-string attributes on every render, which produces a new object, so `json-view.tsx:32-38` resets the state. Typing in the Attributes search box collapses every open JSON attribute. `tool-call-card.tsx` has the same pattern. Fix: memoize the parse on the string.
3. **U15: tree search is O(matches × depth) and allocates a `Set` per match** (`tree-grid.tsx:162-169`, `:659-672`). On the 10 000-span trace in the dev build it took 150–360 ms per keystroke *(browser)*, with no debounce. Fix: compute all ancestors in one stack pass, and use `useDeferredValue`.
4. **U28: `insertSpan` marks a real root span (no `parent_span_id`) as `detached: true`** and appends it at the end (`lib/tree.ts:162-167`). Fix: insert it as a root in time order. Add a test.
5. **U28: `follow` is recomputed with `Date.now()` on every render** (`trace-view.tsx:82-85`). A trace left open past the recency window closes its stream on an unrelated re-render. Fix: latch the value when the stream opens.
6. **U27: the stream stays open, and retries forever, while the list shows an `invalid_filter` error** (`traces/index.tsx:195`; `lib/sse.ts:105-110`). Fix: do not open the stream while `filterError` is set.
7. **U28: the `.live-row` keyframes are defined twice**, inline in `live-toggle.tsx:7-16` and in `tree-grid.tsx:49-57`. The data-table flash works only while `LiveToggle` is mounted. Fix: move them into `styles.css`.
8. **U16/U17: unguarded `localStorage`** in `span-detail.tsx:33-38` and `:103-109`, and in `format-switch.tsx:16-22` and `:37`. A browser with blocked storage throws during render. The other stores are guarded with try/catch.
9. **U16: duplicate React keys.** Events are keyed `${time}-${name}`, links `${trace_id}-${span_id}`, and pretty messages by role and text (`events-list.tsx:30,116`; `payload-view.tsx:104-107`). Two identical items collide. `chat-view.tsx` already uses a counter.
10. **U17: `json-rows.ts` paths collide for an empty key.** `{"": {...}}` gets the same path as the root (`json-rows.ts:58`), which gives duplicate keys and collapses the wrong rows.
11. **U17: the 1 MB cap shows a notice and "Render anyway" but does not put JSON/Raw first**, as the plan says (`payload-view.tsx:35-55`). `JSON.stringify` also runs twice per render without memoization.
12. **U20: a `?span=` beyond the 50-turn cap is neither selected nor scrolled to** (`transcript-view.tsx:58-78`). Errored turns have no retry.
13. **U19: trace and span query errors render an inline `<p>{title}</p>`** (`trace-view.tsx:156-160`, `span-detail.tsx:49-53`, `scores-tab.tsx:28-31`). There is no detail and no retry. The plan allows an inline title for the span detail, but the split's trace error should use `ErrorState`.
14. **U12: the list error message appears twice**, once in the page body (`traces/index.tsx:318-322`) and once under the filter bar (`filter-bar.tsx:101-103`).
15. **U12: `FilterEditor` remounts when `value` or the error range changes**, which drops typed text (`editor.tsx:101-166`). It does not close when it loses focus.
16. **U12: the chip popover can silently retype or empty a value.** `name = 5` becomes `'5'`, and an empty IN list gives `IN ()` (`logic.ts:463-513`).
17. **U9: a pending search debounce survives a URL change**, so Back can re-apply the old text. Each debounced search also pushes a history entry (`list-toolbar.tsx:145-178`).
18. **U10: the sort cycle writes `sort=start_time&order=desc`** instead of dropping the defaults (`traces/index.tsx:473-483`). A `score:x` column in `cols` is dropped when no loaded row has that score.
19. **U14: "modified" compares the URL to the view without normalizing defaults.** A view saved without `range` shows as modified once 24h is picked (`saved-views-menu.tsx:56-57`).
20. **U14: spec 11 lists "share with project" and "set as project default" on an existing view, but the menu offers sharing only in "Save as…"**. Either the spec or the menu should change.
21. **U6/U7 accessibility:**
    - Neither grid sets `aria-activedescendant` or row ids, so a screen reader does not announce moves.
    - `aria-rowcount` is a known number even when more pages exist (it should be `-1` then).
    - The column resize handle is mouse-only (`data-table.tsx:276-288`).
    - The tree has no `aria-level`, and `aria-expanded` sits on an inner button rather than the row.
22. **U6: the traces list renders nothing while the first page loads.** There are no skeleton rows and no `aria-busy`.
23. **U24: non-Esc shortcuts still fire while a Radix dialog or menu is open.** `isOverlayOpen()` is checked only for `dismiss` (`palette.tsx:39-45`). Esc inside the search input does not blur it.
24. **U23: the group colours cycle through 5 tokens for up to 9 series** (`metric-chart.tsx:22-28`), so groups 6 to 9 reuse colours. `sumSeries` is O(n²) (`lib/metrics.ts:227`).
25. **U29: spec 11 is not fully synced.**
    - Line 115 lists keyboard shortcuts as `/`, `j/k`, `esc` and `?`. It omits `f`, `t`, `0`, the g-chords and ⌘K.
    - Line 30 describes the palette without span-ID search, recent traces, theme or time zone.
    - Sessions say "expandable into its trace", but the build opens the trace in the right pane.
26. **U4/U29: `check-dist.mjs` is weak.**
    - It scans only `index.html` for `http(s)://`, not the JS and CSS.
    - The `mulberry32` needle is a function name that minification will mangle.
    - No needle covers the SSE mocks.
    - Fix: scan every emitted text file, and add string-literal markers.
27. **Smaller items:**
    - **Contrast (U29):** light `--chart-2`, used for "improved" KPI deltas, is 4.0:1 against the background and is missing from `contrast.test.ts`.
    - **First paint (U2):** `index.html` has no `class="dark"`.
    - **`formatDuration` rounding (U3):** 9.996 s gives `10.00s` and 59.96 s gives `60.0s`, because rounding happens after the bucket is chosen.
    - **Weak tests (U3, U26):**
      - the local-time `formatTimestamp` test compares against the same `Date` getters, so it cannot fail;
      - `sse.test.ts` "closes useStream on unmount" would pass without closing;
      - the backoff sequence is untested.
    - **MSW unhandled requests (tests):** they only warn, because `setup.ts` sets `onUnhandledFrame` but not `onUnhandledRequest`. For example, `traces/index.test.tsx` "keeps the selected row in place…" calls an unmocked `/traces/{id}/stream`.

## Contract alignment

**Every call exists in `openapi.json`:**
- `get_me`, `list_projects`, `get_project`;
- `list_traces`, `get_trace`, `get_span`, `list_spans`;
- `list_sessions`, `get_session`, `list_users`, `get_user`;
- `get_metrics`, `get_schema`, `list_scores`;
- `list_saved_views`, `create_saved_view`, `update_saved_view`, `delete_saved_view`;
- `stream_traces`, `stream_trace`, `get_media`.

No page calls a path outside the schema, and nothing calls a host outside the API.

**Calls without a fixture in `fixtures/api/`:**
- `create_saved_view`, `update_saved_view` and `delete_saved_view` are served by the stateful mock (`web/src/mocks/saved-views.ts`, D131).
- `stream_traces` and `stream_trace` are hand-written in `web/src/mocks/sse.ts`, and only in the browser worker. The node test server has no stream handler, which is why tests use `server.use`.

**Mock-only behaviour that G will meet on the real server:**
- **Query parameters are ignored.**
  - Fixture handlers match an entry when its query keys are a subset of the request's.
  - So `filter`, `sort`, `from`/`to`, `cursor`, `group_by` and `search` return the same body.
  - Tests for these assert the request URL, not the result.
  - `get_metrics` has one entry, so grouped rendering is covered only by `lib/metrics.test.ts`.
- **The saved-views mock is more permissive than spec 04.**
  - PATCH and DELETE work on any visible view; there is no 403.
  - `kind` and `name` are not validated.
  - `limit` is ignored and the cursor is always `null`.
  - The UI hides actions with a client-side `owned` check, and shows `detail ?? title` on errors.
- **The generated handlers return `X-Mock-Filter-Ignored: 1`.** Only `filter=bad` produces a 400 `invalid_filter`. The UI assumes `position.column` is 1-based and counted in characters.
- **Requests are not batched.** Transcript fires up to 50 separate `get_span` calls (R2 is still open). The Scores tab reads one page of `limit=1000` and never follows `cursor`.
- **SSE goes through a test polyfill.** Tests use `test/event-source.ts` instead of the native `EventSource`, so reconnect after an HTTP error (401, 404, 429) is never exercised.
- **Server routes:** `crates/server/src/api/` has modules for every one of these areas. Whether each handler is implemented is outside this review; the A2 and G reviews cover it.

## Flaky tests

**How I ran it:**
- Machine: 32 cores; jsdom; Vitest with its default worker pool and its 5 s test timeout; Testing Library's default `asyncUtilTimeout` of 1 000 ms.
- At idle, the suite passes in about 14 s: 63 files, 351 tests.
- Under load:
  - first, a parallel `cargo build --workspace --all-targets`, plus another session's `cargo test` (load average 12 to 30): 1 failure in 4 runs;
  - then 32 busy-loop processes: 6 failures in 6 runs, 3 to 7 failing tests per run.

**Tests that failed under load** (failures out of 7 failing runs):

| Test | Fails | Error |
|---|---|---|
| `components/trace/span-detail.test.tsx` › shows the fixture span input… | 6 | `findByRole("region", {name: "Span"})` gave up after 1 s |
| `components/trace/scores-tab.test.tsx` › lists the trace and span scores… | 6 | same |
| `test/quality-bar.test.tsx` › keeps a 5 000-trace list under 60 rows | 5 | 5 s test timeout, or `findByRole("grid")` after 1 s |
| `routes/…/dashboard/index.test.tsx` › renders the five KPIs… | 5 | 5 s test timeout |
| `routes/…/traces/index.test.tsx` › virtualizes a large list… | 3 | 5 s test timeout |
| `scores-tab` (no scores), `span-detail` (model/tokens), `layout` (redirect), `palette` (⌘K), `filter-bar` (chips), `tree-grid` (highlight) | 1–2 each | 1 s `findBy` or 5 s timeout |

**Cause:**
- **The first test in each file pays a cold-start cost.** It takes about 2 s at idle, against 0.5–0.9 s for the tests after it. That covers the first `renderApp`, the route modules, the first jsdom layout, and the first MSW and React Query pass. Examples:
  - `span-detail`: 2 168 ms, then 776 ms;
  - `dashboard`: 2 630 ms, then 1 340 ms;
  - `layout`: 1 841 ms, then 76 ms.
- **The seeded-dataset tests generate their data inside the timed test.** For example, `createDataset({traces: 5_000})` at `quality-bar.test.tsx:12`. That test takes 3.8 s at idle.
- **Under contention these times grow 3–4×.** The worst case reached 14.5 s, so they exceed:
  - the 1 s `findBy*` default, which most page tests use for their first `await screen.findByRole(…)`;
  - the 5 s Vitest test timeout.
- **Logic is ruled out.** Fake timers are used only in pure-module tests, and the real-timer sleeps (`setTimeout(80)` in `trace-view.test.tsx:121` and `traces/index.test.tsx:615`) waste time but never assert on time.
- **Code splitting is ruled out.** Turning off `autoCodeSplitting` changed nothing: 4–8 failures in 3 out of 3 loaded runs.

**Experiment:**
- I used a throwaway config, since deleted: `configure({ asyncUtilTimeout: 5000 })` in a setup file, plus `testTimeout: 15000`.
- Under the same 32-process load: **3 of 3 runs passed, with 0 failures.** Wall time was 8.5–13.4 s, so the suite did not get slower.

**Proposed fixes, in order:**
1. **Timeouts:**
   - In `web/src/test/setup.ts`: `configure({ asyncUtilTimeout: 5_000 })`.
   - In `vite.config.ts` `test`: `testTimeout: 15_000`.
   - These are generous ceilings that cost nothing when tests pass. This alone fixed every loaded run.
2. **Move dataset generation out of the timed test:**
   - Build the 5 000-trace and 10 000-span datasets once per file in `beforeAll`.
   - Better, memoize `createDataset` per `(seed, traces)`, with a fixed `now`: `Date.now()` already makes these tests non-deterministic.
3. **Optional, for CI with shared runners:** set `maxWorkers` (for example `"50%"`) so the suite does not oversubscribe a busy machine.
4. **Cleanup:**
   - Replace the fixed `setTimeout(80)` sleeps with `waitFor` on the condition, or with `vi.advanceTimersByTime`.
   - Set `onUnhandledRequest: "error"` so missing handlers fail instead of warning.

## Discarded findings

- **`cn` is not in the plan's dependency table.** It was added before track C, in `1526215`.
- **`?mode` is not in the URL on the spans and session pages.** The plan's URL map gives the spans page no `mode`, and the session page is not a trace route. This is as planned.
- **Code splitting causes the flakiness.** Ruled out by the experiment above.
- **The tick labels and the hover needle use different origins after a zoom.** The plan itself says ticks are relative to the window (U7) and the needle is relative to the trace start (U15). This is consistent with the plan; at most a plan wording issue.
- **`TooltipProvider` per `TimeText`/`IdText`.** This is negligible overhead and has no behavioural effect.
- **The traces and spans routes duplicate about 120 lines.** `AGENTS.md` allows this, and it is not a defect.
- **Dead `sessionsSearch` router state (U21).** Kept only as a note in the status table; it is harmless.
- **`EndUserDetail.sessions` is capped at 100 with no notice.** The contract truncates the list, and the plan does not ask for a notice.
- **The spans page shares the traces split ratio.** Harmless, and only a preference key.
- **The error underline ignores `position.line`.** The DSL is single-line, so `line` is always 1.
- **Double-quoted identifiers containing `AND` are split (U11).** This is real, but no column name in `/schema` contains spaces, so I folded it into M2/M3's fix: tokenize `"…"` as one token.

## Open questions

1. **U8 review gate.** No commit, document or U8a/U8b sub-phase shows that the user reviewed the UI before U9. U2–U13 all landed on 2026-11-11 within a few hours. Did the review happen off-repo? If it did not, the density choices (13 px base, `text-xs` = 9.75 px in tree metric cells, the 40/55 split defaults) have never been validated, and G is a good time to do it.
2. **Onboarding trigger (U25).** The list API cannot answer "this project has never had a trace". Should G add a cheap signal, such as `first_seen` on `get_project` or a wide-range `limit=1` probe? Or does the product accept "nothing in the selected preset range"? Either way, record it as a decision.
3. **Live and `search` (U27).** Should live mode be disabled while `search` is set? The other options are a `search` parameter on `stream_traces`, or filtering on the client.
4. **Live caps.** The plan does not bound the live buffer or the prepended rows. What limits does the product want?
5. **Saved views (U14 vs spec 11).** Which is authoritative for "share with project" and "set as project default" on an existing view?
6. **Typed durations in the time picker.** `2h` becomes absolute `from`/`to`, and Refresh then does not move the window. Should a typed duration stay relative? Spec 11 lists only the six presets.
7. **JSON numbers above 2^53 in payloads** are rounded by `response.json()` before `JsonView` sees them. Is that acceptable, or should payload JSON be parsed losslessly?
