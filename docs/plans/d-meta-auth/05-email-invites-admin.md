# Part 5 · Email, invites, verification and instance admin (P19–P22)

---

## P19 — Mailer

**Goal:** SMTP sending with lettre, the minijinja templates of spec 08 § Email, background delivery with retries, and a stub transport for tests.

**Edit only:** `crates/server/src/email/{mod.rs,templates/*}` (new), `crates/server/src/config.rs` (`SmtpUrl`, `email_from` validation), `crates/server/src/http/mod.rs` (`mailer: Option<Mailer>` in `AppInner`), `crates/server/src/serve.rs`, `crates/server/src/test_support.rs`, `crates/server/Cargo.toml`, root `Cargo.toml` (`lettre`, `minijinja`).

**Commit:** `server: add the mailer and email templates`

### Config

- `smtp_url` parses at load into `SmtpUrl { tls: Tls, host, port, credentials: Option<(String, Secret<String>)> }`:
  - `smtp://user:pass@host:587`: STARTTLS required (`Tls::Required`);
  - `smtps://user:pass@host:465`: implicit TLS (`Tls::Wrapper`);
  - `smtp://host:25?tls=none`: no TLS (local relays);
  - default ports 587 / 465 / 25; user and password percent-decoded; any other scheme or query → error naming `smtp_url` (never its value: it holds a password).
- `email_from` is required when `smtp_url` is set, and parses as a mailbox (`"llmobs <noreply@acme.com>"` or a bare address); error names the key.

### `email/mod.rs`

```rust
pub(crate) enum Email {
    Invite { org_name: String, inviter_name: Option<String>, role: OrgRole, url: String },
    PasswordReset { url: String },
    VerifyEmail { url: String },
    Test,
}
pub(crate) struct Mailer { transport: Transport, from: Mailbox, templates: minijinja::Environment<'static> }
enum Transport { Smtp(AsyncSmtpTransport<Tokio1Executor>), #[cfg(test)] Stub(Arc<Mutex<Vec<SentMail>>>) }
impl Mailer {
    pub(crate) fn from_config(config: &ServerConfig) -> Result<Option<Self>, MailError>;
    pub(crate) fn render(&self, email: &Email) -> Result<Rendered, MailError>;      // subject, text, html
    pub(crate) async fn send_now(&self, to: &str, email: &Email) -> Result<(), MailError>;
    pub(crate) fn send_in_background(self: &Arc<Self>, to: String, email: Email);
}
```

