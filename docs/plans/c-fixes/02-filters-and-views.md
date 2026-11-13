# CF2–CF3 — Filters and saved views

See [README.md](README.md) for the rules that apply to every phase.

---

## CF2 — Filter text never changes meaning (M2, M3, M4)

**Edit only:**
- `web/src/lib/dsl/dsl.ts` and `web/src/lib/dsl/dsl.test.ts`;
- `web/src/components/filter-bar/logic.ts` and `web/src/components/filter-bar/logic.test.ts`;
- `web/src/components/filter-bar/filter-bar.test.tsx`;
- `docs/spec/11-ui.md`;
- `docs/mvp-decisions.md`;
- the `Status:` line of `docs/plans/c-fixes/README.md`.

**Commit:** `web: keep OR and BETWEEN filters intact in the filter bar`

### Problem

1. **`splitConjuncts` splits on every top-level `AND`** (`web/src/lib/dsl/dsl.ts`, the function under the comment "Splits on top-level AND").
   - In SQL, `AND` binds tighter than `OR`. So `a = 1 OR b = 2 AND c = 3` means `a = 1 OR (b = 2 AND c = 3)`.
   - Today it becomes the chips `[a = 1 OR b = 2]` and `[c = 3]`.
   - `serializeFilter` wraps the first in parentheses, so editing or removing a chip applies `(a = 1 OR b = 2) AND …`. That is a different filter (D129).
2. **`cost_total BETWEEN 1 AND 2` is split** into `cost_total BETWEEN 1` and `2`.
3. **`appendFilter` joins with a bare `AND`** (`web/src/components/filter-bar/logic.ts`, `export function appendFilter`). With `a = 1 OR b = 2`, click-to-filter produces `a = 1 OR b = 2 AND status = 'error'`, which widens the result.

### Changes in `web/src/lib/dsl/dsl.ts`

1. **Export `hasTopLevelOr`.** Change `function hasTopLevelOr(text: string): boolean` to `export function hasTopLevelOr(text: string): boolean`. Do not change its body.

2. **Rewrite `splitConjuncts`.** Keep its signature and its return shape (`Conjunct[]` with `text`, `start` and `end` offsets into the input):

   a. **Top-level OR.** If `hasTopLevelOr(text)` is true, return one conjunct holding the whole trimmed text. Return `[]` if the trimmed text is empty. Otherwise compute `start` as the index of the first non-space character and `end = start + trimmed.length`, exactly as the existing loop does for each piece.

   b. **`BETWEEN`.** Otherwise, keep the existing loop and add one rule: when a token at depth 0 is the identifier `between` in any case, the next top-level `and` keyword is not a cut.
      - Use the existing helper `isName(token, "between")`. `between` is not in `KEYWORDS`, so it tokenizes as `ident`. Do not add it to `KEYWORDS`.
      - Track this with a boolean, for example `skipNextAnd`:
        - set it to `true` when `depth === 0 && isName(token, "between")`;
        - when `depth === 0 && isKeyword(token, "and")`: if `skipNextAnd` is true, set it to `false` and do not cut; otherwise cut as today.
      - `NOT BETWEEN` needs nothing extra, because `between` is still the token that sets the flag.

   c. Update the doc comment to: `/** Splits on top-level AND. A text with a top-level OR is one conjunct, and the AND of a BETWEEN is never a cut. Trims each piece and drops empty ones. */`

3. **Leave `serializeFilter` unchanged.** A raw chip with a top-level `OR` is already parenthesized, which is still correct now that such a chip is the whole filter.

### Changes in `web/src/components/filter-bar/logic.ts`

Replace the body of `appendFilter`:

```ts
export function appendFilter(current: string, conjunct: string): string {
  const base = current.trim();
  if (base.length === 0) {
    return conjunct;
  }
  return hasTopLevelOr(base) ? `(${base}) AND ${conjunct}` : `${base} AND ${conjunct}`;
}
```

Import `hasTopLevelOr` from `@/lib/dsl/dsl`. The file may already import from that module; if so, add it to that import.

### Tests

