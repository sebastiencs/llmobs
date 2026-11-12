# A1 · M4b — Compaction jobs, the task, and its tests

Second half of milestone M4, **Compaction and GC**. It needs `07-m4a-gc-and-merge.md` done, through M4.4. What it adds:
- the policy that picks jobs;
- jobs that merge, write, commit and publish;
- the background task and its configuration;
- the crash, race, model and write-amplification tests that are M4's exit criteria.

The conventions of `07-m4a-gc-and-merge.md` § Conventions apply unchanged. Read it again if you start here.

**M4 exit criteria** (`00-design.md` § Milestones):
- write amplification measured on a synthetic day stays bounded: about 2, asserted at most 3 (M4.8);
- crash tests pass with compaction running (M4.7);
- race tests pass: compaction against queries, against flush, and against itself (M4.6, M4.7).

---

## M4.5 — The policy: which job, on which files

**Goal:** a pure function that turns the current catalog, the claimed files and `now` into the list of jobs, following spec 03 §5 "Policy, per partition" exactly. It does no I/O, so every rule is unit-tested.

### `src/compaction/plan.rs`

```rust
/// The policy values, from `[storage.compaction]` (M4.6) or set directly by tests.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Policy {
    pub(crate) target_file_size: u64, // bytes
    pub(crate) size_ratio: u64,
    pub(crate) l0_files: usize,
    pub(crate) l0_bytes: u64,         // bytes
    pub(crate) l0_age: i64,           // ns
    pub(crate) close_delay: i64,      // ns
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobKind { L0Merge, DayClose }

/// One compaction job: some files of one `(table, project, day)` partition.
#[derive(Debug, Clone)]
pub(crate) struct Job {
    pub(crate) table: FileTable,          // Spans or Scores, never Traces
    pub(crate) project: ProjectId,
    pub(crate) day: NaiveDate,
    pub(crate) kind: JobKind,
    /// Input files, ascending `FileId` (oldest first).
    pub(crate) inputs: Vec<Arc<FileMeta>>,
    /// The job took every file of the partition.
    pub(crate) whole_partition: bool,
    /// Scores only: drop rows whose latest version is deleted (day close).
    pub(crate) drop_deleted: bool,
}

pub(crate) fn plan(catalog: &Catalog, claimed: &HashSet<FileId>, policy: &Policy, now: i64)
    -> Vec<Job>;
```

### Rules

Visit `FileTable::Spans` and then `FileTable::Scores`. Never visit `Traces`. Within a table, visit each project and each day in `BTreeMap` order, using `Catalog::files_all` or iterating `tables[file_index(table)]`; add a `pub(super)` accessor on `Catalog` if needed. For each partition, with `files` = its active files:

1. **Claimed.** If any file of the partition is in `claimed`, skip the partition.
2. **Measures:**
   - `l0` = files with `level == 0`;
   - `total` = sum of `size` over `files`;
   - `newest_l0` = max of `max_ingest_time` over `l0`, if any.
3. **Day end.** `day_end` = `(day + 1 day)` at 00:00:00 UTC, in ns. Use `NaiveDate::succ_opt`, `and_hms_opt(0, 0, 0)`, `and_utc().timestamp_nanos_opt()`. If any step returns `None`, skip the partition.
4. **Closed:**
   - `spans`: `l0.is_empty() && (files.len() <= 1 || total > target_file_size)`;
   - `scores`: every file has `fully_merged`.
5. **Day close is due** when all of:
   - `now >= day_end.saturating_add(close_delay)`;
   - the partition is not closed;
   - `newest_l0.is_none_or(|t| now.saturating_sub(t) >= close_delay)`.

   The job:
   - **inputs:** every file of the partition when `table == Scores || total <= target_file_size`, with `whole_partition = true`; otherwise `l0` only, with `whole_partition = false`;
   - **`drop_deleted`:** `table == Scores`;
   - **kind:** `DayClose`.

   A day close may have a single input (one L0 file, or one scores file not yet fully merged). Emit it and go to the next partition.
6. **L0 merge.** Otherwise, the L0 merge is due when any of these holds:
   - `l0.len() >= l0_files`;
   - the sum of `l0` sizes `>= l0_bytes`;
   - `l0.len() >= 2 && now.saturating_sub(newest_l0) >= l0_age`.

   When it is due:
   - inputs = all of `l0`, plus L1 files chosen by the size ratio: sort the level-1 files by `(size, id)` ascending, start with `out` = the sum of `l0` sizes, and for each L1 file `f` in that order, if `out >= size_ratio * f.size`, add `f` and `out += f.size`; otherwise stop;
   - if the inputs number fewer than 2, emit nothing;
   - otherwise emit `kind = L0Merge`, `whole_partition = (inputs.len() == files.len())`, and `drop_deleted = false`.
