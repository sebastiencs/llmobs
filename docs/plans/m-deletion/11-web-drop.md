# M11 — Web: delete project, delete organization, retention upsell

Read [README.md](README.md) first, especially § Design › Access, Retention (D-M-16) and decision D-M-12. M9's `TypeToConfirmDialog` exists; M5's `DELETE` routes and M6's `plan_required` code are in `schema.gen.ts`.

**Goal:**
1. **Project General** gets a "Danger zone" section: "Delete project" (org admins and owners only), type the project slug, `DELETE /api/v1/projects/{project}?confirm=<slug>`, then go to the org home.
2. **Org General** gets a "Danger zone" section: "Delete organization" (owners only), type the org slug, `DELETE /api/v1/orgs/{org}?confirm=<slug>`, then go to `/`.
3. **Project General**'s retention save shows the plan message when the server answers `403 plan_required` (D177, D-M-16; unreachable until track P, tested with a mock).

**Edit only:**
- `web/src/components/deletions/danger-zone.tsx` (new);
- `web/src/routes/$org/$project/settings/general.tsx` and `general.test.tsx`;
- `web/src/routes/$org/settings/general.tsx` and `general.test.tsx`.

**Commit:** `Add project and organization deletion to settings`

## Read first

- M9's `type-to-confirm-dialog.tsx`.
- Both `general.tsx` pages and their tests (`projectAs(role)` in the project test).
- `web/src/api/queries.ts`: `meQuery`, `projectListOptions`, `projectQuery`; `web/src/api/queries-settings.ts`: `orgQuery`.
- `fixtures/api/static/me.json`: `orgs[]` entries carry `slug` and `role` (the org role).
- `web/src/lib/roles.ts`: `atLeast`.

## Danger zone

`web/src/components/deletions/danger-zone.tsx`:

```tsx
export function DangerZone({
  title,
  description,
  buttonLabel,
  disabledReason,
  children,
  onOpen,
}: {
  title: string;           // "Delete project"
  description: ReactNode;
  buttonLabel: string;     // "Delete project…"
  disabledReason?: string; // set → button disabled with the reason (D-L-4)
  onOpen: () => void;
  children?: ReactNode;    // the dialog
})
```

A `<section aria-labelledby=…>` with a red-bordered box (`border-destructive/50`), heading `<h2>Danger zone</h2>`, a row with `title` (bold) and `description`, and a `Button variant="destructive"` wrapped in `DisabledReason`.

## Project General

- The org role: `const me = useQuery(meQuery)`; `const orgRole = me.data?.orgs.find((entry) => entry.slug === org)?.role;`. Deleting a project needs `atLeast(orgRole, "admin")` (spec 08: an org action; a project-admin override is not enough). Reason when not: `Only organization admins and owners can delete projects`.
- `DangerZone` below the form: title `Delete project`, description "Deletes every trace, span, score, media file and setting of this project. Data is erased within 24 hours. This cannot be undone.", button `Delete project…`.
- `TypeToConfirmDialog`: title `Delete project ${data.name}?`, `expected = data.slug`, `inputLabel = Type the project slug "${data.slug}" to confirm`, confirm `Delete project`.
- `$api.useMutation("delete", "/api/v1/projects/{project}")` with `{ params: { path: { project: ref }, query: { confirm: data.slug } } }`.
  - success → `queryClient.removeQueries({ queryKey: projectQuery(org, project).queryKey })`, `invalidateQueries({ queryKey: projectListOptions(org).queryKey })`, then `navigate({ to: "/$org", params: { org } })`;
  - `403` → dialog error "Only organization admins and owners can delete projects."; other → `problemMessage(caught, "Could not delete the project.")`.
- **Retention upsell.** In the existing `onError` of the save mutation, before the generic `403` branch: `if (problemOf(caught)?.code === "plan_required") { setError("Data retention management is part of the Pro plan."); return; }`.

## Org General

- `DangerZone` below the form: title `Delete organization`, description "Deletes the organization, all its projects and their data, its members and invitations. Data is erased within 24 hours. This cannot be undone.", button `Delete organization…`, `disabledReason` `Only owners can delete the organization` unless `data.role === "owner"` (`data` is the org query's answer, which carries the caller's role).
- `TypeToConfirmDialog`: title `Delete organization ${data.name}?`, `expected = data.slug`, input label `Type the organization slug "${data.slug}" to confirm`, confirm `Delete organization`.
- `$api.useMutation("delete", "/api/v1/orgs/{org}")` with `{ params: { path: { org }, query: { confirm: data.slug } } }`; success → `queryClient.removeQueries({ queryKey: orgQuery(org).queryKey })`, `invalidateQueries({ queryKey: meQuery.queryKey })`, `navigate({ to: "/" })`; errors as above with "Only owners can delete the organization."

## Tests

Project `general.test.tsx`:
1. owner (fixture `me.json` role `owner`): `Delete project…` enabled; the dialog's confirm stays disabled until `support-bot` is typed; confirm sends `DELETE /api/v1/projects/acme%2Fsupport-bot?confirm=support-bot` (capture the URL in `server.use(http.delete(…))`, answer `202` with `deletion-project.json`); the router ends on `/acme`.
2. org member with project role admin (`projectAs("admin")` plus a `me` override whose `acme` role is `member`): the button is disabled with the org-admin reason.
3. a `403` answer shows the org-admin message in the dialog.
4. retention save answered `403` `{code: "plan_required", …}` → `Data retention management is part of the Pro plan.`.

Org `general.test.tsx`:
5. owner: type `acme`, confirm → `DELETE /api/v1/orgs/acme?confirm=acme`; ends on `/`.
6. admin (`me`/org override with role `admin`): disabled with `Only owners can delete the organization`.

## Done when

- [ ] Both danger zones work against the mocks; roles disable them with the reason; the plan message shows for `plan_required`.
- [ ] The 6 tests pass; existing `general` tests still pass; `just check` is green; one commit `Add project and organization deletion to settings`.
