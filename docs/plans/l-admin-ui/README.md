Status: L1–L12 done on 2026-11-13; L13–L20 not started

# L · Admin UI

Track L of Wave 2 builds the account, auth and admin pages of spec 11 in `web/`, over track D's API (`crates/server/openapi.json`, `fixtures/api/`). D's server side is merged, so every page here has a contract. The pages are:
- sign-in, sign-up and first-run setup, including the OAuth/OIDC buttons that `GET /api/auth/config` lists;
- invite acceptance, forgot/reset password and email verification;
- the CLI sign-in page;
- the account page: profile, password and personal access tokens;
- org settings: name, members and invites;
- project settings: name and retention, API keys, and per-project role overrides;
- instance admin: users and settings, including the test email;
- the auth gate and the redirect to `/login` on a `401`.

**Not in L:**
- LLM connections, evaluators, alerts and datasets (tracks I, J, K);
- org usage (track N);
- deleting a project or an org, and GDPR deletion (track M);
- custom model prices (unassigned).

There is no design doc. D's contract and fixtures exist, and track C settled the UI conventions. The open points were product choices, and the user settled them (§ Decisions).

The plan has 20 sub-phases, **L1–L20**. Each one is a single commit and is done by one agent working alone, in order. Each ends with `just check-web` green and then `just check` green.

**Read first, for every sub-phase:**
- `AGENTS.md`;
- this README, all of it;
- the sub-phase's own section;
- `docs/spec/08-auth.md` and `docs/spec/11-ui.md` § Navigation and § Settings;
- `docs/spec/09-api.md` § Conventions;
- `fixtures/api/README.md` (fixed ids and emails);
- the existing code the sub-phase names. Read every file before you edit it.

## Files

| File | Sub-phases |
|---|---|
| [01-foundations.md](01-foundations.md) | L1 shared helpers and form primitives · L2 secret dialog and snippet tabs |
| [02-sign-in.md](02-sign-in.md) | L3 login page · L4 first-run setup · L5 auth gate, 401 redirect and sign-out · L6 workspace creation |
| [03-auth-pages.md](03-auth-pages.md) | L7 sign-up and email verification · L8 forgot and reset password · L9 invite acceptance |
| [04-account.md](04-account.md) | L10 top bar outside an org, the user menu and the account profile · L11 personal access tokens · L12 CLI sign-in |
| [05-settings.md](05-settings.md) | L13 org settings shell, org general and new org · L14 org members and invites · L15 project settings shell, project general and new project · L16 API keys · L17 project members |
| [06-admin.md](06-admin.md) | L18 admin users · L19 admin settings · L20 dev write mocks and closeout |

## Decisions

The user settled these on 2026-11-13. They are written in `docs/mvp-decisions.md` § Track L and in spec 11, in the same commit as this plan.

