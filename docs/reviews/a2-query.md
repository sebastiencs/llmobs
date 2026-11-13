# Review: track A2 (query layer)

Reviewed on `main` at `8022dc8` (2026-11-13). The baseline is plan `docs/plans/a2-query/` (Q1–Q20), spec `docs/spec/05-query.md`, and decisions D89–D101.
Code reviewed:
- `crates/storage/src/{views,filter,sql,reads}/`;
- `query_text.rs` and `query_error.rs`;
- the Q20 contract changes in `crates/server`, `openapi.json`, `fixtures/api/` and `web/src/api/schema.gen.ts`.

Method: five read-only review passes, one per group of sub-phases (Q1–5, Q6–9, Q10–13, Q14–17, Q18–20). Every finding kept here was checked again against the code. Findings marked **probed** were reproduced with a throwaway test that was not committed (code below in § Probes).

`cargo test -p llmobs-storage --lib` on the A2 modules (`views:: filter:: sql:: reads:: query_text query_error`): 132 passed, 0 failed.

## Summary

A2 is complete and close to the plan. All 20 sub-phases landed, one commit each, and the deviations found later were mostly recorded (D153, D168, `optimizer_rules` in the README). The keyset cursors, NULLS LAST handling, cursor decode hardening, score "latest" rule, window pushdown and error positions are correct.

The weak point is **resource safety of the SQL console**, the one surface where users write arbitrary text:

- **D95 does not hold.** Chains of operators that the token pre-check does not count abort the whole process with a stack overflow, in release builds, under the 64 KiB limit.
- **The D91 memory cap is bypassable.** Scalar functions such as `range` and `repeat` allocate outside the DataFusion memory pool. One query took the process to 1.6 GB of RSS under a 64 MiB pool.

Since llmobs is a single binary, both let one authenticated user take the server down. Fix them before G exposes `/sql`.

Counts: **2 blocker, 4 major, 22 minor** confirmed; 5 discarded.

**Known item (per-partition ordering):** no query code relies on per-partition order, so results are not wrong for that reason. A1 already added `with_preserve_order(true)` to both query scans (`b0b8305`). The order-sensitive thing found instead is the float `cost_total` sum, which affects cursor paging (M-4). Details are in § Known item.

## Status per sub-phase

| Sub-phase | Commit | Status | Note |
|---|---|---|---|
| Q1 spec and decisions | `65d41df` | as planned | D91–D101 recorded; spec 05/09/11 and spec 03 §6 updated. |
| Q2 context, base views, test_support | `c113ffa` | as planned | Later recorded changes: `session()` public (D168), `optimizer_rules` (D153). Sandbox tests are loose (m-12). Scalar allocators left in the registry (B-2). |
| Q3 sessions view | `40ccac5` | as planned | No test under a time window (m-13). |
| Q4 hex-ID rule | `a72bc0f` | as planned | Mixed `IN` list edge case (m-16); several arms untested (m-14). |
| Q5 view catalog | `ba1a656` | as planned | Nullability and function registration not cross-checked (m-15). |
| Q6 query text limits | `50a94b4` | deviates (gap) | The D95 operator count misses set operations and `AT TIME ZONE` (B-1). |
| Q7 filter compile | `1da0b90` | as planned | Most allow-listed functions have no positive test (m-17). |
| Q8 score functions | `28bc194` | as planned | — |
| Q9 exclusions, window, search | `ad0795e` | deviates (unrecorded) | `exclusions` signature differs from the plan (m-10); no view guard (m-11). |
| Q10 statement validation | `ff44b06` | as planned | Shares the B-1 gap. |
| Q11 parameters | `fd7cbe0` | as planned | `:name` placeholders escape the positioned rejection (m-6). |
| Q12 execution, JSON | `e18bc08` | as planned, with bugs | Encoding errors become 500 (m-4); nested timestamps are wrong (m-3); decimals lose digits (m-5). |
| Q13 exports | `96ea591` | deviates | Parquet export buffers the whole file (M-1). |
| Q14 cursors, trace list | `c368414` (+A1 `1f2fcc2`, `3789be4`) | as planned | Float sort keys are not stable across pages (M-4). `trace_list_plan` is public but not in the README (m-20). |
| Q15 span list, details | `591b13b` | as planned | — |
| Q16 sessions and users | `42805f7` | as planned, with perf issues | Detail queries collect every row only to test existence (M-3); unbounded `IN` list (m-9). |
| Q17 scores | `643934e` | as planned | The `updated_at` rule is not tested (m-19). |
| Q18 metrics buckets | `ea22825` | as planned, with a gap | `Auto` has no bucket cap (M-2). |
| Q19 metrics series | `21910bb` | as planned | Test gaps (m-21). |
| Q20 API contract | `4a81c88` | as planned | The fixture's `in_sql` values are not checked (m-22). |

