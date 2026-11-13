# L13–L17 — Settings

See [README.md](README.md) for the decisions, the shared building blocks, the conventions, **the Pitfalls** and the commands that apply to every phase. L1–L12 are done when these phases start: `web/src/lib/roles.ts`, `web/src/lib/redirect.ts`, the form primitives, `ConfirmDialog`, `DisabledReason`, `Forbidden`, `SecretDialog`, `SnippetTabs`, `problemResponse`, `web/src/test/fixtures.ts`, `authConfigQuery`, `TopBarLayout` and the shadcn `select`, `checkbox`, `radio-group` and `alert-dialog` components exist. Read them before you start.

**Order inside this file.** L13 builds the org side of the settings area. L15 adds the project side: its layout swap, its nav group and the sidebar's "Settings" entry. It comes later because the project settings pages first exist there. L14, L16 and L17 each add one page and one nav entry.

Facts every phase relies on:
- `/$org` (`web/src/routes/$org/route.tsx`) renders `TopBarLayout` (L10) around an `<Outlet />`. Its loader 404s when the org is not in `me.orgs`.
- `/$org/$project` (`web/src/routes/$org/$project/route.tsx`) loads the project and wraps the page in `ProjectSidebar`. It also provides `useProject()`, which returns `{ org, project, ref, data }` with `data` a `Schema<"Project">`. `ProjectSidebar` renders `SidebarInset`, which is a `<main>`.
- TanStack Router ranks a static segment above a dynamic one. So `/$org/settings/…` wins over `/$org/$project/…` for the path `/acme/settings/general`.
- The caller's org role is `me.orgs.find((o) => o.slug === org)?.role`. The caller's project role is `useProject().data.role`. In the fixtures Ada is `owner` on both. **Always compare roles with `atLeast(role, "admin")`, never with `=== "admin"`**, because an owner's project role is `owner`.
- Fixtures used here: `static/org.json` (`get_org` for `acme`), `org-members.json` (Ada owner, Bob member), `invites.json` (`guest@example.com`, member), `projects.json`, `project.json` (`support-bot`, retention 30, role owner), `api-keys.json` (one secret key `CI`, no `last_used_at`, no `expires_at`), `project-members.json` (Ada owner/admin, Bob member with override viewer), `auth-config.json` (`smtp_enabled: false`), `me.json`. Tests write every POST, PATCH, PUT and DELETE answer themselves with `server.use`.
- **Fixture imports in tests** (JSON imports, relative to the test file):
  - from `web/src/routes/$org/settings/*.test.tsx`: `import inviteCreated from "../../../../../fixtures/api/static/invite-created.json";` (5 × `..`);
  - from `web/src/routes/$org/$project/settings/*.test.tsx` and `web/src/routes/$org/$project/traces/index.test.tsx`: `import projectFixture from "../../../../../../fixtures/api/static/project.json";` and `import apiKeyCreated from "../../../../../../fixtures/api/static/api-key-created.json";` (6 × `..`);
  - from `web/src/routes/home.test.tsx`: `import projectsFixture from "../../../fixtures/api/static/projects.json";` (3 × `..`);
  - from `web/src/components/org-switcher.test.tsx`: `import meFixture from "../../../fixtures/api/static/me.json";` (3 × `..`).
- **Status line.** Each phase updates the README's first line as README § Conventions → Status line says.
- **Hooks come first.** In every component, every hook call (`useState`, `useQuery`, `useInfiniteQuery`, `useMutation`, `useQueryClient`, `useNavigate`, `useParams`, …) comes before the first `if (…) return …` or `if (…) throw …`. A hook placed after an early return changes the hook count between renders, and React fails with "Rendered more hooks than during the previous render". This covers `OrgHome`, `OrgMembers`, `OrgInvites`, the API keys list and the project members page.
- **Proving that no request was made.** The fixtures answer every GET in the manifest, so "there is no handler" proves nothing for a GET. A test that claims a GET was not sent installs a counting handler (`let calls = 0; server.use(http.get(path, () => { calls += 1; return HttpResponse.json({ data: [], next_cursor: null }); }))`) and asserts `calls === 0`. For a write (POST, PATCH, PUT, DELETE), having no handler is enough: an unhandled write fails the test.

## Shared pieces created in this file

| File | Export | Created in |
|---|---|---|
| `web/src/components/settings/settings-page.tsx` | `SettingsPage` | L13 |
| `web/src/components/settings/settings-nav.ts` | `orgSettingsNav` (L13), `projectSettingsNav` (L15), `isProjectSettingsPath` (L15) | L13, L15 |
| `web/src/components/settings/settings-sidebar.tsx` | `SettingsSidebar` | L13 (org group), L15 (project group) |
| `web/src/api/queries-settings.ts` | list `…Init` helpers and keys | L13, extended by L14–L17 |

`SettingsPage` props: `title: string; description?: string; actions?: ReactNode; children: ReactNode`. It renders a `<div>`, not a `<main>`: `SidebarInset` is already the page's `<main>` (README § Pitfalls).

```tsx
<div className="mx-auto flex w-full max-w-4xl flex-1 flex-col gap-6 overflow-auto px-4 py-6">
  <div className="flex items-start justify-between gap-4">
    <div className="flex flex-col gap-1">
      <h1 className="text-lg font-medium">{title}</h1>
      {description ? <p className="text-sm text-muted-foreground">{description}</p> : null}
    </div>
    {actions ? <div className="flex shrink-0 items-center gap-2">{actions}</div> : null}
  </div>
  {children}
</div>
```

Sections inside a page are `<section aria-labelledby={id} className="flex flex-col gap-3">` with `<h2 id={id} className="text-sm font-medium">`.

**Dialogs with forms** follow README § Pitfalls "Resetting a dialog's form". The form is an inner component rendered only while the dialog is open, so every open starts with empty fields and no error. When the mutation lives in the inner form, it is discarded on unmount. When it lives in the outer component (L16), the close handler calls `mutation.reset()`.

**Shared row mutations** (the role select in L14, the access select in L17) follow README § Pitfalls "Row actions and `mutate` callbacks". Their `onSuccess` and `onError` go in the third argument of `$api.useMutation`, and the row that is pending is `mutation.isPending && mutation.variables?.params.path.user === row.user.id`.

---

# L13 — Org settings shell, org general, new organization

**Edit only:**
- `web/src/components/settings/settings-page.tsx` (new);
- `web/src/components/settings/settings-nav.ts` (new);
- `web/src/components/settings/settings-sidebar.tsx` (new);
- `web/src/components/settings/new-org-dialog.tsx` (new);
- `web/src/api/queries-settings.ts` (new);
- `web/src/routes/$org/settings/route.tsx`, `index.tsx`, `general.tsx` (new);
- `web/src/routes/$org/settings/general.test.tsx` (new);
- `web/src/routes/$org/index.tsx` (the "Settings" button);
- `web/src/components/org-switcher.tsx` and `web/src/components/org-switcher.test.tsx` (new test file);
- the README's `Status:` line.

Run `pnpm gen:routes` after adding the route files. `web/src/routeTree.gen.ts` is git-ignored and not committed.

**Commit:** `web: add the settings area and org general settings`

## Steps

### 1. `web/src/api/queries-settings.ts`

```ts
import { $api } from "./client";

export function orgInit(org: string) {
  return { params: { path: { org } } };
}

export function orgQuery(org: string) {
  return $api.queryOptions("get", "/api/v1/orgs/{org}", orgInit(org));
}
```

Later phases add their list helpers to this file. Each phase gives the code.

### 2. `web/src/components/settings/settings-page.tsx`

Exactly the `SettingsPage` above.

### 3. `web/src/components/settings/settings-nav.ts`

```ts
export const orgSettingsNav = [{ to: "/$org/settings/general", label: "General" }] as const;
```

L14 appends Members. Do not add entries for pages that do not exist yet.

### 4. `web/src/components/settings/settings-sidebar.tsx`

```tsx
export function SettingsSidebar({ org, children }: { org: string; children: ReactNode })
```

Structure (all components from `@/components/ui/sidebar`):

```tsx
<SidebarProvider className="h-[calc(100dvh-40px)] overflow-hidden">
  <Sidebar collapsible="none" aria-label="Settings">
    <SidebarHeader>
      <SidebarMenu>
        <SidebarMenuItem>
          <SidebarMenuButton asChild>
            <Link to="/$org" params={{ org }}>
              <ArrowLeft />
              <span>Back to projects</span>
            </Link>
          </SidebarMenuButton>
        </SidebarMenuItem>
      </SidebarMenu>
    </SidebarHeader>
    <SidebarContent>
      <SidebarGroup>
        <SidebarGroupLabel>Organization</SidebarGroupLabel>
        <SidebarGroupContent>
          <SidebarMenu aria-label="Organization settings">
            {orgSettingsNav.map((item) => (
              <SidebarMenuItem key={item.to}>
                <SidebarMenuButton asChild isActive={Boolean(matchRoute({ to: item.to, params: { org } }))}>
                  <Link to={item.to} params={{ org }}>{item.label}</Link>
                </SidebarMenuButton>
              </SidebarMenuItem>
            ))}
          </SidebarMenu>
        </SidebarGroupContent>
      </SidebarGroup>
    </SidebarContent>
  </Sidebar>
  <SidebarInset className="min-h-0 overflow-hidden">{children}</SidebarInset>
</SidebarProvider>
```

- `matchRoute` is `useMatchRoute()` from `@tanstack/react-router`.
- `ArrowLeft` comes from `lucide-react`.
- `SidebarMenu` is a `<ul>` that passes `aria-label` through. Tests find the nav with `screen.getByRole("list", { name: "Organization settings" })`; L15 adds a second list, "Project settings", with links of the same names. Tests always query links `within` one of these lists.
- The settings sidebar does not collapse, and it does not read or write the `sidebar` localStorage key. The project sidebar keeps its own state.
- TanStack's `Link` sets `aria-current="page"` on the active link; the tests rely on it.

### 5. Routes

`web/src/routes/$org/settings/route.tsx`:

```tsx
import { createFileRoute, Outlet } from "@tanstack/react-router";
import { SettingsSidebar } from "@/components/settings/settings-sidebar";

export const Route = createFileRoute("/$org/settings")({ component: OrgSettingsLayout });

function OrgSettingsLayout() {
  const { org } = Route.useParams();
  return (
    <SettingsSidebar org={org}>
      <Outlet />
    </SettingsSidebar>
  );
}
```

`web/src/routes/$org/settings/index.tsx`:

```tsx
import { createFileRoute, redirect } from "@tanstack/react-router";

export const Route = createFileRoute("/$org/settings/")({
  beforeLoad: ({ params }) => {
    throw redirect({ to: "/$org/settings/general", params });
  },
});
```

`web/src/routes/$org/settings/general.tsx` is `createFileRoute("/$org/settings/general")`. Its loader is `({ context, params }) => context.queryClient.ensureQueryData(orgQuery(params.org))` and its component is `OrgGeneralPage`.

### 6. `OrgGeneralPage` (in `general.tsx`)

- Data: `const { data } = $api.useSuspenseQuery("get", "/api/v1/orgs/{org}", orgInit(org));`. It is an `OrgMembership`: `name`, `slug`, `role`.
- `canEdit = atLeast(data.role, "admin")`.
- State: `name` (initialised from `data.name`), `nameError` (string or undefined), `error` (string or undefined) and `saved` (boolean).
- `mutation = $api.useMutation("patch", "/api/v1/orgs/{org}")`. This is a single form, so callbacks go in `mutate`.
- `queryClient = useQueryClient()` from `@tanstack/react-query`.

