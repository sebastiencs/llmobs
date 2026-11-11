# Part 4 · Orgs, projects, members, API keys and PATs (P14–P18)

At the end of this part the tenancy API is complete, API keys and personal access tokens authenticate requests, `llmobs login`/`logout` work, and a generated suite checks every route against every kind of principal.

**List responses** in this part follow spec 09: `?limit` (1–1000, default 50), opaque `?cursor`, `{data, next_cursor}`. Reuse the existing `PageParams` and page types in `api::common`. The cursor encodes the last sort key (base64url of a small JSON); a cursor that does not decode is 400.

---

## P14 — Orgs and projects

**Goal:** list, create and rename orgs; list, create, read and update projects.

**Edit only:** `crates/server/src/routes/{orgs.rs,projects.rs}` (new), `crates/server/src/routes/mod.rs`, `crates/server/src/api/account.rs` (or a new `api/orgs.rs`), `crates/server/src/http/access.rs`, `crates/server/src/api/openapi.rs`, `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `fixtures/api/`, `crates/server/src/test_support.rs`.

**Commit:** `server: add the org and project endpoints`

| Route | Access | Behaviour |
|---|---|---|
| `GET /api/v1/orgs` | `User` | The caller's orgs as `OrgMembership`, by name. |
| `POST /api/v1/orgs` | `User` | `{name, slug?}`. Name 1–100 characters after trimming. `slug` given → `validate_slug`, taken → 409; absent → derived. The caller becomes `owner`. 201 `OrgMembership`. Audit `org created`. |
| `GET /api/v1/orgs/{org}` | `Org { View }` | `OrgMembership` with the caller's role. |
| `PATCH /api/v1/orgs/{org}` | `Org { Rename }` | `{name}` → 200 `OrgMembership`. Slugs do not change (URLs would break). |
| `GET /api/v1/orgs/{org}/projects` | `Org { View }` | Existing contract `list_projects`. Projects the caller can see (hidden by `none` → absent), with the effective role as `role` (`OrgRole` spelling: `admin`, `member`, `viewer`). |
| `POST /api/v1/orgs/{org}/projects` | `Org { CreateProject }` | `{name, slug?, retention_days?}`; `retention_days` 1–36 500 or absent (= forever). 201 `Project`. |
| `GET /api/v1/projects/{project}` | `Project { View }` | Existing contract `get_project`. |
| `PATCH /api/v1/projects/{project}` | `Project { ManageSettings }` | `{name?, retention_days?}`; `retention_days: null` sets forever, absent leaves it. 200 `Project`. Storage is not told here: the retention job (later) reads `projects.retention_days`. |

`Project.retention_days` is documented "Omitted when the project uses the default". Spec 04 says NULL means forever. Align the doc comment with spec 04 ("Omitted when data is kept forever") and regenerate; the field shape does not change.

PATCH bodies use a double option (`Option<Option<u32>>` with a `deserialize_with` that distinguishes absent from `null`), with `deny_unknown_fields`.

### Examples

`orgs.json` (page of `OrgMembership`), `org.json`, `project.json` and `projects.json` already exist: keep them valid.

### Tests

- Ada creates an org: owner; its slug derived; a second org with the same name gets `-2`; explicit taken slug → 409; reserved slug → 400 field error on `slug`.
- `GET /orgs` lists only the caller's orgs.
- A viewer cannot rename (403) or create a project (403); an admin can.
- Projects list hides the `none` override and reports effective roles; `GET /projects/{uuid}` and `/projects/acme%2Fsupport-bot` both work; another org's project → 404.
- PATCH retention: set 30, set null, absent; 0 and 36 501 → 400.
- Pagination: 3 orgs with `limit=2` → `next_cursor`, then the last one; a bad cursor → 400.

### Report

Standard.

---

## P15 — Members and project overrides

**Goal:** list, change and remove org members, and manage per-project overrides, with the ceilings and the last-owner rule of D137.

**Edit only:** `crates/server/src/routes/members.rs` (new), `crates/server/src/routes/mod.rs`, `crates/server/src/api/` (new `members.rs` types), `crates/server/src/http/access.rs`, `crates/server/src/api/openapi.rs`, `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `fixtures/api/`.

