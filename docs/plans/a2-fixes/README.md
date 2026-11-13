# A2 fixes — plan

This plan fixes the blocker and major findings of the A2 review ([`docs/reviews/a2-query.md`](../../reviews/a2-query.md)): B-1, B-2, M-1, M-2, M-3 and M-4. The minor findings are out of scope, except m-9 (P6 rewrites the same function).

The work is split into 7 phases, P1–P7. One agent implements one phase, in order. Each phase:
- lists the files it may edit;
- gives the exact behaviour, the tests and the doc changes;
- ends with `just check` green and one commit.

**Do only the phase you were given.** If something in the phase file does not match the code, or a step fails in a way the file does not cover, stop and report: do not improvise a different design.

**Status:** decisions settled with the user (2026-11-13). Nothing is implemented.

**Read first** (every phase):
- `AGENTS.md`;
- this README;
- the review findings your phase fixes (`docs/reviews/a2-query.md`);
- `docs/spec/05-query.md` § SQL console;
- your phase file.

| Phase | File | Fixes | Commit subject |
|---|---|---|---|
| P1 | [01-sql-statement-caps.md](01-sql-statement-caps.md) | B-1 stack overflow | `Cap set operations and tables in SQL console statements` |
| P2 | [02-guarded-functions.md](02-guarded-functions.md) | B-2, part 1 | `Guard scalar functions with the query memory pool` |
| P3 | [03-guard-rule.md](03-guard-rule.md) | B-2, part 2 | `Guard planner-built functions and string concatenation` |
| P4 | [04-parquet-export-chunks.md](04-parquet-export-chunks.md) | M-1 | `Stream Parquet exports one row group at a time` |
| P5 | [05-metrics-auto-cap.md](05-metrics-auto-cap.md) | M-2 | `Cap automatic metric buckets at 2,000 days` |
| P6 | [06-detail-queries.md](06-detail-queries.md) | M-3, m-9 | `Stop collecting every row in session and user details` |
| P7 | [07-order-independent-costs.md](07-order-independent-costs.md) | M-4 | `Sum costs in fixed point so pages are stable` |

## Order

```
P1 ─▶ P2 ─▶ P3            (the SQL sandbox; strictly in order)
P6 ─▶ P7                  (both edit reads/users.rs)
P4, P5                    (any time)
```

- P1–P3 run in this order. P6 runs before P7.
- The two chains and P4, P5 may run in parallel, in separate worktrees. Their source files overlap only in `crates/storage/src/views/mod.rs`: P2 adds `pub(crate) mod guard;` and P7 changes the `sessions` re-export. Both are one-line changes, so a merge conflict there is trivial.
- Shared docs:
  - P1 creates the `## Track A2 — Query layer` section in `docs/mvp-decisions.md`. P2, P3, P5 and P7 add or extend a row in it; a phase that finds the section missing creates it exactly as P1 describes, and adds its own row.
  - `docs/spec/05-query.md` is edited by P1 (Sandbox items 1–2), P2 and P3 (Sandbox item 4), P5 (Dashboard metrics) and P7 (Logical views). Each phase edits only its own paragraphs.

## Decisions

Each decision goes into `docs/mvp-decisions.md`, section `## Track A2 — Query layer`, in the phase that implements it. The section sits after `## Track A1 — Storage engine` and before `## Open`, with the same table header as the other track sections (`| # | Topic | Decision |`). The exact row text is in each phase file.

| # | Topic | Phase |
|---|---|---|
| D-A2-1 | SQL statement caps: every left-deep chainable token counts as an operator; pipe syntax rejected; at most 32 tables per statement | P1 |
| D-A2-2 | Guarded scalar functions: deny-list, and a memory-pool reservation around every scalar function call except `get_field` | P2 (P3 appends one sentence) |
| D-A2-3 | `bucket=auto` past 2,000 days is a 400 | P5 |
| D-A2-4 | Costs are summed in fixed point (1e-18 USD) so sums do not depend on order | P7 |

