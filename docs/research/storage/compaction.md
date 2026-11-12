# Compaction write amplification

M4's exit criterion (`docs/plans/a1-storage/00-design.md` § Milestones): write
amplification of a synthetic day stays bounded — about 2, asserted at most 3.
Measured by `compaction::job::tests::write_amplification_of_a_synthetic_day`.

## Method

One day (2026-03-05) of steady ingest under the default policy shape:

- 48 half-hour steps; each appends 40 traces × 10 spans (400 spans), with
  `start_time` spread 4.5 s apart over the half hour and `ingest_time` =
  `start_time` + 1 s. Every span carries 200 hex characters of pseudo-random
  `input` (xorshift64, seeded with 1), so files are not mostly Parquet overhead.
- Each step: restart (flushing one L0 spans file, its size added to `flushed`),
  then one pass at the end of the half hour + 1 s (`spans_output_bytes` added to
  `rewritten`).
- Policy: `target_file_size` 64 MiB, `size_ratio` 4, `l0_files` 8, `l0_bytes`
  64 MiB, `l0_age` 10 min, `close_delay` 2 h. The day totals a few MiB, far
  below the target, so the day close takes the whole partition.
- The newest L0 file of a step is seconds old at its pass, so only the count
  trigger fires: one L0 merge every 8 steps, 6 in all, each making one L1 file.
  The size ratio keeps equal-sized L1 files out of later L0 merges.
- One day close of the 6 L1 files at D+1 02:00 + 1 s.

## Result

From `cargo test -p llmobs-storage write_amplification -- --nocapture`:

```
write amplification: flushed 3131358 B, rewritten 4857589 B, wa 1.55, L1 files before close 6, spans files after 1
```

wa 1.55 ≤ 3, below the expected ~2: merged files compress a little better than
the flush outputs, so the rewritten bytes are smaller than the flushed bytes.

## Machine

- CPU: AMD Ryzen 9 3950X (32 threads)
- Disk: NVMe (ext4)
- Toolchain: rust 1.99.1, debug build

## Commit

Measured at `ee2fcb3` (`storage: test compaction under crashes and in a model`),
the commit before the one carrying this note and the test.