Render `SettingsPage` with title `General` and description `Name and URL of this organization.`. Inside, one `<form noValidate aria-label="Organization" className="flex max-w-md flex-col gap-4">`:

1. `TextField` id `org-name`, label `Name`, `value={name}`, `required`, `maxLength={100}`, `disabled={!canEdit || mutation.isPending}`, `error={nameError}`. Its `onChange` sets `name` and sets `saved` to false.
2. `TextField` id `org-slug`, label `Slug`, `value={data.slug}`, `readOnly`, `onChange={() => {}}`, description `The slug is part of every URL in this organization and cannot be changed.`
3. `<FormError message={error} />` and `<FormSuccess message={saved ? "Saved" : undefined} />`.
4. `<DisabledReason reason={canEdit ? undefined : "Only admins and owners can rename the organization"}><SubmitButton pending={mutation.isPending} disabled={!canEdit}>Save</SubmitButton></DisabledReason>`.

Submit (`event.preventDefault()` first):
1. `setSaved(false)`, `setError(undefined)`, `setNameError(undefined)`, `mutation.reset()`.
2. `const trimmed = name.trim()`. When it is empty, set `nameError` to `Enter a name.` and stop.
3. `mutation.mutate({ params: { path: { org } }, body: { name: trimmed } }, { onSuccess, onError })`.
4. `onSuccess`:
   - `setSaved(true)`;
   - `void queryClient.invalidateQueries({ queryKey: orgQuery(org).queryKey })`;
   - `void queryClient.invalidateQueries({ queryKey: meQuery.queryKey })`, because the top bar shows the name from `me`.
5. `onError(error)`:
   - a `fieldErrors(error).name` → `setNameError(it)`;
   - otherwise `problemOf(error)?.status === 403` → `setError("Only admins and owners can rename the organization.")`;
   - otherwise `setError(problemMessage(error, "Could not save the organization."))`.

### 7. Org home "Settings" button — `web/src/routes/$org/index.tsx`

Replace the header block `<div className="flex flex-col gap-1">…</div>` with the block below. The buttons sit in their own `div`, so L15's "New project" lands beside "Settings":

```tsx
<div className="flex items-start justify-between gap-4">
  <div className="flex flex-col gap-1">
    <h1 className="text-2xl font-medium">{membership?.name ?? org}</h1>
    <h2 className="text-sm text-muted-foreground">Projects</h2>
  </div>
  <div className="flex shrink-0 items-center gap-2">
    <Button asChild variant="outline" size="sm">
      <Link to="/$org/settings/general" params={{ org }}>
        <Settings />
        Settings
      </Link>
    </Button>
  </div>
</div>
```

The `Settings` icon comes from `lucide-react`. Change nothing else in the file. L15 adds "New project" as the first child of the buttons `div`.

### 8. `web/src/components/settings/new-org-dialog.tsx`

```tsx
export function NewOrgDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void })
```

- A shadcn `Dialog` (`open`, `onOpenChange`). Its `DialogContent` holds `DialogHeader` with `DialogTitle` `New organization` and `DialogDescription` `You will be its owner.`, then `{open ? <NewOrgForm create={create} onDone={() => onOpenChange(false)} /> : null}`.
- The mutation lives in `NewOrgDialog` (`useCreateOrg()` in the same file, typed as `ReturnType<typeof useCreateOrg>`), so the dialog knows when a request is in flight: while `create.isPending` the root `onOpenChange(false)` is ignored and `DialogContent`'s `onEscapeKeyDown` / `onPointerDownOutside` call `event.preventDefault()`, so Esc, outside clicks and the X button leave the dialog open until the request settles. `NewOrgForm` receives `create` as a prop.
- `NewOrgForm({ create, onDone }: { create: CreateOrgMutation; onDone: () => void })`, in the same file, owns all the state:
  - `name`, `slug`, `nameError`, `slugError`, `error`;
  - `queryClient = useQueryClient()`, `navigate = useNavigate()`.
- `NewOrgForm` renders one `<form noValidate className="flex flex-col gap-4">` with:
  - `TextField` id `new-org-name`, label `Name`, `required`, `maxLength={100}`;
  - `TextField` id `new-org-slug`, label `Slug (optional)`, description `Lowercase letters, digits and dashes. Leave empty to derive it from the name.`, `maxLength={48}`;
  - `FormError`;
  - `DialogFooter` with a `Button variant="outline" type="button"` `Cancel` (calls `onDone()`) and `SubmitButton` `Create organization`.
- Submit:
  1. Clear the three errors and call `mutation.reset()`.
  2. Empty trimmed name → `nameError` `Enter a name.`; stop.
  3. Otherwise the body is `{ name: trimmed, ...(slug.trim() ? { slug: slug.trim() } : {}) }`.
- `onSuccess(data)` (an `async` function):
  1. `await queryClient.refetchQueries({ queryKey: meQuery.queryKey, type: "all" })` (README § Pitfalls: the `/$org` loader uses `ensureQueryData`, so `me` must list the new org before the navigation);
  2. `onDone()`;
  3. `void navigate({ to: "/$org", params: { org: data.slug } })`.
- `onError(error)`:
  - `problemOf(error)?.status === 409` → `slugError` `This slug is taken.`;
  - else, when `fieldErrors(error).slug === "reserved slug"` → `slugError` `This slug is reserved. Choose another.`;
  - else any other `slug` or `name` field error → shown under that field, as the server wrote it;
  - else `error` = `problemMessage(error, "Could not create the organization.")`.
- Closing in any way unmounts `NewOrgForm`, so fields and errors are dropped (README § Pitfalls). The mutation now outlives the form in `NewOrgDialog`, but `create.reset()` at the start of every submit clears its previous result. Do not add a reset effect.

### 9. `web/src/components/org-switcher.tsx`

- Add `const [creating, setCreating] = useState(false);`.
- After `</DropdownMenuRadioGroup>`, add `<DropdownMenuSeparator />` and:

  ```tsx
  <DropdownMenuItem onSelect={() => setCreating(true)}>
    <Plus />
    New organization
  </DropdownMenuItem>
  ```
- Render `<NewOrgDialog open={creating} onOpenChange={setCreating} />` as a sibling after `</DropdownMenu>`, and wrap both in a fragment.

## States

| Page | Loading | Error | Empty |
|---|---|---|---|
| `/$org/settings/general` | the route loader holds the page (the existing pending behaviour) | the route `ErrorState` | — |

## Keyboard

- Enter submits each form.
- The new-org dialog opens from the switcher with the keyboard: open the menu, arrow to "New organization", press Enter.
- Radix focuses the dialog's first field, and Esc closes it.

## Tests

`web/src/routes/$org/settings/general.test.tsx` imports `renderApp`, `problemResponse`, `ORG_ID`, `server`, `http` and `HttpResponse`, and `screen` and `within` from `@testing-library/react`. `orgNav()` is `within(screen.getByRole("list", { name: "Organization settings" }))`.

1. `it("redirects /acme/settings to general and marks it active")`:
   - `renderApp("/acme/settings")`;
   - `await screen.findByRole("heading", { name: "General", level: 1 })`;
   - the pathname is `/acme/settings/general`;
   - `orgNav().getByRole("link", { name: "General" })` has `aria-current="page"`;
   - the `Slug` input has value `acme` and is read-only;
   - the link `Back to projects` has `href` `/acme`.
2. `it("renames the organization")`:
   - capture the PATCH body with `server.use(http.patch("/api/v1/orgs/:org", async ({ request }) => { body = await request.json(); return HttpResponse.json({ id: ORG_ID, slug: "acme", name: "Acme Corp", role: "owner" }); }))`;
   - clear `Name`, type `Acme Corp`, click `Save`;
   - `body` equals `{ name: "Acme Corp" }`;
   - `await screen.findByRole("status")` has text `Saved`.
3. `it("refuses an empty name without calling the server")`:
   - clear `Name` and click `Save`;
   - `Enter a name.` is shown;
   - no PATCH handler is installed, so an unhandled request would fail the test.
4. `it("shows the server's field error")`:
   - PATCH answers `problemResponse(400, "bad_request", { errors: [{ field: "name", message: "name is too long" }] })`;
   - the text `name is too long` is shown, and the `Name` input has `aria-invalid="true"`.
5. `it("disables renaming for a member")`:
   - override `GET /api/auth/me` with `me.json`'s user and `orgs: [{ id: ORG_ID, slug: "acme", name: "Acme", role: "member" }]`;
   - override `GET /api/v1/orgs/:org` with `{ id: ORG_ID, slug: "acme", name: "Acme", role: "member" }`;
   - the `Name` input and the `Save` button are disabled;
   - `screen.getByRole("button", { name: "Save" }).parentElement` has `toHaveAccessibleDescription("Only admins and owners can rename the organization")`.
6. `it("opens settings from the org home")`: `renderApp("/acme")`, then click the link `Settings`; the pathname is `/acme/settings/general`.

`web/src/components/org-switcher.test.tsx` (`renderApp`, `server`, `http`, `HttpResponse`, `problemResponse`, `meFixture` as above):

1. `it("creates an organization and opens it")`:
   - Keep a mutable `orgs` array that starts as `[...meFixture.orgs]`, and `server.use(http.get("/api/auth/me", () => HttpResponse.json({ user: meFixture.user, orgs })))`.
   - `http.post("/api/v1/orgs", …)` captures the body, pushes `{ id: "01900000-0000-7000-8000-000000000011", slug: "beta", name: "Beta", role: "owner" }` into `orgs` and answers it with status 201.
   - `http.get("/api/v1/orgs/beta/projects", () => HttpResponse.json({ data: [], next_cursor: null }))`.
   - `renderApp("/acme")`; click the button `Acme`; click the menu item `New organization`. If the next click is refused, see README § Pitfalls (menus that open dialogs).
   - Type `Beta` in `Name` and click `Create organization`.
   - The body equals `{ name: "Beta" }`, the pathname becomes `/beta`, and `await screen.findByRole("heading", { name: "Beta" })`.
2. `it("sends the slug and shows a taken slug")`:
   - POST answers `problemResponse(409, "conflict", { detail: "slug taken" })`;
   - type the name `Acme two` and the slug `acme`, then submit;
   - the body is `{ name: "Acme two", slug: "acme" }`;
   - `This slug is taken.` is shown, and the dialog is still open.
3. `it("explains a reserved slug")`: POST answers `problemResponse(400, "bad_request", { errors: [{ field: "slug", message: "reserved slug" }] })`; `This slug is reserved. Choose another.` is shown.
4. `it("shows a generic failure and starts clean when reopened")`:
   - POST answers `problemResponse(500, "internal")`;
   - type `Beta` and submit;
   - `getByRole("alert")` has the text `Internal Server Error` (`problemMessage` falls back to the title);
   - click `Cancel`, and the dialog is gone;
   - open the switcher and `New organization` again: the `Name` input has value `""` and `queryByRole("alert")` is null.
5. `it("asks for a name without calling the server")`: no POST handler is installed (an unhandled write fails the test); open the dialog and click `Create organization` with an empty `Name`; `Enter a name.` is shown under `Name`, which has `aria-invalid="true"`.
6. `it("shows the server's name error under the field")`: POST answers `problemResponse(400, "bad_request", { errors: [{ field: "name", message: "must be 1 to 100 characters" }] })`; type `Beta` and submit; `must be 1 to 100 characters` is shown and the `Name` input has `aria-invalid="true"`.

