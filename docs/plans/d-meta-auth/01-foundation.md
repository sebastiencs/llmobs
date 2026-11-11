# Part 1 · Foundation (P1–P5)

Config, `meta.db`, the axum app and `llmobs serve`. At the end of P5, `llmobs serve` starts, opens storage and `meta.db`, answers `/healthz` and `/readyz`, and shuts down cleanly. There are no auth routes yet.

---

## P1 — Specs and decisions

**Goal:** the specs and the decision log say what this plan builds, before any code.

**Edit only:** `docs/mvp-decisions.md`, `docs/spec/04-metadata-db.md`, `docs/spec/08-auth.md`, `docs/spec/09-api.md`, `docs/spec/12-operations.md`, and this plan directory (only to renumber decisions).

**Commit:** `docs: record the D plan decisions in the specs`

### Decision numbers

Find the highest `D<n>` in `docs/mvp-decisions.md` on `main`. If it is below 135 and no open plan reserves 135–151, keep D135–D151. Otherwise renumber them to the next free block, and update every `D13x`/`D14x`/`D15x` reference in `docs/plans/d-meta-auth/`.

### `docs/mvp-decisions.md`

- Append D135–D151 to the decision log, one row each, with the wording of the README table (shortened to one or two sentences each, as the other rows are, ending with the spec section they change).
- D43 contradicts spec 08: change it to "Email/password on by default (argon2id, spec 08 § Passwords); sign-up policy `LLMOBS_AUTH_SIGNUP` (default `invite_only`) and `LLMOBS_AUTH_DISABLE_PASSWORD`; the first user becomes instance admin and owner of the first org; GitHub, Google, generic OIDC; works without email (copyable invite links, CLI password reset). No SAML in v1." This is a wording fix, not a new decision.

### Spec 04

- Header paragraph: `synchronous=FULL` instead of `NORMAL`, with "writes are rare; every commit is durable before the response (D136)". "one writer connection (serialized writes through a `Mutex`) and a read pool of 4 read-only connections". Add: "`llmobs admin` commands open `meta.db` without running migrations and refuse a database at another migration version (D136)."
- `users.email`: `TEXT NOT NULL UNIQUE` without `COLLATE NOCASE`, comment "normalized: trimmed, NFC, lowercase (D136)". Same for `invites.email`.
- `invites`: add `CREATE UNIQUE INDEX invites_pending ON invites(org_id, email) WHERE accepted_at IS NULL;` and the sentence "A new invite for the same (org, email) replaces the pending one (D147)."
- New table after `password_resets`:

```sql
CREATE TABLE email_verifications (
  token_hash TEXT PRIMARY KEY,        -- SHA-256 hex of a 32-byte random token
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, used_at INTEGER
);
```

- `api_keys.prefix` and `personal_tokens.prefix` comment: "type prefix + first 6 random characters, e.g. `sk-llmobs-a1B2c3` (D140)".
- `web_sessions.id` comment: "SHA-256 hex of the cookie value (32 random bytes, base64url)".
- § Metering & instance: after the `instance_settings` DDL, add "Created by track E with its first use (D135)."

### Spec 08

