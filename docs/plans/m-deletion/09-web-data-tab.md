# M9 — Web: Data tab and type-to-confirm dialog

Read [README.md](README.md) first, especially § Design › Lifecycle and Access, and decision D-M-12 (confirmations).

**Goal:** the project settings **Data** tab (`/$org/$project/settings/data`, admins only):
- "Delete a user's data": a user-id field, then a type-to-confirm dialog, then `POST …/deletions {kind: "user"}`;
- "Deletion history": the project's deletions with their status, refreshed while any is open.

Plus the reusable `TypeToConfirmDialog` that M10 and M11 use.

**Edit only:**
- `web/src/components/type-to-confirm-dialog.tsx` and `.test.tsx` (new);
- `web/src/components/deletions/user-deletion-form.tsx`, `deletion-history.tsx` and their tests (new);
- `web/src/lib/deletions.ts` and `.test.ts` (new);
- `web/src/api/queries-settings.ts` (`deletionsInit`, `deletionsKey`);
- `web/src/components/settings/settings-nav.ts` and `settings-nav.test.ts`;
- `web/src/routes/$org/$project/settings/data.tsx` and `data.test.tsx` (new).

**Commit:** `Add the Data settings tab`

## Read first

- `web/src/components/confirm-dialog.tsx` (the dialog to mirror) and `confirm-dialog.test.tsx`.
- `web/src/routes/$org/$project/settings/api-keys.tsx` and its test: an admin-only settings page (`Forbidden` for non-admins, D-L-4), `$api.useMutation`, `useQueryClient().invalidateQueries`, `problemOf`/`fieldErrors`/`problemMessage`.
- `web/src/routes/$org/$project/settings/general.tsx`: form conventions (D-L-11: controlled inputs, `TextField`, `FormError`, `FormSuccess`, `SubmitButton`).
- `web/src/components/settings/settings-page.tsx`, `settings-nav.ts`, `settings-sidebar.tsx`.
- `web/src/components/shell/project-context.tsx`: `useProject()` → `{ org, project, ref, data }`; `data.role` is the effective project role.
- `web/src/api/queries-settings.ts`, `web/src/api/client.ts` (`$api`), `web/src/api/types.ts` (`Schema<"…">`).
- `web/src/components/time-text.tsx`, `id-text.tsx`, `ui/table.tsx`, `ui/badge.tsx`, `empty-state.tsx`.
- `web/src/test/render.tsx` (`renderApp`), `web/src/mocks/node.ts` (`server`), `web/src/test/problem.ts` (`problemResponse`).
- `fixtures/api/static/deletions.json`, `deletion.json` (M4): the default mocks serve `GET …/deletions` from them.

Routes are file-based (`@tanstack/router-plugin` generates `src/routeTree.gen.ts`, which is gitignored): creating `data.tsx` registers the route.

## `TypeToConfirmDialog`

`web/src/components/type-to-confirm-dialog.tsx`:

```tsx
export function TypeToConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  expected,
  inputLabel,
  confirmLabel,
  pending,
  error,
  onConfirm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: string;
  description: ReactNode;
  /** The exact text the user must type (a slug or a user id). */
  expected: string;
  /** For example `Type the project slug "support-bot" to confirm`. */
  inputLabel: string;
  confirmLabel: string;
  pending: boolean;
  error?: string;
  onConfirm: () => void;
})
```

- Same structure as `ConfirmDialog` (`AlertDialog`, header with title and description, `FormError`, footer with `Cancel` and a `destructive` button), plus an `Input` with a `Label` (`inputLabel`) between the description and the error. `autoComplete="off"`, `spellCheck={false}`.
- The confirm button is disabled unless the typed text equals `expected` exactly (no trimming, case-sensitive) and `!pending`.
- Enter in the input calls `onConfirm` when the button is enabled.
- The typed text resets to `""` whenever `open` becomes `true` (a `useEffect` on `open`).
- Esc and the outside click do not close it while `pending` (as `ConfirmDialog`).

