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
- `web/src/routes/$org/$project/users/$userId.tsx` and `$userId.test.tsx`.

**Commit:** `Add delete actions to the traces and user pages`

## Read first

- M9's `type-to-confirm-dialog.tsx`, `user-deletion-form.tsx` (mutation and error handling) and `lib/deletions.ts`.
- `web/src/components/confirm-dialog.tsx`, `disabled-reason.tsx`.
- `web/src/routes/$org/$project/traces/index.tsx`: `range`, `resolved = resolveRange(range, anchor)` (`{from, to}` RFC 3339), `search.filter`, `search.search`, `search.eval`, `search.exp`, and the toolbar JSX (`ListToolbar`, `SavedViewsMenu`).
- `web/src/components/data-table/use-infinite-list.ts`: `traceListOptions` and its query key (the trace list is an infinite query over `GET /api/v1/projects/{project}/traces`).
- `web/src/components/trace/trace-header.tsx` (`TraceHeader`, its `onClose` prop) and where `TraceView` renders it.
- `web/src/routes/$org/$project/users/$userId.tsx`: `UserHeader`.
- `web/src/lib/format.ts` for number formatting (use its integer formatter if one exists, else `toLocaleString("en-US")`).

All mutations use `$api.useMutation("post", "/api/v1/projects/{project}/deletions")` (or `/deletions/preview`) with `params: { path: { project: ref } }`, `ref` from `useProject()`. After a successful deletion, invalidate the trace list and the deletion history: `queryClient.invalidateQueries({ queryKey: ["get", "/api/v1/projects/{project}/traces"] })` (openapi-react-query keys start with method and path; check one existing key with `traceListOptions(...).queryKey` and use its first two elements) and `queryClient.invalidateQueries({ queryKey: deletionsKey(org, project) })`.

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

Behaviour:
- When `open` becomes `true`, run the preview mutation (reset any previous state). While it runs: description "Counting matching traces…", confirm disabled.
- Preview answered `{matched_traces: 0}`: "No trace matches. Nothing to delete." and the confirm button hidden; Cancel reads `Close`.
- `{capped: true}`: "More than 100,000 traces match. Narrow the filter or the time range." (with `role="alert"`), confirm hidden.
- Otherwise: "N trace(s) match the current filter, search and time range. Their spans, scores and evaluation traces are hidden within seconds and erased within 24 hours. This cannot be undone." and the destructive confirm button `Delete N trace(s)` (N formatted, singular for 1).
- Preview error: `400 invalid_filter` → "The filter is not valid. Fix it on the list first."; other → `problemMessage(caught, "Could not count the traces.")`; confirm hidden.
- Confirm: the deletion mutation. Success → close, invalidate as above. `422` with code `too_many_traces` → the capped text; other errors → `problemMessage(caught, "Could not request the deletion.")`.
- Built on `AlertDialog` like `ConfirmDialog` (a plain confirmation: the count preview is the protection, D-M-12), title `Delete matching traces?`.

`traces/index.tsx`:
- `const { org, project, data } = useProject();` (extend the existing destructuring).
- A `Button variant="outline" size="sm"` labelled `Delete matching…` (lucide `Trash2` icon) placed in the toolbar right after `SavedViewsMenu`, wrapped in `DisabledReason` with reason `Only project admins can delete traces` when `!atLeast(data.role, "admin")` (disabled then).
- It opens `DeleteMatchingDialog` with `selection = { filter: search.filter, search: search.search, from: resolved.from, to: resolved.to, includeEval: search.eval === 1, includeExperiments: search.exp === 1 }`.

## Delete trace

`trace-header.tsx`:
- Add `const { org, project, ref, data } = useProject();` (it already takes `org`, `project`).
- In the right-hand button group, before `CopyButton`: an icon button (`variant="ghost" size="icon-xs"`, `aria-label="Delete trace"`, lucide `Trash2`), wrapped in `DisabledReason` (`Only project admins can delete traces`) and disabled for non-admins.
- It opens `ConfirmDialog`: title `Delete this trace?`, description "Its spans, scores and evaluation traces are hidden within seconds and erased within 24 hours. This cannot be undone.", `confirmLabel="Delete trace"`, `destructive`.
- Confirm → `POST …/deletions { kind: "trace_ids", trace_ids: [trace.trace_id] }`. Success: close the dialog, invalidate as above, then `onClose?.()` when the header is in the split pane, else `navigate({ to: "/$org/$project/traces", params: { org, project } })`. Error: `problemMessage(caught, "Could not delete the trace.")` in the dialog.

## Delete user data

`users/$userId.tsx`, `UserHeader`:
- Add a `Delete user data…` button (`variant="outline" size="xs"`, `Trash2`) at the end of the first row (the `IdText` row; make that row a flex row with the button pushed right), `DisabledReason` `Only project admins can delete data` for non-admins.
- It opens `TypeToConfirmDialog` exactly as M9's `UserDeletionForm` does (same title, description, input label, confirm label, body and error handling), with `expected = user.user_id`.
- Success: invalidate as above (plus the users list: `["get", "/api/v1/projects/{project}/users"]`), then navigate to `/$org/$project/settings/data`.

To avoid a second copy of the dialog wiring, extract from `UserDeletionForm` a component `UserDeletionDialog({ userId, open, onOpenChange, onDone })` in `user-deletion-form.tsx` that owns the mutation and the dialog, and use it in both places (the edit list covers `user-deletion-form.tsx` for this; M9's tests must still pass unchanged).

## Tests

`delete-matching-dialog.test.tsx` (render the dialog inside the app via `renderApp` on the traces page, or alone with a `QueryClientProvider` and `ProjectProvider` as other component tests do; follow `confirm-dialog.test.tsx`):
1. preview `{matched_traces: 3, capped: false}` → text `3 traces match…`, button `Delete 3 traces`; confirm sends `{kind: "filter", filter: "name = 'x'", from, to, include_eval: false, include_experiments: false}` (assert the body; `search` absent).
2. `{matched_traces: 1}` → `Delete 1 trace`.
3. `{capped: true}` → the narrow-it alert, no delete button.
4. `{matched_traces: 0}` → `No trace matches`, no delete button.
5. preview `400 invalid_filter` → the invalid-filter text.
6. deletion `422 too_many_traces` → the capped text.

`traces/index.test.tsx`:
7. owner: `Delete matching…` is enabled; clicking it posts the preview with the URL's `filter`, `search`, the resolved range and the toggles (`renderApp("/acme/support-bot/traces?filter=status%20%3D%20'error'&search=refund&range=24h&eval=1")`; assert `filter`, `search`, `include_eval: true`, and that `from`/`to` are present and ordered).
8. `projectAs("member")`: the button is disabled and its reason is in the accessible description.

`trace-view.test.tsx`:
9. delete flow from the trace header: confirm → body `{kind: "trace_ids", trace_ids: ["<the fixture trace id>"]}`; on the full trace page it then navigates to `/acme/support-bot/traces`.
10. member: the delete button is disabled.

`users/$userId.test.tsx`:
11. type-to-confirm with the fixture user id → body `{kind: "user", user_id: <id>}` → navigates to `/acme/support-bot/settings/data`.
12. member: disabled.

Each page test answers the POST with `server.use(http.post(…, () => HttpResponse.json(deletionDocument, { status: 202 })))` and captures the body (D-L-10).

## Done when

- [ ] The three actions work against the mocks; non-admins see them disabled with the reason.
- [ ] The 12 tests pass; M9's tests still pass; `just check` is green; one commit `Add delete actions to the traces and user pages`.