7. **Order.** Sort each job's `inputs` by `FileId`. Use saturating arithmetic for every multiplication and sum.

Return the jobs in visiting order.

### Tests (`plan.rs`, no I/O)

**Helpers:**
- Build `FileMeta`s with a helper `meta(table, day, level, size, max_ingest_time, fully_merged)`. Each one gets a fresh `FileId::new_v7()` and placeholder values for the other fields.
- Insert them through `Catalog::empty().apply(&CatalogEdit { add, .. })`. `apply` doesn't check staging.

**Fixture values:**
- Policy: `target 100`, `ratio 4`, `l0_files 8`, `l0_bytes 1000`, `l0_age 10 min`, `close_delay 2 h`.
- Day D = 2026-03-05. `now` = D 12:00 unless a test says otherwise.

1. **L0 merge triggers.**
   - 7 L0 files of size 1, all fresh: no job.
   - 8 such files: one `L0Merge` with 8 inputs.
2. **L0 size trigger.** 2 fresh L0 files of 500 each: a job.
3. **L0 age trigger:**
   - 2 L0 files, the newest 10 min old: a job;
   - 9 min 59 s old: no job;
   - 1 L0 file, 1 h old: no job.
4. **Size ratio.**
   - L0 sizes sum to 40, with L1 files of 5, 10 and 20.
   - Inputs are L0 + 5 + 10, so `out` = 55 and `55 < 4 × 20`. The 20 stays.
   - With L1 files of 20 and 5, the order is by size, so 5 is checked first: added, `out` = 45; then `45 < 80`, so 20 stays.
5. **Claimed.** A claimed file anywhere in the partition means no job, even when it is due.
6. **Day close timing.**
   - Spans, 3 L0 files totalling 30. At `now` = D+1 01:59:59, no day close; L0 rules apply instead, so give the files fresh ingest times and expect no job.
   - At D+1 02:00:00 with the newest L0 3 h old: `DayClose`, `whole_partition = true`, 3 inputs.
7. **Late data.**
   - Same as test 6, but the newest L0 is 1 h old: no day close.
   - Its age is ≥ `l0_age` with 3 L0 files, so expect an `L0Merge` instead.
8. **Big day.** Spans with 2 L1 files of 80 and 2 old L0 files of 10, after the delay: `DayClose` with the 2 L0 inputs only, and `whole_partition = false`.
9. **Closed spans.**
   - One L1 file and no L0 after the delay: no job.
   - Two L1 files totalling 150 (> 100): no job.
   - Two L1 files totalling 60: `DayClose`, `whole_partition = true`.
10. **Scores day close.**
    - Scores files of 80 and 80, one not `fully_merged`, after the delay: `DayClose` with both inputs, `whole_partition = true`, `drop_deleted = true`.
    - Both `fully_merged`: no job.
11. **Ignored tables and day ends.**
    - `traces` files never produce a job.
    - A partition at `NaiveDate::MAX` is skipped without panicking.

**Commit:** `storage: plan compaction jobs`

---

## M4.6 — Jobs, the pass, the task and the config

**Goal:** run jobs. Each one claims, merges slice by slice, writes, stages, publishes and commits one catalog edit, then publishes the new catalog to readers. Add `[storage.compaction]`, the background task, and the hidden `Storage::compact`. Remove the `#![allow(dead_code)]` of `compaction/mod.rs`.

### Config (`config.rs`)

`pub struct CompactionConfig`, TOML `[storage.compaction]`, with `#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]` and `#[serde(deny_unknown_fields)]`. Every field is `pub`, as in `GcConfig`: `StorageConfig` derives `PartialEq, Eq`, and integration tests build it from outside the crate.
- On `StorageConfig`: `pub compaction: CompactionConfig` with `#[serde(default)]`.
- Each field has its own `default_*` function, like the existing sections, and the struct has a manual `Default`.

| Field | Type | Default | Valid range (`validate()`) |
|---|---|---|---|
| `interval` | `Duration` (humantime) | 30 s | 100 ms … 1 h |
| `threads` | `usize` | `max(1, available_parallelism / 4)` (1 if unknown) | 1 … 64 |
| `target_file_size` | `ByteSize` | 512 MiB | 1 MiB … 16 GiB |
| `size_ratio` | `u64` | 4 | 2 … 100 |
| `l0_files` | `usize` | 8 | 2 … 1000 |
| `l0_bytes` | `ByteSize` | 512 MiB | 1 MiB … 16 GiB |
| `l0_age` | `Duration` (humantime) | 10 min | 1 s … 24 h |
| `close_delay` | `Duration` (humantime) | 2 h | 1 s … 7 days |
| `max_memory` | `ByteSize` | 1 GiB | 16 MiB … 1 TiB |