**Commit:** `server: add member and override endpoints`

### Types

- `MemberUser { id, email, name? }` (not `User`: org members must not learn who is an instance admin).
- `OrgMember { user: MemberUser, role: OrgRole, joined_at }`.
- `ProjectRoleOverride` enum `admin | member | viewer | none` (API spelling of `ProjectOverride`).
- `ProjectMember { user: MemberUser, org_role: OrgRole, override?: ProjectRoleOverride, effective_role?: OrgRole }` (`effective_role` absent when hidden).

### Routes

| Route | Access | Behaviour |
|---|---|---|
| `GET /api/v1/orgs/{org}/members` | `Org { View }` | Page of `OrgMember` by email. |
| `PATCH /api/v1/orgs/{org}/members/{user}` | `Org { ManageMembers, session_only }` | `{role}`. Rules below. 200 `OrgMember`. |
| `DELETE /api/v1/orgs/{org}/members/{user}` | `Org { View, session_only }` | Anyone may remove themselves; removing someone else needs `ManageMembers` (403). Rules below. 204. |
| `GET /api/v1/projects/{project}/members` | `Project { View }` | Every org member as `ProjectMember`, by email. |
| `PUT /api/v1/projects/{project}/members/{user}` | `Project { ManageMembers, session_only }` | `{role: admin | member | viewer | none}`. 200 `ProjectMember`. |
| `DELETE /api/v1/projects/{project}/members/{user}` | `Project { ManageMembers, session_only }` | Removes the override. 204 (also when there was none). |

**Org role rules** (all in one write transaction, re-reading the roles inside it):
- `{user}` is not a member of the org → 404.
- The caller cannot give a role ranked above their own; only an `owner` may set `owner` or change an owner's role, or remove an owner.
- After the change, the org must still have an owner: demoting or removing the last owner → 409 `conflict` ("an org needs an owner").
- A target who becomes `owner` or `admin` loses their overrides in this org's projects (D137: they are never overridden).
- Removing a member deletes their overrides (P6 `remove_org_member`).
- Audit `member role changed` / `member removed` with org, actor and target ids.

**Override rules:**
- `{user}` not an org member → 404. Target is an org `owner` or `admin` → 400 ("owners and admins cannot be overridden").
- The caller's effective role is `admin` (enforced by the perm); `admin` is the highest override, so the ceiling holds.
- Setting an override on oneself is allowed (a project admin lowering themselves).

### Examples

`org-members.json`, `project-members.json` (add `bob@example.com` as a second synthetic user only if needed, and document it in `fixtures/api/README.md`).

### Tests

- Admin promotes member → admin; admin tries member → owner → 403; owner does it → 200.
- Admin demotes an owner → 403; owner demotes the only owner (self) → 409; with two owners → 200.
- Self-removal by a viewer → 204; a member removing someone else → 403; removing the last owner → 409.
- Promotion to admin deletes the target's overrides (check the table).
- Overrides: project admin sets `none` on a member → the member's `GET /projects/{p}` → 404 and the project is gone from their list; set on an org admin → 400; a member (not project admin) → 403; on a non-member → 404.
- A viewer raised to `admin` by override can then manage overrides on that project but not on another project of the org (403 there).
- A PAT cannot change members (session-only) — covered generically in P18; here assert the `Access` of each route.

### Report

Standard.

---

## P16 — API keys

**Goal:** create, list and revoke project API keys; authenticate requests with `sk-`/`pk-` bearers; cache and coalesce as D138 says.

