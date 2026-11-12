# Trace-list benchmark — M3 implementation

The S1/S1b spike results are recorded in this file on the
`docs/s1-trace-bench-results` branch (a separate results branch, never merged
into `main`). This branch carries the **M3 implementation** section; the
coordinating session combines the two. Numbers below come from the runs;
anything not measured is labelled as pending.

## M3 implementation

### Harness

`crates/storage/tests/trace_bench.rs` is an `#[ignore]` integration test, run
by hand on the same server as S1b:

```bash
LLMOBS_TRACE_BENCH_DIR=/path/to/data cargo test --release \
    -p llmobs-storage --test trace_bench -- --ignored --nocapture
```

`LLMOBS_TRACE_BENCH_TRACES` overrides the trace count (default 5,000,000).
`LLMOBS_TRACE_BENCH_DIR` is generated once (when it holds no `catalog.db`) and
reused afterwards; a `bench-manifest.json` keeps the dataset info and Q4 id
sample across runs. The Q2r thresholds are recomputed each run with untimed
range-mode queries, as S1b did. The data directory is never committed.

Generation uses a seeded xorshift (seed 1), 10 spans per trace over 7 days from
2026-03-02 00:00 UTC, a day/night rate cycle, log-normal duration/cost/tokens,
the environment / session / user / release / tag / name / preview / model mix,
3% error traces, and 40% of traces split into 2–3 groups whose non-root groups
start 0–1 min / 1–30 min / 1–3 h after the root (the last group exercises the
window limit). Groups are appended in `ingest_time` order in batches of about
10,000 spans. `wal.sync = 100ms`, `flush.max_age = 1h`, `flush.max_bytes` starts
at 128 MiB and is scaled once by `count / 1000` if the spans file count falls
outside 900–1,100. Everything is appended and flushed, then the storage is
reopened and the last two minutes of `ingest_time` are appended without a
shutdown, so the tail stays in the WAL and replays into the memtable on every
later open.

Every query runs through `QueryContext::new(storage.query(project, options), None)`
and A2's reads at `target_partitions = 8`: one cold run, then 20 warm runs
(p50 / p95 / max). Q4a builds its own context with `around` inside the timing,
Q4b uses one context; each of 99 lookups is one sample. Q5 is issued as SQL
through the context's session. Each list result is compared with the same
request over a context whose `optimizer_rules` were emptied (range mode), and the
differing ids are reported — only window-limit traces may differ. The physical
plan of each list query is printed as `list` or `range`; a missed target also
prints an `EXPLAIN ANALYZE` plan.

### Machine

Validation runs below were made on a machine **different from the S1b server**:
AMD Ryzen 9 3950X (16 cores / 32 threads), 65,727,192 kB RAM (~64 GiB),
disk `sda` (rotational, model "STORAGE DEVICE"), kernel
7.2.15-200.fc44.x86_64, commit `1f2fcc22c610`. The S1b server was an
Intel Core i7-8700 (12 threads, 126 GiB RAM, kernel 7.0.0-38-generic), so the
two sets of numbers are not directly comparable. Page cache was left warm.

### Results

**The 5,000,000-trace target run is pending.** The plan requires it to be run
by hand on the S1b server, in `--release`, which was not available here. Per
the plan ("don't tune past this sub-phase: report it, and the coordinating
session decides"), no target is claimed as met or missed until that run happens;
the table below is a reduced validation run, not the target verdict.

#### Validation run (reduced dataset, debug build — not comparable with targets)

`LLMOBS_TRACE_BENCH_TRACES=100000`, 100,000 traces, 11 spans files (after one
regeneration), 37,344,116 bytes on disk, generated in 23.3 s, 2,970 traces
with a 1–3 h late group. Times are milliseconds. All list queries used `list`
mode; `differing ids` is 0 for every list query (no window-limit differences at
this size). These numbers show the harness and the real engine run end to end;
they are **not** the M3 exit-criterion results.

| query | mode | cold | p50 | p95 | max | target | pass | rows | differing ids |
|---|---|---:|---:|---:|---:|---:|---|---:|---:|
| Q1 | list | 13.7 | 12.8 | 14.7 | 15.7 | 200 | pass | 50 | 0 |
| Q2 | list | 32.0 | 32.0 | 35.1 | 37.3 | 2000 | pass | 50 | 0 |
| Q2r-2h | list | 34.0 | 32.1 | 34.7 | 35.5 | 2000 | pass | 50 | 0 |
| Q2r-15h | list | 49.3 | 48.1 | 51.2 | 52.8 | 2000 | pass | 50 | 0 |
| Q3 | list | 13.0 | 12.7 | 13.7 | 13.9 | 500 | pass | 50 | 0 |
| Q6 | list | 13.0 | 12.2 | 13.5 | 14.1 | 200 | pass | 50 | 0 |
| Q4a | lookup | 14.3 | 18.4 | 21.0 | 22.1 | 100 | pass | 99 | 0 |
| Q4b | lookup | 15.7 | 18.9 | 21.5 | 22.8 | 1000 | pass | 99 | 0 |
| Q5 | sql | 101.7 | 90.2 | 103.1 | 105.9 | 2000 | pass | 168 | 0 |

Informational `metrics` (7 days, `H1` buckets) on the same run: cold 372 ms,
p50 373 ms, p95 403 ms, max 407 ms — no target.

### Comparison with S1b

S1b measured a *spike*: a SQL `GROUP BY trace_id` merge over pre-built Parquet
partials on the i7-8700, with a widening-window fast path layered on top. Its
8-partition warm p95 on the `l0` layout was Q1 203 ms, Q2 196 ms, Q2r-2h
1,802 ms, Q2r-15h 6,184 ms (a miss), Q3 182 ms, Q6 282 ms (a miss), and
dedicated Q4a 102 ms (a 2 ms miss), Q4b 123 ms, Q5 669 ms. M3 replaces that
spike with the real
engine: partials are written at flush, the merge is a `TraceMerger` operator
inside `TracesExec`, and the widening windows are now `TracesExec`'s list mode
installed by `TraceListRule` (M3.7) — the same 15 min / 1 h / 6 h / 24 h /
whole-range ladder, but ranked and fetched from live files plus the memtable
rather than a static listing. Both agree on the shape of the problem: the rare
cost filter (Q2r-15h) is the expensive case and the 1–3 h late groups are the
documented window-limit differences. A like-for-like verdict needs the full
5,000,000-trace `--release` run on the S1b server with the command above; the
reduced numbers here cannot substitute for it.