- Error field names are `"storage.compaction.<field>"`. Rule strings read `"must be between <min> and <max>"`, written in the table's units.
- Tests:
  - add `compaction` to `unknown_keys_fail_at_every_level`;
  - add both bounds of every field to the bounds test;
  - add one parse test with every field set.
- `Policy::from(&CompactionConfig)` converts durations to ns (saturating) and sizes to bytes.

### Publishing a catalog-only change (`read_state.rs`)

```rust
/// Publishes `catalog` with the current memtable snapshots unchanged.
/// Call it inside the committer's publish closure (the committer mutex is held).
pub(crate) fn publish_catalog(published: &Published, catalog: &Arc<Catalog>)
```

The body is `published.replace(|state| ReadState { spans: Arc::clone(&state.spans), scores: Arc::clone(&state.scores), catalog: Arc::clone(catalog) })`.

The lock order is still: committer, then memtable, then `Published`. This function takes no memtable lock.

### Committer: the `traces` files of some `spans` files

```rust
/// For every active traces file with a source among `spans`, all of its source pairs.
pub(crate) async fn derived_traces(&self, spans: &[FileId])
    -> Result<Vec<(FileId, FileId)>, CommitError>; // (traces, spans)
```

The query is below. Run it in chunks of at most 500 ids, building the `IN (?, ?, …)` list for each chunk, and merge the results without duplicates (a `BTreeSet`). A `sqlx` error is `CommitError::Db`. Parse ids with `parse_file_id`; a bad id is `CommitError::conflict("invalid file id")`.

```sql
SELECT fs.traces_file_id, fs.spans_file_id FROM file_sources AS fs
JOIN data_files AS t ON t.id = fs.traces_file_id
WHERE t.state = 'active' AND fs.traces_file_id IN (
  SELECT traces_file_id FROM file_sources WHERE spans_file_id IN (<ids>))
```

### `src/compaction/job.rs`

```rust
/// Shared by every job: built once in `Storage::open`.
pub(crate) struct Compactor {
    committer: Arc<Committer>,
    objects: Arc<Objects>,
    published: Arc<Published>,
    merge: MergeContext,
    /// Read at the start of each pass. Tests replace it (`Storage::set_policy`).
    policy: std::sync::Mutex<Policy>,
    threads: usize,
    claims: std::sync::Mutex<HashSet<FileId>>,
    /// Set by `Storage::shutdown`; jobs check it before each slice and before committing.
    stopping: AtomicBool,
    hooks: Hooks,
}

impl fmt::Debug for Compactor { /* `threads` and the claim count only */ }

/// What a pass did. Byte counts are object sizes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompactionReport {
    /// Jobs that committed.
    pub jobs: u64,
    /// Jobs that failed or lost a conflict (logged); a later pass redoes them.
    pub failed: u64,
    /// Files the committed jobs removed, `traces` included.
    pub input_files: u64,
    /// Files the committed jobs added, `traces` included.
    pub output_files: u64,
    /// Bytes of the `spans` and `scores` inputs of committed jobs.
    pub input_bytes: u64,
    /// Bytes of the `spans` and `scores` outputs of committed jobs.
    pub output_bytes: u64,
    /// The part of `output_bytes` that is `spans`.
    pub spans_output_bytes: u64,
}
```

Re-export `CompactionReport` and `CompactionError` from `lib.rs`.

**Building the `Compactor`** in `open_inner`, after `published` is created (`Published::new`) and before the flush tasks start. `Storage` derives `Debug`, hence the manual `Debug` above.
- **Runtime:** a separate `RuntimeEnv` for compaction, `RuntimeEnvBuilder::new().with_memory_pool(Arc::new(GreedyMemoryPool::new(max_memory))).build_arc()`, then `objects.register(&compaction_runtime)`.
- **Merge context:** `MergeContext { runtime, batch_size: 8192 }`.
- **Policy:** `Policy::from(&config.compaction)`, with `threads` from the config.

The query runtime is untouched.

**`Compactor::pass(&self, now: i64) -> Result<CompactionReport, CompactionError>`:**
1. `catalog = committer.catalog().await`.
2. Under `claims`:
   - `jobs = plan(&catalog, &claims, &policy, now)`;
   - insert every input id of every job into `claims`.
3. Run the jobs with at most `threads` at a time: a `tokio::task::JoinSet` plus a `tokio::sync::Semaphore`.
   - Each job runs `self.run(job)` in its own task. `Compactor` is behind an `Arc`; `pass` takes `self: &Arc<Self>`.
   - Add the report of each `Ok`.
   - Each `Err`, and each task `JoinError`, adds 1 to `failed` and is logged with `warn!`: table, project, day, kind, input count, and the error. A conflict is logged at `info!` instead, since a later pass redoes it.
4. Return `Ok(report)`. The only `Err` from `pass` would come from step 1, and `committer.catalog()` is infallible today, so `pass` returns `Ok` in practice. Keep the `Result` for the hidden API.