## Commands

```bash
export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
```

```bash
cd web && pnpm gen:routes && cd ..
```

```bash
just fmt
```

```bash
just check-web
```

```bash
just check
```

## Done when

- `/acme/settings` redirects to `/acme/settings/general`, which shows the settings sidebar instead of any project sidebar.
- Renaming works for admins and owners, and is disabled with the reason for members and viewers.
- "New organization" in the org switcher creates an org and opens it.
- All the tests above pass, `just check-web` and `just check` are green, and there is one commit.

---

# L14 — Org members and invites

**Edit only:**
- `web/src/api/queries-settings.ts`;
- `web/src/components/settings/settings-nav.ts`;
- `web/src/components/settings/org-members.tsx` (new);
- `web/src/components/settings/org-invites.tsx` (new);
- `web/src/components/settings/invite-dialog.tsx` (new);
- `web/src/routes/$org/settings/members.tsx` and `members.test.tsx` (new);
- the README's `Status:` line.

Run `pnpm gen:routes` after adding the route file.

**Commit:** `web: add org members and invites settings`

## Steps

### 1. Query helpers — append to `web/src/api/queries-settings.ts`

```ts
export function orgMembersInit(org: string) {
  return { params: { path: { org }, query: { limit: 50 } } };
}
export function orgMembersKey(org: string) {
  return $api.queryOptions("get", "/api/v1/orgs/{org}/members", orgMembersInit(org)).queryKey;
}
export function orgInvitesInit(org: string) {
  return { params: { path: { org }, query: { limit: 50 } } };
}
export function orgInvitesKey(org: string) {
  return $api.queryOptions("get", "/api/v1/orgs/{org}/invites", orgInvitesInit(org)).queryKey;
}
```

Lists use `$api.useInfiniteQuery("get", path, init, { pageParamName: "cursor", initialPageParam: null, getNextPageParam: (page) => page.next_cursor ?? undefined })`. Invalidating with the key above also refreshes the infinite query, because TanStack Query matches by prefix.

### 2. Nav — `settings-nav.ts`

Append `{ to: "/$org/settings/members", label: "Members" }` to `orgSettingsNav`. Every role sees it.

### 3. Route — `web/src/routes/$org/settings/members.tsx`

`createFileRoute("/$org/settings/members")`, component `OrgMembersPage`:

```tsx
function OrgMembersPage() {
  const { org } = Route.useParams();
  const { data: me } = $api.useSuspenseQuery("get", "/api/auth/me");
  const role = me.orgs.find((item) => item.slug === org)?.role;
  return (
    <SettingsPage title="Members" description="People in this organization and their roles.">
      <OrgMembers org={org} actorRole={role} me={me.user.id} />
      {atLeast(role, "admin") ? <OrgInvites org={org} actorRole={role} /> : null}
    </SettingsPage>
  );
}
```

Viewers and members never request the invites list, which would answer `403`.

### 4. `OrgMembers` — `web/src/components/settings/org-members.tsx`

