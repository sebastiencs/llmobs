# A1 · S1b — Trace-list benchmark, corrected rerun

Follow-up to spike S1 (`02-s1-trace-list-benchmark.md`). One agent runs it from start to end. It fixes the defects found in the first run and measures what that run left open. M3 is planned from this run's conclusion.

**Same rules as S1.**
- The code stays on `spike/s1-trace-bench` (new commits on top of `a97d387`) and is **never merged**.
- The deliverable is an updated `docs/research/storage/traces-benchmark.md`, committed on `docs/s1-trace-bench-results` (on top of `e5d01a0`).
- The spike branch doesn't have to pass `just check`. The results commit does.

**Read first:**
- `02-s1-trace-list-benchmark.md`: the original plan. Everything in it still applies unless this plan changes it.
- `docs/research/storage/traces-benchmark.md` on `docs/s1-trace-bench-results`: the first run's results.
- `docs/spec/01-data-model.md` § Trace (merge table) and `docs/spec/03-storage.md` §6 (window, widening, pushdown).

## Why a rerun

The first run concluded "holds with a custom merge operator". That conclusion rests on the widening variant returning 49 wrong ids out of 50. That failure is mostly a generator artifact, not a property of the design:

- **Late spans pile up at the end of the range.** `model.rs:268` clamps a late span's start to `RANGE_END − 2 s`:

  ```rust
  let span_start = (start + delay as i64 * NS).min(RANGE_END - 2 * NS);
  ```

  Every trace that starts in the last 1–3 h of the range and has a late partial puts that partial at one instant, later than every real trace. The 15-minute window picks those partials first. Their roots are outside the 1 h pad, so each one looks like a new trace. In real data a span cannot start after "now". Long traces still produce this error, but in proportion to their share of recent spans, not 49 of 50.
- **The day-close fold breaks spec 01's merge rules.** `generate.rs` `merge_row` copies the root partial's `tags`, `session`, `user`, `environment` and `release` over the accumulator. Spec 01 says `tags` is a union, and the context fields are "first non-null, root preferred". This is why closed Q4 disagrees on 44 of 99 lookups.
- **The anchor rebuild inserts every row it reads.** `anchor.rs` `push_ids` puts every row of every touched file in the map, including rows hours away from midnight. On `closed80` the map has 1.43 M entries for a whole day. The 80 MB figure, and its 803 MB extrapolation, overstate the cost.
- **Selective filtered lists were never timed deep.** With the artifact, Q2 found 50 rows in the first 15-minute window. Nothing measured a filter that forces the 6 h or 24 h window, which is the real question for the 2 s target.
- **Mismatches are not attributed.** The JSON does not say which ids differ, so the busy-memtable disagreements (2 on Q1, 2 on Q3, one Q5 hour) are unexplained.
- **The machine was not recorded.**

## Changes to the harness

Make each change as its own commit on `spike/s1-trace-bench`.

### C1. No spans after the end of the range

In `model.rs`, drop a late (non-root) partial entirely when its unclamped span start, or its unclamped write time, is at or after `RANGE_END`. Spans that would arrive after "now" do not exist yet. Remove the `.min(RANGE_END − …)` clamps on late span starts and late write times (`model.rs:268`, `:269`, `:340`).

- Drop the whole partial, not single spans. The delay is per partial, so all of its spans fall past the end together.
- A trace that loses partials keeps its root and its other partials. Its totals are what the remaining spans add up to. The oracle computes from the generated data, so it stays consistent.
- The root span's end clamp (`model.rs:249`) stays: a trace may still be running at the end of the range.
- Add to the `stats` printout:
  - the number of dropped late partials;
  - the number of partials whose `start_time` falls in the last 10 seconds of the range, next to the number of roots in that interval.

  Late partials must no longer outnumber roots there. The generator fails if they do.

### C2. Day close follows spec 01

Rewrite `generate.rs` `merge_row` so that the fold of a trace's partials into one `fully_merged` row applies spec 01's merge table to every column:
- `tags` and `models`: sorted distinct union;
- `session_id`, `user_id`, `environment`, `release`: the root partial's value if non-null, else the first non-null in `(start_time, span_id)` order;
- `partial_count`: 1 (spec 01: "= 1 after full compaction");
- every other column as the merge SQL already does.

Add a check to `oracle`: for each `closed80` closed day, the fully merged row of every trace equals the oracle's merged trace on every compared column. Any difference fails the run.

### C3. Anchor rebuild reads only the boundary window

In `anchor.rs`:
- insert a row into the map only if its `start_time` is within `[midnight − 1 h, midnight + 1 h]`;
- skip row groups whose `start_time` min/max lie outside that window, using the row-group statistics, before reading them. Files are sorted by `start_time`, so a closed day file should read about 2 h of its rows, not the whole day.

Report for both layouts:
- files touched;
- row groups read out of the total;
- rows read;
- map entries;
- map bytes;
- wall time;
- the 10× estimates, labelled as estimates.

### C4. Mismatch attribution

In `bench.rs`, for every latest-N comparison (Q1–Q3, Q6, and the new Q2r), record in the JSON each differing position:
- the returned id;
- the expected id;
- the class:
  - **`window_limit`**: the oracle marks the returned or the expected trace as `late`, meaning its partials span more than 1 h;
  - **`other`**: everything else.

Keep at most 20 entries per query in the JSON. Always keep the counts per class.

For Q5, record the hours that differ with both values.

