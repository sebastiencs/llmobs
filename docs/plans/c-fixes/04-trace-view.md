# CF5–CF7 — Trace view

See [README.md](README.md) for the rules that apply to every phase.

---

## CF5 — External media is never loaded without a click (M1)

**Edit only:**
- `web/src/lib/media.ts` and `web/src/lib/media.test.ts`;
- `web/src/components/payload/media-view.tsx` and `web/src/components/payload/media-view.test.tsx`;
- `web/src/components/payload/media-chip.tsx`, plus a new `web/src/components/payload/media-chip.test.tsx`;
- the `Status:` line of `docs/plans/c-fixes/README.md`.

**Commit:** `web: never load media from outside the API without a click`

### Problem

Spec 11 § Quality bar says "No external network calls", and plan U18 says "External URLs are never loaded automatically". Today:
- `isExternal` in `media-view.tsx` is `uri.startsWith("https:") || uri.startsWith("http:")`, which is case-sensitive.
- Every other URI goes through `sourceUrl` → `mediaUrl`.
- `mediaUrl` (`lib/media.ts`) returns anything that is not `media://` or `llmobs:dropped` unchanged.
- The result is put straight into `<img src>` or `<audio src>`.

So `HTTPS://host/x.png`, `//host/x.png`, `ftp://…` and relative URLs all load as soon as the span is opened. `MediaChip` also links any such URI with `target="_blank"`.

### New classification — `web/src/lib/media.ts`

Replace `mediaUrl` with one classifier. Delete `mediaUrl`, and update every caller in the same commit. Today its only callers are `media-view.tsx` and `media-chip.tsx`.

```ts
const MEDIA_PREFIX = "media://";

export type MediaSource =
  /** `llmobs:dropped`: the bytes were dropped at ingest. */
  | { kind: "dropped" }
  /** `media://<sha256>` of this project: served by the API. */
  | { kind: "stored"; url: string }
  /** `data:image/…` or `data:audio/…`: inline bytes, no network. */
  | { kind: "inline"; url: string }
  /** `http:`/`https:` in any case, or protocol-relative `//host/…`: loaded only after a click. */
  | { kind: "external"; url: string }
  /** Anything else (other schemes, relative paths, other `data:` types, or `media://` with no project): never loaded or linked. */
  | { kind: "unsupported" };