**`web/src/lib/dsl/dsl.test.ts`** — add these cases to the `cases` array. Put them just after the case named `"top-level or is wrapped"`, which stays unchanged. The file already has the helpers `compare`, `str` and `num`; `num(value, text)` builds a number `ChipValue`.

| `name` | `input` | `chips` | `canonical` |
|---|---|---|---|
| `"or binds looser than and"` | `a = 1 OR b = 2 AND c = 3` | `[{ kind: "raw", text: "a = 1 OR b = 2 AND c = 3" }]` | `(a = 1 OR b = 2 AND c = 3)` |
| `"and before a top-level or"` | `status = 'error' AND a = 1 OR b = 2` | `[{ kind: "raw", text: "status = 'error' AND a = 1 OR b = 2" }]` | `(status = 'error' AND a = 1 OR b = 2)` |
| `"or inside parentheses still splits"` | `(a = 1 OR b = 2) AND c = 3` | `[{ kind: "raw", text: "(a = 1 OR b = 2)" }, compare("c", "=", num(3, "3"))]` | `(a = 1 OR b = 2) AND c = 3` |
| `"between keeps its and"` | `cost_total BETWEEN 1 AND 2` | `[{ kind: "raw", text: "cost_total BETWEEN 1 AND 2" }]` | `cost_total BETWEEN 1 AND 2` |
| `"between then another conjunct"` | `cost_total BETWEEN 1 AND 2 AND status = 'ok'` | `[{ kind: "raw", text: "cost_total BETWEEN 1 AND 2" }, compare("status", "=", str("ok"))]` | `cost_total BETWEEN 1 AND 2 AND status = 'ok'` |
| `"not between, lower case"` | `duration_ms not between 1 and 2 AND a = 1` | `[{ kind: "raw", text: "duration_ms not between 1 and 2" }, compare("a", "=", num(1, "1"))]` | `duration_ms not between 1 and 2 AND a = 1` |

The existing `it.each(cases)` also checks the offsets and that non-raw chips round-trip, so these cases need no other assertions.

Add one offsets test after `it("records conjunct offsets", …)`:

```ts
it("records the offsets of a whole OR filter", () => {
  expect(splitConjuncts("  a = 1 OR b = 2  ")).toEqual([{ text: "a = 1 OR b = 2", start: 2, end: 16 }]);
});
```

**`web/src/components/filter-bar/logic.test.ts`** — import `appendFilter` and add:

```ts
it("appends a conjunct and parenthesizes a top-level OR", () => {
  expect(appendFilter("", "x = 1")).toBe("x = 1");
  expect(appendFilter("  ", "x = 1")).toBe("x = 1");
  expect(appendFilter("a = 1", "b = 2")).toBe("a = 1 AND b = 2");
  expect(appendFilter("a = 1 OR b = 2", "status = 'error'")).toBe("(a = 1 OR b = 2) AND status = 'error'");
  expect(appendFilter("(a = 1 OR b = 2)", "c = 3")).toBe("(a = 1 OR b = 2) AND c = 3");
});
```

**`web/src/components/filter-bar/filter-bar.test.tsx`** — add two page tests.

Every URL in them includes `range=1h`. Until CF3 lands, an empty search re-applies the seeded default view, "Errors", which would change the filter under the test.

```ts
it("shows a filter with a top-level OR as one chip and removes it whole", async () => {
  const filter = "a = 1 OR b = 2 AND c = 3";
  const { router, user } = await renderApp(
    `/acme/support-bot/traces?range=1h&filter=${encodeURIComponent(filter)}`,
  );
  const remove = await screen.findByRole("button", { name: `Remove ${filter}` });
  expect(screen.queryByRole("button", { name: "Remove c = 3" })).not.toBeInTheDocument();
  await user.click(remove);
  await waitFor(() => {
    expect(router.state.location.search.filter).toBeUndefined();
  });
  expect(router.state.location.search.range).toBe("1h");
});

it("parenthesizes an OR filter before appending from a row", async () => {
  const { router, user } = await renderApp(
    `/acme/support-bot/traces?range=1h&filter=${encodeURIComponent("a = 1 OR b = 2")}`,
  );
  await screen.findByRole("grid", { name: "Traces" });
  await user.click(screen.getByRole("button", { name: "Filter by status error" }));
  await waitFor(() => {
    expect(router.state.location.search.filter).toBe("(a = 1 OR b = 2) AND status = 'error'");
  });
});
```

