# H1 — Read credentials on query POSTs

**Commit subject:** `server: admit read credentials on query POSTs`

`POST /api/v1/projects/{project}/sql` (H4) is a read that uses POST. Today the access layer refuses every non-GET request from a read-only PAT and from an `sk` key whose scope is `read`. H1 adds a project permission, `ProjectPerm::Query`, that counts as a read: a read-only PAT and a `read` key may call a POST route that declares it (D-H-4). No route uses it yet; H4 adds the first.

## Read first

- README § Design → Access, § Contracts between sub-phases.
- `crates/server/src/auth/roles.rs`: `ProjectPerm`, `min_role`, `key_scope`, the test `permissions_follow_the_spec_table`.
- `crates/server/src/http/access.rs`: `Access`, `prepare` (the read-PAT check right after the principal is loaded), `grant_key_project` (the `KeyScope::Read` method check), and the test module (helpers `route`, `echo_project`, `personal_token`, `assert_outcome`).
- `crates/server/src/http/access_suite.rs`: `expected` and `key_outcome`. This suite runs every documented route against every caller and predicts the `x-test-access` header, so it must learn the new rule now, before H4 adds a `Query` route.

## Files

| File | Change |
|---|---|
| `crates/server/src/auth/roles.rs` | add `ProjectPerm::Query` |
| `crates/server/src/http/access.rs` | `Access::reads_by_post`, the two method checks, one new test |
| `crates/server/src/http/access_suite.rs` | `expected` and `key_outcome` follow the new rule |

Nothing else. No OpenAPI change (no route uses `Query` yet).

## Steps

### 1. `ProjectPerm::Query` (`auth/roles.rs`)

Add the variant last in the enum, with this doc comment:

```rust
    /// Read project data through a POST with a body, such as the SQL console (D-H-4).
    /// Same holders as `View`; a read-only PAT and a `read` key may use it on POST.
    Query,
```

In `ProjectPerm::min_role`, add `Self::Query` to the `Viewer` arm: `Self::View | Self::Query => Role::Viewer,`.

In `ProjectPerm::key_scope`, add it to the `Read` arm: `Self::View | Self::Query => Some(KeyScope::Read),`.

In the test `permissions_follow_the_spec_table`, add after the `ProjectPerm::View` assertions:

```rust
        assert_eq!(ProjectPerm::Query.min_role(), Role::Viewer);
        assert_eq!(ProjectPerm::Query.key_scope(), Some(KeyScope::Read));
```

### 2. `Access::reads_by_post` (`http/access.rs`)

Add this `impl` right after the `enum Access` definition:

```rust
impl Access {
    /// Whether this request is a read made with POST: a read-only PAT and a `read` key may
    /// call it (D-H-4). Only POST counts; PUT, PATCH and DELETE on the same route stay writes.
    pub(crate) fn reads_by_post(self, method: &axum::http::Method) -> bool {
        *method == axum::http::Method::POST
            && matches!(
                self,
                Self::Project {
                    perm: ProjectPerm::Query,
                    ..
                }
            )
    }
}
```

