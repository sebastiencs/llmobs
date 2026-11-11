# C · Live mode and closeout (U26–U29)

See [README.md](README.md) for the decisions, boundaries, dependency versions and the rules that apply to every sub-phase.

Live mode follows spec 09 § SSE and D130:
- `text/event-stream`;
- events `span`, `trace`, `resync` and `ping` (every 15 s);
- `span` carries a `SpanSummary` and `trace` a `TraceSummary`;
- `resync` and `ping` carry `{}`;
- auth by cookie;
- at most 100 streams per user;
- the trace stream closes after 10 min idle.

---

## U26 — The SSE client and its mocks

**Edit only:** `web/src/lib/sse.ts` (+ test), `web/src/mocks/sse.ts` (new), `web/src/mocks/generated/handlers.ts`, `web/src/mocks/browser.ts`, plus tests.

**Commit:** `web: server-sent events client and mocks`

### `lib/sse.ts`

```ts
type StreamEvent =
  | { type: "span"; span: Schema<"SpanSummary"> }
  | { type: "trace"; trace: Schema<"TraceSummary"> }
  | { type: "resync" };
function openStream(url: string, onEvent: (e: StreamEvent) => void, onStatus: (s: "connecting" | "open" | "retrying" | "closed") => void): () => void;
function useStream(url: string | undefined, onEvent: (e: StreamEvent) => void): { status: …; };
```

- `openStream` uses `EventSource` with `withCredentials: false`: same origin, so cookies are sent anyway.
- It listens for `span`, `trace`, `resync` and `ping`. `ping` only marks the stream alive.
- It parses the JSON. A malformed event is dropped and counted; nothing is logged with its content.
- **Reconnects** use exponential backoff: 1 s, 2 s, 4 s … capped at 30 s. After each reconnect it emits a synthetic `resync`, because events may have been missed. A `ping` gap above 45 s counts as a dead connection: close and reconnect.
- The returned function closes the stream. `useStream` opens on mount and when `url` changes, and closes on unmount or `url === undefined`.
- The URL is built with the query params of the list (`filter`, `include_eval`, `include_experiments`), the same as the list request minus paging and time.

### Mocks (`mocks/sse.ts`)

- `traceStreamHandler(source)` and `spanStreamHandler(source)` use MSW's `sse()` (msw 3.0.3). `source` is a function that yields events on a timer.
- **The fixture mode** sends only a `ping` every 15 s: the fixture project is static.
- **The large mode** (`?mock=large`) is a generator in `generated/handlers.ts`:
  - it emits a new `TraceSummary` every 2 s (new IDs, the start time now), then a `trace` update for the same ID 1 s later with an increased `span_count` and `end_time`, so updates are visible;
  - on `/traces/{id}/stream` for a recent trace, it emits 3 new child spans of the root 1 s apart.

  These new traces and spans are also added to the dataset, so a later `get_trace` or list request returns them.
- Tests drive the events manually: the handlers take an injected source that the test pushes to.

**Tests:**
- in node, against the MSW `server` with `sse()` handlers. If MSW's node interceptor doesn't support `EventSource` in jsdom, provide a minimal `EventSource` polyfill in `test/` that uses `fetch` + `ReadableStream` (no dependency), and say so in the report;
- events are parsed and dispatched by type;
- a malformed event is dropped;
- a server close leads to `retrying`, then `open`, plus a synthetic `resync`;
- the ping-timeout reconnect (fake timers);
- `useStream` closes on unmount.

---

## U27 — Live traces list

**Edit only:** `web/src/routes/$org/$project/traces/index.tsx`, `web/src/components/list-toolbar.tsx`, `web/src/components/data-table/**`, `web/src/components/live-toggle.tsx` (new), `web/src/lib/search/traces.ts`, plus tests.

**Commit:** `web: live mode on the traces list`

- **A "Live" toggle** in the toolbar (`live=1`). It's enabled only with a preset range: with an absolute range it's disabled, with a tooltip "Live needs a relative time range". Turning it on re-anchors the range to now.
- **While live:**
  - the list subscribes to `/traces/stream` with the list's `filter`, `include_eval` and `include_experiments`;
  - a `trace` event for an **unknown** ID is prepended to the first page in the query cache (`queryClient.setQueryData`) when it matches the current sort. Live is only offered with `sort=start_time desc`; other sorts disable it with a tooltip. The row slides in with a 1.5 s highlight (CSS animation; `prefers-reduced-motion` disables it);
  - a `trace` event for a **known** ID replaces that row in place;
  - `resync` refetches the list from the first page;
  - a status dot next to the toggle shows connecting, open or retrying.