| # | Decision |
|---|---|
| D-L-1 | **Signed out means redirect to `/login` and back.** Every route except the public ones (§ Routes) is behind an auth gate. The gate is the root route's `beforeLoad`, which loads `GET /api/auth/me`. A `401` there redirects to `/login?redirect=<path and search>`. When `GET /api/auth/config` says `setup_required`, it redirects to `/setup` instead. A `401` from any `/api/v1/*` call or from `GET /api/auth/me` during use does the same redirect and clears the query cache; the handler does nothing while the current page is public. Unsaved form input is lost. The SSE streams use `EventSource`, which the middleware does not see; their `401` is handled when the next list refetch fails. |
| D-L-2 | **L owns `/login` and the 401 handling.** This amends D-G-19. L can land long before G's M5, so L builds the real sign-in page, the gate and the 401 redirect. G's M5 only serves the UI on the real API and checks these pages work there. The coordinator updates D-G-19 and G's design. |
| D-L-3 | **First run is one form, then a "Send your first trace" screen.** `/setup?token=` is one page with three sections (You, Organization, Project) and the SSO buttons. On success it shows the new secret key in the OTel, Python, TypeScript and Vercel snippets. It polls the project's traces and says when the first one arrives. "Go to project" opens the traces page. SSO setup creates only the admin (D148), so it returns to `/setup/workspace`. That page creates the org, the project and a key with three calls and shows the same screen. A signed-in user with no org gets a "Create an organization" button on `/` that opens it. |
| D-L-4 | **Too low a role hides pages and disables actions.** A settings page that the role cannot use at all is not in the settings nav. Opened by URL, it shows the Forbidden state. Inside a visible page, an action the role cannot take is disabled, with the reason in a tooltip and in the accessible description. Inside a dropdown menu, where a tooltip cannot be reached by keyboard, the reason is a visible line under the disabled items. `web/src/lib/roles.ts` mirrors the server's rules (D139); the server stays the authority, and its `403` is shown when it disagrees. |
| D-L-5 | **A new secret is shown once, in a dialog that must be closed on purpose.** The dialog shows the API key or personal access token in mono with a Copy button and the line "Copy it now. You won't see it again." A secret key also gets the snippet tabs with the key filled in. "Done" closes it. Esc or an outside click closes it only after a Copy; otherwise the dialog first asks "Close without copying?". The secret lives only in component state. It is never in the URL, localStorage or the query cache: every mutation whose response carries a secret is created with `{ gcTime: 0 }` (`$api.useMutation(method, path, { gcTime: 0 })`) and reset when the dialog closes, so TanStack's mutation cache drops it at once. The setup screen (D-L-3) shows the key on the page with the same rules. |
| D-L-6 | **Settings are their own area.** Org settings are `/$org/settings/{general,members}`. Project settings are `/$org/$project/settings/{general,api-keys,members}`. On a settings route, the settings sidebar replaces the project sidebar. It has a "Back" link, a Project group (on project routes) and an Organization group. The project sidebar gets a "Settings" entry at its bottom, and the org home gets a "Settings" button. `/account` and `/admin/{users,settings}` are full pages under the top bar, opened from the user menu. |
| D-L-7 | **No global banners.** When SMTP is off, the invite dialog shows the accept link to copy, `/reset` tells the user to ask an admin, and Admin → Settings shows the SMTP state with "Send test email" disabled. An unverified-email banner would need `email_verified` on `GET /api/auth/me`, which it does not have (§ Contract gaps). |
| D-L-8 | **Auth URLs.** `/login?redirect=&error=`, `/signup`, `/setup?token=`, `/setup/workspace`, `/invite/$token`, `/reset` (no token: the "forgot" form; `?token=`: the new-password form), `/verify?token=`, `/cli-auth?port=&state=&host=`. Every one of them is a reserved org slug on the server (`crates/server/src/meta/orgs.rs` `RESERVED`), so none can shadow an org. |
| D-L-9 | **CLI sign-in.** `/cli-auth?port=&state=&host=` asks the signed-in user to confirm. It creates a personal access token named `CLI on <host>`, or `CLI` without `host`, with scopes `read, write`. Then it navigates to `http://127.0.0.1:<port>/callback?token=<secret>&state=<state>`. Cancel navigates to `…/callback?error=access_denied&state=<state>`. `port` is an integer from 1024 to 65535. `state` is 1–256 characters of `[A-Za-z0-9_-]` with at least one character that is not a digit (TanStack Router would turn an all-digit value into a number; README § Conventions, Search params). `host` is optional, 1–64 characters of `[A-Za-z0-9.-]`. Any invalid value shows "This sign-in link is invalid" and calls nothing. The CLI owner sends `host` (note for track H). |
| D-L-10 | **Writes in tests are mocked per test.** Page tests override every POST, PATCH, PUT and DELETE they trigger with `server.use(http.<method>(…))`, and they assert the request body. `just web-dev` gets stateless write handlers in `web/src/mocks/writes.ts`, which only the browser worker installs (L20). They answer with the `fixtures/api/static/*-created.json` examples or `204`. |
| D-L-11 | **Forms use no form library.** They use controlled inputs and plain `onSubmit`. The client checks only "required", the password length (10 to 128) and that the two password fields match. Everything else is the server's call. A `400` problem's `errors[]` is shown under the field it names, and `detail` above the submit button. |
| D-L-12 | **The password form always offers "Current password".** `Me` does not say whether the account has a password (§ Contract gaps). The field is optional and says "Leave empty if you have never set a password (single sign-on accounts)." The server answers `400` on `current_password` when it is missing but needed, and the form shows that message under the field. |

## Contract gaps

Adding server endpoints or fields is out of scope for L. These are reported to the coordinator, and nothing in L waits for them.

| # | Gap | Effect in L | Request |
|---|---|---|---|
| CG1 | `User` in `GET /api/auth/me` has no `email_verified` and no `has_password` | No unverified-email banner (D-L-7). The password form cannot tell whether "Current password" is needed (D-L-12). | Add both fields to `User` (spec 09 `GET /me`). |
| CG2 | Project slugs are not reserved (`orgs.rs`: "Project slugs do not use this list") | A project with slug `settings` has the same URL prefix as `/$org/settings/…`. L's routes are static, so `/$org/settings/general` and `/$org/settings/members` win over that project's pages. Its `traces`, `spans` and other pages still resolve, because `/$org/settings` has no children with those names. | Reserve `settings` as a project slug, or accept this and document it in spec 11. |
| CG3 | `/cli-auth` has no host name in spec 08 | L defines `host` (D-L-9). | The CLI (track H) sends `host`; spec 08 names it. |
| CG4 | `GET /api/v1/me` (spec 09) is not implemented; only `GET /api/auth/me` exists | None. L reads `GET /api/auth/me`. | Drop `GET /api/v1/me` from spec 09, or add it. |
| CG5 | `UpdateProjectRequest.retention_days` is not nullable in `openapi.json`, but the server takes `null` as "keep forever" | L15 sends `null` through one commented cast in `projectPatch()`. | Mark the field nullable in the contract. |

