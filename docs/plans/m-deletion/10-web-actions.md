# M10 — Web: delete matching traces, a trace, a user's data

Read [README.md](README.md) first, especially § Design › Kinds and Limits, and decision D-M-12. M9's `TypeToConfirmDialog` and `lib/deletions.ts` exist.

**Goal:** three entry points, all admin-only (shown disabled with the reason for other roles, D-L-4):
1. **Traces list** — "Delete matching…" deletes what the list shows: the current filter, search, time range and the eval/experiment toggles. The dialog first previews the count (`POST …/deletions/preview`), then its button reads "Delete N traces".
2. **Trace header** — "Delete trace" asks for a plain confirmation, then `POST …/deletions {kind: "trace_ids"}`.
3. **User page** — "Delete user data…" opens the type-to-confirm dialog (type the user id), then `POST …/deletions {kind: "user"}`.

**Edit only:**
- `web/src/components/deletions/delete-matching-dialog.tsx` and `.test.tsx` (new);
- `web/src/routes/$org/$project/traces/index.tsx` and `index.test.tsx`;
- `web/src/components/trace/trace-header.tsx`, `web/src/components/trace/trace-view.test.tsx`;
- `web/src/routes/$org/$project/users/$userId.tsx` and `$userId.test.tsx`;
- `web/src/components/deletions/user-deletion-form.tsx` (extract `UserDeletionDialog`, below);
- `web/src/components/command/recent-traces.ts` (add `forgetTrace`);
- `web/src/lib/deletions.ts` and its test (add `invalidateProjectReads`).

**Commit:** `Add delete actions to the traces and user pages`

## Read first

- M9's `type-to-confirm-dialog.tsx`, `user-deletion-form.tsx` (mutation and error handling) and `lib/deletions.ts`.
- `web/src/components/confirm-dialog.tsx`, `disabled-reason.tsx`.
- `web/src/routes/$org/$project/traces/index.tsx`: `range`, `resolved = resolveRange(range, anchor)` (`{from, to}` RFC 3339), `search.filter`, `search.search`, `search.eval`, `search.exp`, and the toolbar JSX (`ListToolbar`, `SavedViewsMenu`).
- `web/src/components/data-table/use-infinite-list.ts`: `traceListOptions` and its query key (the trace list is an infinite query over `GET /api/v1/projects/{project}/traces`).
- `web/src/components/trace/trace-header.tsx` (`TraceHeader`, its `onClose` prop) and where `TraceView` renders it.
- `web/src/routes/$org/$project/users/$userId.tsx`: `UserHeader`.
- Numbers in texts are formatted with `n.toLocaleString("en-US")` (`lib/format.ts` has no integer formatter).

All mutations use `$api.useMutation("post", "/api/v1/projects/{project}/deletions")` (or `/deletions/preview`) with `params: { path: { project: ref } }`, `ref` from `useProject()`. After a successful deletion, invalidate every read of the project, since traces, spans, sessions, users and metrics all show deleted data: `queryClient.invalidateQueries({ predicate: (query) => typeof query.queryKey[1] === "string" && query.queryKey[1].startsWith("/api/v1/projects/{project}/") })` (openapi-react-query keys are `[method, path template, init]`; check one with `traceListOptions(...).queryKey`). Put this in one exported helper `invalidateProjectReads(queryClient)` in `lib/deletions.ts`.

