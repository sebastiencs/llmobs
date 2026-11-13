# P6 — Session and user details without full collects (M-3, m-9)

Read [README.md](README.md) first. Independent of the other phases.

**Goal:** `session_detail` and `user_detail` stop materialising every trace row of the session or user just to check that one exists, and `user_detail` stops building an `IN (…)` literal list of every session ID. The results stay exactly the same.

**Edit only:**
- `crates/storage/src/reads/sessions.rs`;
- `crates/storage/src/reads/users.rs`.

**Commit:** `Stop collecting every row in session and user details`

## Background

**`reads/sessions.rs`, `async fn load(ctx, session_id, include_eval, include_experiments)`:**
1. It builds `scoped` (the scoped traces filtered on `session_id`) and `plan = scoped.logical_plan().clone()`.
2. It runs `DataFrame::new(state.clone(), plan.clone()).collect()` into `existing`, which holds every `traces` view column for every trace of the session, with no limit. It then returns `Ok(None)` when `row_count(&existing) == 0`.
3. It builds `sessions_from_traces(plan.clone())`, collects it, and `single_row(&session)?` returns `Ok(None)` when there is no row.
4. It runs the turns and roots queries.

Step 2 is redundant: the aggregate of step 3 groups by `session_id`, so an empty input gives no row and step 3 already returns `Ok(None)`.

**`reads/users.rs`, `async fn load(ctx, user_id, include_eval, include_experiments)`:**
1. It builds `user_plan` (the scoped traces filtered on `user_id`).
2. It collects `user_rows` from `user_plan` (every column, every trace of the user) only to return `Ok(None)` on zero rows.
3. It runs `aggregate_users(DataFrame::new(state, user_plan))`, and `single_row(&user)?` returns `Ok(None)` when there is no row. `aggregate_users` groups by `user_id`, so an empty input gives no row.
4. It collects the distinct non-null `session_id`s of `user_plan`, reads them back with `string_column` (which silently skips a column that is not `Utf8`), and filters the scoped traces with `col(SESSION_ID).in_list(<one literal per id>, false)`.
5. It builds `sessions_from_traces` over that, sorts by `start_time DESC, session_id DESC`, and limits to `MAX_USER_SESSIONS`.

`filter/scope.rs` already uses a subquery the same way: `in_subquery(col(trace_id), Arc::new(plan))` for search on traces. DataFusion decorrelates it into a semi join.

## Changes

### `reads/sessions.rs`, `load`

Delete the `existing` collect and its `row_count` check (Background step 2). Nothing else changes: the session aggregate's `single_row` already returns `Ok(None)` for an unknown session.

If `row_count` then has no caller in this file, delete it.

### `reads/users.rs`, `load`

1. Delete the `user_rows` collect and its `row_count` check (Background step 2).
2. Replace steps 4 and 5 (from `let session_ids = …` up to the `sessions_from_traces(…)` call) with a subquery:

   ```rust
       let session_ids = DataFrame::new(state.clone(), user_plan)
           .filter(col(traces::SESSION_ID).is_not_null())
           .map_err(classify)?
           .select(vec![col(traces::SESSION_ID)])
           .map_err(classify)?
           .distinct()
           .map_err(classify)?
           .into_unoptimized_plan();
       let session_traces = DataFrame::new(state.clone(), excluded_plan)
           .filter(in_subquery(col(traces::SESSION_ID), Arc::new(session_ids)))
           .map_err(classify)?;
       let sessions =
           sessions_from_traces(session_traces.into_unoptimized_plan()).map_err(RunError::Internal)?;
   ```

   - `in_subquery` is `datafusion::logical_expr::in_subquery`, as in `filter/scope.rs`.
   - The sort, `limit(0, Some(MAX_USER_SESSIONS))` and collect that follow stay as they are.
   - Put the subquery and `sessions_from_traces` into a function that T4 calls too:

     ```rust
     /// Sessions of the user whose traces `user_plan` selects, over `excluded_plan`
     /// (all scoped traces), through an `IN (subquery)` that plans as a semi join.
     fn user_sessions_plan(
         state: &SessionState,
         excluded_plan: LogicalPlan,
         user_plan: LogicalPlan,
     ) -> Result<LogicalPlan, RunError>
     ```

     It returns the `sessions_from_traces(…)` plan. `load` calls it, then sorts, limits and collects as today.
     `SessionState` is `datafusion::execution::SessionState`.