Edit in place; keep the structure.
- § Bootstrapping: the setup page needs the setup token of D144 (`/setup?token=…`, logged once at startup while no user exists). Headless init runs once, in one transaction, only when `users` is empty; `LLMOBS_INIT_SECRET_KEY`, when set, must match the secret key format (`sk-llmobs-` + 32 base62 characters) or startup fails.
- § Sign-up policy: add "An invite token admits its own email only (D147). Signup under `open` with an email already registered answers 409 (D145)." In the `LLMOBS_AUTH_ENFORCE_SSO_DOMAINS` row, add "any configured OAuth/OIDC provider satisfies it".
- § Passwords: NFC normalization, the length counted in Unicode scalar values, the list source (SecLists `10k-most-common.txt`, entries of 10+ characters), the hashing semaphore, the dummy hash, the rate limits and lockout of D143 (in memory; keys; lockout does not block OAuth/OIDC login or password reset; reset clears it). Uniform answers of D145. Password change: `POST /api/v1/me/password` revokes every other web session and all PATs.
- § Web sessions: CSRF rule of D141 replaces the current bullet. Add: "A bearer token wins over the cookie (D140)." Add: "Disabling a user deletes their sessions."
- § OAuth / OIDC: per-provider `link_existing_by_email` (GitHub and Google default `true`, `[[auth.oidc]]` default `false`); the linking rules of D148 replace the "Account linking" bullet; refuse multi-tenant Entra issuers; `allowed_domains` checks the verified email's domain, and for Google also the `hd` claim; state cookie signed with a per-process key, 10 minutes, `SameSite=Lax`, `Path=/api/auth/callback`; the callback redirects to the relative path given to `/authorize?redirect=` (must start with `/` and not `//`), and errors redirect to `/login?error=<code>`.
- § Authorization: the override rule of D139 (who can be overridden, who edits overrides, ceilings, last owner); instance admins have no implicit access to org data; any signed-in user creates orgs. "404 for what you cannot see, 403 for what you can see but not change" (D138). Session-only operations and PAT scopes of D140. Replace "Deletion by user/project still requires `admin` (PAT or session), not an API key." with "Deletion by user or project requires `admin` with a session or a `read, write` PAT, never an API key."
- § API keys: prefix of D140; cache eviction on revoke; credentials only in the `Authorization` header. CLI login: the browser page is `/cli-auth?port=…&state=…`, the callback host is always `127.0.0.1`; `llmobs logout` revokes the PAT with `DELETE /api/v1/me/tokens/current`.
- § Email: transactional emails are sent by a background task (D145). Templates add `verify_email` links `<public_url>/verify?token=…`, reset links `<public_url>/reset?token=…`, invite links `<public_url>/invite/<token>`.
- § Auth events log: D150 (ids only; `account = "unknown"`).
- New § Client IP: D142.

### Spec 09

- § Conventions: "Credentials are accepted only in the `Authorization` header or the session cookie, never in a query string (D140)." Codes: add `conflict` (409) and `sso_required` (403), `email_not_verified` (403).
- § Auth table: `POST /setup` body `{setup_token, email, password, name?, org_name, project_name}`; add `POST /email/verify/resend`. `GET /authorize/{provider}` takes `?redirect=`.
- § Orgs, projects, members, keys: add `GET/PATCH /api/v1/me`, `POST /api/v1/me/password`, `DELETE /api/v1/me/tokens/current`, `GET /api/v1/projects/{project}/members` (effective roles and overrides), `PATCH /api/v1/admin/users/{id}`, `POST /api/v1/admin/settings/test-email`. Mark `GET/PATCH /api/v1/admin/settings` as `GET` only, "PATCH comes with the first editable setting (D149)". Mark `DELETE /orgs/{org}` and `DELETE /projects/{project}` as "GDPR deletion track". Mark `llm-connections` and `usage` as "track E / metering".

### Spec 12

- § Configuration table: add `trusted_proxies` (env `LLMOBS_TRUSTED_PROXIES`, comma-separated CIDRs, default empty, D142) and `log.format`/`log.level` stay. Add the `[init]` section keys with their `LLMOBS_INIT_*` aliases.
- § Process lifecycle, Startup: "lock data dir and open storage (`catalog.db`, replay WAL) → open `meta.db` + migrations → (log the setup URL if no user exists) → start background tasks → bind ports → `/readyz` = 200" (D137). Remove "verify encryption key" from the D-era order and add "(track E adds: verify encryption key, after `meta.db`)".
- § Security checklist: add "credentials never in query strings; request logs carry route templates, not raw URIs (D150)".

### Checks

`just check` (docs only, but the tree must stay clean). Read every changed section once more for contradictions with the README decisions.

### Report

The final decision numbers, and any spec sentence you could not reconcile.

---

## P2 — Server config

**Goal:** `ServerConfig::load` reads defaults, `llmobs.toml` and env vars with figment, rejects unknown keys and invalid values, and never prints a secret.