Props: `org: string; actorRole: OrgRole | undefined; me: string` (the caller's user id).

- Data: `$api.useInfiniteQuery("get", "/api/v1/orgs/{org}/members", orgMembersInit(org), …)`, and `rows = pages.flatMap((p) => p.data)`.
- Loading: three `Skeleton className="h-8 w-full"`. Error: `throw query.error`, which reaches the route's `ErrorState`. Empty cannot happen (the caller is a member); render the table anyway.
- No section heading: the page `h1` already says "Members", and the table's `aria-label` names it.
- An `actionError` state (`string | undefined`) is rendered with `FormError` above the table. It is cleared at the start of every action.
- `Table aria-label="Members"` with the columns `Name`, `Email`, `Role` and `Joined`, plus an empty header cell for actions (`<TableHead><span className="sr-only">Actions</span></TableHead>`):
  - **Name:** `member.user.name ?? "—"`, plus a `Badge variant="secondary"` `You` when `member.user.id === me`.
  - **Email:** `member.user.email`.
  - **Role:** a shadcn `Select`:
    - `value={member.role}`;
    - `onValueChange={(value) => changeRole(member, value)}`;
    - `SelectTrigger` with `aria-label={`Role of ${member.user.email}`}` and `className="h-7 w-28"`;
    - items: `grantableRoles(actorRole)`, plus `member.role` first when it is not in that list, each labelled with `roleLabel`;
    - `disabled` when `!canChangeRole(actorRole, member.role)`, or when `patch.isPending && patch.variables?.params.path.user === member.user.id`.

    The whole `<Select>` is wrapped in `DisabledReason` with this reason:
    - actor below admin → `Only admins and owners can change roles`;
    - target is an owner and the actor is not → `Only an owner can change an owner's role`;
    - otherwise no reason.
  - **Joined:** `<TimeText value={member.joined_at} style="datetime" />`.
  - **Actions:** a `DropdownMenu`. Its trigger is `<Button variant="ghost" size="icon-xs" aria-label={`Actions for ${member.user.email}`}><MoreHorizontal /></Button>`.
    - It has one item: `Leave organization` when `member.user.id === me`, else `Remove from organization`.
    - When `!canRemoveMember(actorRole, member.role, member.user.id === me)`, the trigger button is `disabled`. The **whole `<DropdownMenu>`** is then wrapped in `DisabledReason`, with `Only admins and owners can remove members` (actor below admin) or `Only an owner can remove an owner` (target owner).
    - Never wrap only the trigger: `DropdownMenuTrigger asChild` would hand its slot to the wrapper span.
- A "Load more" button (`variant="outline"`, `className="w-fit"`) while `hasNextPage`, as in `web/src/routes/$org/index.tsx`.

Role change (one mutation shared by all rows, callbacks on the hook):

```ts
const patch = $api.useMutation("patch", "/api/v1/orgs/{org}/members/{user}", {
  onSuccess: () => {
    void queryClient.invalidateQueries({ queryKey: orgMembersKey(org) });
    void queryClient.invalidateQueries({ queryKey: meQuery.queryKey });
    // The new org role changes project access. `$api` query keys are `[method, path, init?]`,
    // so these two prefixes hit every cached project and project-member entry.
    void queryClient.invalidateQueries({ queryKey: ["get", "/api/v1/projects/{project}"] });
    void queryClient.invalidateQueries({ queryKey: ["get", "/api/v1/projects/{project}/members"] });
  },
  onError: (error) => {
    const status = problemOf(error)?.status;
    setActionError(
      status === 409
        ? "An organization needs at least one owner. Make someone else an owner first."
        : status === 403
          ? "You can't give that role."
          : problemMessage(error, "Could not change the role."),
    );
  },
});

function changeRole(member: Schema<"OrgMember">, value: string) {
  const next = ORG_ROLES.find((role) => role === value);
  if (!next || next === member.role) {
    return;
  }
  setActionError(undefined);
  patch.mutate({ params: { path: { org, user: member.user.id } }, body: { role: next } });
}
```

Remove or leave:
- Selecting the menu item stores the member in `removeTarget` state (never cleared on close, so the dialog does not blank while Radix animates it out), sets `removeOpen` to true, and opens a `ConfirmDialog` (`open={removeOpen}`; `onOpenChange(false)` sets it back to false and resets the mutation; the next `onRemove` replaces the target):
  - Remove: title `Remove member?`, description `{name or email} loses access to every project in this organization.`, confirm label `Remove`, `destructive`.
  - Leave: title `Leave organization?`, description `You will lose access to every project in this organization.`, confirm label `Leave`, `destructive`.
- `remove = $api.useMutation("delete", "/api/v1/orgs/{org}/members/{user}")`. The dialog blocks the page while it is open, so its callbacks go in `remove.mutate(…, { onSuccess, onError })`.
- `onSuccess`:
  - Remove: close the dialog (`setRemoveOpen(false)` plus `remove.reset()`) and invalidate `orgMembersKey(org)`.
  - Leave: close the dialog the same way, then `await queryClient.refetchQueries({ queryKey: meQuery.queryKey, type: "all" })` (README § Pitfalls), then `void navigate({ to: "/" })`.
- `onError`: the dialog stays open and shows, through its `error` prop:
  - 409 → `An organization needs at least one owner. Make someone else an owner first.`;
  - else `problemMessage(error, "Could not remove the member.")`.
- The dialog's `onOpenChange(false)` sets `removeOpen` to false and calls `remove.reset()`; `removeTarget` keeps the member (the pattern of `disableTarget` in `routes/admin/users.tsx`).

### 5. `OrgInvites` — `web/src/components/settings/org-invites.tsx`

Props: `org: string; actorRole: OrgRole | undefined`. It is always admin or owner here.

- A `<section aria-labelledby="invites-heading">` whose heading row is `<div className="flex items-center justify-between">` with:
  - `<h2 id="invites-heading" className="text-sm font-medium">Pending invitations</h2>`;
  - a `Button size="sm"` `Invite member` (icon `UserPlus`), which sets `inviting` to true.
- `<InviteDialog org={org} actorRole={actorRole} open={inviting} onOpenChange={setInviting} />`.
- Data: `$api.useInfiniteQuery("get", "/api/v1/orgs/{org}/invites", orgInvitesInit(org), …)`. Loading: three skeletons. Error: throw.
- Empty: `<p className="text-sm text-muted-foreground">No pending invitations.</p>`.
- `Table aria-label="Pending invitations"` with these columns:
  - `Email`;
  - `Role` (`roleLabel`);
  - `Invited by` (`invite.invited_by?.name ?? invite.invited_by?.email ?? "—"`);
  - `Expires` (`TimeText` with `style="datetime"`);
  - the actions column.
- The actions dropdown (trigger `Actions for {email}`) has one item, `Revoke invitation`. It stores the invite in `revokeTarget` (never cleared on close, so the description keeps the email during the exit animation), sets `revokeOpen` to true, and opens a `ConfirmDialog`:
  - title `Revoke invitation?`, description `The link sent to {email} stops working.`, confirm `Revoke`, destructive;
  - the call is `$api.useMutation("delete", "/api/v1/orgs/{org}/invites/{id}")` with `params: { path: { org, id: invite.id } }`;
  - success: close and invalidate `orgInvitesKey(org)`;
  - error: `problemMessage(error, "Could not revoke the invitation.")` in the dialog;
  - closing resets the mutation.
- "Load more" as above.

### 6. `InviteDialog` — `web/src/components/settings/invite-dialog.tsx`

Props: `org: string; actorRole: OrgRole | undefined; open: boolean; onOpenChange: (open: boolean) => void`.

- The mutation lives in `InviteDialog` (`useCreateInvite(org)` in the same file, typed as `ReturnType<typeof useCreateInvite>`), not in the form: a close mid-request must not lose the accept link. Its third-argument options are `{ gcTime: 0 }` (required: `accept_url` carries the invitation token, the only response that contains it, and TanStack's mutation cache must not keep it — README § Privacy, D-L-5) plus an `onSuccess` that invalidates `orgInvitesKey(org)`. `onSuccess` sits on the hook so a close mid-request cannot drop the invalidation.
- `InviteDialog` closes through one `close()` helper that calls `create.reset()` and `onOpenChange(false)`. While `create.isPending` closing is blocked: the root `onOpenChange(false)` is ignored and `DialogContent`'s `onEscapeKeyDown` / `onPointerDownOutside` call `event.preventDefault()` (README § Pitfalls). Closing in any way (Done, Cancel, Esc, an outside click) unmounts `InviteForm` and resets the mutation, which drops the email, the role and the accept link.
- `InviteDialog` renders `<Dialog open={open} …><DialogContent …>{open && !create.data ? <InviteForm org={org} actorRole={actorRole} create={create} onCancel={close} /> : null}{open && create.data ? <InviteResult created={create.data} onDone={close} /> : null}</DialogContent></Dialog>`.
- `InviteForm` owns the state:
  - `email`;
  - `role` (`OrgRole`, initially `"member"`);
  - `emailError`, `error`.
- **Form view**, while `!mutation.data`:
  - `DialogHeader`: `DialogTitle` `Invite member` and `DialogDescription` `They get the role you choose when they accept.`.
  - A `<form noValidate className="flex flex-col gap-4">` with:
    - `TextField` id `invite-email`, `type="email"`, label `Email`, `required`, `autoComplete="off"`, `error={emailError}`;
    - the role: `<Label htmlFor="invite-role">Role</Label>` and a `Select` (`value={role}`, `onValueChange={(value) => { const next = ORG_ROLES.find((r) => r === value); if (next) setRole(next); }}`) whose `SelectTrigger` has `id="invite-role"`. Its items are `grantableRoles(actorRole)`, labelled with `roleLabel`;
    - `FormError`;
    - a `DialogFooter` with `Button variant="outline" type="button"` `Cancel` (calls `onDone()`) and `SubmitButton` `Send invitation`.
  - Submit:
    1. Clear the errors and call `create.reset()`.
    2. Empty trimmed email → `emailError` `Enter an email address.`; stop.
    3. Otherwise `create.mutate({ params: { path: { org } }, body: { email: trimmed, role } }, { onError })`; the invalidation is the hook's `onSuccess`.
  - `onError(error)`:
    - 409 → `emailError` `This person is already a member.`;
    - 403 → `error` `You can't invite with that role.`;
    - else a `fieldErrors(error).email` → under Email, as written (e.g. `invalid email`);
    - else a `role` field error → `error`;
    - else `error` = `problemMessage(error, "Could not create the invitation.")`.
- **Result view** (`InviteResult({ created, onDone })`, rendered by `InviteDialog` when `create.data` (a `CreatedInvite`) is set):
  - `config = useQuery(authConfigQuery)` for `smtp_enabled`; while it loads, SMTP counts as off.
  - `DialogHeader`: `DialogTitle` `Invitation created`. The `DialogDescription` is the paragraph, which has inline content only:
    - SMTP on: `We emailed an invitation to <strong>{created.invite.email}</strong>. You can also send them this link:`;
    - SMTP off: `Email is not configured on this server, so no email was sent. Send this link to <strong>{created.invite.email}</strong>:`.
  - Then a row with a read-only `Input` (`aria-label="Invitation link"`, `className="font-mono text-xs"`, value `created.accept_url`) and `<CopyButton value={created.accept_url} label="invitation link" />`.
  - Then `<p className="text-xs text-muted-foreground">The link expires <TimeText value={created.invite.expires_at} style="datetime" />.</p>`.
  - Footer: one `Button` `Done` that calls `onDone()`.

## States

| Part | Loading | Error | Empty |
|---|---|---|---|
| Members | 3 skeleton rows | route `ErrorState` | never |
| Invites | 3 skeleton rows | route `ErrorState` | `No pending invitations.` |

## Keyboard

- The role `Select` opens with Enter, Space or the arrow keys. The arrows move between options, and Enter picks one.
- Row action menus open from their icon button.
- In dialogs, Enter submits, Esc closes, and focus returns to the button that opened them.

## Tests — `web/src/routes/$org/settings/members.test.tsx`

Imports: `renderApp`, `problemResponse`, `ADA_ID`, `BOB_ID`, `ORG_ID`, `INVITE_ID`, `server`, `http`, `HttpResponse`, `screen`, `within` and `waitFor`, and `inviteCreated` (path in the intro).

Helpers at the top of the file:
- `const ADA = { id: ADA_ID, email: "ada@example.com", name: "Ada Example" }`;
- `const BOB = { id: BOB_ID, email: "bob@example.com", name: "Bob Example" }`;
- `function meAs(role: string, orgs = [{ id: ORG_ID, slug: "acme", name: "Acme", role }])`, which returns `http.get("/api/auth/me", () => HttpResponse.json({ user: { ...ADA, is_instance_admin: true }, orgs }))`;
- `const orgNav = () => within(screen.getByRole("list", { name: "Organization settings" }))`.

The first assertion of every test is `await screen.findByRole("table", { name: "Members" })` (README § Pitfalls, lists without a loader). Selects follow README § Pitfalls if a click is refused.

1. `it("lists members and pending invitations")`:
   - `renderApp("/acme/settings/members")`;
   - the `Members` table has rows for `ada@example.com` (with `You`) and `bob@example.com`;
   - `await screen.findByRole("table", { name: "Pending invitations" })` has a row with `guest@example.com`;
   - `orgNav().getByRole("link", { name: "Members" })` has `aria-current="page"`.
2. `it("changes a member's role")`:
   - PATCH `/api/v1/orgs/:org/members/:user` captures `params.user` and the body, and answers `{ user: BOB, role: "admin", joined_at: "2026-01-02T09:00:00.000000000Z" }`;
   - click the combobox `Role of bob@example.com`, then the option `Admin`;
   - the user param is `BOB_ID` and the body is `{ role: "admin" }`.
3. `it("explains the last-owner rule")`: PATCH answers `problemResponse(409, "conflict", { detail: "an org needs an owner" })`; change Ada's role to `Member`; `An organization needs at least one owner. Make someone else an owner first.` is shown.
4. `it("explains a role above the caller's")`: PATCH answers `problemResponse(403, "forbidden")`; change Bob's role to `Viewer`; `You can't give that role.` is shown.
5. `it("offers only roles up to the caller's")`:
   - `server.use(meAs("admin"))`;
   - open `Role of bob@example.com`: the options (`getAllByRole("option")` names) are exactly `Admin`, `Member`, `Viewer`;
   - press `{Escape}`;
   - the combobox `Role of ada@example.com` is disabled, and its `parentElement` has the accessible description `Only an owner can change an owner's role`.
6. `it("removes a member after confirmation")`:
   - DELETE `/api/v1/orgs/:org/members/:user` captures `params.user` and answers 204 (`new HttpResponse(null, { status: 204 })`);
   - open `Actions for bob@example.com` and click `Remove from organization`;
   - `await screen.findByRole("alertdialog", { name: "Remove member?" })`;
   - click `Remove` within it;
   - the user param is `BOB_ID`, and the alertdialog is gone (`waitFor`).
7. `it("keeps the dialog open when the last owner tries to leave")`:
   - DELETE answers `problemResponse(409, "conflict", { detail: "an org needs an owner" })`;
   - open `Actions for ada@example.com`, then `Leave organization`, then `Leave`;
   - the alertdialog `Leave organization?` is still open, and `within(alertdialog).getByRole("alert")` has `An organization needs at least one owner. Make someone else an owner first.`.
8. `it("lets a member leave and goes home")`:
   - keep a mutable `let orgs = [{ id: ORG_ID, slug: "acme", name: "Acme", role: "member" }]` and `server.use(http.get("/api/auth/me", () => HttpResponse.json({ user: { ...ADA, is_instance_admin: true }, orgs })))`;
   - the DELETE handler sets `orgs = []` and answers 204;
   - open `Actions for ada@example.com`, click `Leave organization`, then `Leave`;
   - install `let invitesCalls = 0; server.use(http.get("/api/v1/orgs/:org/invites", () => { invitesCalls += 1; return HttpResponse.json({ data: [], next_cursor: null }); }))` before `renderApp`;
   - the pathname becomes `/` (`waitFor`);
   - `invitesCalls` is `0`: a member never requests the invites list.
9. `it("disables actions a viewer cannot take")`:
   - `server.use(meAs("viewer"))` and the same counting invites handler as test 8;
   - `Role of bob@example.com` is disabled, and its `parentElement` has the description `Only admins and owners can change roles`;
   - the button `Actions for bob@example.com` is disabled, and its `parentElement` has the description `Only admins and owners can remove members`;
   - `queryByRole("heading", { name: "Pending invitations" })` and `queryByRole("button", { name: "Invite member" })` are null;
   - `invitesCalls` is `0`.
10. `it("loads more members")`:
    - GET `/api/v1/orgs/:org/members` answers `{ data: [Ada as owner], next_cursor: "c2" }` without `cursor`, and `{ data: [{ user: { id: "01900000-0000-7000-8000-000000000103", email: "carol@example.com", name: "Carol Example" }, role: "viewer", joined_at: "2026-01-03T09:00:00.000000000Z" }], next_cursor: null }` with `cursor=c2`;
    - click `Load more`;
    - `await screen.findByRole("row", { name: /carol@example.com/ })`;
    - `Load more` is gone.
11. `it("invites a member and shows the link without SMTP")`:
    - POST `/api/v1/orgs/:org/invites` captures the body and answers `HttpResponse.json(inviteCreated, { status: 201 })`;
    - click `Invite member`, type `guest@example.com` in `Email`, click `Send invitation`;
    - the body is `{ email: "guest@example.com", role: "member" }`;
    - `await screen.findByText(/^Email is not configured on this server/)`;
    - the textbox `Invitation link` has value `http://localhost:8080/invite/EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXA`;
    - click `Done`, and `queryByRole("dialog")` is null.
12. `it("invites with another role")`:
    - same POST handler;
    - open the dialog, type `guest@example.com`, open the combobox `Role` and pick `Admin`, then submit;
    - the body is `{ email: "guest@example.com", role: "admin" }`.
13. `it("says the invitation was emailed when SMTP is on")`:
    - also override `GET /api/auth/config` with `{ ...authConfigFixture, smtp_enabled: true }` (import `auth-config.json` like `inviteCreated`);
    - after submitting, `await screen.findByText(/^We emailed an invitation to/)`.
14. `it("shows an existing member")`: POST answers `problemResponse(409, "conflict", { detail: "already a member" })`; `This person is already a member.` is shown, and the `Email` input has `aria-invalid="true"`.
15. `it("shows the server's email error")`: POST answers `problemResponse(400, "bad_request", { errors: [{ field: "email", message: "invalid email" }] })`; type `nope` and submit; `invalid email` is shown under `Email`.
16. `it("drops the link when the dialog closes")`:
    - destructure `queryClient` from `renderApp`;
    - POST answers `inviteCreated` (201);
    - submit, then press `{Escape}`; the dialog is gone;
    - `await waitFor(() => expect(JSON.stringify(queryClient.getMutationCache().getAll().map((m) => m.state.data ?? null))).not.toContain("EXAMPLE0"))`;
    - click `Invite member` again: the `Email` input has value `""`, and `queryByRole("textbox", { name: "Invitation link" })` is null.
17. `it("revokes an invitation")`:
    - DELETE `/api/v1/orgs/:org/invites/:id` captures `String(params.id)` and answers 204;
    - open `Actions for guest@example.com`, then `Revoke invitation`, then `Revoke`;
    - the id is `INVITE_ID`.
18. `it("shows a failed removal in the dialog")`: DELETE `/api/v1/orgs/:org/members/:user` answers `problemResponse(500, "internal")`; open `Actions for bob@example.com`, `Remove from organization`, `Remove`; `within(await screen.findByRole("alertdialog", { name: "Remove member?" })).getByRole("alert")` has the text `Internal Server Error` (`problemMessage` falls back to the title); the dialog stays open.
19. `it("shows a failed revoke in the dialog")`: DELETE `/api/v1/orgs/:org/invites/:id` answers `HttpResponse.error()` (a network failure, so the error is not a problem); open `Actions for guest@example.com`, `Revoke invitation`, `Revoke`; `within(await screen.findByRole("alertdialog", { name: "Revoke invitation?" })).getByRole("alert")` has the text `Could not revoke the invitation.`; the dialog stays open.
20. `it("explains an invitation above the caller's role")`: POST `/api/v1/orgs/:org/invites` answers `problemResponse(403, "forbidden")`; open the dialog, type `guest@example.com`, submit; `getByRole("alert")` has `You can't invite with that role.`.
21. `it("offers an admin only roles up to admin when inviting")`: `server.use(meAs("admin"))`; click `Invite member`, open the combobox `Role`; the option names are exactly `Admin`, `Member`, `Viewer`.
22. `it("keeps the invite dialog open while the request runs")`: POST `/api/v1/orgs/:org/invites` returns a promise the test resolves by hand; open the dialog, type `guest@example.com`, submit, press `{Escape}` — the dialog `Invite member` is still open; resolve the POST with `inviteCreated` (201); the dialog `Invitation created` shows the link.

## Commands

The same as L13.

## Done when

- `/acme/settings/members` lists members, and, for admins and owners, pending invitations.
- Roles change, and members are removed or leave.
- Invitations are created and revoked. The link is always shown, with copy that depends on SMTP.
- Viewers see disabled actions with reasons.
- The tests pass, the checks are green, and there is one commit.

---

# L15 — Project settings shell, project general, new project

**Edit only:**
- `web/src/components/settings/settings-nav.ts`, `settings-nav.test.ts` (new);
- `web/src/components/settings/settings-sidebar.tsx`;
- `web/src/components/settings/new-project-dialog.tsx` (new);
- `web/src/components/shell/project-sidebar.tsx` (footer entry);
- `web/src/routes/$org/$project/route.tsx` (sidebar swap);
- `web/src/routes/$org/$project/settings/index.tsx`, `general.tsx`, `general.test.tsx` (new);
- `web/src/routes/$org/index.tsx` and `web/src/routes/home.test.tsx` (the "New project" button and its tests);
- the README's `Status:` line.

Run `pnpm gen:routes` after adding the route files.

**Commit:** `web: add project general settings and project creation`

## Steps

### 1. `settings-nav.ts`

```ts
export const projectSettingsNav = [
  { to: "/$org/$project/settings/general", label: "General", adminOnly: false },
] as const;

/** True on `/<org>/<project>/settings` and every path below it. Slugs are `[a-z0-9-]`. */
export function isProjectSettingsPath(pathname: string, org: string, project: string): boolean {
  const base = `/${org}/${project}/settings`;
  return pathname === base || pathname.startsWith(`${base}/`);
}
```

L16 and L17 append `api-keys` (`adminOnly: true`) and `members` (`adminOnly: false`).

### 2. `SettingsSidebar` gains the project group

The new props are `{ org: string; project?: { slug: string; role: OrgRole | undefined }; children: ReactNode }`.

- With `project`:
  - the header link is `Back to project` → `<Link to="/$org/$project/traces" params={{ org, project: project.slug }}>`;
  - a first `SidebarGroup`, labelled `Project`, has a `<SidebarMenu aria-label="Project settings">`. It lists `projectSettingsNav`, skipping an item with `adminOnly` when `!atLeast(project.role, "admin")`. Its links use `params={{ org, project: project.slug }}`, and `isActive` uses `matchRoute({ to: item.to, params: { org, project: project.slug } })`.
- Without `project`: as in L13.
- The `Organization` group (`aria-label="Organization settings"`) is always shown, after the project group.

### 3. Sidebar swap — `web/src/routes/$org/$project/route.tsx`

In `ProjectLayout`:

```tsx
const pathname = useRouterState({ select: (state) => state.location.pathname });
const inSettings = isProjectSettingsPath(pathname, org, project);
…
<ProjectProvider value={…}>
  <RememberOpenTrace />
  {inSettings ? (
    <SettingsSidebar org={org} project={{ slug: project, role: data.role }}>
      <Outlet />
    </SettingsSidebar>
  ) : (
    <ProjectSidebar>
      <Outlet />
    </ProjectSidebar>
  )}
</ProjectProvider>
```

`useRouterState` is already imported in that file.

### 4. "Settings" entry — `web/src/components/shell/project-sidebar.tsx`

After `</SidebarContent>` add:

```tsx
<SidebarFooter>
  <SidebarMenu>
    <SidebarMenuItem>
      <SidebarMenuButton asChild tooltip="Settings">
        <Link to="/$org/$project/settings/general" params={{ org, project }}>
          <Settings />
          <span>Settings</span>
        </Link>
      </SidebarMenuButton>
    </SidebarMenuItem>
  </SidebarMenu>
</SidebarFooter>
```

Import `SidebarFooter` from `@/components/ui/sidebar`, and `Settings` from `lucide-react`. Every role sees it, because viewers can read General.

### 5. Routes

- `web/src/routes/$org/$project/settings/index.tsx`: `createFileRoute("/$org/$project/settings/")` with `beforeLoad: ({ params }) => { throw redirect({ to: "/$org/$project/settings/general", params }); }`.
- `web/src/routes/$org/$project/settings/general.tsx`: `createFileRoute("/$org/$project/settings/general")`, component `ProjectGeneralPage`.

Do not create a `settings/route.tsx`; `ProjectLayout` does the layout.

### 6. `ProjectGeneralPage`

- `const { org, project, ref, data } = useProject();`
- `canEdit = atLeast(data.role, "admin")`. When it is false, the reason is `Only project admins can change these settings`.
- State:
  - `name` = `data.name`;
  - `forever` = `data.retention_days === undefined`;
  - `days` = `data.retention_days === undefined ? "" : String(data.retention_days)`;
  - `nameError`, `daysError`, `error` and `saved`.
- `mutation = $api.useMutation("patch", "/api/v1/projects/{project}")`; `queryClient = useQueryClient()`.

Render `SettingsPage` with title `General` and description `Name, URL and data retention of this project.`. Inside, one `<form noValidate aria-label="Project" className="flex max-w-md flex-col gap-4">`:

1. `TextField` id `project-name`, label `Name`, `required`, `maxLength={100}`, `error={nameError}`, disabled when `!canEdit` or pending.
2. `TextField` id `project-slug`, label `Slug`, value `data.slug`, `readOnly`, `onChange={() => {}}`, description `The slug is part of every URL of this project and cannot be changed.`
3. `TextField` id `project-retention`, `type="number"`, label `Retention (days)`, value `days`, error `daysError`, disabled when `forever`, `!canEdit` or pending. Its description is `Lowering retention drops older data right away. Deleted data cannot be recovered.`
4. A row `<div className="flex items-center gap-2">` with `<Checkbox id="project-retention-forever" checked={forever} onCheckedChange={(value) => setForever(value === true)} disabled={!canEdit || mutation.isPending} />` and `<Label htmlFor="project-retention-forever">Keep data forever</Label>`.
5. `FormError`, `FormSuccess` (`Saved`), and `<DisabledReason reason={canEdit ? undefined : "Only project admins can change these settings"}><SubmitButton pending={mutation.isPending} disabled={!canEdit}>Save</SubmitButton></DisabledReason>`.

Every field's `onChange` sets `saved` to false.

Submit:
1. Clear the errors and `saved`, and call `mutation.reset()`.
2. Empty trimmed name → `nameError` `Enter a name.`.
3. When not `forever`: `days` must match `/^\d+$/` and be 1–36500, else `daysError` `Enter a whole number of days from 1 to 36500.`.
4. Stop if any error is set.
5. Build the body. The generated type has `retention_days?: number`, but the server takes `null` to mean "forever" (spec 09; README contract gap **CG5**). Build it with this function in `general.tsx`:

   ```ts
   type ProjectPatch = Schema<"UpdateProjectRequest">;
   /** `retention_days: null` keeps data forever. The OpenAPI type omits the null (contract gap CG5). */
   function projectPatch(name: string, retentionDays: number | null): ProjectPatch {
     return { name, retention_days: retentionDays } as unknown as ProjectPatch;
   }
   ```

   Call `mutation.mutate({ params: { path: { project: ref } }, body: projectPatch(trimmed, forever ? null : Number(days)) }, { onSuccess, onError })`.
6. `onSuccess`: `setSaved(true)`; invalidate `projectQuery(org, project).queryKey` and `projectListOptions(org).queryKey`, both from `@/api/queries`.
7. `onError(error)`:
   - `fieldErrors(error).name` → under Name, and `retention_days` → under Retention;
   - otherwise 403 → `error` `Only project admins can change these settings.`;
   - otherwise `error` = `problemMessage(error, "Could not save the project.")`.

### 7. `NewProjectDialog` — `web/src/components/settings/new-project-dialog.tsx`

Props: `org: string; open: boolean; onOpenChange: (open: boolean) => void`. The structure is the same as `NewOrgDialog` (L13 step 8):
- `Dialog`;
- `DialogHeader` with `DialogTitle` `New project` and `DialogDescription` `Projects keep their traces, keys and settings apart.`;
- `{open ? <NewProjectForm org={org} create={create} onDone={() => onOpenChange(false)} /> : null}`.

As in L13 step 8, the mutation lives in the dialog component (`useCreateProject()`), and Esc, outside clicks and the X button are ignored while `create.isPending`.

`NewProjectForm` owns its state and receives `create`:
- Fields:
  - `TextField` id `new-project-name`, label `Name`, `required`, `maxLength={100}`;
  - `TextField` id `new-project-slug`, label `Slug (optional)`, `maxLength={48}`, description `Lowercase letters, digits and dashes. Leave empty to derive it from the name.`;
  - `TextField` id `new-project-retention`, `type="number"`, label `Retention in days (optional)`, description `Leave empty to keep data forever.`.
- `FormError`; a footer with `Cancel` (calls `onDone()`) and `SubmitButton` `Create project`.
- `mutation = $api.useMutation("post", "/api/v1/orgs/{org}/projects")`, in `NewProjectDialog` (see above).
- Submit:
  1. Clear the errors and call `mutation.reset()`.
  2. Name required: `Enter a name.`.
  3. Retention, when not empty, is validated as in step 6 (`Enter a whole number of days from 1 to 36500.`).
  4. Stop on any error.
  5. Build the typed body:

     ```ts
     const body: Schema<"CreateProjectRequest"> = { name: trimmed };
     if (slug.trim()) body.slug = slug.trim();
     if (retention.trim()) body.retention_days = Number(retention.trim());
     ```
- `onSuccess(data)`: invalidate `projectListOptions(org).queryKey`; `onDone()`; `void navigate({ to: "/$org/$project/traces", params: { org, project: data.slug } })`.
- `onError(error)`:
  - 409 → slug error `This slug is taken.`;
  - 403 → `error` `Only admins and owners can create projects.`;
  - `fieldErrors` for `name`, `slug` and `retention_days` → under their fields;
  - else `problemMessage(error, "Could not create the project.")`.

### 8. Org home — `web/src/routes/$org/index.tsx`

As the first child of the buttons `div` from L13 step 7, before the `Settings` button, add:

```tsx
<DisabledReason reason={atLeast(membership?.role, "admin") ? undefined : "Only admins and owners can create projects"}>
  <Button size="sm" disabled={!atLeast(membership?.role, "admin")} onClick={() => setCreating(true)}>
    <Plus />
    New project
  </Button>
</DisabledReason>
```

- Add `const [creating, setCreating] = useState(false);` at the **top** of `OrgHome`, on the line right after `const { org } = Route.useParams();` and before `const { data: me } = …` and `const projects = …`. `OrgHome` already has `if (projects.error) throw …` and `if (projects.isPending) return …` in the middle of the function; a `useState` placed after them breaks the hook order (this file's intro, "Hooks come first").
- Put `<NewProjectDialog org={org} open={creating} onOpenChange={setCreating} />` at the end of `main`.
- The existing `EmptyState` `No projects yet` gets `description="Create a project to start sending traces."`.

## States

| Page | Loading | Error | Empty |
|---|---|---|---|
| Project general | the project layout's loader | route `ErrorState`; a 404 project is `NotFound` | — |

## Keyboard

Enter submits; Space toggles the checkbox; dialogs as in L13.

## Tests

`web/src/components/settings/settings-nav.test.ts`:
- `isProjectSettingsPath("/acme/bot/settings", "acme", "bot")` and `isProjectSettingsPath("/acme/bot/settings/general", "acme", "bot")` are true;
- `("/acme/bot/settingsx", …)`, `("/acme/bot/traces", …)` and `("/acme/other/settings", "acme", "bot")` are false.

`web/src/routes/$org/$project/settings/general.test.tsx`. The project fixture is Support bot, retention 30, role owner. Import `projectFixture` as the intro says. Helpers:
- `const projectNav = () => within(screen.getByRole("list", { name: "Project settings" }))`;
- `function projectAs(role: string) { return http.get("/api/v1/projects/:project", () => HttpResponse.json({ ...projectFixture, role })); }`.

1. `it("replaces the project sidebar with the settings sidebar")`:
   - `renderApp("/acme/support-bot/settings")`;
   - `await screen.findByRole("heading", { name: "General", level: 1 })`;
   - the pathname is `/acme/support-bot/settings/general`;
   - the link `Back to project` has `href` `/acme/support-bot/traces`;
   - `screen.getByRole("list", { name: "Project settings" })` and `screen.getByRole("list", { name: "Organization settings" })` both exist;
   - `projectNav().getByRole("link", { name: "General" })` has `aria-current="page"`;
   - `screen.queryByRole("link", { name: "Traces" })` is null.
2. `it("opens settings from the project sidebar")`: `renderApp("/acme/support-bot/traces")`; click the link `Settings`; the pathname is `/acme/support-bot/settings/general`.
3. `it("shows Settings in the project sidebar for a viewer")`: `server.use(projectAs("viewer"))`; `renderApp("/acme/support-bot/traces")`; `await screen.findByRole("link", { name: "Settings" })` exists.
4. `it("saves the name and retention")`:
   - PATCH `/api/v1/projects/:project` captures the body and answers `{ ...projectFixture, name: "Support", retention_days: 7 }`;
   - change `Name` to `Support` and `Retention (days)` to `7`, then click `Save`;
   - the body is `{ name: "Support", retention_days: 7 }`;
   - `await screen.findByRole("status")` has text `Saved`.
5. `it("sends null to keep data forever")`:
   - check `Keep data forever`; the retention input is disabled;
   - click `Save`;
   - the body is `{ name: "Support bot", retention_days: null }`.
6. `it("rejects an invalid retention without calling the server")`: clear the retention and type `0`; click `Save`; `Enter a whole number of days from 1 to 36500.` is shown. No PATCH handler is installed.
7. `it("shows a refusal from the server")`: PATCH answers `problemResponse(403, "forbidden")`; click `Save`; `getByRole("alert")` has `Only project admins can change these settings.`.
8. `it("shows the server's field errors under their fields")`: PATCH answers `problemResponse(400, "bad_request", { errors: [{ field: "name", message: "must be 1 to 100 characters" }, { field: "retention_days", message: "must be 1 to 36500" }] })`; click `Save`; `must be 1 to 100 characters` is shown and `Name` has `aria-invalid="true"`; `must be 1 to 36500` is shown and `Retention (days)` has `aria-invalid="true"`.
9. `it("is read-only for a member")`:
   - `server.use(projectAs("member"))`;
   - `Name` and `Save` are disabled;
   - `Save`'s `parentElement` has the description `Only project admins can change these settings`.

`web/src/routes/home.test.tsx` (append; import `projectsFixture` as the intro says, plus `problemResponse`, `server`, `http` and `HttpResponse` if not already imported):

10. `it("creates a project and opens its traces")`:
   - POST `/api/v1/orgs/:org/projects` captures the body and answers 201 with `{ ...projectsFixture.data[0], slug: "new-bot", name: "New bot" }`;
   - `GET /api/v1/projects/:project` (any project) answers that same object, and `GET /api/v1/projects/:project/traces` answers `{ data: [], next_cursor: null }`;
   - `renderApp("/acme")`, click `New project`, type `New bot` in `Name`, click `Create project`;
   - the body is `{ name: "New bot" }`;
   - the pathname becomes `/acme/new-bot/traces` (`waitFor`).
11. `it("sends the slug and retention")`: same POST handler; type `New bot`, slug `bot-2` and retention `14`; the body is `{ name: "New bot", slug: "bot-2", retention_days: 14 }`.
12. `it("shows a taken slug and a refusal")`:
    - POST answers `problemResponse(409, "conflict", { detail: "slug taken" })`; `This slug is taken.` is shown;
    - then `server.use` POST answering `problemResponse(403, "forbidden")` and submit again; `Only admins and owners can create projects.` is shown.
13. `it("rejects a bad retention without calling the server")`: type `New bot` and retention `0`, then submit; `Enter a whole number of days from 1 to 36500.` is shown. No POST handler is installed.
14. `it("asks for a project name without calling the server")`: no POST handler is installed; click `New project`, then `Create project` with an empty `Name`; `Enter a name.` is shown and `Name` has `aria-invalid="true"`.
15. `it("disables New project for a member")`: override `GET /api/auth/me` with Ada as `member` of acme; the `New project` button is disabled, and its `parentElement` has the description `Only admins and owners can create projects`.

## Commands

The same as L13.

## Done when

- Project settings open from the sidebar, and the settings sidebar replaces the project sidebar there.
- Name and retention save, including "forever" as `null`.
- Members and viewers see a read-only form with the reason.
- Admins create projects from the org home.
- The tests pass, the checks are green, and there is one commit. The report names CG5.

---

# L16 — API keys

**Edit only:**
- `web/src/api/queries-settings.ts`;
- `web/src/components/settings/settings-nav.ts`;
- `web/src/components/settings/create-key-dialog.tsx` (new);
- `web/src/routes/$org/$project/settings/api-keys.tsx` and `api-keys.test.tsx` (new);
- `web/src/components/onboarding/panel.tsx`, `snippets.ts`, `onboarding.test.ts` (the key note);
- `web/src/routes/$org/$project/traces/index.test.tsx` (two new tests for the key note);
- the README's `Status:` line.

Run `pnpm gen:routes` after adding the route file.

**Commit:** `web: add project API keys settings`

## Steps

### 1. Query helpers

Append to `queries-settings.ts`, and import `projectRef` from `./project-ref`:

```ts
export function apiKeysInit(org: string, project: string) {
  return { params: { path: { project: projectRef(org, project) }, query: { limit: 50 } } };
}
export function apiKeysKey(org: string, project: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/api-keys", apiKeysInit(org, project)).queryKey;
}
```

### 2. Nav

Append `{ to: "/$org/$project/settings/api-keys", label: "API keys", adminOnly: true }` to `projectSettingsNav`, after General.

### 3. Page — `web/src/routes/$org/$project/settings/api-keys.tsx`

`createFileRoute("/$org/$project/settings/api-keys")`, component `ApiKeysPage`.

- `const { org, project, data } = useProject();`
- When `!atLeast(data.role, "admin")`: render `<Forbidden description="Only project admins can manage API keys." />` and make **no** list request.
- Otherwise render `ApiKeysList` (same file):
  - `SettingsPage` with title `API keys`, description `Keys let SDKs and OpenTelemetry exporters send data to this project.`, and as actions a `Button size="sm"` `Create key` (icon `Plus`). The button sets `creating` to true. Render `<CreateKeyDialog org={org} project={project} open={creating} onOpenChange={setCreating} />` in the page.
  - Data: `$api.useInfiniteQuery("get", "/api/v1/projects/{project}/api-keys", apiKeysInit(org, project), …)`.
  - Loading: three skeletons. Error: `problemOf(error)?.status === 403` → `<Forbidden description="Only project admins can manage API keys." />`; anything else → `throw`.
  - Empty: `<EmptyState icon={KeyRound} title="No API keys yet" description="Create a secret key to send traces." />`.
  - `Table aria-label="API keys"`, with these columns:

    | Header | Cell |
    |---|---|
    | `Name` | `key.name ?? "—"` |
    | `Key` | `<span className="font-mono">{key.prefix}…</span>` |
    | `Kind` | `Badge variant="secondary"`: `Secret` or `Public` |
    | `Scopes` | `key.scopes.join(", ")` |
    | `Created` | `TimeText datetime` |
    | `Last used` | `TimeText datetime`, or `Never` |
    | `Expires` | `TimeText datetime`, or `Never` |
    | (sr-only `Actions`) | dropdown, trigger label `Actions for {key.name ?? key.prefix}`, one item `Revoke` |

  - Revoke opens a `ConfirmDialog`:
    - title `Revoke key?`, description `Requests that use {prefix}… fail at once. This cannot be undone.`, confirm `Revoke key`, destructive;
    - the call is `$api.useMutation("delete", "/api/v1/projects/{project}/api-keys/{id}")` with `params: { path: { project: projectRef(org, project), id } }`;
    - success: close and invalidate `apiKeysKey`;
    - error: `problemMessage(error, "Could not revoke the key.")` in the dialog;
    - closing resets the mutation.
  - "Load more" while `hasNextPage`.

### 4. `CreateKeyDialog` — `web/src/components/settings/create-key-dialog.tsx`

Props: `org: string; project: string; open: boolean; onOpenChange: (open: boolean) => void`.

Constants in the file:

```ts
const EXPIRY = [
  { value: "never", label: "Never", days: undefined },
  { value: "30", label: "In 30 days", days: 30 },
  { value: "90", label: "In 90 days", days: 90 },
  { value: "365", label: "In 1 year", days: 365 },
] as const;

/** RFC 3339 for `now + days`. */
export function expiresAt(now: number, days: number): string {
  return new Date(now + days * 86_400_000).toISOString();
}
```

Structure. The mutation lives in `CreateKeyDialog`, so the secret outlives the form:

```tsx
export function CreateKeyDialog({ org, project, open, onOpenChange }: Props) {
  const create = useCreateKey();
  const close = () => {
    create.reset();
    onOpenChange(false);
  };
  return (
    <>
      <Dialog
        open={open && !create.data}
        onOpenChange={(next) => {
          // While the request runs, the key may already exist on the server: closing now
          // would reset the mutation and lose the secret. Esc and outside clicks wait.
          if (!next && !create.isPending) close();
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Create API key</DialogTitle>
            <DialogDescription>Choose who uses the key and how long it works.</DialogDescription>
          </DialogHeader>
          {open && !create.data ? <CreateKeyForm org={org} project={project} create={create} onCancel={close} /> : null}
        </DialogContent>
      </Dialog>
      {create.data ? (
        <SecretDialog
          open
          title={create.data.key.kind === "secret" ? "Secret key created" : "Public key created"}
          secret={create.data.secret}
          description={create.data.key.kind === "secret" ? "Use it as the bearer token of your SDK or exporter." : "Use it in browsers to send end-user feedback."}
          showSnippets={create.data.key.kind === "secret"}
          onDone={close}
        />
      ) : null}
    </>
  );
}
```

`CreateKeyForm({ org, project, create, onCancel })`, in the same file:
- State: `name` (`""`), `kind` (`"secret" | "public"`, initially `"secret"`), `scopes` (`"full" | "ingest"`, initially `"full"`), `expiry` (`EXPIRY[number]["value"]`, initially `"never"`), `nameError`, `expiryError`, `error`.
- Type `create` with a hook defined in the same file. `CreateKeyDialog` calls `const create = useCreateKey();` instead of calling `$api.useMutation` directly:

  ```ts
  function useCreateKey() {
    // The response holds the secret: gcTime 0 drops it from the mutation cache once reset (D-L-5).
    return $api.useMutation("post", "/api/v1/projects/{project}/api-keys", { gcTime: 0 });
  }
  type CreateMutation = ReturnType<typeof useCreateKey>;
  ```

  The form's props are `{ org: string; project: string; create: CreateMutation; onCancel: () => void }`.
- A `<form noValidate className="flex flex-col gap-4">` with:
  1. `TextField` id `key-name`, label `Name (optional)`, `maxLength={100}`, description `For example the service or environment that uses it.`, `error={nameError}`.
  2. `Kind`: `<Label id="key-kind-label">Kind</Label>` and `<RadioGroup aria-labelledby="key-kind-label" value={kind} onValueChange={(value) => setKind(value === "public" ? "public" : "secret")}>`, with two items. Each item is:

     ```tsx
     <div className="flex items-start gap-2">
       <RadioGroupItem value="secret" id="key-kind-secret" aria-describedby="key-kind-secret-help" />
       <div className="flex flex-col gap-0.5">
         <Label htmlFor="key-kind-secret">Secret key</Label>
         <p id="key-kind-secret-help" className="text-xs text-muted-foreground">For servers: SDKs, OpenTelemetry exporters, CI.</p>
       </div>
     </div>
     ```

     The second item is the same with `public`, `Public key` and `For browsers: end-user feedback only.`. The label text stays exactly `Secret key` / `Public key`, so the radio's accessible name is only that.
  3. Only when `kind === "secret"`: `<Label id="key-scopes-label">Scopes</Label>` and `<RadioGroup aria-labelledby="key-scopes-label" value={scopes} onValueChange={(value) => setScopes(value === "ingest" ? "ingest" : "full")}>`. Its items are `full` (`Ingest, read and write`, id `key-scopes-full`) and `ingest` (`Ingest only`, id `key-scopes-ingest`), each a `RadioGroupItem` plus a `Label` with exactly that text.
  4. `Expires`: `<Label htmlFor="key-expiry">Expires</Label>` and a `Select` (`value={expiry}`, `onValueChange={(value) => { const next = EXPIRY.find((item) => item.value === value); if (next) setExpiry(next.value); }}`) whose `SelectTrigger` has `id="key-expiry"` and `aria-describedby` pointing at the `expiryError` paragraph when there is one. Its items are `EXPIRY`. Below it: `{expiryError ? <p id="key-expiry-error" className="text-xs text-destructive">{expiryError}</p> : null}`.
  5. `FormError`; a `DialogFooter` with `Button variant="outline" type="button" disabled={create.isPending}` `Cancel` (calls `onCancel()`) and `SubmitButton pending={create.isPending}` `Create key`.

Submit:
1. Clear the errors and call `create.reset()`.
2. Build a typed body:

   ```ts
   const body: Schema<"CreateApiKeyRequest"> = { kind };
   const trimmed = name.trim();
   if (trimmed) body.name = trimmed;
   if (kind === "secret" && scopes === "ingest") body.scopes = ["ingest"];
   const days = EXPIRY.find((item) => item.value === expiry)?.days;
   if (days !== undefined) body.expires_at = expiresAt(Date.now(), days);
   ```

   A full secret key and a public key send no `scopes`; the server's defaults are ingest, read and write, and feedback.
3. `create.mutate({ params: { path: { project: projectRef(org, project) } }, body }, { onSuccess, onError })`.

- `onSuccess`: `void queryClient.invalidateQueries({ queryKey: apiKeysKey(org, project) })`, with `queryClient = useQueryClient()` called in `CreateKeyForm`. Nothing else is needed: `create.data` is now set, so the form dialog closes and `SecretDialog` opens. Its `onDone` (`close`) resets the mutation, which drops the secret (D-L-5).
- `onError(error)`:
  - `fieldErrors(error).name` → `nameError`, and `expires_at` → `expiryError`;
  - otherwise 403 → `error` `Only project admins can create keys.`;
  - otherwise `error` = `problemMessage(error, "Could not create the key.")`.

### 5. Onboarding note

In `web/src/components/onboarding/snippets.ts`, add `export const API_KEY_ASK_NOTE = "Ask a project admin for a secret key";` and keep `API_KEY_NOTE`.

In `web/src/components/onboarding/panel.tsx`, replace the last `<p>` with `<ApiKeyNote />`, defined in the same file:

```tsx
function ApiKeyNote() {
  const project = useOptionalProject();
  if (!project) {
    return <p className="text-sm text-muted-foreground">{API_KEY_NOTE}</p>;
  }
  if (!atLeast(project.data.role, "admin")) {
    return <p className="text-sm text-muted-foreground">{API_KEY_ASK_NOTE}</p>;
  }
  return (
    <p className="text-sm text-muted-foreground">
      <Link
        to="/$org/$project/settings/api-keys"
        params={{ org: project.org, project: project.project }}
        className="underline underline-offset-4"
      >
        {API_KEY_NOTE}
      </Link>
    </p>
  );
}
```

`FirstTraceScreen` (L4) does not use `ApiKeyNote` or `OnboardingPanel`'s note: it shows the real key, and it renders outside any project route.

In `onboarding.test.ts`, add `API_KEY_ASK_NOTE` to the imports (in Biome's order) and `expect(API_KEY_ASK_NOTE).toBe("Ask a project admin for a secret key")`.

## States

| Page | Loading | Error | Empty | Forbidden |
|---|---|---|---|---|
| API keys | skeletons | route `ErrorState` | `No API keys yet` | role below admin, or a `403` |

## Keyboard

The radio groups move with the arrow keys, and the expiry `Select` works as in L14. Enter submits. The secret dialog follows D-L-5 (L2).

## Tests — `api-keys.test.tsx`

Imports: `renderApp`, `problemResponse`, `API_KEY_ID`, `server`, `http`, `HttpResponse`, `screen`, `within`, `waitFor`, `projectFixture` and `apiKeyCreated` (paths in the intro), and `expiresAt` from `@/components/settings/create-key-dialog`. Helpers:
- `projectAs(role)` as in L15;
- `const projectNav = () => within(screen.getByRole("list", { name: "Project settings" }))`;
- `async function openCreate(user)`: click `Create key` (the page button), then `return await screen.findByRole("dialog", { name: "Create API key" })`;
- `function submit(dialog)`: click `within(dialog).getByRole("button", { name: "Create key" })`.

1. `it("lists keys")`:
   - `renderApp("/acme/support-bot/settings/api-keys")`;
   - `const table = await screen.findByRole("table", { name: "API keys" })`;
   - `const row = within(table).getByRole("row", { name: /^CI\b/ })` contains `sk-llmobs-EXAMPL…`, `Secret` and `ingest, read, write`;
   - `within(row).getAllByText("Never")` has length 2 (last used and expires);
   - `projectNav().getByRole("link", { name: "API keys" })` has `aria-current="page"`.
2. `it("creates a secret key and shows it once")`:
   - POST `/api/v1/projects/:project/api-keys` captures the body and answers `HttpResponse.json(apiKeyCreated, { status: 201 })`;
   - `openCreate`, type `CI` in `Name (optional)`, `submit`;
   - the body is `{ kind: "secret", name: "CI" }`;
   - `const secret = await screen.findByRole("dialog", { name: "Secret key created" })`;
   - `within(secret).getByLabelText("Secret")` has value `sk-llmobs-EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0`;
   - `within(secret).getByRole("tab", { name: "OTel env vars" })` exists;
   - click `Done`; `queryByRole("dialog")` is null and `queryByLabelText("Secret")` is null;
   - the secret left TanStack's mutation cache (D-L-5): with `queryClient` from `renderApp`, `await waitFor(() => expect(JSON.stringify(queryClient.getMutationCache().getAll().map((m) => m.state.data ?? null))).not.toContain("sk-llmobs-EXAMPLE0"))`.
3. `it("creates an ingest-only key with an expiry")`:
   - pick the radio `Ingest only`;
   - open the combobox `Expires` and pick `In 30 days` (README § Pitfalls if refused);
   - `submit`;
   - the body has `scopes: ["ingest"]` and an `expires_at` string;
   - `Date.parse(body.expires_at) - Date.now()` is between `29.9 * 86_400_000` and `30.1 * 86_400_000`.
4. `it("creates a public key without snippets")`:
   - pick the radio `Public key`; `queryByRole("radiogroup", { name: "Scopes" })` is null;
   - the POST answers `{ ...apiKeyCreated, key: { ...apiKeyCreated.key, kind: "public", scopes: ["feedback"], prefix: "pk-llmobs-EXAMPL" }, secret: "pk-llmobs-EXAMPLE0EXAMPLE0EXAMPLE0" }` with status 201;
   - `submit`; the body is `{ kind: "public" }`;
   - in the dialog `Public key created`, `queryByRole("tab", { name: "OTel env vars" })` is null.
5. `it("shows the server's expiry error")`: POST answers `problemResponse(400, "bad_request", { errors: [{ field: "expires_at", message: "must be in the future" }] })`; `submit`; `must be in the future` is shown in the form dialog.
6. `it("revokes a key after confirmation")`:
   - DELETE `/api/v1/projects/:project/api-keys/:id` captures `params.id` and answers 204;
   - open `Actions for CI`, then `Revoke`, then `Revoke key` in the alertdialog `Revoke key?`;
   - the id is `API_KEY_ID`, and the alertdialog is gone.
7. `it("keeps the revoke dialog open on failure")`: DELETE answers `problemResponse(500, "internal")`; after `Revoke key`, `within(alertdialog).getByRole("alert")` has `Internal Server Error`.
8. `it("is forbidden for a member")`:
   - `server.use(projectAs("member"))`, and a counting list handler: `let listCalls = 0; server.use(http.get("/api/v1/projects/:project/api-keys", () => { listCalls += 1; return HttpResponse.json({ data: [], next_cursor: null }); }))` (the fixtures would answer the GET silently otherwise);
   - `await screen.findByText("You don't have access to this page")`, and `Only project admins can manage API keys.` is shown;
   - `projectNav().queryByRole("link", { name: "API keys" })` is null;
   - `listCalls` is `0`.
9. `it("shows Forbidden when the list answers 403")`: GET `/api/v1/projects/:project/api-keys` answers `problemResponse(403, "forbidden")`; `await screen.findByText("Only project admins can manage API keys.")`.
10. `it("computes expiry dates")`: `expiresAt(Date.UTC(2026, 0, 1), 30)` equals `"2026-01-31T00:00:00.000Z"`.
11. `it("shows the empty state")`: GET `/api/v1/projects/:project/api-keys` answers `{ data: [], next_cursor: null }`; `await screen.findByText("No API keys yet")`; `Create a secret key to send traces.` is shown.
12. `it("loads more keys")`: the GET answers `{ data: [the CI key from api-keys.json], next_cursor: "c2" }` without `cursor`, and `{ data: [{ ...that key, id: "01900000-0000-7000-8000-000000000202", name: "Staging", prefix: "sk-llmobs-STAGIN" }], next_cursor: null }` with `cursor=c2`; click `Load more`; `await screen.findByRole("row", { name: /^Staging\b/ })`; `Load more` is gone.
13. `it("explains a refused creation")`: POST answers `problemResponse(403, "forbidden")`; `openCreate`, `submit`; `within(dialog).getByRole("alert")` has `Only project admins can create keys.`; the form dialog stays open.
14. `it("shows the server's name error")`: POST answers `problemResponse(400, "bad_request", { errors: [{ field: "name", message: "must be at most 100 characters" }] })`; `openCreate`, type `CI`, `submit`; `must be at most 100 characters` is shown and `Name (optional)` has `aria-invalid="true"`.
15. `it("cancels without creating a key")`: no POST handler is installed (an unhandled write fails the test); `openCreate`, type `CI`, click `Cancel`; `queryByRole("dialog")` is null; `openCreate` again: `Name (optional)` has value `""`.

`web/src/routes/$org/$project/traces/index.test.tsx` (append; reuse the empty-list override from the existing test `shows onboarding snippets when the project has no traces`, including its `savedViewHandlers([])` line):

16. `it("links admins to the API keys page from onboarding")`: the traces GET answers `{ data: [], next_cursor: null }`; `renderApp("/acme/support-bot/traces")`; `await screen.findByRole("link", { name: "Create a secret key in Settings → API keys" })` has `href` `/acme/support-bot/settings/api-keys`.
17. `it("asks members to get a key from an admin")`: the same empty list, plus `GET /api/v1/projects/:project` answering `{ ...projectFixture, role: "member" }`; `await screen.findByText("Ask a project admin for a secret key")`; `queryByRole("link", { name: /API keys/ })` is null.

The existing onboarding test still finds `Create a secret key in Settings → API keys` with `getByText`, because the text is the link's own text. Change nothing in it.

## Commands

The same as L13.

## Done when

- Admins list, create and revoke keys: secret keys with snippets, public keys, ingest-only keys and keys with an expiry.
- The secret is shown once and dropped on Done.
- Members get the Forbidden state and no nav entry.
- The onboarding note links admins to the page.
- The tests pass, the checks are green, and there is one commit.

---

# L17 — Project members (overrides)

**Edit only:**
- `web/src/api/queries-settings.ts`;
- `web/src/components/settings/settings-nav.ts`;
- `web/src/routes/$org/$project/settings/members.tsx` and `members.test.tsx` (new);
- the README's `Status:` line.

Run `pnpm gen:routes` after adding the route file.

**Commit:** `web: add project member access settings`

## Steps

### 1. Query helpers

```ts
export function projectMembersInit(org: string, project: string) {
  return { params: { path: { project: projectRef(org, project) }, query: { limit: 50 } } };
}
export function projectMembersKey(org: string, project: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/members", projectMembersInit(org, project)).queryKey;
}
```

### 2. Nav

Append `{ to: "/$org/$project/settings/members", label: "Members", adminOnly: false }`.

### 3. Page — `members.tsx`

`createFileRoute("/$org/$project/settings/members")`, component `ProjectMembersPage`.

- `const { org, project, ref, data } = useProject();` and `canEdit = atLeast(data.role, "admin")`.
- `const { data: me } = $api.useSuspenseQuery("get", "/api/auth/me");`
- `SettingsPage` with title `Members` and description `Org members and their access to this project. Org owners and admins always have admin access.`.
- Data: `$api.useInfiniteQuery("get", "/api/v1/projects/{project}/members", projectMembersInit(org, project), …)`. Loading: skeletons. Error: throw. Empty: never.
- An `actionError` above the table with `FormError`.
- `Table aria-label="Project members"` with these columns:
  - `Member`: `name ?? email` on the first line and, when `name` exists, `email` below it in `text-xs text-muted-foreground`;
  - `Org role`: `roleLabel(member.org_role)`;
  - `Project access`:
    - when `!canHaveOverride(member.org_role)`: `<span className="text-muted-foreground">Always admin</span>`;
    - else a `Select` with `value={member.override ?? "inherit"}`, `onValueChange={(value) => changeAccess(member, value)}`, a trigger with `aria-label={`Project access of ${member.user.email}`}` and `className="h-7 w-40"`, and the items `inherit`, `admin`, `member`, `viewer`, `none` labelled with `overrideLabel`. It is disabled when `!canEdit` or when this row is pending (see below). The whole `<Select>` is wrapped in `DisabledReason` with `Only project admins can change access` when `!canEdit`.
  - `Effective role`: `member.effective_role ? roleLabel(member.effective_role) : "No access"`.
- "Load more" while `hasNextPage`.

Changes (shared row mutations, callbacks on the hooks; README § Pitfalls):

```ts
const OVERRIDES = ["admin", "member", "viewer", "none"] as const;

function onChanged(user: string) {
  void queryClient.invalidateQueries({ queryKey: projectMembersKey(org, project) });
  if (user === me.user.id) {
    void queryClient.invalidateQueries({ queryKey: projectQuery(org, project).queryKey });
  }
}
function onFailed(error: unknown) {
  setActionError(
    problemOf(error)?.status === 403
      ? "Only project admins can change access."
      : problemMessage(error, "Could not change access."),
  );
}

const put = $api.useMutation("put", "/api/v1/projects/{project}/members/{user}", {
  onSuccess: (_data, variables) => onChanged(variables.params.path.user),
  onError: onFailed,
});
const del = $api.useMutation("delete", "/api/v1/projects/{project}/members/{user}", {
  onSuccess: (_data, variables) => onChanged(variables.params.path.user),
  onError: onFailed,
});

function changeAccess(member: Schema<"ProjectMember">, value: string) {
  setActionError(undefined);
  const path = { project: ref, user: member.user.id };
  if (value === "inherit") {
    del.mutate({ params: { path } });
    return;
  }
  const role = OVERRIDES.find((item) => item === value);
  if (role) {
    put.mutate({ params: { path }, body: { role } });
  }
}
```

A row is pending when `(put.isPending && put.variables?.params.path.user === member.user.id) || (del.isPending && del.variables?.params.path.user === member.user.id)`.

A `400` carries the server's detail, such as `owners and admins cannot be overridden`, which `problemMessage` shows as written.

## States

| Page | Loading | Error | Empty |
|---|---|---|---|
| Project members | skeletons | route `ErrorState` | never |

## Keyboard

As L14's selects.

## Tests — `members.test.tsx`

Imports: `renderApp`, `problemResponse`, `ADA_ID`, `BOB_ID`, `server`, `http`, `HttpResponse`, `screen`, `within`, `waitFor` and `projectFixture`. Helpers:
- `projectAs(role)` as in L15;
- `const projectNav = () => within(screen.getByRole("list", { name: "Project settings" }))`;
- `const BOB = { id: BOB_ID, email: "bob@example.com", name: "Bob Example" }`;
- `const ADA = { id: ADA_ID, email: "ada@example.com", name: "Ada Example" }`.

The first assertion of each test is `const table = await screen.findByRole("table", { name: "Project members" })`.

1. `it("shows effective roles and overrides")`:
   - `renderApp("/acme/support-bot/settings/members")`;
   - `const ada = within(table).getByRole("row", { name: /ada@example.com/ })`; its cells (`within(ada).getAllByRole("cell")`) are, in order, the member, `Owner`, `Always admin` and `Admin`;
   - `const bob = within(table).getByRole("row", { name: /bob@example.com/ })`;
   - `within(bob).getByRole("combobox", { name: "Project access of bob@example.com" })` has text content `Viewer`;
   - the last cell of Bob's row (`within(bob).getAllByRole("cell")[3]`) has text content `Viewer`.
2. `it("sets an override")`:
   - PUT `/api/v1/projects/:project/members/:user` captures `params.user` and the body, and answers `{ user: BOB, org_role: "member", override: "none" }`;
   - pick `No access` for Bob;
   - the body is `{ role: "none" }`, and the user param is `BOB_ID`.
3. `it("removes an override with Inherit")`: DELETE `/api/v1/projects/:project/members/:user` captures `params.user` and answers 204; pick `Inherit org role` for Bob; the DELETE was called with `BOB_ID`.
4. `it("shows the server's refusal")`: PUT answers `problemResponse(400, "bad_request", { detail: "owners and admins cannot be overridden" })`; pick `Admin` for Bob; `owners and admins cannot be overridden` is shown.
5. `it("explains a 403")`: PUT answers `problemResponse(403, "forbidden")`; pick `Member` for Bob; `Only project admins can change access.` is shown.
6. `it("refreshes the caller's project role after changing their own access")`:
   - `GET /api/v1/projects/:project` answers `{ ...projectFixture, role: "admin" }` and counts its calls in `projectCalls`;
   - `GET /api/v1/projects/:project/members` answers `{ data: [{ user: ADA, org_role: "member", override: "admin", effective_role: "admin" }, { user: BOB, org_role: "member", override: "viewer", effective_role: "viewer" }], next_cursor: null }`;
   - PUT answers `{ user: ADA, org_role: "member", override: "member", effective_role: "member" }`;
   - after the table shows, store `const before = projectCalls`;
   - pick `Member` for `ada@example.com`;
   - `await waitFor(() => expect(projectCalls).toBeGreaterThan(before))`.
7. `it("loads more members")`:
   - GET members answers the fixture's two rows with `next_cursor: "c2"` without `cursor`, and `{ data: [{ user: { id: "01900000-0000-7000-8000-000000000103", email: "carol@example.com", name: "Carol Example" }, org_role: "viewer", effective_role: "viewer" }], next_cursor: null }` with `cursor=c2`;
   - click `Load more`;
   - `await screen.findByRole("row", { name: /carol@example.com/ })`.
8. `it("is read-only for a viewer")`:
   - `server.use(projectAs("viewer"))`;
   - the combobox `Project access of bob@example.com` is disabled, and its `parentElement` has the description `Only project admins can change access`;
   - `projectNav().getByRole("link", { name: "General" })` and `projectNav().getByRole("link", { name: "Members" })` exist, and `projectNav().queryByRole("link", { name: "API keys" })` is null.

## Commands

The same as L13.

## Done when

- Project admins set and clear overrides for org members and viewers.
- Owners and admins show "Always admin".
- Everyone else sees a disabled select with the reason.
- The tests pass, the checks are green, and there is one commit.