**Claims are always released.** A guard struct holding the ids, with a `Drop` that removes them from `claims`, is created as soon as ids are claimed. It lives until the job's task ends, whatever the outcome.

**`Compactor::run(&self, job: Job) -> Result<CompactionReport, CompactionError>`**, in order:
1. **Traces files** (spans only).
   - `pairs = committer.derived_traces(&input_ids).await?`.
   - Every input must appear in exactly one pair, otherwise `CompactionError::Input { reason: "spans file without exactly one traces file" }`.
   - Every pair's spans id must be an input, otherwise `Input { reason: "traces file shared outside the job" }`. That can't happen with D-A1-1; it is a guard.
   - Under `claims`, if any of these traces ids is already claimed, return `Input { reason: "traces file claimed" }`. Otherwise claim them, and add them to the job's guard.
2. **Slices:**
   - `input_bytes` = sum of input sizes;
   - `values = time_values(objects.store(), &inputs, time_column(table)).await?`;
   - `slices = slices(&values, slice_count(input_bytes, policy.target_file_size))`;
   - for each slice, its row count: the number of `values` inside it, from `partition_point` on the sorted values;
   - drop `values`.
3. **For each slice, in order:**
   0. If `stopping` is set, return `CompactionError::Stopped`.
   1. Mint the output paths with `Objects::table_file_path(table, project, day, 1)`, and for spans also `Objects::table_file_path(FileTable::Traces, project, day, 1)`.
   2. `committer.stage(&[paths]).await?`, then hook `CompactionAfterStage` (pause, then fail → `CompactionError::Fault`).
   3. `(target, file) = objects.create_output(&spans_or_scores_path).await?`.
   4. `writer = FileWriter::new(table, 1, file, expected_rows)`, where `expected_rows` = the slice's row count from step 2. `writer_properties` clamps it per row group (M4.2).
   5. `merged = merge_slice(&self.merge, table, &inputs, slice, job.drop_deleted, writer).await`. On `Err`, `discard_output(target)` (ignore its error) and return the error.
   6. If `merged.rows == 0`, `discard_output(target)` and continue with the next slice. The staged rows stay for GC. This can only happen when a scores day close drops everything.
   7. On `spawn_blocking`, `let (file, summary) = merged.writer.finish()?`. Then take `file_len = file.metadata()?.len()`, read the last 8 bytes (`Seek::seek(SeekFrom::End(-8))` and `read_exact`; the file was opened readable in M4.2), and compute `footer_size_from_tail(&tail, file_len)`. An I/O error here is `ObjectError::io("reading compaction output", temp path, error)`, using the existing constructor.
   8. `size = objects.publish_output(target, file).await?`.
   9. **Spans only:**
      - `traces = write_file(FileTable::Traces, 1, &partials as Vec<Arc<RecordBatch>>)?`, on `spawn_blocking`;
      - `objects.put_durable(&traces_path, traces.bytes.clone()).await?`.
   10. Build the output `FileMeta`s with `level 1` and `fully_merged = job.whole_partition && (table == FileTable::Spans || job.kind == JobKind::DayClose)`. The other fields come from the summary or `WrittenFile`: `size`, `footer_size`, `rows`, the bounds, `table`, `project`, `day`, `path`, `id`.
       - A scores **L0 merge** that happens to take every file is not `fully_merged`: it kept deleted versions, and the scores "closed" rule reads `fully_merged`, so the day close must still run.
       - The traces output gets the same `fully_merged` as its spans output.
4. **Hook `CompactionAfterWrite`** (pause, then fail → `Fault`). Then, if `stopping` is set, return `CompactionError::Stopped` without committing.
5. **No outputs.** If every slice was empty, the edit has `add` empty. That is legal: the inputs are removed, which is the right result for a scores day close that dropped every row.
6. **The edit:**
   - `add`: per slice, the spans (or scores) output, then its traces output;
   - `remove`: every input and every traces id from step 1, each with `FileState::Active`;
   - `sources`: `(traces_out_id, spans_out_id)` per slice;
   - `flushed_seq`: `None`.

   Commit it with `committer.commit(&edit, |catalog| publish_catalog(&self.published, catalog)).await?`.
7. **Hook `CompactionAfterCommit`** (pause, then fail → log `warn!` and continue, as flush does).
8. **Report:**
   - `jobs = 1`;
   - `input_files` = removed count, `output_files` = added count;
   - `input_bytes` = sum of the spans or scores input sizes;
   - `output_bytes` = sum of the spans or scores output sizes;
   - `spans_output_bytes` = `output_bytes` when `table == Spans`, else 0.

   Log one `info!` with the table, project, day, kind and the report's counts.

**On any error after step 3.2,** the staged paths stay staged and GC removes them after 1 h. Objects already published but not committed are among them. Never delete them yourself.

### Hook points

