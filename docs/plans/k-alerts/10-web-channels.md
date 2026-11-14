# K10 — Web: alert API helpers and the Channels settings page

**Commit subject:** `web: add the notification channels settings page`

Adds the query helpers every alert page uses, and the project settings page `/$org/$project/settings/channels`: the channel list with its last delivery, a create/edit dialog per kind, delete, and "Send test" (D-K-5, D-K-6, D-K-7, D-K-9). The pages run on MSW mocks built from `fixtures/api` (K5).

## Read first

- README § Design, D-K-5, D-K-6, D-K-7, D-K-9.
- `docs/plans/l-admin-ui/README.md` § Decisions (D-L-4 role gating, D-L-6 settings area, D-L-10 writes mocked per test) and § Conventions.
- `web/src/routes/$org/$project/settings/api-keys.tsx` and `api-keys.test.tsx` (the model page and test), `web/src/components/settings/create-key-dialog.tsx` (dialog form), `settings-nav.ts`, `settings-sidebar.tsx`, `settings-page.tsx`.
- `web/src/api/queries-settings.ts` (`apiKeysInit`, `apiKeysKey`), `web/src/api/client.ts` (`$api`), `web/src/api/problem.ts` (`problemOf`, `fieldErrors`, `problemMessage`), `web/src/api/types.ts` (`Schema`), `web/src/api/project-ref.ts`.
- `web/src/components/form/text-field.tsx`, `form-error`, `submit-button`, `web/src/components/confirm-dialog.tsx`, `empty-state.tsx`, `forbidden.tsx`, `disabled-reason.tsx`, `time-text.tsx`, `web/src/components/ui/{badge,dialog,dropdown-menu,radio-group,table,textarea,tooltip}.tsx`.
- `web/src/lib/roles.ts` (`atLeast`), `web/src/lib/format.ts` (`formatRelative`).
- `web/src/test/render.tsx` (`renderApp`), `web/src/test/problem.ts` (`problemResponse`), `web/src/test/fixtures.ts`, `web/src/test/setup.ts` (unhandled requests fail a test).
- `fixtures/api/static/channels.json` (K5).

## Files

| File | Change |
|---|---|
| `web/src/api/queries-alerts.ts` | new |
| `web/src/components/settings/settings-nav.ts` | the Channels entry |
| `web/src/routes/$org/$project/settings/channels.tsx` | new page |
| `web/src/routes/$org/$project/settings/channels.test.tsx` | new |
| `web/src/components/alerts/channel-dialog.tsx` | new |
| `web/src/components/alerts/headers-editor.tsx` | new |
| `web/src/test/fixtures.ts` | channel and rule ids |

## 1. `web/src/api/queries-alerts.ts`

```ts
import { $api } from "@/api/client";
import { projectRef } from "@/api/project-ref";

export function channelsInit(org: string, project: string) {
  return { params: { path: { project: projectRef(org, project) } } };
}

export function channelsQuery(org: string, project: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/channels", channelsInit(org, project));
}

export function alertRulesQuery(org: string, project: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/alert-rules", {
    params: { path: { project: projectRef(org, project) } },
  });
}

// alertRuleQuery, alertEventsInit and alertEventsKey are used by K11 and K12.
export function alertRuleQuery(org: string, project: string, id: string) {
  return $api.queryOptions("get", "/api/v1/projects/{project}/alert-rules/{id}", {
    params: { path: { project: projectRef(org, project), id } },
  });
}

export function alertEventsInit(org: string, project: string, id: string) {
  return { params: { path: { project: projectRef(org, project), id }, query: { limit: 50 } } };
}

export function alertEventsKey(org: string, project: string, id: string) {
  return $api.queryOptions(
    "get",
    "/api/v1/projects/{project}/alert-rules/{id}/events",
    alertEventsInit(org, project, id),
  ).queryKey;
}
```

Run `just fmt` (Biome) after writing; the code above may be reflowed.