**Edit only:** `crates/server/src/meta/api_keys.rs`, `crates/server/src/auth/{principal.rs,cache.rs,last_used.rs}` (`cache.rs`, `last_used.rs` new), `crates/server/src/http/access.rs`, `crates/server/src/routes/api_keys.rs` (new), `crates/server/src/routes/mod.rs`, `crates/server/src/api/` (new `keys.rs` types), `crates/server/src/serve.rs` (flush task and shutdown flush), `crates/server/src/api/openapi.rs`, `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `fixtures/api/`, `crates/server/src/test_support.rs`.

**Commit:** `server: add project API keys`

### Store

`list_api_keys(pool, project_id) -> Vec<ApiKeyRow>` (not revoked, newest first); `api_key_by_hash(pool, hash) -> Option<ApiKeyRow>` (joined with the project's `state`); `api_key_by_id(pool, project_id, id)`; `revoke_api_key(conn, project_id, id, now) -> Option<String /*hash*/>`; `set_last_used(conn, &[(Uuid, i64)])` (one statement per row inside one transaction).

### Principal

`Principal::ApiKey { key_id: Uuid, project_id: ProjectId, kind: KeyKind, scopes: KeyScopes }` (`KeyScopes` a small bit set over `KeyScope`).

`authenticate`: `parse_key` → `Secret` or `Public` → `hash_token` → `KeyCache` (60 s, at most 10 000 entries, misses are not cached) → DB on a miss. Revoked, project not `active`, or `expires_at <= now` (checked on every use, cached or not) → 401. A `Personal` kind is P17.

`KeyCache` (`auth/cache.rs`): `get(hash, now)`, `put(hash, value, now)`, `evict(hash)`. P17 reuses it for PATs (generic over the cached value).

`enforce` for `ApiKey`:
- `Project { perm, .. }` with `session_only: false`: `{project}` must resolve to the key's project (UUID, slugs, or `current`) else 404; `perm.key_scope()` must be `Some(s)` with `s` in the key's scopes else 403; `s == Read` also requires a safe method (GET, HEAD) else 403. `ProjectAccess.role` is `None` for keys (make the field `Option<Role>`; handlers that need a user role get it only on user principals).
- Every other `Access` → 403 (keys are project-bound; public keys only serve the feedback route of a later track).

`LastUsed` (`auth/last_used.rs`): `record(id, now)` into an in-memory map (latest wins); `flush(meta)` writes and clears; `serve` flushes every 60 s and once at shutdown. Updated only on successful authentication.

### Routes

| Route | Access | Behaviour |
|---|---|---|
| `GET /api/v1/projects/{project}/api-keys` | `Project { ManageSettings, session_only }` | Page of `ApiKey { id, kind, name?, prefix, scopes, created_by?, created_at, expires_at?, last_used_at? }`, newest first. |
| `POST /api/v1/projects/{project}/api-keys` | same | `{kind: secret \| public, name?, scopes?, expires_at?}`. Secret: scopes a non-empty subset of `ingest`, `read`, `write` (default all three). Public: scopes absent or `[feedback]`. `expires_at` in the future, at most 10 years ahead. Name ≤ 100. 201 `{key: ApiKey, secret: string}`; `secret` appears only here. Audit `api key created` (key, project, user ids). |
| `DELETE /api/v1/projects/{project}/api-keys/{id}` | same | Revokes; evicts the cache entry; 204. Unknown, already revoked, or another project's key → 404. Audit `api key revoked`. |

### Examples

`api-keys.json`, `api-key-created.json` (synthetic secret, as in P11).

### Tests

- Create a secret key with `read` → `GET /api/v1/projects/current` with it → 200; `GET /projects/{uuid}` → 200; another project → 404; `PATCH /projects/{p}` → 403; `GET /api/auth/me` → 403; `GET /orgs` → 403.
- Ingest-only key → 403 on `GET /projects/current`.
- Public key → 403 everywhere in D.
- Revoke → the next request with the key → 401 at once (cache evicted). A key revoked directly in the DB (simulating another process) still works until the cache entry is 60 s old, then 401.
- Expired key → 401 even when cached.
- `last_used_at` is null until a flush, then set; shutdown flushes.
- Create, list: the list never contains the secret; the DB has the hash only; `assert_logs_clean` with the secret.
- Session-only: creating a key with a key or (later) a PAT → 403 (assert the `Access`).
- Wrong length or alphabet bearer → 401 without a DB query.

### Report

Standard.

---

## P17 — Personal access tokens and CLI login

**Goal:** `/api/v1/me/tokens`, PAT authentication with the scope rules of D138, and `llmobs login` / `llmobs logout`.

**Edit only:** `crates/server/src/meta/personal_tokens.rs`, `crates/server/src/auth/principal.rs`, `crates/server/src/http/access.rs`, `crates/server/src/routes/tokens.rs` (new), `crates/server/src/routes/mod.rs`, `crates/server/src/api/keys.rs`, `crates/server/src/api/openapi.rs`, `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `fixtures/api/`, `crates/cli/src/{main.rs,login.rs}` (`login.rs` new), `crates/cli/Cargo.toml`, root `Cargo.toml` (`reqwest`, `dirs`, `toml`, as in the README table).