## Confirmed findings

### Blockers

**B-1. Uncounted operator chains overflow the stack and abort the process (D95).** Probed.
- Sub-phase: Q6/Q10. Location: `crates/storage/src/query_text.rs:239` (`is_operator`) and `sql/validate.rs`.
- Evidence. Both of these abort with `fatal runtime error: stack overflow` (SIGABRT) on a 2 MiB thread, in debug **and release** builds:
  - `SELECT 1 AS a` followed by 3,300 × ` UNION ALL SELECT 1` (62 KiB, 0 counted operators);
  - `SELECT now()` followed by 3,000 × ` AT TIME ZONE 'UTC'` (57 KiB). 1,000 repetitions pass.
- Why it happens:
  - `is_operator` does not count `UNION`/`INTERSECT`/`EXCEPT` or `AT TIME ZONE` (nor `COLLATE` and the other uncounted forms).
  - sqlparser builds these chains left-deep without consuming recursion depth.
  - Something recursive afterwards (sqlparser's derived `Visitor`, `Drop`, or DataFusion planning; not isolated) overflows.
- The filter DSL is safe: its 4 KiB limit is too short, and the validator rejects these forms at the outermost node.
- Reference: D95 ("checked on the token stream before parsing … the overflow aborts the process"); 03-sql-console.md § Q10 "Stack".
- Suggested fix:
  - Count set-operation keywords, `AT`, `COLLATE` and every other binary form the parser chains, or better, every keyword that can make a left-deep chain.
  - Add the two probes above as 2 MiB-stack tests, next to the existing 512-operator test.

**B-2. Scalar functions allocate without limit, outside the memory pool (D91, D96).** Probed.
- Sub-phase: Q2/Q10. Location: `crates/storage/src/views/session.rs:43-44` (`SessionStateDefaults::default_scalar_functions()`).
- Evidence. Under a 64 MiB `RuntimeEnv` pool, every query below succeeded:
  - `SELECT array_length(range(0, 100000000))`: peak RSS 48 → 825 MiB;
  - `SELECT length(repeat('x', 400000000))`: 825 → 1,598 MiB;
  - `array_repeat(1, 100000000)`;
  - `count(*)` over `unnest(range(0, 20000000))`.
- None of these allocations is tracked by the pool, so `TooLarge` never fires.
- The timeout cannot interrupt a synchronous kernel. `range(0, 4000000000)` would reserve about 32 GB, which on an overcommitting host means the OOM killer.
- The session comment says table functions are left out so "callers must not invent rows", yet the scalar `range`/`generate_series` combined with `unnest` do exactly that.
- Reference: D91 (per-query memory pool), D96, spec 05 § Sandbox.
- Suggested fix:
  - Remove the size-generating scalars from the registry (`range`, `generate_series`/`gen_series`, `array_repeat`, `repeat`, `array_resize`, and check `lpad`/`rpad`), or reject them by name in the validator.
  - Record the list in D96 and test each one.

### Major

**M-1. Parquet export holds the whole file in memory.** Probed.
- Sub-phase: Q13. Location: `crates/storage/src/sql/encode.rs:218-224` (`ParquetEncoder::push`).
- Evidence:
  - `push` takes bytes only when `in_progress_size() > 32 MiB`. `in_progress_size` covers only the open row group.
  - `ArrowWriter` closes row groups itself at `max_row_group_row_count`, and those bytes stay in the inner `Vec<u8>` until `finish`.
  - Exporting 3M one-column rows produced **one** chunk of 3.8 MB at the end of the stream.
  - For narrow rows, a 10M-row export is buffered whole. No test covers more than one chunk.
- Reference: 03-sql-console.md § Q13 ("bounded memory, chunks as row groups complete, footer last").
- Suggested fix: after each `write`, drain `inner_mut()` whenever `flushed_row_groups().len()` grew, or when the buffer passes a threshold. Add a test that asserts several chunks and a round trip of the concatenation.

**M-2. `bucket=auto` has no bucket cap.**
- Sub-phase: Q18. Location: `crates/storage/src/reads/metrics.rs:365-381` (`resolve`) and `398` (`bucket_grid`).
- Evidence:
  - `Auto` falls back to `D1` without checking the count; only `Fixed` is checked against `FIXED_LIMIT = 2000`.
  - `from = 0, to = i64::MAX` gives about 106,750 daily buckets.
  - `bucket_grid` and `fill` then allocate one `Point` per bucket for every series × group, synchronously after the queries. With `group_by` this reaches tens of millions of points, outside the pool and the timeout.
- This follows the spec text ("else `1d`"), so the gap is in spec 05 as well.
- Suggested fix: apply the 2,000 limit to the resolved width too, or cap the window length. Record it in spec 05 § Dashboard metrics.

**M-3. `session_detail` and `user_detail` collect every trace row only to test that one exists.**
- Sub-phase: Q16. Location: `crates/storage/src/reads/sessions.rs:196-203` and `reads/users.rs:248-254`.
- Evidence:
  - The scoped traces plan, with all view columns including `metadata`, tags and previews and no limit, is `.collect()`ed only to test `row_count == 0`.
  - The aggregate that follows already returns `Option` through `single_row`.
  - A user or session with many traces pays one full materialisation (and can hit `TooLarge`) on top of the 2 or 3 scans that follow.
- Suggested fix: drop the pre-collect and return `Ok(None)` when the aggregate yields no row.

**M-4. Cursors on float sort keys are not stable across pages.** This is where the known item leads.
- Sub-phase: Q14/Q16. Location: `reads/cursor.rs` (float value), `reads/traces.rs` `TraceSort::CostTotal`, `reads/sessions.rs` and `reads/users.rs` `CostTotal` sorts. Root cause in A1's `partials.rs:469` (`sum_f64`).
- Evidence:
  - A trace's `cost_total` is an f64 sum over its partials, in the order they reach `TraceMerger`. That order is not deterministic: `UnionExec` → hash `RepartitionExec`, file grouping, and work stealing when it is enabled.
  - f64 addition is not associative, and A1's own property test `merge_is_independent_of_input_order` compares `cost_total` with a `1e-9` relative tolerance (`partials.rs:1237`).
  - Sessions and users add a second, partitioned `SUM` on top.
  - The cursor stores the exact f64 and the next page compares with `=`/`<`. If the recomputed sum differs by one ulp, the boundary row (or a near-equal neighbour) is repeated or skipped.
  - It needs a row with 3 or more cost-bearing partials at a page boundary, so it is rare, but it is a wrong result.
- Suggested fix: make the sum order-independent in `TraceMerger` (sort the partial costs per trace before summing, or accumulate in fixed point, e.g. integer nano-dollars), and do the same for session and user `SUM`, or round the sort key to a fixed scale on both sides of the cursor. Add a page-walk test over traces with several partials.

### Minor

| # | Sub-phase | Location | Finding | Suggested fix |
|---|---|---|---|---|
| m-1 | Q13 | `sql/run.rs:153-160` | `sql_export` runs `prepare_sql` and `execute_stream` before the deadline applies, so planning is not bounded by the timeout (spec 05 § Sandbox). `Instant::now() + request.timeout` panics on `Duration::MAX`; the server is expected to bound it. | Wrap setup in `timeout_at`; use `checked_add`. |
| m-2 | Q12 | `sql/encode.rs` (`normalize_timestamps`) | Out-of-range timestamp casts give a 500. Probed: `arrow_cast(100000000000,'Timestamp(Second, None)')` → `Internal("failed to rebuild a batch", "declared as non-nullable but contains null values")`. | Cast with `safe: false` and return `Failed` naming the column. |
| m-3 | Q12 | `sql/encode.rs` | Timestamps nested in lists, structs or maps are not normalised. Probed: `[arrow_cast(1700000000000,'Timestamp(Millisecond, Some("+02:00"))')]` prints `2023-11-15T00:13:20Z`; the same value at the top level prints `22:13:20Z`. Wrong data (D98). | Normalise nested timestamp fields, or cast nested tz types to UTC. |
| m-4 | Q12/Q13 | `sql/encode.rs:44-52` | Encoder type errors are `Internal` (500) although the caller's query caused them. Probed: `SELECT map([1],[2])` → `Internal("Only UTF8 keys supported…")`. Same for Parquet-unsupported types. | Map them to `Failed` naming the column and type. |
| m-5 | Q12 | `sql/encode.rs` | Decimals pass through f64. Probed: a `Decimal128(38,3)` value returned `1.2345678901234568e+28`; NDJSON keeps the digits, so the formats disagree. | Write decimals as strings, or document it in D98. |
| m-6 | Q11 | `sql/params.rs` (`reject_raw_placeholder`) | Only `$n` and `?` tokens are rejected. Probed: `SELECT :a` → `Failed("Placeholder ':a' was not provided…")` (400 `query_failed`, no position); `SELECT @a` → `Invalid` with no position. | Walk the AST for `Value::Placeholder` and reject with a position. |
| m-7 | Q2 | `query_error.rs` (`RunError::Internal(#[source] DataFusionError)`) | An error-chain logger (`{:#}`, anyhow) prints the DataFusion source, which can quote literal values (D94: logs carry the kind only). Nothing pins the safe path. | Log only `kind()` at call sites, or drop `#[source]`; note it for the G server work. |
| m-8 | Q2/Q6/Q7/Q9–Q11 | `query_text.rs:6`, `filter/{mod,scope,validate}.rs`, `sql/{mod,params,validate}.rs`, `views/mod.rs:28-33,155-160,285` | Module-wide `#![allow(dead_code)]` and "callers arrive later" comments remain, though every caller has landed. They hide real dead code (e.g. the unused `expect_end(_, _text)` parameter, the unread `Placeholder.position`). | Remove the allows and comments; fix what clippy reports. |
| m-9 | Q16 | `reads/users.rs:270-276`, `303-318` | `user_detail` turns every distinct session id of the user into an `IN (…)` literal list with no cap, although only 100 sessions are returned. `string_column` silently skips a non-`Utf8` column, which would make `sessions: []`. | Use a semi-join on the subquery; error on an unexpected type. |
| m-10 | Q9 | `filter/scope.rs:33` | `exclusions(ctx, view, …) -> Result<Option<Expr>, RunError>` differs from the plan's `exclusions(include_eval, include_experiments, filter_references_environment) -> Option<Expr>`, and the change is not recorded. | Update the plan or README signature. |
| m-11 | Q9 | `filter/scope.rs:33-59` | `exclusions` does not reject the `sessions`/`scores` views as `search` does. A misuse surfaces as an opaque planning error or `Internal`. | Match the view and return an error. |
| m-12 | Q2 | `views/session.rs` test `sandbox_rejects_tables_outside_the_views` | Assertions accept any non-empty error (`SHOW TABLES`, URL table) or any message containing the name. Missing cases: `datafusion.public.spans`, `COPY`, `SET`, `CREATE VIEW`, and a positive control. | Assert the error class per case; add the cases. |
| m-13 | Q2/Q3 | `views/mod.rs` `time_window_filters_every_scan` | Not tested: the `sessions` view under a window (D101), and a row at exactly `from` (inclusive bound). | Add both. |
| m-14 | Q4 | `views/hex.rs` tests | Untested arms: `NOT IN`, a literal on the left, the column-to-column compare, a NULL literal, the `<`/`>`/`IS DISTINCT FROM` pass-through, and row results for the parent and root ids. | Table-driven test comparing rows with the rule on and off. |
| m-15 | Q5 | `views/catalog.rs` `catalog_columns_match_the_registered_views` | Catalog nullability is checked only against the fixture, not against the view schema. Nothing checks that each `in_sql` function is registered in the session. | Assert `nullable == field.is_nullable()`; resolve each function. |
| m-16 | Q4 | `views/hex.rs:93-107` | A list with one invalid literal is not rewritten at all. So `trace_id IN ('<UPPERCASE hex>','nope')` misses a row that `trace_id IN ('<UPPERCASE hex>')` or `= '<UPPERCASE hex>'` matches. Case sensitivity then depends on the other literals. | Rewrite valid literals and make invalid ones never match (or always compare lowercased); test an uppercase mixed list. |
| m-17 | Q7 | `filter/mod.rs` tests | No positive planning test for `lower`, `upper`, `length`, `coalesce`, `array_has`, `array_length`, `date_trunc`, `abs`, `IS DISTINCT FROM`, `NOT IN` or `::TIMESTAMP`. No negative test for `TRY_CAST`, named arguments or `now(1)`. | One table-driven test per allow-listed form. |
| m-18 | Q9 | `filter/scope.rs:73-99` | Search text has no length cap (D95 covers filter and SQL only). It becomes four `ILIKE`s over `input` and `output`. | Cap it (e.g. 4 KiB) here or in the server; record it in spec 05 § Full-text search. |
| m-19 | Q17 | `reads/scores.rs` tests | The only "latest" case ties on `updated_at`, so dropping `updated_at` from the order would still pass. | Add a case where the greater `updated_at` has the smaller `score_id`. |
| m-20 | Q14 | `reads/traces.rs:350`, `reads/mod.rs:33`, `lib.rs:85` | `trace_list_plan` (A1 M3, `#[doc(hidden)] pub`, bench only) is not in the A2 README's public API. | List it in the README with its reason, or keep it crate-private. |
| m-21 | Q16/Q18/Q19 | `reads/{users,sessions,metrics}.rs` tests | Untested: `user_list` with a window; `MAX_SESSION_TURNS` and `MAX_USER_SESSIONS` truncation; `Auto` at 120/121 buckets; `Fixed` at exactly 2,000; negative `from`; the `top_models` tie on cost; span-series `group_by`. | Add them. |
| m-22 | Q20 | `views/catalog.rs:945-946` | `function_catalog_matches_the_fixture` checks `in_sql` against a hard-coded name list, not against `expected["in_sql"]`. | Compare with the fixture value. |

## Known item: per-partition ordering in query scans

Question: A1 fixed compaction (`058b80f`) because DataFusion's file-stream work stealing moved a file into another partition, breaking a merge that assumed sorted partitions. Do `query/mod.rs` and `query/traces.rs` have the same problem?

What the code shows:

1. **Already mitigated.** `b0b8305` set `with_preserve_order(true)` on both query scans (`query/mod.rs:686`, `query/traces.rs:652`). No test pins it, unlike compaction's `the_scan_never_moves_a_file_to_another_partition`.
2. **The flag is not guaranteed to survive.** DataFusion 55.2's `LimitPushdown` calls `with_preserve_order(global_state.preserve_order)` on the plan it pushes a fetch into (`limit_pushdown.rs:282,292`). `DataSourceExec` forwards that to `FileScanConfig`. So an unordered `LIMIT` over the `spans` view can turn work stealing back on. This is harmless today, but the comment "No work stealing" is not a guarantee.
3. **Nothing on the query side depends on partition order:**
   - neither query scan declares an `output_ordering`, so DataFusion never assumes one, and every read that needs an order has its own `SortExec`;
   - the spans and scores scans are plain unions, and scores latest-row selection is a window function;
   - the traces merge hash-repartitions the partials by `trace_id` (which already interleaves inputs non-deterministically), then merges them in a `HashMap` with rank-based rules (`Ranked` compares rank and then value; min, max, set union, OR). All of that is order-independent.

   **So no wrong results come from file movement between partitions, and no demonstrating test was written for it.**
4. **One merge rule is order-dependent: the f64 `cost_total` sum** (`partials.rs:469`). Its result depends on arrival order with or without work stealing, and A1's tests already tolerate it. A2's cursor paging turns that last-ulp difference into a repeated or skipped row (M-4). A minimal demonstration: `(0.1 + 0.2) + 0.3 != 0.1 + (0.2 + 0.3)` in f64.

## Discarded findings

- Q10-2 "CTE names are global, not in scope". DataFusion enforces scope. Probed: `SELECT * FROM (WITH r AS (SELECT 1 AS a) SELECT * FROM r) x, r` returns `table 'r' not found` with a position.
- Q6-2 "`DataFusionError::Internal` maps to `Failed` (400)". This is the plan's documented choice (03-sql-console.md § Q12, error table and its rationale).
- Q14-2 "NaN cursor encodes as null". Reachable only if a stored `cost_total` is NaN or ±inf. Costs are computed by the B1 cost table from token counts, so a non-finite cost is not expected; this was not traced end to end. At most a defensive check.
- Q14-3 "cursor not bound to the filter". The plan binds cursors to list, sort and order only, and the behaviour under a changed filter is well defined.
- Q17-2 "score_list validates arguments after decoding the cursor". This only changes which of two 400 messages is returned.

## Open questions

1. **`bucket=auto` cap (M-2).** The spec says "else `1d`" with no bound. Should the D1 fallback respect the 2,000-bucket limit (400), or should the window length be capped?
2. **SQL JSON response size.** `sql_json` caps rows at 10,000 but not bytes, and collected batches are not tracked by the pool. 10,000 rows of `input`/`output` can be very large. Should D98 add a byte cap?
3. **Empty `session_id`.** The sessions view drops only NULL. Does B1 normalisation guarantee that `""` is stored as NULL? If not, all such traces form one session `""`.
4. **CSV formula injection.** Exported CSV cells starting with `=`, `+`, `-` or `@` are written verbatim (stored span names and payloads). Should the export neutralise them?
5. **Plain `EXPLAIN`** (allowed by D96) prints provider internals: file paths, counts and the `TracesExec` display. Is that acceptable for project members?
6. **Hex literal case.** Spec 05 and the Q4 rule accept uppercase hex for `=` and all-valid `IN` lists, which differs from string semantics on the lowercase view column. Is that intended everywhere (see m-16)?

## Probes

The throwaway test module used for the probed findings, run with `cargo test -p llmobs-storage --lib [--release] -- --nocapture --exact review_probe::<name>`. It was not committed.

```rust
fn ctx_with_pool(bytes: usize) -> QueryContext {
    let mut q = example_query();
    q.runtime = Arc::new(RuntimeEnvBuilder::new().with_memory_limit(bytes, 1.0).build().unwrap());
    QueryContext::new(q, None).unwrap()
}

// B-2: run each query through ctx_with_pool(64 MiB).sql_json(...) and read VmHWM
//   from /proc/self/status before and after.
//   "SELECT array_length(range(0, 100000000)) AS n"
//   "SELECT length(repeat('x', 400000000)) AS n"

// B-1: on std::thread::Builder::new().stack_size(2 * 1024 * 1024), with a
//   current-thread runtime, run sql_json on:
//   "SELECT 1 AS a" + " UNION ALL SELECT 1" x 3300            -> SIGABRT (debug and release)
//   "SELECT now()" + " AT TIME ZONE 'UTC'" x 3000 + " AS t"    -> SIGABRT (debug and release)

// M-1: sql_export("SELECT unnest(range(0, 3000000)) AS x", Parquet)
//   -> 1 chunk of 3,836,098 bytes.
```
