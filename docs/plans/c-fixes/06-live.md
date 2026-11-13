# CF9–CF11 — Live mode

See [README.md](README.md) for the rules that apply to every phase.

---

## CF9 — Live mode caps (M10)

**Edit only:**
- `web/src/components/data-table/live-list.ts` and `web/src/components/data-table/live-list.test.ts`;
- `web/src/components/data-table/use-live-traces.ts`;
- `web/src/routes/$org/$project/traces/index.tsx` and `web/src/routes/$org/$project/traces/index.test.tsx`;
- `docs/spec/11-ui.md`;
- `docs/mvp-decisions.md`;
- the `Status:` line of `docs/plans/c-fixes/README.md`.

**Commit:** `web: bound the live traces buffer and first page`

### Problem

In `use-live-traces.ts`:
- **The paused buffer grows without limit.** It holds the traces that arrive while the list is scrolled away from the top: `writeBuffer([trace, ...bufferRef.current])`.
- **Page 1 grows without limit.** `prependLiveTraces` in `live-list.ts` adds every new trace to the first page.
- **`timers.current` keeps every highlight timer id forever.**

On a busy project, a tab left open grows without bound.

### Rule (D-C-4)

- **The paused buffer keeps at most 500 traces, the newest.** When a trace is dropped, the buffer is marked overflowed and the pill reads `500+ new traces ↑` instead of `N new traces ↑`.
  - Revealing an overflowed buffer, by the pill or by scrolling to the top, does **not** insert the 500. It refetches page 1, as `resync` does: drop pages after the first, then invalidate. It also scrolls to the top.
- **Page 1 never grows past 500 rows from live inserts.** When inserting new traces would make page 1 longer than 500 rows, page 1 is refetched instead. That covers both the at-top path and a non-overflowed reveal.
  - The refetched page 1 comes from the server with a correct cursor, so older rows come back by scrolling.
  - Trimming page 1 in place would break the cursor of page 2, so it is not done.
- The new traces are still highlighted. `mark(ids)` is still called with their ids, and they show the highlight if they are in the refetched page.
- A highlight timer removes its own id from `timers.current` when it fires.

### Changes

1. **`web/src/components/data-table/live-list.ts`:** add

   ```ts
   export const LIVE_BUFFER_MAX = 500;
   export const LIVE_PAGE_MAX = 500;

   /** Newest first. Keeps at most `max` traces; `overflowed` is true when one was dropped. */
   export function pushBuffered(
     buffer: readonly Schema<"TraceSummary">[],
     trace: Schema<"TraceSummary">,
     max: number,
   ): { buffer: Schema<"TraceSummary">[]; overflowed: boolean } {
     const next = [trace, ...buffer];
     return next.length > max
       ? { buffer: next.slice(0, max), overflowed: true }
       : { buffer: next, overflowed: false };
   }

   /** True when adding `adding` rows to the first page would make it longer than `max`. */
   export function firstPageWouldExceed(data: TracePages, adding: number, max: number): boolean {
     return (data.pages[0]?.data.length ?? 0) + adding > max;
   }
   ```

2. **`web/src/components/data-table/use-live-traces.ts`:**

   a. **Test override.** Follow the `setOnboardingPollMs` pattern in `components/onboarding/poll.ts`. At module level:

   ```ts
   let limits = { buffer: LIVE_BUFFER_MAX, page: LIVE_PAGE_MAX };

   /** Tests only: lower the live caps. Restore with `setLiveLimits({ buffer: LIVE_BUFFER_MAX, page: LIVE_PAGE_MAX })`. */
   export function setLiveLimits(next: { buffer: number; page: number }): void {
     limits = next;
   }
   ```

   b. **Overflow state.** Add `const overflowRef = useRef(false);` and `const [overflowed, setOverflowed] = useState(false);`. Add `overflowed: boolean` to the hook's return type and return value.

   c. **One refetch helper.** Move the body of the `resync` branch into a function inside the hook, and call it from that branch:

   ```ts
   function refetchFirstPage() {
     const key = queryKeyRef.current;
     queryClient.setQueryData<TracePages>(key, (current) => (current ? firstPageOnly(current) : current));
     void queryClient.invalidateQueries({ queryKey: key, exact: true });
   }
   ```

   d. **Buffering, in `onEvent`.** Replace the `if (!atTop) { writeBuffer([trace, ...bufferRef.current]); return; }` branch with:

   ```ts
   if (!atTop) {
     const pushed = pushBuffered(bufferRef.current, trace, limits.buffer);
     if (pushed.overflowed && !overflowRef.current) {
       overflowRef.current = true;
       setOverflowed(true);
     }
     writeBuffer(pushed.buffer);
     return;
   }
   ```

   e. **At-top insert, in `onEvent`.** Before the `queryClient.setQueryData(… prependLiveTraces(data, [trace]) …)` call, and after the existing `belongsAtTop` check, add:

   ```ts
   if (firstPageWouldExceed(current, 1, limits.page)) {
     refetchFirstPage();
     mark([trace.trace_id]);
     return;
   }
   ```

   `current` is already known to be defined there, because the preceding guard returns when it is not.

   f. **`reveal()`.** After `writeBuffer([]); pinTop.current = true; scrollAdjust.current = 0;`:

   ```ts
   if (overflowRef.current) {
     overflowRef.current = false;
     setOverflowed(false);
     refetchFirstPage();
     mark(rows.map((trace) => trace.trace_id));
     return;
   }
   insertBuffered(rows);
   ```

   This replaces the plain `insertBuffered(rows)` call.

   g. **`insertBuffered`.** After `const unseen = rows.filter(…)`, add:

   ```ts
   if (firstPageWouldExceed(current, unseen.length, limits.page)) {
     refetchFirstPage();
     mark(unseen.map((trace) => trace.trace_id));
     return;
   }
   ```

   h. **Timer pruning in `mark`.** Inside the `setTimeout` callback, after the `setHighlighted` call, add:

   ```ts
   timers.current = timers.current.filter((id) => id !== timer);
   ```

   `timer` is the `const` the callback closes over.