**Commit:** `server: add personal access tokens and CLI login`

### Store

`insert_token`, `list_user_tokens(pool, user_id)` (not revoked, newest first), `token_by_hash(pool, hash)`, `revoke_token(conn, user_id, id, now) -> Option<String>`, `revoke_user_tokens` (exists, P13), and `LastUsed` support (tag the map by table, or keep two maps).

### Principal

`Principal::Pat { user: AuthUser, token_id: Uuid, write: bool }`. `authenticate`: `Personal` kind → cache (token row: id, user id, scopes, expires) → **the user is loaded from the DB on every request** (disabled users fail at once, D147; roles are resolved per request by `enforce`, so a role change applies at once). Revoked, expired, or user disabled → 401.

`enforce` for `Pat`: `session_only` routes and `InstanceAdmin` → 403. `write == false` and a method other than GET/HEAD → 403. Otherwise like a session user.

### Routes

| Route | Access | Behaviour |
|---|---|---|
| `GET /api/v1/me/tokens` | `User { session_only }` | Page of `PersonalToken { id, name, prefix, scopes, created_at, expires_at?, last_used_at? }`. |
| `POST /api/v1/me/tokens` | `User { session_only }` | `{name, scopes: [read] \| [read, write], expires_at?}`; name 1–100; 201 `{token: PersonalToken, secret}`. Audit. |
| `DELETE /api/v1/me/tokens/{id}` | `User { session_only }` | 204; not the caller's or revoked → 404. |
| `DELETE /api/v1/me/tokens/current` | `User` | Only with a PAT principal: revokes that token, 204. With a session → 404. |

Check that axum 0.8's router lets `/me/tokens/current` and `/me/tokens/{id}` coexist with the static route winning; if not, make `current` a reserved value handled in the `{id}` handler and document it.

### CLI

```
llmobs login --url <URL> [--no-browser]
llmobs logout [--url <URL>]
```

`crates/cli/src/login.rs`:
- `login`: bind `127.0.0.1:0`; `state = new opaque token` (generate with `rand` + base64url in the CLI; do not reach into server internals); URL `<URL>/cli-auth?port=<p>&state=<s>`; print it; unless `--no-browser`, try `xdg-open` / `open` / `cmd /c start` (ignore failures). Serve exactly one valid request: `GET /callback?token=…&state=…` with a matching state and a token that parses as a PAT; others get 400 and the wait continues; give up after 5 minutes. Answer the browser with a small static HTML page that does not echo the token. Verify the token with `GET <URL>/api/auth/me` (bearer); on success, save it.
- Credentials: `dirs::config_dir()/llmobs/credentials.toml`, directory 0700, file 0600 (Unix; written via a temp file + rename with the mode set at creation). Format: `[servers."<URL>"]` `token = "…"`. A server URL is normalized (no trailing slash).
- `logout`: with `--url`, or the only saved server; `DELETE <URL>/api/v1/me/tokens/current`; remove the entry even if the call fails (print a warning).
- The local callback server is a tiny `tokio` TCP loop parsing the request line, or axum (already a dependency of `llmobs-server`; add it to the CLI only if it keeps the code shorter). The CLI never logs the token.