`CompactionAfterStage`, `CompactionAfterWrite` and `CompactionAfterCommit` raise `COUNT` to 21.

### The background task (`src/compaction/task.rs`)

Mirror `gc.rs`:
- `CompactionHandle { stop: watch::Sender<bool>, task: std::sync::Mutex<Option<JoinHandle<()>>> }` with `abort()`;
- `spawn(compactor: Arc<Compactor>, interval: Duration) -> CompactionHandle`, using `tokio::time::interval_at(Instant::now() + interval, interval)` with `MissedTickBehavior::Delay`, and a `select!` on stop and tick;
- each tick runs `compactor.pass(now_ns()).await`. Log the report at `debug!` when `jobs + failed > 0`, and an `Err` at `warn!`.

**`Storage`:**
- **Fields:** `compactor: Arc<Compactor>` and `compaction: CompactionHandle`.
- **Open:** start the task at the end of `open_inner`, after the GC task.
- **`shutdown`:** stop compaction first, before `gc.abort()`, without aborting a job in the middle of a commit:
  1. set `compactor.stopping`;
  2. send `true` on the task's `stop` channel;
  3. await the task's `JoinHandle` for at most 30 s (`tokio::time::timeout`);
  4. on timeout, abort it.

  A running job sees `stopping` before its next slice or its commit and returns `Stopped`, leaving staged outputs and temp files that GC and the startup cleanup remove. The task loop exits when its pass returns and `stop` is set.

  `CompactionHandle` gets `stop_and_wait(&self, limit: Duration)` for this. `abort()` stays for `Drop`.
- **`Drop`:** sets `stopping` and aborts.
- **`compact` after `shutdown`:** `stopping` makes every job return `Stopped`. Jobs aren't expected to run then.
- **Hidden API:**

  ```rust
  /// One compaction pass at `now` (ns), run to completion. The background task passes
  /// the wall clock every `compaction.interval`.
  #[doc(hidden)]
  pub async fn compact(&self, now: i64) -> Result<CompactionReport, CompactionError>
  ```

  It calls `self.compactor.pass(now)`. It may run alongside the background task; claims keep them apart.
- **Crate doc in `lib.rs`:** remove the line "compaction does not run, so level-0 files accumulate;".

### Tests (`compaction/job.rs`, through `Storage`)

**Setup:**
- Use the existing in-crate `config_in(dir)` pattern, with `compaction.interval = 1h` so only `compact(now)` runs jobs, and `flush.max_age = 1h`.
- Each `Storage::shutdown` plus `Storage::open` flushes one L0 file per partition; that is how the tests make L0 files.
- For a smaller `target_file_size` than the config allows, build the policy directly: add a `#[cfg(test)] fn set_policy(&self, policy: Policy)` on `Storage` that replaces the compactor's policy. `policy` is then a `std::sync::Mutex<Policy>`, read at the start of each pass.
- Day D = 2026-03-05. Build spans with `test_support::span_row` / `span_rows` (M4.4) at D 10:00 + k s, with `ingest_time` = start + 1 s.
- Tests that hold a hook while another task runs use `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`, because `Hooks::pause` blocks its thread.

1. **L0 merge.**
   - Setup: 3 L0 spans files of D, with traces A and B spread over all three, and `l0_files = 3`.
   - Before: `SELECT * FROM spans` and `SELECT * FROM traces` rows, through `Storage::query` and `test_support::rows`.
   - `compact(now)` with `now` = D 11:00 reports `jobs = 1, input_files = 6, output_files = 2`.
   - The catalog has 1 spans and 1 traces file, both level 1, and the 6 inputs are `obsolete`.
   - The spans rows are equal.
   - The traces rows are equal except `partial_count`, which is now 1 for every trace.
   - **How to read `partial_count`:** the `traces` view hides it. Register `ProjectQuery.traces` (the raw provider) in a fresh `SessionContext` under the name `traces`, as `crash_tests::stored_ids` does for spans, and query it with SQL there. Use the same method in every test that reads `partial_count`. `test_support::rows` takes a `QueryContext`, so use it only for queries through the views.
   - A reopen passes `check_file_sources`.
2. **Duplicate collapse.**
   - The same `(trace, span)` is appended in two flushes, with `ingest_time` t and t + 5.
   - After the merge, `spans` has one row for it, with t + 5.
   - The trace's `span_count` is one less than before the merge (before, the read-time merge counted it twice).
3. **Size ratio.**
   - `set_policy` with `target_file_size = 10 MiB`.
   - Make one big L1 file: 4 L0 files of 200 spans each, `l0_files = 4`, compact.
   - Then add 2 small L0 files of 5 spans each with `l0_files = 2`, and compact at a `now` before the day ends.
   - Before that second `compact`, assert the precondition from `catalog_files`: the 2 small L0 sizes sum to less than 4 × the big L1 size. Parquet overhead makes small files bigger than their rows suggest, so give each of the first 200-span batches a 1 KiB `input` per span, to make the big file clearly bigger.
   - The big L1 is not an input, and the partition now has 2 L1 spans files.