3. **`web/src/routes/$org/$project/traces/index.tsx`.** In the pill, `{live.pending} new traces ↑` becomes:

   ```tsx
   {live.overflowed ? `${live.pending}+` : live.pending} new traces ↑
   ```

   The accessible name stays `"<n> new traces ↑"` or `"<n>+ new traces ↑"`.

### Tests

**`web/src/components/data-table/live-list.test.ts`** — add:
- `pushBuffered`:
  - pushing onto a buffer of length 1 with `max` 2 → length 2, the new trace first, `overflowed: false`;
  - pushing onto a buffer of length 2 with `max` 2 → length 2, the new trace first, the oldest dropped, `overflowed: true`.
- `firstPageWouldExceed`:
  - a first page of 5 rows with `max` 5: adding 0 → `false`, adding 1 → `true`;
  - an empty `pages` array with `max` 0: adding 1 → `true`.

  Build the `TracePages` the way the file's existing tests do.

**`web/src/routes/$org/$project/traces/index.test.tsx`** — add two tests. They use the file's existing helpers `openLive`, `liveTrace`, `tracesScroller`, `LATER` and `LATER_STILL`. Import `setLiveLimits` from `@/components/data-table/use-live-traces`, and `LIVE_BUFFER_MAX` and `LIVE_PAGE_MAX` from `@/components/data-table/live-list`. Always restore the defaults in `finally`.

```ts
it("caps the paused buffer and refetches the first page on reveal", async () => {
  setLiveLimits({ buffer: 2, page: LIVE_PAGE_MAX });
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/traces")) urls.push(url);
  };
  server.events.on("request:start", onStart);
  try {
    const { emit } = await openLive("/acme/support-bot/traces?live=1");
    const scroller = tracesScroller();
    scroller.scrollTop = 400;
    fireEvent.scroll(scroller);
    emit.trace(liveTrace("c1000000000000000000000000000011", "first over", LATER));
    emit.trace(liveTrace("c1000000000000000000000000000012", "second over", LATER));
    emit.trace(liveTrace("c1000000000000000000000000000013", "third over", LATER_STILL));
    const pill = await screen.findByRole("button", { name: "2+ new traces ↑" });
    const before = urls.length;
    fireEvent.click(pill);
    await waitFor(() => expect(urls.length).toBeGreaterThan(before));
    expect(scroller.scrollTop).toBe(0);
    expect(screen.queryByRole("button", { name: /new traces/ })).not.toBeInTheDocument();
    expect(await screen.findByText("5 loaded")).toBeInTheDocument();
  } finally {
    server.events.removeListener("request:start", onStart);
    setLiveLimits({ buffer: LIVE_BUFFER_MAX, page: LIVE_PAGE_MAX });
  }
});

it("refetches the first page instead of growing it past the cap", async () => {
  setLiveLimits({ buffer: LIVE_BUFFER_MAX, page: 5 });
  const urls: URL[] = [];
  const onStart = ({ request }: { request: Request }) => {
    const url = new URL(request.url);
    if (url.pathname.endsWith("/traces")) urls.push(url);
  };
  server.events.on("request:start", onStart);
  try {
    const { emit } = await openLive("/acme/support-bot/traces?live=1");
    expect(screen.getByText("5 loaded")).toBeInTheDocument();
    const before = urls.length;
    emit.trace(liveTrace("c1000000000000000000000000000014", "over the cap", LATER));
    await waitFor(() => expect(urls.length).toBeGreaterThan(before));
    await waitFor(() => expect(screen.getByText("5 loaded")).toBeInTheDocument());
    expect(screen.queryByText("6 loaded")).not.toBeInTheDocument();
  } finally {
    server.events.removeListener("request:start", onStart);
    setLiveLimits({ buffer: LIVE_BUFFER_MAX, page: LIVE_PAGE_MAX });
  }
});
```

