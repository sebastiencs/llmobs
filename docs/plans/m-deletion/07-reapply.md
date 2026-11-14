# M7 — Re-apply deletions after a restore

Read [README.md](README.md) first, especially § Design › Audit (D-M-9).

**Goal:** `llmobs admin reapply-deletions --from <data_dir>` copies, from another data directory's `meta.db`, every deletion the current `meta.db` does not have, with its scope, as an **open** row. The running (or next) `llmobs serve` then registers or drops it again through M3's worker.

This is how a backup restore keeps erasures done after the backup. Before restoring, the operator keeps the old data directory (or a copy of its `meta.db`). After the restore, they start `llmobs serve` once if the backup is from an older version (both databases must be at this binary's schema), then run this command with `--from` pointing at the old directory. Spec 12 § Backups says so (the plan commit updated it, and spec 09 § CLI lists the command).

The command opens only `meta.db` files, never storage, so it works whether the server runs or not.

**Edit only:**
- `crates/server/src/meta/deletions.rs` (`all_deletions`, `insert_copy`);
- `crates/server/src/meta/projects.rs`, `crates/server/src/meta/orgs.rs` only if `insert_copy` needs a helper there (it may run the two `UPDATE`s below inline);
- `crates/server/src/admin.rs` (`reapply_deletions`, `ReapplyReport`);
- `crates/cli/src/main.rs` (`AdminCommand::ReapplyDeletions`).

**Commit:** `Add admin reapply-deletions`

## Read first

- `crates/server/src/admin.rs`: `AdminError`, `open_meta`, `map_meta`, `list_users`, and the tests' `ready_config`.
- `crates/cli/src/main.rs`: `AdminCommand`, `admin_command`, `load_config`.
- `crates/server/src/meta/mod.rs`: `MetaDb::open_existing(data_dir)`, which refuses a missing file (`NotInitialized`) and another schema version (`SchemaMismatch`).
- M1's `meta/deletions.rs`; README § Design › Lifecycle (how the worker treats a `resolving` row with and without `trace_count`, and a `pending` drop with `handed_over_at` NULL). The copy relies on M3's worker: a `resolving` row whose `trace_count` is set is registered directly with its stored scope (`trace_ids`/`filter` with an empty scope go straight to `applied`; `user` always registers), and a `pending` drop is dropped again (`drop_project` is idempotent).

## Store

`meta/deletions.rs`:

```rust
/// Every row, oldest first (`created_at, id`).
pub(crate) async fn all_deletions(pool: &SqlitePool) -> Result<Vec<DeletionRow>, MetaError>;

/// Inserts `row` from another `meta.db` as an open row, with its scope. Returns `false`
/// (and writes nothing) when a row with that id exists.
///
/// The new row keeps `id`, `org_id`, `project_id`, `kind`, `user_id`, `request`, the
/// actor and `created_at` (the cut-off). Its state:
/// - `failed` rows: copied as `failed` with their `error` (audit only, nothing to redo);
/// - `trace_ids`, `filter`, `user`: `resolving`, with `trace_count` and `session_count`
///   as in the source (so a resolved scope is registered as is, and an unresolved one is
///   resolved again), `handed_over_at` and `applied_at` NULL;
/// - `project`, `org`: `pending`, `handed_over_at` and `applied_at` NULL; in the same
///   transaction the project (for `project`) or the org and every listed project (for
///   `org`) is set to `state = 'deleting'` when it exists, so it is hidden at once, as
///   when the drop was first requested.
pub(crate) async fn insert_copy(
    conn: &mut SqliteConnection,
    row: &DeletionRow,
    traces: &[TraceId],
    sessions: &[String],
    projects: &[ProjectId],
) -> Result<bool, MetaError>;
```

`insert_copy`:
1. `SELECT 1 FROM deletions WHERE id = ?`. If found → `Ok(false)`.
2. One `INSERT` that lists every column explicitly. Use the same serialization as `insert_deletion` for the enums and `request`.
3. `insert_traces`, `insert_sessions`, `insert_projects` with the given slices.
4. For a `project` row that is not `failed`: `UPDATE projects SET state = 'deleting', updated_at = ? WHERE id = ?` (`updated_at` = the row's `created_at`). For an `org` row: the same on `orgs` for `org_id`, and on `projects` for each id in `projects`. A missing project or org changes nothing.

## Admin function

`admin.rs`:

```rust
/// What `reapply_deletions` did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReapplyReport {
    /// Rows copied as open rows, to be applied again.
    pub reopened: u64,
    /// `failed` rows copied for the audit trail.
    pub failed_copied: u64,
    /// Rows the current `meta.db` already had.
    pub present: u64,
}

/// Copies the deletions of `<from>/meta.db` that `config`'s `meta.db` lacks (D-M-9).
/// Both databases must be at this binary's schema version.
///
/// # Errors
///
/// [`AdminError::NotInitialized`] or [`AdminError::Schema`] for either database (the
/// source's are reported the same way), [`AdminError::Database`] for a query failure.
pub async fn reapply_deletions(config: &ServerConfig, from: &Path) -> Result<ReapplyReport, AdminError>;
```

Body: open both, run a private `async fn copy_all(target: &MetaDb, source: &MetaDb) -> Result<ReapplyReport, MetaError>`, then **always** close both before returning its result (as `list_users` runs its work, closes, then returns):
1. `let target = open_meta(config).await?;`
2. `let source = match MetaDb::open_existing(from).await { Ok(db) => db, Err(error) => { target.close().await; return Err(map_meta(error)); } };`
3. `let result = copy_all(&target, &source).await; source.close().await; target.close().await; result.map_err(map_meta)`.

`copy_all`:
1. `let rows = all_deletions(source.reader()).await?;`
2. For each row:
   - read its scope from the source: `scope_traces`, `scope_sessions` and `scope_projects`;
   - then `target.write(|conn| insert_copy(conn, &row, &traces, &sessions, &projects))`, one transaction per row, so an interrupted run leaves whole rows.
   - Count each row: `false` → `present`; `Failed` → `failed_copied`; otherwise `reopened`.
3. Return the report.

`open_existing` opens the source read-write (WAL), so a source on read-only media fails with `AdminError::Database`; the doc comment of `reapply_deletions` says to copy the old directory somewhere writable first. Pointing `--from` at the current data directory copies nothing (every id is present); no special case.

## CLI

`crates/cli/src/main.rs`, `AdminCommand`:

```rust
/// Re-open the deletions of another data directory that this one lacks (after a backup restore).
/// Both `meta.db` files must be at this binary's schema; run `llmobs serve` once on a restored older backup first.
ReapplyDeletions {
    /// The data directory whose `meta.db` holds the deletions to re-apply (for example the
    /// directory as it was before the restore).
    #[arg(long)]
    from: PathBuf,
    /// Config file. Overrides `LLMOBS_CONFIG`.
    #[arg(long)]
    config: Option<PathBuf>,
},
```

In `admin_command`:

```rust
AdminCommand::ReapplyDeletions { from, config } => {
    let loaded = load_config(config.as_deref())?;
    let report = admin::reapply_deletions(&loaded, &from).await.context("re-applying deletions")?;
    writeln!(
        io::stdout().lock(),
        "Re-opened {} deletions ({} failed ones copied for the record, {} already present). `llmobs serve` applies them.",
        report.reopened, report.failed_copied, report.present
    )?;
}
```

Change the `Admin` variant's doc comment from "Manage users in this data directory." to "Admin operations on this data directory.". There is no `--help` snapshot to update.

CLI tests, in `crates/cli/src/main.rs`'s test module, modeled on the existing `list_users` missing-database test there: `reapply_deletions_on_a_missing_database_names_serve` (the error text mentions `llmobs serve`), and a parse test that `llmobs admin reapply-deletions` without `--from` is a clap error.

## Tests

In `admin.rs` tests, using `ready_config()` for the target and a second `tempfile::tempdir()` with `MetaDb::open(dir)` for the source. Insert source rows with M1's `insert_deletion`, `insert_traces`, `insert_sessions`, `insert_projects`, `store_counts`, `mark_handed_over`, `mark_applied` and `mark_failed`, then `close()` the source before calling `reapply_deletions`.

1. `copies_missing_rows_as_open_rows`. Source rows:
   - (a) `trace_ids`, resolved (`trace_count` 2), handed over and applied, scope `[T1, T2]`;
   - (b) `user` `u1`, resolved with `trace_count` 1 and `session_count` 1, `pending`, scope `[T3]`, sessions `["s1"]`;
   - (c) `filter`, still `resolving` (`trace_count` NULL), `request.filter` set;
   - (d) `project`, applied;
   - (e) `org`, applied, `deletion_projects` `[P1, P2]`;
   - (f) `trace_ids`, `failed` (`ResolutionFailed`).

   The report is `{reopened: 5, failed_copied: 1, present: 0}`. In the target:
   - (a) `resolving`, `trace_count == Some(2)`, `handed_over_at` and `applied_at` `None`, scope `[T1, T2]`;
   - (b) `resolving` with counts 1/1, scope and sessions copied;
   - (c) `resolving`, `trace_count` `None`, `request` equal to the source's;
   - (b) also has `user_id == Some("u1")`, `request` equal to the source's, and `scope_sessions == ["s1"]`;
   - (d) and (e) `pending`, `handed_over_at` `None`, (e) with `deletion_projects` `[P1, P2]`;
   - before the copy, insert into the target an org `O` (`insert_org`) with projects `P1`, `P2` (`insert_project`, use these as (e)'s `org_id` and projects), and a project `P0` in another org as (d)'s `project_id`: after the copy, `P0`, `P1`, `P2` and `O` have `state == "deleting"`; (f) being `failed` changes no state;
   - (f) `failed` with `error == Some(ResolutionFailed)`;
   - every `created_at` and actor unchanged.
2. `second_run_copies_nothing`: run again → `{reopened: 0, failed_copied: 0, present: 6}`.
3. `rows_already_present_are_left_alone`: the target already has row (a)'s id as `applied` → it stays `applied`, and the report is `{reopened: 4, failed_copied: 1, present: 1}`.
4. `missing_source_is_not_initialized`: `from` is an empty temp dir → `AdminError::NotInitialized`.
5. `source_at_another_schema_is_refused`: a source `meta.db` with an extra future row in `_sqlx_migrations` (as `schema_mismatch_names_the_versions` does, with `expected_version() + 1`) → `AdminError::Schema { .. }`.
6. `copied_rows_are_applied_by_the_worker`:
   - `let app = TestApp::new().await; let seeded = app.seed().await;`. Commit trace A to the seeded project with `ingest_time = now`, as in M3's tests.
   - Commit trace A to the seeded project with `ingest_time = app.state.clock.now_ns()` set on the span (M3's tests explain why), then advance the clock 1 s. In a source data dir, insert a `trace_ids` row for the seeded project (`org_id` = `seeded.org_id`, actor any `Uuid::now_v7()`; there are no foreign keys) with scope `[A]`, `store_counts(1)`, `created_at = app.state.clock.now_ns()`, and mark it applied. This is the deletion "done after the backup".
   - `reapply_deletions(&app.state.config, source_dir)` → `reopened == 1`. This is the target opened while `TestApp` holds it, which `create_user_works_while_the_server_holds_the_directory` shows is fine.
   - `crate::deletion::run_once(&app.state)` → the row is `pending`, and `GET` trace A is `404`.

## Done when

- [ ] `llmobs admin reapply-deletions --from <dir>` exists and prints the report.
- [ ] The 6 admin tests and the 2 CLI tests pass; `just check` is green; one commit `Add admin reapply-deletions`.