**Edit only:** `crates/server/src/config.rs` (new), `crates/server/src/lib.rs`, `crates/server/Cargo.toml`, root `Cargo.toml` (`figment`, `ipnet`, plus `thiserror`/`tracing`/`url` if the server crate lacks them; `url` only if already in `Cargo.lock`, else parse with `http::Uri` from axum in P4 and keep a `String` here).

**Commit:** `server: load the server config`

### Types

```rust
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub data_dir: PathBuf,                 // default "./llmobs-data"
    pub public_url: PublicUrl,             // default "http://localhost:8080"
    pub http_addr: SocketAddr,             // default 0.0.0.0:8080
    pub trusted_proxies: Vec<IpNet>,       // default empty (D142)
    pub storage: StorageSection,           // the [storage] table, see below
    pub auth: AuthSettings,
    pub smtp_url: Option<SmtpUrl>,         // P19 adds the parsed type; P2 keeps Option<Secret<String>>
    pub email_from: Option<String>,
    pub init: InitConfig,
    pub log: LogConfig,
}
```

- `PublicUrl`: parsed and validated at load: scheme `http` or `https`, a host, no query or fragment, path empty or `/` (sub-path deployments are not supported). Exposes `origin() -> &str` (scheme://host[:port], no trailing slash), `is_https()`, and `join(path) -> String`.
- `AuthSettings` (spec 08 § Sign-up policy): `signup: SignupPolicy` (reuse `api::account::SignupPolicy`, default `InviteOnly`), `signup_domains: Vec<String>`, `disable_password: bool`, `require_email_verification: bool`, `enforce_sso_domains: Vec<String>`. P23 adds `github`, `google`, `oidc`; until then they are not fields (no placeholders).
- `InitConfig`: `admin_email`, `admin_password` (a `Secret<String>`), `org`, `project`, `secret_key` (`Secret<String>`), all `Option`.
- `LogConfig`: `format: LogFormat` (`pretty` | `json`, default: decided by the CLI from `isatty`, so `Option<LogFormat>` here), `level: String` (default `info`, an `EnvFilter` directive).
- `Secret<T>`: a newtype whose `Debug` prints `Secret(***)` and that has no `Display`; `expose(&self) -> &T`. It exists because config values are logged with `{:?}` in errors and must never show secrets. Tests assert the `Debug` output.
- `StorageSection`: how it maps to `llmobs_storage::StorageConfig` depends on that type. `StorageConfig` has `data_dir` and `#[serde(deny_unknown_fields)]`. Deserialize the `[storage]` table into `StorageConfig` directly, then set its `data_dir` from the top-level `data_dir`; if the user set `storage.data_dir`, fail with "set `data_dir` at the top level". Check whether `StorageConfig`'s `data_dir` is `#[serde(default)]`; if it is required, deserialize the section through a `serde_json::Value`/figment value with `data_dir` inserted. Choose the simplest path that keeps one source of truth, and describe it in a doc comment.

`impl ServerConfig { pub fn load(path: Option<&Path>) -> Result<Self, ConfigError>; pub fn storage_config(&self) -> StorageConfig; }`

### Sources (spec 12)

1. Defaults (`Serialized::defaults` of a `Default`-like struct, or `#[serde(default = …)]` per field).
2. The TOML file at `path`, else `$LLMOBS_CONFIG`, else none. A path given but missing is an error.
3. Env: `Env::prefixed("LLMOBS_").split("__")` maps `LLMOBS_STORAGE__WAL__SYNC` to `storage.wal.sync` (D90). Then the **aliases**, which win over the split form:

| Env | Key |
|---|---|
| `LLMOBS_DATA_DIR` | `data_dir` |
| `LLMOBS_PUBLIC_URL` | `public_url` |
| `LLMOBS_HTTP_ADDR` | `http_addr` |
| `LLMOBS_TRUSTED_PROXIES` | `trusted_proxies` (comma-separated) |
| `LLMOBS_OBJECT_STORE` | `storage.object_store` |
| `LLMOBS_SMTP_URL`, `LLMOBS_EMAIL_FROM` | `smtp_url`, `email_from` |
| `LLMOBS_AUTH_SIGNUP` | `auth.signup` |
| `LLMOBS_AUTH_SIGNUP_DOMAINS` | `auth.signup_domains` (comma-separated) |
| `LLMOBS_AUTH_DISABLE_PASSWORD` | `auth.disable_password` |
| `LLMOBS_AUTH_REQUIRE_EMAIL_VERIFICATION` | `auth.require_email_verification` |
| `LLMOBS_AUTH_ENFORCE_SSO_DOMAINS` | `auth.enforce_sso_domains` (comma-separated) |
| `LLMOBS_INIT_ADMIN_EMAIL`, `…_ADMIN_PASSWORD`, `…_ORG`, `…_PROJECT`, `…_SECRET_KEY` | `init.*` |
| `LLMOBS_LOG` | `log.level` |

Aliased names must not also be read by the split provider (`LLMOBS_AUTH_SIGNUP` would otherwise become a top-level `auth_signup` and fail `deny_unknown_fields`): filter them out of the prefixed provider. `LLMOBS_CONFIG` is not a key; filter it too. Comma-separated values: split, trim, drop empty items.

### Validation (after extraction)

- `signup_domains`, `enforce_sso_domains`: lowercase, no `@`, non-empty labels.
- `require_email_verification = true` without `smtp_url` → error "email verification needs `smtp_url`" (spec 08 § Email).
- `auth.disable_password = true` with `auth.signup = open` and no OAuth provider: allowed in P2 (providers come in P23), P23 adds the check "password disabled and no provider configured".
- `init`: either all of `admin_email`, `admin_password`, `org`, `project` are set, or none (`secret_key` optional). Invalid email → error naming the key, not the value.
- `http_addr` parse errors name the key.
- `ConfigError` variants: `Figment(figment::Error)` (its message names the key; check that figment never includes the value for a type error on a `Secret` — if it does, map to a message with the key only), `Invalid { key: &'static str, reason: &'static str }`, `MissingFile(PathBuf)`.

### Tests (`config.rs`)

Use `figment::Jail` only if it does not need the `test` feature at runtime; otherwise build figments from `Toml::string` and an explicit env map (refactor `load` into `load_from(toml: Option<String>, env: &[(&str, &str)])` used by `load` and the tests; no process-env mutation in tests).
- defaults: every default of spec 12 and spec 08;
- TOML overrides defaults; env overrides TOML; an alias overrides the split form;
- `LLMOBS_STORAGE__WAL__SYNC="5ms"` reaches `storage.wal.sync`;
- unknown key in TOML → error naming it; unknown `LLMOBS_FOO` → error;
- `storage.data_dir` set → error; the top-level `data_dir` reaches `storage_config().data_dir`;
- comma lists; `trusted_proxies` with an invalid CIDR → error naming the key;
- `public_url` with a path, a query, `ftp://` → errors; `origin()` for `http://localhost:8080/` is `http://localhost:8080`;
- verification without SMTP → error; partial `init` → error;
- `format!("{:?}", config)` with `init.admin_password = "hunter2hunter2"` and `init.secret_key` set contains neither value.

### Report

How `[storage]` maps to `StorageConfig`, and whether figment error messages can contain values.

---

## P3 — `meta.db`

**Goal:** `MetaDb` opens `<data_dir>/meta.db` with the pragmas of D136, runs the first migration, and gives one serialized writer and a read pool.

**Edit only:** `crates/server/src/meta/mod.rs` (new), `crates/server/src/clock.rs` (new), `crates/server/migrations/meta/0001_identity.sql` (new), `crates/server/src/lib.rs`, `crates/server/Cargo.toml` (`sqlx`, `tokio`, `uuid`, `tempfile` dev).

**Commit:** `server: open meta.db with its first migration`

### Migration `0001_identity.sql`

Exactly the spec 04 DDL (as edited in P1) for: `users`, `identities`, `web_sessions`, `orgs`, `org_members`, `projects`, `project_members`, `invites` (+ `invites_pending`), `password_resets`, `email_verifications`, `api_keys`, `personal_tokens`. Plus these indexes, which the queries of P6–P17 need:

```sql
CREATE INDEX identities_user ON identities(user_id);
CREATE INDEX web_sessions_user ON web_sessions(user_id);
CREATE INDEX org_members_user ON org_members(user_id);
CREATE INDEX project_members_user ON project_members(user_id);
CREATE INDEX projects_org ON projects(org_id);
CREATE INDEX api_keys_project ON api_keys(project_id);
CREATE INDEX personal_tokens_user ON personal_tokens(user_id);
CREATE INDEX password_resets_user ON password_resets(user_id);
CREATE INDEX email_verifications_user ON email_verifications(user_id);
```

Name the file the way `crates/storage/migrations/catalog/0001_catalog.sql` is named, so sqlx sorts it the same way.

### Types

```rust
pub(crate) struct MetaDb { writer: tokio::sync::Mutex<SqliteConnection>, readers: SqlitePool }

impl MetaDb {
    /// Creates `meta.db` if missing, applies pragmas, runs migrations. `llmobs serve` only.
    pub(crate) async fn open(data_dir: &Path) -> Result<Self, MetaError>;
    /// Opens an existing `meta.db` without migrating. Fails with `MetaError::NotInitialized`
    /// when the file is missing, and `MetaError::SchemaMismatch { found, expected }` when the
    /// latest applied migration is not the binary's latest. `llmobs admin` only.
    pub(crate) async fn open_existing(data_dir: &Path) -> Result<Self, MetaError>;
    pub(crate) fn reader(&self) -> &SqlitePool;
    /// Runs `f` in one `BEGIN IMMEDIATE` transaction on the writer and commits.
    pub(crate) async fn write<T, F>(&self, f: F) -> Result<T, MetaError>
    where F: for<'c> AsyncFnOnce(&'c mut SqliteConnection) -> Result<T, MetaError>;
    pub(crate) async fn ping(&self) -> Result<(), MetaError>;   // SELECT 1 on the pool
    pub(crate) async fn close(self);
}
```

- The writer connection: `SqliteConnectOptions` with `create_if_missing(true)`, WAL, `SqliteSynchronous::Full`, `foreign_keys(true)`, `busy_timeout(5 s)`. The readers: same file, `read_only(true)`, `max_connections(4)`, same pragmas that apply to readers (`foreign_keys`, `busy_timeout`).
- `write`: lock the mutex, `BEGIN IMMEDIATE`, run `f`, `COMMIT`; on `Err` (or panic unwinding through the future being dropped) `ROLLBACK`. The closure shape above needs Rust's async closures; if the pinned toolchain or sqlx lifetimes make that awkward, use `F: FnOnce(&mut SqliteConnection) -> BoxFuture<'_, Result<T, MetaError>>`. Pick one, document it on the method, and use it everywhere.
- A dropped future must never leave a transaction open: if `f` is cancelled mid-way, the next `write` must find the connection outside a transaction (issue `ROLLBACK` when sqlx reports an open transaction, or use sqlx's `Transaction` type, which rolls back on drop). Test it.
- `MetaError`: `Open { path, source }`, `Migrate { source }`, `NotInitialized`, `SchemaMismatch { found: i64, expected: i64 }`, `Db(sqlx::Error)`, `Conflict(&'static str)` (unique violations mapped by the stores, P6), `Corrupt { table: &'static str, column: &'static str }` (a row that does not decode). Messages carry no row values.

`clock.rs`:

```rust
#[derive(Clone)]
pub(crate) enum Clock { System, #[cfg(test)] Manual(Arc<AtomicI64>) }
impl Clock { pub(crate) fn now_ns(&self) -> i64; #[cfg(test)] pub(crate) fn advance(&self, d: Duration); }
```

`System` reads `SystemTime::now()`; a time before the epoch or beyond `i64` ns is not possible on supported systems: document the invariant next to the conversion instead of unwrapping silently.

### Tests (`meta/mod.rs`)

- `open` on an empty temp dir creates the file; `PRAGMA journal_mode` is `wal`, `synchronous` is `2`, `foreign_keys` is `1` on the writer and on a pooled reader.
- Schema snapshot: `SELECT type, name, sql FROM sqlite_master WHERE name NOT LIKE '\_sqlx%' ESCAPE '\' ORDER BY name` pinned with `insta`.
- `open` twice (second process simulated by a second `MetaDb::open` after closing the first) is idempotent.
- `open_existing`: missing file → `NotInitialized`; after `open` → ok; with an extra row inserted into `_sqlx_migrations` at a higher version → `SchemaMismatch`.
- `write` commits; an `Err` from `f` rolls back (row absent); a future dropped after its first statement leaves no open transaction (the next `write` succeeds and the dropped write's row is absent).
- Readers cannot write (`INSERT` through the pool fails).
- Concurrency: 50 concurrent `write`s each inserting one org row all succeed, and the count is 50.

### Report

Which `write` signature you chose and why.

---

## P4 — HTTP skeleton

**Goal:** an axum router with shared state, problem+json errors, the client-IP rule, request logging by route template, `/healthz` and `/readyz`, and the `TestApp` harness.

**Edit only:** `crates/server/src/http/{mod.rs,error.rs,client_ip.rs,health.rs}` (new), `crates/server/src/test_support.rs` (new), `crates/server/src/lib.rs`, `crates/server/src/api/common.rs` (new problem codes), `crates/server/Cargo.toml`, root `Cargo.toml` (`axum`, `tower`).

**Commit:** `server: add the HTTP skeleton and health probes`

### State and router

```rust
#[derive(Clone)]
pub(crate) struct AppState(Arc<AppInner>);
pub(crate) struct AppInner {
    pub(crate) config: ServerConfig,
    pub(crate) meta: MetaDb,
    pub(crate) storage: Arc<llmobs_storage::Storage>,
    pub(crate) clock: Clock,
}
pub(crate) fn router(state: AppState) -> Router;
```

Later sub-phases add fields (`limits`, `principals`, `mailer`, `setup`, …) to `AppInner` as they need them.

- Routes in P4: `GET /healthz`, `GET /readyz`. Unknown paths: 404 problem `not_found`; a known path with the wrong method: 405 with a problem body (`bad_request` code, status 405 — add `ProblemCode::MethodNotAllowed` only if a 405 code does not fit `bad_request`; prefer adding `method_not_allowed` with status 405 for clarity). Check how axum 0.8 lets you replace its default 404/405 bodies (`Router::fallback`, `method_not_allowed_fallback`).
- Body limit: `DefaultBodyLimit::max(1 MiB)` for the whole router (D's bodies are small; later tracks raise it per route).
- JSON depth: serde_json's default recursion limit (128) matches spec 12; note it in a comment.

### Errors (`http/error.rs`)

```rust
#[derive(Debug, thiserror::Error)]
pub(crate) enum ApiError {
    #[error("bad request")] BadRequest { detail: Option<String>, errors: Vec<FieldError> },
    #[error("unauthorized")] Unauthorized,
    #[error("forbidden")] Forbidden { code: ProblemCode },      // forbidden, sso_required, email_not_verified
    #[error("not found")] NotFound,
    #[error("conflict")] Conflict { detail: &'static str },
    #[error("rate limited")] RateLimited { retry_after_s: u64 },
    #[error("internal error")] Internal(#[source] Box<dyn std::error::Error + Send + Sync>),
}
impl IntoResponse for ApiError { … }   // application/problem+json, `Problem::new`
impl From<MetaError> for ApiError { … } // Conflict → Conflict, the rest → Internal
```

- `Internal` logs the source at `error` with `tracing` (the message of a `MetaError` has no values, P3) and answers a generic `detail`.
- `RateLimited` sets `Retry-After` and the `RateLimit-*` headers of spec 09 (`RateLimit-Limit`, `RateLimit-Remaining: 0`, `RateLimit-Reset`).
- axum's own rejections (`JsonRejection`, `PathRejection`, `QueryRejection`) become `BadRequest` problems; `JsonRejection::MissingJsonContentType` becomes 415 with code `bad_request` (add `unsupported_media_type` if you prefer a precise code; then add it to `ProblemCode` and spec 09). Their `detail` is axum's message only when it cannot contain request content: check what `JsonRejection` messages include (serde errors can quote input) and use a fixed message per variant instead (`"invalid JSON body"`, plus the serde path, never the value).
- New `ProblemCode`s: `Conflict` (409), `SsoRequired` (403), `EmailNotVerified` (403), and whatever the previous bullets decided. Update `status()`, `reason_phrase()`, the snapshot tests in `api/common.rs`, and `just openapi`.

### Client IP (`http/client_ip.rs`)

`pub(crate) fn client_ip(peer: IpAddr, headers: &HeaderMap, trusted: &[IpNet]) -> IpAddr` (D142): if `peer` is not trusted, return `peer`. Else walk `X-Forwarded-For` from the right (all header lines, comma-separated, trimmed); return the first address not in `trusted`; malformed entries stop the walk and return the last good one (or `peer` if none). The router is served with `into_make_service_with_connect_info::<SocketAddr>()`; a tiny extractor `ClientIp(IpAddr)` reads `ConnectInfo` and the config. In `oneshot` tests there is no `ConnectInfo`: `TestApp` inserts a `ConnectInfo` extension (default `127.0.0.1:50000`, settable per request).

### Request logging

A `from_fn` middleware logs one `info` event per request after the response: `method`, `route` (the `MatchedPath` template, or `"<unmatched>"`), `status`, `duration_ms`. Never the URI, query or headers (D150). `target = "http"`.

### Health

- `GET /healthz` → 200 `{"status":"ok"}`.
- `GET /readyz` → 200 `{"status":"ready"}` when `meta.ping()` succeeds and `storage.health()` reports neither WAL failed; else 503 problem `busy` with detail naming which check failed (`"meta.db"`, `"spans WAL"`, `"scores WAL"`). The object-store and disk checks of spec 12 come with the tracks that own them; add a `// spec 12 also lists …` comment only if it is not a placeholder (it is a note, not code).
- Both are public and not in the OpenAPI document (spec 09 lists them as probes, not API). Check whether the openapi test requires every route to be documented once P10 exists; the route table marks them `documented: false`.

### `TestApp` (`test_support.rs`, `#[cfg(test)]`)

- `TestApp::new().await`: temp dir; `ServerConfig` from defaults with `data_dir` = temp dir and `public_url` = `http://localhost:8080`; `Storage::open(config.storage_config())`; `MetaDb::open`; `Clock::Manual` at `2026-01-01T00:00:00Z`; `router(state)`.
- `TestApp::with_config(|c| …)` to tweak config before opening.
- `app.request(Method, path) -> TestRequest` with `.json(value)`, `.header(k, v)`, `.peer(ip)`, `.origin(Some/None)` (default `Some(public origin)` for non-GET), `.send().await -> TestResponse { status, headers, body: Bytes }`, `.json::<T>()`, `.problem() -> Problem`.
- `assert_logs_clean(needles: &[&str])`: captures `tracing` output for the duration of a closure (a `tracing_subscriber` layer writing to a buffer, installed with `with_default` for the current thread; the router runs on the same thread in `#[tokio::test]` current-thread runtime) and fails if any needle appears. Later sub-phases use it with emails, passwords and token values.

### Tests (`http/*`)

- `/healthz` 200; `/readyz` 200; `/readyz` 503 with `busy` after the `meta.db` pool is closed (close the pool in the test).
- Unknown path 404 problem with `content-type: application/problem+json`; wrong method 405 problem.
- A route added in a test-only router that takes `Json<serde_json::Value>`: wrong content type → 415 problem; invalid JSON → 400 problem whose `detail` does not contain the invalid input text (send `{"secret": tru` and assert `tru` is absent); body over 1 MiB → 413 problem.
- `client_ip`: untrusted peer ignores XFF; trusted peer with `XFF: 203.0.113.9, 10.0.0.2` and `10.0.0.0/8` trusted → `203.0.113.9`; all entries trusted → leftmost; malformed entry → stops; IPv6.
- Request logging: a request to a route with a path parameter logs the template, and the parameter value does not appear (`assert_logs_clean`).
- `ApiError::RateLimited` sets `Retry-After` and `RateLimit-*`.

### Report

The axum mechanisms used for 404/405 bodies and whether `JsonRejection` messages can contain input.

---

## P5 — `llmobs serve`

**Goal:** `llmobs serve [--config path]` starts the server and shuts it down cleanly; the startup order is D137.

**Edit only:** `crates/server/src/serve.rs` (new), `crates/server/src/lib.rs`, `crates/cli/src/main.rs`, `crates/cli/Cargo.toml` (`tokio`, `tracing`, `tracing-subscriber`, `anyhow`), `crates/server/Cargo.toml`.

**Commit:** `cli: add llmobs serve`

### `serve::run`

```rust
pub async fn run(config: ServerConfig, shutdown: impl Future<Output = ()> + Send + 'static) -> Result<(), ServeError>;
pub async fn bind(config: ServerConfig) -> Result<Running, ServeError>;  // used by run and by tests
pub struct Running { pub local_addr: SocketAddr, /* join handle, shutdown sender, state */ }
impl Running { pub async fn shutdown(self) -> Result<(), ServeError>; }
```

Order (D137):
1. Create `data_dir` if missing.
2. `Storage::open(config.storage_config())` (takes the lock; a second process fails here with `ServeError::Storage`, whose message says the data dir is in use when that is the cause — check how A1's `OpenError` reports a held lock).
3. `MetaDb::open(&config.data_dir)`.
4. Build `AppState`, start background tasks (none yet; P8 and P16 add theirs, registered in one `Vec<JoinHandle>` owned by `Running`).
5. Bind `http_addr` (`tokio::net::TcpListener`), log `info` "listening" with the address.
6. Serve with `axum::serve(…).with_graceful_shutdown(…)`.

Shutdown (spec 12): stop accepting, drain in-flight requests for at most 20 s (then drop them), stop background tasks (each one gets a final flush call where it has one: P16's `last_used_at`), `storage.shutdown()` (best effort, ≤ 30 s, log on error), close `meta.db`. `ServeError` variants: `Config`, `Storage(OpenError)`, `Meta(MetaError)`, `Bind { addr, source }`, `Serve(io::Error)`.

### CLI

```
llmobs serve [--config <path>]
```

- `--config` overrides `LLMOBS_CONFIG`.
- Logging: `tracing_subscriber` with `EnvFilter` from `config.log.level`; format from `config.log.format`, else `pretty` when stdout is a terminal (`std::io::IsTerminal`), else `json`.
- Signals: SIGINT and SIGTERM (`tokio::signal::unix` on Unix, `ctrl_c` elsewhere) trigger shutdown; a second signal exits immediately with status 130.
- Errors: `anyhow` with context (`"loading config"`, `"opening storage"`, …); exit status 1. Config errors print the key, never the value (P2 guarantees it).
- `llmobs version` stays.

### Tests

- `serve.rs`: `bind` on `127.0.0.1:0` with a temp data dir; `GET /healthz` over real TCP (use `tokio::net::TcpStream` and a hand-written HTTP/1.1 request, or `hyper`'s client if it is already a dependency through axum; do not add `reqwest` for this) returns 200; `shutdown` returns `Ok` and the data dir LOCK is released (a second `bind` on the same dir then succeeds).
- A second `bind` on the same data dir while the first runs fails with `ServeError::Storage` (it stopped at step 2, before `meta.db`).
- Shutdown with an open keep-alive connection that is idle completes within the drain limit.
- CLI: `llmobs serve --config /missing.toml` exits 1 with a message naming the path (an `assert_cmd`-style test is not available; test the `run_cli(args) -> anyhow::Result<()>` function that `main` calls).

### Report

How a held lock is reported, and the shutdown timings you used.
