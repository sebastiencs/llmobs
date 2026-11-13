# Storage benchmarks — M6.6

Results of the `#[ignore]` benchmarks in `crates/storage/tests/ingest_bench.rs` and
`crates/storage/tests/trace_bench.rs` (D-A1-12), run by hand in `--release`.
Each run lists its command, machine, dataset and printed output. The numbers
are checked against spec 03 § Capacity sanity check in the last section.

## Machine and environment

- CPU AMD Ryzen 9 3950X (16 cores, 32 threads), 65,727,192 kB RAM, disk `sda`
  (rotational, "STORAGE DEVICE"), kernel 7.2.15-200.fc44.x86_64.
- Local runs (1, 3): built and run at commit `5e4061a` (M6.5). The multipart
  bug fixed in `e73fe87` is on the remote upload path only, so local numbers
  do not depend on it.
- S3 runs (2, 4, 5, 6): built and run at commit `e73fe87` (after the fix).
- S3 server: Versity Gateway v1.11.0 on `127.0.0.1:7070`, a 20 GiB tmpfs
  (`VGW_BACKEND=posix`). **The server runs on the same machine as the
  benchmark**, so S3 latency here is a local-loopback round trip: cold-cache
  numbers understate what a real S3 endpoint costs per request. Treat the S3
  rows as the engine's overhead on an S3 API, not as network-bound S3 latency.
- The benchmark binaries were built with `RUST_MIN_STACK=33554432` (one release
  build crashed with SIGSEGV in rustc without it).

Common environment for every S3 run:

```bash
export LLMOBS_TEST_S3_ENDPOINT=http://127.0.0.1:7070 LLMOBS_TEST_S3_BUCKET=llmobs-bench \
  AWS_ACCESS_KEY_ID=llmobs AWS_SECRET_ACCESS_KEY=llmobs-secret
```

## Runs

| # | Benchmark | Store | Config | Result |
|---|---|---|---|---|
| 1 | ingest 60 s | local disk | defaults + the bench config below | measured |
| 2 | ingest 60 s | S3 (`llmobs-bench/ingest-bench-<uuidv7>`) | same | measured |
| 3 | trace_bench, 1,000,000 traces | local disk | defaults | measured |
| 4 | trace_bench, 1,000,000 traces | S3 `trace-bench` | `LLMOBS_TRACE_BENCH_CACHE=cold` | measured |
| 5 | trace_bench, 1,000,000 traces | S3 `trace-bench` | `warm` | measured |
| 6 | trace_bench, 1,000,000 traces | S3 `trace-bench` | `off` | measured |

The ingest bench config is the defaults except `min_free_bytes = 0`,
`flush.max_bytes = 256 MiB`, `memtable.hard_limit = 1 GiB`, and
**`compaction.interval = 1 h`**. The last one is a deviation: the config
validator rejects 24 h (the maximum is 1 h), and the plan's default 30 s would
let the background compaction task run inside the measurement. With 1 h it does
not run during a bench of these lengths, so compaction is measured only by the
explicit passes.

### Run 1 — ingest, local disk

```bash
LLMOBS_INGEST_BENCH_DIR=<scratchpad>/bench/ingest-local LLMOBS_INGEST_BENCH_SECS=60 \
  cargo test --release -p llmobs-storage --test ingest_bench -- --ignored --nocapture
```

```
ingest: 5165000 spans in 61.7 s = 83671 spans/s, 224.4 MiB/s Arrow, 2812 Arrow bytes per span, 23294 backpressure retries
drain: shutdown flushed in 2.09 s; flushed object bytes 768133073
compaction: 2 passes, 1 jobs, 0 failed, 34.7 s, input 765645153 bytes (21.1 MiB/s), output 764887786 bytes
write amplification: 2.00 = (flush 768133073 + compaction output 764887786) / flush 768133073
```

Eight writers, batches of 1,000 spans, 10 spans per trace. 10% of spans are
LLM spans with 10 KiB input and output of random lowercase text; the rest carry
a 300-byte attribute. Measured Arrow size: 2,812 bytes per span on average.
The ingest time (61.7 s) is measured from the start to the moment all writers
had joined; the writers stop at the 60 s deadline but finish their in-flight
append (including backpressure retries) first.

### Run 2 — ingest, S3

```bash
LLMOBS_INGEST_BENCH_DIR=<scratchpad>/bench/ingest-s3 LLMOBS_INGEST_BENCH_SECS=60 \
  cargo test --release -p llmobs-storage --test ingest_bench -- --ignored --nocapture
  # with the common S3 environment above
```

```
ingest: 5005000 spans in 60.0 s = 83374 spans/s, 223.6 MiB/s Arrow, 2812 Arrow bytes per span, 23491 backpressure retries
drain: shutdown flushed in 2.64 s; flushed object bytes 745637964
compaction: 2 passes, 1 jobs, 0 failed, 34.5 s, input 743191079 bytes (20.6 MiB/s), output 738324619 bytes
write amplification: 1.99 = (flush 745637964 + compaction output 738324619) / flush 745637964
```