export function classifyMedia(projectRef: string | undefined, uri: string): MediaSource {
  if (uri === "llmobs:dropped") return { kind: "dropped" };
  if (uri.startsWith(MEDIA_PREFIX)) {
    if (projectRef === undefined) return { kind: "unsupported" };
    const sha = uri.slice(MEDIA_PREFIX.length);
    return {
      kind: "stored",
      url: `/api/v1/projects/${encodeURIComponent(projectRef)}/media/${encodeURIComponent(sha)}`,
    };
  }
  if (uri.startsWith("//")) return { kind: "external", url: uri };
  const scheme = /^([a-zA-Z][a-zA-Z0-9+.-]*):/.exec(uri)?.[1]?.toLowerCase();
  if (scheme === "http" || scheme === "https") return { kind: "external", url: uri };
  if (scheme === "data" && /^data:(image|audio)\//i.test(uri)) return { kind: "inline", url: uri };
  return { kind: "unsupported" };
}
```

Keep the existing URL shape for `stored`: `/api/v1/projects/<encoded ref>/media/<encoded sha>`. The `stored` row of the new table test pins it.

### `web/src/components/payload/media-view.tsx`

Rewrite `MediaView`'s decision with `classifyMedia`:

1. **`part.type === "blob"`:** unchanged. Build `blobUrl(part)`. Render `ImageMedia` for an image, `AudioControl` for audio, otherwise a `MediaChip` with `uri={blobUrl(part)}`. The chip will not link it (see below). Blobs are bytes from the payload, never network.
2. **`part.type === "uri"`:** `const source = classifyMedia(projectRef, part.uri)`, then:
   - `dropped` → the existing dropped label (`droppedLabel(part.size)`).
   - `external` → the existing `ExternalMedia`, unchanged. Its Load button is the only path to `<img src={external url}>`.
   - `stored` or `inline` → `ImageMedia src={source.url}` for an image, `AudioControl src={source.url}` for audio, else `MediaChip uri={part.uri} …`.
   - `unsupported` → `MediaChip uri={part.uri} mimeType={mime} projectRef={projectRef}`. The chip shows a label and no link.
3. Delete `sourceUrl` and `isExternal`. `classifyMedia` replaces them.

### `web/src/components/payload/media-chip.tsx`

1. Replace `const href = ref ? mediaUrl(ref, uri) : undefined;` with:

   ```ts
   const source = classifyMedia(ref, uri);
   const href = source.kind === "stored" || source.kind === "external" ? source.url : undefined;
   ```

2. `inline`, `dropped` and `unsupported` get no link, so `javascript:`, `data:` and other schemes are never an `href`.
3. `chipLabel` is unchanged.

### Tests

**`web/src/lib/media.test.ts`** — replace the three `mediaUrl` tests with this table test:

```ts
it.each([
  ["llmobs:dropped", "acme/support-bot", { kind: "dropped" }],
  ["media://497790947d466676", "acme/support-bot", { kind: "stored", url: "/api/v1/projects/acme%2Fsupport-bot/media/497790947d466676" }],
  ["media://497790947d466676", undefined, { kind: "unsupported" }],
  ["https://example.com/a.png", "acme/support-bot", { kind: "external", url: "https://example.com/a.png" }],
  ["HTTPS://example.com/a.png", "acme/support-bot", { kind: "external", url: "HTTPS://example.com/a.png" }],
  ["Http://example.com/a.png", "acme/support-bot", { kind: "external", url: "Http://example.com/a.png" }],
  ["//example.com/a.png", "acme/support-bot", { kind: "external", url: "//example.com/a.png" }],
  ["data:image/png;base64,AAAA", "acme/support-bot", { kind: "inline", url: "data:image/png;base64,AAAA" }],
  ["DATA:AUDIO/wav;base64,AAAA", "acme/support-bot", { kind: "inline", url: "DATA:AUDIO/wav;base64,AAAA" }],
  ["data:text/html,<b>x</b>", "acme/support-bot", { kind: "unsupported" }],
  ["javascript:alert(1)", "acme/support-bot", { kind: "unsupported" }],
  ["ftp://example.com/a.png", "acme/support-bot", { kind: "unsupported" }],
  ["gs://bucket/a.png", "acme/support-bot", { kind: "unsupported" }],
  ["a.png", "acme/support-bot", { kind: "unsupported" }],
  ["/a.png", "acme/support-bot", { kind: "unsupported" }],
])("classifies %s", (uri, ref, expected) => {
  expect(classifyMedia(ref, uri)).toEqual(expected);
});
```

**`web/src/components/payload/media-view.test.tsx`** — keep the three existing tests and add:

```ts
it.each(["HTTPS://example.com/a.png", "//example.com/a.png"])(
  "does not load %s until Load is clicked",
  (uri) => {
    render(
      <MediaView part={{ type: "uri", uri, modality: "image", mime_type: "image/png" }} projectRef="acme/bot" />,
    );
    expect(document.querySelector("img")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Load" }));
    expect(document.querySelector("img")).toHaveAttribute("src", uri);
  },
);

it.each(["javascript:alert(1)", "ftp://example.com/a.png", "a.png", "data:text/html,<b>x</b>"])(
  "never loads or links %s",
  (uri) => {
    render(
      <MediaView part={{ type: "uri", uri, modality: "image", mime_type: "image/png" }} projectRef="acme/bot" />,
    );
    expect(document.querySelector("img")).toBeNull();
    expect(screen.queryByRole("button", { name: "Load" })).not.toBeInTheDocument();
    expect(screen.queryByRole("link")).not.toBeInTheDocument();
  },
);

it("renders an inline data image from a uri part", () => {
  render(
    <MediaView
      part={{ type: "uri", uri: "data:image/png;base64,AAAA", modality: "image", mime_type: "image/png" }}
      projectRef="acme/bot"
    />,
  );
  expect(document.querySelector("img")).toHaveAttribute("src", "data:image/png;base64,AAAA");
});

it("renders a stored image through the API", () => {
  render(
    <MediaView
      part={{ type: "uri", uri: "media://497790947d466676", modality: "image", mime_type: "image/png" }}
      projectRef="acme/bot"
    />,
  );
  expect(document.querySelector("img")).toHaveAttribute(
    "src",
    "/api/v1/projects/acme%2Fbot/media/497790947d466676",
  );
});
```

The `part` objects must satisfy `Schema<"UriPart">`. If `tsc` rejects one, compare it with the existing test's part and report.

**`web/src/components/payload/media-chip.test.tsx`** (new):

```ts
import { render, screen } from "@testing-library/react";
import { expect, it } from "vitest";
import { MediaChip } from "@/components/payload/media-chip";

it("links a stored file through the API", () => {
  render(<MediaChip uri="media://497790947d466676" mimeType="application/pdf" projectRef="acme/bot" />);
  expect(screen.getByRole("link")).toHaveAttribute("href", "/api/v1/projects/acme%2Fbot/media/497790947d466676");
});

it("links an external file, opened only by the click", () => {
  render(<MediaChip uri="HTTPS://example.com/a.pdf" mimeType="application/pdf" projectRef="acme/bot" />);
  expect(screen.getByRole("link")).toHaveAttribute("href", "HTTPS://example.com/a.pdf");
});

it.each(["javascript:alert(1)", "data:text/html,<b>x</b>", "data:image/png;base64,AAAA", "ftp://example.com/a"])(
  "does not link %s",
  (uri) => {
    render(<MediaChip uri={uri} mimeType="application/octet-stream" projectRef="acme/bot" />);
    expect(screen.queryByRole("link")).not.toBeInTheDocument();
  },
);
```

`MediaChip` calls `useOptionalProject()`. It returns `null` outside a project route. That is why every test passes `projectRef`. If rendering it outside the router throws, stop and report.

### Docs

None. Spec 11 § Quality bar already requires this; the code now matches it.

### Done when

- `just check-web` is green.
- `mediaUrl` no longer exists. `grep -rn mediaUrl web/src` finds nothing.
- The new tests pass.

---

## CF6 — Transcript waits for the previous LLM call (M11)

**Edit only:**
- `web/src/components/trace/transcript-view.tsx` and `web/src/components/trace/transcript-view.test.tsx`;
- `docs/spec/11-ui.md`;
- the `Status:` line of `docs/plans/c-fixes/README.md`.

**Commit:** `web: load the previous LLM call before trimming a transcript turn`

### Problem

`LlmTurn` shows only the messages not seen in the previous LLM call's input and output (plan U20). It reads that previous call's detail from the query cache with `useQuery({ ...spanDetailQuery(…previousLlmId…), enabled: false })`, which never fetches.

The loader, `useDetailLoader`, prefetches only `visibleIds`: the turns in the virtual window. So when the previous call is outside the window, still loading, or failed, `freshInput(undefined, input)` returns the whole input. The turn shows the full history, then jumps once the previous detail arrives.

### Change — `web/src/components/trace/transcript-view.tsx`

1. **Load the previous call first.** In `TranscriptView`, replace the `visibleIds` computation with:

   ```ts
   const visibleIds = [
     ...new Set(
       virtualizer
         .getVirtualItems()
         .map((item) => rows[item.index])
         .flatMap((row) =>
           row?.kind === "turn"
             ? [...(row.previousLlmId ? [row.previousLlmId] : []), row.item.span.span.span_id]
             : [],
         ),
     ),
   ];
   ```

   - A turn's `previousLlmId` is always an earlier turn below the cap, as `visibleRows` builds it. So the loader still requests at most `cap` distinct spans: the quality bar of at most 50 requests before "Load more" holds.
   - `useDetailLoader` and its concurrency of 4 are unchanged.

2. **Wait for the previous call before rendering an LLM turn.** In `TurnRow`, replace the rendering branch:

   ```tsx
   {loaded ? ( … ) : detail.isError ? ( … ) : ( <Skeleton … /> )}
   ```

   with:

   ```tsx
   {detail.isError ? (
     <p className="text-xs">Could not load this span</p>
   ) : !loaded || (span.span_kind === "llm" && previousLlmId !== undefined && previous.isPending) ? (
     <Skeleton className="h-4 w-2/3" />
   ) : span.span_kind === "tool" ? (
     <ToolTurn detail={loaded} />
   ) : (
     <>
       {previousLlmId !== undefined && previous.isError ? (
         <p className="text-xs text-muted-foreground">Earlier context could not be loaded</p>
       ) : null}
       <LlmTurn
         detail={loaded}
         previous={previousLlmId !== undefined ? previous.data : undefined}
         projectRef={ref}
       />
     </>
   )}
   ```

   - `previous.isPending` is true while the cache has no data and no error for that key. That is the case before the loader has fetched it, and while it is in flight.
   - Keep the `previous` `useQuery` call as it is, including the `previousLlmId ?? span.span_id` key and `enabled: false`.
   - `LlmTurn` is unchanged. When `previous` is `undefined` after an error, it shows the full input, as intended.

### Tests — `web/src/components/trace/transcript-view.test.tsx`

Fixture trace `TRACE_1` (`a1000000000000000000000000000001`), in order:

| Span | Kind | Payload |
|---|---|---|
| `b100000000000011` | agent `support-agent` | — |
| `b100000000000012` | llm | input: the user message "My order 1234 hasn't arrived."; output: a tool call |
| `b100000000000013` | tool `get_order_status` | — |
| `b100000000000014` | llm | input repeats the user message, the tool call and the tool result; output: "Your order 1234 shipped and arrives tomorrow." |

Inside the `describe("transcript", …)` block, add these two tests.

```ts
it("waits for the previous call before showing a turn", async () => {
  let release = () => {};
  const gate = new Promise<void>((resolve) => {
    release = resolve;
  });
  let started = false;
  server.use(
    http.get("/api/v1/projects/:project/traces/:trace_id/spans/:span_id", async ({ params }) => {
      if (params.span_id !== "b100000000000012") return undefined;
      started = true;
      await gate;
      return undefined;
    }),
  );
  await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}&mode=transcript`);
  const transcript = await screen.findByRole("region", { name: "Transcript" });
  await waitFor(() => expect(started).toBe(true));
  // Let the second call's detail arrive while the first is held.
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 50));
  });
  expect(within(transcript).queryByText("My order 1234 hasn't arrived.")).not.toBeInTheDocument();
  expect(
    within(transcript).queryByText("Your order 1234 shipped and arrives tomorrow."),
  ).not.toBeInTheDocument();
  release();
  expect(
    await within(transcript).findByText("Your order 1234 shipped and arrives tomorrow."),
  ).toBeInTheDocument();
  expect(within(transcript).getAllByText("My order 1234 hasn't arrived.")).toHaveLength(1);
});

it("shows the full input when the previous call fails", async () => {
  server.use(
    http.get("/api/v1/projects/:project/traces/:trace_id/spans/:span_id", ({ params }) => {
      if (params.span_id !== "b100000000000012") return undefined;
      return HttpResponse.json(
        { type: "about:blank", title: "Not Found", status: 404, code: "not_found" },
        { status: 404 },
      );
    }),
  );
  await renderApp(`/acme/support-bot/traces?trace=${TRACE_1}&mode=transcript`);
  const transcript = await screen.findByRole("region", { name: "Transcript" });
  expect(await within(transcript).findByText("Earlier context could not be loaded")).toBeInTheDocument();
  expect(within(transcript).getByText("Could not load this span")).toBeInTheDocument();
  expect(within(transcript).getAllByText("My order 1234 hasn't arrived.")).toHaveLength(1);
  expect(
    within(transcript).getByText("Your order 1234 shipped and arrives tomorrow."),
  ).toBeInTheDocument();
});
```

Why the first test fails on today's code: the second turn renders its full input, which includes the user message, while the first call is held. Today the count is 1 during the hold; after the fix it is 0.

Notes on these tests:
- A 404 is used rather than a 500, because a 4xx is never retried.
- A resolver that returns `undefined` falls through to the next matching handler, here the fixture handler. That holds even after an `await`; checked on 2026-11-13.
- Do **not** use `passthrough()` here. It sends the request to the real network, which fails in tests.

The existing `"loads at most 50 spans, four at a time, until Load more"` test must pass unchanged. Its 60 bulk LLM turns each have a `previousLlmId` within the cap.

### Docs

**`docs/spec/11-ui.md` § Trace view, the Transcript sentence.** After "…shows the trace's LLM and tool spans in order as one conversation.", add:

> Each LLM turn shows only the messages not already in the previous LLM call; it waits for that call's detail and, if it cannot be loaded, shows the full input with "Earlier context could not be loaded".

### Done when

- `just check-web` is green.
- The two new tests pass, and the bulk test is unchanged.

---

## CF7 — Tests for tree interactions and split ratios (M12, M13)

**Edit only:**
- `web/src/components/trace/tree-grid.test.tsx`;
- a new `web/src/components/trace/split-pane.test.tsx`;
- the `Status:` line of `docs/plans/c-fixes/README.md`.

**Commit:** `web: test tree keyboard, timeline gestures and split ratios`

This phase adds **tests only**. If a test fails because the behaviour differs from what is described here, stop and report the behaviour. Do not change product code.

### Facts the tests rely on (checked 2026-11-13)

- Trace `TRACE_1` = `a1000000000000000000000000000001`.
  - It lasts 4.20 s. Its time window is exactly 4.20 s, because no span exceeds the trace bounds.
  - Its rows by `aria-rowindex`, with the header at 1: 2 `support-agent` (`b100000000000011`, the root, which has children), 3 `chat claude-sonnet-4-5` (`b100000000000012`), 4 `get_order_status` (`b100000000000013`), 5 `chat claude-sonnet-4-5` (`b100000000000014`).
  - With no `?span=`, the root is selected.
- `TreeGrid` key handling (`tree-grid.tsx`, `onKeyDown`):
  - ↓ and `j` select the next visible row; ↑ and `k` the previous one.
  - → expands a collapsed node that has children.
  - ← collapses an expanded node, else selects the parent.
  - Enter toggles a node with children.
  - `0` resets the zoom (the `fit` binding).
- Selecting a row writes `?span=`.
- The search box ("Search spans", `tree-toolbar.tsx`): Enter goes to the next match and Shift+Enter to the previous one.
- Wheel (a native listener on the grid, so `fireEvent.wheel` works):
  - only when the pointer is over the timeline column header (`timelinePercent` uses that header's `getBoundingClientRect`);
  - Ctrl or ⌘ + wheel zooms;
  - Shift + wheel pans.
- Header drag: `pointerdown`, `pointermove`, `pointerup` on the timeline column header zoom to the dragged range when it is wider than 1 %.
- Needle: `pointermove` over the grid shows a label with the offset from the trace start, `formatDuration`. `pointerleave` hides it.
- jsdom gives every element a zero rect, except the stubs in `src/test/layout.ts`. So each gesture test must stub the timeline header's rect:

  ```ts
  vi.spyOn(timeline, "getBoundingClientRect").mockReturnValue(new DOMRect(400, 0, 400, 28));
  ```

  `restoreMocks: true` in `vite.config.ts` restores it after each test. A pointer at `clientX: 600` is then at 50 % of the timeline.
- Split panes (`components/trace/split-pane.tsx`) render each handle as `role="separator"`. Its `aria-controls` is the start panel id (`traces-list` for the list split, `trace-tree` for the tree split), and its `aria-valuenow` is that panel's size in percent. The defaults are 40 and 55. Stored values live in `llmobs.split.traces` and `llmobs.split.trace`, as `{"<start id>": n, "<end id>": 100 - n}`. A value that is not a number strictly between 0 and 100 falls back to the default.
- Keyboard resizing of a split does nothing in jsdom (checked: `aria-valuenow` stays put). So the **write** path, `onLayoutChanged` → `localStorage`, cannot be tested here. Do not try. Report it as untested.

### Tests to add to `web/src/components/trace/tree-grid.test.tsx`

Use the existing `dataRows(tree)` helper. Add `fireEvent` to the `@testing-library/react` import, and `vi` to the `vitest` import.

1. **`"moves, expands and collapses with the arrow keys and Enter"`**
   1. Render `/acme/support-bot/traces?trace=${TRACE_1}`; `tree = await screen.findByRole("grid", { name: "Trace" })`; `tree.focus()`.
   2. Wait for the selected row (`within(tree).getByRole("row", { selected: true })`) to have `aria-rowindex` `"2"`.
      - After **every** key below, `await waitFor(…)` for that step's expected state before pressing the next key.
      - The grid derives its position from `?span=`, which arrives through the router, so a key pressed too early would start from the old position.
   3. `fireEvent.keyDown(tree, { key: "ArrowDown" })` → the selected row's `aria-rowindex` becomes `"3"` (use `waitFor`), and `router.state.location.search.span` is `"b100000000000012"`.
   4. `ArrowUp` → `"2"`.
   5. `ArrowLeft` on the expanded root → `dataRows(tree)` has length 1.
   6. `ArrowRight` → length 4.
   7. `Enter` → length 1; `Enter` again → length 4.
   8. `ArrowDown` twice → `"4"` (`get_order_status`, a leaf).
   9. `ArrowLeft` on the leaf selects its parent → `"2"`, and `span` is `"b100000000000011"`.
2. **`"fits the timeline with 0"`**
   1. Render as above.
   2. `timeline = within(tree).getByRole("columnheader", { name: "Timeline" })`; `before = timeline.textContent`.
   3. `await user.dblClick(screen.getByLabelText("Timeline get_order_status"))`. Then `waitFor` until the text differs from `before`. This is the same zoom as the existing test `"zooms to a span and fits the trace again"`.
   4. `fireEvent.keyDown(tree, { key: "0" })` → `waitFor` until `timeline.textContent` equals `before`.
3. **`"goes to the previous match with Shift+Enter"`**
   1. As in the existing `"moves the selection to the next match"`: type `chat` in the search box, then `{Enter}` twice, which ends on `aria-rowindex` `"5"` and `"2 / 2"`.
   2. `await user.keyboard("{Shift>}{Enter}{/Shift}")` → the selected row is `"3"`, and the text `"1 / 2"` is shown.
4. **`"zooms with ctrl+wheel and pans with shift+wheel over the timeline"`**
   1. Render, get `timeline`, and stub its rect as above; `before = timeline.textContent`.
   2. `fireEvent.wheel(tree, { clientX: 100, deltaY: -100, ctrlKey: true })`. The pointer is outside the timeline, so `timeline.textContent` is still `before`.
   3. `fireEvent.wheel(tree, { clientX: 600, deltaY: -100, ctrlKey: true })` → `expect(await screen.findByText("350ms–3.85s")).toBeInTheDocument()`.
      - This is the toolbar's zoom label, `formatDuration(start)–formatDuration(end)` relative to the trace start. Before any zoom it reads `0ms–4.20s`.
   4. `fireEvent.wheel(tree, { clientX: 600, deltaY: 100, shiftKey: true })` → `expect(await screen.findByText("700ms–4.20s")).toBeInTheDocument()`.
      - Do not assert on the timeline header's text here. Its tick labels are offsets from the window start, so a pan, which keeps the width, does not change them.
      - Both label values were checked with a probe on 2026-11-13.
5. **`"zooms to a range dragged on the timeline header"`**
   1. Render, get `timeline`, and stub its rect; `before = timeline.textContent`.
   2. `fireEvent.pointerDown(timeline, { clientX: 500, button: 0, pointerId: 1 })`, `fireEvent.pointerMove(timeline, { clientX: 600, pointerId: 1 })`, `fireEvent.pointerUp(timeline, { clientX: 600, pointerId: 1 })`.
   3. `waitFor` until the text differs from `before`.
6. **`"shows the offset under the pointer and hides it on leave"`**
   1. Render, get `timeline`, and stub its rect.
   2. `fireEvent.pointerMove(tree, { clientX: 600 })` → `screen.getByText("2.10s")` is in the document. That is 50 % of 4.20 s, as `formatDuration` writes it.
   3. `fireEvent.pointerLeave(tree)` → `screen.queryByText("2.10s")` is null.
   4. If `2.10s` also appears elsewhere on the page, for example as a duration cell, scope the query to the needle. The needle is the `span` inside the absolutely positioned `div` that is a direct child of the grid. If you cannot scope it without changing product code, report it.

### `web/src/components/trace/split-pane.test.tsx` (new)

```ts
import { screen } from "@testing-library/react";
import { expect, it } from "vitest";
import { renderApp } from "@/test/render";

const URL_WITH_TRACE = "/acme/support-bot/traces?range=1h&trace=a1000000000000000000000000000001";

function size(startPanelId: string): string | null {
  return (
    document
      .querySelector(`[role="separator"][aria-controls="${startPanelId}"]`)
      ?.getAttribute("aria-valuenow") ?? null
  );
}

it("opens both splits at their default ratios", async () => {
  await renderApp(URL_WITH_TRACE);
  await screen.findByRole("grid", { name: "Trace" });
  expect(size("traces-list")).toBe("40");
  expect(size("trace-tree")).toBe("55");
});

it("restores stored ratios", async () => {
  localStorage.setItem("llmobs.split.traces", JSON.stringify({ "traces-list": 30, "traces-detail": 70 }));
  localStorage.setItem("llmobs.split.trace", JSON.stringify({ "trace-tree": 65, "trace-detail": 35 }));
  await renderApp(URL_WITH_TRACE);
  await screen.findByRole("grid", { name: "Trace" });
  expect(size("traces-list")).toBe("30");
  expect(size("trace-tree")).toBe("65");
});

it.each([
  ["not json", "{"],
  ["out of range", JSON.stringify({ "traces-list": 0, "traces-detail": 100 })],
  ["wrong keys", JSON.stringify({ list: 30, detail: 70 })],
])("falls back to the default for a stored value that is %s", async (_label, raw) => {
  localStorage.setItem("llmobs.split.traces", raw);
  await renderApp(URL_WITH_TRACE);
  await screen.findByRole("grid", { name: "Trace" });
  expect(size("traces-list")).toBe("40");
});
```

`localStorage` is cleared after each test by `src/test/setup.ts`.

### Docs

None.

### Done when

- `just check-web` is green.
- 6 new tree tests pass, plus 3 split tests, the last of which runs 3 cases.
- No product file changed.