- Templates: `templates/{invite,password_reset,verify_email,test}.{txt,html}` loaded with `include_str!` into the environment at construction. The `.html` ones auto-escape (minijinja picks auto-escaping by template name extension). A global `product` holds the name constant of P7. The HTML is the simple inline-styled layout of spec 08 with no external assets.
- Subjects: "You're invited to <org> on llmobs", "Reset your llmobs password", "Verify your email for llmobs", "llmobs test email" (built from the name constant).
- Messages are `multipart/alternative` (text first, then HTML), built with lettre's builder; lettre generates the `Message-ID`.
- `send_in_background`: `tokio::spawn`; up to 3 attempts, waiting 1 s then 2 s between them; logs at `info` (sent) or `warn` (failed after retries) with `template`, `message_id` and `attempt`. Never the recipient, the subject variables or the body (spec 08 § Email, D150).
- `MailError` messages carry no address and no SMTP server text (lettre errors can quote the server's reply, which may contain the address): map to fixed kinds (`Connect`, `Tls`, `Auth`, `Rejected`, `Template`, `InvalidAddress`) and keep the lettre error only as a `#[source]` that is never formatted into responses. Logging the source chain at `warn` is allowed only if it cannot contain an address; if unsure, log the kind only.
- `TestApp::with_mailer()` installs the stub transport and exposes `sent() -> Vec<SentMail { to, subject, text, html }>`.

### Tests

- Each template renders (text and HTML) with fixed inputs, pinned with `insta`.
- An org named `<script>alert(1)</script>` is escaped in the HTML and appears verbatim in the text part.
- `SmtpUrl` parsing: the three forms, default ports, percent-encoded password, `http://…` → error, and the error's `Debug`/`Display` do not contain the password.
- `email_from` missing with `smtp_url` → config error; invalid mailbox → error.
- `send_in_background` with a stub that fails twice then succeeds: one mail sent, logs contain `template` and no recipient (`assert_logs_clean`).
- `Mailer::from_config` without `smtp_url` → `None`; `GET /api/auth/config` reports `smtp_enabled` from the config (already P11; keep it true to the mailer).

### Report

The lettre feature names used.

---

## P20 — Invites

**Goal:** invite by email with a copyable link, accept while signed in or at signup (D147).

**Edit only:** `crates/server/src/meta/invites.rs` (new), `crates/server/src/meta/mod.rs`, `crates/server/src/routes/{invites.rs,auth.rs}` (`invites.rs` new), `crates/server/src/routes/mod.rs`, `crates/server/src/api/` (invite types, `SignupRequest.invite_token`), `crates/server/src/http/access.rs`, `crates/server/src/api/openapi.rs`, `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `fixtures/api/`.

**Commit:** `server: add org invites`

### Store

`replace_invite(conn, org_id, email, role, hash, invited_by, now, expires_at) -> InviteRow` (deletes the pending invite for `(org, email)` first, in the same transaction); `pending_invites(pool, org_id, now)`; `invite_by_hash(pool, hash, now) -> Option<(InviteRow, OrgRow)>` (pending and unexpired only); `delete_invite(conn, org_id, id) -> bool`; `mark_accepted(conn, id, now)`.

### Routes

| Route | Access | Behaviour |
|---|---|---|
| `GET /api/v1/orgs/{org}/invites` | `Org { ManageMembers }` | Page of pending `Invite { id, email, role, invited_by?: MemberUser, created_at, expires_at }`. |
| `POST /api/v1/orgs/{org}/invites` | `Org { ManageMembers, session_only }` | `{email, role}`. Ceiling (D139: no role above the caller's; `owner` only by an owner). Email already a member → 409. Token: `new_opaque_token()`, 7 days. 201 `{invite: Invite, accept_url: "<public_url>/invite/<token>"}`. With a mailer, also `send_in_background(email, Email::Invite{…})`. Audit `invite created` (org, actor, invite ids). |
| `DELETE /api/v1/orgs/{org}/invites/{id}` | `Org { ManageMembers, session_only }` | 204; unknown → 404. |
| `GET /api/auth/invites/{token}` | `Public` | `limits.ip_forms`. Pending and unexpired → 200 `InvitePreview { org: {name, slug}, email, role, expires_at, account_exists }`; else 404. (`account_exists` tells the UI whether to show login or signup; only the token holder learns it.) |
| `POST /api/auth/invites/{token}/accept` | `User { session_only }` | `limits.ip_forms`. Invite not pending/expired → 404. The caller's email ≠ the invite's → 403 `forbidden` ("this invitation is for another email address"). Already a member → mark accepted, 200 with the current membership. Else add the membership with the invite's role, mark accepted, set `email_verified_at` if null, 200 `OrgMembership`. Audit. |

### Signup with an invite (`POST /api/auth/signup`)

`SignupRequest` gains `invite_token?`. With a token:
- `disabled` policy → 403 (registration is closed; invited people need an admin-created account).
- `invite_only` or `open`: the invite must be pending (else 400 field error on `invite_token`: "invalid or expired invitation"); the request's email must equal the invite's (else 400 field error on `email`); `enforce_sso_domains` still applies (403 `sso_required`); `signup_domains` does not (the admin chose this person).
- Create the user verified, add the membership, mark the invite accepted, start a session; 201 `SignupResponse { me, verification_required: false }`.

### Examples

`invites.json`, `invite-created.json`, `invite-preview.json`.

### Tests

- Admin invites a member → link and (with mailer) one mail whose text contains the link; the DB holds the hash only.
- Second invite to the same email replaces the first (old token → 404).
- Admin invites `owner` → 403; owner can.
- Inviting an existing member → 409.
- Preview: pending → 200 with `account_exists`; expired (advance 7 days + 1 s) → 404; deleted → 404.
- Accept signed in with the right email → membership; with another email → 403; twice → 200 idempotent; with a PAT → 403 (session-only).
- Signup with an invite under `invite_only` → 201, member, verified; wrong email → 400; under `disabled` → 403; domain outside `signup_domains` still accepted.
- `assert_logs_clean` with the token and the email.

### Report

Standard.

---

## P21 — Forgot password and email verification

**Goal:** password reset by email, and the verification flow behind `require_email_verification` (D145, D146).

**Edit only:** `crates/server/src/meta/email_verifications.rs` (new), `crates/server/src/meta/mod.rs`, `crates/server/src/auth/reset.rs`, `crates/server/src/routes/auth.rs`, `crates/server/src/api/account.rs`, `crates/server/src/http/access.rs`, `crates/server/src/api/openapi.rs`, `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `fixtures/api/`.

**Commit:** `server: add password reset and email verification by email`

### Routes

| Route | Access | Behaviour |
|---|---|---|
| `POST /api/auth/password/forgot` | `Public` | `{email}`. No mailer → 400 `bad_request` ("email is not configured"), whatever the email. Password login disabled → 403. Else always **204**: `limits.ip_forms` (429 is allowed: it is per IP, not per account); the per-email limiter `limits.email_mail` silently skips sending when exceeded; the email is valid, the user exists and is not disabled → `reset_link` (P13) and `send_in_background(Email::PasswordReset)`. |
| `POST /api/auth/email/verify` | `Public` | `{token}`. Valid, unused, unexpired → `email_verified_at = now`, 204. Else 400 ("invalid or expired link"). |
| `POST /api/auth/email/verify/resend` | `Public` | `{email}`. Always 204 with the same limits as forgot; sends only for an existing, unverified, enabled user. No mailer → 400. |

Verification tokens: `new_opaque_token()`, 24 hours, table `email_verifications`; creating one deletes the user's older unused ones. Link: `<public_url>/verify?token=<token>`.

Signup (P12) with `require_email_verification`: now creates a verification token and sends `Email::VerifyEmail` in the background.

### Tests

- Forgot for an existing user → one mail with a reset link that works with `POST /password/reset`; for an unknown email → 204 and no mail; both answers byte-identical; the 4th request in an hour for one email sends nothing and still answers 204; without a mailer → 400 for both.
- Verification: signup with verification required → no session, one mail; login → 403 `email_not_verified`; verify → 204; login → 200; token reuse → 400; expired after 24 h → 400; resend for a verified user → 204 and no mail.
- `assert_logs_clean` with the emails and tokens.

### Report

Standard.

---

## P22 — Instance admin endpoints

**Goal:** `/api/v1/admin/users` and `/api/v1/admin/settings` (D149).

**Edit only:** `crates/server/src/routes/admin.rs` (new), `crates/server/src/routes/mod.rs`, `crates/server/src/api/` (new `admin.rs` types), `crates/server/src/meta/users.rs`, `crates/server/src/http/access.rs`, `crates/server/src/api/openapi.rs`, `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `fixtures/api/`.

**Commit:** `server: add the instance admin endpoints`

All routes are `Access::InstanceAdmin` (session only).

| Route | Behaviour |
|---|---|
| `GET /api/v1/admin/users` | Page by email of `AdminUser { id, email, name?, is_instance_admin, disabled, email_verified, has_password, providers: [string], created_at }`. |
| `POST /api/v1/admin/users` | `{email, name?, is_instance_admin?}` → creates a verified user without a password; 201 `{user: AdminUser, reset_url}` (1-hour link to set the password). Duplicate → 409. Audit. |
| `PATCH /api/v1/admin/users/{id}` | `{disabled?, is_instance_admin?}`. On oneself → 400. Leaving no active instance admin → 409. Disabling the last active (not disabled) owner of any org → 409 ("the user is the last owner of an org"). Disabling deletes the user's web sessions; PATs fail at once (P17 loads the user per request). Re-enabling clears `disabled_at`. 200 `AdminUser`. Audit. |
| `POST /api/v1/admin/users/{id}/reset-link` | 200 `{url}`; disabled user → 409. Audit (ids only). |
| `GET /api/v1/admin/settings` | 200 `AdminSettings { public_url, signup, signup_domains, password_enabled, email_verification_required, enforce_sso_domains, smtp_enabled, email_from?, providers: [AuthProvider], trusted_proxies: [string] }`. No secret: no SMTP URL, no client secrets. |
| `POST /api/v1/admin/settings/test-email` | No mailer → 400. Sends `Email::Test` to the caller's own address with `send_now`; 204, or 503 `busy` with the fixed detail "the SMTP server could not be reached or refused the message". |

### Examples

`admin-users.json`, `admin-user-created.json`, `admin-settings.json`.

### Tests

- A non-admin → 403 on each route (also covered by P18 once these routes exist: re-run it; it picks them up from the table).
- Create → reset link works; duplicate → 409.
- Disable another user → their session and PAT get 401; their login → 401 (uniform); re-enable → login works.
- Disable or demote oneself → 400. Demoting another admin → 200 (the caller remains). Because the caller is always an active admin, the HTTP path can never empty the admin set; unit-test the "would leave no active instance admin" check on the store with crafted rows instead (the CLI or a raw DB change could otherwise reach it).
- Last owner: a user who is the only owner of org A → disable → 409.
- Settings: no secret fields; `assert_logs_clean` with the SMTP password from config.
- Test email with the stub mailer → one mail to the caller.

### Report

Standard.
