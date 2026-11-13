# P1 — SQL statement caps (B-1)

Read [README.md](README.md) first.

**Goal:** no SQL console statement within the limits can overflow a 2 MiB stack, and none can make the planner run for seconds. Three changes:
1. More tokens and keywords count as operator tokens: everything sqlparser chains left-deep without its recursion limit.
2. Pipe syntax (`|>`) is rejected.
3. A statement reads at most 32 tables.

**Edit only:**
- `crates/storage/src/query_text.rs`;
- `crates/storage/src/sql/validate.rs`;
- `crates/storage/src/sql/mod.rs` (the pipe check in `plan_tokens`, and tests);
- `docs/spec/05-query.md`;
- `docs/mvp-decisions.md`.

**Commit:** `Cap set operations and tables in SQL console statements`

## Background

- The token pre-check (`tokenize` in `query_text.rs`) counts operator tokens with `is_operator` and stops at `SQL_LIMITS.max_operators = 512` (filters: `FILTER_LIMITS.max_operators = 128`).
- sqlparser parses several constructs as left-deep chains, in a loop, without touching its recursion limit. DataFusion then overflows the stack while planning or optimizing them. Measured on a 2 MiB thread (release build unless noted):

  | Chain | Overflows at | Passes at |
  |---|---|---|
  | `UNION ALL SELECT 1` | 2,600 | 512 (debug and release) |
  | `INTERSECT SELECT 1` | 1,000 | 512 (debug and release) |
  | `AT TIME ZONE 'UTC'` | 2,600 | 512 (debug and release) |
  | `CROSS JOIN` tables | 700 | 256 (debug), 512 (release) |
  | comma-joined tables | 1,000 | 256 (debug) |
  | subscripts `['k']` | 1,000 (debug) | 512 (debug; planning error, no abort) |
  | pipe stages `\|> WHERE true` | 4,000 (debug) | 512 (debug) |
  | `~~ 'a'` (PostgreSQL LIKE) | 2,000 (debug) | 1,000 (debug) |

  An overflow aborts the process.
- Other forms the GenericDialect chains the same way (`sqlparser-0.62.0/src/dialect/mod.rs:808-930`, `parser/mod.rs:3782-3812`), uncounted today:
  - the `~~` family;
  - `#-` and `@?`;
  - `^@`;
  - `#`;
  - `:` (map access and slices);
  - `[`;
  - the keywords `RLIKE`, `REGEXP`, `MATCH`, `MEMBER`, `OVERLAPS`, `OPERATOR`.
- Parsing, sqlparser's `Visitor` walk (`validate_statement`) and dropping the AST survive 3,000-term chains. Planning and optimization are what overflow, so caps checked before planning are enough.
- Join planning time grows superlinearly: 64 tables take about 0.6 s, 128 about 4 s, and 512 comma-joined tables did not finish in 10 minutes. Planning is synchronous, so `tokio::time::timeout` cannot interrupt it. The table cap (32) bounds both the time and the stack.

## Changes

### 1. `query_text.rs`: more operator tokens

In `is_operator`, add these token variants to the symbol arm (after `Token::DoubleColon`):

```rust
        | Token::LBracket
        | Token::Colon
        | Token::Sharp
        | Token::DoubleTilde
        | Token::DoubleTildeAsterisk
        | Token::ExclamationMarkDoubleTilde
        | Token::ExclamationMarkDoubleTildeAsterisk
        | Token::HashMinus
        | Token::AtQuestion
        | Token::CaretAt
```

and these keywords to the `Token::Word(word) => matches!(word.keyword, …)` arm (after `Keyword::DIV`):

```rust
                | Keyword::UNION
                | Keyword::INTERSECT
                | Keyword::EXCEPT
                | Keyword::MINUS
                | Keyword::AT
                | Keyword::COLLATE
                | Keyword::RLIKE
                | Keyword::REGEXP
                | Keyword::MATCH
                | Keyword::MEMBER
                | Keyword::OVERLAPS
                | Keyword::OPERATOR
```

- Nothing else changes in `tokenize`. The messages stay `the query has more than 512 operators` / `the filter has more than 128 operators`, positioned at the first token over the limit.
- Side effects, all accepted:
  - an array literal or subscript costs one operator per `[`;
  - a `{name:Type}` parameter costs one operator for its `:`, because placeholders are bound after `tokenize` counts;
  - an unquoted column named `at` or `match` costs one.

  A quoted identifier such as `"union"` has `keyword == Keyword::NoKeyword` and costs nothing.
