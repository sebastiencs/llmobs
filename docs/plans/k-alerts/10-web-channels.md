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
| `web/src/components/settings/settings-nav.ts` (+ its test) | the Channels entry |
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

`settings-nav.ts`: append `{ to: "/$org/$project/settings/channels", label: "Channels", adminOnly: false }` to `projectSettingsNav` (after Members). Viewers see it (they may read channels); the page disables writes for them. Update `settings-nav.test.ts` if it lists the entries.

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
- `const query = useQuery(channelsQuery(org, project));` (`useQuery` from `@tanstack/react-query`).
- A `403` from the query (`problemOf(query.error)?.status === 403`) → `<Forbidden description="You don't have access to this project's channels." />`; another error is thrown (as `api-keys.tsx` does).
- `<SettingsPage title="Notification channels" description="Where alert rules send their notifications." actions={…}>`. The action is a `Button` "New channel" with a `Plus` icon, `disabled={!canEdit}`, wrapped in `DisabledReason reason={canEdit ? undefined : reason}`; it opens `ChannelDialog` in create mode.
- Loading: three `<Skeleton className="h-8 w-full" />`.
- Empty (`data.length === 0`): `<EmptyState icon={BellRing} title="No channels yet" description="Add Slack, a webhook or email to receive alert notifications." />`.
- Otherwise a `Table aria-label="Notification channels"` with columns **Name**, **Kind**, **Target**, **Last delivery**, and an actions column (`<span className="sr-only">Actions</span>`):
  - Kind: `Badge variant="outline"` with `Slack`, `Webhook` or `Email`. When `available` is false, a second `Badge variant="destructive"` reading `SMTP not configured`, with `title="Email channels need SMTP (smtp_url). Ask an instance admin."`.
  - Target: `target_hint` in `font-mono text-xs`.
  - Last delivery: absent → `—`; else `Badge` (`sent` → `secondary` reading `Sent`, `failed` → `destructive` reading `Failed`) followed by `<TimeText value={last_delivery.at} style="datetime" />`; when `error` is present, a line below in `text-xs text-muted-foreground` with the error text.
  - Actions: a `DropdownMenu` with `Button variant="ghost" size="icon-xs" aria-label={`Actions for ${channel.name}`}`, items **Send test**, **Edit**, **Delete** (`className="text-destructive"`). For a viewer the trigger is disabled with the `DisabledReason`.
- **Edit** opens `ChannelDialog` in edit mode with the channel.
- **Delete** opens `ConfirmDialog` titled `Delete channel?`, description `` `${name}` will be removed from every alert rule that uses it. `` , `confirmLabel="Delete"`, `destructive`. It calls `$api.useMutation("delete", "/api/v1/projects/{project}/channels/{id}")`; on success: close, `reset()`, invalidate `channelsQuery(org, project).queryKey` and `alertRulesQuery(org, project).queryKey`. Its error shows through `ConfirmDialog`'s `error` prop with `problemMessage(error, "Could not delete the channel.")`.
- **Send test** calls `$api.useMutation("post", "/api/v1/projects/{project}/channels/{id}/test")` and, when it settles, opens a `Dialog` titled `Test notification`: `ok` → "The channel accepted the test message." ; `ok: false` → "The channel did not accept the test message:" plus the `error` in a `FormError`; a request error → `problemMessage(error, "Could not send the test.")`. While pending, the row's actions trigger shows `Loader2` spinning. After the test, invalidate `channelsQuery` (the last delivery does not change, but keep the list fresh).

## 4. `components/alerts/channel-dialog.tsx`

```tsx
export function ChannelDialog(props: {
  org: string;
  project: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Absent: create. Present: edit this channel. */
  channel?: Schema<"NotificationChannel">;
}): JSX.Element;
```

Like `CreateKeyDialog`: the form component is mounted only while open (so its state resets), and the dialog refuses to close while the mutation is pending.

Form state (`useState`): `name`, `kind` (`"slack" | "webhook" | "email"`, create only; default `"slack"`), `url`, `secret`, `headers` (`{ name: string; value: string }[]`), `recipients` (a string, one address per line), field errors, and a form error.

Fields (all `TextField` unless noted; ids prefixed `channel-`):
- **Name** (`maxLength 100`).
- **Kind**: `RadioGroup` labelled "Kind" with Slack, Webhook, Email (create only; in edit mode a read-only line `Kind: Slack`).
- Slack: **Webhook URL** (`url`), description "The Slack incoming webhook URL (https://hooks.slack.com/…)." In edit mode the field starts empty with description "Leave empty to keep the current URL (`<target_hint>`)."
- Webhook: **URL**; **Signing secret** (`type="password"`, description "16 to 256 characters. Verify `X-Llmobs-Signature` with it." — write the header name with the product constant if `web/src/product.ts` exports one, else literally); in edit mode both start empty with "Leave empty to keep the current one."; **Headers** with `HeadersEditor` (below). In edit mode the editor starts with one row per `header_names` entry, value empty, and the description "Values are not shown. Saving replaces every header; re-enter the values." Headers are sent only when the user changed the editor (track a `headersTouched` flag).
- Email: **Recipients** (`Textarea`, label "Recipients", description "One address per line, at most 20."), starting with `channel.recipients.join("\n")` in edit mode. When the email kind is selected on create, show a note "Email needs SMTP on the server. Without it the channel cannot be created." only if there is no other signal (the UI cannot read SMTP state; keep the note static).