Use `matches!` (a `match` that returns `true`/`false` trips clippy's `match_like_matches_macro`). H6 adds `| Self::Pat` inside this `matches!`.

### 3. The read-PAT check in `prepare`

Today:

```rust
    if let Principal::Pat { write: false, .. } = &principal
        && !matches!(
            parts.method,
            axum::http::Method::GET | axum::http::Method::HEAD
        )
    {
        return Err(forbidden());
    }
```

Add one condition, `&& !access.reads_by_post(&parts.method)`, after the method check, and extend the comment above it: `A read PAT cannot call a write method. That is the token's scope, so it is 403 even when the resource would be 404. A POST that reads (Access::reads_by_post) is not a write.`

### 4. The read-key check in `grant_key_project`

Today:

```rust
    if scope == KeyScope::Read
        && !matches!(
            parts.method,
            axum::http::Method::GET | axum::http::Method::HEAD
        )
    {
        return Err(forbidden());
    }
```

Before the `if`, bind `let reads_by_post = perm == ProjectPerm::Query && parts.method == axum::http::Method::POST;` and add `&& !reads_by_post` as the last condition (a named boolean keeps clippy's `nonminimal_bool` quiet). `perm` is already a parameter of `grant_key_project`. Do not change anything else in that function: a key still needs the `read` scope (`key_scope()` of `Query`), and a key on another project is still 404 (that check runs earlier, in `grant_project`).

### 5. The authorization suite (`http/access_suite.rs`)

In `expected`, the line

```rust
    if who == Who::PatRead && !safe(method) {
```

becomes

```rust
    if who == Who::PatRead && !safe(method) && !access.reads_by_post(method) {
```

In `key_outcome`, the condition

```rust
    if scope == KeyScope::Read && !safe(method) {
```

becomes

```rust
    let reads_by_post = perm == ProjectPerm::Query && *method == Method::POST;
    if scope == KeyScope::Read && !safe(method) && !reads_by_post {
```

The suite walks the server's real route table, and no real route uses `Query` until H4 adds `POST …/sql`, so these branches start being exercised in H4. In the suite, `PatRead` and `PatWrite` belong to the member whose override on project A1 is `none`, so on `POST …/sql` they will get `denied-404` from the project lookup, which `expected` already predicts through `project_role`; `KeyRead` will be `allowed`. The direct tests below cover every case now.

### 6. New tests in `http/access.rs` (test module)

Add `#[tokio::test] async fn a_query_route_admits_read_credentials_on_post()` at the end of the test module. It uses the existing helpers `route`, `echo_project`, `personal_token`, `cookie`, `user` and `assert_outcome`, and adds imports `axum::routing::{delete, patch, put}`, `crate::meta::api_keys::{NewApiKey, insert_api_key}`.

Route table:

```rust
        let app = TestApp::with_route_table(vec![
            route(Method::POST, "/t/projects/{project}/query",
                Access::Project { perm: ProjectPerm::Query, session_only: false }, post(echo_project)),
            route(Method::PATCH, "/t/projects/{project}/query",
                Access::Project { perm: ProjectPerm::Query, session_only: false }, patch(echo_project)),
            route(Method::PUT, "/t/projects/{project}/query",
                Access::Project { perm: ProjectPerm::Query, session_only: false }, put(echo_project)),
            route(Method::DELETE, "/t/projects/{project}/query",
                Access::Project { perm: ProjectPerm::Query, session_only: false }, delete(echo_project)),
            route(Method::POST, "/t/projects/{project}/view",
                Access::Project { perm: ProjectPerm::View, session_only: false }, post(echo_project)),
        ])
        .await;
```

(Two rows on one path with different methods are fine: `build_router` wraps each row separately.)

Setup:
- `let seeded = app.seed().await;` Ada (`ada@example.com`) is the owner of org `acme`, so her effective project role is `admin` (owners and admins are project admins, spec 08). That is why the echo bodies below say `"role": "admin"`.
- `let read_pat = personal_token(&app, "ada@example.com", vec![KeyScope::Read]).await;`
- `let write_pat = personal_token(&app, "ada@example.com", vec![KeyScope::Read, KeyScope::Write]).await;`
- `let (_other, other_key) = app.add_project(&seeded, "other-bot", None).await;` — an `ingest` key of another project in the same org.
- A write-only key on `support-bot`: `let minted = secret::new_key(KeyKind::Secret).unwrap();` keep `minted.plaintext`, then in one `app.state.meta.write` call `insert_api_key(conn, NewApiKey { project_id: seeded.project.id, kind: "secret", scopes: vec![KeyScope::Write], name: None, prefix: minted.prefix, hash: minted.hash, created_by: <Ada's id>, expires_at: None }, now)`. Ada's id: `users::user_by_email(app.state.meta.reader(), "ada@example.com").await.unwrap().unwrap().id`; `now = app.state.clock.now_ns()`.
- A viewer: in one `app.state.meta.write`, `insert_user(conn, user("viewer@example.com", false), now)` and `add_org_member(conn, seeded.org_id, viewer.id, OrgRole::Viewer, now)`; then `let viewer_cookie = cookie(&app, viewer_id).await;` (the existing helper returns `HeaderValueCookie`; use `.0`).

Paths: `query = format!("/t/projects/{}/query", seeded.project.id)`, `view = …/view`, `missing = "/t/projects/01900000-0000-7000-8000-000000009999/query"`, `current = "/t/projects/current/query"`.

Send every request with `app.request(method, path).json(&json!({}))` plus the credential: `header("cookie", …)` for sessions, `header("authorization", &format!("Bearer {secret}"))` for PATs and keys. Check each with `assert_outcome(&response, status, marker, case)`:

| Case label | Method | Path | Credential | Status | Marker | Body check |
|---|---|---|---|---|---|---|
| `"anonymous query"` | POST | `query` | none | 401 | `denied-401` | — |
| `"session query"` | POST | `query` | `seeded.cookie` | 200 | `allowed` | `{"org": null, "project": "support-bot", "role": "admin"}` |
| `"viewer session query"` | POST | `query` | viewer cookie | 200 | `allowed` | `{"org": null, "project": "support-bot", "role": "viewer"}` |
| `"read pat query"` | POST | `query` | `read_pat` | 200 | `allowed` | as `"session query"` |
| `"write pat query"` | POST | `query` | `write_pat` | 200 | `allowed` | as `"session query"` |
| `"read pat patch"` | PATCH | `query` | `read_pat` | 403 | `denied-403` | — |
| `"read pat put"` | PUT | `query` | `read_pat` | 403 | `denied-403` | — |
| `"read pat delete"` | DELETE | `query` | `read_pat` | 403 | `denied-403` | — |
| `"write pat patch"` | PATCH | `query` | `write_pat` | 200 | `allowed` | as `"session query"` |
| `"read pat view"` | POST | `view` | `read_pat` | 403 | `denied-403` | — |
| `"read pat missing project"` | POST | `missing` | `read_pat` | 404 | `denied-404` | — |
| `"session current"` | POST | `current` | `seeded.cookie` | 404 | `denied-404` | — |
| `"pat current"` | POST | `current` | `read_pat` | 404 | `denied-404` | — |
| `"read key query"` | POST | `query` | `seeded.read_key` | 200 | `allowed` | `{"org": null, "project": "support-bot", "role": null}` |
| `"read key current"` | POST | `current` | `seeded.read_key` | 200 | `allowed` | as `"read key query"` |
| `"read key patch"` | PATCH | `query` | `seeded.read_key` | 403 | `denied-403` | — |
| `"read key put"` | PUT | `query` | `seeded.read_key` | 403 | `denied-403` | — |
| `"read key delete"` | DELETE | `query` | `seeded.read_key` | 403 | `denied-403` | — |
| `"read key view"` | POST | `view` | `seeded.read_key` | 403 | `denied-403` | — |
| `"ingest key query"` | POST | `query` | `seeded.ingest_key` | 403 | `denied-403` | — |
| `"write-only key query"` | POST | `query` | the write-only key | 403 | `denied-403` | — |
| `"other project key"` | POST | `query` | `other_key` | 404 | `denied-404` | — |

Compare bodies with `assert_eq!(response.json::<Value>(), json!({...}), "{case}")`.

Also add a `#[test] fn reads_by_post_needs_post_and_query()`: `Access::Project { perm: ProjectPerm::Query, session_only: false }.reads_by_post(&Method::POST)` is true; with `&Method::PATCH`, `&Method::DELETE`, `&Method::PUT` it is false; `Access::Project { perm: ProjectPerm::View, … }.reads_by_post(&Method::POST)` is false; `Access::User { session_only: false }.reads_by_post(&Method::POST)` is false.

## Commands

```bash
export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH"
cargo test -p llmobs-server access
cargo clippy -p llmobs-server --all-targets -- -D warnings
cargo test -p llmobs-server roles
just check
```

The first two run `a_query_route_admits_read_credentials_on_post`, `every_route_against_every_principal` and `permissions_follow_the_spec_table`.

## Done when

- `ProjectPerm::Query` exists with `min_role` viewer and `key_scope` `Read`.
- A read-only PAT and a `read` key pass the access layer on a POST route declared with `ProjectPerm::Query`, and are still refused (403) on PUT, PATCH or DELETE of that route and on a POST route declared with any other permission.
- `every_route_against_every_principal` passes (no route uses `Query` yet, so its new `Query` branches stay unexercised until H4 adds `POST …/sql`; the direct test above is what verifies the rule in H1).
- `just check` passes. One commit, subject `server: admit read credentials on query POSTs`.

Report "no new dependency".