- **Paused on scroll:**
  - when the list's scroll offset is not at the top, new rows are buffered instead of inserted, and a pill "N new traces ↑" appears at the top;
  - clicking it, or scrolling to the top, inserts the buffer and scrolls to the top;
  - the selected row and the open trace never move under the user's pointer: inserting above them keeps their scroll position (adjust `scrollTop` by the inserted height).
- **Turning live off** closes the stream. Leaving the page closes it too.

**Tests:**
- `live=1` opens the stream with the filter params;
- a pushed `trace` event prepends a row with the highlight class;
- an update replaces in place;
- when scrolled down, events are buffered and the pill shows the count, and clicking it inserts them;
- `resync` refetches;
- live is disabled with an absolute range and with `sort=cost_total`.

---

## U28 — Live trace view

**Edit only:** `web/src/components/trace/{trace-view,tree-grid,trace-header}.tsx`, `web/src/lib/tree.ts` (+ test), plus tests.

**Commit:** `web: live updates in the trace view`

- **When the stream opens:** the trace is "recent" when its `end_time` is within the last 10 minutes, or when the list is in live mode. In those cases `TraceView` subscribes to `/traces/{traceId}/stream`. A "Live" badge in the trace header shows the connection status, and a "Stop" button closes it.
- **`span` events** are inserted into the cached `TraceDetail.spans` in pre-order. `lib/tree.ts` gains `insertSpan(spans, summary): TreeSpan[]`:
  - It finds the parent by `parent_span_id`.
  - It inserts after the parent's last descendant whose start is ≤ the new span's start (sibling order by `(start_time, span_id)`), with `depth = parent.depth + 1`.
  - A span with no known parent is appended as a root with `detached: true`.
  - **Reconciliation.** When a later span arrives whose parent was detached-appended earlier, nothing moves: the next `resync` or a manual refresh re-fetches the tree, which re-parents correctly.
  - A duplicate `span_id` replaces the row, so updates of a span in progress are allowed.
  - It's O(n) per insert; fine for live rates.
- **Trace totals in the header** are updated from `trace` events when present, or recomputed from the spans for duration and count.
- The **timeline window** grows when new spans extend it, unless the user has zoomed: then it stays.
- **New rows flash** like U27, respecting reduced motion.
- **`resync`** refetches `get_trace`.

**Tests:**
- `insertSpan`: a child into the right place, a sibling ordering tie on `span_id`, an unknown parent appended detached, a duplicate replaced, 10 000-span input stays correct;
- page: a pushed `span` event shows the new row under its parent; the header span count increases; `resync` refetches; Stop closes the stream; an old trace (`end_time` far in the past, not live) opens no stream.

---

## U29 — Quality bar and closeout

**Edit only:** files under `web/src/` that the checks below require (fixes only, no new features), `docs/spec/11-ui.md` (only to correct drift found here), `docs/plans/c-web-ui/README.md` (status line).

**Commit:** `web: quality bar checks and spec sync`

1. **Large-data tests in one place:** `web/src/test/quality-bar.test.tsx`. With `largeHandlers`:
   - **the traces list** with 5 000 traces: fewer than 60 rendered rows after scrolling to the middle;
   - **the trace view** with the 10 000-span trace, fully expanded with "Expand all": fewer than 80 rendered tree rows;
   - **Transcript** on a trace with 200 LLM spans: at most 50 detail requests before "Load more";
   - **the JSON viewer** with a 20 000-key object: fewer than 100 rendered rows.

   These replace no earlier tests; they gather the bar in one file (D128).
2. **Keyboard pass.** Every page is usable with the keyboard alone: lists, open a trace, move in the tree, switch tabs, close, the palette, the time picker, the filter bar. Fix the gaps found. Add a test per page that tabs through the main controls and checks there's a visible focus ring (`:focus-visible` style present on the focused element).
3. **Colour pass.** Status is never colour-only: dot plus text or icon, everywhere `StatusDot` and error states appear. Check the light and dark tokens for contrast against the background (WCAG AA for text; compute the ratio from the oklch tokens in a unit test of the token values, or check by hand and list the ratios in the report).
4. **Bundle.** Run `vite build` and record:
   - the entry chunk size;
   - the size of the chunks for the traces route, the trace view and the dashboard;
   - that `recharts` and `@codemirror/*` are not in the entry chunk.

   `check-dist` must still pass, with no `http(s)://` URLs in `index.html` and no mocks.
5. **Spec sync.** Compare spec 11 with what was built. Fix spec wording where the plan's decisions changed details: for example, a param name or a default ratio. Never fix it to describe missing features.
6. **README status:** add "Status: U1–U29 done on <date>" at the top of this plan's README.

**Report:** the bundle sizes, the contrast ratios (or the list of tokens checked), keyboard gaps fixed, and spec edits.