4. **Day close, whole.**
   - 3 L0 files of D; `now` = D+1 02:00 + 1 s, with every L0 older than 2 h.
   - Result: 1 spans file and 1 traces file, both `fully_merged`, and `partial_count = 1` for every trace.
   - A second `compact` at the same `now` does nothing (`jobs = 0`).
5. **Day close, slices.**
   - `set_policy` with `target_file_size` = 1/3 of the L0 total; read the total from `catalog_files`.
   - Result: at least 2 spans files, each with its own traces file (`file_sources` 1:1).
   - The spans files' `[min_time, max_time]` ranges don't overlap.
   - Spans rows are unchanged.
   - Traces queried through `TracesProvider` are equal to the rows before, except `partial_count`.
6. **Scores.**
   - Versions of score `s` across 3 L0 files, the newest deleted; score `t` live.
   - An `L0Merge` (`now` inside D, `l0_files = 3`) keeps 1 row of `s` (deleted) and 1 of `t` in the file. The `scores` view shows only `t`.
   - A day close then leaves one `fully_merged` scores file holding only `t`.
7. **Conflict.**
   - Hold `CompactionAfterWrite`, then start `compact` in a task.
   - Commit, through the committer in the test, an edit that removes one input spans file and its traces file. Publish it with `storage.committer.commit(&edit, |catalog| publish_catalog(&storage.published, catalog))`, so readers stop seeing the removed file.
   - Release. `compact` reports `failed = 1, jobs = 0`, and claims are empty afterwards.
   - `collect_staged(Duration::ZERO)` removes the job's staged paths and objects.
   - Every span except those of the removed file is still queryable once.
8. **Concurrent passes.**
   - Hold `CompactionAfterStage`.
   - Start two `compact(now)` calls on a partition with one due job.
   - Release. The sum of `jobs` is 1, and neither reports `failed`.
9. **Query during compaction.**
   - Take `q = storage.query(project, default)` before compacting.
   - After `compact` commits, `collect_obsolete(Duration::ZERO)` reports `in_use ≥ 1` and deletes none of the inputs `q` references.
   - A query on `q` still returns the old rows.
   - Drop `q`. The next `collect_obsolete(Duration::ZERO)` deletes them.
10. **Flush during compaction.** This test uses `flush.max_age = 100ms`, so the new rows flush by age.
    - Hold `CompactionAfterWrite`.
    - Append spans of D and flush them (`wait_rows` pattern from `flush.rs`). The flush commits.
    - Release. The job commits too.
    - The new L0 file is still active, and every span is queryable once.
11. **The task runs.**
    - `compaction.interval = 200ms`, `l0_files = 2`.
    - Make 2 L0 files; poll `catalog_files` until a level-1 spans file appears (timeout 15 s).
    - `shutdown` returns `Ok`.

**Commit:** `storage: run compaction jobs`

---

## M4.7 — Crash and model tests

**Goal:** M4's crash exit criterion: compaction and obsolete-file GC can be killed at any step without losing an acknowledged row, leaving a bad catalog, or leaking objects. Also a model test of append, restart, compaction and GC against a simple model of the rows.

### Deterministic aborts (`src/crash_tests.rs`)

**The child,** a second ignored test `compaction_child`:
- It reads `LLMOBS_COMPACTION_CRASH_DIR` and `LLMOBS_COMPACTION_CRASH_POINT`.
- It uses `quiet_config` plus `compaction.interval = 1h` and `compaction.l0_files = 2`, with every other compaction value at its default.
- It runs, in order:
  1. For `i` in 0..3: open, append `span_rows(&[span_row(0, 2 * i, t, t + 1 s), span_row(0, 2 * i + 1, t + 1 s, t + 2 s)])` with `t` = D 10:00 + `i` min, then `shutdown`. That leaves 3 L0 spans files and 3 traces files of one trace.
  2. Open. `hooks().abort_at(point)`.
  3. `compact(D 12:00)`, then `collect_obsolete(Duration::ZERO)`. At D 12:00 the three L0 files are about 2 h old and the day hasn't ended, so the pass runs one `L0Merge`.
  4. If it gets here, it exits normally.

**The parent,** `every_compaction_hook_aborts_safely`:
- **It is `#[ignore]`d**, like `every_flush_hook_aborts_safely`. The flush parent runs alone because forked children must not inherit other tests' `LOCK` file descriptors, and the same applies here.
  - Add it to the `test` recipe of `justfile` as a third line: `cargo test -p llmobs-storage --lib --all-features -- --ignored --exact crash_tests::every_compaction_hook_aborts_safely`.
  - This is the one change outside `crates/storage/` and `docs/` that M4 makes.
