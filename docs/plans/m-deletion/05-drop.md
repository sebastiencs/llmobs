# M5 — Project and org drop routes

Read [README.md](README.md) first, especially § Design › Lifecycle (project and org kinds), Cascades and Access.

**Goal:** the routes that create `project` and `org` deletions, plus the org-level history:
- `DELETE /api/v1/projects/{project}?confirm=<project slug>` → `202` `Deletion`;
- `DELETE /api/v1/orgs/{org}?confirm=<org slug>` → `202` `Deletion`;
- `GET /api/v1/orgs/{org}/deletions` → `Page<Deletion>`, newest first.

The project or org becomes `deleting` in the request transaction, so it is `404` everywhere at once. M3's worker drops it within seconds. Orgs being deleted also leave the caller's org list.

**Edit only:**
- `crates/server/src/api/deletions.rs` (`DropParams`);
- `crates/server/src/auth/roles.rs` (`ProjectPerm::Drop`, `OrgPerm::Delete`, `OrgPerm::ViewDeletions`);
- `crates/server/src/routes/deletions.rs` (handlers and routes);
- `crates/server/src/meta/projects.rs` (`mark_project_deleting`);
- `crates/server/src/meta/orgs.rs` (`mark_org_deleting`; the `orgs_of_user` filter);
- `crates/server/src/api/openapi.rs`;
- `crates/server/src/api_examples.rs`, `crates/server/tests/api_examples.rs`;
- `fixtures/api/static/org-deletions.json`, `deletion-project.json`, `deletion-org.json` (new); `fixtures/api/manifest.json` (regenerated);
- `crates/server/openapi.json`, `web/src/api/schema.gen.ts` (regenerated);
- `web/src/mocks/writes.ts`, `web/src/mocks/writes.test.ts`.

**Commit:** `Add project and org deletion routes`

## Read first

- M4's `routes/deletions.rs` (`deletion_json`, `routes()`, `field_error`) and `api/deletions.rs`.
- M3's `deletion/mod.rs`: `drop_project_step`, `drop_org_step`, and the "superseded" rule.
- `crates/server/src/routes/projects.rs`: `routes()`, `ProjectAccess`. `crates/server/src/routes/orgs.rs`: `OrgAccess`, `list_orgs`.
- `crates/server/src/http/access.rs`:
  - `grant_org`, `grant_session_project`, `key_only_project`: a project or org whose `state` is not `active` is `404`;
  - `build_router`: two `RouteDef`s with the same path and different methods merge.
- `crates/server/src/auth/roles.rs`: `OrgPerm::min_role`, `ProjectPerm::{min_role, key_scope}`.
- `crates/server/src/auth/principal.rs`: `session_user(&Principal)`, which refuses an API key.
- `crates/server/src/meta/members.rs`: `org_role(pool, org_id, user_id) -> Option<OrgRole>`.
- `crates/server/src/meta/orgs.rs`: `orgs_of_user`, `OrgRow`. `crates/server/src/meta/projects.rs`: `ProjectRow`.
- `crates/server/src/api/openapi.rs`: `project_paths`, which builds the `PathItem`s of `/api/v1/orgs/{org}` and `/api/v1/projects/{project}`.

## Types

`api/deletions.rs`:

```rust
/// Query of `DELETE /api/v1/projects/{project}` and `DELETE /api/v1/orgs/{org}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct DropParams {
    /// The slug of the project or organization being deleted, typed to confirm.
    pub confirm: String,
}
```

No `into_params` attribute: `with_query` places the parameters in the query, as for `PageParams`.

`DropParams` must be an extractor in the handler signature (`error::Query<DropParams>`), so a request without `confirm` is refused with `400` before the handler body runs. `http/access_suite.rs` sends every documented route as every principal, the org owner included, with no query: this is what keeps it from dropping its own project or org. The suite needs no other change; it derives expectations from `min_role` and `key_scope`.

## Permissions

`auth/roles.rs`:
- `OrgPerm::Delete` (`/// Delete the org (owner only, spec 08).`): `min_role` `OrgRole::Owner`.
- `OrgPerm::ViewDeletions` (`/// The org's deletion history (D-M-8).`): `min_role` `OrgRole::Admin`.
- `ProjectPerm::Drop` (`/// Delete the project. The handler also requires org admin (spec 08, D-M-4).`): `min_role` `Role::Admin`, `key_scope` `None`.