The first S3 attempt of this run had a failed compaction job; see Findings (b).
The prefix was deleted at the end of the run.

### Run 3 — trace_bench, local disk, 1,000,000 traces

```bash
LLMOBS_TRACE_BENCH_DIR=<scratchpad>/bench/trace-local LLMOBS_TRACE_BENCH_TRACES=1000000 \
  cargo test --release -p llmobs-storage --test trace_bench -- --ignored --nocapture
```

Dataset: 1,000,000 traces, 31 spans files, 719,041,050 bytes. Generation
77.5 s. `max_bytes` was scaled to 3.8 MiB (see Findings (c)). 29,353 traces with a
1–3 h late group. `target_partitions` 8.

| query | mode | cold | p50 | p95 | max | target | pass | rows | differing ids |
|---|---|---:|---:|---:|---:|---:|---|---:|---:|
| Q1 | list | 20.5 | 12.3 | 16.2 | 16.6 | 200 | pass | 50 | 0 |
| Q2 | list | 41.9 | 36.2 | 38.0 | 39.3 | 2000 | pass | 50 | 0 |
| Q2r-2h | list | 153.3 | 122.7 | 132.9 | 142.2 | 2000 | pass | 50 | 0 |
| Q2r-15h | list | 563.6 | 506.9 | 604.9 | 622.6 | 2000 | pass | 50 | 0 |
| Q3 | list | 12.0 | 11.6 | 12.6 | 12.9 | 500 | pass | 50 | 0 |
| Q6 | list | 22.8 | 20.8 | 21.8 | 22.4 | 200 | pass | 50 | 2 |
| Q4a | lookup | 15.2 | 18.0 | 24.6 | 34.1 | 100 | pass | 99 | 0 |
| Q4b | lookup | 22.7 | 30.8 | 36.9 | 54.1 | 1000 | pass | 99 | 0 |
| Q5 | sql | 1105.9 | 556.8 | 1127.0 | 1211.1 | 2000 | pass | 168 | 0 |

Times in milliseconds. Q6's two differing ids are window-limit traces, as in
the M3 harness. The informational `metrics` over 7 days: cold 1939.1 ms,
p50 2144.6 ms, p95 3351.7 ms.

### Run 4 — trace_bench, S3, cold cache

```bash
LLMOBS_TRACE_BENCH_DIR=<scratchpad>/bench/trace-s3 LLMOBS_TRACE_BENCH_TRACES=1000000 \
  LLMOBS_TRACE_BENCH_CACHE=cold cargo test ... trace_bench -- --ignored --nocapture
  # with the common S3 environment above
```

The directory was empty, so the dataset was generated onto S3 first: 28 spans
files at 128 MiB, regenerated once at `max_bytes` 3.6 MiB into 32 files,
837,362,707 bytes, generation 74.0 s.

| query | mode | cold | p50 | p95 | max | target | pass | rows | differing ids |
|---|---|---:|---:|---:|---:|---:|---|---:|---:|
| Q1 | list | 16.0 | 12.5 | 13.4 | 15.4 | 200 | pass | 50 | 0 |
| Q2 | list | 42.4 | 35.4 | 38.5 | 41.1 | 2000 | pass | 50 | 0 |
| Q2r-2h | list | 110.7 | 113.1 | 143.7 | 156.9 | 2000 | pass | 50 | 0 |
| Q2r-15h | list | 316.1 | 322.8 | 364.8 | 386.0 | 2000 | pass | 50 | 0 |
| Q3 | list | 12.5 | 12.5 | 13.5 | 14.0 | 500 | pass | 50 | 0 |
| Q6 | list | 22.5 | 24.1 | 26.4 | 27.0 | 200 | pass | 50 | 0 |
| Q4a | lookup | 15.7 | 21.3 | 31.3 | 41.7 | 100 | pass | 99 | 0 |
| Q4b | lookup | 19.3 | 22.1 | 25.1 | 28.7 | 1000 | pass | 99 | 0 |
| Q5 | sql | 477.3 | 933.2 | 1033.0 | 1081.0 | 2000 | pass | 168 | 0 |

Read cache after the run: hits 22,409, misses 283, 495 blocks, 493,671,741
bytes. The cold run's first query was the only one that started with an empty
cache. Later queries of the same run already hit blocks that earlier queries had
fetched (see Findings (d)).

### Run 5 — trace_bench, S3, warm cache

Same dataset and command with `LLMOBS_TRACE_BENCH_CACHE=warm`. The whole query
set ran once untimed, then again measured.