The fixture list returns 5 traces ("5 loaded"), and the mock ignores everything the live traces add. So after a refetch the count is 5 again.

The existing live tests must pass unchanged. That includes "prepends a new trace and highlights the row" ("6 loaded") and "buffers traces … from the pill" ("2 new traces ↑"). Both run under the default caps.

### Docs

**`docs/spec/11-ui.md` § Traces (list), the Live mode bullet.** After "Scrolling away from the top buffers arrivals behind a "N new traces" pill.", insert:

> The buffer keeps the newest 500; past that the pill reads "500+ new traces" and revealing it refetches the first page. A live insert that would grow the first page past 500 rows refetches it instead (D-C-4).

**`docs/mvp-decisions.md`, under `## Track C — Web UI`.** Add:

```markdown
| D-C-4 | Live caps | **Live mode keeps memory bounded.** The paused buffer keeps the newest 500 traces; once one is dropped the pill reads "500+ new traces" and revealing refetches page 1 (drop later pages, invalidate) instead of inserting. A live insert that would make page 1 longer than 500 rows refetches page 1 instead; trimming it in place would break page 2's cursor. |
```

### Done when

- `just check-web` is green.
- The new unit tests and the two page tests pass.
- The existing live tests are unchanged.

---

## CF10 — `search` on the trace stream: contract (M9, part 1)

**Edit only:**
- `crates/server/src/api/traces.rs`;
- `crates/server/openapi.json` and `web/src/api/schema.gen.ts`, both regenerated, never edited by hand;
- `docs/spec/09-api.md`, `docs/spec/05-query.md`, `docs/mvp-decisions.md`;
- the `Status:` line of `docs/plans/c-fixes/README.md`.

**Commit:** `api: accept search on the trace stream`

### Why

The live traces list ignores the free-text `search`, so traces that do not match the search are prepended to a searched list. The user chose, on 2026-11-13, to add `search` to `GET /traces/stream`.

This phase changes the **contract only**. The stream handler does not exist yet: track G owns SSE and implements the matching to this contract. Add no handler, route or server logic.

### Changes

1. **`crates/server/src/api/traces.rs`, `pub struct TraceStreamParams`.** Add a field after `filter`:

   ```rust
       /// Full-text search (spec 05 § Full-text search). A trace is sent once any of its spans
       /// matches, then on every later update while the stream stays open.
       #[serde(default)]
       pub search: Option<String>,
   ```

   - The doc comment becomes the OpenAPI parameter description (utoipa `IntoParams`), as for the other fields.
   - `TraceListParams.search` has the doc `/// Full-text search (spec 05 § Full-text search).`. Keep the same first sentence.

2. **Regenerate** with `just openapi`. It rewrites `crates/server/openapi.json` (through `LLMOBS_OPENAPI_WRITE=1 cargo test -p llmobs-server --test openapi -- contract_file_is_up_to_date`) and `web/src/api/schema.gen.ts`.
   - Check the diff. Only the new `search` query parameter of `stream_traces` may change in both files.
   - If anything else changes, stop and report.

3. **Do not touch `fixtures/api/`.** `stream_traces` has no JSON example (`crates/server/tests/api_examples.rs` excludes it).

### Docs

**`docs/spec/09-api.md` § Observability data.** In the row for `` `GET /traces/stream` ``, replace the Notes cell with:

> **SSE**: new/updated trace summaries for the list (respects `filter` on light columns, and `search`: a trace is sent once any of its spans matches, then on every update while the stream stays open; 05 § Full-text search; D-C-5)

**`docs/spec/05-query.md` § Full-text search.** Insert this paragraph just before the line "Later (post-MVP): Tantivy index per partition built at compaction.":