## 2. Settings nav

`settings-nav.ts`: append `{ to: "/$org/$project/settings/channels", label: "Channels", adminOnly: false }` to `projectSettingsNav` (after Members). Viewers see it (they may read channels); the page disables writes for them. `settings-nav.test.ts` needs no change (it tests only `isProjectSettingsPath`); test 2 covers the entry.

`web/src/test/fixtures.ts`: add

```ts
export const SLACK_CHANNEL_ID = "01900000-0000-7000-8000-000000000500";
export const WEBHOOK_CHANNEL_ID = "01900000-0000-7000-8000-000000000501";
export const EMAIL_CHANNEL_ID = "01900000-0000-7000-8000-000000000502";
export const FIRING_RULE_ID = "01900000-0000-7000-8000-000000000510";
export const SCORE_RULE_ID = "01900000-0000-7000-8000-000000000511";
export const COST_RULE_ID = "01900000-0000-7000-8000-000000000512";
```

## 3. The page (`settings/channels.tsx`)

```tsx
export const Route = createFileRoute("/$org/$project/settings/channels")({
  component: ChannelsPage,
});
```

`ChannelsPage`:
- `const { org, project, data } = useProject(); const canEdit = atLeast(data.role, "member"); const reason = "Only project members and above can manage channels";`
- `const query = useQuery(channelsQuery(org, project));` (`useQuery` from `@tanstack/react-query`); `const channels = query.data?.data ?? [];`.
- Dialog state follows `api-keys.tsx`: for the create/edit dialog, the delete dialog and the test result, keep a target that is never cleared on close plus an `open` boolean, so a dialog does not blank while it animates out.
- A `403` from the query (`problemOf(query.error)?.status === 403`) → `<Forbidden description="You don't have access to this project's channels." />`; another error is thrown (as `api-keys.tsx` does).
- `<SettingsPage title="Notification channels" description="Where alert rules send their notifications." actions={…}>`. The action is a `Button` "New channel" with a `Plus` icon, `disabled={!canEdit}`, wrapped in `DisabledReason reason={canEdit ? undefined : reason}`; it opens `ChannelDialog` in create mode.
- Loading: three `<Skeleton className="h-8 w-full" />`.
- Empty (`channels.length === 0`): `<EmptyState icon={BellRing} title="No channels yet" description="Add Slack, a webhook or email to receive alert notifications." />`.
- Otherwise a `Table aria-label="Notification channels"` with columns **Name**, **Kind**, **Target**, **Last delivery**, and an actions column (`<span className="sr-only">Actions</span>`):
  - Kind: `Badge variant="outline"` with `Slack`, `Webhook` or `Email`. When `available` is false, a second `Badge variant="destructive"` reading `SMTP not configured`, with `title="Email channels need SMTP (smtp_url). Ask an instance admin."`.
  - Target: `target_hint` in `font-mono text-xs`.
  - Last delivery: absent → `—`; else `Badge` (`sent` → `secondary` reading `Sent`, `failed` → `destructive` reading `Failed`) followed by `<TimeText value={last_delivery.at} style="datetime" />`; when `error` is present, a line below in `text-xs text-muted-foreground` with the error text.
  - Actions: a `DropdownMenu` with `Button variant="ghost" size="icon-xs" aria-label={`Actions for ${channel.name}`}`, items **Send test**, **Edit**, **Delete** (`className="text-destructive"`). For a viewer the trigger is disabled and the whole `DropdownMenu` is wrapped in `DisabledReason` (as `org-members.tsx` does); the menu never opens, so there are no disabled items inside it (D-L-4).