| query | mode | cold | p50 | p95 | max | target | pass | rows | differing ids |
|---|---|---:|---:|---:|---:|---:|---|---:|---:|
| Q1 | list | 14.5 | 14.0 | 16.1 | 16.1 | 200 | pass | 50 | 0 |
| Q2 | list | 34.2 | 34.3 | 39.6 | 40.7 | 2000 | pass | 50 | 0 |
| Q2r-2h | list | 125.9 | 104.2 | 107.4 | 111.8 | 2000 | pass | 50 | 0 |
| Q2r-15h | list | 510.6 | 490.3 | 558.8 | 566.3 | 2000 | pass | 50 | 0 |
| Q3 | list | 12.6 | 12.3 | 13.8 | 30.7 | 500 | pass | 50 | 0 |
| Q6 | list | 20.4 | 22.3 | 22.8 | 22.8 | 200 | pass | 50 | 0 |
| Q4a | lookup | 18.1 | 21.4 | 26.6 | 33.6 | 100 | pass | 99 | 0 |
| Q4b | lookup | 16.9 | 21.5 | 23.4 | 24.3 | 1000 | pass | 99 | 0 |
| Q5 | sql | 468.4 | 473.7 | 524.4 | 599.8 | 2000 | pass | 168 | 0 |

Cache after the untimed pass: hits 13,359, misses 0. After the measured pass:
hits 31,813, misses 0, 559 blocks. The measured pass reads no bytes from S3.
Q3's max (30.7 ms) is one outlier; its p95 is 13.8 ms.

### Run 6 — trace_bench, S3, cache off

Same dataset and command with `LLMOBS_TRACE_BENCH_CACHE=off`
(`query.read_cache_bytes = 0`): every read goes to S3.

| query | mode | cold | p50 | p95 | max | target | pass | rows | differing ids |
|---|---|---:|---:|---:|---:|---:|---|---:|---:|
| Q1 | list | 32.7 | 21.6 | 23.1 | 24.1 | 200 | pass | 50 | 0 |
| Q2 | list | 65.9 | 57.7 | 67.7 | 72.9 | 2000 | pass | 50 | 0 |
| Q2r-2h | list | 185.4 | 161.6 | 176.0 | 178.3 | 2000 | pass | 50 | 0 |
| Q2r-15h | list | 580.7 | 544.4 | 595.5 | 616.2 | 2000 | pass | 50 | 0 |
| Q3 | list | 20.5 | 18.4 | 19.7 | 19.8 | 500 | pass | 50 | 0 |
| Q6 | list | 34.7 | 34.0 | 36.6 | 36.8 | 200 | pass | 50 | 0 |
| Q4a | lookup | 31.0 | 36.8 | 46.8 | 53.5 | 100 | pass | 99 | 0 |
| Q4b | lookup | 32.9 | 39.8 | 45.2 | 46.2 | 1000 | pass | 99 | 0 |
| Q5 | sql | 534.5 | 495.9 | 545.5 | 617.3 | 2000 | pass | 168 | 0 |

All nine targets pass in every run, local and S3. With the cache off, the S3
round trip costs about 1.5–2× the local p50 on the list queries (Q1 21.6 ms
against 12.3 ms local, Q2r-15h 544 ms against 507 ms). The cache closes that gap
once warm.

## Against the capacity check

Spec 03 § Capacity sanity check assumes 50k spans/s at about 3 KiB per span,
flushes every ~1.7 s at 256 MiB, and compressed output of 20–30 MiB/s.

- **Ingest rate, 50k spans/s: measured, exceeded.** 83,671 spans/s local (run
  1) and 83,374 spans/s on S3 (run 2), with 8 writers and backpressure retries
  (about 23,000 per run). The 50k target is reached with margin on both stores.
- **Span size, about 3 KiB: measured.** 2,812 Arrow bytes per span (local), 2,812
  (S3). The spec's 3 KiB is the normalized span; the benchmark measures in-memory
  Arrow size (`RecordBatch::get_array_memory_size`), not the normalized JSON, so
  the two are close but not the same measure.
- **Ingest bytes, 150 MiB/s Arrow: measured, exceeded.** 224.4 MiB/s local, 223.6
  MiB/s S3.
- **Flush cadence, 256 MiB every ~1.7 s: partly measured.** The benchmark did not
  count flush files, so the cadence is not measured directly. Total flushed bytes
  over the run were 768 MB local (732 MiB) and 746 MB on S3 (711 MiB) in 60 s.
  Flushes are cut at 256 MiB of Arrow, and the measured Arrow rate (224 MiB/s)
  would fill one about every 1.1 s, so the spec's 1.7 s figure (at its lower
  150 MiB/s) is consistent in order of magnitude. Not measured: the exact flush
  count and interval.
