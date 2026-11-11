# D · Metadata DB, server foundation and auth: plan

This is the implementation plan for track D (Wave 1). It covers:
- the **server foundation** that every later server track builds on: config loading, the axum app, problem errors, `llmobs serve`, health probes and test helpers;
- **`meta.db`**: the migration framework and the identity, tenancy and credential tables;
- **authentication**: first-run setup, passwords, web sessions, CSRF, rate limits and lockout, API keys, personal access tokens, invites, email, password reset and email verification, and OAuth/OIDC (GitHub, Google, generic OIDC);
- **authorization**: roles, project overrides, the permission check and the route table that declares access for every route;
- the **API contract**, handlers and `fixtures/api` examples for auth, orgs, projects, members, invites, keys, `/me` and `/admin`;
- the CLI commands `serve`, `admin create-user | reset-password | list-users`, `login` and `logout`.

The work is split into 26 sub-phases, P1–P26. Each is done by one agent, in order, and ends with `just check` green and one commit. A sub-phase is self-contained: it lists the files it may edit, the behaviour, the tests and the commit subject. Do the sub-phase you were given, nothing more.

**Status:** decisions settled with the user (2026-11-11). Nothing is implemented.

**Read first** (every sub-phase):
- `AGENTS.md`;
- this README (decisions, boundaries, conventions, dependency table);
- `docs/spec/04-metadata-db.md` § Identity & tenancy, § Keys & credentials;
- `docs/spec/08-auth.md`;
- `docs/spec/09-api.md` § Conventions and § Endpoints › Auth, Orgs;
- `docs/spec/12-operations.md` § Configuration, § Process lifecycle;
- `docs/research/auth/README.md` (lessons from competitors' advisories);
- the sub-phase's own file.

| File | Sub-phases |
|---|---|
| [01-foundation.md](01-foundation.md) | P1 specs and decisions · P2 config · P3 `meta.db` · P4 HTTP skeleton (state, errors, client IP, health) · P5 `llmobs serve` |
| [02-identity.md](02-identity.md) | P6 identity and tenancy store · P7 secrets and passwords · P8 rate limits and lockout |
| [03-sessions-and-access.md](03-sessions-and-access.md) | P9 web sessions · P10 principals, roles and the route table · P11 setup and headless init · P12 login, logout, signup, `/me` · P13 password reset, account endpoints, admin CLI |
| [04-keys-orgs-members.md](04-keys-orgs-members.md) | P14 orgs and projects · P15 members and project overrides · P16 API keys · P17 personal access tokens and CLI login · P18 the generated authorization suite |
| [05-email-invites-admin.md](05-email-invites-admin.md) | P19 mailer · P20 invites · P21 forgot password and email verification · P22 instance admin endpoints |
| [06-oauth-oidc.md](06-oauth-oidc.md) | P23 provider config and the mock IdP · P24 authorize and callback · P25 linking, provisioning and domain rules |
| [07-security-review.md](07-security-review.md) | P26 independent security review |

There is no separate "API contract" part. Every sub-phase that adds an endpoint also adds its handler with `#[utoipa::path]`, its entry in `openapi.json` and its `fixtures/api` examples. C can plan the login, account and settings pages once P12–P17 have landed.

## Decisions

These go into `docs/mvp-decisions.md` in P1. **The numbers may need rebasing.** On 2026-11-11, `main` used numbers up to D120, and the C plan (PR #17, not merged) reserves D121–D132. If other decisions land first, P1 renumbers these and updates every reference in this directory.

| # | Topic | Decision | Source |
|---|---|---|---|
| D133 | Scope of track D | D builds the server foundation (config, axum app, problem errors, `llmobs serve`, `/healthz`, `/readyz`, test helpers), the `meta.db` migration framework and the identity, tenancy and credential tables: `users`, `identities`, `web_sessions`, `orgs`, `org_members`, `projects`, `project_members`, `invites`, `password_resets`, `email_verifications`, `api_keys`, `personal_tokens`. The other spec 04 tables come with the tracks that use them. `instance_settings` comes with its first user (the encryption key check value, track E). Secret encryption (`LLMOBS_ENCRYPTION_KEY`, `rotate-encryption-key`) is E's. Project and org erasure come with GDPR deletion. OTLP ingest, the read API handlers, SSE, MCP and embedding the web UI are later server work. | User |
| D134 | `meta.db` access | One writer connection behind a `tokio::sync::Mutex`, plus a read-only pool of 4 connections. `journal_mode=WAL`, **`synchronous=FULL`**, `foreign_keys=ON`, `busy_timeout=5000`. Writes are rare, so every commit is durable before the response; an API key already shown to the user is never lost to a power cut. Migrations run only in `llmobs serve`. `llmobs admin` commands open `meta.db` without migrating and refuse a database whose latest applied migration differs from the binary's. Emails are stored normalized (trim, Unicode NFC, lowercase) and compared exactly; there is no `COLLATE NOCASE` (ASCII-only in SQLite, not portable to Postgres). | User (sync), research G30 |
| D135 | Startup order | `llmobs serve` opens storage first, because `Storage::open` takes `<data_dir>/LOCK`, then `meta.db` (migrations), then binds. A second process on the same data dir fails on the lock before touching `meta.db`. `llmobs admin` commands do not take the lock; they work while the server runs (spec 09 § CLI). | Spec 12 order adjusted |
| D136 | Route table and access | Every route is declared in one table with its `Access`: public, any signed-in user, a session-only user, an org permission, a project permission, or instance admin. One middleware enforces it before the handler runs, resolves `{org}` and `{project}`, and hands the handler the resolved access. A resource outside the caller's reach answers **404**, never 403, so ids cannot be probed. A visible resource with too low a role answers **403**. Tests are generated from the table: every route declares access, the OpenAPI security matches, and a principal from another org gets 404 on every route. | Research (IDOR advisories, Langfuse CVE-2025-59305) |
| D137 | Roles and overrides | Effective project role = the `project_members` override if one exists, else the org role (`owner` acts as `admin` on a project). Overrides apply only to org `member`s and `viewer`s; they can raise to `admin` or lower to `viewer`/`none`. Org owners and admins are never overridden (setting one is a 400). Only callers with effective role `admin` on the project edit overrides. Nobody grants a role above their own, and only owners grant or change `owner`. An org always keeps at least one owner. Any signed-in user may create an org and becomes its owner. Instance admins get the `/api/v1/admin/*` endpoints only, with no implicit access to org data. | User |
| D138 | Credentials | A bearer token wins over a cookie; when a request has both, the cookie is ignored. PATs with scope `read` may call safe methods (GET, HEAD) only; `read, write` may call everything except session-only routes. **Session-only routes:** creating, listing or revoking API keys and PATs, changing members, roles, overrides or invites, the password and the account name. Exception: a PAT may revoke itself (`DELETE /api/v1/me/tokens/current`, used by `llmobs logout`). API keys are project-bound: in D, `read` grants the safe methods of project-scoped routes; later tracks map `ingest`, `write` and `feedback` to their permissions. The display `prefix` is the type prefix plus the first 6 random characters (`sk-llmobs-a1B2c3`), not 12 characters (which kept only 2 random ones). Key and token lookups are cached 60 s by hash; revoking or expiring evicts the entry at once in this process. `last_used_at` is coalesced in memory and written every 60 s and at shutdown. Credentials are read only from the `Authorization` header, never from a query string. | User (PAT limits), research G9, G12, G19, G22 |
| D139 | CSRF | Every state-changing request (not GET, HEAD, OPTIONS) authenticated by the session cookie, and every state-changing request under `/api/auth/` whatever its credentials (login, signup, setup, password reset, invite accept, logout), must carry an `Origin` equal to the origin of `public_url`. Requests authenticated by a bearer token skip the check. JSON bodies require `Content-Type: application/json` (axum's `Json` extractor rejects others with 415). | Spec 08, research G8 |
| D140 | Client IP | New config `trusted_proxies` (list of CIDRs, empty by default). The client IP is the socket peer, unless the peer is in the list: then it is the rightmost address of `X-Forwarded-For` that is not itself in the list. Rate limits, lockout keys and `web_sessions.ip` use it. | User |
| D141 | Passwords, rate limits and lockout | Passwords are NFC-normalized, then checked: 10–128 characters (Unicode scalar values), not equal to the email, not in the embedded common-password list (SecLists `10k-most-common.txt`, MIT, kept entries of 10+ characters). argon2id m = 19 456 KiB, t = 2, p = 1, in `spawn_blocking` behind a semaphore (`max(2, cores / 2)` permits). Login with an unknown email verifies against a fixed dummy hash, so its timing matches. **Rate limits and lockout are in memory** (a restart clears them): login 10/min per (client IP, email); signup, setup, password forgot and verification resend 5/min per client IP; password forgot and resend also 3/hour per email. Lockout: 20 consecutive failed password logins for one email lock password login for that email for 15 minutes; a success or a password reset clears it; OAuth/OIDC login is not affected. | User, spec 08, research G23–G25 |
| D142 | Setup token | While `users` is empty, `llmobs serve` generates a random setup token at startup and logs the setup URL `<public_url>/setup?token=…` once at `warn`. This is the only secret that is ever logged, on purpose, like Jupyter's. `POST /api/auth/setup` requires it. The headless `LLMOBS_INIT_*` bootstrap does not need it. | User, research G7 |
| D143 | Uniform answers | Login answers the same 401 `unauthorized` for an unknown email, a wrong password, a disabled user, a user without a password and a locked email. Password forgot and verification resend always answer 204. Emails are sent from a background task after the response is decided, so timing does not depend on SMTP. Signup under `open` answers 409 `conflict` for an email already registered: that reveals the account exists, which is accepted (the default policy is `invite_only`). | Spec 08, research G24 |
| D144 | Account endpoints | New: `PATCH /api/v1/me` (name), `POST /api/v1/me/password` (current and new password; revokes every other web session and all PATs), `POST /api/auth/email/verify/resend`. Email verification tokens get their own table `email_verifications`. Changing one's email is not in the MVP. | Research G21, G26 |
| D145 | Invites | An invite is bound to its email: signup with an invite token uses the invite's email, and accepting while signed in requires the same email. One pending invite per (org, email): a new one replaces the old. The role ceiling of D137 applies. The response to `POST …/invites` always carries the accept URL, for copying when SMTP is off. Accepting with a valid token marks the email verified. | Research G20 |
| D146 | OAuth/OIDC linking | A provider login with a known `(provider, subject)` signs that user in. Otherwise, when a user with the same email exists, the identity is linked only if the provider's `link_existing_by_email` is on (default on for GitHub and Google, off for generic OIDC), the provider asserts the email verified (a missing claim is false), and the user is not an instance admin. Linking revokes the user's web sessions and PATs, and clears the password when the email was never verified (account pre-hijacking). The callback ignores any existing session. `state`, PKCE (S256) and `nonce` are always used; they travel in a short-lived cookie signed with a per-process random key. Multi-tenant issuers (Entra `common`, `organizations`, `consumers`) are refused at config load. Provider metadata is discovered lazily and cached for one hour. | User, research G1–G5 |
| D147 | Instance admin API | `/api/v1/admin/users` lists and creates users; new `PATCH /api/v1/admin/users/{id}` sets `disabled` and `is_instance_admin` (never on oneself, never removing the last active instance admin); `POST …/reset-link` returns a reset URL. `GET /api/v1/admin/settings` returns the effective, non-secret settings (read-only: they come from config). `POST /api/v1/admin/settings/test-email` sends the test email to the caller's own address. `PATCH /api/v1/admin/settings` comes with the first editable setting. Disabling a user deletes their web sessions; their PATs fail at once because every request checks `disabled_at`. | Research G10, G18 |
| D148 | Logs | Request logs carry the matched route template (`/api/auth/invites/{token}`), the status and the duration, never the raw URI, so tokens in paths are not logged. Audit events (`target = "audit"`, spec 08) carry user, org, project and key ids, never emails, names or token values. A failed login for an unknown email logs `account = "unknown"`. | Research G22, G32 |
| D149 | Tests | HTTP tests live inside the crate (`#[cfg(test)]`), drive the router with `tower::ServiceExt::oneshot`, and use a manual clock and a stub mail transport that are compiled only for tests. OAuth/OIDC tests use an in-process mock IdP (discovery, JWKS, token endpoint, test RSA key). No containers. | User |

## Boundaries

```
llmobs-cli ──ServerConfig::load──▶ server::serve::run ──▶ Storage::open (LOCK, replay) ──▶ MetaDb::open (migrate) ──▶ bind :8080
           ──server::admin::{create_user, reset_link, list_users}──▶ MetaDb::open_existing (no migration)
           ──login/logout──▶ HTTP to a running server
later server tracks ──RouteDef { access: Access::Project(ProjectPerm::…) }──▶ D's enforcement ──▶ Extension<ProjectAccess>
```

**A1 (storage).** D calls `Storage::open(StorageConfig)`, `Storage::health()` and `Storage::shutdown()`. The server builds `StorageConfig` from the `[storage]` section and the top-level `data_dir`. D adds no storage API. Requests for A1: none blocking (see § Requests).

**A2 (query).** None. The read API handlers that use A2 come later; they declare their routes in D's table with `ProjectPerm::View`.

**B1 (normalization).** None. The OTLP endpoint comes later; it adds `ProjectPerm::Ingest` and the `ingest` key scope.

**C (web UI).** D delivers the contract for the login, signup, setup, invite, reset, verify, account and settings pages. Those pages and the `/cli-auth` page that `llmobs login` opens are C's (request R-C1). D changes `crates/server/openapi.json`, which regenerates `web/src/api/schema.gen.ts` through `just openapi`: D commits that generated file and nothing else under `web/`.

**E (evaluation)** owns `LLMOBS_ENCRYPTION_KEY`, `instance_settings`, `llm_connections`, `notification_channels` and `rotate-encryption-key`.

**GDPR deletion (later)** owns `deletions`, `deletion_*`, `DELETE /projects/{project}`, `DELETE /orgs/{org}` and user erasure.

**Directories D edits:** `crates/server/`, `crates/cli/`, `fixtures/api/`, `docs/`, plus the root `Cargo.toml`/`Cargo.lock` for the dependencies listed below (approved with this plan) and the generated `web/src/api/schema.gen.ts`.

## Conventions for every sub-phase

**Module layout** (`crates/server/src/`). New files are created by the sub-phase that first needs them.

```
lib.rs              pub mod api; pub mod config; pub mod serve; pub mod admin; the rest pub(crate)
config.rs           ServerConfig and its sections, load()                                  (P2)
clock.rs            Clock: system time, or a manual clock in tests                         (P3)
meta/mod.rs         MetaDb, MetaError, transactions                                        (P3)
meta/*.rs           one module per table group: users, orgs, projects, members, invites,
                    web_sessions, api_keys, personal_tokens, password_resets,
                    email_verifications, identities                                        (P6+)
auth/secret.rs      random tokens, base62, SHA-256 hex, token kinds and prefixes           (P7)
auth/password.rs    policy, hashing, the dummy hash                                         (P7)
auth/limits.rs      keyed token buckets and the lockout table                               (P8)
auth/roles.rs       Role, ProjectRole, effective role, ceilings, ProjectPerm, OrgPerm       (P10)
auth/principal.rs   Principal, bearer and cookie resolution, caches, last_used coalescing   (P10, P16)
auth/oidc/*.rs      providers, flow, linking                                                (P23+)
email/              mailer and templates                                                    (P19)
http/mod.rs         AppState, RouteDef, Access, router(), middleware                        (P4, P10)
http/error.rs       ApiError → problem+json                                                 (P4)
routes/*.rs         handlers, one module per area                                           (P11+)
bootstrap.rs        setup token and headless init                                           (P11)
serve.rs            run()                                                                   (P5)
admin.rs            functions behind `llmobs admin …`                                       (P13)
test_support.rs     #[cfg(test)] TestApp and helpers                                        (P4+)
crates/server/migrations/meta/0001_identity.sql, 0002_…                                     (P3+)
```

`api/` keeps the request and response types (existing `account.rs`, `common.rs`, plus new files per area); handlers live in `routes/`.

**Errors.** One `thiserror` enum per area: `ConfigError`, `MetaError`, `AuthError`, `ServeError`, `MailError`, `OidcError`. HTTP handlers return `Result<_, ApiError>`. `ApiError` maps to `Problem` (spec 09) and never carries payload content, emails, secrets or token values in `detail`. 5xx details are generic; the cause is logged with ids only.

**Database.** sqlx runtime queries (`sqlx::query`, `query_as`), never the `query!` macros (no `DATABASE_URL`). Times are `i64` ns. IDs are UUIDv7 strings (`OrgId`, `ProjectId` from core; `Uuid` for the rest, D61). Every write goes through `MetaDb::write(|tx| …)` (P3); reads use the pool.

**HTTP tests** use `TestApp` (P4, grown by later sub-phases): a temp data dir with a real `Storage` and `MetaDb`, the manual clock, the stub mailer, `public_url = http://localhost:8080`, and a request builder that sets `Origin` and the cookie or bearer. They assert status, `code` and body.

**OpenAPI.** Each new handler gets `#[utoipa::path]` with `operation_id`, tag, parameters, request body, responses (including the problem responses) and `security`. Register it in `api::openapi::openapi()`; check how utoipa 6 adds a `#[utoipa::path]` handler to the existing `PathsBuilder` (`PathsBuilder::path_from::<…>()`) and use that. Operations already declared with the builder (`get_auth_config`, `get_me`, `list_projects`, `get_project`) move onto their handler without changing the committed document, unless the sub-phase says the shape changes. P10 adds the security schemes. After a contract change, run `just openapi` and commit `crates/server/openapi.json` and `web/src/api/schema.gen.ts`.

**Examples.** Every new response type gets a hand-written example in `fixtures/api/static/` and a manifest entry, following `fixtures/api/README.md` (synthetic data, the fixed ids, `ada@example.com` as the only email; add `bob@example.com` only if a sub-phase needs a second user, and update the README's rule in that sub-phase). `just api-examples` regenerates the manifest.

**Privacy.** No email, name, password, token or key value in logs or error details. Tests assert this where the plan says so (`assert_logs_clean`, P4).

## Dependencies

Approved with this plan for the root `[workspace.dependencies]`. Each sub-phase adds only the ones it uses, at these versions (looked up 2026-11-11 with `cargo info`). If a version no longer resolves, take the latest patch of the same minor and say so in the report.

| Crate | Version | Features | First used |
|---|---|---|---|
| `figment` | 0.10.19 | `toml`, `env` | P2 |
| `ipnet` | 2.12.2 | `serde` | P2 |
| `axum` | 0.8.9 | defaults minus `tracing` if it pulls nothing we need; keep `json`, `query`, `tokio`, `http1` | P4 |
| `tower` | 0.5.3 | `util` (`ServiceExt::oneshot` in tests) | P4 |
| `unicode-normalization` | 0.1.26 | | P6 |
| `argon2` | 0.6.0 | defaults (`alloc`, `getrandom`, `password-hash`) | P7 |
| `rand` | 0.10.3 (already in `Cargo.lock`) | defaults | P7 |
| `subtle` | 2.6.1 | | P7 |
| `axum-extra` | 0.12.6 | `cookie` | P9 |
| `dirs` | 7.0.0 | | P17 (CLI) |
| `toml` | 1.2.1 | | P17 (CLI credentials file) |
| `lettre` | 0.11.23 | `default-features = false`: `builder`, `hostname`, `pool`, `smtp-transport`, `tokio1`, the tokio rustls feature and a root-store feature (check exact names with `cargo info lettre@0.11.23 -v`) | P19 |
| `minijinja` | 3.1.0 | defaults | P19 |
| `openidconnect` | 4.1.0 | `default-features = false`: `reqwest`, `rustls-tls` | P23 |
| `oauth2` | 5.0.0 | same backend features as `openidconnect` (GitHub) | P23 |
| `reqwest` | 0.12.28 (the version `openidconnect` 4.1 uses; already in `Cargo.lock`) | `default-features = false`: `rustls-tls`, `json` | P17 (CLI login), then P23 |
| `hmac` | 0.13.0 | | P23 |
| `rsa` (dev) | 0.10.0 | | P23 (mock IdP signing) |

`sha2` 0.11, `base64` 0.23, `serde_urlencoded`, `tokio`, `tracing`, `uuid`, `chrono`, `sqlx`, `clap`, `anyhow` (CLI only) and `tempfile` (tests) are already in the workspace. Everything RustCrypto stays on the 0.11/0.13 generation of `sha2`/`hmac`; `openidconnect` brings its own older versions, which is allowed (only the DataFusion family is banned from duplicates).

## Requests for other tracks

| # | To | Request | Needed by |
|---|---|---|---|
| R-C1 | C | Pages: `/setup?token=`, `/login` (with `?error=` codes from P24), `/signup` (with `?invite=`), `/invite/{token}`, `/reset?token=`, `/verify?token=`, `/account` (name, password, tokens), org settings (members, invites, projects), project settings (API keys, overrides), `/admin` (users, settings), and `/cli-auth?port=&state=`: after the user confirms, create a PAT named `CLI on <hostname>` (scopes `read, write`) and redirect to `http://127.0.0.1:<port>/callback?token=…&state=…`. The host is always `127.0.0.1`; only the port comes from the query, as an integer 1024–65535. | P12–P22 landed |
| R-E1 | E | `instance_settings`, `LLMOBS_ENCRYPTION_KEY`, `secret.key`, the check value and `rotate-encryption-key` (D133). Use AES-256-GCM with AAD = `table:column:row_id`, not the row id alone (research G28). | E |
| R-S1 | later server tracks | Declare every route in D's table (D136). Add `ProjectPerm` variants with the spec 08 role row and their API key scope, and extend the generated authorization suite (P18) with a request body per new route if the default `{}` is rejected before enforcement. | Wave 2 |
| R-S2 | GDPR deletion track | Project and org erasure, `deletions` tables, and user erasure; the "last owner" rule of D137 also guards org deletion. | later |

No request for `llmobs-core`: D uses `OrgId`, `ProjectId` and `time_serde` as they are.

## Sub-phase order

P1 → P2 → … → P26, strictly in order. P23–P25 (OAuth/OIDC) could move after P22 without touching anything else. P26 must be the last, and must be done by a different model from the one that implemented P1–P25.