The mocks ignore `filter`, so the grid still shows the fixture rows, including one with status error. The existing test `"appends a status comparison from the row"` relies on the same thing.

### Docs

**`docs/spec/11-ui.md` § Filter bar.** After the sentence ending "Anything else is a raw chip.", insert:

> A filter with a top-level `OR` is one raw chip, because `AND` binds tighter and splitting it would change its meaning. The `AND` of a `BETWEEN` is never a split point. Click-to-filter puts a filter with a top-level `OR` in parentheses before appending `AND` (D-C-1).

**`docs/mvp-decisions.md`.** Create the `## Track C — Web UI` section exactly as `docs/plans/c-fixes/README.md` § Decisions shows, then add this row:

```markdown
| D-C-1 | OR in the filter bar | **A filter with a top-level `OR` is one raw chip.** `AND` binds tighter than `OR`, so splitting `a OR b AND c` on `AND` and re-joining the chips would apply `(a OR b) AND c`. `splitConjuncts` returns the whole text as one conjunct when it has a top-level `OR`; the `AND` of `[NOT] BETWEEN x AND y` is never a split point; click-to-filter wraps a filter with a top-level `OR` in parentheses before appending `AND`. Refines D129. |
```

### Done when

- `just check-web` is green.
- The six new `cases` rows, the offsets test, the `appendFilter` test and the two page tests pass.
- The commit holds only the files listed under **Edit only**.

---

## CF3 — Default saved view only when a list page is entered (M5)

**Edit only:**
- `web/src/components/saved-views/default-view.ts`;
- `web/src/components/saved-views/saved-views-menu.test.tsx`;
- `web/src/components/shell/project-sidebar.tsx`;
- `web/src/components/shell/project-switcher.tsx`;
- `web/src/components/command/palette.tsx`;
- `web/src/routes/$org/$project/traces/index.tsx` and `web/src/routes/$org/$project/traces/index.test.tsx`;
- `docs/spec/11-ui.md`;
- `docs/mvp-decisions.md`;
- the `Status:` line of `docs/plans/c-fixes/README.md`.

**Commit:** `web: apply the default saved view only when a list page is entered`

### Problem

`useDefaultSavedView` in `web/src/components/saved-views/default-view.ts` applies the default view whenever the route's search is empty (`const empty = Object.keys(search).length === 0`). It is used by the traces, spans, sessions and users lists.

So any in-page change that empties the search re-applies the default view. Reproduced on 2026-11-13: on `/traces?eval=1`, switching Eval off put back `filter=status = 'error'` and `view=…`.

The traces page works around this for "Clear filters" only: `withoutConstraints` in `traces/index.tsx` returns `{ range: "24h" }` when nothing else is left.

### Rule (D-C-2)

The default view applies when **both** hold:
- the search is empty;
- the current history entry is one where the page was **entered**:
  - (a) the entry the page component mounted on (a fresh load, a typed URL, or a link from another route);
  - (b) an entry whose history state carries `applyDefaultView: true`. The sidebar, the ⌘K palette, the `g` shortcuts and the project switcher set it.

It applies at most once per history entry. In-page changes never set `applyDefaultView`, so they never re-apply it.

TanStack Router gives every history entry a unique `state.__TSR_key`. This was checked on 2026-11-13 with memory history: the initial entry has one, and every navigation gets a new one.

### Changes