Add `Self::ViewDeletions` to the existing arm of `OrgPerm::min_role` that returns `OrgRole::Admin` (it also lists track I's `ManageConnections`), and a new arm `Self::Delete => OrgRole::Owner`. In `ProjectPerm::min_role`, add `Self::Drop` to the arm returning `Role::Admin`; in `ProjectPerm::key_scope`, `Self::Drop => None`.

Extend `permissions_follow_the_spec_table` in `roles.rs`: `OrgPerm::Delete.min_role() == OrgRole::Owner`, `OrgPerm::ViewDeletions.min_role() == OrgRole::Admin`, `ProjectPerm::Drop.min_role() == Role::Admin`, `assert!(ProjectPerm::Drop.key_scope().is_none())`; and update its comment "Owner-only rows (delete org, transfer ownership) are not permissions yet" to say only transfer ownership is not a permission yet.

## Store

`meta/projects.rs`:

```rust
/// `active` → `deleting`. Returns whether the row changed.
pub(crate) async fn mark_project_deleting(conn: &mut SqliteConnection, id: ProjectId, now: i64) -> Result<bool, MetaError>;
```
`UPDATE projects SET state = 'deleting', updated_at = ? WHERE id = ? AND state = 'active'`. M3 already added `project_ids_of_org`; call M3's variant inside the transaction: `project_ids_of_org(&mut *conn, org)` if it is generic over the executor, else its `_in` twin.

`meta/orgs.rs`:

```rust
/// `active` → `deleting`. Returns whether the row changed.
pub(crate) async fn mark_org_deleting(conn: &mut SqliteConnection, id: OrgId, now: i64) -> Result<bool, MetaError>;
```

and in `orgs_of_user`, add `AND orgs.state = 'active'` to the `WHERE` clause, so `GET /api/auth/me` stops listing orgs being deleted (`GET /api/v1/orgs` already filters them in `routes/orgs.rs::list_orgs`; leave that filter, it is harmless). Update its doc comment: "Active orgs the user belongs to…". Its callers: `routes/auth.rs::me_from` (also in the login, signup and setup answers), `routes/orgs.rs::list_orgs`, `mcp/tools.rs` `list_projects` (already skips inactive orgs; its `mcp/mod.rs` test that marks an org deleting still passes), and a test in `meta/members.rs` that uses active orgs.

Add `meta` tests: `mark_project_deleting` and `mark_org_deleting` return `true` then `false`; `orgs_of_user` omits an org after `mark_org_deleting`.

## Routes

Add to `routes/deletions.rs::routes()`:

```rust
RouteDef { method: Method::DELETE, path: "/api/v1/projects/{project}",
           access: Access::Project { perm: ProjectPerm::Drop, session_only: false }, documented: true, handler: delete(drop_project) },
RouteDef { method: Method::DELETE, path: "/api/v1/orgs/{org}",
           access: Access::Org { perm: OrgPerm::Delete, session_only: false }, documented: true, handler: delete(drop_org) },
RouteDef { method: Method::GET, path: "/api/v1/orgs/{org}/deletions",
           access: Access::Org { perm: OrgPerm::ViewDeletions, session_only: false }, documented: true, handler: get(list_org_deletions) },
```

`DELETE /api/v1/projects/{project}` shares its path with the `GET`/`PATCH` in `routes/projects.rs`; `build_router` merges them. The same for `/api/v1/orgs/{org}`. `routes/deletions.rs` needs `use axum::routing::delete;`. The `meta.write` closures are `move` and `'static`: copy `user.id`, `access.project.id`, `access.project.org_id` and clone `name`/`slug` into locals before them (`user` borrows `principal`).

### `drop_project`

```rust
async fn drop_project(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Query(params): error::Query<DropParams>,
) -> Result<(StatusCode, error::Json<Deletion>), ApiError>
```

1. `let user = principal::session_user(&principal)?;`. A key never reaches here: `ProjectPerm::Drop` has no key scope, so `grant_key_project` answers `403`.
2. Org role: `crate::meta::members::org_role(state.meta.reader(), access.project.org_id, user.id).await?`. Not `Owner` or `Admin` → `ApiError::Forbidden { code: ProblemCode::Forbidden }`. A project admin by override only is refused here (spec 08: deleting projects is an org admin action).
3. `params.confirm != access.project.slug` → `field_error("confirm", "type the project slug to confirm")`.
4. `id = Uuid::now_v7()`, `now = state.clock.now_ns()`. One `meta.write`:
   - `if !mark_project_deleting(conn, access.project.id, now).await? { return Err(MetaError::Conflict("the project is already being deleted")) }`;
   - `insert_deletion(conn, &store::NewDeletion { id, org_id: access.project.org_id, project_id: Some(access.project.id), kind: store::DeletionKind::Project, user_id: None, request: store::RequestDetails { name: Some(access.project.name.clone()), slug: Some(access.project.slug.clone()), ..Default::default() }, actor_kind: store::ActorKind::User, actor_id: user.id, state: store::DeletionState::Pending, created_at: now }).await?`.
5. `state.deletions.wake.notify_one()`; audit `deletion requested` with the same fields as M4.
6. `202` with `deletion_json(&row)`.

### `drop_org`

`Extension(access): Extension<OrgAccess>`, the same principal and query.
1. `session_user` (an API key gets `404` from `grant_org` before this).
2. `params.confirm != access.org.slug` → `field_error("confirm", "type the organization slug to confirm")`.
3. One `meta.write`:
   - `mark_org_deleting` (false → `Conflict("the organization is already being deleted")`);
   - `let projects = project_ids_of_org(&mut *conn, org).await?`;
   - `insert_deletion` (kind `Org`, `project_id: None`, `request.name`/`slug` of the org, `state: Pending`);
   - `insert_projects(conn, id, &projects)`.
4. Wake, audit (`project_id` field omitted: use a second `tracing::info!` form without it), `202`.

### `list_org_deletions`

The same paging as M4's `list_deletions`, over `page_of_org(reader, access.org.id, after, limit + 1)`. Factor it into private helpers in `routes/deletions.rs` and make M4's `list_deletions` use them: `fn after_of(params: &PageParams) -> Result<Option<(i64, Uuid)>, ApiError>` (cursor decoding) and `fn page(rows: Vec<store::DeletionRow>, limit: u32) -> Result<Page<Deletion>, ApiError>` (truncate, next cursor, mapping).

After an org is dropped, its own deletion row is reachable only in `meta.db` (and in the `202` answer and the audit log): `GET /orgs/{org}/deletions` is `404` once the org is gone. Accepted; the OpenAPI description of `delete_org` says so.

## OpenAPI

In `project_paths`, add a `delete` operation to the existing `PathItem` of `/api/v1/projects/{project}` and to that of `/api/v1/orgs/{org}`. Set `item.delete = Some(op)` before the item goes into `paths.path(…)`.

| Operation id | Tag | Summary | Parameters | Success | Errors |
|---|---|---|---|---|---|
| `delete_project` | `projects` | Delete a project (org admins; API keys always get 403) | `with_query::<DropParams>(vec![project_param()])` | `202` `json_of::<Deletion>("The deletion. The project is gone from every route at once; its data is erased in the background.")` | `[400, 401, 403, 404, 409, 429]` |
| `delete_org` | `orgs` | Delete an organization (owners) | `with_query::<DropParams>(vec![path_string("org", "Org UUID or slug.")])` | `202`, the same about the organization and all its projects; the description adds "Its deletion record is then kept in the database and the audit log only." | `[400, 401, 403, 404, 409, 429]` |
| `list_org_deletions` | `deletions` | List an organization's deletions | `path_string("org", …)` + `with_query::<PageParams>` | `200` `json_named("DeletionPage", "One page of deletions, newest first.")` | `QUERY_ERRORS` |

Put `list_org_deletions` at `/api/v1/orgs/{org}/deletions` in M4's `deletion_paths`. `409` means the project or org was already being deleted by a racing request.

Update `crates/server/tests/openapi.rs`: add the three operations to `OPERATIONS`; `delete_project` and `delete_org` get a branch `["400", "401", "403", "404", "409", "429"]`; `list_org_deletions` goes in `QUERY_OR_BODY`. Run `just openapi`.

## Examples

- `fixtures/api/static/deletion-project.json`: a `Deletion` with:
  - id `01900000-0000-7000-8000-000000000903`, `kind` `project`, `status` `pending`;
  - `org_id` acme, `project_id` the second project `01900000-0000-7000-8000-000000000002`;
  - `name` `Playground`, `slug` `playground`;
  - requested by user `…0100`, `created_at` `2026-01-03T08:00:00.000000000Z`.
- `fixtures/api/static/deletion-org.json`: id `…0904`, `kind` `org`, `status` `pending`, `org_id` acme, no `project_id`, `name` `Acme`, `slug` `acme`, the same requester, `created_at` `2026-01-03T09:00:00.000000000Z`.
- `fixtures/api/static/org-deletions.json`: a `Page<Deletion>` with `deletion-project.json`'s row, then the three rows of `deletions.json`, newest first, `next_cursor` null.
- `api_examples.rs`, in the static `out.manifest.extend([…])` list:
  - `example("list_org_deletions", params(&[("org", "acme")]), BTreeMap::new(), "static/org-deletions.json", "application/json")`;
  - two `202` entries with path params. No helper fits (`example_status` has no path params; `example_created` is fixed at 201), so add `fn example_accepted(operation_id: &str, path_params: BTreeMap<String, String>, file: &str) -> Example` next to `example_created`, identical except `status: 202`, and add `example_accepted("delete_project", BTreeMap::new(), "static/deletion-project.json")` (`project` is never in `path_params`) and `example_accepted("delete_org", params(&[("org", "acme")]), "static/deletion-org.json")`. If M4's `create_deletion` entry used `example_status(…, 202)`, leave it.
- `tests/api_examples.rs`: round-trip `Page<Deletion>` for `org-deletions.json` and `Deletion` for the two new files in `static_files_match_their_types`; in `generated_files_roundtrip`, add `"list_org_deletions" => roundtrip::<Page<Deletion>>(…)` and `"delete_project" | "delete_org" => roundtrip::<Deletion>(…)` (or extend M4's arm).
- Run `just api-examples`.

## Web write mocks

`web/src/mocks/writes.ts`: import the two new files (Biome order: after `api-key-created`, `deletion-org` then `deletion-project`, before `invite-created`), then add

```ts
{ operationId: "delete_project", method: "delete", path: "/api/v1/projects/{project}" },
{ operationId: "delete_org", method: "delete", path: "/api/v1/orgs/{org}" },
```

and

```ts
delete_project: () => HttpResponse.json(deletionProjectDocument, { status: 202 }),
delete_org: () => HttpResponse.json(deletionOrgDocument, { status: 202 }),
```

`writes.test.ts` already covers both paths through its prefixes: its coverage test fails until these exist. Add two tests: `DELETE ${PROJECT}?confirm=support-bot` answers `202` with `kind` `project`; `DELETE /api/v1/orgs/acme?confirm=acme` answers `202` with `kind` `org`.

## Tests

In `routes/deletions.rs` tests, reusing M4's helpers (`Auth`, `post`, `get`, `key_with`, `commit`) and `test_support`'s `add_member` and `personal_token`. Add `async fn send(app: &TestApp, method: Method, path: &str, auth: &Auth, body: Option<serde_json::Value>) -> TestResponse` for any full path (DELETE, other projects, `/api/v1/orgs/…`, `/api/auth/me`), and make M4's `post`/`get` call it (`TestApp::request` already sets the `Origin` header). Tests that commit spans or run the worker use `#[tokio::test(flavor = "multi_thread")]`. "Owner cookie" is the seeded user, who owns `acme`. `app.add_project(&seeded, slug, None)` creates a project whose name equals its slug, with one ingest key; for a key with every scope on it, call `key_with(&app, project.id, vec![Ingest, Read, Write, Feedback])`.

OTLP with a project's key: `app.request(Method::POST, "/v1/traces").header("authorization", &format!("Bearer {key}")).header("content-type", "application/json").body("{}").send().await`. An empty JSON export is accepted (`200`) for an active project. The key cache matters: a key **never used** before its project became `deleting` is refused at lookup with `401`; a key used once (cached) reaches the project check and gets `404`.

1. `project_drop_hides_and_drops`:
   - `app.add_project(&seeded, "doomed", None)` gives a project and its ingest key. Commit one span to it.
   - `DELETE /api/v1/projects/{id}?confirm=doomed` with the owner cookie → `202`, `kind` `project`, `status` `pending`, `slug` `doomed`.
   - Right away, before the worker:
     - `GET /api/v1/projects/{id}` → `404`;
     - `GET /api/v1/orgs/acme/projects` does not list it;
     - an OTLP request with its ingest key → `404`. Send one OTLP export with that key **before** the `DELETE` (expect `200`), so the key is cached; a second key of the same project, minted with `key_with` **before** the `DELETE` and never used before it, gets `401` after the `DELETE`.
   - `run_once`. `GET /api/v1/orgs/acme/deletions` lists the row as `applied`; `project_by_id` is `None`.
2. `project_drop_confirmation`: `confirm=wrong` → `400`, field `confirm`; a missing `confirm` → `400`; the project is still `active`.
3. `project_drop_needs_org_admin`:
   - an org member with a project override `admin` → `403`;
   - an org `admin` → `202`;
   - an `sk` key with every scope → `403`;
   - a read-only PAT of the owner → `403`;
   - a read-write PAT of the owner → `202`. Use a second project each time.
4. `second_drop_is_404`: after one `202`, the same `DELETE` → `404`. The project is no longer `active`, so access refuses it.
5. `org_drop_hides_the_org_and_its_projects`:
   - In one `meta.write`: `insert_org(conn, "Beta", "beta", now)`, `add_org_member(conn, org.id, ada_id, OrgRole::Owner, now)`, `insert_project` twice (`"b1"`, `"b2"`). Commit a span to `b1` with `commit`.
   - `DELETE /api/v1/orgs/beta?confirm=beta` → `202`, `kind` `org`.
   - Right away:
     - `GET /api/v1/orgs/beta` → `404`;
     - `GET /api/v1/orgs` lists `acme` only;
     - `GET /api/auth/me` lists `acme` only;
     - each project of `beta` → `404`.
   - `run_once` → the org row is gone, and the deletion is `applied`. Reading it needs the store: the org is gone, so its route is `404`. Use `deletion_by_id`.
   - `deletion_projects` holds both project ids.
   - Then: a second org `gamma` with one project `g1`; `DELETE` project `g1` (row `pending`, not run), then `DELETE /api/v1/orgs/gamma?confirm=gamma` → `202`; `run_once` → both rows `applied`.
6. `org_drop_is_owner_only`: an org `admin` → `403`; an `sk` key → `404`; `confirm=acme-typo` → `400`.
7. `open_data_deletions_of_a_dropped_project_are_superseded`:
   - A `user` deletion created through M4's POST on project P, still `resolving` (do not run the worker).
   - Then `DELETE` P → `202`. Then `run_once`.
   - Both rows are `applied`: the user row by the superseded rule, without a storage call (its `trace_count` stays `None` and `storage.deletion_status(user_row_id)` is `Err(DeletionError::NotFound)`); the project row by the drop.
8. `org_history_pages`: two project drops in one org. `GET /api/v1/orgs/acme/deletions?limit=1` → one row and a cursor; the next page → the other. An org `member` → `403`; an org `admin` → `200`.
9. `drop_audit_carries_no_slug`: create project `secret-slug-1`; `let (response, logs) = capture_logs(DELETE /api/v1/projects/{project id}?confirm=secret-slug-1).await` (the path uses the id, not the slug); `202`, `logs.contains("deletion requested")` and `!logs.contains("secret-slug-1")`.

## Done when

- [ ] The three routes behave as above; `orgs_of_user` hides orgs being deleted.
- [ ] `openapi.json`, `schema.gen.ts`, the examples and the manifest are regenerated.
- [ ] The write mocks cover `delete_project` and `delete_org`.
- [ ] The 9 route tests and the `meta`/`roles.rs` assertions pass; `just check` is green; one commit `Add project and org deletion routes`.