Submit:
- Client checks: name not blank (`"Enter a name."`); on create, `url` required for Slack and webhook (`"Enter a URL."`), `secret` required for webhook (`"Enter a signing secret."`), at least one recipient for email (`"Enter at least one address."`). Recipients are split on newlines, trimmed, empties dropped.
- Create body (`Schema<"CreateChannelRequest">`): `{ name, kind }` plus only the kind's fields: Slack `{ url }`; webhook `{ url, secret, headers }` where `headers` is an object from the non-empty rows; email `{ recipients }`.
- Edit body (`Schema<"UpdateChannelRequest">`): `name` when changed; Slack/webhook `url` when non-empty; webhook `secret` when non-empty, `headers` when touched; email `recipients` when changed.
- Mutations: `$api.useMutation("post", "/api/v1/projects/{project}/channels")` or `("patch", "/api/v1/projects/{project}/channels/{id}")`. On success: invalidate `channelsQuery(org, project).queryKey`, close.
- Errors: `fieldErrors(error)` mapped to the fields `name`, `kind`, `url`, `secret`, `headers`, `recipients` (the `headers` error shows under the editor); a `409` → form error `problemMessage(error, "A channel with this name already exists.")`; a `403` → `"Only project members and above can manage channels."`; anything else `problemMessage(error, "Could not save the channel.")`.
- Buttons: `Cancel` (`variant="outline"`) and `SubmitButton` reading `Create channel` or `Save`.

## 5. `components/alerts/headers-editor.tsx`

```tsx
export function HeadersEditor(props: {
  rows: { name: string; value: string }[];
  onChange: (rows: { name: string; value: string }[]) => void;
  error?: string;
  disabled?: boolean;
}): JSX.Element;
```

A `fieldset` with legend "Headers"; one line per row with two `Input`s (`aria-label="Header name 1"`, `"Header value 1"`, numbered from 1) and a remove `Button variant="ghost" size="icon-xs" aria-label="Remove header 1"`; an "Add header" button (disabled at 10 rows). The error renders below with `role="alert"` like `TextField`'s.

## 6. Tests (`settings/channels.test.tsx`)

Use `renderApp("/acme/support-bot/settings/channels")`; the default MSW handlers serve `channels.json` and the project as `owner`. Override per test with `server.use`; capture write bodies with `await request.json()`; never `vi.mock`.

1. `lists channels with their last delivery`: the table shows `On-call email`, `Pager webhook`, `Team Slack` in that order; `hooks.slack.com/…/abcd`; `Sent` and `Failed` badges; `the endpoint answered 500`.
2. `shows the Channels entry in the settings nav` with `aria-current="page"`.
3. `creates a webhook channel`: click "New channel", type the name, choose Webhook, type URL and secret, add a header `X-Team` / `core`, submit → the POST body equals `{ name: "Pager", kind: "webhook", url: "https://hooks.example.com/x", secret: "0123456789abcdef", headers: { "X-Team": "core" } }`; the response (`201`, the webhook object from `channels.json`) closes the dialog and the list is refetched (count GET calls with `server.events.on("request:start", …)` or a counter in a `server.use` handler).
4. `creates an email channel`: recipients `"a@x.io\n\n b@x.io "` → body `recipients: ["a@x.io", "b@x.io"]`.
5. `shows field errors from the server`: POST answers `problemResponse(400, "bad_request", { errors: [{ field: "url", message: "must use https" }] })` → the URL input has `aria-invalid="true"` and the message is visible.
6. `edits without resending secrets`: Edit on `Pager webhook`, change only the name → PATCH body `{ name: "Pager 2" }` exactly.
7. `deletes a channel`: Delete → confirm → DELETE sent to `…/channels/01900000-0000-7000-8000-000000000501`, answered `204`; the dialog closes.
8. `sends a test`: POST `…/test` answered `{ ok: false, error: "the endpoint answered 500" }` → the result dialog shows the error; answered `{ ok: true }` → "The channel accepted the test message."
9. `viewers can read but not write`: project role `viewer` (override `GET /api/v1/projects/:project` with `{ ...projectFixture, role: "viewer" }`) → the list shows, "New channel" is disabled, and no write is sent.
10. `email without SMTP is flagged`: override the list with one email channel with `available: false` → the `SMTP not configured` badge is visible.

## Commands

```bash
export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
```

```bash
just fmt
```

```bash
pnpm --dir web test -- channels
```

```bash
just check
```

## Done when

- `/acme/support-bot/settings/channels` works on the mocks as described; the 10 tests pass; `just check` is green.
