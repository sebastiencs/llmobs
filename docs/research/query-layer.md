# Query layer research

How GreptimeDB, Parseable and OpenObserve build their query layer on DataFusion, compared with the A2 plan (`docs/plans/a2-query/`). Gathered on 2026-11-10 from the same checkouts as the storage research (`docs/research/storage/`, branch `worktree-a1-storage-design`):

| Source | Snapshot | DataFusion |
|---|---|---|
| GreptimeDB | `edbcb8224`, 2026-09-21 | 55.1 (forked, with a forked sqlparser) |
| Parseable | `503fa56`, 2026-09-18 | 55.0 |
| OpenObserve | `8dcaf5e8e`, 2026-09-20 | 55.1 |

Paths are relative to each repository. Nothing was copied (AGENTS.md § Reference implementations). Points marked "(inferred)" come from reading the code; they were not run.

## Comparison

| | GreptimeDB | Parseable | OpenObserve | A2 plan |
|---|---|---|---|---|
| Session | `with_default_features()` plus many custom analyzer, optimizer and physical rules (`src/query/src/query_engine/state.rs:140-304`) | One global `SessionContext`, `with_default_features()` (`src/query/mod.rs:94-106, 346`) | One context per query, `with_default_features()`, optimizer rule list replaced by a hand-picked one (`src/search/src/datafusion/exec.rs:293-309`, `optimizer/mod.rs:91-179`) | Explicit function registry, no table functions, no `information_schema`, recursive CTEs off (D91) |
| Read-only check | Hand-written statement dispatch; plan check looks at the root node only (`src/frontend/src/instance.rs:1798-1800`) | None; DDL, `SET`, `COPY` would be planned (inferred) | None; tables must resolve to streams | One query statement, `EXPLAIN` without `ANALYZE`, `SQLOptions` re-check on the whole plan (D96) |
| Stack protection | 8 MB stacks in debug builds only (`src/cmd/src/bin/greptime.rs:81-92`) | None | 16 MB stacks on every runtime after real overflows (`d6a80fd66`) | Size and operator-count limits before parsing (D95) |
| Time range | Optional | Required; a filter wraps every table scan, subqueries included (`src/query/mod.rs:907-966`) | Required; per-file time pruning, filter only on boundary files | Read endpoints: required where it matters. SQL console: optional window in the views (D101) |
| Non-SQL filters | None | Alert conditions compiled to SQL text; injection bug `d2507cc` | Alert conditions compiled with `format!`; quoting added late (`4e2949868`), group-by columns unchecked | DSL parsed and validated on the AST, compiled to `Expr` (D95–D97, D100) |
| Parameters | Postgres/MySQL placeholders; MySQL fallback splices text into SQL (`src/servers/src/mysql/handler.rs:730-760`) | None | None | Typed `{name:Type}` bound on the plan (D99) |
| Timeout | Remaining budget per `next()`, cancel-on-drop wrapper (`instance.rs:630-676`, `src/frontend/src/stream_wrapper.rs`) | `timeout` around a spawned task; streaming escapes it; unbounded channel (`src/query/mod.rs:200-219, 418-445`) | `select!` with abort, but detached tasks outlive the client (`src/search_service/src/lib.rs:201`) | `timeout` around the future; stream deadline for exports; no detached tasks |
| Memory | One global pool, `TrackConsumersPool` | One global pool at 85 % of RAM, RSS-polling admission | Full-size pool per query, no global budget in OSS | Per-query pool from A1, admission in A1 M6 |
| Errors | Out-of-memory → 500, raw text; only `Internal` masked | Raw text; execution errors → 500 | Classified by message substring; default 400 | Classified by error variant (D94) |
| Results | Fully collected; drains past `limit` (`src/servers/src/http/handler.rs:232-263, 710-777`) | arrow-json → bytes → serde with errors swallowed; no row cap | arrow-json → bytes → serde, then truncated | `limit + 1` before encoding, 10k cap, streaming exports (D98) |
| Paging | None on HTTP | Keyset only in log context (`src/handlers/http/query_context.rs`) | Offset; result cache; newest-first time partitions | Keyset cursors (D92) |
| Histogram | `RANGE … ALIGN`; gaps filled only between each series' first and last point | Bins from `startTime`; approximate counts from manifests | Interval ladder with no bucket cap; fixed time-zone offset breaks across DST | Auto ≤ 120, explicit ≤ 2,000 buckets, epoch-aligned, full grid filled |

## Changes made to the A2 plan

- **D101, SQL time window.** All three bound queries by time. The window is written into the views, so it reaches every scan, including joins, subqueries and CTEs (checked on DataFusion 55.2). It stays optional: without it, the timeout, memory cap and admission bound the query.
- **Buckets aligned to their width from the epoch** (Q18), as Parseable's and OpenObserve's `date_bin` origins are, instead of starting at `from`.
- **Error classification unwraps `Diagnostic`** as well as `Context` and `Shared` (Q12), as GreptimeDB does (`src/common/query/src/error.rs:269-297`).
- **No detached work** (README conventions), and tests that cancelling a query or dropping an export stream releases its memory (Q12, Q13).
- **`LIMIT {n:Int}`** is tested (Q11); GreptimeDB had to special-case `LIMIT` placeholders (`src/query/src/planner.rs:598-623`).
- **Server and A1 notes:** larger worker stacks as a backstop to D95; `TrackConsumersPool`; admission on reserved memory; TopK dynamic filters for the latest-N trace list (GreptimeDB `b623cb1aa`).

## Checked on DataFusion 55.2

- String timestamp literals (`'2026-01-01T10:00:03Z'`, `'2026-01-01 10:00:03'`, `BETWEEN`) reach the provider as typed `TimestampNanosecond` bounds. GreptimeDB needs a rule for this (`TypeConversionRule`) because of its session time zones; we don't.
- A filter on the raw scan inside a view reaches the provider in every scan of the view.
- Dropping a running query's stream releases its memory, but late: 1.6 s for an 8 GB sort.

## Not adopted

- **Result cache, newest-first time partitions** (OpenObserve). They pay off with a distributed engine reading S3. A1's latest-N path covers the main case.
- **Replacing DataFusion's optimizer rule list** (OpenObserve). With pinned versions it only loses upstream fixes.
- **Forking DataFusion or sqlparser** (GreptimeDB). Upgrade cost.
- **Manifest-only counts and histograms** (Parseable). Approximate, and they double-count boundary files.
- **Offset paging** (OpenObserve). Each partition fetches `from + size` rows.