`components/command/recent-traces.ts` keeps recently opened traces (id and name) in `localStorage`. Add `export function forgetTrace(traceId: string): void` that removes that id (follow the file's read/write helpers), and call it after a successful single-trace deletion.

## Delete matching

`web/src/components/deletions/delete-matching-dialog.tsx`:

```tsx
export type TraceSelection = {
  filter?: string;
  search?: string;
  from: string;   // RFC 3339
  to: string;     // RFC 3339
  includeEval: boolean;
  includeExperiments: boolean;
};

export function DeleteMatchingDialog({
  open,
  onOpenChange,
  selection,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  selection: TraceSelection;
})
```

Body sent to both endpoints: `{ filter, search, from, to, include_eval: includeEval, include_experiments: includeExperiments }` with `filter`/`search` omitted when empty or blank; for the deletion add `kind: "filter"`.

Structure: `DeleteMatchingDialog` renders `AlertDialog` / `AlertDialogContent` (not `ConfirmDialog`: it hides buttons and renames Cancel) and puts the description, error and footer in an inner component `DeleteMatchingBody` rendered inside `AlertDialogContent`. Radix unmounts the content when closed, so the body mounts once per opening: run `preview.mutate(...)` in a mount-only `useEffect(() => { … }, [])` with the comment `// biome-ignore lint/correctness/useExhaustiveDependencies: runs once per opening` (the precedent is `disabled-reason.tsx`'s style of biome-ignore). Do not put `selection` in an effect dependency: it is a new object on every render.

Behaviour:
- On mount of the body, run the preview mutation. While it runs: description "Counting matching traces…", confirm disabled.
- Preview answered `{matched_traces: 0}`: "No trace matches. Nothing to delete." and the confirm button hidden; Cancel reads `Close`.
- `{capped: true}`: "More than 100,000 traces match. Narrow the filter or the time range." (with `role="alert"`), confirm hidden.
- Otherwise: "N trace(s) match the current filter, search and time range. Their spans, scores and evaluation traces are hidden within seconds and erased within 24 hours. This cannot be undone." and the destructive confirm button `Delete N trace(s)` (N formatted, singular for 1).
- Preview error: `400 invalid_filter` → "The filter is not valid. Fix it on the list first."; other → `problemMessage(caught, "Could not count the traces.")`; confirm hidden.
- Confirm: the deletion mutation. Success → close, invalidate as above. `422` with code `too_many_traces` → the capped text; other errors → `problemMessage(caught, "Could not request the deletion.")`.
- Built on `AlertDialog` like `ConfirmDialog` (a plain confirmation: the count preview is the protection, D-M-12), title `Delete matching traces?`.

`traces/index.tsx`:
- `const { org, project, data } = useProject();` (extend the existing destructuring).
- A `Button variant="outline" size="sm"` labelled `Delete matching…` (lucide `Trash2` icon), wrapped in `DisabledReason` with reason `Only project admins can delete traces` when `!atLeast(data.role, "admin")` (disabled then). `SavedViewsMenu` is passed to `ListToolbar` as its `views` prop; pass `views={<><SavedViewsMenu … /><DisabledReason …><Button …/></DisabledReason></>}`. Render `DeleteMatchingDialog` as a sibling inside the page's existing `<TooltipProvider>`.
- It opens `DeleteMatchingDialog` with `selection = { filter: search.filter, search: search.search, from: resolved.from, to: resolved.to, includeEval: search.eval === 1, includeExperiments: search.exp === 1 }`.

## Delete trace

`trace-header.tsx`:
- Add `const { org, project, ref, data } = useProject();` (it already takes `org`, `project`), `const navigate = useNavigate();` (`import { useNavigate } from "@tanstack/react-router"`) and `useQueryClient`.
- In the right-hand button group, before `CopyButton`: an icon button (`variant="ghost" size="icon-xs"`, `aria-label="Delete trace"`, lucide `Trash2`), wrapped in `DisabledReason` (`Only project admins can delete traces`) and disabled for non-admins.
- It opens `ConfirmDialog`: title `Delete this trace?`, description "Its spans, scores and evaluation traces are hidden within seconds and erased within 24 hours. This cannot be undone.", `confirmLabel="Delete trace"`, `destructive`.
- Confirm → `POST …/deletions { kind: "trace_ids", trace_ids: [trace.trace_id] }`. Success: close the dialog, `forgetTrace(trace.trace_id)`, then `onClose?.()` when the header is in a split pane (the traces and spans list pages both pass `onClose`), else `await navigate({ to: "/$org/$project/traces", params: { org, project } })`; invalidate after leaving (navigating first keeps the trace query from refetching a 404 into the error boundary). Error: `problemMessage(caught, "Could not delete the trace.")` in the dialog.

## Delete user data

`users/$userId.tsx`, `UserHeader`:
- `UserHeader` calls `useProject()` for `org`, `project`, `data`, plus `useNavigate()`.
- Replace the lone `<IdText …/>` first row with `<div className="flex items-center gap-2"><IdText …/><div className="ml-auto">{button}</div></div>`; the button is `Delete user data…` (`variant="outline" size="xs"`, `Trash2`) in `DisabledReason` `Only project admins can delete data` for non-admins.
- It opens `UserDeletionDialog` (below) with `userId = user.user_id`.
- `onDone`: `await navigate({ to: "/$org/$project/settings/data", params: { org, project } })`, then `invalidateProjectReads(queryClient)` (navigate first: the user page's query would refetch a 404).

Extract from M9's `UserDeletionForm` a component in the same file:

```tsx
export function UserDeletionDialog({ userId, open, onOpenChange, onDone, onFieldError }: {
  userId: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Called after a successful request. */
  onDone: () => void;
  /** A 400 on `user_id`: the form shows it under its field and closes the dialog. Without it, the dialog shows the message. */
  onFieldError?: (message: string) => void;
})
```

It owns the mutation (and its `reset()` on opening) and the `TypeToConfirmDialog`. `UserDeletionForm` passes `onDone` (show the success text, clear the field, invalidate the history) and `onFieldError`; the user page passes only `onDone`. M9's tests must pass unchanged.

## Tests

`delete-matching-dialog.test.tsx`: render the dialog alone with `<QueryClientProvider client={new QueryClient()}><ProjectProvider value={{ org: "acme", project: "support-bot", ref: "acme/support-bot", data: projectFixture }}><DeleteMatchingDialog open onOpenChange={…} selection={…} /></ProjectProvider></QueryClientProvider>` (`projectFixture` from `fixtures/api/static/project.json`); answer the preview and the deletion with `server.use`. Errors use `problemResponse(status, code, { detail })`; assert on texts and codes, not titles:
1. preview `{matched_traces: 3, capped: false}` → text `3 traces match…`, button `Delete 3 traces`; confirm sends `{kind: "filter", filter: "name = 'x'", from, to, include_eval: false, include_experiments: false}` (assert the body; `search` absent).
2. `{matched_traces: 1}` → `Delete 1 trace`.
3. `{capped: true}` → the narrow-it alert, no delete button.
4. `{matched_traces: 0}` → `No trace matches`, no delete button.
5. preview `400 invalid_filter` → the invalid-filter text.
6. deletion `422 too_many_traces` → the capped text.

`traces/index.test.tsx`:
7. owner: `Delete matching…` is enabled; clicking it posts the preview with the URL's `filter`, `search`, the resolved range and the toggles (`renderApp("/acme/support-bot/traces?filter=status%20%3D%20'error'&search=refund&range=24h&eval=1")`; assert `filter`, `search`, `include_eval: true`, and that `from`/`to` are present and ordered).
8. `projectAs("member")` (define the 4-line helper locally, as `general.test.tsx` does): the button is disabled, and `expect(button.parentElement).toHaveAccessibleDescription("Only project admins can delete traces")`.

`trace-view.test.tsx`:
9. delete flow from the trace header on `renderApp("/acme/support-bot/traces/<fixture trace id>")`: confirm → body `{kind: "trace_ids", trace_ids: ["<the fixture trace id>"]}`; then the router is on `/acme/support-bot/traces`.
10. member (`projectAs("member")`, local helper): the delete button is disabled; its reason is the wrapper's accessible description.

`users/$userId.test.tsx`:
11. type-to-confirm with the fixture user id → body `{kind: "user", user_id: <id>}` → navigates to `/acme/support-bot/settings/data`.
12. member (`projectAs`, local helper): disabled, with the reason as the wrapper's accessible description.

Each page test answers the POST with `server.use(http.post(…, () => HttpResponse.json(deletionDocument, { status: 202 })))` and captures the body (D-L-10).

## Done when

- [ ] The three actions work against the mocks; non-admins see them disabled with the reason.
- [ ] The 12 tests pass; M9's tests still pass; `just check` is green; one commit `Add delete actions to the traces and user pages`.
