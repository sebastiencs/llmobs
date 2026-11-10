# A1 · S1 — Trace-list benchmark (spike)

Plan for spike S1 of the storage track. One agent runs it from start to end, alongside M1. It measures whether the `traces` view of the design is fast enough before M3 builds it. See `docs/plans/a1-storage/00-design.md` § Trace-list spike.

**This spike's code is throwaway.**
- It lives on its own branch, `spike/s1-trace-bench`, and is **never merged**.
- The only deliverable that reaches `main` is the results document, `docs/research/storage/traces-benchmark.md`, in a separate commit on a branch cut from `main`.
- The spike branch doesn't have to pass `just check`. The results commit does.

**Read first:**
- `docs/plans/a1-storage/00-design.md`: § Trace partials and the `traces` view, § Trace-list spike, § Trace anchor › Exit ramp.
- `docs/spec/01-data-model.md` § Trace (the merge table and the order rule), `docs/spec/03-storage.md` §3 (Parquet settings) and §6 (`TracesProvider`), `docs/spec/05-query.md` § Dashboard metrics.
- `docs/research/storage/README.md` (G15–G18: why the window and pushdown rules exist).
- `AGENTS.md` (§ Reference implementations: read, never copy).

## Questions the spike must answer

1. Does a plain DataFusion `GROUP BY trace_id` merge of partial rows (spec 01's merge rules) meet the latency targets for the trace list, filtered lists, a single trace and the dashboard?
2. How much do the widening-window fast path and day-close merging help?
3. Does DataFusion 55 prune Parquet files and row groups the way spec 03 §6 assumes?
   - by `start_time` statistics on files sorted by `start_time`;
   - by bloom filters on `trace_id` (`FixedSizeBinary(16)`) for equality;
   - TopK (`ORDER BY … LIMIT`) when applicable.
4. How expensive is the trace anchor's startup rebuild (spec 03 § Trace anchor)? That is: reading `trace_id` and `start_time` from the `traces` files of a 2-hour window around midnight.
5. **Conclusion:** one of the three outcomes in § Results.

## Setup

- **Branch:** `spike/s1-trace-bench`, from `main`.
- **Crate:** `tools/trace-bench/`, a binary crate added as a workspace member on the spike branch only.
  - Dependencies: `llmobs-core` (for `traces_schema()` and the `spans` schema), `datafusion`, `arrow`, `parquet`, `tokio`, `clap`, and `serde_json` for the results output.
  - Use the workspace pins; if anything else is needed, add it on the spike branch only.
  - Generate random data with a small seeded xorshift; no `rand` crate is needed.
- **Machine:** the dev machine. Record in the results:
  - CPU model, core count, RAM, disk type;
  - OS and kernel;
  - the exact commit.

  Run every query with `target_partitions = 8` to match spec 03's target box (8 cores), and once with the machine's full core count for comparison.
- **Commands** (subcommands of the binary):
  - `generate --out <dir> --layout <name> --seed <n>`: writes a dataset (§ Data);
  - `bench --data <dir> --variant <name> --runs <n>`: runs the queries (§ Queries, § Variants) and writes JSON results;
  - `oracle --data <dir>`: computes the reference answers (§ Correctness);
  - `anchor --data <dir>`: question 4.

## Data

One project, 7 days (2026-03-02 00:00 → 2026-03-09 00:00 UTC), **5,000,000 traces**, each with 10 spans.

**Traces:**
- `start_time`: uniform over the 7 days, with a daily cycle (twice the rate between 08:00 and 20:00 UTC).
- Duration: log-normal, median 2 s, p99 60 s.
- 2% of traces start within 1 h before midnight and end after it (they matter for the anchor numbers).
- `environment`: `production` 80%, `staging` 15%, `llmobs-eval` 5%.
- `session_id`: 30% of traces, drawn from 50,000 sessions. `user_id`: 60% of traces, from 20,000 users.
- `release`: 5 values. `tags`: 0–3 of 10 values.
- `metadata`: 0–4 keys from 20, short values.
- `name`: 30 distinct root names.
- `input_preview` / `output_preview`: 100–400 ASCII characters (realistic size, synthetic text).
- Counts: `error_count` > 0 on 3% of traces; `llm_count` 1–5; tokens and `cost_total` log-normal (median cost $0.002, p99 $0.20); `models`: 1–2 of 6 values.

**Partials:** each trace is written as one or more partial rows in `traces_schema()`, following spec 01's columns and how a flush would produce them:
- 60% of traces have one partial, with the root (`has_root = true`, `partial_count = 1`).
- 40% are split into 2 or 3 partials. Exactly one partial holds the root; the others have `has_root = false`, carry the counts and costs of their spans, and use the earliest of their spans' names and context fields. Their non-root `session_id` / `user_id` are null in half the cases, to exercise "root preferred".
- When a non-root partial was written, relative to the root partial's `start_time`:

  | Delay | Share |
  |---|---|
  | within 1 min | 80% |
  | 1–30 min | 15% |
  | 1–3 h | 5% |

  The last group is beyond `trace_lookback` (1 h), so the documented window limit shows up.
- `last_ingest_time` is the partial's write time; its `start_time` / `end_time` cover only its own spans.

**Files** (spec 03 §3 settings for `traces`):
- zstd level 3;
- row groups of 100,000 rows;
- page index on, page-level statistics;
- bloom filters on `trace_id`, `session_id` and `user_id` (fpp 0.01);
- rows sorted by `start_time`, with the sort order declared in the file metadata;
- the spec 03 KV metadata.

Files go under `<out>/day=<YYYY-MM-DD>/`. Two layouts:

| Layout | Files per day | Meaning |
|---|---|---|
| `l0` | one file per 10-minute flush window (144 per day) | Uncompacted: every partial sits in the window in which it was written. The worst case before compaction. |
| `closed80` | days 1–6 (≈ 80%): one file per day with exactly one merged partial per trace (`fully_merged`); day 7: as `l0` | Six closed days plus today, which is what day close (spec 03 §5) produces. |

**Memtable:** the newest 2 minutes of data are held as in-memory **span** batches, not partials. Generate the 10 spans per trace for that window only, in the `spans` schema. The queries compute partials from them on the fly, as `TracesProvider` will (spec 03 §6). Also generate a "busy" variant with 2 minutes at 2,000 traces/s (240,000 traces, 2.4 M spans), to see what a large memtable costs.

Report the dataset sizes: rows, files, bytes on disk per layout, and generation time.

## Queries

All queries run against a merged view written as SQL in the spike:
- a `UNION ALL` of the Parquet partials (registered as a listing table over the layout's files) and the partials computed from the memtable spans;
- then `GROUP BY trace_id` with the spec 01 merge aggregates:
  - `min(start_time)`, `max(end_time)`;
  - `bool_or(has_root)`;
  - sums of the counts, tokens and cost;
  - distinct union of `tags` and `models`;
  - the root partial's `name`, `root_span_id` and previews;
  - "first non-null, root preferred, then `(start_time, span_id)` order" for `session_id`, `user_id`, `environment` and `release` (use `first_value(... ORDER BY ...)` with a root-first key);
  - `status` per the spec 01 rule.

  `metadata` merging may be simplified to "root's map": say so in the results.
- Time filters on `start_time` are padded by `trace_lookback` (1 h) **on both sides** before the scan, and applied exactly after the merge (spec 03 §6).
- Other filters apply **after** the merge (the pushdown rule).

| # | Query | Range | Target (warm p95, 8 partitions) |
|---|---|---|---|
| Q1 | Latest 50 traces, ordered by merged `start_time` desc, no filter | whole 7 days | 200 ms |
| Q2 | Latest 50 traces with `cost_total > 0.05` | whole 7 days | 2 s |
| Q3 | Latest 50 traces with `environment = 'production'` | whole 7 days | 500 ms |
| Q4a | One trace by `trace_id`, with a time hint (files within ±1 day of the hint) | — | 100 ms |
| Q4b | One trace by `trace_id`, no hint (bloom filters across all files) | — | 1 s |
| Q5 | Dashboard: per hour over 7 days, trace count, error count, p50/p95/p99 of `end_time − start_time`, and `sum(cost_total)`, on merged traces | whole 7 days | 2 s |
| Q6 | Q1 restricted to one day in the middle of the range | one day | 200 ms |

For Q4a and Q4b, pick 100 trace ids at random (half split, half single) and report the distribution over them.

## Variants

| Variant | What runs |
|---|---|
| (a) `plain` | Each query as one SQL statement over the merged view, on layout `l0`. |
| (b) `widening` | Q1–Q3 and Q6 use the fast path of spec 03 §6: query the windows `[b − 15 min, b]`, `[b − 1 h, b]`, `[b − 6 h, b]`, `[b − 24 h, b]`, then the whole range, each padded by 1 h on both sides, stopping at the first window that yields 50 rows after the merge and the post-merge filter. Report how many windows each query needed. Layout `l0`. |
| (c) `closed` | Variant (b) on layout `closed80`. |
| (d) `pushdown` *(informational)* | Q3 with `environment = 'production'` pushed into the partial scan, which is **wrong** for traces whose non-root partials lack it. Report the speed-up and the number of wrong rows, so the cost of the correct rule is known. |

Run each query of each variant:
- **cold:** the first run in a fresh process, with no DataFusion caches. If the machine allows it (root), drop the OS page cache first (`echo 3 > /proc/sys/vm/drop_caches`) and say so. Otherwise say the page cache was warm.
- **warm:** 20 runs in the same process after one warm-up. Report p50, p95 and max.

For each variant and query, also record:
- peak memory (the DataFusion memory pool's peak, and the process RSS high-water mark from `/proc/self/status` `VmHWM`);
- rows and bytes scanned, and files and row groups pruned (from DataFusion's metrics, `EXPLAIN ANALYZE`);
- the physical plan (abridged) for one run.

## Correctness

The `oracle` command computes, for every query, the exact answer by merging **all** partials of every trace (no time window), and the variants are compared with it:
- Q1–Q3 and Q6: same trace ids in the same order, with equal merged values.
- Q4: identical rows.
- Q5: bucket values equal within floating-point tolerance.

Differences caused by partials more than 1 h away from the rest of their trace are expected (the documented window limit). Count them and report them separately. Any other difference is a bug in the spike or a flaw in the design, and must be explained in the results.

## The anchor's startup cost (`anchor` command)

Measure what spec 03 § Trace anchor needs at startup:
- read `trace_id` and `start_time` from the `traces` files whose time range touches a 2-hour window around one midnight, on both layouts;
- build the `trace_id → day` map in memory.

Report wall time, rows read, files opened and the map's memory size. Extrapolate linearly to 10× the trace rate.

## Results

Write `docs/research/storage/traces-benchmark.md`:
1. **Setup:** machine, commit, dataset sizes and generation time, any deviation from this plan.
2. **Results tables:** per variant and query, cold and warm p50/p95/max against the target, peak memory, rows/bytes scanned and pruning counts.
3. **DataFusion findings:**
   - whether `start_time` statistics pruned files and row groups;
   - whether `trace_id` bloom filters pruned (and with which literal form: binary, or hex rewritten);
   - whether TopK was used;
   - surprises in the plans.
4. **Correctness:** oracle comparison, the window-limit count, the `pushdown` variant's error count.
5. **Anchor:** the numbers of § The anchor's startup cost.
6. **Conclusion:** exactly one of the following, with the evidence for it:
   - **Holds as written:** targets met with variant (b) or (c); M3 builds `TracesProvider` as specified.
   - **Holds with a custom merge operator:** targets missed because of the generic `GROUP BY` (for example, hashing all partials when they are already sorted by `start_time`). Describe the operator that would fix it: a streaming merge over sorted partials, keyed by `trace_id`, with early termination for "latest N". Say which queries need it.
   - **Partials don't pay off:** the merge costs about as much as aggregating spans directly would. Then M3 is replanned to aggregate the `traces` view from `spans` at read time. If you reach this conclusion, also measure Q1 and Q5 computed from spans for one day, so the claim rests on numbers.
7. **Recommendations for M3:**
   - the latency budget per query;
   - the widening windows' sizes;
   - whether day close is needed for performance or only for tidiness;
   - whether the anchor's rebuild cost is acceptable (input for the § Exit ramp decision);
   - anything that should change in spec 03 §6.

Keep the document factual. Numbers come from the runs; label estimates as estimates. Don't paste large plans: abridge them to the operators that matter.

## Steps

The spike branch can have as many commits as useful; these are the steps.

1. **Generator.** `generate` for both layouts and the memtable spans, plus a `stats` printout (rows, files, bytes). Check the partial invariants on a sample: one root per trace, sums across partials equal the trace's totals, the delay distribution. Commit on the spike branch.
2. **Oracle.** `oracle`: a full merge, kept in memory or written to a reference Parquet file, with the expected answers per query.
3. **Bench harness.** The merged-view SQL, the queries, variants (a)–(d), the cold/warm protocol and metrics capture, JSON output. Verify the correctness comparison passes on a tiny dataset (10,000 traces) before the full one.
4. **Full runs.** Generate the full datasets, run every variant, keep the JSON outputs on the spike branch (`tools/trace-bench/results/`).
5. **Anchor measurement.**
6. **Results document.** On a new branch from `main` (`docs/s1-trace-bench-results`), add only `docs/research/storage/traces-benchmark.md`, and add a line linking it to `docs/research/storage/README.md` § Sources. Commit `docs: record the trace-list benchmark results`. `just check` must pass on that branch.

## Report

When done, report:
- the conclusion (one of the three) and the two or three numbers that decide it;
- every deviation from this plan, and why;
- whether a question of § Questions the spike must answer stayed open, and why;
- the name of the spike branch and of the results branch.

Do not open a PR for the spike branch.
