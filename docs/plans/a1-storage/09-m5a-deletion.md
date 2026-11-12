# A1 · M5a — Deletion: tombstones, flush, compaction and the rewriter

First half of milestone M5, **Deletion, retention, project drop, media** (`00-design.md` § Milestones). It builds GDPR deletion of `traces` and `user` scopes end to end:
- the catalog bookkeeping (`deletions_applied_seq`, the `deletions` tables, deletion sequence numbers);
- tombstones that hide rows from every query as soon as `register_deletion` returns;
- flush and compaction that drop tombstoned rows;
- the rewriter that erases them from existing files, and the "applied" bookkeeping.

The second half, `10-m5b-retention-media-drop.md`, adds retention, media blobs, project drop, and the crash and model tests over all of M5.

Do the sub-phases in order, one commit each. Don't start M5b before M5.4 is committed.

## Read first

- `AGENTS.md`, in full. The rules on compatibility layers, abstractions, placeholders, errors, `unwrap`, panics on disk input, privacy in logs, and tests all apply.
- `docs/spec/03-storage.md` §4 (catalog, committer, read state), §5 (compaction) and §7 (deletion). §7 was rewritten together with this plan and is what this plan implements.
- `docs/plans/a1-storage/00-design.md` § Public API, § Deletion, § Commit protocol, § Testing.
- `docs/mvp-decisions.md` D83, D88, D-A1-1, and D-A1-3 to D-A1-5 (written with this plan).
- `docs/plans/a1-storage/07-m4a-gc-and-merge.md` and `08-m4b-compaction.md`: the compaction code this plan extends.
- The code on `main` after PR #30 (commit `856c826`): `crates/storage/src/{catalog/, compaction/, query/, flush.rs, memtable.rs, read_state.rs, gc.rs, hooks.rs, config.rs, error.rs, lib.rs, test_support.rs, crash_tests.rs}`.

## Prerequisites