Tests (`type-to-confirm-dialog.test.tsx`, rendering the component alone inside the same providers `confirm-dialog.test.tsx` uses):
1. the confirm button is disabled until the exact text is typed; a different case (`Support-Bot`) keeps it disabled;
2. clicking it calls `onConfirm` once; Enter does the same;
3. reopening clears the field;
4. while `pending`, Cancel is disabled and Esc does not call `onOpenChange`.

## Labels

`web/src/lib/deletions.ts`:

```ts
import type { Schema } from "@/api/types";
type Deletion = Schema<"Deletion">;

/** "User", "Traces by id", "Traces by filter", "Project", "Organization". */
export function deletionKindLabel(kind: Deletion["kind"]): string;
/** "Finding data", "Hidden, erasing", "Erased", "Failed". */
export function deletionStatusLabel(status: Deletion["status"]): string;
/** What was deleted, in words:
 *  user → `user <user_id>`; trace_ids → `<requested_traces> trace(s)` (plural rule as `count` in the users page);
 *  filter → the filter text, else `search "<search>"`, else `all traces`, followed by ` (<trace_count> traces)` once known;
 *  project / org → `<name> (<slug>)`. */
export function deletionSubject(deletion: Deletion): string;
/** "Too many traces matched (over 100,000)" / "Could not find the data to delete". */
export function deletionErrorLabel(error: NonNullable<Deletion["error"]>): string;
/** True while the status is `resolving` or `pending`. */
export function isOpenDeletion(deletion: Deletion): boolean;
```

Tests (`deletions.test.ts`): one case per kind for `deletionSubject` (filter with text, with search only, with neither; with and without `trace_count`), every status and error label, `isOpenDeletion` for the four statuses.

## Queries

`web/src/api/queries-settings.ts`:

```ts
export function deletionsInit(org: string, project: string) {
  return { params: { path: { project: projectRef(org, project) }, query: { limit: 50 } } };
}
export function deletionsKey(org: string, project: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/deletions", deletionsInit(org, project)).queryKey;
}
```

## Components

