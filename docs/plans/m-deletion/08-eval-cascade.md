# M8 — Evaluation cascade

Read [README.md](README.md) first, especially § Design › Evaluation traces and Cascades, and § Coordination (I).

**Prerequisite — check before anything else.** Track I's M3 (the job queue and the executor) must be merged:
- some file in `crates/server/migrations/meta/` contains `CREATE TABLE eval_jobs` (`grep -l 'CREATE TABLE eval_jobs' crates/server/migrations/meta/`);
- `crates/server/src/meta/eval_jobs.rs` exists;
- `crates/server/src/eval/dispatch.rs` exists and `grep -n 'fn run_job' crates/server/src/eval/execute.rs` finds the job runner.

If any is missing, stop: commit nothing, and report "M8 waits for track I's M3".

**Goal:** a deletion reaches the evaluation side of `meta.db` and I's executor:
1. **Cascade.** In the transaction that stores a resolved scope (M3's `resolve`), delete the `eval_jobs` rows of the scope's traces that are not `running`.
2. **Guard.** I's job runner stops a job about a guarded trace before the judge call (nothing leaves the server) and again before it commits anything; I's test run and manual run refuse a guarded trace. The guard semantics are M3's `DeletionGuard::blocks` (D-M-6): trace-id entries always block; a user entry blocks only a trace whose data was ingested at or before that deletion's cut-off.

**Edit only:**
- `crates/server/src/deletion/cascade.rs` (new), `crates/server/src/deletion/mod.rs` (declare it), `crates/server/src/deletion/resolve.rs` (call it);
- `crates/server/src/deletion/guard.rs` (drop the `dead_code` allowance);
- `crates/server/src/meta/eval_jobs.rs` (one function, below);
- `crates/server/src/eval/execute.rs` (the job runner and its result type), `crates/server/src/eval/dispatch.rs` and `crates/server/src/eval/budget.rs` (only the handling of the new outcome), `crates/server/src/eval/test_run.rs`, and the route file that holds the manual-run handler (`grep -rln 'evaluators/{id}/run' crates/server/src/routes`; list it in your report) — only at the points named;
- the `error_class` value set, if I enumerates it: `grep -rn 'error_class' crates/server/src/api web/src` — when an API type, the OpenAPI schema or the web code lists the allowed values, add `deleted` there (and run `just openapi`); otherwise report that none needed it.

**Commit:** `Cascade deletions to evaluation jobs`

## Read first

- M3's `deletion/resolve.rs` (the scope transaction) and `deletion/guard.rs` (`blocks(project, trace, users, newest_ingest_ns, now)`).
- I's design `docs/plans/i-online-eval/00-design.md`: § Tables (`eval_jobs`: `id`, `project_id TEXT`, `kind`, `priority`, `rule_id`, `evaluator_id`, `trace_id BLOB`, `span_id`, `trace_start_ns`, `dedup_key UNIQUE`, `status`, `error_class`, `error`, `judge_trace_id`, `score_id`, usage columns, `created_at`, `updated_at`, `finished_at`), D-I-12 (job states; `finish(job, result)` is fenced on `status = 'running' AND claims = …`; skip reasons such as `budget` and `expired` live in `error_class`), D-I-13, D-I-15 (cost reservation, then actual), D-I-18, D-I-21 (test runs), D-I-22 (manual runs), D-I-23 (trace loading).
- I's code: `meta/eval_jobs.rs`; `eval/execute.rs` — `run_job`, the result type it hands to `finish`, the steps (load the trace, guards, resolve variables, reserve the cost, call the judge, commit the judge trace and the score, record usage); `eval/test_run.rs`.
- `llmobs_core::span::Span`: `user_id: Option<String>`, `ingest_time: i64`.

## Cascade

`crates/server/src/meta/eval_jobs.rs`, next to I's other functions:

```rust
/// Deletes the jobs of `project` whose `trace_id` is in `traces`, except `running` ones
/// (D-M-6: those are stopped by the deletion guard). Returns the rows deleted. Runs in
/// chunks of 500 ids.
pub(crate) async fn delete_for_traces(conn: &mut SqliteConnection, project: ProjectId, traces: &[TraceId]) -> Result<u64, MetaError>;
```

`eval_jobs` has no index on `trace_id`, and a scope can hold 100,000 traces, so scan once instead of once per chunk:
1. An empty `traces` returns `Ok(0)` at once.
2. `let wanted: HashSet<TraceId> = traces.iter().copied().collect();`
3. `SELECT id, trace_id FROM eval_jobs WHERE project_id = ? AND status <> 'running'` (bind `project.to_string()`), keeping the ids whose `TraceId::from_slice(&trace_id)` is in `wanted` (a blob that does not decode is skipped, never a panic).
4. `DELETE FROM eval_jobs WHERE id IN (?, …)` in chunks of 500 (`sqlx::QueryBuilder<Sqlite>`), summing `rows_affected()`.

`crates/server/src/deletion/cascade.rs`:

```rust
//! `meta.db` rows that follow a deletion's scope (spec 03 § Cascades, D-M-7).
//!
//! Called in the transaction that stores a resolved scope. Track J adds its dataset-item
//! deletion here (README § Coordination).

/// Runs every cascade of a stored scope. Returns the number of `eval_jobs` rows deleted.
/// Today it has one statement; it exists so that track J adds its dataset-item deletion here
/// instead of in `resolve.rs` (README § Coordination).
pub(crate) async fn traces_deleted(conn: &mut SqliteConnection, project: ProjectId, traces: &[TraceId]) -> Result<u64, MetaError> {
    crate::meta::eval_jobs::delete_for_traces(conn, project, traces).await
}
```

`deletion/resolve.rs`: in the scope `meta.write`, after `store_counts` and only when it returned `true`, call `crate::deletion::cascade::traces_deleted(conn, project, &scope).await?` (`scope` includes the evaluation traces). Inside the same closure, before the cascade, call `guard.add_traces(project, &scope, row.created_at)` on a clone of the guard handle (pass `&state.deletions.guard` into the closure by reference if the closure's lifetime allows, else call it right before `meta.write`); it is idempotent and harmless if the transaction rolls back, and it closes the gap in which a running job could pass its last check. M3's `register` adds the same entries again.

`running` jobs keep their rows, as do the `skipped` rows they become, for up to 7 days (D-I-28 prunes them); they hold ids, not payload. Accepted.

## Guard in the job runner

A new outcome in the type `run_job` returns to `finish` (in `eval/dispatch.rs`; name it as the type's existing variants are named, e.g. `JobResult::Deleted { usage }`), carrying `usage: Option<…>` of the same type `finish` already takes for `done`/`failed` (tokens, cost, latency). `finish` writes it with the usual fenced `UPDATE`: `status = 'skipped'`, `error_class = 'deleted'`, `error = NULL`, `finished_at = now`, `updated_at = now`, and, when `usage` is `Some`, also `input_tokens`, `output_tokens`, `cost_usd`, `latency_ms` and the `rule_spend` update, as the failure-after-call path does. Checks 0 and A return `usage: None`. It does not pause the rule, does not touch `online_rules` or `llm_connections`, does not count in the breaker window (D-I-14), and releases any budget reservation the way a failure before the call does.

Checks in `run_job`, each with a `// D-M-6` comment:

```rust
let now = state.clock.now_ns();
// D-M-6: a trace deleted after this job was enqueued.
if state.deletions.guard.blocks(job.project_id, job.trace_id, &[], 0, now) { return <Deleted>; }
```

- **Check 0**, at the very start of `run_job`, before loading: trace id only (`&[]`, `0`). It catches jobs re-queued from `running` (retries, janitor, restart) that the cascade left.
- **Check A**, right after the trace is loaded (D-I-23) and before the cost reservation and the judge call: compute `users` = the distinct non-empty `user_id`s of the loaded spans (`Vec<&str>`), and `newest` = the largest `ingest_time` of the loaded spans; `if state.deletions.guard.blocks(project, trace, &users, newest, now) { return <Deleted>; }`. Nothing was reserved or sent yet.
- **Check B**, immediately before the judge-trace commit, and again before each retry of that write (D-I-13): the same call with the same `users` and `newest`. If it blocks, the judge was already paid: return `Deleted { usage: Some(actual) }` (the job row and `rule_spend` get the actual cost) and call the judge usage counter as the success path does (D-I-15, D-I-18: usage counts either way); commit no judge trace and no score.
- **Check B2**, immediately before `scores::write` (I commits the judge trace first, then the score), and before each retry of it: the same call. If it blocks now, the judge trace is already stored (it targets the deleted trace and will be found by a later deletion's eval-trace lookup only if one is made; note it in the report); write no score and return `Deleted { usage: Some(actual) }`.

Use the executor's real names for the job's project, trace id and loaded spans. If check 0 turns out to be dead code in I's structure (every path already loads first), keep checks A and B and say so in the report.

## Guard in test runs and manual runs

- `eval/test_run.rs` (D-I-21), for a test run whose target is a trace: right after the target trace is loaded and before the judge call, the same check as A; and again immediately before it commits the test judge trace (it copies target data). When either blocks, answer `ApiError::NotFound` (the trace is gone from the caller's point of view) and commit nothing. A test run on literal variables has no target and is not checked.
- The manual-run handler, `POST /api/v1/projects/{project}/evaluators/{id}/run` (D-I-22): after the body's `trace_id` is decoded and before the job is inserted, `let now = state.clock.now_ns(); if state.deletions.guard.blocks(project_id, trace_id, &[], 0, now) { return Err(ApiError::NotFound) }`. It does not load the trace; check A in the runner covers end users.

Remove `#![cfg_attr(not(test), allow(dead_code))]` from `deletion/guard.rs`. If clippy then reports an unused item there, delete the item rather than restoring the allowance.

## Tests

`meta/eval_jobs.rs` and `deletion/cascade.rs` tests. Create jobs with I's own enqueue or insert function from `meta/eval_jobs.rs` (with a real project and evaluator row, built the way I's `meta/eval_jobs.rs` tests build them; copy their helper), then `UPDATE eval_jobs SET status = ? WHERE id = ?` for `running` and `done`. A second project and evaluator are needed for the "other project" job.

1. `deletes_jobs_of_deleted_traces_except_running`: jobs for trace A in `queued`, `done`, `running`; a `queued` job for trace B; a `queued` job for trace A in another project. `delete_for_traces(project, [A])` returns 2; the `running` job of A, the job of B and the other project's job remain.
2. `empty_scope_deletes_nothing`: returns 0.
3. `resolution_cascades_to_eval_jobs` (in `deletion/mod.rs` tests): a `queued` job for trace A (A committed with `ingest_time = now`, as in M3), then a `trace_ids` deletion of A and `run_once` → the job row is gone, the deletion is `pending`.

Spans in tests 3–7 are committed with explicit `ingest_time` (and `user_id = "u1"` where named), as in M3: the fixture's own `ingest_time` is later than the manual clock.

Executor tests, next to I's: find the M3 executor test that ends with a `done` job, a score and a judge trace (`grep -n 'judge_trace_id' crates/server/src/eval/*.rs`) and copy its setup (it runs a job against the mock provider or a stub runner).

4. `a_job_on_a_deleted_trace_is_skipped`: `state.deletions.guard.add_traces(project, &[A], now)` before `run_job` → the job row has `status = 'skipped'` and `error_class = 'deleted'`; no score for A (scores list), and the project's trace list with `include_eval=true` has no judge trace; the provider stub was not called.
5. `a_job_on_a_deleted_users_old_trace_is_skipped`: target trace whose spans have `user_id = "u1"` and `ingest_time = t`; `add_user(project, "u1", t + 1)` → skipped as in 4.
6. `new_data_of_a_deleted_user_is_still_evaluated`: same, but `add_user(project, "u1", t - 1)` (the deletion predates the data) → the job ends `done` with a score.
7. `a_trace_deleted_during_the_judge_call_is_not_committed` (check B): a stub runner (or a mock-provider hook, whichever I's tests already use) that calls `add_traces(project, &[A], now)` while the judge "runs" → the job is `skipped`/`deleted`, the provider was called once, no score and no judge trace exist, and the job row carries the actual cost.
8. `manual_run_on_a_deleted_trace_is_404`: guard holds A → `POST …/evaluators/{id}/run` for A answers `404`.
9. `test_run_on_a_deleted_trace_is_404`: guard holds A → `POST …/evaluators/test` with target trace A answers `404`.
10. `test_run_deleted_during_the_call_commits_nothing`: a stub runner that adds A to the guard while the judge runs → `404`, and no judge trace for A exists.

## Done when

- [ ] `meta::eval_jobs::delete_for_traces` runs in the scope transaction through `deletion::cascade::traces_deleted`; `running` jobs are left to the guard.
- [ ] `run_job` checks the guard at start, after loading and before committing; test runs and manual runs refuse a guarded trace; every call site has a `// D-M-6` comment.
- [ ] The 10 tests pass; `just check` is green; one commit `Cascade deletions to evaluation jobs`.
- [ ] Report the function and line of each guard call, the outcome type and variant you added, and anything in I's code that differed from this file.