### Tests

- Server: PAT `read` → GET routes work, `POST /orgs` → 403, `PATCH /orgs/{org}` → 403; PAT `read, write` → `POST /orgs` 201; any PAT → `POST /me/tokens` 403, `POST /projects/{p}/api-keys` 403, `PATCH /orgs/{org}/members/{u}` 403, `GET /admin/users` (P22) 403.
- A PAT plus a session cookie → treated as the PAT (cookie ignored): `POST /me/tokens` → 403.
- `DELETE /me/tokens/current` with a PAT → 204, then 401; with a session → 404.
- User disabled (set in DB) → PAT 401 immediately (no cache delay). Removed from the org → PAT gets 404 on that org at once.
- `POST /me/password` revokes all PATs (P13) → they get 401.
- CLI: run `login` against a `TestApp` bound on TCP (`serve::bind`) with `--no-browser`, capture the printed URL, create a PAT through the API with a session, `GET http://127.0.0.1:<port>/callback?token=…&state=…` → the credentials file contains the token with mode 0600; a wrong state → 400 and still waiting; `logout` revokes it on the server and removes the entry. Point `dirs` at a temp dir by passing the credentials path into the function (the `main` wrapper resolves `dirs::config_dir()`).

### Report

The router behaviour for `current`, and how the CLI test controls the credentials path.

---

## P18 — The generated authorization suite

**Goal:** one test drives every route in `routes()` with every kind of principal and checks the outcome predicted from its `Access` (D136, research test cases 6, 7, 11–15).

**Edit only:** `crates/server/src/http/access_suite.rs` (new, `#[cfg(test)]`), `crates/server/src/http/access.rs` (a `#[cfg(test)]` response header), `crates/server/src/test_support.rs`.

**Commit:** `server: test every route against every principal`

### The world

Through the API (not raw SQL), build: instance admin Root (member of nothing); org A with owner Ada, admin, member, viewer, a member with override `none` on project A1, a viewer with override `admin` on A1; org B with owner Bob and project B1; for A's member: PAT `read` and PAT `read, write`; for A1: secret key `read`, secret key `ingest`, public key; for B1: secret key `read`. Plus "no credentials".

### Driving the routes

- For each documented route: substitute `{org}` with A's slug, `{project}` with A1's id, `{user}` with A's member id, `{id}` with an id that exists for that route where one is cheap to create, else a fresh UUID, `{token}` and `{provider}` with syntactically valid values.
- Send `{}` with `Content-Type: application/json` and the right `Origin` for non-GET routes.
- `enforce` adds a `#[cfg(test)]` response header `x-test-access: allowed | denied-401 | denied-403 | denied-404` so the suite can tell an enforcement decision from a handler's own 404/400.
- `expected(access, method, principal) -> Outcome` is a pure function written from D136–D138 in the test, not by calling `enforce`.
- Assert the header equals the prediction for every (route, principal) pair. The failure message names the route, the principal and both outcomes.

### Extra assertions

- **Cross-tenant:** Bob, B1's key, and Root on every `Org`/`Project` route → `denied-404`.
- **No route without access:** `routes()` has no route outside the `Access` enum shapes (trivially true by type) and no route with `{project}` that is not `Project` (P10 invariant, repeated here for completeness).
- **Query-string credentials:** `GET /api/auth/me?token=<pat>` and `?api_key=<sk>` without a header → 401.
- **Secrets never echoed:** after building the world, `GET` every list route as Ada and assert no response body contains any key or PAT plaintext, and no JSON key ends with `hash`.
- **Logs:** run the whole suite under `assert_logs_clean` with every plaintext token, every email and every password used.

### Report

The number of (route, principal) pairs checked.
