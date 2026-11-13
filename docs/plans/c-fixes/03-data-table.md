# CF4 — Data table: header scroll and keyboard origin (M6, M7)

See [README.md](README.md) for the rules that apply to every phase.

**Edit only:**
- `web/src/components/data-table/data-table.tsx`;
- `web/src/routes/$org/$project/traces/index.test.tsx`;
- the `Status:` line of `docs/plans/c-fixes/README.md`.

**Commit:** `web: scroll the table header with its rows and start keys from the open row`

`DataTable` is shared by the traces, spans, sessions and users lists. The fix applies to all of them. The tests use the traces list.

## M6 — the header does not scroll horizontally

### Problem

In `DataTable`, the header row (`role="row"`, `aria-rowindex={1}`) is a sibling **above** the scroll container (`<div ref={scrollRef} data-scroll-container className="relative min-h-0 flex-1 overflow-auto">`).

When the columns are wider than the pane, the body scrolls horizontally and the header does not. This always happens with the split open. It was reproduced on 2026-11-13: the body's `scrollLeft` was 300, the header had not moved, and the columns no longer lined up.

### Change

1. **Add a ref and the total width.** Next to `scrollRef`, add:

   ```ts
   const headerRef = useRef<HTMLDivElement>(null);
   ```

   Where `template` is computed from `headerGroup`, also compute:

   ```ts
   const totalWidth = headerGroup
     ? headerGroup.headers.reduce((sum, header) => sum + header.getSize(), 0)
     : undefined;
   ```

2. **Wrap the header row in a non-scrolling clip.** Change the `{headerGroup ? (<div role="row" …>…</div>) : null}` block to:

   ```tsx
   {headerGroup ? (
     <div ref={headerRef} className="shrink-0 overflow-hidden [scrollbar-gutter:stable]">
       <div
         role="row"
         aria-rowindex={1}
         className="grid h-row items-center border-b bg-background text-[11px] font-medium text-muted-foreground uppercase"
         style={{ gridTemplateColumns: template, minWidth: totalWidth }}
       >
         {/* unchanged children */}
       </div>
     </div>
   ) : null}
   ```

   - Move `shrink-0` from the row's class list to the wrapper. Keep every other class of the row as it is.
   - Add `[scrollbar-gutter:stable]` to the scroll container's class list as well. Both the header clip and the body then reserve the same vertical-scrollbar gutter, so their widths, and their maximum `scrollLeft`, are equal. Without it, at the far right the header would stop one scrollbar width short.
   - *Added after implementation (checked in Chrome on 2026-11-13):* the gutter alone was not enough. With `overflow: hidden`, Chrome still capped the header's `scrollLeft` one gutter short of the body's. So the wrapper is `flex`, the row is `shrink-0`, and an empty `aria-hidden` `w-8` spacer follows the row. The header can then always scroll as far as the body.
   - The wrapper has no role. It is a generic container, which ARIA allows between a `grid` and its `row`.

3. **Mirror the scroll.** On the scroll container (`data-scroll-container`), add:

   ```tsx
   onScroll={(event) => {
     if (headerRef.current) {
       headerRef.current.scrollLeft = event.currentTarget.scrollLeft;
     }
   }}
   ```

4. **Give the rows the same width.** In each data row's `style`, add `minWidth: totalWidth` next to `gridTemplateColumns: template`. The selected and hover backgrounds then span all the columns when they are scrolled into view.

Do not change the virtualizer, the row positions, or the "Loading more" row.

### Test

Add to `web/src/routes/$org/$project/traces/index.test.tsx`:

```ts
it("scrolls the header with the rows", async () => {
  await renderApp("/acme/support-bot/traces?range=1h");
  const grid = await screen.findByRole("grid", { name: "Traces" });
  const scroller = grid.querySelector("[data-scroll-container]");
  if (!(scroller instanceof HTMLElement)) {
    throw new Error("missing scroll container");
  }
  const [header, firstRow] = within(grid).getAllByRole("row");
  if (!header || !firstRow) {
    throw new Error("missing rows");
  }
  expect(header.style.minWidth).toMatch(/^\d+px$/);
  expect(firstRow.style.minWidth).toBe(header.style.minWidth);
  scroller.scrollLeft = 300;
  fireEvent.scroll(scroller);
  expect(header.parentElement?.scrollLeft).toBe(300);
});
```

The existing test `"persists a column width and restores the default on double-click"` reads `handle.parentElement?.parentElement`: separator → column header → header row. That path is unchanged, because the wrapper sits outside the header row. The test must still pass unchanged.

## M7 — `j`/`k` start from row 1 after a deep link

### Problem

In `DataTable`'s `onKeyDown`, `const origin = focusedIndex ?? -1;`. `focusedIndex` is set only by a row click or a key move.

So after loading `?trace=<id>`, `j` moves to row index 0 instead of the row after the open trace. It was reproduced on 2026-11-13: with the 5th row open, `j` opened the 1st.

### Change

In `onKeyDown`, replace `const origin = focusedIndex ?? -1;` with:

```ts
const selectedIndex =
  selectedRowId === undefined ? -1 : rows.findIndex((row) => getRowId(row) === selectedRowId);
const origin = focusedIndex ?? selectedIndex;
```

`selectedIndex` is `-1` when nothing is open or the open trace is not among the loaded rows. That keeps today's behaviour for those cases, where `j` goes to the first row.

Leave the `open` (Enter) branch unchanged.

### Tests

The fixture list `list_traces` (no query) returns, in order:

| Row | Trace ID |
|---|---|
| 1 | `a1000000000000000000000000000006` |
| 2 | `a1000000000000000000000000000004` |
| 3 | `a1000000000000000000000000000003` |
| 4 | `a1000000000000000000000000000002` |
| 5 | `a1000000000000000000000000000001` |

Add to `web/src/routes/$org/$project/traces/index.test.tsx`:

```ts
it("moves from the open trace after a deep link", async () => {
  const { router } = await renderApp(
    "/acme/support-bot/traces?range=1h&trace=a1000000000000000000000000000003",
  );
  const list = await screen.findByRole("grid", { name: "Traces" });
  await screen.findByRole("grid", { name: "Trace" });
  list.focus();
  fireEvent.keyDown(list, { key: "j" });
  await waitFor(() => {
    expect(router.state.location.search.trace).toBe("a1000000000000000000000000000002");
  });
  fireEvent.keyDown(list, { key: "k" });
  await waitFor(() => {
    expect(router.state.location.search.trace).toBe("a1000000000000000000000000000003");
  });
});

it("moves up from the open trace after a deep link", async () => {
  const { router } = await renderApp(
    "/acme/support-bot/traces?range=1h&trace=a1000000000000000000000000000003",
  );
  const list = await screen.findByRole("grid", { name: "Traces" });
  await screen.findByRole("grid", { name: "Trace" });
  list.focus();
  fireEvent.keyDown(list, { key: "k" });
  await waitFor(() => {
    expect(router.state.location.search.trace).toBe("a1000000000000000000000000000004");
  });
});
```

Both tests fail on today's code. `j` would open `…0006`, and `k` would stay on index 0, which is `…0006`.

## Docs

None. Spec 11 already says "`j`/`k` … move between rows and the open trace follows" and describes a table header. This phase makes the code match it.

## Done when

- `just check-web` is green.
- The three new tests pass.
- The existing column-resize and j/k tests pass unchanged.