`web/src/components/deletions/deletion-history.tsx` — `export function DeletionHistory()`:
- `const { org, project } = useProject();` `const list = $api.useQuery("get", "/api/v1/projects/{project}/deletions", deletionsInit(org, project), { refetchInterval: (query) => (query.state.data?.data.some(isOpenDeletion) ? 5000 : false) });`
- A `<section aria-labelledby=…>` with heading `<h2>Deletion history</h2>`.
- Pending: nothing (no spinner). Error: `FormError` with `problemMessage(list.error, "Could not load the deletions.")`. Empty: `EmptyState` titled `No deletions yet`.
- Otherwise a `Table` (`ui/table.tsx`) with `aria-label="Deletions"` and columns: **Requested** (`TimeText value={created_at} style="datetime"`), **What** (`deletionKindLabel(kind)` then, muted, `deletionSubject`), **Status** (`Badge` with `deletionStatusLabel`; variant `destructive` for `failed`, `secondary` for `applied`, `outline` otherwise; for `failed` also `deletionErrorLabel(error)` below it), **By** (`API key` or `Member`, then `IdText value={requested_by.id}`; check `IdText`'s props), **Erased** (`TimeText` of `applied_at`, or `—`).
- Only the first page (50 rows) is shown; no pager.

`web/src/components/deletions/user-deletion-form.tsx` — `export function UserDeletionForm()`:
- `<section>` with heading `<h2>Delete a user's data</h2>` and the text: "Removes every span and score of this end user, with their traces, sessions and evaluation traces. The data is hidden within seconds and erased within 24 hours. This cannot be undone."
- A form (`aria-label="Delete a user's data"`, `noValidate`) with `TextField` `id="deletion-user-id"`, label `User ID`, `required`. Submit button `Delete user data…`: an empty field sets the field error `Enter a user ID.`; a value over 255 bytes (`new TextEncoder().encode(value).length > 255`) sets `Use at most 255 bytes.`; otherwise it opens the dialog. The value is sent as typed (not trimmed: user ids match exactly).
- `TypeToConfirmDialog`: `title="Delete this user's data?"`, `description` repeats the paragraph above, `expected={userId}`, `inputLabel={`Type the user ID "${userId}" to confirm`}`, `confirmLabel="Delete user data"`.
- `const mutation = $api.useMutation("post", "/api/v1/projects/{project}/deletions");` with `{ params: { path: { project: ref } }, body: { kind: "user", user_id: userId } }`.
  - success: close the dialog, clear the field, show `FormSuccess` "Deletion requested. The data is hidden within seconds and erased within 24 hours.", and `invalidateQueries({ queryKey: deletionsKey(org, project) })`;
  - error: `fieldErrors(caught).user_id` → under the field (and close the dialog); status `403` → dialog error "Only project admins can delete data."; status `409` → dialog error `problemMessage(caught, …)` (the server says "too many open deletions"); otherwise `problemMessage(caught, "Could not request the deletion.")` in the dialog.

## Page

`web/src/routes/$org/$project/settings/data.tsx`:

```tsx
export const Route = createFileRoute("/$org/$project/settings/data")({ component: ProjectDataPage });

function ProjectDataPage() {
  const { data } = useProject();
  if (!atLeast(data.role, "admin")) {
    return <Forbidden description="Only project admins can delete data." />;
  }
  return (
    <SettingsPage title="Data" description="Delete end-user data and follow deletion requests. Retention is under General.">
      <div className="flex max-w-3xl flex-col gap-8">
        <UserDeletionForm />
        <DeletionHistory />
      </div>
    </SettingsPage>
  );
}
```

Check `SettingsPage`'s props in `settings-page.tsx` and `Forbidden`'s in `forbidden.tsx`; follow `api-keys.tsx` for both.

`settings-nav.ts`: add `{ to: "/$org/$project/settings/data", label: "Data", adminOnly: true }` after `API keys`. Update `settings-nav.test.ts` if it lists the entries.

## Tests

`data.test.tsx` (`renderApp("/acme/support-bot/settings/data")`; override endpoints with `server.use(http.…)`; the project fixture's role is `owner`→ admin; use `projectAs(role)` as in `general.test.tsx` for other roles):

1. `shows_the_history`: the three fixture rows appear in the `Deletions` table with `Hidden, erasing`, `Erased`, `Failed`, and `user user-3`.
2. `admins_only`: with `projectAs("member")` the page shows the Forbidden text, and the settings sidebar has no `Data` link.
3. `requests_a_user_deletion`: type `user-7`, click `Delete user data…`; the dialog's confirm is disabled; type `user-7` in the dialog; confirm. The captured POST body equals `{kind: "user", user_id: "user-7"}` (D-L-10: assert the body in a `server.use` handler that answers `202` with `deletion.json`); the success text appears; the list is fetched again (count GET calls in a `server.use` handler: 2).
4. `validates_the_user_id`: empty → `Enter a user ID.`, no dialog; 256 ASCII characters → `Use at most 255 bytes.`.
5. `shows_server_errors`: POST answers `409` with `problemResponse(409, "conflict", "too many open deletions")` (check `problemResponse`'s signature) → the dialog shows `too many open deletions`; a `400` with `errors: [{field: "user_id", message: "user_id must be 1 to 255 bytes"}]` → that text under the field.
6. `polls_while_open`: with the default fixture (an open row) the list refetches; use `vi.useFakeTimers({ shouldAdvanceTime: true })`, advance 5 s, and expect a second GET. If fake timers fight React Query in this setup, assert instead that the query's `refetchInterval` option returns 5000 by rendering with a closed-only list (`server.use` returning only `applied` rows) and checking no second GET after advancing; pick the variant that is stable and say which in your report.
7. `empty_history`: `server.use` returns `{data: [], next_cursor: null}` → `No deletions yet`.

## Commands

```bash
export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
```
`cd web && pnpm vitest run src/routes/\$org/\$project/settings/data.test.tsx` while working; `just check` before the commit.

## Done when

- [ ] `/acme/support-bot/settings/data` works for admins against the mocks and the real server; non-admins get Forbidden and no nav entry.
- [ ] The dialog, label and page tests pass; `just check` is green; one commit `Add the Data settings tab`.
