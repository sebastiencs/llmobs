# A1 · M4a — Obsolete-file GC and the merge engine

First half of milestone M4, **Compaction and GC** (`00-design.md` § Milestones). It builds the pieces compaction needs and tests each on its own:
- GC of obsolete files;
- a streaming Parquet writer with temp-file publication;
- one dedup module for flush and compaction, plus a stricter commit check;
- the time-slice merge.

The second half, `08-m4b-compaction.md`, plans jobs, runs them, and adds the crash, race, model and write-amplification tests.

Do the sub-phases in order, one commit each. Don't start M4b before M4.4 is committed.

## Read first

- `AGENTS.md`, in full. The rules on compatibility layers, abstractions, placeholders, errors, `unwrap`, panics on disk input, privacy in logs, and tests all apply.
- `docs/spec/03-storage.md` §4 (catalog, committer) and §5 (compaction, GC). §5 is the policy this plan implements. It was rewritten together with this plan.
- `docs/plans/a1-storage/00-design.md` § Durability, § Commit protocol, § Compaction, § Testing.
- `docs/mvp-decisions.md` D83, D84, D87, D88, D152, D-A1-1, D-A1-2.
- The code, as merged on `main` by PR #24 (commit `b6e6250`): `crates/storage/src/{catalog/, gc.rs, flush.rs, parquet.rs, objects.rs, partials.rs, read_state.rs, hooks.rs, config.rs, error.rs, lib.rs}` and `tests/crash.rs`.

## Prerequisites

