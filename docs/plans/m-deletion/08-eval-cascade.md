# M8 — Evaluation cascade

Read [README.md](README.md) first, especially § Design › Evaluation traces and Cascades, and § Coordination (I).

**Prerequisite — check before anything else.** Track I's M3 (the job queue and the executor) must be merged:
- some file in `crates/server/migrations/meta/` contains `CREATE TABLE eval_jobs`;
- `crates/server/src/eval/` exists and holds the job executor (I's design names it `eval/execute.rs`).

If either is missing, stop: commit nothing, and report "M8 waits for track I's M3".

**Goal:** a deletion reaches the evaluation side of `meta.db` and I's executor:
1. **Cascade.** In the transaction that stores a resolved scope (M3's `resolve`), delete the `eval_jobs` rows of the scope's traces that are not `running`. They hold no payload, but a queued job would later load a deleted trace for nothing, and its row names the trace.
2. **Guard.** I's executor does not commit a judge trace or an evaluator score about a trace the deletion guard blocks (D-M-6), and a manual run on such a trace is refused.

**Edit only:**
- `crates/server/src/deletion/cascade.rs` (new), `crates/server/src/deletion/mod.rs` (declare it), `crates/server/src/deletion/resolve.rs` (call it);
- `crates/server/src/deletion/guard.rs` (drop the `dead_code` allowance now that it has a caller);
- the I files named in § Guard below, and only at the points named.

**Commit:** `Cascade deletions to evaluation jobs`

## Read first

- M3's `deletion/resolve.rs` (the scope transaction) and `deletion/guard.rs` (`blocks`).
- The `eval_jobs` migration: column names (`project_id`, `trace_id` BLOB, `status`), and the status values. I's design (`docs/plans/i-online-eval/00-design.md` § Tables and D-I-12) lists `queued`, `running`, `done`, `failed`, `skipped`.
- I's executor, `crates/server/src/eval/execute.rs` (or the module that runs a job): find
  - (a) where a job loads its target trace (it knows `project_id`, `trace_id`, and after the load the target's `user_id`);
  - (b) the first point after the judge call where it **commits** something: the judge trace through `state.ingest.commit`, or the evaluator score through `scores::write` / storage `append_scores`;
  - (c) how a job ends `skipped` with a reason (D-I-12; the reason column may be `error_class`).
- I's manual-run route (`POST /api/v1/projects/{project}/evaluators/{id}/run`, D-I-21 / D-I-25).

## Cascade

`crates/server/src/deletion/cascade.rs`:

```rust
//! `meta.db` rows that follow a deletion's scope (spec 03 § Cascades, D-M-7).
//!
//! Called in the transaction that stores a resolved scope. Track J adds its dataset-item
//! deletion here (README § Coordination).

use llmobs_core::ids::{ProjectId, TraceId};
use sqlx::SqliteConnection;

use crate::meta::MetaError;

/// Deletes the `eval_jobs` rows of `project` whose `trace_id` is in `traces` and whose
/// status is not `running` (a running job is stopped by the guard instead). Returns the
/// number of rows deleted. Runs in chunks of 500 ids.
pub(crate) async fn traces_deleted(
    conn: &mut SqliteConnection,
    project: ProjectId,
    traces: &[TraceId],
) -> Result<u64, MetaError>;
```

SQL per chunk, built with `sqlx::QueryBuilder<Sqlite>`: `DELETE FROM eval_jobs WHERE project_id = ? AND status != 'running' AND trace_id IN (?, ?, …)`, binding `project.to_string()` and each `trace.as_bytes().as_slice()`. An empty `traces` does nothing. Use the real column names from the migration; if `trace_id` is stored as text there (it is a BLOB in I's design), bind `trace.to_hex()` instead and say so in your report.

`deletion/resolve.rs`: in the scope `meta.write`, after `insert_sessions` and before `store_counts`, call `crate::deletion::cascade::traces_deleted(conn, project, &scope).await?` (the `scope` vector, evaluation traces included). Log nothing per row; M3's `deletion registered` event stays the only one.

## Guard

In I's executor, at point (b) — before the first commit of a judge trace or score for a job:

```rust
if state.deletions.guard.blocks(job.project_id, job.trace_id, target_user_id.as_deref(), state.clock.now_ns()) {
    // D-M-6: the target was deleted after the job loaded it.
    return <end the job as skipped, reason "deleted">;
}
```

- `job.project_id` / `job.trace_id` / `target_user_id` are whatever names the executor uses for the job's project, its target trace id (`TraceId`) and the `user_id` of the loaded target trace (the root span's `user_id`, or `None`).
- "End the job as skipped" uses I's existing skip path (c). If I's skip reasons are a closed enum, add a variant `Deleted` (text `deleted`) to it, with its doc comment `/// The target trace was deleted (D-M-6).`; if they are free text, use `"deleted"`.
- Nothing is committed for a skipped job: no judge trace, no score, no usage (`usage_daily.judge_*`).

In I's manual-run handler, after it resolves the target trace and before it enqueues: if `state.deletions.guard.blocks(project, trace, None, now)` → `ApiError::NotFound` (the trace is gone from the caller's point of view).

Remove `#![cfg_attr(not(test), allow(dead_code))]` from `deletion/guard.rs` (now `blocks` has a caller). If clippy then reports another unused item there, delete that item rather than re-adding the allowance.

## Tests

`deletion/cascade.rs` tests (with `TestApp`, inserting `eval_jobs` rows directly with the migration's columns; set every `NOT NULL` column, using an evaluator row inserted the way I's own tests insert one — copy their helper call):

1. `deletes_jobs_of_deleted_traces_except_running`: jobs for trace A in `queued`, `done`, `running`; a `queued` job for trace B; a `queued` job for trace A in another project. `traces_deleted(project, [A])` returns 2; the `running` job of A, the job of B and the other project's job remain.
2. `empty_scope_deletes_nothing`: returns 0.

`deletion/mod.rs` tests (extending M3's):

3. `resolution_cascades_to_eval_jobs`: a `queued` job for trace A, then a `trace_ids` deletion of A and `run_once` → the job row is gone, and the deletion is `pending`.

I's executor tests (next to I's existing executor tests, reusing their setup, which runs a job against a stub judge or the mock provider):

4. `a_job_whose_target_was_deleted_is_skipped`: load a job for trace A; add A to the guard (`state.deletions.guard.add_traces(project, &[A], now)`) after the job is claimed but before it commits — if I's test harness cannot pause there, add A to the guard before running the job (the check runs before the commit either way). The job ends `skipped` with reason `deleted`; no score exists for it (storage `scores_by_id` or the scores list), and no judge trace (`include_eval` trace list of the project is empty).
5. `a_job_of_a_deleted_user_is_skipped`: same with `add_user(project, "u1", now)` and a target trace whose root span has `user_id = "u1"`.
6. `manual_run_on_a_deleted_trace_is_404`: guard holds A → `POST …/evaluators/{id}/run` for A answers `404`.

## Done when

- [ ] `cascade::traces_deleted` runs in the scope transaction; `running` jobs are left to the guard.
- [ ] I's executor checks the guard before its first commit, and the manual run refuses a blocked trace.
- [ ] The 6 tests pass; `just check` is green; one commit `Cascade deletions to evaluation jobs`.
- [ ] Report the I function and line where each guard call went, the skip reason form you used, and any column name that differed from this file.