## Routes

Public routes: `/login`, `/signup`, `/setup`, `/reset`, `/verify` and `/invite/$token`. They render without the top bar. `isPublicPath(pathname)` in `web/src/lib/redirect.ts` (L1) is the single list. **`/setup/workspace` and `/cli-auth` are not public:** they need a session.

```
/login                          ?redirect ?error ?email                            L3
/setup                          ?token                                             L4
/setup/workspace                (signed in)                                        L6
/signup                                                                            L7
/verify                         ?token                                             L7
/reset                          ?token                                             L8
/invite/$token                                                                     L9
/account                        (signed in; top bar)                               L10, L11
/cli-auth                       ?port ?state ?host (signed in)                     L12
/$org/settings                  → redirect to /$org/settings/general               L13
/$org/settings/general                                                             L13
/$org/settings/members                                                             L14
/$org/$project/settings         → redirect to /$org/$project/settings/general      L15
/$org/$project/settings/general                                                    L15
/$org/$project/settings/api-keys                                                   L16
/$org/$project/settings/members                                                    L17
/admin                          → redirect to /admin/users                         L18
/admin/users                                                                       L18
/admin/settings                                                                    L19
```

`/` keeps its behaviour: it redirects to the first org. A user without an org gets a "Create an organization" button that opens `/setup/workspace` (L6).

## Who may do what (D139, spec 08, `crates/server/src/routes/*`)

The UI reads the caller's org role from `me.orgs[i].role`. Their effective project role is `project.role` from `GET /api/v1/projects/{project}`. The server's access per operation:

| Operation | Needs |
|---|---|
| `get_org`, `list_projects`, `list_org_members`, `delete_org_member` on yourself (leave) | org viewer |
| `patch_org`, `post_project`, `list_org_invites`, `post_org_invite`, `delete_org_invite`, `patch_org_member`, `delete_org_member` on someone else | org admin |
| `get_project`, `list_project_members` | project viewer |
| `patch_project`, `list_api_keys`, `post_api_key`, `delete_api_key`, `put_project_member`, `delete_project_member` | project admin |
| `list_admin_users`, `post_admin_user`, `patch_admin_user`, `post_admin_user_reset_link`, `get_admin_settings`, `post_admin_settings_test_email` | `is_instance_admin` |
| `patch_me`, `post_me_password`, `list_personal_tokens`, `post_personal_token`, `delete_personal_token`, `post_org`, `post_invite_accept` | any signed-in user, by session |

