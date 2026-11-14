# H2 — Saved views in `meta.db`

**Commit subject:** `server: store saved views in meta.db`

Adds the `saved_views` table (spec 04) and its store module, `crate::meta::saved_views`. No route yet: H3 adds the four HTTP routes on top of this module, H7 and H8 read it from the MCP tools. The store knows the visibility rule (D-H-6) and the default-clearing rule; who may change what is the route's job (H3).

## Read first

- README § Design → Saved views, § Contracts between sub-phases (the signatures below repeat them; they must match exactly).
- `docs/spec/04-metadata-db.md` § Saved views (the `CREATE TABLE`).
- `crates/server/src/meta/mod.rs`: `MetaDb::write`, `MetaError`, `unique_or`, the tests `schema_matches_the_snapshot`, `open_twice_is_idempotent`, `open_existing_checks_the_file_and_the_version`.
- `crates/server/src/meta/personal_tokens.rs`: the house style for a store (a `*Record` `sqlx::FromRow` struct, a `row_from` decoder, `MetaError::Corrupt` for bad stored values).
- `crates/server/src/api/saved_views.rs`: `SavedView`, `SavedViewKind`.

## Files

| File | Change |
|---|---|
| `crates/server/migrations/meta/0003_saved_views.sql` | new |
| `crates/server/src/meta/saved_views.rs` | new |
| `crates/server/src/meta/mod.rs` | `pub(crate) mod saved_views;`; two test numbers |
| `crates/server/src/admin.rs` | one test's migration numbers |
| `crates/server/tests/snapshots/meta_schema.snap` | regenerated |

## 1. Migration `0003_saved_views.sql`

Exactly:

```sql
CREATE TABLE saved_views (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  state TEXT NOT NULL,
  owner_user_id TEXT REFERENCES users(id) ON DELETE SET NULL,
  shared INTEGER NOT NULL DEFAULT 0,
  is_default INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX saved_views_lookup ON saved_views(project_id, kind);
CREATE INDEX saved_views_owner ON saved_views(project_id, owner_user_id);
```

`kind` is one of `traces`, `spans`, `sessions`, `users`, `sql`. `state` is the JSON text of an object. `shared` and `is_default` are `0` or `1`. Times are nanoseconds.

## 2. `meta/mod.rs`