- **Edit** opens `ChannelDialog` in edit mode with the channel.
- **Delete** opens `ConfirmDialog` titled `Delete channel?`, description `` `${name}` will be removed from every alert rule that uses it. ``, `confirmLabel="Delete"`, `destructive`, `pending={remove.isPending}`. It calls `const remove = $api.useMutation("delete", "/api/v1/projects/{project}/channels/{id}")`; on success: close, `remove.reset()`, invalidate `channelsQuery(org, project).queryKey` and `alertRulesQuery(org, project).queryKey` (the second has no observer on this page, so it fetches nothing). Its error shows through `ConfirmDialog`'s `error` prop with `problemMessage(remove.error, "Could not delete the channel.")`.
- **Send test**: one `const test = $api.useMutation("post", "/api/v1/projects/{project}/channels/{id}/test")` for the page. When it settles it opens a `Dialog` titled `Test notification` whose `DialogDescription` reads the outcome: `ok` → "The channel accepted the test message."; `ok: false` → "The channel did not accept the test message:" followed by the `error` in a `FormError`; a request error → `problemMessage(error, "Could not send the test.")`. A `DialogFooter` has `Button variant="outline"` "Close"; closing calls `test.reset()`. While pending, the row whose id equals `test.variables?.params.path.id` shows a spinning `Loader2` instead of `MoreHorizontal` in its actions trigger.

## 4. `components/alerts/channel-dialog.tsx`

```tsx
export function ChannelDialog(props: {
  org: string;
  project: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Absent: create. Present: edit this channel. */
  channel?: Schema<"NotificationChannel">;
})
```

No return type annotation (React 19 has no global `JSX` namespace; the repo never writes one). The title is `New channel` or `Edit channel`, with a `DialogDescription` ("Alert rules send their notifications to channels." / "Fields you leave empty keep their current value."). Both mutations, `const create = $api.useMutation("post", "/api/v1/projects/{project}/channels")` and `const update = $api.useMutation("patch", "/api/v1/projects/{project}/channels/{id}")`, live in `ChannelDialog` and are passed to the inner form; `pending = create.isPending || update.isPending`; both are reset when the dialog closes. Like `CreateKeyDialog`, the form component is mounted only while open (so its state resets), and the dialog refuses to close while pending.

Form state (`useState`): `name`, `kind` (`Schema<"ChannelKind">`, create only; default `"slack"`), `url`, `secret`, `headers` (`{ id: number; name: string; value: string }[]`; `id` comes from a counter so React keys are stable; on create the list starts empty), `headersTouched`, `recipients` (a string, one address per line), field errors, and a form error.

