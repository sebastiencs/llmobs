# Part 3 · Sessions, access control, setup and login (P9–P13)

At the end of this part a user can set up the instance, sign up (open policy), log in and out, reset a password with a link, and every route is protected by the route table.

---

## P9 — Web sessions

**Goal:** create, look up, slide and delete web sessions, and build the session cookie (spec 08 § Web sessions).

**Edit only:** `crates/server/src/meta/web_sessions.rs` (new), `crates/server/src/meta/mod.rs`, `crates/server/src/auth/session.rs` (new), `crates/server/src/auth/mod.rs`, `crates/server/src/serve.rs` (purge task), `crates/server/Cargo.toml`, root `Cargo.toml` (`axum-extra` with `cookie`).

**Commit:** `server: add web sessions`

### Store (`meta/web_sessions.rs`)

- `insert_session(conn, NewSession { hash, user_id, user_agent, ip }, now, expires_at)`.
- `session_user(pool, hash, now) -> Option<SessionRow { hash, user: UserRow, last_seen_at, expires_at }>`: joins `users`; returns `None` when `expires_at <= now` or the user has `disabled_at` set.
- `touch_session(conn, hash, now, expires_at)`; `delete_session(conn, hash)`; `delete_user_sessions(conn, user_id, keep: Option<&str>)`; `purge_expired_sessions(conn, now) -> u64`.
- `user_agent` is cut to 256 characters before insert; `ip` is the client IP of D140 as text.

### Cookie and lifecycle (`auth/session.rs`)