1. **`web/src/components/saved-views/default-view.ts`:**

   a. Add this declaration merge. Look at the existing one in `web/src/lib/search/sessions.ts` and follow the same form:

   ```ts
   declare module "@tanstack/react-router" {
     interface HistoryState {
       /** Set by navigation that enters a list page (sidebar, palette, g shortcuts, project switcher). */
       applyDefaultView?: true;
     }
   }
   ```

   b. Rewrite `useDefaultSavedView`, keeping its signature and return type:

   ```ts
   export function useDefaultSavedView(
     kind: Schema<"SavedViewKind">,
     search: object,
     onApply: (search: Record<string, unknown>) => void,
   ): { hold: boolean } {
     const { org, project } = useProject();
     const location = useLocation();
     const entryKey = location.state.__TSR_key;
     const mountKey = useRef(entryKey);
     const appliedKey = useRef<string | undefined>(undefined);
     const views = useQuery(savedViewsQuery(org, project, kind));
     const me = useQuery(meQuery);
     const empty = Object.keys(search).length === 0;
     const entered = entryKey === mountKey.current || location.state.applyDefaultView === true;
     const pending = empty && entered && appliedKey.current !== entryKey;
     const settled = views.isFetched && me.isFetched;
     const match =
       pending && settled ? defaultSavedView(views.data?.data ?? [], kind, me.data?.user.id) : undefined;

     useEffect(() => {
       if (!match) {
         return;
       }
       appliedKey.current = entryKey;
       onApply({ ...searchFromViewState(match.state), view: match.id });
     }, [match, onApply, entryKey]);

     return { hold: pending && (!settled || match !== undefined) };
   }
   ```

   - Import `useLocation` from `@tanstack/react-router` and `useRef` from `react`.
   - Update the doc comment to: `/** Applies the caller's default view when the list page is entered with no search params (D-C-2). */`
   - `defaultSavedView` stays as it is.

2. **Set `applyDefaultView` on the four entry navigations.** Use the same updater form as `web/src/routes/$org/$project/sessions/index.tsx`, which writes `state: (current) => ({ ...current, sessionsSearch: search })`.

   | File | Where | Change |
   |---|---|---|
   | `web/src/components/shell/project-sidebar.tsx` | the `<Link to={item.to} params={{ org, project }}>` inside `projectNav.map` | add `state={(current) => ({ ...current, applyDefaultView: true })}` |
   | `web/src/components/command/palette.tsx` | the `g`-shortcut navigation `void navigate({ to: destination, params: { org, project } });` | add `state: (current) => ({ ...current, applyDefaultView: true })` |
   | `web/src/components/command/palette.tsx` | the page items, `void navigate({ to: item.to, params: { org, project } });` inside `projectNav.map` | the same `state` |
   | `web/src/components/command/palette.tsx` | the project items, `void navigate({ to: "/$org/$project/traces", params: { org, project: item.slug } })` | the same `state` |
   | `web/src/components/shell/project-switcher.tsx` | `void navigate({ to: "/$org/$project/traces", params: { org, project: slug } });` | the same `state` |

   Two cases are deliberate, not oversights:
   - `projectNav` includes the dashboard. The flag is harmless there: the dashboard does not call `useDefaultSavedView`.
   - Leave every other navigation unchanged: dashboard bucket clicks, the events-list link, opening a trace, and all in-page updaters.

   If `tsc` rejects the updater type on `<Link state=…>`, stop and report the error. Do not cast.

3. **Remove the workaround** in `web/src/routes/$org/$project/traces/index.tsx`, in `function withoutConstraints`. Replace:

   ```ts
     // An empty search would reapply the default saved view and put the filter back.
     return Object.keys(rest).length === 0 ? { range: "24h" } : rest;
   ```

   with:

   ```ts
     return rest;
   ```

### Tests

**`web/src/routes/$org/$project/traces/index.test.tsx`:**
- The test `"clears a filter and shows onboarding when nothing is left"` asserts `expect(router.state.location.search.range).toBe("24h");`.
- Replace that line with `expect(router.state.location.search).toEqual({});`.
- Leave the rest of the test as it is. Its onboarding assertion still holds: the search is empty and the range is the 24h default.

**`web/src/components/saved-views/saved-views-menu.test.tsx`** — keep `"applies the default view when the page opens with no search"` as it is, and add three tests. The seeded store (`fixtures/api/static/saved-views.json`) has a shared default traces view, "Errors": `filter: status = 'error'`, `range: 24h`, id `ERRORS` (the constant already in the file).