Rerun the busy-memtable variant and explain each `other` difference in the results. A difference with no explanation is a bug to fix before the results are written.

### C5. Memory cap

The session setup in `bench.rs` already builds a `GreedyMemoryPool`. Add a `--memory-limit` flag, default 4 GiB.
- A query that fails with `ResourcesExhausted` is recorded as `exceeds <limit>` with its peak reservation, and the run continues.
- Never raise the limit to make a query pass.
- Do not rerun plain Q1, Q2, Q3 or Q6 over the whole week. The first run already measured them (13–14 s, about 11 GiB of pool, 25 GiB RSS on the machine that ran it). They need not run again.

### C6. Machine record

Add to `manifest.json`:
- `lscpu` model name;
- `nproc`;
- total RAM from `/proc/meminfo`;
- the data directory's disk (`lsblk -d -o NAME,ROTA,MODEL` for the device holding `--data`);
- `uname -r`;
- the spike commit;
- whether the page cache was dropped.

## New measurements

### Q2r: a selective filtered list

Add two queries of the Q2 form, with thresholds chosen from the oracle:

| # | Filter | Chosen so that |
|---|---|---|
| Q2r-2h | `cost_total > t₁` | the 50th newest match starts about 2 h before the end of the range |
| Q2r-15h | `cost_total > t₂` | the 50th newest match starts about 15 h before the end |

Compute `t₁` and `t₂` in `oracle` and write them into the answers file, so `bench` reads them instead of hard-coding them. Report the exact thresholds. Q2r uses the same target as Q2 (2 s, warm p95, 8 partitions).

### Window cost curve

For Q1, Q2, Q2r-2h, Q2r-15h, Q3 and Q6, on `l0` and on `closed80`, run each fixed window by itself, without the stop rule:
- 15 min, 1 h, 3 h, 6 h and 24 h;
- each padded by 1 h on both sides;
- `target_partitions` 8.

Per window, report:
- warm p50 / p95;
- rows returned after the post-merge filter;
- pool peak;
- bytes scanned and file ranges kept;
- mismatches by class against the oracle's answer for that window (the merged traces whose oracle `start_time` is inside the window).

Skip a window once the previous window for the same query exceeded the memory limit.

This replaces the first run's linear "≈ 0.6 s for 3 h" estimate with measurements.

### Widening, rerun

Rerun variants `widening` (`l0`) and `closed` (`closed80`) exactly as specified in S1, adding Q2r-2h and Q2r-15h, with C1–C5 in place. Report:
- the windows each query needed;
- the timings;
- mismatches by class.

### Plain, partial rerun

Rerun only plain Q4a, Q4b and Q5 on `l0`. Q4 and Q5 also run on `closed80` (with C2's fold). Their data changed in the last hours of the range, so check they still match the oracle.

## Run protocol

- Regenerate both layouts, the memtable and the busy memtable with seed 1, then run `oracle`.
- If the run has root access, drop the page cache before each cold run. Otherwise say so, as before.
- 20 warm runs per query, as before. The exception is the window cost curve, which uses 10 warm runs per window.
- Keep the JSON in `tools/trace-bench/results/`, replacing the first run's files. The first run stays in git history at `a97d387`.

## Results

Update `docs/research/storage/traces-benchmark.md` on `docs/s1-trace-bench-results`. It describes this run. Where a first-run number is kept (plain Q1–Q3 and Q6), label it as from the first run, with that run's commit.

Required changes to the document:

1. **Setup:**
   - the machine record;
   - the dataset sizes after C1, including dropped late partials;
   - the deviations list.

   Remove the deviations that C1 and C2 fixed. Keep the late-span-offset deviation and say why.
2. **Results:**
   - the tables for widening, closed, plain Q4/Q5, and busy;
   - the new window cost curve table.
3. **Correctness:**
   - mismatches by class for every query;
   - an explanation of each `other` difference.
4. **Anchor:** the C3 numbers.
5. **Conclusion:** exactly one of S1's three outcomes, decided by these rules.
   - **Holds as written** if all of these are true:
     - widening Q1, Q3 and Q6 meet their targets;
     - Q2 and Q2r-2h meet 2 s;
     - every mismatch is `window_limit`.

     State Q2r-15h's time either way: it is the cost of a rare filter.
   - **Holds with a custom merge operator** if a correct window misses a target because of the `GROUP BY` itself. That means the window cost curve shows time growing with rows hashed, not with bytes read. Name the queries and the window sizes where it happens.
   - **Partials don't pay off** only if the window cost curve shows the merge costs about as much as aggregating spans. In that case, measure Q1 and Q5 from spans for one day, as S1 specified.
6. **Recommendations for M3:**
   - the widening ladder;
   - the latency budget per query;
   - what latest-N returns for traces longer than `trace_lookback`, with the measured share of `window_limit` rows in Q1's answer under this delay mix;
   - day close (performance or tidiness);
   - the anchor rebuild cost (input for the § Exit ramp decision in `00-design.md`);
   - spec 03 §6 changes.

   Replace the first run's recommendations. Do not keep a recommendation that depended on the clamp artifact.

Commit with `docs: rerun the trace-list benchmark with corrected data`. `just check` must pass.

## Report

When done, report:
- the conclusion and the numbers that decide it;
- the Q2r thresholds and the windows Q2r needed;
- the share of `window_limit` mismatches in Q1;
- every deviation from this plan, and why;
- anything left open.