> **On the live trace stream** (`GET /traces/stream`, 09), `search` is checked against each arriving span, on the same columns with the same escaping as the `spans`-view search. The stream sends a trace from its first matching span on, and every later update of that trace while the stream stays open. Spans that arrived before the stream opened are not checked; the client's refetch on `resync` covers them. There is no time window (D-C-5).

**`docs/mvp-decisions.md`, under `## Track C — Web UI`.** Add:

```markdown
| D-C-5 | Live search | **`GET /traces/stream` takes `search`** (05 § Full-text search). The stream checks each arriving span against the search columns of the `spans` view and sends its trace from the first match on; later updates of a matched trace are sent while the stream stays open. Spans that arrived before the stream opened are not checked (the client refetches on `resync`). Track G implements the matching with the stream handler. |
```

### Done when

- `just check` is green: Rust, web, the Python SDK and the TypeScript SDK. The contract test `contract_file_is_up_to_date` passes without `LLMOBS_OPENAPI_WRITE`, and `pnpm --dir web check:api` passes.
- Only the listed files changed.

---

## CF11 — The live list sends `search` (M9, part 2)

Needs CF10, because CF11 uses the regenerated `schema.gen.ts`.

**Edit only:**
- `web/src/lib/sse.ts` and `web/src/lib/sse.test.ts`;
- `web/src/routes/$org/$project/traces/index.tsx` and `web/src/routes/$org/$project/traces/index.test.tsx`;
- `docs/spec/11-ui.md`;
- `docs/roadmap.md`;
- the `Status:` line of `docs/plans/c-fixes/README.md`.

**Commit:** `web: send the search to the live trace stream`

### Changes

1. **`web/src/lib/sse.ts`, `traceStreamUrl`.** Add `search?: string` to the `query` parameter type. After the `filter` line, add:

   ```ts
   if (query.search) {
     params.set("search", query.search);
   }
   ```

   Keep the order filter, search, include_eval, include_experiments.

2. **`web/src/routes/$org/$project/traces/index.tsx`.** In the `traceStreamUrl(projectRef(org, project), { … })` call, add `search: search.search,` after `filter: search.filter,`.
   - When the search changes, the URL changes. `useStream` already closes the old stream and opens a new one when its `url` changes. The list query also refetches, because `search` is in its key.

3. **Do not change `traceLiveReason`.** Live stays available while searching.

4. **Leave the live mock alone.** The `?mock=large` mock (`liveTraceSource` in `web/src/mocks/generated/handlers.ts`) ignores the stream's query, as it ignores `filter`.

### Tests

**`web/src/lib/sse.test.ts`.** Find the existing test of `traceStreamUrl`. It is the one that builds a URL with `filter` and the `include_*` flags.
- `traceStreamUrl` returns a path, and the existing test compares exact strings. Add:

  ```ts
  it("adds search to the trace stream url", () => {
    expect(
      traceStreamUrl("acme/support-bot", { filter: "status = 'error'", search: "refund policy" }),
    ).toBe(
      "/api/v1/projects/acme%2Fsupport-bot/traces/stream?filter=status+%3D+%27error%27&search=refund+policy",
    );
    expect(traceStreamUrl("acme/support-bot", { search: "" })).toBe(
      "/api/v1/projects/acme%2Fsupport-bot/traces/stream",
    );
    expect(traceStreamUrl("acme/support-bot", { search: undefined })).toBe(
      "/api/v1/projects/acme%2Fsupport-bot/traces/stream",
    );
  });
  ```

**`web/src/routes/$org/$project/traces/index.test.tsx`.** In the existing test `"opens the trace stream with the list filter"`:
1. Add `&search=refund` to the rendered URL.
2. Add `expect(streams[0]?.searchParams.get("search")).toBe("refund");`.
3. Rename the test to `"opens the trace stream with the list filter and search"`.

### Docs

**`docs/spec/11-ui.md` § Traces (list), the Live mode bullet.** After "subscribes to `/traces/stream` when the range is a preset and the sort is `start_time` descending.", insert:

> The stream gets the list's `filter`, `search`, `include_eval` and `include_experiments` (D-C-5).

**`docs/roadmap.md`, the track C row:**
- In the **Status** cell, replace the text with exactly:

  ```markdown
  **Implemented** on mocks. Reviewed (`docs/reviews/c-web-ui.md`); the review's major findings are fixed by [`plans/c-fixes/`](plans/c-fixes/) (CF1–CF11). Wiring to the real API is track G.
  ```
- In the **Decisions** cell, change `D123–D134` to `D123–D134, D-C-1–D-C-5`.

### Done when

- `just check-web` is green.
- The updated tests pass.
- `docs/plans/c-fixes/README.md` says `Status: CF1–CF11 done on <date>`.