- **The parent's own opens** use `quiet_config` plus `compaction.interval = 1h` and `compaction.l0_files = 2`, the child's config, so the background task never races its checks.
- It runs the child for each point of:

  ```rust
  const COMPACTION_POINTS: &[&str] = &[
      "CompactionAfterStage", "ObjectSync", "ObjectRename", "ObjectDirSync",
      "CompactionAfterWrite", "CatalogCommit", "CompactionAfterCommit",
      "GcObsoleteBeforeDelete", "GcObsoleteAfterDelete",
  ];
  ```

  Map the names to `HookPoint` as `POINTS` does today.
- `ObjectSync`, `ObjectRename` and `ObjectDirSync` are hit by `publish_output` and `put_durable` during compaction. Steps 1–2 cannot hit them after `abort_at` is armed: the reopen in step 2 has nothing to flush.
- **The child must abort.** A normal exit fails the test with "point not reached".
- **Then the parent checks:**
  1. `Storage::open` succeeds, so `check_file_sources` passed.
  2. The stored span ids equal the 6 written in step 1. The child arms `abort_at` only after step 1, so all 6 are always flushed.
  3. `objects_match_catalog`: every active file's object exists with its size.
  4. Run `compact(D 12:00)` until it reports `jobs = 0` (at most 5 calls), then `collect_obsolete(Duration::ZERO)` and `collect_staged(Duration::ZERO)`.
  5. The span ids still equal the 6.
  6. The catalog has exactly one active spans file and one active traces file for D, both level 1.
  7. No `obsolete` row and no `staged_objects` row remain in `catalog.db`.
  8. No temp file is left under `objects/`.

### The crash harness (`tests/crash.rs`)

**Child config** (`child_config`):
- `compaction.interval = 200ms`, `compaction.l0_files = 2`, `compaction.l0_age = 1s`, `compaction.close_delay = 1s`;
- `compaction.target_file_size = 1MiB`, so day closes cut slices;
- `gc.grace = 1s`.

The stream's times are in March 2026, long past, so day closes are due at once and the background task compacts throughout the run.

**After each reopen,** `check_dir` keeps every existing check, with the object rule of M4.1 and one more change:
- **`check_source_pair` reads active pairs only.** Today it runs on every `file_sources` row, obsolete ones included. Restrict its query to pairs whose traces file and spans file are both `state = 'active'`.
- With `gc.grace = 1s`, the open-time GC can be killed between an object delete and its row delete. That leaves an obsolete pair with no object, which is fine.

**Background compaction off for the final steps.** The parent's opens in `collect_crash_orphans` and in the final step below use `child_config` with `compaction.interval = 1h`. Otherwise:
- the background task can hold claims while an explicit `compact` reports `jobs = 0`;
- a job stopped by `shutdown` leaves staged outputs, which would fail the "no staged row" and "no temp file" checks.

**At the end of `crash_recovery`,** after `collect_crash_orphans`:
1. Open. Call `compact(now_ns())` until `jobs = 0` (at most 20 calls), then `collect_obsolete(Duration::ZERO)` and `collect_staged(Duration::ZERO)`. Shut down.
2. Run `check_dir` again, then check:
   - no active spans file has level 0;
   - no `obsolete` row remains;
   - for every active spans file, `check_source_pair` holds (already part of `check_dir`).

### Model test (`compaction/model.rs`, a `#[cfg(test)]` module)

A `proptest` with `ProptestConfig { cases: 16, .. }`. Each case opens a fresh storage on a `tempfile` directory. Use:
- `compaction.interval = 1h`;
- `l0_files = 2`;
- the default `close_delay`;
- the default `target_file_size` (512 MiB). A day holds at most a few dozen single-row L0 files, a few hundred KiB with Parquet overhead, so a day close always takes the whole partition.

The proptest body runs async code: build a `tokio::runtime::Runtime` (multi-thread, 2 workers) once per case and `block_on` the case.

**Operations** (`Vec<Op>`, 1 to 40 of them):

```rust
enum Op {
    /// One span. `trace` ∈ 0..6, `span` ∈ 0..12. Its day is `trace % 3` (D, D+1, D+2).
    Append { trace: u8, span: u8 },
    /// shutdown + open: flushes everything.
    Restart,
    /// One pass at the model clock, after advancing it by `hours` ∈ 0..72.
    Compact { hours: u8 },
    /// collect_obsolete(Duration::ZERO).
    Gc,
}
```

**The model:**
- a clock starting at D 01:00;
- `BTreeMap<(u8 trace, u8 span), i64 ingest_time>`.

**Append** writes one span with `test_support::span_row(trace, span, start, ingest)`:
- `start_time` = start of day `trace % 3` + 1 h + `span` s;
- `ingest_time` = the model clock + its op index in ns, so later appends are newer;
- the model keeps the latest per key.