- Add `pub(crate) mod saved_views;` in the module list, alphabetically (after `projects`).
- In the test `open_twice_is_idempotent`: `assert_eq!(count, 2);` becomes `assert_eq!(count, 3);`.
- In the test `open_existing_checks_the_file_and_the_version`: the inserted future migration `VALUES (3, 'future', …)` becomes `VALUES (4, 'future', …)`, `assert_eq!(found, 3)` becomes `assert_eq!(found, 4)`, and `assert_eq!(expected, 2)` becomes `assert_eq!(expected, 3)`.
- In `crates/server/src/admin.rs`, the test `schema_mismatch_names_the_versions` inserts a future migration `VALUES (3, 'future', …)` and expects `"this binary expects schema 2, found 3"`: change them to `VALUES (4, 'future', …)` and `"this binary expects schema 3, found 4"`. Then search for other hard-coded versions with `grep -rn "'future'\|expects schema\|count, 2)" crates/` and fix any other hit the same way.
- Regenerate the schema snapshot: `INSTA_UPDATE=always cargo test -p llmobs-server schema_matches_the_snapshot`, then check with `git diff crates/server/tests/snapshots/meta_schema.snap` that the only change is four added blocks. The snapshot lists `sqlite_master` rows `ORDER BY name`, so they land at these places: `table saved_views`, then `index saved_views_lookup`, then `index saved_views_owner` (together, after the `projects`/`project_members` blocks and before the `sqlite_autoindex_*` blocks), and `index sqlite_autoindex_saved_views_1` (the primary key's index, no SQL line) among the other `sqlite_autoindex_*` blocks, after `sqlite_autoindex_projects_*`. Any other difference: STOP.

## 3. `meta/saved_views.rs`

Module doc: `//! `saved_views` rows (spec 04): saved list views and SQL queries (D-H-6).`

Do not add a `#![cfg_attr(not(test), allow(dead_code))]` line: `meta/mod.rs` already carries that inner attribute, and it covers its submodules until H3 calls this module.

Imports you need: `serde_json::{Map, Value}`, `sqlx::{SqliteConnection, SqlitePool}`, `uuid::Uuid`, `llmobs_core::ids::ProjectId`, `super::MetaError`, `crate::api::saved_views::{SavedView, SavedViewKind}`.

### Constants

```rust
/// Most views one user may own in one project (D-H-6).
pub(crate) const MAX_VIEWS_PER_OWNER: i64 = 1000;

/// Unsharing a shared view whose owner was deleted would hide it from everyone (D-H-6).
pub(crate) const UNSHARE_ORPHAN: &str = "the view has no owner; delete it instead";
```

### Public types (exactly these)

```rust
/// Who is reading. A user sees their own views and every shared view; a key sees shared views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Viewer {
    /// A session or PAT user, by `users.id`.
    User(Uuid),
    /// A project API key.
    Key,
}

/// One stored view.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SavedViewRow {
    pub(crate) id: Uuid,
    pub(crate) kind: SavedViewKind,
    pub(crate) name: String,
    pub(crate) state: Map<String, Value>,
    pub(crate) owner_user_id: Option<Uuid>,
    pub(crate) shared: bool,
    pub(crate) is_default: bool,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

/// One page of [`list_visible`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SavedViewPage {
    pub(crate) rows: Vec<SavedViewRow>,
    /// The last row's id when more rows follow, else `None`.
    pub(crate) next_cursor: Option<String>,
}

/// Columns of a new view. `name` is already trimmed and checked; `state` is already checked.
#[derive(Debug, Clone)]
pub(crate) struct NewSavedView {
    pub(crate) project_id: ProjectId,
    pub(crate) kind: SavedViewKind,
    pub(crate) name: String,
    pub(crate) state: Map<String, Value>,
    pub(crate) owner_user_id: Uuid,
    pub(crate) shared: bool,
    pub(crate) is_default: bool,
}

/// What [`update_view`] changes. `None` leaves the column alone.
#[derive(Debug, Clone, Default)]
pub(crate) struct SavedViewChanges {
    pub(crate) name: Option<String>,
    pub(crate) state: Option<Map<String, Value>>,
    pub(crate) shared: Option<bool>,
    pub(crate) is_default: Option<bool>,
}
```

Give every field a one-line `///` doc comment, as `personal_tokens.rs` does. The workspace lint `missing_debug_implementations` is on, so every struct and enum in this file derives `Debug` (the `SavedViewRecord` below too).

### Helpers

```rust
/// The `kind` column text.
pub(crate) fn kind_str(kind: SavedViewKind) -> &'static str
// Traces → "traces", Spans → "spans", Sessions → "sessions", Users → "users", Sql → "sql".

fn parse_kind(text: &str) -> Option<SavedViewKind>   // the inverse; anything else is None

/// Whether `viewer` may see `row`: a user sees rows they own and shared rows; a key sees
/// shared rows. A private row with no owner is visible to nobody.
pub(crate) fn visible_to(row: &SavedViewRow, viewer: Viewer) -> bool
// row.shared || matches!(viewer, Viewer::User(id) if row.owner_user_id == Some(id))
```

A `#[derive(Debug, sqlx::FromRow)] struct SavedViewRecord` with the nine selected columns (every column but `project_id`) as `String` (`id`, `kind`, `name`, `state`), `Option<String>` (`owner_user_id`), `i64` (`shared`, `is_default`, `created_at`, `updated_at`), and

```rust
fn row_from(record: SavedViewRecord) -> Result<SavedViewRow, MetaError>
```

which decodes:
- `id` with `Uuid::parse_str`, else `MetaError::Corrupt { table: "saved_views", column: "id" }`;
- `kind` with `parse_kind`, else `Corrupt { …, column: "kind" }`;
- `state` with `serde_json::from_str::<Map<String, Value>>`, else `Corrupt { …, column: "state" }`;
- `owner_user_id` with `Uuid::parse_str` when present, else `Corrupt { …, column: "owner_user_id" }`;
- `shared` and `is_default`: `!= 0`.

Keep one `const COLUMNS: &str = "id, kind, name, state, owner_user_id, shared, is_default, created_at, updated_at";` and use it in every `SELECT`.

The visibility condition in SQL, used by the three read functions, is:
- `Viewer::User(id)`: `AND (shared = 1 OR owner_user_id = ?)` with `id.to_string()` bound;
- `Viewer::Key`: `AND shared = 1`.

Build the query text with `format!` from fixed pieces only (never from caller text) and bind every value. Bind ids as text: `project.to_string()` (`ProjectId` displays as its UUID) and `id.to_string()`.

### Reads (on the read pool)

```rust
/// Views of `project` that `viewer` may see, optionally of one `kind`, ordered by `id`
/// ascending (creation order). `cursor` is the last id of the previous page: rows with a
/// greater `id` follow. `limit` is 1–1000; the route checks it.
pub(crate) async fn list_visible(
    pool: &SqlitePool,
    project: ProjectId,
    viewer: Viewer,
    kind: Option<SavedViewKind>,
    limit: usize,
    cursor: Option<&str>,
) -> Result<SavedViewPage, MetaError>
```

SQL: `SELECT {COLUMNS} FROM saved_views WHERE project_id = ? [AND kind = ?] [visibility] [AND id > ?] ORDER BY id LIMIT ?`. Bind in that order, skipping the absent parts: `project.to_string()`, `kind_str(kind)`, the viewer's user id (only for `Viewer::User`), the cursor, and finally `limit + 1` as an `i64`. Convert with `limit.checked_add(1).and_then(|n| i64::try_from(n).ok())`; on `None` return `Err(MetaError::Conflict("page size is too large"))` (the route never passes more than 1,000, so this is unreachable in practice; no `unwrap`). When `limit + 1` rows come back, drop the last and set `next_cursor` to the id of the last row kept; otherwise `None`.

Ordering note: ids are `Uuid::now_v7()` written with `to_string()`, i.e. lowercase hyphenated hex, so comparing them as text (`ORDER BY id`, `id > ?`) follows their timestamps. Within one process `now_v7` is also monotonic for ids made in the same millisecond, so `ORDER BY id` is creation order here. The route validates that a cursor parses as a UUID before calling.

```rust
/// The view `id` of `project` when `viewer` may see it.
pub(crate) async fn get_visible(
    pool: &SqlitePool,
    project: ProjectId,
    viewer: Viewer,
    id: Uuid,
) -> Result<Option<SavedViewRow>, MetaError>

/// Every visible view of `kind` named exactly `name`, the viewer's own first, then by id.
pub(crate) async fn find_visible_by_name(
    pool: &SqlitePool,
    project: ProjectId,
    viewer: Viewer,
    kind: SavedViewKind,
    name: &str,
) -> Result<Vec<SavedViewRow>, MetaError>
```

Names are not unique (D-H-6): several visible views may share a name, which is why this returns a list. `find_visible_by_name` compares `name = ?` exactly (SQLite's `=` on text is case-sensitive) and orders in Rust after the query: rows whose `owner_user_id` equals the viewer's user id first, then the rest; within each group by `id` ascending. For `Viewer::Key` it is simply `id` ascending.

### Reads and writes inside a write transaction

```rust
/// The view `id` of `project`, whoever owns it. Callers check visibility and permissions.
pub(crate) async fn view_by_id(
    conn: &mut SqliteConnection,
    project: ProjectId,
    id: Uuid,
) -> Result<Option<SavedViewRow>, MetaError>

/// Inserts a view. When the owner already owns [`MAX_VIEWS_PER_OWNER`] views in the project,
/// returns `MetaError::Conflict("too many saved views")`. When `is_default` is set, clears
/// the other defaults of the same scope first (see [`clear_other_defaults`]).
pub(crate) async fn insert_view(
    conn: &mut SqliteConnection,
    view: NewSavedView,
    now: i64,
) -> Result<SavedViewRow, MetaError>

/// Applies `changes` to the view `id` of `project` and returns the row after the change, or
/// `None` when there is no such view. `updated_at` becomes `now` when anything changed.
/// See the steps below for unsharing and defaults. Unsharing a shared view that has no owner
/// is `MetaError::Conflict(UNSHARE_ORPHAN)`.
pub(crate) async fn update_view(
    conn: &mut SqliteConnection,
    project: ProjectId,
    id: Uuid,
    changes: SavedViewChanges,
    now: i64,
) -> Result<Option<SavedViewRow>, MetaError>

/// Deletes the view `id` of `project`. Returns whether a row was deleted.
pub(crate) async fn delete_view(
    conn: &mut SqliteConnection,
    project: ProjectId,
    id: Uuid,
) -> Result<bool, MetaError>
```

Private helper:

```rust
/// Clears `is_default` on every other view of the same scope as `row`: the shared views of
/// `row.kind` when `row.shared`, else the owner's private views of `row.kind`.
async fn clear_other_defaults(conn: &mut SqliteConnection, project: ProjectId, row: &SavedViewRow)
    -> Result<(), MetaError>
```

SQL for the two scopes:
- shared: `UPDATE saved_views SET is_default = 0 WHERE project_id = ? AND kind = ? AND shared = 1 AND id <> ?`
- private: `UPDATE saved_views SET is_default = 0 WHERE project_id = ? AND kind = ? AND shared = 0 AND owner_user_id = ? AND id <> ?` (a private row always has an owner when created; if `row.owner_user_id` is `None`, skip the update).

These updates do not touch `updated_at` of the other rows.

`insert_view` steps:
1. `SELECT COUNT(*) FROM saved_views WHERE project_id = ? AND owner_user_id = ?`; when `>= MAX_VIEWS_PER_OWNER`, return `Err(MetaError::Conflict("too many saved views"))`.
2. Build the row with `id = Uuid::now_v7()`, `created_at = updated_at = now`, `owner_user_id = Some(view.owner_user_id)`.
3. `INSERT INTO saved_views (id, project_id, kind, name, state, owner_user_id, shared, is_default, created_at, updated_at) VALUES (?, …)`. `state` is `serde_json::to_string(&view.state)`; a serialization failure is impossible for a `Map` but map it to `MetaError::Corrupt { table: "saved_views", column: "state" }` rather than unwrapping.
4. When `row.is_default`, call `clear_other_defaults`.

`update_view` steps (move each `Some` value out of `changes` into the row; do not clone, clippy's `needless_pass_by_value` is on):
1. `view_by_id`; `None` → return `Ok(None)`. Keep a copy of the stored row as `before` (one `clone()` of the row is needed for the comparison in step 4).
2. If `before.shared && before.owner_user_id.is_none() && changes.shared == Some(false)` → return `Err(MetaError::Conflict(UNSHARE_ORPHAN))`.
3. Apply each `Some` field (`name`, `state`, `shared`, `is_default`) to the row. Then, only when `before.shared` is true, the row is now private (`!row.shared`) and `changes.is_default` is `None`, set `row.is_default = false` (unsharing drops the project default). When `is_default` is given in the same change, it applies as given, in the resulting scope. `shared: Some(false)` on a view that was already private changes nothing.
4. If `row == before` (nothing changed), return `Ok(Some(row))` without writing; `updated_at` stays.
5. Otherwise set `updated_at = now` and `UPDATE saved_views SET name = ?, state = ?, shared = ?, is_default = ?, updated_at = ? WHERE project_id = ? AND id = ?`.
6. When the row is a default after the change, call `clear_other_defaults` (in its resulting scope).
7. Return `Ok(Some(row))`.

`insert_view` also moves the fields of `view` into the row (no clones). Duplicate names are allowed, and so are any keys inside `state`.

### `From<SavedViewRow> for SavedView`

```rust
impl From<SavedViewRow> for SavedView {
    fn from(row: SavedViewRow) -> Self {
        Self {
            id: row.id,
            kind: row.kind,
            name: row.name,
            state: row.state,
            owner_user_id: row.owner_user_id,
            shared: row.shared,
            is_default: row.is_default,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}
```

Put it in `meta/saved_views.rs` (the API type is in this crate, so the orphan rule allows it).

## 4. Tests (in `meta/saved_views.rs`, `#[cfg(test)] mod tests`)

Each test starts with `let (_dir, db) = crate::meta::test_db().await;` — keep `_dir` bound for the whole test: dropping the `TempDir` deletes the database directory. Use these helpers, written in the test module:

```rust
const NOW: i64 = 1_767_225_600_000_000_000;

/// One org, two projects (`support-bot`, `other-bot`), and three users: `owner`, `other`, `third`.
struct World { project: ProjectId, second: ProjectId, owner: Uuid, other: Uuid, third: Uuid }
async fn world(db: &MetaDb) -> World
```

`world` runs one `db.write`: `crate::meta::users::insert_user(conn, NewUser { email: "owner@example.com" | "other@example.com" | "third@example.com", name: None, password_hash: None, email_verified_at: None, is_instance_admin: false }, NOW)` (the third argument is the creation time), `crate::meta::orgs::insert_org(conn, "Acme", "acme", NOW)`, and `crate::meta::projects::insert_project(conn, org.id, "Support", "support-bot", None, NOW)` and the same with `"Other"`, `"other-bot"`.

```rust
/// A view with state `{"filter": "status = 'error'"}`.
fn new_view(project: ProjectId, owner: Uuid, kind: SavedViewKind, name: &str, shared: bool, is_default: bool) -> NewSavedView
/// `insert_view` in its own `db.write`.
async fn insert(db: &MetaDb, view: NewSavedView, now: i64) -> SavedViewRow
/// `update_view` in its own `db.write`.
async fn update(db: &MetaDb, project: ProjectId, id: Uuid, changes: SavedViewChanges, now: i64) -> Result<Option<SavedViewRow>, MetaError>
/// The stored row, whoever may see it (`view_by_id` in a `db.write`).
async fn stored(db: &MetaDb, project: ProjectId, id: Uuid) -> Option<SavedViewRow>
```

Ids are generated, so compare by the ids you captured. `MetaError` has no `PartialEq`: compare errors with `matches!`.

Rows and visibility:
1. `insert_then_get_round_trips` — a private `traces` view of `owner`; `get_visible(…, Viewer::User(owner), id)` returns a row equal to the inserted one (`name`, `kind`, `state`, `owner_user_id == Some(owner)`, `shared == false`, `is_default == false`, `created_at == updated_at == NOW`).
2. `visibility_follows_owner_and_shared` — A = private by `owner`, B = shared by `owner`. `list_visible(kind: None)` for `Viewer::User(owner)` has A and B; for `Viewer::User(other)` only B; for `Viewer::Key` only B. `get_visible(A)` is `None` for `other` and for `Viewer::Key`.
3. `list_without_kind_returns_every_kind` — one view of each of the five kinds for `owner`; `list_visible(kind: None, limit: 50)` returns 5 rows.
4. `kind_filter_and_pages` — three `sql` views and one `traces` view for `owner`. `list_visible(kind: Some(Sql), limit: 2)` returns 2 rows and `next_cursor == Some(<second row's id as string>)`; with that cursor, 1 row and `next_cursor == None`. The three ids equal the three `sql` ids; within a page they ascend.
5. `a_full_last_page_has_no_cursor` — two views; `list_visible(limit: 2)` returns both and `next_cursor == None`.
6. `projects_are_isolated` — `owner` inserts V in `project` and W in `second`. In `second`: `list_visible` has only W; `get_visible(…, second, …, V.id)` is `None`; `view_by_id(second, V.id)` is `None`; `update_view(second, V.id, name: Some("x"))` is `Ok(None)` and V is unchanged; `delete_view(second, V.id)` is `Ok(false)` and V still exists. Defaults: shared default S1 in `project` and shared default S2 in `second` (same kind) both stay default.
7. `deleting_the_project_deletes_its_views` — insert two views in `second`, then `DELETE FROM projects WHERE id = ?` (`second`) in a `db.write`; `SELECT COUNT(*) FROM saved_views WHERE project_id = ?` is 0, and the views of `project` remain.
8. `deleting_the_owner_hides_private_views` — private P and shared S of `third`; `DELETE FROM users WHERE id = ?` (`third`). `list_visible` for `owner` has S with `owner_user_id == None` and not P; `stored(P)` still exists with `owner_user_id == None`.

Defaults:
9. `private_default_clears_only_the_owners_other_default` — `owner` inserts private P1 (default), then P2 (default); `other` inserts private Q (default), all `traces`. After: P1 `false`, P2 `true`, Q `true`.
10. `shared_default_clears_other_shared_defaults` — `owner` inserts shared S1 (default), `other` shared S2 (default), `owner` private P (default), all `traces`. After: S1 `false`, S2 `true`, P `true`. Then `update(S1, is_default: Some(true))`: S1 `true`, S2 `false`, P `true`.
11. `defaults_are_per_kind` — `owner` private default T (`traces`) and private default Q (`sql`): both stay `true`.
12. `unsharing_clears_the_default` — shared default S of `owner`; `update(S, shared: Some(false), now: NOW + 1)` → `shared == false`, `is_default == false`, `updated_at == NOW + 1`.
13. `unsharing_with_a_default_keeps_it_in_the_private_scope` — `owner` has private default P and shared default S (`traces`). `update(S, shared: Some(false), is_default: Some(true))` → S private and default; P is no longer default.
14. `unsharing_a_private_view_changes_nothing` — private default P; `update(P, shared: Some(false), now: NOW + 3)` → `is_default == true`, `updated_at == NOW` (no write).
15. `sharing_a_private_default_takes_over_the_project_default` — shared default S1 of `other`; private default P of `owner`; `update(P, shared: Some(true))` → P shared and default; S1 not default.
16. `clearing_a_default_touches_nothing_else` — private default P1 and non-default P2 of `owner`; `update(P1, is_default: Some(false))` → P1 `false`, P2 `false`, and P2's `updated_at == NOW`.
17. `an_orphan_private_default_is_left_alone` — insert private default P of `third`, delete `third` (P's owner is now `NULL`), then `owner` inserts private default R: P keeps `is_default == true` (no scope matches a missing owner; `clear_other_defaults` skips a row without an owner and the private-scope SQL never matches `NULL`).

Updates:
18. `name_and_state_changes_bump_updated_at` — `update(name: Some("Renamed"), now: NOW + 1)` → name changed, `updated_at == NOW + 1`; `update(state: Some({"filter": "x"}), now: NOW + 2)` → state changed, `updated_at == NOW + 2`.
19. `same_values_write_nothing` — `update(SavedViewChanges::default(), NOW + 5)` and `update(name: Some(<same name>), shared: Some(<same>), NOW + 5)` both return `updated_at == NOW`.
20. `unsharing_an_orphan_shared_view_is_a_conflict` — shared S of `third`; delete `third`; `update(S, shared: Some(false))` → `matches!(…, Err(MetaError::Conflict(UNSHARE_ORPHAN)))`; S is still shared. `delete_view(S)` → `Ok(true)`.
21. `update_and_delete_of_a_missing_view` — `update` of `Uuid::now_v7()` → `Ok(None)`; `delete_view` → `Ok(false)`. `delete_view` of an existing view → `Ok(true)`, then `get_visible` → `None`.

Names and limits:
22. `find_by_name_puts_the_viewers_own_first` — shared `"Errors"` by `other` (inserted first), private `"Errors"` by `owner`, shared `"errors"` by `owner`, all `traces`. For `Viewer::User(owner)`: `[owner's private "Errors", other's shared "Errors"]`. For `Viewer::User(third)`: `[other's shared]`. For `Viewer::Key`: `[other's shared]`.
23. `owner_cap_is_a_conflict` — in one `db.write`, insert `MAX_VIEWS_PER_OWNER` views for `owner`: half `traces` private, half `sql` shared (the cap counts every kind and both visibilities). One more → `matches!(…, Err(MetaError::Conflict("too many saved views")))`. `other` can still insert, and so can `owner` in `second`.

Corruption:
24. `corrupt_columns_are_reported` — for each of `UPDATE saved_views SET kind = 'bogus'`, `SET state = 'not json'`, `SET state = '[]'`, `SET id = 'not-a-uuid'`, and an owner id that is not a UUID. `PRAGMA foreign_keys` cannot be switched inside `db.write`'s transaction, and the `users` foreign key is enforced on every write (the default `NO ACTION`), so for the owner case satisfy the foreign key instead: in one `db.write`, `insert_user` a fresh user `bad@example.com` (no memberships, so nothing references it), `UPDATE users SET id = 'not-a-uuid' WHERE id = ?` (that user), then `UPDATE saved_views SET owner_user_id = 'not-a-uuid' WHERE id = ?`. In every case: first `UPDATE saved_views SET shared = 1` on that view (before corrupting `id`, since that update finds the row by id), then corrupt it; reading through `list_visible(Viewer::Key)` returns `Err(MetaError::Corrupt { table: "saved_views", column })` with `column` `kind`, `state`, `state`, `id`, `owner_user_id` respectively. Use a fresh view per case.
25. `kind_text_round_trips` — for each of the five kinds, `parse_kind(kind_str(kind)) == Some(kind)`; `parse_kind("trace") == None`.

26. `visible_to_agrees_with_list_visible` — rows: private of `owner`, shared of `owner`, private of `other`, shared of `other`, and an orphan private and orphan shared (made by deleting `third` after inserting one of each for `third`). For each viewer in `[Viewer::User(owner), Viewer::User(other), Viewer::Key]`, the ids `list_visible(kind: None, limit: 100)` returns are exactly the rows (read with `view_by_id`) for which `visible_to(&row, viewer)` is true.
27. `rows_convert_to_the_api_type` — `SavedView::from(row)` copies every field (`id`, `kind`, `name`, `state`, `owner_user_id`, `shared`, `is_default`, `created_at`, `updated_at`), checked field by field on one inserted row.

## Commands

```bash
export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH"
INSTA_UPDATE=always cargo test -p llmobs-server schema_matches_the_snapshot
git diff crates/server/tests/snapshots/meta_schema.snap
cargo test -p llmobs-server saved_views
cargo test -p llmobs-server meta::
just check
```

## Done when

- Migration 3 creates `saved_views` and its two indexes; the schema snapshot shows exactly that addition.
- `meta::saved_views` exposes `Viewer`, `SavedViewRow`, `SavedViewPage`, `NewSavedView`, `SavedViewChanges`, `MAX_VIEWS_PER_OWNER`, `UNSHARE_ORPHAN`, `kind_str`, `visible_to`, `list_visible`, `get_visible`, `find_visible_by_name`, `view_by_id`, `insert_view`, `update_view`, `delete_view`, and `From<SavedViewRow> for SavedView`, with the signatures above.
- The 27 tests pass; `just check` passes. One commit, subject `server: store saved views in meta.db`.

Report "no new dependency".