```ts
it("keeps the search empty after an in-page change", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces?eval=1");
  await screen.findByRole("grid", { name: "Traces" });
  await user.click(screen.getByRole("switch", { name: "Eval" }));
  await waitFor(() => {
    expect(router.state.location.search).toEqual({});
  });
  // Give a wrongly re-applied default view time to land.
  await new Promise((resolve) => setTimeout(resolve, 100));
  expect(router.state.location.search).toEqual({});
});

it("does not apply the default view after deleting the loaded view", async () => {
  server.use(
    ...savedViewHandlers([
      ...seedSavedViews(),
      {
        id: REFUNDS,
        kind: "traces",
        name: "Refunds",
        state: { filter: "name = 'refund'" },
        owner_user_id: "01900000-0000-7000-8000-000000000100",
        shared: false,
        is_default: false,
        created_at: "2026-01-01T09:00:00.000000000Z",
        updated_at: "2026-01-01T09:00:00.000000000Z",
      },
    ]),
  );
  // Only `view` in the URL, so deleting the view leaves the search empty.
  const { router, user } = await renderApp(`/acme/support-bot/traces?view=${REFUNDS}`);
  await user.click(await screen.findByRole("button", { name: /Refunds/ }));
  await user.click(screen.getByRole("menuitem", { name: "Delete" }));
  await user.click(screen.getByRole("button", { name: "Delete view" }));
  await waitFor(() => {
    expect(router.state.location.search.view).toBeUndefined();
  });
  await new Promise((resolve) => setTimeout(resolve, 100));
  expect(router.state.location.search).toEqual({});
});

it("applies the default view again from the sidebar", async () => {
  const { router, user } = await renderApp("/acme/support-bot/traces?range=1h");
  await screen.findByRole("grid", { name: "Traces" });
  await user.click(screen.getByRole("link", { name: "Traces" }));
  await waitFor(() => {
    expect(router.state.location.search).toMatchObject({
      filter: "status = 'error'",
      range: "24h",
      view: ERRORS,
    });
  });
});
```

Setup for these tests:
- Add `const REFUNDS = "01900000-0000-7000-8000-000000000299";` next to `ERRORS`.
- Import `savedViewHandlers` and `seedSavedViews` from `@/mocks/saved-views`. `server` is already imported from `@/mocks/node`.
- `src/test/setup.ts` calls `server.use(...savedViewHandlers(seedSavedViews()))` before each test. Calling `savedViewHandlers` again in a test replaces the in-memory store. This is how the second test adds a private view, "Refunds".
- `owner_user_id` `01900000-0000-7000-8000-000000000100` is the user in the `get_me` fixture, so the view is owned and the menu offers Delete.
- The delete handler in `saved-views-menu.tsx` drops only `view` from the search, so with `?view=` alone the search becomes empty. With today's code, the default view "Errors" would then be applied, so the test fails before the fix.
- With `?view=` alone, the URL differs from the view's state. The menu button therefore shows the "Unsaved changes" dot, and its accessible name is "Refunds Unsaved changes". That is why the test matches `/Refunds/`.

### Docs

**`docs/spec/11-ui.md`, line 32 ("**Saved views**…").** Append:

> The default view applies when a list page is entered with no search params: when the page loads, or from the sidebar, ⌘K or a `g` shortcut. A change made on the page that leaves the search empty does not re-apply it (D-C-2).

**`docs/mvp-decisions.md`, under `## Track C — Web UI`.** Add:

```markdown
| D-C-2 | Default saved view | **The default view applies only when a list page is entered with an empty search**: on the history entry the page mounted on, or on an entry whose history state carries `applyDefaultView` (set by the sidebar, the ⌘K palette, the `g` shortcuts and the project switcher). It applies once per history entry. An in-page change that empties the search (a toggle, removing the last chip, deleting the loaded view, "Clear filters") never re-applies it. |
```

### Done when

- `just check-web` is green.
- The three new tests pass.
- The first new test fails if `useDefaultSavedView` is reverted. Check this once by hand.
