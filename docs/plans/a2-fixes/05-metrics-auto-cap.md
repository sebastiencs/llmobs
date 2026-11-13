# P5 — Cap automatic metric buckets (M-2)

Read [README.md](README.md) first. Independent of the other phases.

**Goal:** `bucket=auto` over a window longer than 2,000 days is a 400, like an explicit bucket that would give more than 2,000 points. Today it falls back to 1-day buckets with no limit. A window of `[0, i64::MAX)` then makes about 106,750 buckets, and `bucket_grid` and `fill` allocate a point for each one, for every series and group, outside the memory pool and the timeout.

**Edit only:**
- `crates/storage/src/reads/metrics.rs`;
- `docs/spec/05-query.md`;
- `docs/mvp-decisions.md`.

**Commit:** `Cap automatic metric buckets at 2,000 days`

## Background

In `reads/metrics.rs`:
- `const AUTO_LIMIT: i64 = 120;`, `const FIXED_LIMIT: i64 = 2_000;`, `const TOO_MANY_BUCKETS: &str = "too many buckets; pick a wider bucket";`;
- `fn too_many() -> RunError` returns `RunError::InvalidRequest(TOO_MANY_BUCKETS.to_owned())`;
- `fn resolve(window: TimeWindow, bucket: Bucket) -> Result<i64, RunError>`: `Fixed` checks `1..=FIXED_LIMIT`; `Auto` tries `AUTO_WIDTHS` and returns `BucketWidth::D1.ns()` unchecked;
- `fn bucket_count(window, width) -> Result<i64, RunError>` counts buckets aligned to the epoch, with checked arithmetic (`too_many()` on overflow);
- `compute` calls `resolve` first, before any query.

The user chose the same limit (2,000) and the same message as for explicit buckets.

## Changes

In `resolve`, replace the `Auto` arm's final `Ok(BucketWidth::D1.ns())` with:

```rust
            let width = BucketWidth::D1.ns();
            if bucket_count(window, width)? > FIXED_LIMIT {
                return Err(too_many());
            }
            Ok(width)
```

Update the `/// # Errors` doc of `pub async fn metrics` to say:

```
/// [`RunError::InvalidRequest`] when the buckets would emit more than 2,000
/// points (an explicit bucket, or `Auto` over more than 2,000 aligned daily
/// buckets).
```

Keep the rest of that doc as it is.

## Docs

### `docs/spec/05-query.md`, § Dashboard metrics

In the paragraph that starts `` `bucket=auto` picks the smallest width…``, replace `giving at most 120 buckets over `[from, to)`, else `1d`.` with:

```
giving at most 120 buckets over `[from, to)`, else `1d`; past 2,000 daily buckets (aligned to midnight UTC, so a 2,000-day window that does not start at midnight already has 2,001) it is a 400, as for an explicit bucket (D-A2-3).
```

### `docs/mvp-decisions.md`, section `## Track A2 — Query layer`

Add, in ID order (create the section as P1 describes if it is missing):

```
| D-A2-3 | Automatic buckets | **`bucket=auto` over more than 2,000 daily buckets is a 400 with the explicit-bucket message (`too many buckets; pick a wider bucket`).** Without the cap a window of `[0, i64::MAX)` makes about 106,750 daily buckets, and the dashboard allocates a point per bucket for every series and group, outside the memory pool and the timeout (spec 05 § Dashboard metrics). |
```

## Tests

In `reads/metrics.rs` `mod tests`. `D` below is `BucketWidth::D1.ns()`.

**T1 `auto_buckets_stop_at_two_thousand_days`.** Call `resolve` directly:
- `resolve(TimeWindow::new(0, 2_000 * D).expect("window"), Bucket::Auto)` returns `Ok(D)`: exactly 2,000 aligned buckets.
- `resolve(TimeWindow::new(0, 2_000 * D + 1).expect("window"), Bucket::Auto)` returns `Err(RunError::InvalidRequest(m))` with `m == "too many buckets; pick a wider bucket"`.
- `resolve(TimeWindow::new(0, i64::MAX).expect("window"), Bucket::Auto)` returns the same error, without a panic.
- `resolve(TimeWindow::new(i64::MIN, i64::MAX).expect("window"), Bucket::Auto)` returns `Err(RunError::InvalidRequest(_))`, without a panic. The message may be the bucket message or whatever `bucket_count`'s overflow path returns; assert the variant only.

If `TimeWindow::new` rejects one of these windows, use the widest window it accepts and say so in the report.

**T2 `metrics_reject_an_auto_window_past_two_thousand_days`.** Mirrors `two_thousand_one_buckets_are_rejected`, with `window: TimeWindow::new(0, 2_001 * D)` and `bucket: Bucket::Auto`. `metrics(&context(), request)` returns `Err(RunError::InvalidRequest(message))` with the same message.

**T3 `exactly_two_thousand_fixed_buckets_are_accepted`.**
- `resolve(TimeWindow::new(0, 2_000 * BucketWidth::M1.ns()).expect("window"), Bucket::Fixed(BucketWidth::M1))` returns `Ok(BucketWidth::M1.ns())`.
- The existing test `two_thousand_one_buckets_are_rejected` covers 2,001.

**Existing tests** must pass unchanged.

## Done when

- `cargo test -p llmobs-storage --lib reads::metrics` passes.
- `just check` passes.