- `main` contains M4 (PR #30). Branch from it.

## Conventions for every sub-phase

- **`just check` passes before each commit.** Commit subjects are given per sub-phase.
- **Dependencies.** M5a adds none. Everything used is in the pinned `datafusion` 55.2, `arrow` 59, `parquet` 59 and `object_store` 0.13. Check these in the registry sources (`~/.cargo/registry/src/*/datafusion-physical-expr-55.*`, `datafusion-physical-plan-55.*`) before using them:
  - `datafusion::physical_expr::expressions::{in_list, BinaryExpr, Column, Literal, IsNotNullExpr, NotExpr}`;
  - `datafusion::physical_plan::filter::FilterExec`, `datafusion::physical_plan::projection::ProjectionExec`, `datafusion::physical_plan::limit::GlobalLimitExec`;
  - `PhysicalExpr::evaluate(&RecordBatch) -> Result<ColumnarValue>` and `ColumnarValue::into_array(num_rows)`;
  - `arrow::compute::{not, filter_record_batch}`.
- **`StorageConfig` literals.** M5.4 adds a field to `StorageConfig`. Run `grep -rn "StorageConfig {" crates/` and update every literal in the same commit, using `Default::default()` for the new section unless the test needs other values. That includes the literals in `crates/server` (`src/serve.rs` tests and `src/test_support/mod.rs`): there, add only the new field, set to `Default::default()`.
- **`FileMeta` literals.** M5.1 adds a field to `FileMeta`. Run `grep -rn "fully_merged:" crates/storage/` to find every literal and set the new field in each, in the same commit.
- **`ReadState` literals.** M5.2 adds a field to `ReadState`. Today they are in `flush.rs` (`publish_flush`), `wal/writer.rs`, `read_state.rs` (`empty`, `publish_catalog`) and `lib.rs` (`open_inner`). Run `grep -rn "ReadState {" crates/storage/src` and update every one.
- **Hook points.** A new `HookPoint` variant also needs its arm in `HookPoint::index`, and `COUNT` raised by one (`hooks.rs`). This plan names every new hook point; add no others.
- **Logs.** Log deletion ids, project ids, file ids, deletion sequence numbers, counts and byte counts. **Never log a `user_id`, a `session_id` or a trace id from a deletion scope**: they identify the person the deletion is about. Never log row content.
  - A DataFusion error raised by a plan that holds the covered-row predicate may quote its literals. So from M5.4 on, the compaction job failure logs (`pass`, the deletion pass) don't print `%error`. They print `error = error.label()`, where `CompactionError::label(&self) -> &'static str` (new, M5.4) returns the variant name in snake case (`"read"`, `"write"`, `"object"`, `"commit"`, `"stage"`, `"dedup"`, `"partials"`, `"input"`, `"task"`, `"fault"`, `"stopped"`, `"deletion"`). A `CommitError::Conflict` still logs its static `reason`.
- **Pause points in tests.** `Hooks::pause` blocks its OS thread. Every test that holds a hook while another task runs uses `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`, like the M4 race tests.
- **Errors.** `thiserror` enums. No `unwrap`/`expect` outside tests unless a comment states the invariant. A malformed file or catalog row is an error, never a panic.
- **Time.** `i64` nanoseconds since the epoch, UTC. "Now" in library code is `catalog::now_ns()`, except where a function takes `now` as a parameter (the deletion pass does, so tests control time).
- **What not to build in M5a.**
  - Retention, media, project drop: M5b.
  - Deletions of kind `project` or `org`: they are `drop_project` calls (M5.7), not `register_deletion`.
  - Server code that calls these APIs: that is the server track (D, wave 2).
- **Report at the end of each sub-phase** (in your final message, not in a file):
  - what was done;
  - any deviation from this plan, and why;
  - anything left undone or unverified;
  - files outside `crates/storage/` and `docs/` that you changed. Only one kind is allowed: the `StorageConfig` literals in `crates/server` (one added field each, in M5.4).

## Vocabulary

- **Deletion:** one request handed over by the server, with an id (UUIDv7), a project, a **cut-off** (`i64` ns, the request's `created_at`) and a **scope**.
- **Scope:**
  - `Traces { trace_ids }`;
  - `User { user_id, trace_ids, session_ids }`. The server resolved the trace and session ids when it created the request, and added the evaluation traces about them.
- **Deletion sequence (`deletion_seq`):** 1, 2, 3, … in registration order, across all projects. Stored in `catalog.db`.
- **Known sequence (`known_seq`):** the highest `deletion_seq` registered so far, pending or applied, or 0.
- **Pending deletion:** registered, not yet applied. Its rows are hidden by **tombstones**.
- **`deletions_applied_seq` (das) of a file:** no row in scope of any deletion with `deletion_seq ≤ das` remains in the file.
- **Applied:** every active `spans` and `scores` file of the project with `min_ingest_time ≤ cutoff` has `das ≥ deletion_seq`, and no memtable batch of the project has `min_ingest_time ≤ cutoff`.

**Which rows a pending deletion `d` covers** (D-A1-4). A row is covered when `ingest_time ≤ d.cutoff` and:

| Table | `Traces` scope | `User` scope |
|---|---|---|
| `spans` | `trace_id ∈ trace_ids` | `trace_id ∈ trace_ids`, or `user_id = user_id` |
| `scores` | `trace_id ∈ trace_ids` | `trace_id ∈ trace_ids`, or `user_id = user_id`, or `session_id ∈ session_ids` |
| `traces` partials (queries only) | `trace_id ∈ trace_ids`, with no cut-off test | same, `trace_id ∈ trace_ids` only |

`session_ids` match `scores` only: a session shared with another user keeps that user's spans. `traces` partials carry no per-row ingest time, so a query hides a partial by trace id alone; the rewriter recomputes `traces` files from rewritten `spans` files, which applies the full rule (D-A1-3). For a `User` scope the `traces` view relies on the resolved trace ids: the server resolves every trace with a span of the user ingested up to the cut-off, so `user_id` adds nothing there.

**Known limits** (documented in spec 03 §7, nothing to build):
- `EXPLAIN` in the SQL console prints the covered-row predicate, so a project member can see the ids of a pending deletion until it is applied (accepted, D-A1-3).
- A row stamped with `ingest_time ≤ cut-off` but appended after its deletion is applied is neither dropped nor hidden. The server stamps `ingest_time` at receipt, so this needs a request in flight for longer than the deletion takes to apply.

---

## M5.1 — Catalog bookkeeping

**Goal:** `catalog.db` stores each file's `deletions_applied_seq` and the deletions with their scopes. The committer persists and loads them. Nothing reads them yet.

### Migration `crates/storage/migrations/catalog/0003_deletions.sql`

```sql
ALTER TABLE data_files ADD COLUMN deletions_applied_seq INTEGER NOT NULL DEFAULT 0;

CREATE TABLE deletions (
    id TEXT PRIMARY KEY,
    deletion_seq INTEGER NOT NULL UNIQUE,
    project_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    cutoff INTEGER NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    registered_at INTEGER NOT NULL,
    applied_at INTEGER
);

CREATE INDEX deletions_pending ON deletions (deletion_seq) WHERE status = 'pending';

CREATE TABLE deletion_traces (
    deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
    trace_id BLOB NOT NULL,
    PRIMARY KEY (deletion_id, trace_id)
);

CREATE TABLE deletion_users (
    deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL,
    PRIMARY KEY (deletion_id, user_id)
);

CREATE TABLE deletion_sessions (
    deletion_id TEXT NOT NULL REFERENCES deletions(id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    PRIMARY KEY (deletion_id, session_id)
);
```

- `kind` is `'traces'` or `'user'`. `status` is `'pending'` or `'applied'`.
- `trace_id` is the 16 raw bytes.
**Existing migration tests** (each later migration of M5 repeats these steps for itself):
- The test in `catalog/committer.rs` that asserts `data_files` has **no** `deletions_applied_seq` column (it queries `pragma_table_info('data_files')`, around line 770) now asserts the column exists.
- The migration counts rise by one per new migration: `assert_eq!(migrations, 2)` in `catalog/committer.rs` (around line 755) and `assert_eq!(versions, 2)` in `catalog/db.rs` (around line 378) become 3 here, then 4, 5 and 6 in M5.5, M5.6 and M5.7.
- Add a test `migration_applies_to_an_m4_catalog` in `catalog/db.rs`:
  1. open a fresh catalog (all migrations applied) and insert one `data_files` row with the 0001 columns only;
  2. undo every migration from 0003 on, newest first: drop the tables and indexes they created (`DROP INDEX deletions_pending`, `DROP TABLE deletion_sessions`, `deletion_users`, `deletion_traces`, `deletions`), `ALTER TABLE data_files DROP COLUMN deletions_applied_seq`, and `DELETE FROM _sqlx_migrations WHERE version >= 3`;
  3. reopen with `open_catalog_db`: the migrations run again, the row has `deletions_applied_seq = 0`, and the new tables exist.

  M5.5, M5.6 and M5.7 add their own objects to step 2 (and keep `version >= 3`).

### `FileMeta`

- Add `pub(crate) deletions_applied_seq: u64` to `FileMeta` (`catalog/mod.rs`), documented as "no row of a deletion with `deletion_seq` at or below this remains in the file (spec 03 §7)".
- `db.rs`: add the column to the `load` `SELECT` and to `file_from_row` (decoded with `parse_u64`, `what = "deletions_applied_seq"`).
- `committer.rs` `write_edit`: the `INSERT INTO data_files` of each added file stores it.
- Every `FileMeta` literal (see § Conventions): flush outputs (`flush.rs` `prepared_file`) and compaction outputs (`compaction/job.rs`) set it to `0` in this sub-phase; M5.3 and M5.4 set real values. Test literals set `0`.

### Raising `deletions_applied_seq` without rewriting

`CatalogEdit` gains:

```rust
/// Files whose `deletions_applied_seq` rises to the given value: the rewriter
/// checked them and found no covered row. Each must be active and its stored
/// value lower than the new one.
pub raise: Vec<(FileId, u64)>,
```

- Every `CatalogEdit` literal sets `raise: Vec::new()` (`grep -rn "CatalogEdit {" crates/storage/src`).
- `write_edit`, after the `remove` checks: for each `(id, seq)`, `SELECT state, deletions_applied_seq FROM data_files WHERE id = ?`. A missing row or a state other than `active` is `CommitError::conflict("raised file is not active")`. A stored value `≥ seq` is `CommitError::conflict("deletion sequence is not greater than the stored value")`. Otherwise `UPDATE data_files SET deletions_applied_seq = ? WHERE id = ?`.
- A file id may not appear in both `remove` and `raise`: `CommitError::conflict("raised file is also removed")`, checked first.
- `Catalog::apply`: for each raised id, replace the file's `Arc<FileMeta>` in its day's `Vec` by a new `Arc` holding a clone with the new value, keeping the `Vec`'s order. An id that is not in the in-memory catalog is `CommitError::conflict("raised file is not active")` (the SQL check already rejects it; this keeps the two in step). Add a helper `fn replace_active(tables: &mut Tables, id: FileId, f: impl FnOnce(&FileMeta) -> FileMeta) -> bool` next to `remove_active`.
- `traces` files are never raised: the "applied" rule reads only `spans` and `scores` files. A `traces` file derived from a clean `spans` file may hold a partial of a covered trace made only of spans ingested after the cut-off; the `traces` view keeps hiding that partial until the deletion is applied. That over-hides for at most `deletion.max_delay`, and is accepted.

### Deletion types (`src/deletion.rs`, new module, `pub(crate) mod deletion;` in `lib.rs`)

Public types, re-exported from `lib.rs` (`pub use crate::deletion::{Deletion, DeletionScope, DeletionStatus};`):

```rust
/// One deletion request handed over by the server (spec 03 §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deletion {
    /// The server's `meta.db` id, UUIDv7. Registration is idempotent by it.
    pub id: Uuid,
    pub project: ProjectId,
    /// Rows with `ingest_time` at or before this (ns) are covered.
    pub cutoff: i64,
    pub scope: DeletionScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeletionScope {
    Traces { trace_ids: Vec<TraceId> },
    User { user_id: String, trace_ids: Vec<TraceId>, session_ids: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionStatus {
    Pending,
    Applied { applied_at: i64 },
}
```

`TraceId` is `llmobs_core::ids::TraceId` (`as_bytes()` gives the 16 bytes). Every public item and field gets a doc comment (`#![warn(missing_docs)]`).

Internal types:

```rust
/// A pending deletion as the read path and the jobs use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingDeletion {
    pub(crate) id: Uuid,
    pub(crate) seq: u64,
    pub(crate) project: ProjectId,
    pub(crate) cutoff: i64,
    pub(crate) registered_at: i64,
    pub(crate) trace_ids: HashSet<[u8; 16]>,
    /// Empty for a `Traces` scope, one entry for a `User` scope.
    pub(crate) users: HashSet<String>,
    /// Empty for a `Traces` scope.
    pub(crate) sessions: HashSet<String>,
}

/// Every pending deletion, by project, plus the known sequence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Tombstones {
    known_seq: u64,
    by_project: HashMap<ProjectId, Arc<[Arc<PendingDeletion>]>>,
}

impl Tombstones {
    pub(crate) fn known_seq(&self) -> u64;
    /// The project's pending deletions in `seq` order; empty when it has none.
    pub(crate) fn for_project(&self, project: ProjectId) -> Arc<[Arc<PendingDeletion>]>;
    /// Every pending deletion, in `seq` order.
    pub(crate) fn all(&self) -> Vec<Arc<PendingDeletion>>;
    /// A copy with `deletion` added and `known_seq = max(known_seq, deletion.seq)`.
    pub(crate) fn with(&self, deletion: PendingDeletion) -> Tombstones;
    /// A copy without the deletion `id` of `project`. `known_seq` is unchanged.
    pub(crate) fn without(&self, project: ProjectId, id: Uuid) -> Tombstones;
    /// A copy with `known_seq = max(known_seq, seq)` and the same deletions (M5.7:
    /// a deletion registered for a dropped project).
    pub(crate) fn with_known_seq(&self, seq: u64) -> Tombstones;
    /// A copy without any deletion of `project` (M5.7). `known_seq` is unchanged.
    pub(crate) fn without_project(&self, project: ProjectId) -> Tombstones;
}
```

Add `with_known_seq` and `without_project` in M5.7, not here (no code without a caller).

- `for_project` returns a shared empty `Arc<[_]>` (built once, e.g. `Arc::from(Vec::new())` per call is fine) when the project has none.
- `with` and `without` keep each project's slice sorted by `seq`; a project whose last deletion goes is removed from the map.

### Errors (`error.rs`)

```rust
/// `register_deletion` or `deletion_status` failed.
#[derive(Debug, thiserror::Error)]
pub enum DeletionError {
    /// The request is malformed. `reason` is static and never holds an id from the scope.
    #[error("invalid deletion: {reason}")]
    Invalid { reason: &'static str },
    /// No deletion with this id was registered.
    #[error("deletion not found")]
    NotFound,
    /// `catalog.db` failed.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
}
```

Don't re-export it from `lib.rs`: like `AppendError` and `StorageError`, callers reach it as `llmobs_storage::error::DeletionError` (`error` is a public module).

### Committer

`CommitterInner` gains `tombstones: Arc<Tombstones>`, loaded at open (below). New methods:

```rust
/// The pending deletions as last committed.
pub(crate) async fn tombstones(&self) -> Arc<Tombstones>;

/// Stores `deletion` with the next `deletion_seq`, then calls `publish` with
/// the new tombstones before releasing the mutex. A known id changes nothing
/// and does not call `publish`.
pub(crate) async fn register_deletion<F>(&self, deletion: &Deletion, now: i64, publish: F)
    -> Result<Registered, DeletionError>
where F: FnOnce(&Arc<Tombstones>);

/// Marks a pending deletion applied at `now`, then publishes the tombstones without it.
/// An id that is not pending is `Ok(false)` and does not call `publish`.
pub(crate) async fn mark_applied<F>(&self, project: ProjectId, id: Uuid, now: i64, publish: F)
    -> Result<bool, CatalogError>
where F: FnOnce(&Arc<Tombstones>);

/// `None` for an unknown id.
pub(crate) async fn deletion_status(&self, id: Uuid) -> Result<Option<DeletionStatus>, CatalogError>;
```

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Registered { New { seq: u64 }, Known }
```

**`register_deletion`, in order, under the committer mutex:**
1. Validate, before touching SQLite (errors are `DeletionError::Invalid`):
   - `cutoff <= 0`: `"cutoff must be positive"`;
   - `Traces` with no trace id: `"empty scope"`;
   - `User` with an empty `user_id`: `"empty user id"`. A `User` scope may have no trace ids and no session ids.
2. `SELECT 1 FROM deletions WHERE id = ?`. Present: return `Registered::Known`. The stored request is not compared with the new one.
3. `seq = inner.tombstones.known_seq() + 1`.
4. In one transaction: insert the `deletions` row (`kind` `'traces'` or `'user'`, `status 'pending'`, `registered_at = now`), one `deletion_traces` row per distinct trace id, one `deletion_users` row for a `User` scope, one `deletion_sessions` row per distinct session id. Then hook `CatalogCommit` (pause, then fail → roll back and return `DeletionError::Catalog(CatalogError::db(sqlx::Error::Io(source)))`, where `source` is the injected `io::Error`, as `commit` does for `CommitError::Db`), then commit.
5. `inner.tombstones = Arc::new(inner.tombstones.with(pending))` where `pending` is the `PendingDeletion` built from the request, then `publish(&inner.tombstones)`.
6. Return `Registered::New { seq }`.

**`mark_applied`:** in one transaction, `UPDATE deletions SET status = 'applied', applied_at = ? WHERE id = ? AND status = 'pending'`; zero rows changed: roll back and return `Ok(false)`. Otherwise delete the scope rows (`DELETE FROM deletion_traces WHERE deletion_id = ?`, the same for `deletion_users` and `deletion_sessions`) and commit. An applied request keeps only its id, project, kind, cut-off, sequence and dates: the ids of the person are erased with the data (spec 03 §7); the audit record is the server's `meta.db` row. Then `inner.tombstones = Arc::new(inner.tombstones.without(project, id))`, `publish(..)`, `Ok(true)`.

**Loading at open (`Committer::open`):** after `load`, read the tombstones:
- `known_seq` = `SELECT COALESCE(MAX(deletion_seq), 0) FROM deletions`;
- every `status = 'pending'` row in `deletion_seq` order, with its scope rows. A `trace_id` blob that is not 16 bytes, a negative `deletion_seq`, an unknown `kind` or a bad id is `CatalogError::corrupt(..)` (use the existing constructor; its message is static).

Put the SQL helpers in `catalog/db.rs` (`load_tombstones(connection) -> Result<Tombstones, CatalogError>`) and keep the committer methods short.

### Tests (`catalog/committer.rs` and `deletion.rs`)

1. **Migration:** a fresh catalog has the new tables and column; the M2/M4 catalog test (above) sees `deletions_applied_seq = 0` on existing rows.
2. **`FileMeta` round trip:** commit a file with `deletions_applied_seq = 7`; reopen; the loaded file has 7.
3. **Raise:**
   - raising an active file from 0 to 3 updates SQLite and the in-memory catalog (the new `Arc` holds 3, the other files keep their `Arc`);
   - raising to a value not greater than the stored one, raising an obsolete file, and raising a file also in `remove` each give the named conflict and change nothing.
4. **Register:**
   - two deletions get `seq` 1 and 2; `publish` sees `known_seq` 2 and both pending;
   - re-registering the first id returns `Known`, does not call `publish`, and inserts no row;
   - each invalid request gives its `Invalid` reason and writes nothing;
   - a `CatalogCommit` fault rolls back: no row, tombstones unchanged, and the next registration still gets the next `seq`.
5. **Reload:** register a `Traces` and a `User` deletion, mark the first applied; the first one's `deletion_traces` rows are gone; reopen; `tombstones()` holds only the second, with its trace ids, user and sessions, and `known_seq` 2.
6. **Status:** pending, applied with its `applied_at`, unknown id is `None`.
7. **`Tombstones` unit tests:** `with` keeps `seq` order; `without` removes and drops an emptied project; `for_project` of an unknown project is empty.

**Commit:** `storage: record deletions and deletions_applied_seq in the catalog`

---

## M5.2 — Tombstones in reads, `register_deletion`, `deletion_status`

**Goal:** once `register_deletion` returns, no query sees a covered row: not in files, not in the memtable, not in the `traces` view. `deletion_status` reports the state.

### The predicate (`deletion.rs`)

One function builds the physical expression "this row is covered" for a set of deletions. Queries, flush and compaction all use it, so the rule exists once (D-A1-3).

```rust
/// `Some(expr)` that is true for rows covered by any of `deletions`, against
/// `schema` (the table's stored schema, or a projection of it that holds the
/// columns the rule reads). `None` when `deletions` is empty.
pub(crate) fn covered_predicate(
    table: FileTable,
    schema: &SchemaRef,
    deletions: &[Arc<PendingDeletion>],
) -> Result<Option<Arc<dyn PhysicalExpr>>, DataFusionError>;
```

- **Per deletion `d`, for `Spans`:** `ingest_time <= d.cutoff AND (T OR U)`, where:
  - `T` = `trace_id IS NOT NULL AND trace_id IN (d.trace_ids)`, present when `d.trace_ids` is not empty;
  - `U` = `user_id IS NOT NULL AND user_id IN (d.users)`, present when `d.users` is not empty.
- **For `Scores`:** `ingest_time <= d.cutoff AND (T OR U OR S)`, with `T` and `U` on the `scores` columns and `S` = `session_id IS NOT NULL AND session_id IN (d.sessions)`, present when `d.sessions` is not empty.
- **For `Traces`:** `T` only, with no `ingest_time` term.
- A deletion whose terms are all absent for this table contributes nothing. If no deletion contributes, return `None`.
- The result is the `OR` of the per-deletion expressions, built left to right in `seq` order.
- **Nulls.** Every `IN` term is guarded by `IS NOT NULL`, whatever the column's nullability, so the expression is never null (`FALSE AND NULL` is `FALSE`). This matters: `scores.trace_id`, `scores.user_id`, `scores.session_id`, `spans.user_id` and `traces.user_id` are nullable (`llmobs-core/src/schema.rs`; a session-target score has no `trace_id`). An unguarded `IN` on a null would make the whole expression null, and both `FilterExec` and `filter_record_batch` drop a row whose predicate is null: a session score would be lost by any `Traces` deletion. `ingest_time` is non-null in `spans` and `scores`.
- **Literals:**
  - `trace_id`: `ScalarValue::FixedSizeBinary(16, Some(bytes.to_vec()))`;
  - `user_id`, `session_id`: `ScalarValue::Utf8(Some(..))`;
  - `ingest_time`: `ScalarValue::TimestampNanosecond(Some(cutoff), tz)`, where `tz` is the timezone of that field's `DataType::Timestamp` in `schema`.
  - Sort each set's values before building the `IN` list, so plans are deterministic in tests.
- **Columns:** `Column::new(name, schema.index_of(name)?)`. A missing column is a `DataFusionError::Plan` (a caller bug: the caller must project it).
- Build `IN` with `in_list(expr, list, &false, schema)`, comparisons with `BinaryExpr::new(.., Operator::LtEq, ..)`, `Operator::And`, `Operator::Or`, and `IsNotNullExpr::new`.

```rust
/// The rows of `batch` not covered by `predicate`. `batch` holds every column
/// `predicate` reads.
pub(crate) fn drop_covered(batch: &RecordBatch, predicate: &Arc<dyn PhysicalExpr>)
    -> Result<RecordBatch, DataFusionError>;
```

Evaluate `predicate` on `batch`, turn the `ColumnarValue` into a `BooleanArray` of `batch.num_rows()` (`into_array`, then `as_boolean_opt`; anything else is `DataFusionError::Internal`). Turn any null into `false` with `arrow::compute::prep_null_mask_filter` (so a null is "not covered", as a second guard behind the `IS NOT NULL` terms), negate it with `arrow::compute::not`, and `filter_record_batch`. Check both functions are re-exported under `arrow::compute` in arrow 59 (they are defined in `arrow-select` and `arrow-arith`).

```rust
/// The deletions that may cover rows of a file or batch: those with `seq`
/// above `applied` (`None` for a memtable batch) and `cutoff >= min_ingest_time`.
pub(crate) fn applicable(
    deletions: &[Arc<PendingDeletion>],
    applied: Option<u64>,
    min_ingest_time: i64,
) -> Vec<Arc<PendingDeletion>>;
```

The **key columns** each table's predicate reads, as a `pub(crate) fn key_columns(table: FileTable) -> &'static [&'static str]`:
- `Spans`: `trace_id`, `user_id`, `ingest_time`;
- `Scores`: `trace_id`, `user_id`, `session_id`, `ingest_time`;
- `Traces`: `trace_id`.

### `MemBatch::min_ingest_time`

- Add `pub min_ingest_time: i64` to `MemBatch` (`memtable.rs`): the minimum of the `ingest_time` column. Both tables have it (`spans::INGEST_TIME`, `scores::INGEST_TIME`, `Timestamp(Nanosecond)`); a missing or null value is `MemtableError::BadColumn { column }`, like the time column.
- `MemBatch::from_batch` computes it. The coalesced batch (`concat_small`) takes the minimum of its parts.
- The other `MemBatch` literals set it too: `anchor.rs` (around line 543) and `flush.rs` tests (around line 621); run `grep -rn "MemBatch {" crates/storage/src` to find them all.
- Make `MemBatch::from_batch(table, batch)` `pub(crate)`. A filtered copy of a batch is `MemBatch::from_batch(table, filtered)`: it recomputes every field and starts a **fresh** partials cache.

### `ReadState`

Add `pub tombstones: Arc<Tombstones>`. Every `ReadState` literal (see § Conventions) copies it from the current state (`Arc::clone(&state.tombstones)`), except:
- `ReadState::empty()`: `Arc::new(Tombstones::default())`;
- `open_inner`: `committer.tombstones().await`.

Add to `read_state.rs`:

```rust
/// Publishes `tombstones` with everything else unchanged.
/// Call it inside the committer's publish closure (the committer mutex is held).
pub(crate) fn publish_tombstones(published: &Published, tombstones: &Arc<Tombstones>)
```

### `Storage::register_deletion` and `Storage::deletion_status` (`lib.rs`)

```rust
/// Registers a `traces` or `user` deletion. Idempotent by `deletion.id`.
/// Rows in scope are hidden from every query once this returns (spec 03 §7).
pub async fn register_deletion(&self, deletion: Deletion) -> Result<(), DeletionError>;

/// `Pending` or `Applied`. An unknown id is `DeletionError::NotFound`.
pub async fn deletion_status(&self, id: Uuid) -> Result<DeletionStatus, DeletionError>;
```

`register_deletion` calls `self.committer.register_deletion(&deletion, catalog::now_ns(), |t| publish_tombstones(&published, t))` and discards the `Registered` value. On `New { seq }` log `info!(deletion_id = %deletion.id, project = %deletion.project, seq, "deletion registered")`; never log the scope.

### Queries: memtable side (`lib.rs` `Storage::query`)

Before building the providers:
- `let deletions = state.tombstones.for_project(project);`
- If it is not empty, replace `state.spans` and `state.scores` by **filtered copies for this project**, built by `pub(crate) fn filter_snapshot(table: Table, snapshot: &MemSnapshot, project: ProjectId, deletions: &[Arc<PendingDeletion>]) -> Result<Arc<MemSnapshot>, QueryError>` in `deletion.rs`:
  - partitions of other projects are dropped from the copy (the providers only read this project);
  - for each batch of the project: `applicable(deletions, None, batch.min_ingest_time)`. Empty: keep the batch as is (same `Arc`s, same partials cache). Otherwise build the predicate for that set against the table's full schema (`table.schema()`), `drop_covered`, and keep `MemBatch::from_batch(table, filtered)` unless it has no rows left.
- Errors: a `DataFusionError` is `QueryError::DataFusion`; a `MemtableError` from `from_batch` is `QueryError::DataFusion(DataFusionError::External(Box::new(error)))`.
- The `traces` provider gets the filtered spans snapshot too, so its memtable partials are computed from the remaining rows (fresh caches).

When `deletions` is empty, nothing is copied: the providers get the snapshots as today.

### Queries: file side, `spans` and `scores` (`query/mod.rs`)

- `ScanUnion` gains `pub deletions: &'a [Arc<PendingDeletion>]`. `SpansProvider` and `ScoresProvider` store `deletions: Arc<[Arc<PendingDeletion>]>` (new constructor argument, passed by `Storage::query`) and pass it. `ScoresProvider::scan` builds a `RawScores` plan whose execution calls `scan_union` (`query/scores.rs`): give `RawScores` the same field and pass it through. Search every `ScanUnion {` literal (`grep -rn "ScanUnion {" crates/storage/src`) and fill the field; tests that build one pass `&[]`.
- In `scan_union`, split `files` into:
  - `clean`: files where `applicable(deletions, Some(file.deletions_applied_seq), file.min_ingest_time)` is empty;
  - `gated`: the others.
- `clean` files use `file_plan` as today.
- `gated` files use a new `gated_file_plan(scan, &gated)`:
  1. The scan projection `indices` is the requested projection (or every column when `None`) plus the indices of `key_columns(table)` that are missing, appended in schema order.
  2. `file_plan(&ScanUnion { projection: Some(&indices), ..*scan }, &gated, None)`: a copy of the `ScanUnion` with the extended projection (it is a struct of references; add `#[derive(Clone, Copy)]` to it if the copy syntax needs it), and **no limit**.
  3. `predicate` = `covered_predicate(table, &projected, deletions)` over **all** of the project's pending deletions, where `projected = Arc::new(scan.schema.project(&indices)?)` (applying a deletion to a file already clean of it removes nothing, and the cut-off is tested per row). `None` cannot happen here (the files were gated by a deletion with a term for this table); treat it as "no filter".
  4. `FilterExec::try_new(Arc::new(NotExpr::new(predicate)), scan)`.
  5. When key columns were appended, a `ProjectionExec` that keeps the first `requested.len()` columns, in order, with their names (`Column::new(name, i)` for `i < requested.len()`). With no appended column, skip it.
- The `both` / `side_limit` logic counts the gated plan as a side: with more than one side present, no side gets the limit, and a `GlobalLimitExec` on top applies it when `filters` is empty (as today for files plus memtable).

### Queries: file side, `traces` (`query/traces.rs` `build_range_plan`)

- `TracesProvider` stores `deletions: Arc<[Arc<PendingDeletion>]>` (new constructor argument). List mode rebuilds its inner plans at execution time from `ScanContext` (`query/traces.rs`, the phase-1 and phase-2 `build_range_plan` calls), so `ScanContext` holds the field too, and `build_range_plan` reads it from there.
- Split the `traces` files by `deletions_applied_seq` **only**: a `traces` file is gated when some pending deletion has `seq > file.deletions_applied_seq` (call `applicable(deletions, Some(file.deletions_applied_seq), i64::MIN)`). Don't use its `min_ingest_time`: a `traces` file's `min_ingest_time` is the smallest `last_ingest_time` of its partials, i.e. a per-trace **maximum**, so a trace with spans ingested before and after the cut-off would slip through. Flush, compaction and the rewriter give a `traces` file the `deletions_applied_seq` of its `spans` file.
- `gated` traces files: `file_plan(...)` as today (the input projection always holds `trace_id`), wrapped in `FilterExec::try_new(Arc::new(NotExpr::new(covered_predicate(FileTable::Traces, &input_schema, deletions)?)), plan)`. Add it to `plans` like the clean file plan. A `None` predicate (no deletion has trace ids) means no filter.
- No change to the merge.

### Tests (`deletion.rs` unit tests, and `lib.rs` or a new `src/deletion_tests.rs` module for the end-to-end ones)

Use `test_support::span_row` and `span_rows`. For scores, move `score_version` from the `compaction/job.rs` tests to `test_support.rs` as `pub(crate) fn score_row(id: u128, trace: u16, created_at: i64, updated_at: i64, ingest: i64, deleted: bool, name: &str) -> Score`: the body of `score_version`, with `created_at` taken from the argument and the trace id built from `trace` exactly like `span_row` builds it (`(trace + 1)` big-endian, repeated). Update the `compaction/job.rs` call sites to `score_row(id, 0, at(10, 0, 0), updated_at, ingest, deleted, name)`. Leave `compaction/merge.rs`'s own `score_version` helper alone. Add a `pub(crate) fn score_rows(scores: &[Score]) -> RecordBatch` next to `span_rows` if none exists (`scores_to_record_batch`, unwrapped). Set `session_id`, `user_id` or `target` on the returned value where a test needs them; a session score has `target: ScoreTarget::Session { .. }` (check the variant in `llmobs-core/src/score.rs`) and no trace id.

1. **`covered_predicate` unit tests**, on hand-built batches:
   - `Traces` scope on spans: a row of a listed trace with `ingest_time` equal to the cut-off is covered; one nanosecond later is not; another trace is not;
   - `User` scope on spans: `user_id` match is covered; a null `user_id` row is not, and the result has no null;
   - `session_ids` cover a score with that `session_id` and **no** span with it;
   - two deletions: a row covered by either is covered;
   - empty deletions: `None`; `Traces` table: only the trace-id term;
   - a session-target score (null `trace_id`, null `user_id`) with `ingest_time` before the cut-off is **not** covered by a `Traces` deletion, and the predicate's value for it is `false`, not null.
2. **Memtable:** append spans of traces 1 and 2 (ingest t), register a `Traces { [1] }` deletion with cut-off t: `SELECT` through the `spans` view returns only trace 2; a span of trace 1 appended afterwards with ingest t + 1 is visible.
3. **Files:** append traces 1 and 2, flush (`shutdown` + `open`), **then** register the deletion (registering before the flush would let M5.3's flush drop the rows, and this test would break one sub-phase later): the file still holds trace 1 (read the Parquet object), queries don't show it; the `traces` view has no row for trace 1; `SELECT COUNT(*)` and a `LIMIT 1` query are right; a session-target score flushed in the same project is still visible.
4. **User scope:** spans of user `u` hidden; spans with no user kept; a score with `session_id` in the scope hidden; a span with that `session_id` and another user still visible.
5. **Clean files are not filtered:** commit a raise of the file's `deletions_applied_seq` to the known sequence (with `commit_test_edit`), then `EXPLAIN` (through `test_support::rows` on `QueryContext`) shows no `FilterExec` whose predicate names `ingest_time`; before the raise, it does.
6. **Reopen:** after `shutdown` and `open`, the rows stay hidden.
7. **API:** idempotent `register_deletion`; `deletion_status` is `Pending`; an unknown id is `NotFound`; `Invalid` passes through.

**Commit:** `storage: hide deleted rows from queries`

---

## M5.3 — Flush drops covered rows

**Goal:** rows covered when a flush starts never reach a file, and every flushed file records the known sequence.

### `flush.rs`

- `flush_once`, right after the freeze: `let tombstones = Arc::clone(&params.published.load().tombstones);`. Pass it to `prepare`.
- `prepare(table, &view, anchors, &tombstones)`:
  - **Spans:** for each partition from `Anchors::partition`, before `dedup::latest_spans`: `deletions = applicable(&tombstones.for_project(key.project), None, min_ingest)` where `min_ingest` is the minimum `ingest_time` of `partition.batch` (`arrow::compute::min` on the column; an empty or all-null column is a `FlushError::dedup("missing ingest time")`). Non-empty: `drop_covered` with `covered_predicate(FileTable::Spans, &spans_schema(), &deletions)`. A partition left with no rows produces no file.
  - **Scores:** concatenate the partition's batches first (`concat_batches` against `scores_schema()`), then the same filter; then `write_file` with the one filtered batch. A partition left with no rows produces no file.
  - Predicate errors map to a new `FlushError::Deletion { reason: &'static str }` variant (`"building the deletion filter"`, `"applying the deletion filter"`). Add it to `FlushError` with `#[error("deletion filter: {reason}")]`.
- Every `FileMeta` the flush builds (`spans`, `traces`, `scores`) has `deletions_applied_seq = tombstones.known_seq()`. Pass it into `prepared_file`.
- **An empty flush commits.** Remove the `FlushError::dedup("empty flush")` return. When `files` is empty, skip staging, `FlushAfterStage`, `put_all` and `FlushAfterWrite`, and commit `CatalogEdit { add: [], remove: [], flushed_seq: Some((table, max_seq)), sources: [], raise: [] }`, publishing as usual (`publish_flush` drops the frozen memtable). Then `FlushAfterCommit`, then WAL truncation as usual.
- The anchor plan is committed as today, even when a covered row was its only row: an anchor entry without rows is harmless.

### Tests (`flush.rs`)

1. **Filtered flush:** append traces 1 and 2, register `Traces { [1] }`, flush: the new `spans` file holds only trace 2 (read the object), its `traces` file has one partial, both files have `deletions_applied_seq = 1`.
2. **All rows covered:** append only trace 1, register, flush: no new file; `flushed_seq` advanced; the WAL segments are truncated; `published_rows(Spans)` is 0.
3. **Scores:** a score on trace 1 is not flushed; a session-target score (null `trace_id`) appended before the registration is flushed.
4. **Race (pause point):** append trace 1; hold `FlushAfterStage`; register the deletion while the flush is parked; release. The file holds trace 1 (it was prepared before the registration) and has `deletions_applied_seq = 0`. A query does not show trace 1 (the file is gated).
5. Update any test that expected the `"empty flush"` error (search `empty flush` in tests).

**Commit:** `storage: drop deleted rows at flush`

---

## M5.4 — Compaction applies tombstones; the rewriter

**Goal:** compaction outputs never contain covered rows; a background rewriter erases covered rows from existing files and marks deletions applied. The milestone race test ("a compaction started before a deletion commits after it, and the rows stay gone") passes.

### Config (`config.rs`)

`pub struct DeletionConfig`, TOML `[storage.deletion]`, `#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]`, `#[serde(deny_unknown_fields)]`, every field `pub`, manual `Default`:

| Field | Type | Default | Valid range (`validate()`) |
|---|---|---|---|
| `max_delay` | `Duration` (humantime) | 24 h | 1 h … 30 days |

- `pub deletion: DeletionConfig` on `StorageConfig` with `#[serde(default)]`. Error field name `"storage.deletion.max_delay"`, rule `"must be between 1h and 30days"`.
- Tests: add `deletion` to `unknown_keys_fail_at_every_level`; both bounds to the bounds test; a parse test with `max_delay = "36h"`.

### Compaction applies tombstones (`compaction/job.rs`, `compaction/merge.rs`)

- At the start of `Compactor::run`, before step 1: `let tombstones = Arc::clone(&self.published.load().tombstones);`, `let deletions = tombstones.for_project(job.project);`, `let known = tombstones.known_seq();`.
- Build `covered = covered_predicate(job.table, &schema, &deletions)?` once per job, where `schema` is `spans_schema()` or `scores_schema()` (the merge output is the stored schema). Errors map to a new `CompactionError::Deletion { reason: &'static str }` (`#[error("deletion filter: {reason}")]`).
- `merge_slice` and `emit_chunk` take `covered: Option<&Arc<dyn PhysicalExpr>>`. In `emit_chunk`, before `latest_spans` / `latest_scores`: when `Some`, `chunk = drop_covered(&chunk, covered)?`; if it has no rows, return without writing and without pushing a partial.
- `merged.rows == 0` can now happen for `spans` too. Keep the `discard_output` + `continue`; update its comment ("every row of the slice was covered or, for a scores day close, deleted").
- Every output `FileMeta` (`spans`/`scores` and `traces`) has `deletions_applied_seq = known`.
- The deletions are those pending **when the job started**. A deletion registered later is not applied and `known` is lower than its `seq`, so the outputs stay candidates for the rewriter (spec 03 §7).

### The rewrite job (`compaction/plan.rs`, `compaction/job.rs`)

- `JobKind::Rewrite`: "rewrite one file without its covered rows". `kind_label` gives `"rewrite"`.
- `CompactionError::label()` (see § Conventions, Logs) and the job failure logs in `pass` switch from `%error` to `error = error.label()` in this sub-phase.
- A rewrite `Job` has `inputs = [the file]`, `whole_partition = false`, `drop_deleted = false`.
- `Compactor::run` changes for `Rewrite` only:
  - output level = `job.inputs[0].level` (paths via `Objects::table_file_path(.., level)`, `FileWriter::new(table, level, ..)`, `write_file(FileTable::Traces, level, ..)`, `FileMeta.level`);
  - output `fully_merged` = `job.inputs[0].fully_merged` (removing rows keeps a merged file merged), for the `spans`/`scores` output and its `traces` output.
  - Everything else is the same path: traces pairs and claims (step 1), slicing, the hooks, the edit, the report.
- `plan()` never returns `Rewrite` jobs; the deletion pass builds them.

### The deletion pass (`compaction/rewrite.rs`, new)

```rust
/// What one deletion pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeletionReport {
    /// Files rewritten without their covered rows.
    pub rewritten: u64,
    /// Files checked and found clean, whose `deletions_applied_seq` was raised.
    pub raised: u64,
    /// Deletions marked applied.
    pub applied: u64,
    /// Rewrites or raise commits that failed or lost a conflict; a later pass redoes them.
    pub failed: u64,
    /// Projects with a deletion applied in this pass, in order, without repeats.
    pub applied_projects: Vec<ProjectId>,
}
```

Re-export `DeletionReport` from `lib.rs`.

`Compactor` gains `max_delay: i64` (ns, from `config.deletion.max_delay`, saturating) and `overdue_logged: std::sync::Mutex<HashSet<Uuid>>`. `Compactor::new` already takes 7 arguments, and clippy's `too_many_arguments` (on, with `-D warnings`) refuses an 8th. So replace its `threads: usize` argument by:

```rust
/// How much the compactor may do at once, and how late a deletion may be.
#[derive(Debug, Clone, Copy)]
pub(crate) struct CompactorLimits {
    pub(crate) threads: usize,
    /// `storage.deletion.max_delay`, ns.
    pub(crate) max_delay: i64,
}
```

and store both fields on `Compactor` as today (`threads`) and new (`max_delay`).

**Where the code goes.** `deletion_pass` lives in `compaction/rewrite.rs` as a second `impl Compactor` block. A sibling module cannot see `job.rs`'s private items, so make these `pub(super)`: the `Compactor` fields `committer`, `objects`, `published`, `merge`, `policy`, `threads`, `claims`, `stopping`, `hooks`; `Compactor::run`; `ClaimGuard` and its fields `compactor` and `ids`; and in `merge.rs`: `file_scan`, `ordering_of` and `read_error`.

```rust
impl Compactor {
    /// One deletion pass at `now`.
    pub(crate) async fn deletion_pass(self: &Arc<Self>, now: i64)
        -> Result<DeletionReport, CompactionError>;
}
```

**Steps:**
1. `tombstones = self.published.load().tombstones`; `pending = tombstones.all()`. Empty: return `Ok(DeletionReport::default())`. `known = tombstones.known_seq()`.
2. **Overdue warning.** For each pending `d` with `d.registered_at.saturating_add(self.max_delay) < now` whose id is not in `overdue_logged`: insert it and log `warn!(deletion_id = %d.id, project = %d.project, seq = d.seq, "deletion is past storage.deletion.max_delay")`.
3. **Candidates.** `catalog = self.committer.catalog().await`. For each project with pending deletions (in `ProjectId` order), for `table` in `[Spans, Scores]`, for each active file of the project (`catalog.files(table, project, NaiveDate::MIN..=NaiveDate::MAX)`): the file is a candidate when `applicable(&tombstones.for_project(project), Some(file.deletions_applied_seq), file.min_ingest_time)` is not empty.
4. **Claim** under the `claims` mutex: skip candidates already claimed; claim the rest and wrap each in a `ClaimGuard` (the same guard as `pass`, released when its task ends).
5. **Check and rewrite**, with at most `threads` candidates at a time (`JoinSet` + `Semaphore`, as in `pass`). Per candidate `file`:
   - `deletions = applicable(..)` as in step 3; `covered = covered_predicate(file.table, &table_schema, &deletions)?` (not `None`: the list is not empty and each pending deletion has at least one term for `spans` and `scores`; treat `None` as clean).
   - **Check:** does the file hold a covered row? Build the plan:
     1. `ParquetSource::new(table_schema).with_pushdown_filters(true).with_predicate(covered_full)`, where `covered_full` is the predicate built against the full table schema;
     2. the scan: like `merge.rs`'s `file_scan(&[&file], source, &ordering_of(table, &schema)?)`, but projected to `key_columns(file.table)`. Give `file_scan` a fourth parameter `projection: Option<Vec<usize>>` passed to `FileScanConfigBuilder::with_projection_indices` (the merge passes `None`; propagate the `?`);
     3. `FilterExec::try_new(covered_projected, scan)`, where `covered_projected` is the predicate built against the projected schema (`table_schema.project(&key_indices)`);
     4. `GlobalLimitExec::new(filter, 0, Some(1))`; `execute_stream` with a `TaskContext` on `self.merge.runtime`, as `merge_slice` builds one; `true` as soon as a batch has a row.

     Statistics prune row groups on `ingest_time`; the bloom filters prune on `trace_id`, plus `user_id` and `session_id` for `spans` (the `scores` files have bloom filters on `trace_id` and `score_id` only). Read errors are `CompactionError::Read`.
   - **Covered:** `self.run(Job { table: file.table, project: file.project, day: file.day, kind: JobKind::Rewrite, inputs: vec![file], whole_partition: false, drop_deleted: false }, policy)`, where `policy` is read from `self.policy` at the start of the pass. `Ok` adds to `rewritten`; `Err` adds to `failed` and is logged like a failed compaction job.
   - **Clean:** the task returns the file id for the raise.
6. **Raise.** If any file came back clean: one `CatalogEdit { raise: [(id, known)] for each clean file whose `deletions_applied_seq < known`, every other field empty }`, committed with `publish_catalog`. `Ok` adds the count to `raised`. A conflict (a file was compacted or rewritten meanwhile) adds 1 to `failed` and is logged at `info!`; the next pass checks again.
7. **Release** every claim (drop the guards).
8. **Applied.** Take **one** snapshot: `state = self.published.load()`, and read the catalog **and** the memtables from it (`state.catalog`, `state.spans`, `state.scores`). Never mix `committer.catalog()` with a separate `published.load()`: a flush prepared before the registration (its file has a lower `deletions_applied_seq`) can commit between two reads, so the older catalog would lack its file while the newer state already lacks its memtable batch, and the deletion would be marked applied with its rows still in that file. A flush commit publishes its file and drops the batch in one `Published::replace`, so one snapshot sees one or the other. For each pending `d` in `seq` order, `d` is applied when:
   - every active `spans` and `scores` file of `d.project` in `state.catalog` with `min_ingest_time <= d.cutoff` has `deletions_applied_seq >= d.seq`;
   - no batch of `d.project` in `state.spans` or `state.scores` has `min_ingest_time <= d.cutoff`.

   Then `self.committer.mark_applied(d.project, d.id, now, |t| publish_tombstones(&self.published, t))`.
   - `Ok(true)`: add 1 to `applied`; push `d.project` onto `applied_projects` unless the list already `contains` it; log `info!(deletion_id = %d.id, project = %d.project, seq = d.seq, "deletion applied")`; remove `d.id` from `overdue_logged`.
   - `Ok(false)`: nothing (another call applied it).
   - `Err(error)`: add 1 to `failed`, log `warn!(deletion_id = %d.id, "marking a deletion applied failed")` (the catalog error text holds no scope value, but keep the line short), and go on with the next deletion. The pass never returns `Err` for it.
9. Return the report.

A deletion whose rows are all in the memtable becomes applied after the next flush, because that flush drops them (M5.3) and leaves no batch behind.

### The task and the hidden API

- `compaction/task.rs`: after each `pass`, run `compactor.deletion_pass(now_ns()).await` (a fresh `now_ns()`). Log the report at `debug!` when `rewritten + raised + applied + failed > 0`, an `Err` at `warn!`. `stop` and the `stopping` flag behave as for `pass`: a rewrite job sees `stopping` and returns `Stopped`.
- `Storage`:

```rust
/// One deletion pass at `now` (ns), run to completion: rewrites, raises, and
/// marks applied. The background task runs one after each compaction pass.
#[doc(hidden)]
pub async fn apply_deletions(&self, now: i64) -> Result<DeletionReport, CompactionError>;
```

### Tests (`compaction/rewrite.rs`, through `Storage`)

Use the M4 setup (`config_in`, `compaction.interval = 1h`, `flush_cycle` to make L0 files, `set_policy`).

1. **Compaction drops covered rows:** 3 L0 files with traces 1 and 2; register `Traces { [1] }` after the flushes; `compact` (an L0 merge): the L1 output holds no trace 1 row and has `deletions_applied_seq = 1`; its `traces` output too; the deletion is then applied by `apply_deletions` without a rewrite (`rewritten = 0`, `applied = 1`).
2. **Rewrite of an L0 and an L1 file:** files at both levels hold trace 1; register; `apply_deletions`: `rewritten = 2`, `applied = 1`; each output keeps its input's level and `fully_merged`; Parquet objects hold no trace 1 row; each new `spans` file has one `traces` file (reopen passes `check_file_sources`); `deletion_status` is `Applied`; the published tombstones are empty.
3. **Clean candidate:** a file with `min_ingest_time` before the cut-off but no covered row: `apply_deletions` gives `raised = 1`, `rewritten = 0`; the file id is unchanged and its `deletions_applied_seq` is the known sequence.
4. **Memtable only:** the covered rows are only in the memtable: `apply_deletions` gives `applied = 0`; after a flush, `apply_deletions` gives `applied = 1`.
5. **Every row covered:** a file whose rows are all trace 1: the rewrite removes it and its `traces` file and adds nothing.
6. **The milestone race:** 2 L0 files with trace 1; hold `CompactionAfterWrite`; start `compact` in a task (`multi_thread` runtime, as the M4 race tests); register `Traces { [1] }`; release. The L1 output holds trace 1 rows (read the object) and has `deletions_applied_seq = 0`. A query does not show trace 1. `apply_deletions` rewrites it (`rewritten = 1`), then `applied = 1`. No Parquet object of an active file holds trace 1.
7. **Claims:** hold `CompactionAfterStage` on a compaction job over the candidate files; `apply_deletions` in another task returns with `rewritten = 0, applied = 0` and leaves the claimed files alone; release; the next `apply_deletions` applies the deletion.
8. **User scope, scores:** a `User` deletion with a session id: the rewriter removes the user's spans, the session's scores, and no span of another user in that session.
9. **Overdue:** a unit test on the overdue step with `max_delay` 1 h and `now` past it: the id is logged once (assert on `overdue_logged`, not on log output), and not again on a second pass.
10. **No early "applied" during a flush:** append trace 1 (memtable only); hold `FlushAfterWrite` (the flush was prepared before the registration, so its file will keep trace 1 with `deletions_applied_seq = 0`); register `Traces { [1] }`; in another task run `apply_deletions(now)` while the flush is held, then release the flush and run `apply_deletions(now)` again. The deletion is never `Applied` while an active file still holds a trace 1 row: after the first two steps it is `Pending`, and a later `apply_deletions` rewrites the flushed file and only then applies it. Check after each `apply_deletions` call by reading every active `spans` object.
11. **Session score survives compaction:** a session-target score and a score on trace 1, both in L0 files; register `Traces { [1] }`; an L0 merge keeps the session score and drops the other.
12. **Config tests** above.

**Commit:** `storage: rewrite files for pending deletions`