Extra role rules (the server's `may_set_role` and `may_remove_other` in `routes/members.rs`, and `may_grant` in `routes/invites.rs`):
- Nobody grants a role above their own, and only an owner grants `owner` or changes an owner's role.
- Only an owner removes an owner, and an org keeps at least one owner. The last owner gets a `409` with detail `an org needs an owner`.
- Overrides apply only to org `member`s and `viewer`s. A `PUT` for an owner or admin answers `400` with detail `owners and admins cannot be overridden`.
- An instance admin cannot disable or demote themself. `patch_admin_user` answers `400` with detail `cannot change your own account`.

## Shared building blocks

L1–L6 create these, and later phases use them exactly as written here. **Do not create a second version of any of them.**

### L1 — `web/src/lib/roles.ts`

```ts
import type { Schema } from "@/api/types";
export type OrgRole = Schema<"OrgRole">;                       // "owner" | "admin" | "member" | "viewer"
export type RoleOverride = Schema<"ProjectRoleOverride">;      // "admin" | "member" | "viewer" | "none"
export const ORG_ROLES: readonly OrgRole[];                    // ["owner", "admin", "member", "viewer"]
export function roleRank(role: OrgRole): number;               // viewer 0, member 1, admin 2, owner 3
export function atLeast(role: OrgRole | undefined, min: OrgRole): boolean; // undefined → false
export function roleLabel(role: OrgRole): string;              // "Owner", "Admin", "Member", "Viewer"
export function grantableRoles(actor: OrgRole | undefined): OrgRole[];
export function canChangeRole(actor: OrgRole | undefined, target: OrgRole): boolean;
export function canRemoveMember(actor: OrgRole | undefined, target: OrgRole, self: boolean): boolean;
export function canHaveOverride(orgRole: OrgRole): boolean;    // member, viewer → true
export function overrideLabel(value: RoleOverride | "inherit"): string;
```

- `grantableRoles` returns `ORG_ROLES` filtered to rank ≤ the actor's, and drops `owner` unless the actor is the owner. It keeps the order owner, admin, member, viewer. For a role below admin, or `undefined`, it returns `[]`.
- `canChangeRole` is true when the actor is at least admin, and also the owner if the target is an owner.
- `canRemoveMember` is true when `self` is true. Otherwise the actor must be at least admin, and also the owner if the target is an owner.
- `overrideLabel` returns: `inherit` → "Inherit org role", `admin` → "Admin", `member` → "Member", `viewer` → "Viewer", `none` → "No access".

### L1 — `web/src/lib/redirect.ts`

```ts
export function safeRedirect(value: unknown): string | undefined;
export function isPublicPath(pathname: string): boolean;
export function authorizeHref(providerId: string, options?: { redirect?: string; setupToken?: string }): string;
```

- `safeRedirect` returns the value when it is a string that:
  - starts with `/`;
  - does not start with `//` or `/\`;
  - is at most 1024 bytes in UTF-8;
  - has no character below U+0020 and no U+007F.

  Otherwise it returns `undefined`. These are the server's rules for `?redirect=`.
- `isPublicPath` is true for exactly `/login`, `/signup`, `/setup`, `/reset` and `/verify`, and for any path that starts with `/invite/`.
- `authorizeHref` returns `/api/auth/authorize/<encodeURIComponent(providerId)>`. It appends `?` plus the `URLSearchParams` of `redirect` and then `setup_token`, each only when given. The `?` is left out when neither is given. For example, `authorizeHref("oidc:okta", { redirect: "/acme" })` is `/api/auth/authorize/oidc%3Aokta?redirect=%2Facme`.

### L1 — `web/src/api/problem.ts` (additions)

```ts
export function fieldErrors(error: unknown): Record<string, string>;  // first message per `errors[].field`; {} when none
export function problemMessage(error: unknown, fallback: string): string; // problem.detail, else problem.title, else fallback
```

### L1 — form primitives in `web/src/components/form/`

| File | Export | Props |
|---|---|---|
| `text-field.tsx` | `TextField` | `id: string; label: string; value: string; onChange: (value: string) => void; type?: "text" \| "email" \| "password" \| "number"; error?: string; description?: string; autoComplete?: string; required?: boolean; disabled?: boolean; readOnly?: boolean; maxLength?: number; placeholder?: string` |
| `form-error.tsx` | `FormError` | `message?: string`. It renders `<p role="alert" className="text-sm text-destructive">{message}</p>`, or `null` when there is no message. |
| `form-success.tsx` | `FormSuccess` | `message?: string`. It renders `<p role="status" className="text-sm text-muted-foreground">{message}</p>`, or `null`. |
| `submit-button.tsx` | `SubmitButton` | `pending: boolean; children: ReactNode; disabled?: boolean; form?: string; variant?: ComponentProps<typeof Button>["variant"]` (`Button` from `@/components/ui/button`, `ComponentProps` from `react`). A `type="submit"` `Button` (passing `form` through, so a dialog footer can submit a form by id), disabled while `pending` or `disabled`, with a `Loader2` spinner (`animate-spin`) before the label while pending. |

`TextField` renders a `Label` (`htmlFor={id}`) and an `Input` with `id`, `name={id}`, `aria-invalid={error ? true : undefined}` and `aria-describedby`. `aria-describedby` lists `${id}-description` when there is a description and `${id}-error` when there is an error. Below the input come the description (`<p id="${id}-description" className="text-xs text-muted-foreground">`) and the error (`<p id="${id}-error" className="text-xs text-destructive">`). A `required` field shows no asterisk; the caller puts "(optional)" in the label of an optional field.

### L1 — other components

| File | Export | What |
|---|---|---|
| `web/src/components/confirm-dialog.tsx` | `ConfirmDialog` | `open: boolean; onOpenChange: (open: boolean) => void; title: string; description: ReactNode; confirmLabel: string; destructive?: boolean; pending: boolean; error?: string; onConfirm: () => void`. It uses shadcn `AlertDialog`. "Cancel" is `AlertDialogCancel`. The confirm button is a plain `Button` (not `AlertDialogAction`, which would close the dialog on click), with `variant="destructive"` when `destructive`. It is disabled while `pending`. `FormError` shows `error` above the buttons. The caller closes the dialog on success. |
| `web/src/components/disabled-reason.tsx` | `DisabledReason` | `reason: string \| undefined; children: ReactElement`. Without a reason it returns `children`. With one it renders `<Tooltip><TooltipTrigger asChild><span tabIndex={0} className="inline-flex w-fit" aria-describedby={id}>{children}</span></TooltipTrigger><TooltipContent>{reason}</TooltipContent></Tooltip>` and then `<span id={id} hidden>{reason}</span>`, where `id` comes from `useId()`. The caller disables the child itself. |
| `web/src/components/forbidden.tsx` | `Forbidden` | `title?: string` (default "You don't have access to this page"); `description: string`. A centered block like `NotFound`, with a "Home" link to `/`. |

L1 also wraps the root route's `<Outlet />` in `<TooltipProvider>` (`web/src/routes/__root.tsx`), so tooltips work outside the project sidebar.

### L2 — secrets and snippets

| File | Export | What |
|---|---|---|
| `web/src/components/onboarding/snippets.ts` | `PLACEHOLDER_KEY = "sk-llmobs-…"`; `otelSnippet(origin, key = PLACEHOLDER_KEY)`, `pythonSnippet(…)`, `typescriptSnippet(…)`, `vercelSnippet(…)` | It gains the `key` parameter. `KEY` is renamed `PLACEHOLDER_KEY` and exported. |
| `web/src/components/onboarding/snippet-tabs.tsx` | `SnippetTabs` | `origin: string; apiKey?: string`. The tabs block that is in `panel.tsx` today, moved here. `OnboardingPanel` uses it. |
| `web/src/components/copy-button.tsx` | `CopyButton` | It gains `onCopied?: () => void`, called after a successful clipboard write. |
| `web/src/components/secret-dialog.tsx` | `SecretDialog` | `open: boolean; title: string; secret: string; description?: string; showSnippets?: boolean; onDone: () => void`. D-L-5's dialog. |

### L3–L6 — auth

| File | Export | What |
|---|---|---|
| `web/src/api/auth.ts` | `authConfigQuery` (L3), `requireSession` (L5) | `GET /api/auth/config` options with `staleTime: 60_000`; the gate. |
| `web/src/lib/search/auth.ts` | `parseLoginSearch`, `parseSetupSearch`, `OAUTH_ERROR_CODES`, `OAuthErrorCode`, `EMAIL_MAX`, `TOKEN_MAX` | Search-param validators of the auth routes (L3, L4). |
| `web/src/components/auth/messages.ts` | `NO_SIGN_IN_METHOD`, `RATE_LIMITED`, `oauthErrorMessage`, `loginErrorMessage` | Shared copy (L3). |
| `web/src/components/auth/auth-layout.tsx` | `AuthLayout` | `title: string; description?: ReactNode; children: ReactNode; footer?: ReactNode; width?: "narrow" \| "wide"` (default `"narrow"`). A centered page for the public routes (L3). |
| `web/src/components/auth/provider-buttons.tsx` | `ProviderButtons`, `OrDivider` | `ProviderButtons({ providers, redirect?, setupToken? })`: one "Continue with <display_name>" link per provider. `OrDivider()`: the "or" line between the buttons and a password form (L3). |
| `web/src/components/auth/password-fields.tsx` | `PasswordFields`, `passwordProblems`, `PasswordProblems` | `PasswordFields({ password, confirm, onPasswordChange, onConfirmChange, passwordError?, confirmError?, disabled?, passwordLabel? = "Password" })`. Its inputs always have the ids `password` and `password_confirm`. `passwordProblems(password, confirm)` returns `{ password?, confirm? }` with "Use at least 10 characters.", "Use at most 128 characters." or "The passwords do not match." (L4). |
| `web/src/components/onboarding/first-trace.tsx` | `FirstTraceScreen` | `{ org, project, projectName, secret }`: the "Send your first trace" screen (L4). |
| `web/src/api/auth.ts` | `clearUserData(queryClient)` (L3) | Drops the previous session's cached data but keeps the auth config. |
| `web/src/components/auth/use-sign-out.ts` | `useSignOut()` (L5) | `POST /api/auth/logout`, clear user data, go to `/login`. |
| `web/src/api/session.ts` | `setUnauthorizedHandler`, `isSessionApiPath`, `unauthorizedMiddleware` | The 401 middleware (L5). The handler receives the API path of the `401`. `isSessionApiPath(pathname)` is true for `/api/v1/*` and for exactly `/api/auth/me`. |

The sections of L3–L6 give their exact props.

### L10–L12 — shell and account

| File | Export | What |
|---|---|---|
| `web/src/components/shell/top-bar-layout.tsx` | `TopBarLayout` | The top bar plus a scrollable content area. `/$org`, `/account` and `/admin` use it (L10). The org and project switchers and ⌘K appear only under `/$org`. |
| `web/src/lib/external-navigation.ts` | `navigateExternal(url)`, `setExternalNavigator(fn)` | A full-page navigation to another origin, with a test seam (L12). |

The user menu gets "Sign out" in L5, "Account" in L10 and "Admin" (instance admins only) in L18, when `/admin/users` exists. A phase never links to a route that a later phase creates: TanStack Router type-checks link targets, so each phase adds the links to its own routes.

### L13–L19 — settings and admin

| File | Export | What |
|---|---|---|
| `web/src/api/queries-settings.ts` | `…Init` helpers and list keys | Settings queries (L13, extended by L14–L17). |
| `web/src/components/settings/settings-page.tsx` | `SettingsPage` | `title; description?; actions?; children`: the page frame of every settings page (L13). |
| `web/src/components/settings/settings-nav.ts` | `orgSettingsNav` (L13), `projectSettingsNav`, `isProjectSettingsPath` (L15) | Nav entries; each page's phase adds its own entry. |
| `web/src/components/settings/settings-sidebar.tsx` | `SettingsSidebar` | The settings sidebar (L13 org group, L15 project group). |
| `web/src/api/queries-admin.ts` | `adminUsersInit`, `adminUsersKey`, `adminSettingsQuery` | Admin queries (L18, L19). |

## Conventions for every sub-phase

- **Environment.** Run this before any `pnpm` or `just` command:

  ```bash
  export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
  ```
- **Checks.** Run `just fmt`, then `just check-web` until it is green, then `just check`, before committing. Biome's line width is 100, and several code blocks in this plan are wider: `just fmt` rewraps them. Do not commit while either fails. If a failure is outside `web/`, stop and report it.
- **Edit only the files your sub-phase lists.** After adding or moving a route file, run `pnpm gen:routes` (from `web/`) to regenerate `web/src/routeTree.gen.ts`; that file is git-ignored and never committed. Never edit `crates/`, `fixtures/api/`, `web/src/api/schema.gen.ts` or `crates/server/openapi.json`. If you need an API change, stop and report it.
- **No new npm dependencies.** shadcn components are added with the CLI, from `web/`:

  ```bash
  pnpm exec shadcn add --yes <name>
  ```

  They go under `components/ui/` and are committed as generated. L1 adds the only ones this plan uses: `alert-dialog`, `select`, `checkbox`, `radio-group`. If the CLI wants to add an npm package, stop and report.
- **API calls.**
  - Reads use `$api.useQuery` / `useSuspenseQuery` / `useInfiniteQuery` or a query-options helper.
  - Writes use `$api.useMutation(method, path)` from `web/src/api/client.ts`, called as `mutation.mutate({ params, body }, { onSuccess, onError })`. On failure, `mutation.error` is the parsed `Problem`. A `204` resolves with `data` `undefined`.
  - After a write, invalidate the list it changed with `queryClient.invalidateQueries({ queryKey: <the list's queryKey> })`. The queryKey comes from `$api.queryOptions(...)` with the same arguments as the read. Never `setQueryData` a list.
  - `{project}` path values come from `projectRef(org, project)`.
- **Lists.** Every D list is `{data, next_cursor}` with `limit` 50. Settings and admin tables use `$api.useInfiniteQuery` with `pageParamName: "cursor"`, `initialPageParam: null` and `getNextPageParam: (page) => page.next_cursor ?? undefined`, as `web/src/routes/$org/index.tsx` does. A "Load more" button is shown while `hasNextPage`. These tables are plain shadcn `Table`, not the virtualized `DataTable`; they stay small.
- **States.** Every page that loads data has four states:
  - loading: `Skeleton` rows;
  - error: a `401` redirects (D-L-1), and anything else throws to the route's `ErrorState`;
  - empty: `EmptyState` with the copy the phase gives;
  - data.

  Every form has three:
  - idle;
  - pending: `SubmitButton` spinner, fields disabled;
  - error: field errors plus `FormError`.

  On success the page does what the phase says.
- **Forms** always set `noValidate` on the `<form>`, so the browser's own checks (`required`, `type="email"`) never block `onSubmit`. jsdom runs those checks too, and a page test that expects the server's `400` would never reach it. An optional field says so in its label string, for example `label="Name (optional)"`.
- **Search params.** TanStack Router JSON-parses every search value before `validateSearch` sees it, so `?state=123` arrives as the number `123` and `?state=1e5` as `100000`. It also re-serialises the URL: `location.searchStr`, `location.href` and the address bar all carry the rewritten value. Only `useRouter().history.location.search` is the raw query string as the browser sent it. The router also merges the raw search into the validated one (`{ ...raw, ...validated }`), so a key the validator leaves out keeps its raw value in `Route.useSearch()` and in `beforeLoad`'s `search`. **Every `validateSearch` function returns every key it knows, set to `undefined` when the value is rejected** (for example `return { redirect: safeRedirect(search.redirect), error: …, email: … }`), so a rejected value is overwritten. A value that must stay byte-for-byte as sent is read from there: `new URLSearchParams(useRouter().history.location.search)`. Opaque values the UI defines (D-L-9's `state`) must also contain a character other than a digit, so a round trip through `/login?redirect=` cannot change them.
- **Copy.** Use the exact strings the phase gives. They are sentence case, with no trailing period on buttons and labels. Emails and names come from the API and are shown as text. Never interpolate them into HTML.
- **Keyboard and accessibility.**
  - Every form submits with Enter.
  - Every dialog traps focus (Radix does this), closes with Esc unless the phase says otherwise, and returns focus to the button that opened it.
  - Every icon-only button has an `aria-label`.
  - Tables have an `aria-label`.
  - Row actions sit in a `DropdownMenu` whose trigger is a ghost icon button (`MoreHorizontal`) labelled "Actions for <email or name>".
  - Status is never colour alone; a badge has text.
- **Privacy.** Never log anything (`console.*` is not used in `src/`). Never put a secret, a password or a token in a URL, localStorage, sessionStorage or the query cache. The only exceptions are the `/setup?token=` and `/reset?token=` links the server sends, and D-L-9's callback. Do not add analytics.
- **Tests** are Vitest and Testing Library (D74, D132):
  - Render with `renderApp(url)` from `web/src/test/render.tsx`. It returns `{ router, user, queryClient, …view }` (L1 adds `queryClient`).
  - Interact with the returned `user` (`userEvent`).
  - Override endpoints with `server.use(http.<method>("<path with :params>", handler))` from `msw`. `server` is imported from `@/mocks/node`.
  - **Never `vi.mock`.**
  - The fixtures answer every GET in `fixtures/api/manifest.json`. You are signed in as Ada (`ada@example.com`, instance admin, owner of org `acme`). To be signed out, override `GET /api/auth/me` with a `401` problem.
  - Every write a test triggers must be overridden; an unhandled request fails the test (`web/src/test/setup.ts`).
  - Capture the body with `await request.json()` inside the handler and assert it.
  - A problem response is `HttpResponse.json({ type: "about:blank", title, status, code, detail?, errors? }, { status, headers: { "Content-Type": "application/problem+json" } })`. L1 adds `problemResponse(status, code, options?)` in `web/src/test/problem.ts` for this; use it.
  - Pure functions get unit tests next to them (`*.test.ts`).
- **Status line.** Each sub-phase sets the first line of this README to `Status: L1–L<n> done on <date +%F>; L<n+1>–L20 not started` (L20: `Status: L1–L20 done on <date>`), in the same commit.
- **Commit.** One commit per sub-phase, with the subject the phase gives and no body unless the phase asks for one. Then report:
  - what was done;
  - any deviation, and why;
  - anything unverified.

## Pitfalls (found by the plan's reviews)

- **No `JSX.Element`.** React 19's types have no global `JSX` namespace. Leave component return types out, or use `ReactElement` from `react`.
- **Row actions and `mutate` callbacks.** TanStack Query runs the `onSuccess` / `onError` passed to `mutation.mutate(vars, { … })` only for the latest call of that mutation. When several rows share one mutation, either disable every row's actions while `mutation.isPending`, or put the callbacks in `$api.useMutation(method, path, { onSuccess, onError })` (the third argument). Dialog-local callbacks are fine in `mutate` because the dialog blocks everything else.
- **Resetting a dialog's form.** Do not reset state in a `useEffect` on `open`: Biome's `useExhaustiveDependencies` flags the missing mutation, and listing it loops. Put the form in an inner component rendered only while the dialog is open (`{open ? <XForm … /> : null}` inside `DialogContent`), so its state starts fresh on every open. Reset the mutation with `mutation.reset()` in the close handler.
- **Menus that open dialogs, in jsdom.** A Radix `DropdownMenu` leaves `pointer-events: none` on `body` until it finishes closing. If a test's next `user.click` inside the dialog is refused ("unable to perform pointer interaction"), first `await waitFor(() => expect(document.body).not.toHaveStyle("pointer-events: none"))`.
- **Radix `Select` in a dialog, in jsdom.** If `user.click` on the combobox or an option is refused, use the keyboard: focus the combobox, press `{Enter}`, then `{ArrowDown}` to the option and `{Enter}`.
- **`me` is cached.** `meQuery` is fresh for 30 s (`staleTime: Infinity` in tests). A page that must know whether the session still exists right now (`/login`, `/invite/$token`) reads it with `fetchQuery({ ...meQuery, staleTime: 0 })`. After creating an org, refresh it with `await queryClient.refetchQueries({ queryKey: meQuery.queryKey, type: "all" })`: `invalidateQueries` does not refetch a query nobody observes, and loaders use `ensureQueryData`, which ignores staleness.
- **Removing a query the page itself observes** (`useSuspenseQuery(authConfigQuery)`) makes it suspend and refetch. Remove only `meQuery` (`removeQueries({ queryKey: meQuery.queryKey })`), and invalidate the config instead of removing it.
- **Secrets and the mutation cache.** See D-L-5: `{ gcTime: 0 }` on every mutation whose response has a secret (setup, workspace key, personal token, API key, CLI token, invite creation (its `accept_url` holds the invite token), admin user creation and reset links).
- **Test wrapper components are defined at module level**, above the tests, never inside an `it` (Biome `noComponentHookFactories`).
- **Resetting state when a prop changes.** Do not write a `useEffect` whose only dependency is the prop (Biome `useExhaustiveDependencies` reports it as unnecessary). Render the stateful part with `key={prop}` so React remounts it.
- **Elements that appear after a request** are found with `await screen.findBy…`, never `getBy…`, including right after a `waitFor` on the pathname.
- **Radix outside clicks in jsdom.** Radix registers its outside-pointer listener in a `setTimeout(0)` and dismisses only when a `click` follows the `pointerdown`. A test first waits a tick (`await act(async () => { await new Promise((resolve) => setTimeout(resolve, 0)); })`), then fires `fireEvent.pointerDown(document.body)` and `fireEvent.click(document.body)`.
- **Hooks come before any early return.** Every `useState`, `useMutation`, `useQueryClient`, `useNavigate` and query hook of a component is called before its first `if (…) return` or `throw`; a hook added below one changes the hook count between renders.
- **Proving that no request is made.** The fixture mocks answer every GET in `manifest.json`, so "no handler" does not prove that a page skipped a request. Install a counting handler (`let calls = 0; server.use(http.get(path, () => { calls += 1; return …; }))`) and assert `calls === 0`.
- **Passwords and the mutation cache.** A mutation whose *request* carries a password or a token (login, sign-up, setup, reset, password change, invite sign-up) is also created with `{ gcTime: 0 }`, so its `variables` leave TanStack's cache as soon as nothing observes it. Cache tests assert on both `state.data` and `state.variables`, and must not rely on `reset()` alone.
- **Callbacks that must run after the component unmounts** (navigation after sign-out, setup success) go in `$api.useMutation(method, path, { onSuccess / onSettled })`, not in `mutate(vars, { … })`, which drops them on unmount.
- **Redirect tests use a target other than the default landing page** (for example `/acme/support-bot/traces`, not `/acme`), so they fail when the redirect is ignored.
- **`autoFocus`.** Do not use it. Radix dialogs focus their first field, and pages need no autofocus.
- **Radix `Select` and `RadioGroup` give a `string`.** Narrow it before it reaches a typed body: `const next = ORG_ROLES.find((role) => role === value); if (!next) return;`, and the same with a local `as const` list for other enums.
- **Lists without a loader.** A table fed by `useInfiniteQuery` is not loaded when `renderApp` returns. The first assertion on it is `await screen.findByRole("table", { name: … })` (or another `findBy…`), never `getBy…`.
- **Nested `<main>`.** `SidebarInset` and `TopBarLayout` pages already provide the page's landmark. Settings pages render a `<div>`, not a second `<main>`.
- **JSON imports are typed exactly.** To drop a key from a copy of a fixture, destructure it out (`const { name: _name, ...rest } = fixture`) instead of `delete`.

## Test helpers (L1)

`web/src/test/problem.ts`:

```ts
import { HttpResponse } from "msw";
export function problemResponse(
  status: number,
  code: string,
  options?: { detail?: string; errors?: { field: string; message: string }[]; title?: string },
): Response;
```

It answers `HttpResponse.json({ type: "about:blank", title: options?.title ?? <reason phrase>, status, code, ...detail, ...errors }, { status, headers: { "Content-Type": "application/problem+json" } })`. The reason phrases are: 400 "Bad Request", 401 "Unauthorized", 403 "Forbidden", 404 "Not Found", 409 "Conflict", 429 "Too Many Requests", 500 "Internal Server Error", 503 "Service Unavailable".

`web/src/test/fixtures.ts` exports the fixed ids from `fixtures/api/README.md` as constants: `ADA_ID`, `BOB_ID`, `NEW_USER_ID`, `ORG_ID`, `PROJECT_ID`, `API_KEY_ID`, `TOKEN_ID` and `INVITE_ID`. It also exports `INVITE_TOKEN = "EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXA"`.

## How to run the UI by hand

```bash
export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
```

```bash
just web-dev
```

Then open `http://localhost:5173/acme/support-bot/settings/general` (port from Vite's output). The mocks sign you in as Ada. Until L20, writes in the dev server fail with a network error; tests do not depend on that.

## Notes for other tracks

- **G (coordinator):** D-L-2 amends D-G-19. G's M5 no longer builds `/login` or the 401 handling; it checks L's pages against the real server. If G's M5 lands before L5, it must not add a second login page; it waits for L or uses L's branch.
- **H (CLI):** send `host` to `/cli-auth` (D-L-9), and handle `error=access_denied` on the callback.
- **D owners (contract):** CG1–CG4 above.
