# M1 — Deletions store

Read [README.md](README.md) first.

**Goal:** the `meta.db` tables for deletion requests and a store module with every query the later sub-phases need. No route, no worker: those come in M3–M5. Everything here is tested directly against `meta.db`.

**Edit only:**
- `crates/server/migrations/meta/NNNN_deletions.sql` (new; `NNNN` as README § Code layout says);
- `crates/server/src/meta/deletions.rs` (new);
- `crates/server/src/meta/mod.rs` (one `pub(crate) mod deletions;` line, alphabetical);
- `crates/server/src/admin.rs` (the test `schema_mismatch_names_the_versions` only, below);
- `docs/spec/04-metadata-db.md` only if it differs from the SQL below. The plan commit already rewrote § Deletion requests with this SQL and lifecycle. Compare them. If they differ, fix the spec, not the SQL, unless the SQL here is wrong.

No `Cargo.toml` change: `serde`, `serde_json`, `uuid` and `sqlx` (with `QueryBuilder`, already used in `meta/users.rs`) are dependencies of `llmobs-server`. At the time of writing the highest migration is `0004_connections.sql`, so `NNNN` is `0005` unless another track merged one first.

One existing test hard-codes the migration count: `admin.rs::schema_mismatch_names_the_versions` inserts a "future" migration with a literal version and asserts the literal message `"this binary expects schema 4, found 5"`. Every new migration breaks it. Make it independent of the count: compute `let expected = crate::meta::expected_version();` (make that function `pub(crate)` in `meta/mod.rs` if it is private), bind `expected + 1` as the inserted version, and assert `format!("this binary expects schema {expected}, found {}", expected + 1)`.

**Commit:** `Add the deletions store`

## Read first

- `crates/server/src/meta/mod.rs`: `MetaDb::write` (a boxed-future closure inside `BEGIN IMMEDIATE`), `MetaDb::reader`, `MetaError` (`Db`, `Conflict`, `Corrupt { table, column }`), and `test_db()`.
- `crates/server/src/meta/api_keys.rs`: the style to copy. A `…Row` struct, a private `…Record` with `#[derive(sqlx::FromRow)]`, and `row_from(record) -> Result<Row, MetaError>`, which maps a bad stored value to `MetaError::Corrupt`.
- `crates/server/migrations/meta/0001_identity.sql`: the migration style (four-space indent, one statement per `CREATE`).
- `llmobs_core::ids`:
  - `TraceId::from_slice(&[u8])` and `TraceId::as_bytes()`;
  - `ProjectId` and `OrgId` (`Display`, `FromStr`, `from_uuid`, `as_uuid`).

## Migration

Create `crates/server/migrations/meta/NNNN_deletions.sql`:

```sql
CREATE TABLE deletions (
    id TEXT PRIMARY KEY,
    org_id TEXT NOT NULL,
    project_id TEXT,
    kind TEXT NOT NULL,
    user_id TEXT,
    request TEXT NOT NULL,
    requested_by_kind TEXT NOT NULL,
    requested_by TEXT NOT NULL,
    status TEXT NOT NULL,
    trace_count INTEGER,
    session_count INTEGER,
    created_at INTEGER NOT NULL,
    handed_over_at INTEGER,
    applied_at INTEGER,
    error TEXT
);

CREATE INDEX deletions_project ON deletions(project_id, created_at, id);

CREATE INDEX deletions_org ON deletions(org_id, created_at, id);

CREATE INDEX deletions_open ON deletions(created_at, id) WHERE status IN ('resolving', 'pending');

CREATE TABLE deletion_traces (
    deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
    trace_id BLOB NOT NULL,
    PRIMARY KEY (deletion_id, trace_id)
);

CREATE TABLE deletion_sessions (
    deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    PRIMARY KEY (deletion_id, session_id)
);

CREATE TABLE deletion_projects (
    deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
    project_id TEXT NOT NULL,
    PRIMARY KEY (deletion_id, project_id)
);
```