- `main` contains M3 (PR #24). Branch from it.
- Nothing else. M4 needs no benchmark result.

## Conventions for every sub-phase

- **`just check` passes before each commit.** Commit subjects are given per sub-phase.
- **Dependencies.** M4 adds none. Everything used is in the pinned `datafusion` 55.2, `parquet` 59 (`Cargo.lock`: 59.3.2), `arrow` 59 and `object_store` 0.13. Check them:
  - `ArrowWriter::append_key_value_metadata` and `ArrowWriter::inner`;
  - `datafusion::execution::memory_pool::GreedyMemoryPool`;
  - `SortPreservingMergeExec::new(LexOrdering, input)` in `datafusion::physical_plan::sorts::sort_preserving_merge`;
  - `FileScanConfigBuilder::with_output_ordering(Vec<LexOrdering>)`;
  - `datafusion::physical_plan::execute_stream`.
- **`StorageConfig` literals.** Adding a field to `StorageConfig` or a nested config struct breaks every struct literal of it, for example `config_in` in `lib.rs`, `gc.rs` and the query tests, and `anchor_config` in `flush.rs`. Run `grep -rn "StorageConfig {" crates/` and update every literal in the same commit, using `Default::default()` for the new section unless the test needs other values.
- **Hook points.** A new `HookPoint` variant also needs:
  - its arm in `HookPoint::index`;
  - `COUNT` raised by one per new variant (`hooks.rs`).

  This plan names every new hook point; add no others.
- **Logs.** Log file ids, paths, partition `(table, project, day)`, row counts and byte counts. Never log row content.
- **Errors.** No `unwrap`/`expect` outside tests. Reading a file from the object store is disk input, so a malformed file is an error, never a panic.
- **Time.** All times are `i64` nanoseconds since the epoch, UTC. "Now" in library code is `catalog::committer::now_ns()`, except where a function takes `now` as a parameter (GC and compaction do, so tests control time).
- **What not to build in M4.**
  - Tombstones and `deletions_applied_seq`: those belong to M5. M4 adds no column to `data_files`.
  - Disk reservation: M6 (D-A1-2).
  - Quarantine of corrupt files: M6.
  - Many-source `traces` files: D-A1-1 keeps one per `spans` file.
- **Report at the end of each sub-phase** (in your final message, not in a file):
  - what was done;
  - any deviation from this plan, and why;
  - anything left undone or unverified;
  - files outside `crates/storage/` and `docs/` that you changed. The only one allowed is `justfile`, in M4.7.

---

## M4.1 — GC of obsolete files

**Goal:** objects that a commit marked `obsolete` are deleted once no reader can still use them, with their `data_files` rows (spec 03 §5 "GC of obsolete files", D87). The design calls this "reference-counted GC". Nothing produces obsolete files before M4.6, but the tests here make them with the committer directly.

### Config

Add `GcConfig`, TOML section `[storage.gc]`, to `config.rs`:
- **The struct:** `pub struct GcConfig`, with `#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]` and `#[serde(deny_unknown_fields)]`, like the other sections. `StorageConfig` derives `PartialEq, Eq`, and the integration tests build `StorageConfig` literals from outside the crate, so the struct and its field are `pub`.
- **Field `pub grace: Duration`:** `#[serde(default = "default_grace", with = "humantime_serde")]`, default 5 minutes.
- **`impl Default for GcConfig`:** returns the default.
- **On `StorageConfig`:** add `pub gc: GcConfig` with `#[serde(default)]`.
- **`validate()`:** `gc.grace` must be at least 1 s and at most 24 h. Otherwise return `ConfigError::invalid("storage.gc.grace", "must be between 1s and 24h")`.
- **Tests (`config.rs`):**
  - add `gc` to `unknown_keys_fail_at_every_level`;
  - add the two bounds to the bounds test;
  - add a test that `grace = "90s"` parses to 90 s.

### Retired files and reference counts

- **`Catalog::apply`** (`catalog/mod.rs`) returns the files it removed:
  - The signature becomes `pub(super) fn apply(&self, edit: &CatalogEdit) -> Result<(Self, Vec<Arc<FileMeta>>), CommitError>`.
  - `remove_active` returns the `Arc<FileMeta>` it took out of the day's `Vec`.
  - Removes whose expected state is not `Active` are still skipped in memory, as today, and return nothing.
  - Update every caller and test.
- **`CommitterInner` gets `retired: HashMap<FileId, Arc<FileMeta>>`**, empty at open.
  - In `Committer::commit`, after `COMMIT` succeeds, insert every removed `Arc` under its id.
  - A file is **in use** while `Arc::strong_count(&retired[id]) > 1`. That count covers every older `Catalog` still held by a `ReadState`, a `ProjectQuery` provider, or a plan that cloned the `Arc<FileMeta>`. DataFusion's `PartitionedFile` holds only the path, which is why the grace also applies.

### Committer methods

Add to `Committer`, under its mutex like the staged-object methods:

```rust
/// Obsolete files whose `obsoleted_at` is at or before `cutoff`, in `(obsoleted_at, id)`
/// order, starting after `after` (keyset paging).
pub(crate) async fn obsolete_before(&self, cutoff: i64, after: Option<(i64, FileId)>, limit: u32)
    -> Result<Vec<ObsoleteFile>, GcError>;

/// Deletes one obsolete file's object, then its row, unless a reader still holds it.
pub(crate) async fn collect_obsolete_file<F, Fut>(
    &self,
    file: &ObsoleteFile,
    hooks: &Hooks,
    delete: F,
) -> Result<ObsoleteRemoval, GcError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<u64, ObjectError>>;

pub(crate) struct ObsoleteFile { pub(crate) id: FileId, pub(crate) path: String, pub(crate) obsoleted_at: i64 }
pub(crate) enum ObsoleteRemoval { Deleted { bytes: u64 }, InUse, Gone }
```

- **`obsolete_before`** runs the query below.
  - A `sqlx` error maps to `GcError::Catalog(CatalogError::db(error))`. `GcError` already has `Catalog(#[from] CatalogError)`.
  - `id` parses with `parse_file_id` from `catalog/db.rs`. Make it `pub(super)`. It returns `CatalogError::Corrupt` for a bad id, which maps the same way.

  With `after = None`, drop the row-value condition:

  ```sql
  SELECT id, path, obsoleted_at FROM data_files
  WHERE state = 'obsolete' AND obsoleted_at <= ?
    AND (obsoleted_at, id) > (?, ?)
  ORDER BY obsoleted_at, id LIMIT ?
  ```

  Bind `id` as its hyphenated text, as everywhere in `catalog.db`. SQLite has compared row values since 3.15, and the bundled SQLite is newer.
- **`collect_obsolete_file`**, in this order, holding the mutex throughout:
  1. If `retired` holds the id and `Arc::strong_count > 1`, return `InUse`.
  2. Run `SELECT state FROM data_files WHERE id = ?`; a `sqlx` error maps as in `obsolete_before`. If there is no row, or the state is not `obsolete`, remove the id from `retired` and return `Gone`.
  3. `hooks.pause(HookPoint::GcObsoleteBeforeDelete)`. Then, if `hooks.fail(HookPoint::GcObsoleteBeforeDelete).is_some()`, return `GcError::Fault`.
  4. `let bytes = delete().await?`. This is `Objects::delete_len`, so a missing object counts as 0 bytes, not an error.
  5. `hooks.pause(HookPoint::GcObsoleteAfterDelete)`. On a `fail`, return `GcError::Fault`: the object is gone and the row stays, and the next collection deletes the row (`delete_len` of a missing object is `Ok(0)`).
  6. `DELETE FROM data_files WHERE id = ?`. The foreign keys are on (`open_catalog_db`), so `file_sources` rows that name the file go with it (`ON DELETE CASCADE`).
  7. Remove the id from `retired`. Return `Deleted { bytes }`.

  Holding the committer mutex during the object delete blocks commits for that long. `collect_staged_path` already does the same. Keep it.

### GC

In `gc.rs`:

```rust
/// What one obsolete-file collection did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ObsoleteReport {
    /// Objects deleted, with their rows.
    pub deleted: u64,
    /// Bytes of the deleted objects.
    pub bytes: u64,
    /// Files skipped because a reader still references them.
    pub in_use: u64,
}

pub(crate) async fn collect_obsolete(
    committer: &Committer, objects: &Objects, hooks: &Hooks, now: i64, grace: Duration,
) -> Result<ObsoleteReport, GcError>
```

- **`collect_obsolete`:**
  - `cutoff = now.saturating_sub(grace in ns, saturated to i64)`.
  - Page with `obsolete_before(cutoff, after, 1000)`: start with `after = None`, then set `after` to the last row's `(obsoleted_at, id)`, and stop when a call returns fewer than 1000 rows. Rows that are `InUse` are thus skipped, not re-read: they never hide later rows.
  - For each row, call `collect_obsolete_file(row, hooks, || objects.delete_len(&row.path))` and add up the report.
  - Log one `info!` with the three counts when `deleted + in_use > 0`.
- **`GcStart`** gets `grace: Duration`, from `config.gc.grace`. Each tick of the existing task runs `collect_once` (staged objects), then `collect_obsolete(.., now_ns(), grace)`. An error from either is logged with `warn!` and the task goes on.
- **At open**, right after the existing single `collect_once` call in `open_inner`, run one `collect_obsolete` with `config.gc.grace`. An error is logged, as for `collect_once`.
- **`Storage::collect_obsolete`.** Add it to `lib.rs`, next to `collect_staged`, and re-export `ObsoleteReport` from `lib.rs`:

  ```rust
  /// One obsolete-file collection with this grace. The background task uses `gc.grace`.
  #[doc(hidden)]
  pub async fn collect_obsolete(&self, grace: Duration) -> Result<ObsoleteReport, GcError>
  ```

### Hook points

- **New variants:** `GcObsoleteBeforeDelete` and `GcObsoleteAfterDelete`, which raises `COUNT` to 18.
- **`GcError::Fault`:** its doc and message become "A test fault armed at a GC hook point" / `"garbage collection was interrupted"`, since three hook points now produce it.
- **Doc updates in `catalog/mod.rs`:**
  - `FileState::Obsolete` becomes "Replaced. GC deletes it after `gc.grace`."
  - Remove "M4 uses this;" from `Quarantined` and leave the rest.

### Crash harness (`tests/crash.rs`)

- **`check_dir` object check.** Today it checks that every `data_files` row has an object of the right size. Change it to:
  - an `active` row must have its object, with a matching size;
  - an `obsolete` row's object is either absent or of matching size.

  M4 starts deleting obsolete objects, so this check must change now.
- **Final step of `crash_recovery`.** After `collect_crash_orphans`, open the storage once more and call `collect_obsolete(Duration::ZERO)`. Then shut down and assert that `catalog.db` has no `obsolete` row left. There are none before M4.6, so this is a plumbing test now and becomes a real one in M4.7.

### Tests (unit tests in `gc.rs` and `catalog/committer.rs`)

Build obsolete files with the committer.
- First commit: stage and add a spans file `S`, a traces file `T` and the source `(T, S)`.
- Second commit: remove `(S, Active)` and `(T, Active)`.
- Write real objects for both paths through `objects.put_durable`.

1. **Collected.** With no reader and `grace = 0`:
   - `collect_obsolete` reports `deleted = 2` with the right bytes;
   - both objects are gone;
   - both rows are gone;
   - `SELECT COUNT(*) FROM file_sources` is 0.
2. **Grace.** Read `obsoleted_at` of `S` from `catalog.db`, then collect with `grace = 5 min` and `now = obsoleted_at + 1 min`: nothing is deleted, and both rows stay `obsolete`. With `now = obsoleted_at + 5 min`, both are deleted.
3. **In use.**
   - Hold `committer.catalog().await` from before the removing commit.
   - The first collection reports `in_use = 2` and deletes nothing.
   - Drop the `Arc`. The next collection deletes both.
4. **After a restart.** Same as test 1, but with a fresh `Committer::open` on the same directory before collecting. `retired` is empty, so the grace alone decides.
5. **Missing object.** An obsolete row whose object was never written is deleted with `bytes = 0`.
6. **Fault after the object delete.**
   - `arm_fail(GcObsoleteAfterDelete)`: the collection returns `GcError::Fault`, the object is gone, and the row is still there.
   - A second collection deletes the row and reports `bytes = 0`.
7. **`Storage::collect_obsolete` on a fresh storage** returns `ObsoleteReport::default()`.

**Commit:** `storage: collect obsolete files`

---

## M4.2 — Streaming Parquet writer and temp-file outputs

**Goal:** compaction writes outputs of up to `target_file_size` without holding them in memory (D-A1-2). There are two parts: a Parquet writer that takes already-sorted batches one at a time, and `Objects` support for writing into a temp file and publishing it durably. `write_file` (flush, `traces` outputs) is rebuilt on the same writer, so there is one encoder.

### `parquet.rs`: `FileWriter`

```rust
/// Encodes one Parquet file from batches already in the table's sort order.
///
/// CPU-bound and blocking: call from `spawn_blocking`.
pub(crate) struct FileWriter<W: Write + Send> {
    table: FileTable,
    level: u8,
    writer: ArrowWriter<W>,
    bounds: Option<Bounds>,
    rows: u64,
}

/// What `FileWriter::finish` measured. The footer size is computed by the caller from the
/// file's last 8 bytes (`footer_size_from_tail`), because only the caller can read `W` back.
pub(crate) struct FileSummary {
    pub(crate) rows: u64,
    pub(crate) min_time: i64,
    pub(crate) max_time: i64,
    pub(crate) min_ingest_time: i64,
    pub(crate) max_ingest_time: i64,
}

impl<W: Write + Send> FileWriter<W> {
    pub(crate) fn new(table: FileTable, level: u8, sink: W, expected_rows: u64)
        -> Result<Self, ParquetWriteError>;
    pub(crate) fn write(&mut self, batch: &RecordBatch) -> Result<(), ParquetWriteError>;
    pub(crate) fn rows(&self) -> u64;
    pub(crate) fn finish(self) -> Result<(W, FileSummary), ParquetWriteError>;
}
```

- **`new`:**
  - `level > 1` is `ParquetWriteError::invalid("level must be 0 or 1")`, the existing message.
  - The schema is the table's stored schema: `spans_schema()`, `traces_schema()` or `scores_schema()`.
  - Properties come from `writer_properties(table, level, expected_rows, schema)`. The `bounds` parameter is removed from `writer_properties`, and so is its `set_key_value_metadata` call.
  - **Bloom filter size.** Each row group's column chunk gets a bloom filter sized for `max_ndv`. Today `max_ndv` is the file's row count, which oversizes every filter once a file has several row groups. In `writer_properties`, set `max_ndv` to `expected_rows.clamp(1, ROW_GROUP_ROWS as u64)`. That applies to flush too. The existing tests check that the filters exist, not their size.
- **`write`:**
  - A batch whose schema **fields** differ from the stored schema's fields (`batch.schema().fields() != schema.fields()`) is rejected with the same error `write_file` gives a foreign batch today. Compare the fields only, not the schema metadata: batches from DataFusion carry their own metadata. Make `write_file`'s existing check compare fields the same way.
  - An empty batch is ignored.
  - Otherwise, run the slicing loop of today's `encode` (`WRITE_SLICE_ROWS`, `ROW_GROUP_ROWS`, `ROW_GROUP_BYTES`, `writer.flush()`).
  - Widen `bounds` with `i64_bounds(batch, time_column(table))` and `i64_bounds(batch, ingest_column(table))`.
  - Add the batch's rows to `rows`.
  - `write` does not check the sort order. The caller guarantees it, and `sorting_columns` is still declared.
- **`finish`:**
  - `rows == 0` is `ParquetWriteError::invalid("empty file")`.
  - Otherwise, append every pair of `file_metadata(table, level, &bounds)` with `append_key_value_metadata`, then call `writer.into_inner()`, which flushes the last row group and writes the footer. Return the sink and the summary.
  - **Do not call `writer.finish()` first.** In parquet 59.3.2, `into_inner` after `finish` fails with "SerializedFileWriter already finished".
  - The KV pairs and values are the same as today. They are just written at the end.
- **`write_file` keeps its signature and `WrittenFile`.** Its body becomes:
  1. validate;
  2. `sort_batches`;
  3. `FileWriter::new(table, level, Vec::new(), rows)`;
  4. `write(&sorted)`;
  5. `finish()`;
  6. `bytes = Bytes::from(vec)` and `footer_size(&bytes)`.

  `encode` is deleted. Every existing `parquet.rs` test must pass unchanged. If one compares KV metadata by position rather than by key, say so in the report and compare by key.
- **Footer size from the tail.** Add the function below, and make `footer_size(bytes)` call it with the last 8 bytes and `bytes.len()`:

  ```rust
  /// Footer size (metadata length + 8) from a file's last 8 bytes and its length.
  /// Errors unless the bytes end in `PAR1` and the footer fits in the file.
  pub(crate) fn footer_size_from_tail(tail: &[u8; 8], file_len: u64) -> Result<u64, ParquetWriteError>
  ```

  The existing checks of `footer_size` move into it, with `file_len` in place of `bytes.len()`.
- **Making the module's pieces available:** `FileWriter`, `FileSummary`, `footer_size_from_tail`, `sort_columns`, `time_column` and `ingest_column` are `pub(crate)`.

### `objects.rs`: temp-file outputs

```rust
/// Where an output is being written before it is published as `key`.
pub(crate) struct OutputTarget {
    key: String,
    temp: PathBuf,
}

impl Objects {
    /// Creates the temp file for the object `key` and returns it opened for writing.
    pub(crate) async fn create_output(&self, key: &str)
        -> Result<(OutputTarget, std::fs::File), ObjectError>;
    /// Makes the finished temp file the object `key`, durably. Returns its size in bytes.
    pub(crate) async fn publish_output(&self, target: OutputTarget, file: std::fs::File)
        -> Result<u64, ObjectError>;
    /// Removes a temp file that will not be published. A missing file is success.
    pub(crate) async fn discard_output(&self, target: OutputTarget) -> Result<(), ObjectError>;
}
```

**Local mode** (`Kind::Local { root }`):
- **`create_output`:**
  1. Validate the key with `validate_key`.
  2. Create the destination directory chain exactly as `put_local` does, including the directory fsyncs.
  3. Open `temp`, the same `.<name>.tmp-<uuidv7>` name in the destination directory that `put_local` uses (`temp_name`, objects.rs:354), with `OpenOptions::new().read(true).write(true).create_new(true)`. It must be readable: the caller reads the footer back from it.

  It runs on `spawn_blocking`.
- **`publish_output`**, on `spawn_blocking`:
  1. Stat the file for its size.
  2. Run the existing tail of `put_local` from the `sync_all` on:
     - hook `ObjectSync` and `sync_all`;
     - if the final file exists, remove `temp` and return `ObjectError::AlreadyExists`;
     - hook `ObjectRename` and rename;
     - hook `ObjectDirSync` and the directory fsync; a fault there renames back, as today.
  3. Extract that tail from `put_local` into one private function that both call, so it exists once.
     - Today `put_local` does not remove its temp file when the final file exists. The shared tail does, so `put_local` changes too.
     - Add a check to the existing `AlreadyExists` test that no temp file is left.

  The `ObjectWrite` hook is not fired here: the writing happened through `FileWriter`.
- The existing startup cleanup already removes these temp names.

**Remote mode** (`Kind::Remote`):
- **Spill directory:** `Objects` needs the data directory. Add a field `spill_dir: PathBuf` = `<data_dir>/tmp/compaction`, set in `Objects::new` from `config.data_dir`.
- **`create_output`:**
  1. Create `spill_dir` with `create_dir_all` (no fsync needed: these files are scratch).
  2. Open `temp = spill_dir/<uuidv7>.parquet` with `read(true).write(true).create_new(true)`.
- **`publish_output`:**
  1. Read the size, then `seek(SeekFrom::Start(0))`: the writer left the handle at the end.
  2. If `size ≤ SINGLE_PUT_MAX`, read the whole file into `Bytes` and use the existing single-put path.
  3. Otherwise, do a multipart upload, reading `MULTIPART_PART` bytes at a time from the file. Refactor `put_remote` so the multipart loop takes parts from either source, and keep one implementation of the abort-on-part-failure rule.
  4. Remove `temp` whether the upload succeeded or failed.
  5. Return the upload error as today (`ObjectError::Upload`).

  File reads run on `spawn_blocking` (`tokio::fs` is also fine; `tokio` has the `fs` feature).
- **`discard_output`** removes `temp` in both modes and ignores `NotFound`.

**Startup cleanup:**
- Rename `remove_local_temp_files` to `remove_temp_files`, and update its doc and its caller in `open_inner`.
- In local mode it does what it does today. In remote mode it removes every file in `spill_dir`, which may not exist; that's fine.
- It returns the count of files removed, as today.

### Tests

Parquet (`parquet.rs`):
1. **Same file both ways.** Write the same sorted spans rows (three batches of 1,000 rows) with `write_file`, and with `FileWriter<std::fs::File>` on a `tempfile`. Read both back with `ParquetRecordBatchReaderBuilder` and check:
   - same rows;
   - same KV pairs, compared as a map;
   - same `sorting_columns`;
   - bloom filters present on the same columns;
   - `footer_size` equal to the value computed from each file's last 8 bytes.
2. **Row groups.** 250,000 one-column-light spans rows (use the existing small-row helper behind `many_small_rows_use_three_row_groups`) written in 30 batches give 3 row groups.
3. **Rejected input.** `finish` without rows is `invalid("empty file")`, and a scores batch given to a spans writer is rejected.

Objects (`objects.rs`):

4. **Local publish.**
   - `create_output`, write 10 bytes, then `publish_output`: the object has the 10 bytes, no temp file is left, and the size returned is 10.
   - With test hooks, the recorded order is `ObjectSync`, `ObjectRename`, `ObjectDirSync`.
5. **Local, final exists.** When the final file exists, `publish_output` returns `AlreadyExists` and the temp file is removed.
6. **Discard and cleanup.**
   - `discard_output` removes the temp file.
   - A leftover temp file from `create_output` is removed by `remove_temp_files`.
7. **Remote** (`Objects::remote_for_test` over `object_store::memory::InMemory`, with a data directory from `tempfile`):
   - a 1 KiB output is uploaded with a single put;
   - an output of `SINGLE_PUT_MAX + 1` bytes is uploaded in parts and reads back equal;
   - the spill file is gone after both;
   - `remove_temp_files` removes a leftover spill file.

   `remote_for_test` must take the data directory: add the parameter and update its callers.
8. **Remote failure.** A `FaultStore` armed with one `FailBefore`, with an output of `SINGLE_PUT_MAX + 1` bytes. `FaultStore` faults the `put_multipart_opts` call, not single parts, so the upload fails when it starts:
   - `publish_output` returns `ObjectError::Upload`;
   - the spill file is removed;
   - no object exists.

**Commit:** `storage: stream Parquet outputs through temp files`

---

## M4.3 — One dedup module, and a source check at commit

**Goal:** flush and compaction collapse duplicate spans with the same code, and the committer refuses an edit that would leave a `spans` file without its `traces` file. Compaction relies on both.

### `src/dedup.rs` (new module)

```rust
/// Latest version of each span: the greatest `ingest_time` per `(trace_id, span_id)`.
/// An equal `ingest_time` keeps the later row. Kept rows keep their input order.
pub(crate) fn latest_spans(batch: &RecordBatch) -> Result<RecordBatch, DedupError>;
```

M4.4 adds `latest_scores` to this module, together with its only caller, so no dead code is committed.

- **`latest_spans`** is the body of `flush::dedup_batch`, moved here. Delete `dedup_batch` from `flush.rs`.
  - Flush calls `dedup::latest_spans(&partition.batch).map_err(|error| FlushError::dedup(error.reason()))`.
  - The `#[cfg(test)] dedup_spans` helper in `flush.rs` now concatenates its batches and calls `latest_spans`.
- **`DedupError`** lives in `error.rs`. It is `pub`, because `CompactionError` (M4.4) is `pub` and wraps it:

  ```rust
  /// A batch could not be deduplicated. `reason` is a static description, never row content.
  #[derive(Debug, thiserror::Error)]
  #[error("dedup failed: {reason}")]
  pub struct DedupError { reason: &'static str }
  impl DedupError { pub(crate) fn new(reason: &'static str) -> Self; pub(crate) fn reason(&self) -> &'static str; }
  ```
- **Reasons**, as `&'static str`:
  - `"missing identity column"`;
  - `"null span identity"`;
  - `"row index"`;
  - `"arrow"`, for a `take` failure. Log the arrow error with `warn!` before returning, without row content.

### Committer: removed files keep their sources whole

In `write_edit` (`catalog/committer.rs`), after the existing source check and before any write, add this check for every `(id, _)` in `edit.remove`. It looks up the row's `table_name`:
- **A removed `spans` file:** every **active** `traces` file that has a `file_sources` row naming it must also be in `edit.remove`. Otherwise it is `CommitError::conflict("a removed spans file keeps its traces file")`. The query:

  ```sql
  SELECT fs.traces_file_id FROM file_sources AS fs
  JOIN data_files AS t ON t.id = fs.traces_file_id
  WHERE fs.spans_file_id = ? AND t.state = 'active'
  ```
- **A removed `traces` file:** every `spans_file_id` of its `file_sources` rows must also be in `edit.remove`. Otherwise it is `CommitError::conflict("a removed traces file keeps a source")`. The query: `SELECT spans_file_id FROM file_sources WHERE traces_file_id = ?`.

`scores` rows need no check. Flush edits have an empty `remove`, so they are unaffected. Don't add any check on added files in M4: the open-time `check_file_sources` stays the guard for those.

### Tests

`dedup.rs`:
1. **Spans.** Higher `ingest_time` wins in either order. An equal `ingest_time` keeps the later row. Kept rows stay in input order. A null `span_id` is an error.

`catalog/committer.rs`. `CatalogEdit` has no `Default`, so write every field (`add`, `remove`, `flushed_seq`, `sources`) in each literal:

2. **A valid compaction-shaped edit commits.**
   - Active `S1, T1 (←S1), S2, T2 (←S2)`.
   - The edit adds `S3, T3` with source `(T3, S3)` and removes `S1, T1, S2, T2`.
   - After `Committer::open` on the same directory, `check_file_sources` passes.
3. **Removing `S1` without `T1`** is the conflict `"a removed spans file keeps its traces file"`. Nothing changed: rows and states are as before, and the in-memory catalog is the same.
4. **Removing `T1` without `S1`** is the conflict `"a removed traces file keeps a source"`.
5. **Flush still dedups through the moved code.** The existing flush dedup tests pass unchanged.

**Commit:** `storage: share dedup and check removed sources at commit`

---

## M4.4 — Time slices and the merge

**Goal:** given the input files of one partition of `spans` or `scores` and one time slice, stream the merged, deduplicated rows into a `FileWriter`. For `spans`, also return the slice's trace partials. This is the core of every compaction job (spec 03 § Slicing, § During a merge). M4.6 calls it. This sub-phase tests it on files written by the tests themselves.

### Module layout

Create `src/compaction/` with `mod.rs` (module doc pointing to spec 03 §5), `slice.rs` and `merge.rs`. M4.5 adds `plan.rs`, and M4.6 adds `job.rs` and `task.rs`.
- Declare `mod compaction;` in `lib.rs`.
- Until M4.6 calls them, the items are reached from tests only. Put `#![allow(dead_code)]` at the top of `compaction/mod.rs` with the comment `// Wired into Storage in M4.6.`, and remove it in M4.6. This is the pattern `parquet.rs` and `catalog/db.rs` used before they were wired in.

### `CompactionError` (`error.rs`, `pub`)

```rust
/// A compaction job or pass failed. No variant carries row content.
#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    /// Reading or merging the input files failed.
    #[error("reading compaction inputs failed")]
    Read { #[source] source: datafusion::error::DataFusionError },
    /// Encoding an output failed.
    #[error(transparent)]
    Write(#[from] ParquetWriteError),
    /// Writing, publishing or deleting an object failed.
    #[error(transparent)]
    Object(#[from] ObjectError),
    /// The catalog refused or failed the edit, or a catalog read failed.
    #[error(transparent)]
    Commit(#[from] CommitError),
    /// Recording the output paths in `staged_objects` failed (`Committer::stage`).
    #[error(transparent)]
    Stage(#[from] CatalogError),
    /// Duplicate collapsing failed on corrupt input.
    #[error(transparent)]
    Dedup(#[from] DedupError),
    /// Computing trace partials failed.
    #[error(transparent)]
    Partials(#[from] PartialsError),
    /// The job's inputs break an invariant it relies on.
    #[error("compaction input: {reason}")]
    Input { reason: &'static str },
    /// A blocking task panicked or was cancelled.
    #[error("compaction task failed")]
    Task,
    /// A test hook failed the job.
    #[error("compaction stopped by a test hook")]
    Fault,
    /// The storage is shutting down; the job gave up before committing.
    #[error("compaction stopped for shutdown")]
    Stopped,
}
```

- **`PartialsError`** is `pub(crate)` today. Make it `pub`. It lives in `error.rs`; its fields are a `String` column name and an `ArrowError`, both fine in a public type.
- **`Read` has no `#[from]`.** Map every `DataFusionError` explicitly with `.map_err(|source| CompactionError::Read { source })`. That covers `ObjectStoreUrl::parse`, `FilterExec::try_new`, `execute_stream`, the stream's items and `ParquetError`s from the readers (`DataFusionError::ParquetError(Box::new(error))`).
- **`Stopped`** is used from M4.6.

### `slice.rs`

```rust
/// Rows with `lower <= time < upper`. `None` is unbounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Slice { pub(crate) lower: Option<i64>, pub(crate) upper: Option<i64> }

/// `max(1, ceil(input_bytes / target))`. `target` is at least 1.
pub(crate) fn slice_count(input_bytes: u64, target: u64) -> usize;

/// Slices from the sorted time values of every input row (spec 03 § Slicing).
pub(crate) fn slices(sorted: &[i64], count: usize) -> Vec<Slice>;

/// The `column` values of every row of `files`, sorted ascending.
pub(crate) async fn time_values(
    store: Arc<dyn ObjectStore>, files: &[Arc<FileMeta>], column: &str,
) -> Result<Vec<i64>, CompactionError>;
```

**`slices`:**
- If `count <= 1` or `sorted.is_empty()`, return `[Slice { lower: None, upper: None }]`.
- Otherwise, the candidate boundaries are `sorted[sorted.len() * i / count]` for `i` in `1..count`, in that order.
- Drop each candidate equal to `sorted[0]` or equal to the previous kept boundary. That leaves strictly increasing boundaries above the minimum.
- With kept boundaries `b1 < … < bk`, the slices are `[None, b1)`, `[b1, b2)`, …, `[bk, None)`.
- Each slice is non-empty: the first holds the minimum, and the others hold their lower bound.

**`time_values`:**
- For each file, open `ParquetRecordBatchStreamBuilder::new(ParquetObjectReader::new(store, ObjectPath::from(file.path.as_str())).with_file_size(file.size))`, as `anchor.rs` does.
- Project the one leaf column by name with `ProjectionMask::columns(builder.parquet_schema(), [column])`.
- Read every batch, and push the values from the `TimestampNanosecondArray`. A null or a missing column is `CompactionError::Input { reason: "missing time column" }`.
- `sort_unstable` the result.
- Errors from the reader map to `CompactionError::Read`, through `DataFusionError::ParquetError` or `DataFusionError::External`.
- Memory is 8 bytes per input row. That is bounded by the job's input size, and the report documents it.

### `merge.rs`

```rust
/// Process-wide settings of the merge, built once at open (M4.6) and by tests.
pub(crate) struct MergeContext {
    /// Memory pool `compaction.max_memory` and the object store (`Objects::register`).
    pub(crate) runtime: Arc<RuntimeEnv>,
    /// Rows per batch read from the inputs.
    pub(crate) batch_size: usize,
}

/// What `merge_slice` produced.
pub(crate) struct MergedSlice<W: Write + Send> {
    pub(crate) writer: FileWriter<W>,
    /// `spans` only: one partial batch per emitted chunk (`traces_schema()`).
    pub(crate) partials: Vec<RecordBatch>,
    /// Rows written to `writer`.
    pub(crate) rows: u64,
}

/// Merges `inputs` (one partition of `table`, `Spans` or `Scores`) restricted to `slice`
/// into `writer`.
pub(crate) async fn merge_slice<W: Write + Send + 'static>(
    ctx: &MergeContext,
    table: FileTable,
    inputs: &[Arc<FileMeta>],
    slice: Slice,
    drop_deleted: bool,
    writer: FileWriter<W>,
) -> Result<MergedSlice<W>, CompactionError>;
```

`table` must be `Spans` or `Scores`; `Traces` is `CompactionError::Input { reason: "traces are not merged" }`. `drop_deleted` is ignored for spans.

**The plan.** Build it by hand from DataFusion pieces; there is no SQL and no `SessionContext`.
1. **Order the inputs** by `FileId` ascending. UUIDv7, so oldest first.
2. **Build the scan.** One `FileGroup` per input, each holding one `PartitionedFile::new(path, size)` with `with_metadata_size_hint(footer_size)`, like `query/mod.rs::file_groups` does per file.
   - Source: `ParquetSource::new(schema)` with `with_pushdown_filters(true)`, plus the slice predicate (below) as its predicate when the slice has a bound. `schema` is the table's stored schema.
   - Config: `FileScanConfigBuilder::new(ObjectStoreUrl::parse(OBJECT_STORE_URL)?, source)`, `.with_file_groups(groups)` and `.with_output_ordering(vec![ordering])`, then `DataSourceExec::from_data_source(builder.build())`.
   - **One file per group** matters. A group with several files concatenates them, so it is not sorted.
3. **The ordering** is the table's `sort_columns`: `trace_id, start_time` for spans and `created_at` for scores. Each column is a `PhysicalSortExpr` on `Column::new(name, index in schema)`, with `SortOptions { descending: false, nulls_first: false }`, the order the writer declares. Build it with `LexOrdering::new(...)`, which returns an `Option`; `None` is `CompactionError::Input { reason: "empty ordering" }`.
4. **The slice predicate**, when the slice has a bound: `time >= lower AND time < upper`, either side present only when bounded.
   - `time` is the table's time column (`time_column(table)`).
   - Literals are `ScalarValue::TimestampNanosecond(Some(v), tz)`, where `tz` is the timezone of that column's `DataType::Timestamp` in the stored schema.
   - Build it as physical expressions: `datafusion::physical_expr::expressions::{BinaryExpr, Column, Literal}` with `Operator::GtEq`, `Operator::Lt`, `Operator::And`.
   - Wrap the scan in `FilterExec::try_new(predicate, scan)`. The source predicate only prunes; the `FilterExec` makes the slice exact, and it keeps the input order.
5. **Merge:** `SortPreservingMergeExec::new(ordering, filter_or_scan).with_round_robin_repartition(false)`. The default (`true`) breaks ties between inputs round-robin. With `false`, rows with equal keys come out in input order, which the tie rule below relies on.
6. **Run it** with `execute_stream(plan, task)`, where `task = Arc::new(TaskContext::default().with_session_config(SessionConfig::new().with_batch_size(ctx.batch_size)).with_runtime(Arc::clone(&ctx.runtime)))`. Map stream errors to `CompactionError::Read`.

**Chunks: whole groups only.** Duplicates are collapsed per **group**: a trace for spans, a `created_at` value for scores. A group's rows are contiguous in the merged order, but a group can straddle two batches, so carry the last group over:
- keep `pending: Option<RecordBatch>`;
- for each batch from the stream, let `combined = concat_batches(pending, batch)`;
- let `last` be the key of `combined`'s last row, and `split` the first row index whose key equals `last`, found by scanning back from the end;
- emit `combined.slice(0, split)` when `split > 0`, and set `pending = combined.slice(split, len − split)`;
- when the stream ends, emit `pending` if it holds rows.

The key is the `trace_id` bytes (`FixedSizeBinaryArray`) for spans and the `created_at` value for scores.

**Emitting a chunk:**
- **Spans:**
  1. `let rows = dedup::latest_spans(&chunk)?`;
  2. `partials.push(partials_from_spans(&rows)?)`;
  3. write `rows`.
- **Scores:**
  1. `let rows = dedup::latest_scores(&chunk, drop_deleted)?`;
  2. if it has rows, write them.

`FileWriter::write` is blocking, so move the writer into `tokio::task::spawn_blocking(move || { writer.write(&rows)?; Ok((writer, ...)) })` and take it back. A `JoinError` is `CompactionError::Task`.

`dedup` keeps input order, so the written rows stay in the table's sort order.

**Ties.** With round-robin tie breaking off, `SortPreservingMergeExec` emits rows with equal sort keys from different inputs in input order (the older file first). With the "later row wins" rule of `dedup`, a full tie keeps the copy from the newer file. Tests don't depend on full ties.

### `dedup::latest_scores`

Add it to `src/dedup.rs` in this sub-phase, next to `latest_spans`:

```rust
/// Latest version of each score: the greatest `(updated_at, ingest_time)` per `score_id`.
/// A full tie keeps the later row. With `drop_deleted`, a kept row with `deleted = true`
/// is then removed. Kept rows keep their input order.
pub(crate) fn latest_scores(batch: &RecordBatch, drop_deleted: bool)
    -> Result<RecordBatch, DedupError>;
```

- `score_id` is `Utf8`. Hash on the string: a `HashMap<&str, (i64, i64, usize)>` over the batch's values avoids copies.
- Compare `(updated_at, ingest_time)` as a tuple of `i64`. The greater tuple wins; on an equal tuple the later row wins.
- Any null in `score_id`, `updated_at`, `ingest_time` or `deleted` is `DedupError::new("null score identity")`. The schema forbids nulls there, so this is corrupt input.
- A missing column is `"missing identity column"`, and a `take` failure is `"arrow"`, as in `latest_spans`.

### Tests (`compaction/merge.rs` and `compaction/slice.rs`)

**Setup:**
- Write input files with `write_file(table, 0, …)` plus `Objects::put_durable` into a local `Objects` on a `tempfile` directory.
- Build each `FileMeta` from the `WrittenFile` (level 0, today's fields).
- `MergeContext` uses `RuntimeEnvBuilder::new().with_memory_pool(Arc::new(GreedyMemoryPool::new(64 << 20))).build_arc()` plus `objects.register(&runtime)`. Use `batch_size = 8192` unless a test says otherwise.
- Read outputs back by writing them to a `tempfile` through `FileWriter<std::fs::File>` and reading it with `ParquetRecordBatchReaderBuilder`.

**Span rows.** The existing builders (`wal::spans_keyed` and its wrappers) fix the trace id and set `ingest_time = start + 200 ns`. Add one builder to `test_support.rs` that sets both ids and both times; M4b's tests use it too:

```rust
/// One span of the fixed test project, with the other fields as `wal::spans_keyed` sets them.
pub(crate) fn span_row(trace: u16, span: u8, start: i64, ingest: i64) -> Span
/// `spans_to_record_batch(spans)`, unwrapped.
pub(crate) fn span_rows(spans: &[Span]) -> RecordBatch
```

- `trace_id` is the 2 big-endian bytes of `trace + 1`, repeated 8 times. `span_id` is 8 bytes of `span + 1`.
- The `+ 1` keeps the ids from being all zero, which the id types reject. `trace` must be below `u16::MAX` and `span` below `u8::MAX`; the builder panics otherwise (test code).
- Copy the field list of `spans_keyed`, with `parent_span_id: None` and `name: "root"`.
- A test that needs another field (`parent_span_id`, `input`) sets it on the returned `Span`.

Build scores batches as the `query/scores.rs` tests do.

1. **`slice_count`:**
   - `(0, t) → 1`;
   - `(t, t) → 1`;
   - `(t + 1, t) → 2`;
   - `(10t, t) → 10`.
2. **`slices`:**
   - `count = 1` gives one open slice;
   - `[1..=100]` with `count = 4` gives boundaries `26, 51, 76`;
   - all-equal values give one slice;
   - a candidate equal to the minimum is dropped;
   - duplicate candidates collapse.
3. **`time_values`** returns the sorted `start_time` values of two files.
4. **Spans merge.**
   - Three inputs; traces `A` and `B` have spans in all three.
   - Span `A/1` appears in input 1 with `ingest_time` 10 and in input 3 with 20.
   - One open slice gives:
     - rows sorted by `(trace_id, start_time)`;
     - one `A/1` row, with `ingest_time` 20;
     - a row count equal to the distinct spans;
     - partials, concatenated and sorted by `trace_id`, equal to `partials_from_spans` of the whole output, sorted the same way.
5. **Two slices.**
   - Same inputs. Use `slices` with `count = 2` on `time_values`.
   - The two outputs together equal the one-slice output.
   - Every row of slice 1 is below the boundary, and every row of slice 2 is at or above it.
   - A trace with spans on both sides has one partial in each slice.
6. **A trace straddling batches.**
   - `batch_size = 3`. A trace with 7 spans plus a duplicate of one of them (higher `ingest_time`) in another input.
   - The output has the 7 distinct spans, with the newer duplicate.
   - Its partials hold exactly one row for that trace.
7. **Scores.**
   - Score `s` has versions in 3 inputs: `updated_at` 1, 3 and 2, and the version at 3 has `deleted = true`.
   - With `drop_deleted = false`, the output has one row for `s`: the deleted version at 3.
   - With `true`, `s` is absent.
   - A score `t` with one live version is kept in both.
8. **`latest_scores` (`dedup.rs`):**
   - a greater `updated_at` wins;
   - with an equal `updated_at`, the greater `ingest_time` wins;
   - on a full tie the later row wins;
   - with `drop_deleted = false` a winning deleted version stays, and with `true` it is removed together with its older versions, which already lost.
9. **Streaming plan.** The plan built for test 4 (expose the builder as `fn merge_plan(...) -> Result<Arc<dyn ExecutionPlan>, CompactionError>` and call it) contains `SortPreservingMergeExec` and no `SortExec`. Check with `displayable(plan.as_ref()).indent(true).to_string()`.

**Commit:** `storage: merge partition files by time slice`

Continue with `08-m4b-compaction.md`.