Every trace lives in one day, and its rows start 1 h after midnight, which is not near midnight for the 1 h anchor window, so the anchor never moves a row. Before compaction, storage may hold several copies of a key, so the per-op check compares distinct keys and maxima, not row counts.

**Check after every op:**
- the set of distinct `(trace_id, span_id)` in `SELECT trace_id, span_id, ingest_time FROM spans` equals the model's keys;
- for each key, the greatest `ingest_time` among its rows equals the model's.

**At the end:**
1. `Restart`, so the memtable is empty and every row is in a file.
2. Set the clock to `max(clock, D+6 00:00)` and `compact(clock)` until `jobs = 0` (at most 10 calls).
3. `Gc`.
4. Check that:
   - no key has two rows in `spans`, and each row's `ingest_time` equals the model's;
   - every trace in the raw `traces` provider (read as in M4.6 test 1) has `span_count` equal to its number of model keys and `partial_count = 1`.

The tests use only the existing `test_support` and `Storage` test helpers.

**Commit:** `storage: test compaction under crashes and in a model`

---

## M4.8 — Write amplification

**Goal:** the measurement in M4's exit criterion. It's a test that runs in `just test` and asserts a bound. Record its output in a research note.

### Test `write_amplification_of_a_synthetic_day` (`compaction/job.rs` tests)

**What it measures:** a day of steady ingest under the default policy shape. Every byte should be rewritten once by an L0 merge and once by the day close, and never again: the size ratio keeps equal-sized L1 files from being rewritten by later L0 merges. A size-ratio bug or a day close that runs twice shows up as a higher number.

**Config:**
- `compaction.interval = 1h` (passes run by hand);
- `flush.max_age = 1h`;
- `set_policy` with `target_file_size = 64 MiB`, `size_ratio = 4`, `l0_files = 8`, `l0_bytes = 64 MiB`, `l0_age = 10 min`, `close_delay = 2 h`.

The whole day is a few MiB, far below `target_file_size`, so the day close takes the whole partition.

**Data:** day D = 2026-03-05, 48 half-hour steps `h = 0..48`. In step `h`:
1. Append 40 traces × 10 spans with `start_time` spread over that half-hour and `ingest_time` = start + 1 s.
   - Build them with `test_support::span_row(trace, span, start, ingest)`, with `trace = h * 40 + t` (`t` ∈ 0..40, so every trace id is new) and `span` ∈ 0..10.
   - Set `input` on each span to 200 hex characters from a xorshift64 generator seeded with 1 at the start of the test. A repeated pattern would compress to nothing and leave the files mostly Parquet overhead.
2. Restart, which flushes one L0 spans file. Read its size from `catalog_files` (the new level-0 spans file) and add it to `flushed`.
3. `compact(now)` with `now` = end of the half-hour + 1 s. Add the report's `spans_output_bytes` to `rewritten`.

At that `now` the newest L0 file is seconds old, so only the count trigger fires: one L0 merge every 8 steps, 6 in all, each making one L1 file of about the same size. No L1 file is ever an input of a later L0 merge, because 8 L0 files are never 4 times one L1 file of 8 L0 files.

Record `l1_before_close` = the number of level-1 spans files after step 47. Then `compact(D+1 02:00 + 1 s)` until `jobs = 0`, adding to `rewritten` the same way. That is one day close of all 6 L1 files.

**Assert and print:**
- `wa = rewritten / flushed` (as `f64`);
- `println!` the line `write amplification: flushed {flushed} B, rewritten {rewritten} B, wa {wa:.2}, L1 files before close {l1_before_close}, spans files after {n}`;
- assert `l1_before_close == 6`;
- assert `wa <= 3.0`;
- assert that exactly one spans file remains (level 1, `fully_merged`), and every spans query row is present once.

Expected: about 2, one L0 merge and one day close per byte. The ratio of output to input bytes is not exactly 1, because merged files compress a little better and Parquet overhead is shared, so 3 gives headroom.

**If the bound fails, don't tune the policy.** Report the number and the per-pass reports; the coordinating session decides.

### Research note

Create `docs/research/storage/compaction.md`:
- the method above, with the policy values;
- the measured line: copy the `println!` output from `cargo test -p llmobs-storage write_amplification -- --nocapture`;
- the machine (CPU, disk);
- the commit hash.

Keep it short; this is a record, not a study.

**Commit:** `storage: measure compaction write amplification`

---

## After M4

- **M5** adds tombstones to compaction outputs (`deletions_applied_seq`, spec 03 §5 "During a merge"). Its rewriter replaces a `spans` file together with its one `traces` file, the same pair rule the committer now enforces (M4.3).
- **M6** adds the disk reservation before a merge (D-A1-2) and quarantine.

Report at the end of M4:
- the write-amplification line;
- any rule of this plan that you found ambiguous, and how you read it.