- **Compressed output, 20–30 MiB/s: measured, not reached.** The flushed output
  was 11.9 MiB/s (local, 768 MB over 61.7 s) and 11.9 MiB/s (S3, 746 MB over 60 s).
  The spec's figure assumes zstd on JSON-heavy text at 5–8×. The benchmark's
  LLM payloads are random lowercase words, which compress less than real prompts
  (about 19× Arrow-to-Parquet here, below the spec's implied ratio). The output
  rate is therefore lower than the spec's, while the input rate is higher. The
  two are not directly comparable. Not reached in the spec's terms.
- **Compaction, not in the spec: measured.** 21.1 MiB/s of input (local, one L0
  job of 765 MB in 34.7 s) and 20.6 MiB/s (S3, 743 MB in 34.5 s). Write
  amplification 2.00 (local) and 1.99 (S3): one merge that rewrites its input.
- **WAL writes, about a third of Arrow: not measured.**
- **Sustained use needs S3 and a bigger box: partly measured.** The 60 s runs
  sustain 83k spans/s on both stores, and the S3 run compacts within the same
  time as local. The benchmarks do not measure longer runs or a real S3 network,
  so the claim stays open.

## Findings

**(a) Range-mode memory.** A range-mode `traces` query has no list fast path: it
merges every trace in its window. Its memory is proportional to the traces in
the window, about 2.4 GB per 1,000,000 traces at 8 partitions (about 296 MB per
`TracesExec` partition). With the default 2 GiB `query.max_memory` it fails as
too large above roughly 850,000 traces in the window. The benchmark's range-mode
reference query (`trace_list(&range, ...)`, used for `differing ids`) failed as
`TooLarge` on the first attempt. The harness now sets `query.max_memory` to 8 GiB
and `query.max_memory_total` to 16 GiB (`trace_bench.rs` `config`), with a
comment. The list-mode queries themselves never needed more than the default.
The production server keeps the defaults; this is a finding for the query
layer, not a bug in the benchmark.

**(b) Multipart bug, found by this benchmark.** The first S3 ingest run
(`ingest-s3.log`, 4,774,000 spans) finished with its compaction job failed: 1
pass, 0 jobs, 1 failed, 0 bytes committed. The cause was in `publish_output`'s
multipart upload. Each part was the result of one `tokio` `read()`. A short read
gave a 2 MiB non-final part, and S3 rejects any non-final part under 5 MiB with
`EntityTooSmall`. The fix (commit `e73fe87`, "storage: fill each multipart part
before uploading it") fills every part to 16 MiB before uploading it. After the
fix, the same 60 s S3 ingest (run 2) compacted: 2 passes, 1 job, 0 failed. The
local runs never used this path.

**(c) Scaling of the generated file count.** The generator's 900–1,100 spans-file
target is written for the 5M-trace shape. At 1M traces it does not converge:
regenerating at a smaller `max_bytes` gave 30 → 31 files locally (3.8 MiB) and
28 → 32 files on S3 (3.6 MiB). The run accepts whatever count generation gives:
31 files (local, 719 MB) and 32 files (S3, 837 MB). Both are recorded above.

**(d) Cold-cache note.** `LLMOBS_TRACE_BENCH_CACHE=cold` empties `<dir>/cache`
once, before the storage opens. The first query of the run is a true cold sample
(empty cache). Later queries of the same cold run may hit blocks that earlier
queries fetched: the cold run's totals were 22,409 hits and 283 misses. So the
cold column is the first sample of each query, and it may be partly warm for
Q2 to Q6. Warm and off runs are not affected. A stricter cold measurement would
clear the cache and reopen the storage before each query.

**(e) Local-server caveat.** The S3 server is Versity Gateway on the same
machine. Its requests cost a loopback round trip, not a network round trip, so
the cache-off and cold numbers understate the latency of a real S3 endpoint.
The warm numbers are not affected by this.

## Commands

Build once, then run one benchmark at a time on each data directory:

```bash
RUST_MIN_STACK=33554432 cargo test --release -p llmobs-storage --test ingest_bench --test trace_bench --no-run
```

- Run 1: see above (`ingest_bench`, `LLMOBS_INGEST_BENCH_DIR`, `LLMOBS_INGEST_BENCH_SECS=60`).
- Run 2: as run 1, with the common S3 environment.
- Run 3: `trace_bench` with `LLMOBS_TRACE_BENCH_DIR=<scratchpad>/bench/trace-local LLMOBS_TRACE_BENCH_TRACES=1000000`.
- Runs 4–6: `trace_bench` with `LLMOBS_TRACE_BENCH_DIR=<scratchpad>/bench/trace-s3 LLMOBS_TRACE_BENCH_TRACES=1000000 LLMOBS_TRACE_BENCH_CACHE=cold|warm|off`, with the common S3 environment. Run 4 generates the dataset; runs 5 and 6 reuse it (the manifest's endpoint and bucket match).