3. Keep today's result for a user without sessions: `sessions` is an empty `Vec`. After the collect, drop zero-row batches:

   ```rust
       let sessions: Vec<RecordBatch> = sessions.into_iter().filter(|batch| batch.num_rows() > 0).collect();
   ```

4. Delete the early `if ids.is_empty() { return Ok(Some(UserDetailData { user, sessions: Vec::new() })) }` branch, and the `string_column` function if it has no other caller. Delete `row_count` too if it has no other caller.
5. Fix the imports so clippy (`-D warnings`) passes:
   - add `std::sync::Arc` and `datafusion::logical_expr::in_subquery`;
   - remove what no longer has a user: `arrow::array::{Array, AsArray}` and `Expr` were used by `string_column` and the `IN` list (`users.rs` lines 10 and 16 today). Check each one.

**Check before committing.** The plan of `load` must still end with exactly one aggregate over the scoped traces for `user`, and one `sessions_from_traces` for `sessions`.

If DataFusion rejects the subquery plan, stop and report the error. Do not fall back to collecting the IDs. Possible errors:
- "Correlated column is not allowed";
- "duplicate qualified field name", since both sides come from the same `traces` view.

## Tests

In `reads/sessions.rs` and `reads/users.rs` `mod tests`. The existing fixture tests already pin the results:
- `session_detail…`;
- `user_detail_matches_the_fixtures`, which covers `user-42`, `user-42-include-eval` and `user-7`.

They must pass unchanged. Add:

T1 and T2 are regression guards: they pass on today's code too. T4 fails on today's code, which has no `user_sessions_plan`.

Unknown IDs are already covered: `session_detail_matches_the_fixture` asserts `session_detail(&ctx, "missing", …)` is `None`, and `user_detail_matches_the_fixtures` asserts the same for `"nobody"`. Those assertions guard the removed existence checks.

**T1 `a_session_with_only_excluded_traces_is_none`.**
- Build spans with `span_row`: one trace whose root span has `session_id = Some("eval-only")` and `environment = Some(llmobs_core::names::environment::EVAL.to_owned())` (the value is `llmobs-eval`).
- Build a context from `example_query()` with `spans` and `traces` replaced by `spans_table(&spans)` and `traces_table(&spans)`.
- `session_detail(&ctx, "eval-only", false, false, TIMEOUT)` returns `Ok(None)`.
- `session_detail(&ctx, "eval-only", true, false, TIMEOUT)` returns `Ok(Some(_))`.

**T2 `a_user_with_only_excluded_traces_is_none`.** The same shape for `user_detail`, with `user_id = Some("eval-user")` on the same eval trace: `Ok(None)` without `include_eval`, `Ok(Some(_))` with it.

**T3 `a_user_without_sessions_has_an_empty_session_list`.**
- The dataset has no such user (`user-42` has `sess-001`, `user-7` has `sess-002`). Build the spans with `span_row`: one trace, `user_id = Some("lonely")`, `session_id = None`, and a context from `example_query()` with `spans` and `traces` replaced by `spans_table(&spans)` and `traces_table(&spans)`.
- `user_detail` returns `Some(data)` with `data.sessions.is_empty()`.

**T4 `user_detail_plans_a_semi_join`.**
- Build `excluded_plan` and `user_plan` as `load` does (`scoped_traces(ctx, scope, false)`, then the `user_id = 'user-42'` filter).
- Call `user_sessions_plan(&state, excluded_plan, user_plan)`, then `state.optimize(&plan)`.
- Assert `format!("{}", optimized.display_indent())` contains `LeftSemi`.
- If it does not, **stop and report** the printed plan. Do not change the assertion to match another shape.

## Done when

- `cargo test -p llmobs-storage --lib reads::` passes.
- `just check` passes.