Fields (all `TextField` unless noted; ids prefixed `channel-`; `TextField` descriptions are plain strings, so write them without backticks):
- **Name** (`maxLength 100`).
- **Kind**: a `Label id="channel-kind-label"` "Kind" and a `RadioGroup aria-labelledby="channel-kind-label"` with items Slack, Webhook, Email, each with an `id` and a `Label htmlFor` as in `create-key-dialog.tsx` (create only; in edit mode a read-only line `Kind: Slack`). A `kind` error renders below it as `<p id="channel-kind-error" className="text-xs text-destructive">`.
- Slack: **Webhook URL** (`id="channel-url"`, `TextField`'s default text type, `autoComplete="off"`), description "The Slack incoming webhook URL (https://hooks.slack.com/…)." In edit mode it starts empty, with no placeholder, and the description reads `Leave empty to keep the current URL (${channel.target_hint}).`
- Webhook: **URL** (`autoComplete="off"`); **Signing secret** (`type="password"`, `autoComplete="new-password"`, description "16 to 256 characters. Verify the X-Llmobs-Signature header with it."); in edit mode both start empty with the description "Leave empty to keep the current one."; **Headers** with `HeadersEditor` (§ 5). In edit mode the editor starts with one row per `header_names` entry, value empty, and the description "Values are not shown. Saving replaces every header; re-enter the values." Any change in the editor sets `headersTouched`.
- Email: **Recipients**: a `Label htmlFor="channel-recipients"` "Recipients" and a `Textarea id="channel-recipients"` with `aria-describedby` (`channel-recipients-description`, plus `channel-recipients-error` when there is an error) and `aria-invalid`; the description `<p id="channel-recipients-description" className="text-xs text-muted-foreground">One address per line, at most 20.</p>` and the error `<p id="channel-recipients-error" className="text-xs text-destructive">`, mirroring `text-field.tsx`. In edit mode it starts with `channel.recipients.join("\n")`. On create with kind email, always show `<p className="text-xs text-muted-foreground">Email needs SMTP on the server. Without it the channel cannot be created.</p>`.

Submit:
- Client checks: name not blank (`"Enter a name."`); on create, `url` required for Slack and webhook (`"Enter a URL."`), `secret` required for webhook (`"Enter a signing secret."`), at least one recipient for email (`"Enter at least one address."`). These are presence checks only (D-L-11); the server checks formats.
- Values: `name`, `url` and `secret` are trimmed, and whitespace-only counts as empty. Recipients are split on newlines, trimmed, empties dropped. `headers` is `Object.fromEntries` of the rows whose `name.trim()` is not empty (name trimmed, value unchanged; a later duplicate name wins); removing every row gives `{}`.
- Create body (`Schema<"CreateChannelRequest">`): `{ name, kind }` plus only the kind's fields: Slack `{ url }`; webhook `{ url, secret, headers }`; email `{ recipients }`.
- Edit body (`Schema<"UpdateChannelRequest">`): `name` when it differs from `channel.name`; Slack/webhook `url` when non-empty; webhook `secret` when non-empty, `headers` when `headersTouched`; email `recipients` when `JSON.stringify` of the list differs from `JSON.stringify(channel.recipients)`. A body with no key closes the dialog without a request.
- On success: invalidate `channelsQuery(org, project).queryKey`, close.
- Errors: `fieldErrors(error)` mapped to `name`, `kind`, `url`, `secret`, `headers`, `recipients` (the `headers` error shows under the editor). When any mapped field error exists, set no form error. Otherwise a `409` → form error `problemMessage(error, "A channel with this name already exists.")`; a `403` → `"Only project members and above can manage channels."`; anything else `problemMessage(error, "Could not save the channel.")`.
- Buttons: `Cancel` (`variant="outline"`) and `SubmitButton` reading `Create channel` or `Save`.

## 5. `components/alerts/headers-editor.tsx`

```tsx
export type HeaderRow = { id: number; name: string; value: string };

export function HeadersEditor(props: {
  rows: HeaderRow[];
  onChange: (rows: HeaderRow[]) => void;
  /** Creates the id of a new row. */
  nextId: () => number;
  error?: string;
  disabled?: boolean;
})
```

A `fieldset` with legend "Headers"; one line per row with two `Input`s (`aria-label="Header name 1"` and `"Header value 1"`, numbered from 1; the value input is `type="password"`, as header values often hold tokens) and a remove `Button variant="ghost" size="icon-xs" aria-label="Remove header 1"`; an "Add header" button (disabled at 10 rows). Rows use `key={row.id}`, never the index (Biome's `noArrayIndexKey`). The error renders below as `<p id="channel-headers-error" className="text-xs text-destructive">` (no `role`: `FormError` is the only alert in the dialog), and the inputs get `aria-invalid` when there is an error.

## 6. Tests (`settings/channels.test.tsx`)

Use `renderApp("/acme/support-bot/settings/channels")`; the default MSW handlers serve `channels.json` and the project as `owner`. Override per test with `server.use`; capture write bodies with `await request.json()`; never `vi.mock`. Copy the `projectAs` helper and the fixture imports (`../../../../../../fixtures/api/static/project.json` and `channels.json`) from `api-keys.test.tsx`. Fixtures are typed with `noUncheckedIndexedAccess`: read rows as `const webhook = channelsFixture.data[1];` and spread them. After clicking a row action, wait for the menu to close (`await waitFor(() => expect(screen.queryByRole("menu")).toBeNull())`) before clicking inside the dialog, as `api-keys.test.tsx` does. Count requests with `server.events.on("request:start", …)` and remove the listener at the end of the test, as `filter-bar.test.tsx` does.

1. `lists channels with their last delivery`: the table shows `On-call email`, `Pager webhook`, `Team Slack` in that order; `hooks.slack.com/…/abcd` (copy it from the fixture: `…` is U+2026); `Sent` and `Failed` badges; `the endpoint answered 500`; the email row shows `—`.
2. `shows the Channels entry in the settings nav` with `aria-current="page"`.
3. `creates a webhook channel`: click "New channel"; in the dialog named `New channel` type the name `Pager`, choose Webhook, type the URL `https://hooks.example.com/x` and the "Signing secret" `0123456789abcdef`, click "Add header" and type `X-Team` / `core`, submit → the POST body equals `{ name: "Pager", kind: "webhook", url: "https://hooks.example.com/x", secret: "0123456789abcdef", headers: { "X-Team": "core" } }`; the response (`201`, `channelsFixture.data[1]`) closes the dialog and the list GET count goes from 1 to 2.
4. `creates an email channel`: recipients `"a@x.io\n\n b@x.io "` → body `recipients: ["a@x.io", "b@x.io"]`.
5. `shows field errors from the server`: type name `Team` and URL `https://x.test/hook`, submit; the POST answers `problemResponse(400, "bad_request", { errors: [{ field: "url", message: "must use https" }] })` → the URL input has `aria-invalid="true"`, the message is visible, and no form error is shown.
6. `edits without resending secrets`: Edit on `Pager webhook`, change only the name → PATCH body `{ name: "Pager 2" }` exactly.
7. `deletes a channel`: Delete → confirm in the `alertdialog` named `Delete channel?` → DELETE sent to `…/channels/01900000-0000-7000-8000-000000000501`, answered `204`; the dialog closes.
8. `sends a test`: POST `…/test` answered `{ ok: false, error: "the endpoint answered 500" }` → the dialog `Test notification` shows "The channel did not accept the test message:" and the error; Close; then a handler answering `{ ok: true }` → "The channel accepted the test message."
9. `viewers can read but not write`: `projectAs("viewer")` → the list shows; "New channel" and the button `Actions for Team Slack` are disabled; with counters on `http.post`, `http.patch` and `http.delete` for `/api/v1/projects/:project/channels*`, clicking both sends nothing (counts stay 0).
10. `email without SMTP is flagged`: the list answers `{ ...channelsFixture, data: [{ ...channelsFixture.data[0], available: false }] }` → the `SMTP not configured` badge is visible.
11. `forbidden and empty states`: the list answering `problemResponse(403, "forbidden")` → "You don't have access to this project's channels."; an empty list → "No channels yet".
12. `delete failure keeps the dialog open`: DELETE answers `problemResponse(500, "internal")` → the dialog stays open and shows the problem's title.
13. `a duplicate name is a conflict`: POST answers `problemResponse(409, "conflict", { detail: "a channel with this name already exists" })` → that detail is shown.
14. `edit shows header names, not values`: Edit on `Pager webhook` → "Header name 1" has value `X-Team` and "Header value 1" is empty; saving after only a name change sends no `headers`; after typing a value `abc`, the PATCH has `headers: { "X-Team": "abc" }`.
15. `client checks`: submit an empty create form → "Enter a name." and "Enter a URL." are shown and no POST is sent (counter stays 0).

## Commands

```bash
export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
```

```bash
pnpm --dir web gen:routes
```

```bash
just fmt
```

```bash
pnpm --dir web test -- channels
```

```bash
just check-web
```

```bash
just check
```

## Done when

- `/acme/support-bot/settings/channels` works on the mocks as described (writes only in tests; K12 adds dev write mocks); the 15 tests pass; `just check` is green.