- The filter DSL shares `is_operator`. Its validator already rejects every new form except `[`, which counts there too: `tags @> ['beta']` now uses 2 of the 128 operators instead of 1.

### 2. `sql/mod.rs`: reject pipe syntax

In `plan_tokens`, before `let mut parsed = parser(tokens);`, add:

```rust
    if let Some(pipe) = tokens
        .iter()
        .find(|token| token.token == Token::VerticalBarRightAngleBracket)
    {
        return Err(invalid(
            "pipe syntax (`|>`) is not allowed",
            Some(position(pipe.span.start.line, pipe.span.start.column)),
        ));
    }
```

- `Token` is already imported there (`datafusion::sql::sqlparser::tokenizer::{Token, TokenWithSpan}`), and so are `invalid` and `position`.
- Spec 05 § Sandbox item 2 lists the allowed query forms, and pipe syntax is not among them.
- Rejecting it on the token stream avoids building the chain at all.

### 3. `sql/validate.rs`: at most 32 tables

1. Add a module-level constant next to the imports:

   ```rust
   /// Tables one statement may read (D-A2-1).
   pub(crate) const MAX_TABLES: usize = 32;
   ```

2. Add a field `tables: usize` to `struct Walk`, and initialise it to `0` where `validate_statement` builds `Walk`.

3. At the very start of `Walk::pre_visit_table_factor`, before the existing `match factor`, insert:

   ```rust
   if !matches!(factor, TableFactor::NestedJoin { .. }) {
       self.tables += 1;
       if self.tables > MAX_TABLES {
           return ControlFlow::Break(invalid(
               format!("the query reads more than {MAX_TABLES} tables"),
               at(factor.span()),
           ));
       }
   }
   ```

   - `NestedJoin` is only parentheses around other factors; the visitor visits those factors on their own, so it is not counted.
   - Every other factor counts, wherever it appears: `FROM`, `JOIN`, comma joins, CTE bodies, subqueries. That covers views, CTE references, derived tables (`(SELECT …) t`, `(VALUES …) v`) and `UNNEST`.
   - Factors that are rejected later (table functions, `PIVOT`, …) are counted first. That is harmless: the statement fails either way, and the first error reached is returned.
   - The visitor is a pre-order walk in AST field order: a query's `WITH` before its body, and a `SELECT`'s projection subqueries before its `FROM`. The 33rd factor in that order gets the error and its position. For the plain `FROM` and `JOIN` lists in the tests, that is source order.

`validate_statement` is only used by the SQL console. The filter DSL has no tables. Read queries are built in Rust and do not go through it.

## Docs

### `docs/spec/05-query.md`, § SQL console › Sandbox

Replace item 1 with:

```
1. Limits: 64 KiB and 512 operator tokens (the same operator tokens as the filter DSL; set operations `UNION`, `INTERSECT`, `EXCEPT`, `MINUS`, the keywords `AT`, `COLLATE`, `RLIKE`, `REGEXP`, `MATCH`, `MEMBER`, `OVERLAPS`, `OPERATOR`, subscripts `[`, `:`, `#` and the PostgreSQL operators `~~`, `~~*`, `!~~`, `!~~*`, `#-`, `@?`, `^@` count too); parser recursion limit 50 (D95, D-A2-1). At most 32 tables per statement: every view, CTE reference, derived table and `UNNEST` in a `FROM` or `JOIN` counts, wherever it appears (D-A2-1).
```

In item 2, append to the end of the item: ` Pipe syntax (`|>`) is rejected (D-A2-1).`

### `docs/mvp-decisions.md`

Create a new section after the `## Track A1 — Storage engine` table and before `## Open`:

```
## Track A2 — Query layer

| # | Topic | Decision |
|---|---|---|
| D-A2-1 | SQL statement caps | **Every token or keyword that sqlparser chains left-deep counts as an operator token (D95): set operations, `AT`, `COLLATE`, subscripts, `:`, `#`, the PostgreSQL `~~` family, `#-`, `@?`, `^@`, `RLIKE`, `REGEXP`, `MATCH`, `MEMBER`, `OVERLAPS` and `OPERATOR`. Pipe syntax (`\|>`) is rejected, and a SQL console statement reads at most 32 tables** (each view, CTE reference, derived table or `UNNEST` in a `FROM` or `JOIN`). sqlparser builds these chains without using its recursion limit, and DataFusion then overflows a 2 MiB stack while planning them: about 2,600 `UNION ALL` terms, 1,000 `INTERSECT` terms, 2,600 `AT TIME ZONE` terms or 700 joined tables in a release build, and 1,000 subscripts, 4,000 pipe stages or 2,000 `~~` in a debug build. That aborts the process. 32 tables also bound planning time: join planning grows superlinearly (about 4 s at 128 tables, over 10 minutes at 512) and the request timeout cannot interrupt it (spec 05 § Sandbox). |
```

If another phase already created the section (see README § Order), add only the D-A2-1 row, as the first row of its table.

## Tests

All tests are new unless stated otherwise. Positions are 1-based `(line, column)` in characters; every text below is ASCII, so the column is the byte index plus 1.

### In `query_text.rs` `mod tests`

**T1 `chained_forms_count_as_operators`.** For each text below:
- tokenize it with `Tokenizer::new(&DIALECT, text).tokenize_with_location().expect("tokens")`;
- count the tokens for which `is_operator(&token.token)` is true;
- assert the count.

| Text | Count |
|---|---|
| `SELECT 1 UNION SELECT 1` | 1 |
| `SELECT 1 UNION ALL SELECT 1` | 1 |
| `SELECT 1 INTERSECT SELECT 1` | 1 |
| `SELECT 1 EXCEPT SELECT 1` | 1 |
| `SELECT 1 MINUS SELECT 1` | 1 |
| `SELECT now() AT TIME ZONE 'UTC'` | 1 |
| `SELECT 'a' COLLATE x` | 1 |
| `SELECT metadata['k'] FROM spans` | 1 |
| `SELECT 'a' ~~ 'a'` | 1 |
| `SELECT 'a' RLIKE 'a'` | 1 |
| `SELECT {n:Int}` | 1 |
| `SELECT "union" FROM spans` | 0 |

### In `sql/mod.rs` `mod tests`

**T2 `set_operations_count_toward_the_operator_limit`.** Mirrors the existing `text_limits_apply_to_the_query`.
1. `let mut text = String::from("SELECT 1 AS a");`
2. Push `" UNION ALL SELECT 1"` 512 times. `plan_sql(&context(), &text)` succeeds.
3. Let `column = text.len() as u32 + 2`, then push one more `" UNION ALL SELECT 1"`.
4. `plan_sql` fails with message `the query has more than 512 operators` and position `at(1, column)`.

**T3 `the_table_cap_counts_every_table_factor`.**
1. `let mut text = String::from("SELECT 1 AS a FROM scores s0");`
2. For `i` in `1..32`, push `format!(" CROSS JOIN scores s{i}")`. `plan_sql` succeeds: 32 tables.
3. Let `column = text.len() as u32 + 13`. That is the column of `scores` in the next push: one space, then 11 characters of `CROSS JOIN `, then `scores`. Push `" CROSS JOIN scores s32"`.
4. `plan_sql` fails with message `the query reads more than 32 tables` and position `at(1, column)`.
5. Repeat steps 1–4 with comma joins: push `format!(", scores s{i}")`, and for the 33rd let `column = text.len() as u32 + 3` (comma, space, then `scores`) before pushing `", scores s32"`. Same message and position rule.
6. CTE references count too. `WITH c AS (SELECT 1 AS a) SELECT 1 AS a FROM c c0` followed by `, c c1` … `, c c31` (32 references; the CTE body has no `FROM`) succeeds. Adding `, c c32` fails with the same message, and the position is `at(1, column)` where `column = text.len() as u32 + 3` before the push.

**T4 `pipe_syntax_is_rejected`.** `plan_sql(&context(), "SELECT 1 AS a |> WHERE true")` fails with message ``pipe syntax (`|>`) is not allowed`` and position `at(1, 15)`.

**T5 — the 2 MiB stack tests.** Refactor the existing `#[tokio::test] async fn a_full_size_query_runs_on_a_two_megabyte_stack` into a helper plus plain tests:

1. The helper:

   ```rust
   #[derive(Clone, Copy)]
   enum OnStack {
       /// Plan, build the physical plan and run it.
       Run,
       /// Plan and build the physical plan only.
       Plan,
       /// `plan_sql` must return an error (and not abort).
       Reject,
   }

   fn on_two_megabytes(sql: String, mode: OnStack) {
       let joined = std::thread::Builder::new()
           .stack_size(2 * 1024 * 1024)
           .spawn(move || {
               let runtime = tokio::runtime::Builder::new_current_thread()
                   .enable_all()
                   .build()
                   .expect("runtime");
               runtime.block_on(async move {
                   let ctx = context();
                   let planned = plan_sql(&ctx, &sql).await;
                   if matches!(mode, OnStack::Reject) {
                       planned.expect_err("rejected");
                       return;
                   }
                   let plan = planned.expect("plan");
                   let frame = ctx
                       .session()
                       .execute_logical_plan(plan)
                       .await
                       .expect("execute");
                   frame.create_physical_plan().await.expect("physical");
                   if matches!(mode, OnStack::Run) {
                       frame.collect().await.expect("run");
                   }
               });
           })
           .expect("thread")
           .join();
       if let Err(payload) = joined {
           std::panic::resume_unwind(payload);
       }
   }
   ```

   `DataFrame::create_physical_plan` takes `&self`.

2. The existing test becomes a plain `#[test] fn a_full_size_query_runs_on_a_two_megabyte_stack()` that builds the same `sql` and calls `on_two_megabytes(sql, OnStack::Run)`. It must not stay a `#[tokio::test]`: blocking on `join()` inside a runtime is wrong.

3. New plain `#[test]` functions:

   | Test | `sql` | Mode |
   |---|---|---|
   | `full_size_set_operations_run_on_a_two_megabyte_stack` | `"SELECT 1 AS a"` + 512 × `" UNION ALL SELECT 1"` | `Run` |
   | `full_size_intersections_run_on_a_two_megabyte_stack` | `"SELECT 1 AS a"` + 512 × `" INTERSECT SELECT 1"` | `Run` |
   | `full_size_time_zone_chains_run_on_a_two_megabyte_stack` | `"SELECT now()"` + 512 × `" AT TIME ZONE 'UTC'"` + `" AS t"` | `Run` |
   | `full_size_like_chains_run_on_a_two_megabyte_stack` | `"SELECT 'a' AS a WHERE 'a'"` + 512 × `" ~~ 'a'"` | `Run` |
   | `full_size_subscripts_are_rejected_on_a_two_megabyte_stack` | `"SELECT metadata"` + 512 × `"['k']"` + `" AS x FROM spans"` | `Reject` |
   | `full_size_joins_plan_on_a_two_megabyte_stack` | `"SELECT 1 AS a FROM scores s0"` + `" CROSS JOIN scores s{i}"` for `i` in `1..32`, then `" WHERE true"` + 479 × `" OR true"` | `Plan` |

   - **The `~~` row.** `SELECT 'a' AS a WHERE 'a' ~~ 'a' ~~ 'a'` plans on today's code (checked), so the full chain is planned and run.
   - **The subscript row.** Indexing a string with `['k']` is a type error after the first level, so `plan_sql` returns an error. The test checks that it is an error and not an abort.
   - **The joins row.** It does not run its plan: a 32-way cross join of `scores` has too many rows.
   - If any of these tests overflows the stack (the process aborts with `has overflowed its stack`), **stop and report**. Do not lower a cap or raise the stack size.

**Existing tests** must pass unchanged, in particular:
- `text_limits_apply_to_the_query`;
- `statements_outside_a_query_are_rejected`;
- `read_queries_plan`;
- the parameter tests in `sql/params.rs` and `sql/mod.rs` (each `{name:Type}` now costs one operator);
- the filter tests in `query_text.rs` and `filter/` (`[` now costs one operator).

If one of these fails only because of the new operator cost, stop and report.

## Done when

- `cargo test -p llmobs-storage --lib sql::`, `cargo test -p llmobs-storage --lib query_text` and `cargo test -p llmobs-storage --lib filter::` pass.
- `just check` passes.
- The commit contains only the files listed under "Edit only".