- `SESSION_COOKIE`: `"llmobs_session"`, built from the single name constant of P7.
- `LIFETIME = 30 days`, `REFRESH_AFTER = 1 hour`.
- `pub(crate) async fn start_session(state, user_id, user_agent, ip) -> Result<HeaderValue, ApiError>`: `new_opaque_token()`, insert its hash, return the `Set-Cookie` value: `llmobs_session=<token>; Path=/; HttpOnly; SameSite=Lax; Max-Age=2592000`, plus `; Secure` when `public_url` is https.
- `pub(crate) fn clear_cookie(state) -> HeaderValue` (`Max-Age=0`, same attributes).
- `pub(crate) async fn resolve_session(state, cookie_value, now) -> Result<Option<(SessionRow, Option<HeaderValue>)>, ApiError>`: rejects values that are not opaque tokens without a DB call; looks up by hash; when `now - last_seen_at >= REFRESH_AFTER`, slides (`last_seen_at = now`, `expires_at = now + LIFETIME`) and returns a fresh `Set-Cookie` with the same token so the browser's `Max-Age` slides too.
- Cookie parsing uses `axum_extra::extract::CookieJar` (or the `cookie` crate's parser on the `Cookie` header). Several `llmobs_session` cookies: use the first that resolves.
- `serve` purges expired sessions once an hour.

### Tests

- Start → resolve finds the user; the cookie string has every attribute; `Secure` only with an https `public_url`.
- After 59 minutes no refresh header; after 61 minutes a refresh header, `expires_at` moved; after 30 days without activity → `None`.
- Disabled user → `None`. A garbage cookie (`"abc"`) → `None` without a query (assert by closing the pool first: no error).
- `delete_user_sessions` with `keep` keeps one; `purge_expired_sessions` counts.
- The DB holds only the hash: after `start_session`, `SELECT id FROM web_sessions` does not contain the token.

### Report

Standard.

---

## P10 — Principals, roles and the route table

**Goal:** every request is authenticated once, every route declares its access in one table, and one middleware enforces it (D136–D139).

**Edit only:** `crates/server/src/auth/{principal.rs,roles.rs}`, `crates/server/src/http/{mod.rs,access.rs,csrf.rs}` (`access.rs`, `csrf.rs` new), `crates/server/src/api/openapi.rs` (security schemes), `crates/server/openapi.json`, `web/src/api/schema.gen.ts` (generated), `crates/server/src/test_support.rs`, `crates/server/src/lib.rs`.

**Commit:** `server: authenticate requests and enforce route access`

### Roles (`auth/roles.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role { Viewer, Member, Admin }               // effective role on a project

pub(crate) fn effective_project_role(org: OrgRole, ov: Option<ProjectOverride>) -> Option<Role>;
// Owner, Admin → Some(Admin) (override ignored, D137); Member/Viewer: override None → hidden (None),
// override Admin/Member/Viewer → that role; no override → Member/Viewer.

pub(crate) fn org_rank(r: OrgRole) -> u8;                   // viewer 0 < member 1 < admin 2 < owner 3

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OrgPerm { View, Rename, CreateProject, ManageMembers }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectPerm { View, ManageSettings, ManageMembers }

impl OrgPerm { pub(crate) fn min_role(self) -> OrgRole; }     // View: viewer; the rest: admin (spec 08 table)
impl ProjectPerm {
    pub(crate) fn min_role(self) -> Role;                       // View: viewer; ManageSettings, ManageMembers: admin
    pub(crate) fn key_scope(self) -> Option<KeyScope>;          // View: Some(Read); the rest: None (keys cannot)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyScope { Ingest, Read, Write, Feedback }
```

Only the permissions D's routes use exist; later tracks add theirs (R-S1). A unit test pins the spec 08 table rows these variants come from.

### Principal (`auth/principal.rs`)

```rust
pub(crate) struct AuthUser { pub(crate) id: Uuid, pub(crate) is_instance_admin: bool, pub(crate) email: String }
pub(crate) enum Principal {
    Session { user: AuthUser, session_hash: String },
}
```

P16 adds `ApiKey`, P17 adds `Pat`. `email` is held for handlers that need it (password checks); it is never logged (the `Debug` impl of `AuthUser` omits it).

`authenticate` middleware (outermost after request logging):
1. If an `Authorization` header is present: it must be `Bearer <token>`; anything else → 401 `unauthorized`. In P10 no bearer kind is recognised yet, so every bearer → 401. The cookie is **ignored** whenever an `Authorization` header is present (D138).
2. Else, if a session cookie resolves (P9) → `Principal::Session`; a refresh `Set-Cookie` is appended to the response.
3. Else no principal.
4. The principal (an `Option<Principal>`) goes into the request extensions.

### CSRF (`http/csrf.rs`, D139)

After `authenticate`: for methods other than GET, HEAD, OPTIONS, when the principal is a `Session` **or** the path starts with `/api/auth/`, the `Origin` header must equal `config.public_url.origin()` exactly; otherwise 403 `forbidden` with detail `"origin check failed"`. Missing `Origin` fails too.

### Access and the route table (`http/access.rs`)

```rust
#[derive(Debug, Clone, Copy)]
pub(crate) enum Access {
    Public,
    User { session_only: bool },
    InstanceAdmin,                                         // session only
    Org { perm: OrgPerm, session_only: bool },             // path has {org}
    Project { perm: ProjectPerm, session_only: bool },     // path has {project}
}
pub(crate) struct RouteDef {
    pub(crate) method: Method,
    pub(crate) path: &'static str,          // axum syntax: /api/v1/orgs/{org}
    pub(crate) access: Access,
    pub(crate) documented: bool,            // false only for /healthz and /readyz
    pub(crate) handler: MethodRouter<AppState>,
}
pub(crate) fn routes() -> Vec<RouteDef>;     // the single table; P4's health routes move here
pub(crate) fn build_router(state: AppState, routes: Vec<RouteDef>) -> Router;
```

`build_router` wraps each `RouteDef.handler` in its own `enforce` layer (`MethodRouter::route_layer` with `from_fn_with_state`) and merges the method routers per path (check whether axum 0.8's `Router::route` merges two calls on the same path; if it panics, group by path and `MethodRouter::merge` first).

`enforce(access)`:
1. `Public` → continue.
2. No principal → 401 `unauthorized`.
3. `session_only` (and `InstanceAdmin`) with a non-session principal → 403 `forbidden`.
4. `User` → continue. `InstanceAdmin` → `is_instance_admin` else 403.
5. `Org { perm }`: read the `{org}` path parameter (`RawPathParams` from the request parts); resolve it as a UUID, else as a slug; load the caller's org role. Org missing, or caller not a member → **404** `not_found`. Role below `perm.min_role()` → 403. Insert `OrgAccess { org: OrgRow, role: OrgRole }`.
6. `Project { perm }`: `{project}` is a UUID, or `<org-slug>/<project-slug>` (the `/` arrives percent-encoded as `%2F` in one segment: test that axum matches the route and decodes the value), or `current` (only for API keys, P16; for users → 404). Load the project, the org role and the override; `effective_project_role` → `None` (not a member, or hidden) → 404; below `perm.min_role()` → 403. Insert `ProjectAccess { project: ProjectRow, role: Role }`.
7. Org and project states other than `active` (`deleting`) → 404.

Handlers extract `Extension<OrgAccess>` / `Extension<ProjectAccess>` / `Extension<Principal>` (for `User` routes, `enforce` inserts the unwrapped principal).

### OpenAPI security

- Components: `session` = API key in cookie `llmobs_session`; `bearer` = HTTP bearer.
- Per operation, from its `Access`: `Public` → `security: []`; `User`, `Org`, `Project` with `session_only: false` → `[{session: []}, {bearer: []}]`; `session_only` and `InstanceAdmin` → `[{session: []}]`.
- Inspect how the current document expresses security (top-level vs per operation). Make every operation that P10 can see consistent with the rule above, including the read-API operations declared without handlers (they will be `Project { View }` routes: `[{session}, {bearer}]`). Regenerate with `just openapi`.

### Tests (`http/access.rs`, with a test-only route table)

A `#[cfg(test)]` table with one route per `Access` shape whose handler echoes what it received (`{"org": …, "project": …, "role": …}`), and fixtures created through the P6 store: org A (owner Ada, admin, member, viewer, a member with override `none`, a viewer with override `admin`), org B (owner Bob), projects in each.
- Public route without credentials → 200; `User` route without → 401; with a session → 200.
- Org route: Ada with `acme` and with the UUID → 200; Bob → 404; a viewer on `ManageMembers` → 403; unknown slug → 404.
- Project route: by UUID, by `acme%2Fsupport-bot`; Bob → 404; member with override `none` → 404; viewer with override `admin` on `ManageSettings` → 200; org admin with a stored override `viewer` → still admin; `current` with a session → 404.
- `session_only` route with a session → 200.
- `InstanceAdmin` route: non-admin → 403.
- CSRF: POST with a session and `Origin: http://evil.example` → 403; without `Origin` → 403; with the right origin → 200; POST under `/api/auth/` without credentials and without `Origin` → 403; GET without `Origin` → 200.
- `Authorization: Basic …` → 401; `Authorization: Bearer x` plus a valid cookie → 401 (cookie ignored).
- Route table invariants (on the real `routes()`): every path parameter `{org}` implies `Access::Org`, `{project}` implies `Access::Project`; no `Org`/`Project` access without that parameter; every `documented` route has an operation with the same method and path in `openapi()` and its security matches the rule above.
- Effective role: the full matrix of `OrgRole × Option<ProjectOverride>`.

### Report

How the merge of method routers works in axum 0.8.9 and how `%2F` behaves.

---

## P11 — Setup, headless init and the auth config

**Goal:** the first user can be created through `POST /api/auth/setup` with the setup token, or headlessly from `[init]` (spec 08 § Bootstrapping, D142); `GET /api/auth/config` gets its handler.

**Edit only:** `crates/server/src/bootstrap.rs` (new), `crates/server/src/meta/api_keys.rs` (new; insert only), `crates/server/src/routes/{mod.rs,auth.rs}` (new), `crates/server/src/api/account.rs` (`SetupRequest`, `SetupResponse`), `crates/server/src/http/{mod.rs,access.rs}`, `crates/server/src/serve.rs`, `crates/server/src/api/openapi.rs`, `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `fixtures/api/` (examples), `crates/server/src/test_support.rs`.

**Commit:** `server: add first-run setup and headless init`

### Store

`meta/api_keys.rs`: `insert_api_key(conn, NewApiKey { project_id, kind, scopes: &[KeyScope], name, prefix, hash, created_by, expires_at }, now) -> ApiKeyRow`. Scopes are stored as a JSON array of the lowercase names. P16 adds the rest of this module.

### Setup state

`AppInner.setup: SetupState { token: Mutex<Option<String>> }`. At `serve` start, after `meta.db`:
- users exist → `None`;
- no user and a complete `[init]` → run `apply_init`, then `None`;
- no user and no `[init]` → generate `new_opaque_token()`, keep it, log once at `warn` with `target = "setup"`: `setup required: open <public_url>/setup?token=<token>` (D142; the only secret ever logged).

### `apply_init(state, init)` (one `MetaDb::write`)

1. Re-check `count_users_tx == 0` inside the transaction; otherwise log `info` "init skipped: users exist" and return.
2. Normalize the email; `check_policy` on the password (failure → `ServeError::Init { reason }` naming the rule, not the value: startup fails).
3. `secret_key` given: `parse_key == Some(Secret)` else `ServeError::Init { reason: "LLMOBS_INIT_SECRET_KEY is not a secret key" }`.
4. Insert: user (`is_instance_admin`, `email_verified_at = now`), org (`init.org` as name, derived slug), owner membership, project (`init.project`, derived slug), and the secret key if given (`scopes = [ingest, read, write]`, name `"init"`, `created_by` = the user).
5. Audit event: `init applied`, user, org, project ids.

A crash before commit leaves an empty database; the next start retries.

### `POST /api/auth/setup` (`Access::Public`)

Request `SetupRequest { setup_token, email, password, name?, org_name, project_name }`; response 201 `SetupResponse { user: User, org: OrgMembership, project: Project, secret_key: String, secret_key_prefix: String }` and a session `Set-Cookie`.
1. `limits.ip_forms` → 429.
2. Password login disabled → 403 `forbidden` ("password setup is disabled; use an identity provider", P25 adds that path).
3. `setup.token` is `None` → 409 `conflict` ("already set up"). Token mismatch (`constant_time_eq`) → 403 `forbidden`.
4. Validate: email (400 field error on `email`), policy (400 field error on `password`, message per rule, never the value), names 1–100 characters after trimming.
5. One `write`: re-check no user (409 if one appeared: concurrent setup), insert user (instance admin, verified), org + owner, project, a new secret key (`new_key(Secret)`, all three scopes, name `"setup"`).
6. Clear `setup.token`; start a session; audit event `setup completed`.

### `GET /api/auth/config` (`Access::Public`)

Moves onto a handler. `setup_required = count_users == 0`; `password_enabled = !auth.disable_password`; `signup`; `providers: []` (P23 fills it); `smtp_enabled = smtp_url.is_some()`; `email_verification_required`. Shape unchanged.

### Examples

`fixtures/api/static/setup-response.json` with the fixed ids of `fixtures/api/README.md` and a synthetic key (`sk-llmobs-` followed by 32 characters of an obviously fake pattern such as `EXAMPLE0EXAMPLE0…`; if a secret scanner covers `fixtures/api`, use the form it allows), plus its manifest entry. `auth-config.json` stays.

### Tests

- No user, no init → `GET /api/auth/config` says `setup_required: true`; the setup token was logged once (capture logs); `POST /setup` without the token → 403; with it → 201, cookie set, `GET /api/auth/me` works with the cookie (P12 adds `me`; until then assert the DB rows), the user is instance admin and owner, the project exists, the returned secret key's hash is in `api_keys` and the plaintext is not.
- A second `POST /setup` → 409. Two concurrent setups → exactly one 201.
- Weak password → 400 with a field error on `password`, the password text absent from the body and logs.
- `[init]` complete → user, org, project created; no setup token generated; restart with the same init → skipped; `secret_key` invalid → startup error naming the variable; weak init password → startup error.
- `disable_password = true` → `POST /setup` 403.
- `GET /api/auth/config` after setup → `setup_required: false`.

### Report

Standard.

---

## P12 — Login, logout, signup and `/me`

**Goal:** password login with the uniform answers, rate limits and lockout of D141/D143; logout; signup under `open`; `GET /api/auth/me`.

**Edit only:** `crates/server/src/routes/auth.rs`, `crates/server/src/api/account.rs` (`LoginRequest`, `SignupRequest`), `crates/server/src/http/access.rs`, `crates/server/src/api/openapi.rs`, `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `fixtures/api/`, `crates/server/src/test_support.rs`.

**Commit:** `server: add password login, logout and signup`

### `POST /api/auth/login` (`Public`)

Request `{email, password}`. Response 200 `Me` and a session cookie.
1. Password login disabled → 403 `forbidden`.
2. `normalize_email`; invalid → 401 `unauthorized` (same as a wrong password).
3. `limits.login` on `(client_ip, email)` → 429.
4. The email's domain is in `enforce_sso_domains` → 403 `sso_required` (domain-based; reveals nothing about the account).
5. Load the user. Locked (`lockout.is_locked`), unknown, without a password, or disabled → still call `hasher.verify(password, None)` (dummy) or the real hash, then answer 401 `unauthorized`. Order: always exactly one `verify` per request, whatever the case.
6. Wrong password → `lockout.record_failure(email)`, audit `login failed` with the user id (or `account = "unknown"`), 401.
7. `require_email_verification` and `email_verified_at` is null → 403 `email_not_verified` (only after a correct password).
8. Success: `lockout.clear`, rehash when `needs_rehash` (write the new hash), start a session, audit `login`.

### `POST /api/auth/logout` (`Public`; CSRF applies through the `/api/auth/` rule)

Deletes the session of the cookie if it resolves; always answers 204 with the clearing cookie.

### `POST /api/auth/signup` (`Public`)

Request `{email, password, name?}` with `#[serde(deny_unknown_fields)]`. P20 adds `invite_token` and its path.
1. `limits.ip_forms` → 429. Password disabled → 403.
2. Policy `disabled` → 403 `forbidden` ("sign-up is disabled"); `invite_only` → 403 ("sign-up requires an invitation").
3. `open`: email valid (400 field error); `signup_domains` non-empty and the domain not in it → 403; `enforce_sso_domains` contains the domain → 403 `sso_required`; policy (400 field error); name ≤ 100.
4. Insert the user (not verified). Email exists → 409 `conflict` (D143).
5. `require_email_verification` → 201 with `{"verification_required": true}` and no session (P21 sends the email). Else → 201 `Me` with a session cookie.

Response type: an enum serialized as `{ "me": Me }` or `{ "verification_required": true }` is awkward for clients; use `SignupResponse { me: Option<Me>, verification_required: bool }`.

### `GET /api/auth/me` (`User`)

Moves onto a handler; shape unchanged: `User` and `orgs_of_user`.

### Tests

- Login success: 200, cookie, `Me`; `/me` with the cookie.
- Uniformity: unknown email, wrong password, disabled user, password-less user and locked email all give byte-identical problem bodies and the same status; each request ran `verify` exactly once (count calls through a test hook on `Hasher`, `#[cfg(test)]` counter).
- Lockout: 20 wrong passwords then the right one → 401; after `clock.advance(15 min)` → 200. Rate limit: 11th attempt in a minute from the same IP for the same email → 429 with `Retry-After`; a different IP is not limited (lockout still applies per email).
- SSO-enforced domain → 403 `sso_required` for known and unknown accounts alike.
- `email_not_verified` only with the correct password.
- Rehash: a user stored with `t = 1` logs in and the stored PHC now has `t = 2`.
- Logout deletes the row; the old cookie then gets 401 on `/me`.
- Signup: each policy; domain restriction; duplicate (case-insensitive) → 409; weak password → 400 field error.
- CSRF: login without `Origin` → 403.
- `assert_logs_clean` with the email and password over a failed and a successful login.

### Report

Standard.

---

## P13 — Password reset, account endpoints and the admin CLI

**Goal:** reset links (consumed through the API), changing one's name and password, and `llmobs admin create-user | reset-password | list-users`.

**Edit only:** `crates/server/src/meta/{password_resets.rs,personal_tokens.rs}` (new; `personal_tokens.rs` gets only `revoke_user_tokens` here), `crates/server/src/auth/reset.rs` (new), `crates/server/src/routes/{auth.rs,me.rs}`, `crates/server/src/admin.rs` (new), `crates/server/src/api/account.rs`, `crates/server/src/http/access.rs`, `crates/server/src/api/openapi.rs`, `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `fixtures/api/`, `crates/cli/src/main.rs`, `crates/cli/Cargo.toml`.

**Commit:** `server: add password reset, account endpoints and admin commands`

### Reset tokens (`meta/password_resets.rs`, `auth/reset.rs`)

- `insert_reset(conn, hash, user_id, now, expires_at)`; `valid_reset(pool, hash, now) -> Option<Uuid>` (unused and unexpired); `use_reset(conn, hash, now) -> Option<Uuid>` (`UPDATE … SET used_at = ? WHERE token_hash = ? AND used_at IS NULL AND expires_at > ? RETURNING user_id`); `delete_user_resets(conn, user_id)`.
- `pub(crate) async fn reset_link(state_or_meta, user_id, now) -> Result<String, …>`: new opaque token, 1 hour, returns `<public_url>/reset?token=<token>`.

### `POST /api/auth/password/reset` (`Public`)

Request `{token, password}`; 204.
1. `limits.ip_forms`. Password login disabled → 403.
2. Token not an opaque token, or `valid_reset` → none → 400 `bad_request` ("invalid or expired link").
3. Check the policy against the user's email **before** using the token (a weak password does not burn the link).
4. One write: `use_reset` (none → 400: raced), set the hash, delete all the user's sessions and other reset tokens. `lockout.clear(email)`. Audit `password reset`.

### Account (`routes/me.rs`)

- `PATCH /api/v1/me` (`User { session_only: true }`): `{name: string | null}` (1–100 characters after trimming, or null to clear) → 200 `User`.
- `POST /api/v1/me/password` (`User { session_only: true }`): `{current_password?, new_password}`; 204.
  - `limits.login` keyed by `(client_ip, email)`.
  - The user has a password → `current_password` required and verified (wrong → 403 `forbidden`, recorded in the lockout like a failed login). No password (SSO-only) → `current_password` must be absent; password login disabled → 403.
  - Policy; set the hash; delete every other session of the user; revoke all their PATs (`revoke_user_tokens(conn, user_id, now)` sets `revoked_at`). Audit `password changed`.

### Admin library (`admin.rs`, public)

```rust
pub async fn create_user(config: &ServerConfig, email: &str, name: Option<&str>, password: Option<&str>, instance_admin: bool) -> Result<CreatedUser, AdminError>;
pub struct CreatedUser { pub id: Uuid, pub reset_url: Option<String> }    // reset_url when no password was given
pub async fn reset_password(config: &ServerConfig, email: &str) -> Result<String, AdminError>;  // the reset URL
pub async fn list_users(config: &ServerConfig) -> Result<Vec<UserSummary>, AdminError>;
pub struct UserSummary { pub id: Uuid, pub email: String, pub name: Option<String>, pub instance_admin: bool, pub disabled: bool, pub created_at: i64 }
```

They use `MetaDb::open_existing` (D134: no migration; `NotInitialized` → "run `llmobs serve` once to create the database"; `SchemaMismatch` → "this binary expects schema N, found M"). They do not take the data-dir lock. `create_user` normalizes the email, checks the policy, and creates a verified user (the operator vouches for it). `AdminError` is a `thiserror` enum; its messages name no email.

### CLI

```
llmobs admin create-user --email <e> [--name <n>] [--password-stdin] [--instance-admin] [--config <path>]
llmobs admin reset-password --email <e> [--config <path>]
llmobs admin list-users [--format table|json] [--config <path>]
```

- `--password-stdin` reads one line from stdin (trailing newline removed). Without it, `create-user` prints the reset URL to stdout ("Set the password at: …").
- `reset-password` prints the URL. These URLs are printed on purpose (the operator asked for them); they never go through `tracing`.
- `list-users` table: id, email, name, admin, disabled, created (RFC 3339).

### Examples

`me-user.json` for `PATCH /me` if the response type is new; otherwise reuse `User`.

### Tests

- Reset: link from `admin::reset_password`, then `POST /password/reset` → 204; old sessions gone; login with the new password; the token used twice → 400; expired after 1 hour → 400; weak password → 400 and the token still works afterwards; a locked email is unlocked by a reset.
- `PATCH /me` with a session → 200; with a bearer (later PATs) the route is session-only (assert the `Access` in the table).
- `POST /me/password`: wrong current → 403; success revokes other sessions (a second session's cookie gets 401) but not the current one; SSO-only user sets a first password without `current_password`; with `current_password` → 400.
- Admin library: on a fresh dir without `meta.db` → `NotInitialized`; after `MetaDb::open`, `create_user` with and without a password; duplicate email → error; `list_users` order; works while a `TestApp` server holds the same data dir (no lock taken).
- CLI: `run_cli(["admin", "list-users", "--config", …])` against a temp config.

### Report

Standard.