`deletions` deliberately has **no** foreign key to `orgs` or `projects`: the audit row outlives them (D-M-8). Nothing ever deletes a `deletions` row. The `ON DELETE CASCADE` on the scope tables only keeps the schema honest.

Column values:
- `kind`: `trace_ids` | `filter` | `user` | `project` | `org`.
- `status`: `resolving` | `pending` | `applied` | `failed`.
- `requested_by_kind`: `user` | `api_key`. `requested_by` is the `users.id` or `api_keys.id` UUID text.
- `error`: NULL, `too_many_traces` or `resolution_failed`.
- `request`: compact JSON of [`RequestDetails`](#types).

## Types

All in `crates/server/src/meta/deletions.rs`. Start the file with `//! \`deletions\` rows and their scope tables (spec 04 § Deletion requests, D-M-8).`. Do not add a `dead_code` attribute: `meta/mod.rs` already has `#![cfg_attr(not(test), allow(dead_code))]` for every child module.

```rust
use llmobs_core::ids::{OrgId, ProjectId, TraceId};
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::{MetaError, unique_or};

/// What a deletion covers (D-M-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DeletionKind {
    /// Listed trace ids.
    TraceIds,
    /// Traces matching a filter, search and window.
    Filter,
    /// One end user's data.
    User,
    /// A whole project.
    Project,
    /// A whole org.
    Org,
}

impl DeletionKind {
    /// The stored text.
    pub(crate) fn as_str(self) -> &'static str { /* "trace_ids", "filter", "user", "project", "org" */ }
    /// Parses the stored text. Unknown text is `None`.
    pub(crate) fn parse(text: &str) -> Option<Self> { /* inverse of as_str */ }
}

/// Where a deletion is in its lifecycle (D-M-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeletionState {
    /// The scope is not handed to storage yet.
    Resolving,
    /// Storage hides the rows; or a project/org drop not run yet.
    Pending,
    /// Done.
    Applied,
    /// Resolution gave up; nothing was deleted.
    Failed,
}
// as_str / parse: "resolving", "pending", "applied", "failed".

/// Who asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActorKind {
    /// A session or personal access token user.
    User,
    /// A project API key.
    ApiKey,
}
// as_str / parse: "user", "api_key".

/// Why a deletion failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FailReason {
    /// A filter matched more than 100,000 traces at resolution.
    TooManyTraces,
    /// Resolution failed 5 times in a row, or could not succeed on retry.
    ResolutionFailed,
}
// as_str / parse: "too_many_traces", "resolution_failed".

/// The request details kept for audit, as the `request` column's JSON.
///
/// Every field is optional; each kind sets its own:
/// - `trace_ids`: `requested` (distinct ids in the request);
/// - `filter`: `filter`, `search` (each only when non-empty), `from`, `to`,
///   `include_eval`, `include_experiments`;
/// - `user`: nothing;
/// - `project`, `org`: `name`, `slug`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RequestDetails {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) requested: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) filter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) search: Option<String>,
    /// ns, inclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) from: Option<i64>,
    /// ns, exclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) to: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) include_eval: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) include_experiments: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) slug: Option<String>,
}

/// One `deletions` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeletionRow {
    pub(crate) id: Uuid,
    pub(crate) org_id: OrgId,
    /// `None` for `org`.
    pub(crate) project_id: Option<ProjectId>,
    pub(crate) kind: DeletionKind,
    /// `Some` for `user` only.
    pub(crate) user_id: Option<String>,
    pub(crate) request: RequestDetails,
    pub(crate) actor_kind: ActorKind,
    pub(crate) actor_id: Uuid,
    pub(crate) state: DeletionState,
    /// Traces in the stored scope; `None` until resolution stored it.
    pub(crate) trace_count: Option<u64>,
    /// Sessions in the stored scope (`user` only); `None` until then.
    pub(crate) session_count: Option<u64>,
    /// Cut-off, ns.
    pub(crate) created_at: i64,
    pub(crate) handed_over_at: Option<i64>,
    pub(crate) applied_at: Option<i64>,
    pub(crate) error: Option<FailReason>,
}

/// Columns of [`insert_deletion`].
#[derive(Debug, Clone)]
pub(crate) struct NewDeletion {
    pub(crate) id: Uuid,
    pub(crate) org_id: OrgId,
    pub(crate) project_id: Option<ProjectId>,
    pub(crate) kind: DeletionKind,
    pub(crate) user_id: Option<String>,
    pub(crate) request: RequestDetails,
    pub(crate) actor_kind: ActorKind,
    pub(crate) actor_id: Uuid,
    /// `Resolving` for `trace_ids`/`filter`/`user`, `Pending` for `project`/`org`.
    pub(crate) state: DeletionState,
    pub(crate) created_at: i64,
}

/// The deletion guard's view of the recent deletions (M3, D-M-6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct GuardEntries {
    /// `(project, trace, created_at)` of every stored scope trace.
    pub(crate) traces: Vec<(ProjectId, TraceId, i64)>,
    /// `(project, user_id, created_at)` of every `user` deletion.
    pub(crate) users: Vec<(ProjectId, String, i64)>,
}
```

Write `as_str`/`parse` as plain `match` blocks (no `strum`). The `DeletionKind` doc comments above are enough; give the other enums' variants the doc comments shown.

The private record that `sqlx` decodes. `FromRow` maps by name, so its fields are the **column** names, and every `SELECT` lists exactly `SELECT_COLUMNS`:

```rust
#[derive(Debug, sqlx::FromRow)]
struct DeletionRecord {
    id: String,
    org_id: String,
    project_id: Option<String>,
    kind: String,
    user_id: Option<String>,
    request: String,
    requested_by_kind: String,
    requested_by: String,
    status: String,
    trace_count: Option<i64>,
    session_count: Option<i64>,
    created_at: i64,
    handed_over_at: Option<i64>,
    applied_at: Option<i64>,
    error: Option<String>,
}
```

`row_from` maps `requested_by_kind` → `actor_kind`, `requested_by` → `actor_id` and `status` → `state`.

## Functions

Every function returns `Result<_, MetaError>` and has a `/// # Errors` section like `api_keys.rs`. Writes take `&mut SqliteConnection` (they run inside `MetaDb::write`). Reads take `&SqlitePool`. Ids are bound as text (`id.to_string()`); trace ids as 16-byte blobs (`trace_id.as_bytes().as_slice()`); `trace_count`/`session_count` as `i64` (`i64::try_from(n)` → on overflow `MetaError::Corrupt { table: "deletions", column: "trace_count" }`). Decode them back with `u64::try_from`. A negative value is `Corrupt`.

`SELECT_COLUMNS` (a `const &str`):
`id, org_id, project_id, kind, user_id, request, requested_by_kind, requested_by, status, trace_count, session_count, created_at, handed_over_at, applied_at, error`.

```rust
/// Inserts a deletion. A duplicate id is `MetaError::Conflict("duplicate deletion")`.
pub(crate) async fn insert_deletion(conn: &mut SqliteConnection, new: &NewDeletion) -> Result<DeletionRow, MetaError>;
```
- `request` is `serde_json::to_string(&new.request)`. That cannot fail for this struct; map the error to `MetaError::Corrupt { table: "deletions", column: "request" }` anyway, with no `unwrap`.
- Use `super::unique_or(error, "duplicate deletion")`.
- Return the row built from `new`: `trace_count`, `session_count`, `handed_over_at`, `applied_at`, `error` are `None`.

```rust
/// Adds trace ids to the scope. Ids already present are ignored. Runs in chunks of 500.
pub(crate) async fn insert_traces(conn: &mut SqliteConnection, id: Uuid, traces: &[TraceId]) -> Result<(), MetaError>;
/// Same for session ids.
pub(crate) async fn insert_sessions(conn: &mut SqliteConnection, id: Uuid, sessions: &[String]) -> Result<(), MetaError>;
/// Same for project ids (org deletions).
pub(crate) async fn insert_projects(conn: &mut SqliteConnection, id: Uuid, projects: &[ProjectId]) -> Result<(), MetaError>;
```
Build one `INSERT OR IGNORE INTO deletion_traces (deletion_id, trace_id) VALUES (?, ?), (?, ?), …` per chunk of 500 with `sqlx::QueryBuilder::<sqlx::Sqlite>::push_values`. Use a new builder per chunk and `.build().execute(&mut *conn)`. An empty slice does nothing. A deletion id that does not exist fails on the foreign key (`foreign_keys=ON`) with `MetaError::Db`. Callers always insert the row first.

```rust
/// Stores the resolved counts. Only a `resolving` row changes; returns whether one did.
pub(crate) async fn store_counts(conn: &mut SqliteConnection, id: Uuid, trace_count: u64, session_count: Option<u64>) -> Result<bool, MetaError>;
```
`UPDATE deletions SET trace_count = ?, session_count = ? WHERE id = ? AND status = 'resolving'`, then `rows_affected() == 1`.

```rust
/// `resolving` → `pending`, or a `pending` row whose hand-over was cleared; sets `handed_over_at`.
pub(crate) async fn mark_handed_over(conn: &mut SqliteConnection, id: Uuid, now: i64) -> Result<bool, MetaError>;
```
`UPDATE deletions SET status = 'pending', handed_over_at = ? WHERE id = ? AND (status = 'resolving' OR (status = 'pending' AND handed_over_at IS NULL))`.

```rust
/// `resolving` or `pending` → `applied` at `applied_at`. `handed_over_at` is kept as is.
pub(crate) async fn mark_applied(conn: &mut SqliteConnection, id: Uuid, applied_at: i64) -> Result<bool, MetaError>;
/// `resolving` → `failed` with `reason`.
pub(crate) async fn mark_failed(conn: &mut SqliteConnection, id: Uuid, reason: FailReason) -> Result<bool, MetaError>;
/// A `pending` row's `handed_over_at` back to NULL (storage lost it; M3 registers it again).
pub(crate) async fn clear_handover(conn: &mut SqliteConnection, id: Uuid) -> Result<bool, MetaError>;
```
- `mark_applied`: `WHERE id = ? AND status IN ('resolving', 'pending')`.
- `mark_failed`: `WHERE id = ? AND status = 'resolving'`.
- `clear_handover`: `WHERE id = ? AND status = 'pending'`.

```rust
/// Open deletions, read inside a write transaction: rows of `project` in `resolving` or `pending`.
/// `org` rows have no `project_id` and never count (the cap is per project, D-M-5).
pub(crate) async fn count_open_in_project(conn: &mut SqliteConnection, project: ProjectId) -> Result<u64, MetaError>;
```
`SELECT COUNT(*) FROM deletions WHERE project_id = ? AND status IN ('resolving', 'pending')`, decoded as `i64`, then `u64::try_from` (a negative value is `Corrupt { table: "deletions", column: "status" }`).

```rust
/// One row by id, any project.
pub(crate) async fn deletion_by_id(pool: &SqlitePool, id: Uuid) -> Result<Option<DeletionRow>, MetaError>;
/// One row by id when it belongs to `project`; `None` otherwise (the route answers 404).
pub(crate) async fn deletion_in_project(pool: &SqlitePool, project: ProjectId, id: Uuid) -> Result<Option<DeletionRow>, MetaError>;
```

```rust
/// Newest first: `ORDER BY created_at DESC, id DESC`. `after` is the last row of the previous
/// page, as `(created_at, id)`; rows strictly after it in that order come next. At most `limit` rows.
pub(crate) async fn page_of_project(pool: &SqlitePool, project: ProjectId, after: Option<(i64, Uuid)>, limit: u32) -> Result<Vec<DeletionRow>, MetaError>;
/// Same over every row of `org` (all kinds, including dropped projects' rows).
pub(crate) async fn page_of_org(pool: &SqlitePool, org: OrgId, after: Option<(i64, Uuid)>, limit: u32) -> Result<Vec<DeletionRow>, MetaError>;
```
SQL for `after`: `… AND (created_at < ? OR (created_at = ? AND id < ?)) ORDER BY created_at DESC, id DESC LIMIT ?`. Bind `created_at` twice, then `id.to_string()`, then `i64::from(limit)`. The tie on `id` (text comparison) keeps the order total. Callers (M4) pass `limit` between 1 and 1001. The function does not clamp it.

```rust
/// Every `resolving` or `pending` row, oldest first (`created_at, id`). The worker's input.
pub(crate) async fn open_deletions(pool: &SqlitePool) -> Result<Vec<DeletionRow>, MetaError>;
```

```rust
/// The stored scope.
pub(crate) async fn scope_traces(pool: &SqlitePool, id: Uuid) -> Result<Vec<TraceId>, MetaError>;
pub(crate) async fn scope_sessions(pool: &SqlitePool, id: Uuid) -> Result<Vec<String>, MetaError>;
pub(crate) async fn scope_projects(pool: &SqlitePool, id: Uuid) -> Result<Vec<ProjectId>, MetaError>;
```
- Order them `ORDER BY trace_id` / `session_id` / `project_id`.
- A blob that `TraceId::from_slice` rejects is `Corrupt { table: "deletion_traces", column: "trace_id" }`.
- A bad project id is `Corrupt { table: "deletion_projects", column: "project_id" }`.

```rust
/// Rows the deletion guard needs: deletions created at or after `since`.
/// - traces: the `deletion_traces` of rows of kind `trace_ids`, `filter` or `user`;
/// - users: `user_id` of rows of kind `user`.
/// Rows in any status, `failed` included: their entries do no harm and expire.
pub(crate) async fn guard_entries(pool: &SqlitePool, since: i64) -> Result<GuardEntries, MetaError>;
```
SQL:
- `SELECT d.project_id, t.trace_id, d.created_at FROM deletions d JOIN deletion_traces t ON t.deletion_id = d.id WHERE d.created_at >= ? AND d.kind IN ('trace_ids', 'filter', 'user') ORDER BY d.created_at, d.id, t.trace_id`;
- `SELECT project_id, user_id, created_at FROM deletions WHERE created_at >= ? AND kind = 'user' ORDER BY created_at, id`.

A NULL `project_id` or `user_id` on those kinds is `Corrupt`.

### Decoding

`row_from(record: DeletionRecord) -> Result<DeletionRow, MetaError>`:
- Each text enum through its `parse`. `None` is `Corrupt { table: "deletions", column: "<column name>" }`.
- `request` through `serde_json::from_str::<RequestDetails>`. An error is `Corrupt { …, column: "request" }`.
- Ids through `Uuid::parse_str` / `str::parse::<OrgId>` / `ProjectId`. An error is `Corrupt`.
- `user_id`: present iff `kind == User`. A mismatch is `Corrupt { column: "user_id" }`.
- `project_id`: absent iff `kind == Org`. A mismatch is `Corrupt { column: "project_id" }`.

## Tests

In `meta/deletions.rs`, `#[cfg(test)] mod tests`, `#[tokio::test]`. Each test opens `let (_dir, db) = crate::meta::test_db().await;` and writes with `db.write(|conn| Box::pin(async move { … })).await.unwrap()`.

Helpers:
- `const ORG: &str = "01900000-0000-7000-8000-000000000010";`, `const PROJECT_A: &str = "01900000-0000-7000-8000-000000000001";`, `const PROJECT_B: &str = "01900000-0000-7000-8000-000000000002";`, parsed with `.parse::<OrgId>().unwrap()` / `.parse::<ProjectId>().unwrap()`.
- `fn trace(n: u128) -> TraceId { TraceId::from_bytes((n + 1).to_be_bytes()).unwrap() }`. `n + 1` keeps the bytes non-zero; `from_bytes` rejects all zeros.
- `fn new(n: u128, kind: DeletionKind, project: ProjectId, created_at: i64) -> NewDeletion`:
  - `id = Uuid::from_u128(n)`, so the id order is the order of `n`;
  - `org_id` = `ORG`;
  - `project_id` = `Some(project)`, or `None` for `Org`;
  - `user_id` = `Some("user-1")` for `User`, else `None`;
  - `request` = `RequestDetails::default()`;
  - actor `ActorKind::User`, `Uuid::from_u128(1000 + n)`;
  - state `Resolving` for `TraceIds`/`Filter`/`User`, `Pending` for `Project`/`Org`.

No `orgs` or `projects` rows are needed: there are no foreign keys, except in test 13.

1. `insert_and_read_back_every_kind`:
   - Insert one row per kind, each with a distinct `RequestDetails`:
     - `trace_ids`: `requested: Some(3)`;
     - `filter`: every filter field set;
     - `project` and `org`: `name` and `slug`.
   - `deletion_by_id` returns rows equal to what `insert_deletion` returned.
   - `trace_count`, `session_count`, `handed_over_at`, `applied_at` and `error` are `None`.
2. `duplicate_id_is_a_conflict`: inserting the same `NewDeletion` twice gives `MetaError::Conflict("duplicate deletion")`.
3. `scope_inserts_ignore_duplicates`:
   - `insert_traces` with `[trace(0), trace(1), trace(0)]`, then `[trace(1), trace(2)]`. `scope_traces` returns `[trace(0), trace(1), trace(2)]`.
   - Same pattern with sessions `["b", "a", "b"]`, then `["c"]`: `scope_sessions` returns `["a", "b", "c"]`.
   - Same with projects `[PROJECT_B, PROJECT_A]`: `scope_projects` returns them sorted by their text, A first.
   - Each `insert_*` with an empty slice returns `Ok(())`.
4. `scope_inserts_chunk`: one `insert_traces` call with `trace(0)..trace(1233)` plus `trace(0)` and `trace(600)` again at the end. `scope_traces` returns 1,234 ids.
5. `scope_insert_for_an_unknown_deletion_fails`: `insert_traces(conn, Uuid::from_u128(99), &[trace(0)])` gives `Err(MetaError::Db(_))`.
6. `resolving_path`, on a `trace_ids` row:
   - `mark_failed` on a `pending` row is tested in 7, so first check `clear_handover` → `false` (still `resolving`).
   - `store_counts(id, 3, None)` → `true`; read back `trace_count == Some(3)` and `session_count == None`.
   - `mark_handed_over(id, 50)` → `true`; status `Pending`, `handed_over_at == Some(50)`.
   - `store_counts` again → `false`.
   - `mark_handed_over(id, 60)` → `false`: it is still handed over, at 50.
   - `mark_failed` → `false`.
   - `clear_handover` → `true`, then `handed_over_at` is `None`; `mark_handed_over(id, 70)` → `true`.
   - `mark_applied(id, 80)` → `true`; status `Applied`, `applied_at == Some(80)`, `handed_over_at == Some(70)`.
   - `mark_applied` → `false`; `mark_handed_over` → `false`; `clear_handover` → `false`.
7. `other_paths`:
   - A `filter` row: `store_counts(id, 0, None)`, then `mark_applied(id, 5)` straight from `resolving` → `true`, and `handed_over_at` stays `None`.
   - A `user` row: `store_counts(id, 2, Some(1))` reads back `session_count == Some(1)`. `mark_failed(id, TooManyTraces)` → `true`, `error == Some(TooManyTraces)`. Then `mark_handed_over` → `false`.
   - A `project` row (inserted `Pending`, `handed_over_at` NULL): `mark_failed` → `false`; `mark_handed_over(id, 9)` → `true`.
   - `store_counts(id, u64::MAX, None)` on a fresh `resolving` row gives `Corrupt { table: "deletions", column: "trace_count" }`.
8. `count_open_counts_resolving_and_pending_of_one_project`:
   - Project A has rows in `resolving`, `pending`, `applied` and `failed`. Use `mark_applied` and `mark_failed` to set the last two.
   - Project B has one `resolving` row. There is one `org` row.
   - `count_open_in_project(A)` is 2.
9. `pages_are_newest_first_with_a_cursor`:
   - Rows `n` = 1..=5 in project A with `created_at` 10, 20, 20, 30, 40 respectively, plus an `org` row `n = 6` at 50.
   - `page_of_project(A, None, 2)` returns n = 5, 4.
   - `after = (30, id(4))` with limit 2 returns n = 3, 2: both at 20, id descending.
   - `after = (20, id(2))` returns n = 1. `after = (10, id(1))` returns nothing.
   - `page_of_org(ORG, None, 10)` returns n = 6, 5, 4, 3, 2, 1.
10. `open_deletions_are_oldest_first`: rows at 30 (`resolving`), 10 (`pending`), 20 (`applied`) and 40 (`failed`). `open_deletions` returns the rows at 10 and 30, in that order.
11. `deletion_in_project_checks_the_project`: a row of project A is `Some` for A and `None` for B.
12. `guard_entries_since`:
    - Row 1, `trace_ids`, at 100, with scope `[trace(0), trace(1)]`.
    - Row 2, `user`, at 200, with scope `[trace(2)]`.
    - Row 3, `filter`, at 50, with scope `[trace(3)]`.
    - Row 4, `project`, at 300, with scope `[trace(4)]` added through `insert_traces`, which a project deletion never does but which proves the kind filter.
    - `guard_entries(100)` returns:
      - `traces == [(A, trace(0), 100), (A, trace(1), 100), (A, trace(2), 200)]`;
      - `users == [(A, "user-1".to_owned(), 200)]`.
13. `rows_outlive_their_org_and_project`:
    - `let org = crate::meta::orgs::insert_org(conn, "Acme", "acme", 1).await?;` and `let project = crate::meta::projects::insert_project(conn, org.id, "Bot", "bot", None, 1).await?;`.
    - Insert a `trace_ids` row for `project.id` with scope `[trace(0)]`, and an `org` row for `org.id`.
    - Run `DELETE FROM projects WHERE id = ?`, then `DELETE FROM orgs WHERE id = ?`.
    - Both deletion rows and the scope trace are still there.
14. `scope_rows_go_with_their_deletion`: a raw `DELETE FROM deletions WHERE id = ?` removes its `deletion_traces`, `deletion_sessions` and `deletion_projects` rows. Nothing in the server does this; the test keeps the schema's `ON DELETE CASCADE` honest.
15. `corrupt_values_are_reported`. Each case starts from a fresh valid row and changes one value with a raw `UPDATE` (or `INSERT` for the scope tables). The expected error follows each change:
    - `deletions`, from `deletion_by_id`:
      - `kind = 'nope'` → `Corrupt { table: "deletions", column: "kind" }`;
      - `status = 'nope'` → column `status`;
      - `requested_by_kind = 'robot'` → column `requested_by_kind`;
      - `request = '{"bogus":1}'` → column `request`;
      - `id = 'x'` (through `page_of_project`) → column `id`;
      - `trace_count = -1` → column `trace_count`;
      - `user_id = 'u'` on a `trace_ids` row → column `user_id`;
      - `user_id = NULL` on a `user` row → column `user_id`;
      - `project_id = NULL` on a `project` row → column `project_id`;
      - `project_id = '<PROJECT_A>'` on an `org` row → column `project_id`.
    - Scope tables:
      - a 15-byte blob in `deletion_traces` → `scope_traces` gives `Corrupt { table: "deletion_traces", column: "trace_id" }`;
      - 16 zero bytes → the same error;
      - `project_id = 'x'` in `deletion_projects` → `scope_projects` gives `Corrupt { table: "deletion_projects", column: "project_id" }`.
    - `guard_entries`: a `user` row with `user_id = NULL` gives `Corrupt { table: "deletions", column: "user_id" }`.

## Done when

- [ ] The migration and `meta/deletions.rs` exist as above; `meta/mod.rs` declares the module.
- [ ] The 15 tests pass.
- [ ] `docs/spec/04-metadata-db.md` § Deletion requests shows this exact SQL.
- [ ] `just check` is green; one commit `Add the deletions store`.