Background for these choices, measured on this repository in a 2 MiB thread (release build unless noted). All are in the review or were measured while writing this plan:
- **Stack.** These chains overflow during planning or optimization:
  - 2,600 `UNION ALL SELECT 1`;
  - 1,000 `INTERSECT SELECT 1`;
  - 2,600 `AT TIME ZONE 'UTC'`;
  - 700 `CROSS JOIN` tables;
  - 1,000 comma-joined tables.

  These pass: 512 `UNION ALL`, 512 `INTERSECT` and 512 `AT TIME ZONE` (debug and release); 256 joined tables (debug); 512 joined tables (release).

  The cold review of this plan found three more chains that overflow in a debug build: 1,000 subscripts `['k']`, 4,000 pipe stages `|> WHERE true`, and 2,000 `~~ 'a'`. P1 covers them.
- **Planning time.** Planning grows superlinearly with joined tables: 64 tables plan in about 0.6 s, 128 in about 4 s, and 512 comma-joined tables did not finish in 10 minutes. Planning is synchronous, so the request timeout cannot interrupt it.
- **Memory.** DataFusion's memory pool never sees the arrays scalar functions allocate. Under a 64 MiB pool:
  - `SELECT array_length(range(0, 100000000))` raised peak RSS from 48 to 825 MiB;
  - `SELECT length(repeat('x', 400000000))` raised it to 1,598 MiB.

  Concatenation chained through CTEs (`WITH a AS (SELECT input || input AS x FROM spans), b AS (SELECT x || x AS x FROM a), …`) doubles the data at each step without any special function. On today's code it reaches several GB and then panics in arrow (an i32 offset overflow).
- **The query pool is unbounded today.** `Storage::open` builds one process-wide query runtime with DataFusion's default `UnboundedMemoryPool`, and spec 03 § 6 keeps it so until A1 M6. The user decided this plan depends on M6 for that. A1's M6.1 (`docs/plans/a1-storage/11-m6a-resources-hardening.md`) gives each query its own pool, capped at `query.max_memory` over a shared pool, whose exhaustion raises `ResourcesExhausted`. The guard (P2, P3) is complete and tested with bounded pools, and takes effect in production when M6.1 lands. Until then only the deny-list protects production.
- **Competitors.** The two competitors with a user SQL console (Laminar, Helicone) send it to ClickHouse with `max_memory_usage`, which ClickHouse enforces in its allocator. DataFusion has no equivalent. The guard (D-A2-2) is the in-process approximation.

## Conventions for every phase

- **Scope.** Edit only the files the phase lists. Phase files name every function and test to touch. If you need another file, stop and report.
- **Errors.** Use the existing `RunError` and `InvalidQuery` types; add no variant unless the phase says so. Messages never include data values, only caller text and names. `unwrap`/`expect` only in tests, or with a comment stating the invariant.
- **No panics** on caller input. No `tokio::spawn` or `spawn_blocking` (A2 rule, plan `a2-query` README).
- **Tests.**
  - Every behaviour in the phase has a test that fails without the change. The phase lists them; write all of them.
  - Use the existing helpers named in the phase (`example_query`, `span_row`, `spans_table`, …).
  - Assert exact values and messages.
  - Existing tests must keep passing unchanged unless the phase names the test and the new expectation.
- **Docs.** Spec and decision text is given verbatim in each phase. Write it exactly; adjust only the surrounding punctuation if the context requires it.
- **Checks.**
  - `just check` must pass before the commit. Rust-only iterations may use `just check-rust` or `cargo test -p llmobs-storage <filter>`.
  - Node is not on the non-interactive PATH on the dev machine: put the version from `.node-version` first on `PATH` before `just check`.
- **Commit.** One commit per phase, with the subject from the table above. Add any trailer your own session instructions require.
- **Report.** At the end, report:
  - what was done;
  - every deviation from the phase file and why;
  - anything left unverified;
  - the exact output of the phase's "Done when" commands, if any failed and were then fixed.

## After the plan

When all seven phases have landed, the coordinating session updates `docs/reviews/a2-query.md` (a "Fixed by" line under each fixed finding) and the A2 row of `docs/roadmap.md`. No phase does that.
