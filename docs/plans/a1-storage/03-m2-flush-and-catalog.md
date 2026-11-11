# A1 · M2 — Flush and catalog

Implementation plan for milestone M2 of the storage track. One agent does the sub-phases in order, on one branch. Each sub-phase ends with `just check` green and one commit, and M2 lands as **one PR**.

**Prerequisites:** M1 is merged (`storage: add crash harness and race tests`). The A2 query layer (`src/views/`, `src/query_*.rs`) is on `main` and must keep passing.

**Read first:**
- `AGENTS.md`, all of it.
- `docs/plans/a1-storage/00-design.md`: § Concurrency model, § Durability, § Commit protocol, § Catalog, § Query resources, § Testing.
- `docs/spec/03-storage.md`: § Layout, §1 (checkpoint, recovery), §2 (memtable, flush), §3 (Parquet files), §4 (catalog, read state, committer, metadata cache), §5 (crash safety, staged objects), §6 (`SpansProvider`, `ScoresProvider`), §10.
- `docs/spec/12-operations.md` § Configuration, § Process lifecycle.
- `docs/mvp-decisions.md`: D39, D83, D84, D87, D89, D90, D91, and D102 (added by M2.1, text below).
- The M1 code: `crates/storage/src/{lib.rs,memtable.rs,read_state.rs,hooks.rs,wal/,query/}` and `tests/crash.rs`.

## What M2 delivers

After M2, the engine is complete for writing and reading `spans` and `scores`:
- memtables flush to Parquet files on local disk or S3;
- `catalog.db` records the files and the WAL checkpoint, and the WAL is truncated after each flush;
- queries read files and memtable together, seeing each row exactly once;
- a restart replays only what wasn't flushed;
- crashes at any step of a flush lose nothing acknowledged and leave no duplicates.

**Still missing after M2:**
- `traces` (M3): flush writes no `traces` files yet, and the `traces` provider stays the empty table M1 left.
- Compaction (M4): L0 files accumulate, one per active `(project, day)` per flush.
- Deletion, retention and media (M5).
- The memory pool and disk-full handling (M6).

M2 never removes an active file: no file becomes obsolete before M4/M5. So obsolete-file GC with reference counts is M4's; M2's GC only collects staged leftovers.

## Decisions this plan relies on

| # | Summary |
|---|---|
| D39 | `sqlx` for SQLite. |
| D83 | Storage owns `catalog.db`, `synchronous=FULL`. |
| D84 | Files per `(project, day)`; a flush uploads in parallel and commits once. |
| D87 | Files durable before active; staged objects; serialized compare-and-set commits. |
| D89 / D91 | Providers expose stored schemas; `ProjectQuery` carries the providers, a `RuntimeEnv` and `target_partitions`; A2 builds the session. |
| D90 | Nested `[storage]` config, human units. |
| **D102 (new, M2.1)** | **S3 testing.** CI tests the S3 code paths through `object_store`'s `InMemory` store wrapped in a fault-injecting store. A MinIO integration test runs only when `LLMOBS_TEST_S3_ENDPOINT` is set. It is documented in `AGENTS.md` and run locally before merging storage changes that touch object writes. |

User answers behind this plan: S3 is in M2, tested with MinIO opt-in; M2 is one PR.

## Versions

Add each to the root `[workspace.dependencies]` and use `workspace = true`. Re-check with `cargo info`, take the latest release at least 7 days old, and report the exact versions.

| Crate | Version found | Features | Why |
|---|---|---|---|
| `sqlx` | 0.9.0 | `default-features = false`, `["runtime-tokio", "sqlite", "migrate", "macros"]` | `catalog.db` (D39). `macros` only for `sqlx::migrate!`. Use the runtime query API (`sqlx::query`), not `query!`, so no `DATABASE_URL` or offline metadata is needed. The server will use the same crate for `meta.db`: one SQLite library in the binary. |
| `object_store` | 0.13 (pinned family) | add `["fs", "aws"]` in `crates/storage/Cargo.toml` | Local reads and S3. Don't change the pin. |
| `parquet` | 59.2 (pinned family) | as pinned | Writing files. Storage needs a direct dependency now. |
| `uuid` | workspace | `v7` | File names. |

`just deny` must pass. If `aws` pulls in a crate that cargo-deny bans or flags (advisories, sources), stop and report rather than adding exceptions.

## Conventions

M1's conventions still apply:
- blocking I/O in `spawn_blocking`;
- one `thiserror` enum per operation;
- no row content in errors or logs;
- no panics on disk input;
- test hooks instead of sleeps.

On top of that:
- **Lock order.** Committer mutex → a table's memtable mutex → the `Published` write lock. Never take them in another order. A function that takes two of them documents it.
- **Object paths:** `tables/<table>/project=<project_id>/day=<YYYY-MM-DD>/<level>-<uuidv7>.parquet` (spec 03 § Layout), relative to the object store root. One function builds them; nothing else formats paths.
- **Catalog writes go through the committer.** Except: inserting `staged_objects` rows (§ M2.2), and deleting them in GC (§ M2.8).

## Final layout after M2

New or changed modules. `views/` and `query_*.rs` belong to A2: don't edit them, except where the M2.7 interface requires it (say so in the report).

```
crates/storage/
  migrations/catalog/0001_catalog.sql   data_files, staged_objects, table_state
  src/
    config.rs        + object_store, s3, flush, query.metadata_cache_bytes
    catalog/
      mod.rs         Catalog (in memory), FileMeta, FileId, load at open
      db.rs          catalog.db: connect, pragmas, migrations, row mapping
      committer.rs   CatalogEdit, Committer, staged rows
    objects.rs       object store setup, durable local writes, S3 puts, paths, runtime registration
    parquet.rs       writer properties per table, sorting, file writing, stats
    flush.rs         flush task: triggers, steps 1–6, retries, WAL truncation
    gc.rs            staged-object GC
    memtable.rs      + frozen, max_seq, oldest-insert age
    read_state.rs    + catalog
    query/           + file scans, scores last-writer-wins plan
    hooks.rs         + flush and object points, abort action
  tests/
    crash.rs         + flushes in the child, catalog checks
    s3.rs            MinIO test, opt-in
```

---

## M2.1 — Dependencies, configuration and D102

**Edit:** root `Cargo.toml`, `Cargo.lock`, `crates/storage/Cargo.toml`, `src/config.rs`, `src/error.rs`, `docs/mvp-decisions.md`, `docs/spec/12-operations.md`, `AGENTS.md`.

### Steps

1. Add the dependencies of § Versions.
2. Add to `StorageConfig`:

   | Field | Type | Default | Validation |
   |---|---|---|---|
   | `object_store` | `ObjectStoreUrl` | `file://<data_dir>/objects` | `file://<absolute path>` or `s3://<bucket>[/<prefix>]`. Any other scheme fails, naming the field. |
   | `s3.region` | `Option<String>` | none (`AWS_REGION` from env) | |
   | `s3.endpoint` | `Option<String>` | none | MinIO, R2, … |
   | `s3.force_path_style` | `bool` | `false` | |
   | `s3.allow_http` | `bool` | `false` | Needed for a local MinIO. |
   | `flush.max_bytes` | `ByteSize` | `256MiB` | ≥ 1 MiB and < `memtable.hard_limit` |
   | `flush.max_age` | `Duration` | `120s` | ≥ 100 ms, ≤ 1 h |
   | `flush.upload_concurrency` | `usize` | `8` | 1–64 |
   | `query.metadata_cache_bytes` | `ByteSize` | `512MiB` | ≥ 0 |

   - Credentials come only from the standard AWS environment (spec 12). Never from the config file, never logged.
   - `ObjectStoreUrl` is an enum `Local(PathBuf) | S3 { bucket, prefix }`, deserialized from the string.
   - The `file://` default depends on `data_dir`, so it is resolved after deserialization, through a method like `object_store_url()`. Keep the field an `Option` in serde.
3. Spec 12's configuration table: add `storage.object_store` (it already has a row; update it to the D90 form), `storage.s3.*`, `storage.flush.upload_concurrency` and `storage.query.metadata_cache_bytes`.
4. Add D102 to `docs/mvp-decisions.md` (text in § Decisions).
5. `AGENTS.md` § Tests: "`LLMOBS_TEST_S3_ENDPOINT` (with `LLMOBS_TEST_S3_BUCKET` and the `AWS_*` credentials) runs the storage MinIO test (D102)."

### Tests

- The default object store resolves under `data_dir`.
- `s3://bucket` and `s3://bucket/a/b` parse; `http://x`, `file://relative` and `s3://` fail with the field name.
- Each new bound fails `validate`.
- `flush.max_bytes ≥ memtable.hard_limit` fails.

**Commit:** `storage: add flush, object store and catalog configuration`

---

## M2.2 — `catalog.db`: schema, loading and the committer

**Goal:** the storage catalog of spec 03 §4, for the tables M2 needs. It is loaded into memory at open, and changed only through a committer that runs compare-and-set edits and publishes the read state.

**Edit:** `migrations/catalog/0001_catalog.sql`, `src/catalog/{mod.rs,db.rs,committer.rs}`, `src/read_state.rs`, `src/error.rs`, `src/hooks.rs`, `src/lib.rs` (module declarations; `open` wiring comes in M2.6).

### Migration `0001_catalog.sql`

Exactly spec 03 §4's definitions for `data_files` (without `deletions_applied_seq`, which M5 adds with its own migration), `staged_objects` and `table_state`, plus their indexes. `file_sources` comes with M3; `projects`, `retention_watermarks`, `media` and the `deletion*` tables with M5. **No table without a user** (AGENTS.md § No placeholders). Migrations are forward-only and embedded with `sqlx::migrate!("./migrations/catalog")`.

### `db.rs`

- `open_catalog_db(data_dir) -> Result<SqliteConnection, CatalogError>`: `<data_dir>/catalog.db`, created if missing, then:
  - `journal_mode=WAL`;
  - `synchronous=FULL` (spec 03 §4: the `flushed_seq` commit must survive a power loss);
  - `foreign_keys=ON`;
  - `busy_timeout=5000`.

  Then run the migrations. A single connection: storage is the only writer, and readers use the in-memory catalog.
- `load(conn) -> Result<(Catalog, TableState), CatalogError>` reads every `active` row and both `table_state` rows (missing rows mean `flushed_seq = 0`).
- Row mapping lives here: `FileMeta` ↔ row.
  - IDs are UUIDv7 text.
  - Times are `INTEGER` ns.
  - `day` is `YYYY-MM-DD`.
  - `table_name` is `spans` or `scores`; any other value read from disk is a `CatalogError::Corrupt { what }`, never a panic.

### `mod.rs`: the in-memory catalog

```rust
pub(crate) struct FileId(Uuid);
pub(crate) struct FileMeta {
    id: FileId, table: Table, project: ProjectId, day: NaiveDate, level: u8, path: String,
    size: u64, footer_size: u64, rows: u64,
    min_time: i64, max_time: i64, min_ingest_time: i64, max_ingest_time: i64,
    fully_merged: bool,
}
pub(crate) struct Catalog {
    // table → project → day → files, each list ordered by (min_time, id)
    tables: [BTreeMap<ProjectId, BTreeMap<NaiveDate, Vec<Arc<FileMeta>>>>; 2],
}
```

- `Catalog` is immutable once published.
- A commit builds the next one by cloning only the maps on the changed paths. Use `Arc` around per-project day maps so an unchanged project costs one `Arc` clone.
- Accessor: `files(table, project, days: RangeInclusive<NaiveDate>) -> impl Iterator<Item = &Arc<FileMeta>>`.

### `committer.rs`

```rust
pub(crate) struct CatalogEdit {
    pub add: Vec<FileMeta>,                    // paths must be in staged_objects
    pub remove: Vec<(FileId, FileState)>,      // expected current state; M2 never uses this
    pub flushed_seq: Option<(Table, u64)>,     // compare-and-set: must be > current
}

pub(crate) struct Committer { inner: tokio::sync::Mutex<CommitterInner> }
// CommitterInner: the connection, the current Arc<Catalog>, the current flushed_seq per table

impl Committer {
    /// Records `paths` in staged_objects in one small transaction. No precondition.
    pub(crate) async fn stage(&self, paths: &[String]) -> Result<(), CatalogError>;

    /// Applies `edit` if its preconditions hold, then calls `publish` with the new catalog,
    /// still holding the committer mutex.
    pub(crate) async fn commit<F>(&self, edit: CatalogEdit, publish: F) -> Result<Arc<Catalog>, CommitError>
    where F: FnOnce(&Arc<Catalog>);
}
```

`commit`, in one transaction:
1. Check every `add` path is in `staged_objects`. Check every `remove` row is in its expected state. Check `flushed_seq` is greater than the stored value.
2. Insert the `data_files` rows and delete their `staged_objects` rows.
3. Update the removed rows (not used in M2: keep the code path tested anyway, since M4 needs it and the schema already has `state`).
4. Upsert `table_state`.
5. Commit. Then build the new `Catalog` and call `publish(&catalog)` before releasing the mutex.

A failed precondition rolls back and returns `CommitError::Conflict { reason }`; the caller's objects stay staged. A SQLite error returns `CommitError::Db`. After a SQLite error, the committer's in-memory state is untouched: it changes only after a successful commit.

### Read state

`ReadState` gains `catalog: Arc<Catalog>`.

The `publish` closure is how a commit and a memtable change become one atomic publish (spec 03 §2 step 5). The flush passes a closure that:
1. locks the table's memtable;
2. drops the frozen memtable;
3. takes the snapshot;
4. calls `Published::replace` with the new catalog and that snapshot;
5. unlocks.

The WAL writer's publish (M1) becomes: lock the memtable, snapshot, `Published::replace` keeping the current catalog, unlock. Both follow the lock order of § Conventions, so a snapshot taken before a flush commit can never be published after it.

### Hooks

Add `HookPoint::CatalogCommit` (before the SQLite transaction commits; `fail` makes it error out).

### Tests

- Open creates `catalog.db` with the pragmas set (query them back), and migrations run once.
- `stage` + `commit` adds files; a reload from disk returns the same `Catalog`.
- Conflicts: a path not staged, a remove of a file in the wrong state, and a `flushed_seq` not greater than the stored one. Each rolls back: no row changes and the in-memory catalog is unchanged.
- An injected `CatalogCommit` failure leaves both the database and memory unchanged.
- Corrupt rows (unknown `table_name`, a bad date, a bad UUID) fail `load` with `Corrupt`, not a panic.
- Concurrency: 16 tasks committing disjoint edits all succeed, and the final catalog holds every file; published catalogs only ever grow (check every intermediate publish).

**Commit:** `storage: add catalog.db and the committer`

---

## M2.3 — Object store and durable writes

**Goal:** one place that knows where objects go and how they become durable (spec 03 §2 step 4, D87).

**Edit:** `src/objects.rs`, `src/hooks.rs`, `src/error.rs`, `src/lib.rs` (module declaration), `src/test_support.rs` (the fault store).

### `objects.rs`

```rust
pub(crate) struct Objects { kind: Kind, store: Arc<dyn ObjectStore>, url: ObjectStoreUrl /* datafusion's */ }
enum Kind { Local { root: PathBuf }, Remote }
```

- `Objects::new(config)`:
  - **Local:** create `root` (fsync parents); `store` is `LocalFileSystem::new_with_prefix(root)`, used for reads only.
  - **S3:** `AmazonS3Builder::from_env()` with the config's bucket, region, endpoint, path style and `allow_http`, then `with_prefix` for the prefix (or `PrefixStore`).
- `register(&self, runtime: &RuntimeEnv)`: registers `store` under a fixed URL (`llmobs://objects/` or similar) that the providers use in their `FileScanConfig`. Document the URL in one constant.
- `table_file_path(table, project, day, level) -> (String, FileId)`: the § Conventions path with a fresh UUIDv7.
- `put_durable(&self, path: &str, bytes: Bytes) -> Result<(), ObjectError>`:
  - **Local**, in `spawn_blocking`:
    1. create missing parent directories (fsync each new directory's parent);
    2. write to `<dir>/.<name>.tmp-<uuid>`;
    3. `sync_all`;
    4. `rename` to the final name;
    5. fsync the directory.

    Each step passes through a hook point (below). An existing final file is an error, since names are unique.
  - **Remote:** `put` for up to 64 MiB, else `put_multipart` in 16 MiB parts. Any error is returned as `ObjectError::Upload` with the path (no retry here: the flush decides).
- `delete(&self, path) -> Result<(), ObjectError>`: idempotent (not found is `Ok`). Used by GC.
- `remove_local_temp_files(&self) -> Result<u64, ObjectError>`: local only. Walks `tables/` and deletes `.*.tmp-*` files, returning the count. Called at open.

### Hooks

Add `ObjectWrite` (before the temp write), `ObjectSync` (before `sync_all`), `ObjectRename` (before `rename`) and `ObjectDirSync`. Also add a test-only **recorder** in `Hooks` that logs the sequence of points passed. The durability-order test (M2.9) asserts on it.

### Fault store (`test_support.rs`, `#[cfg(test)]`)

`FaultStore` implements `object_store::ObjectStore` by wrapping another store (normally `InMemory`). It can be armed to, for its next matching call:
- fail a `put`/multipart before writing;
- write and then **return an error anyway** (the ambiguous case);
- delay.

It is a second real implementation of an external trait, used to exercise the S3 branch in CI (D102).

### Tests

- Local `put_durable` creates the file with the exact bytes and leaves no temp file. With a fault at each local hook point, the final file doesn't exist; the temp file may exist and `remove_local_temp_files` removes it.
- Remote path through `FaultStore(InMemory)`: small and multipart puts round-trip; armed failures surface as `Upload`.
- `table_file_path` produces the spec 03 layout and unique names.
- A `RuntimeEnv` with the store registered can read a file written by `put_durable`, for both kinds.

**Commit:** `storage: add object store access with durable local writes`

---

## M2.4 — Parquet writer

**Goal:** turn a partition's batches into one Parquet file with spec 03 §3's settings, and collect what the catalog row needs.

**Edit:** `src/parquet.rs`, `src/error.rs`, `src/lib.rs`.

### API

```rust
pub(crate) struct WrittenFile { pub bytes: Bytes, pub rows: u64, pub footer_size: u64,
    pub min_time: i64, pub max_time: i64, pub min_ingest_time: i64, pub max_ingest_time: i64 }

/// Sorts, encodes and returns the file. CPU-bound: call from spawn_blocking.
pub(crate) fn write_file(table: Table, level: u8, batches: &[Arc<RecordBatch>]) -> Result<WrittenFile, ParquetWriteError>;
```

### Settings (spec 03 §3)

- **Sort:**
  - `spans` by `trace_id, start_time`;
  - `scores` by `created_at`.

  Concatenate the batches, `lexsort_to_indices`, `take`. Declare the order with `WriterProperties::set_sorting_columns`, and only because the sort was done.
- **Compression** zstd level 3. **Data pages** 1 MiB, max 20,000 rows per page. **Page index** on. **Statistics** page level for all scalar columns.
- **Row groups:** call `flush()` on the `ArrowWriter` when its in-progress size reaches 64 MiB or 100,000 rows. Write in slices so the check happens between slices.
- **Dictionary:** on for the columns spec 01 marks `(dict)`, off for every other column. Write the per-table list as a constant next to the writer properties, with a test that compares it with the names in `llmobs_core::schema`.
- **Bloom filters** (fpp 0.01):
  - `spans`: `trace_id`, `session_id`, `user_id`;
  - `scores`: `trace_id`, `score_id`.

  (`traces` comes in M3.)
- **KV metadata:** `llmobs.table`, `llmobs.schema_version` (use `"1"` and say in a comment that it follows the stored schema version), `llmobs.level`, `llmobs.min_time`, `llmobs.max_time`, `llmobs.min_ingest_time`, `llmobs.max_ingest_time`.
- **`footer_size`:** the Parquet footer length plus 8 (the last 8 bytes of the file hold it).

### Tests

Read back with `parquet`'s reader:
- the rows equal the sorted input for both tables, every column type included;
- the sort is correct and declared;
- compression is zstd on every column chunk;
- bloom filters exist exactly on the listed columns, and a present `trace_id` tests positive;
- the page index is present;
- dictionary encoding matches the list;
- 250,000 small rows give 3 row groups;
- the KV metadata values;
- `footer_size` matches the file's last 8 bytes;
- min/max fields equal a recomputation.

**Commit:** `storage: add Parquet writer`

---

## M2.5 — Frozen memtable and flush triggers

**Goal:** the memtable side of spec 03 §2: an active and a frozen memtable, the data a flush needs, and the triggers.

**Edit:** `src/memtable.rs`, `src/wal/writer.rs`, `src/read_state.rs`, `src/lib.rs`, `src/hooks.rs`.

### Changes

- **`Memtable`** keeps `active` and `frozen: Option<Frozen>`:
  - `Frozen { partitions, bytes, max_seq }`.
  - `insert` now takes the record's `seq` and maintains `active_max_seq` and `active_oldest: Option<Instant>` (when the first batch of the active memtable was inserted).
  - `freeze(&mut self) -> Option<FrozenView>`: if there is no frozen memtable and `active` isn't empty, it moves `active` into `frozen` and returns a view (`Arc`ed partitions plus `max_seq`). If a frozen memtable already exists (a flush is retrying), it returns the existing frozen view and doesn't freeze again.
  - `drop_frozen(&mut self)`.
- **`snapshot`** includes both active and frozen batches. The providers can't tell them apart, and don't need to.
- **`bytes()`** is active + frozen. That is what backpressure compares with `hard_limit` (spec 03 §2).
- **Writer:**
  - passes each request's seq to `insert`;
  - after each group, if `active` bytes ≥ `flush.max_bytes`, notifies the table's flush task through a `tokio::sync::Notify`.
- **Age trigger:** the flush task (M2.6) wakes at the earliest of the notification and `active_oldest + flush.max_age`.
- `HookPoint::FlushAfterFreeze`.

### Tests

- Freeze moves everything, and snapshots taken before and after a freeze see the same rows.
- Inserts after a freeze go to `active`.
- `freeze` during a retry returns the same frozen view.
- `drop_frozen` removes only the frozen rows.
- `bytes()` counts both.
- `max_seq` and `oldest` are right.
- The writer notifies at the size threshold.

**Commit:** `storage: add frozen memtable and flush triggers`

---

## M2.6 — Flush, checkpoint and WAL truncation

**Goal:** spec 03 §2's six flush steps, wired into `Storage`: replay from `flushed_seq`, shutdown flush, WAL truncation.

**Edit:** `src/flush.rs`, `src/lib.rs`, `src/wal/{mod.rs,segment.rs}`, `src/hooks.rs`, `src/error.rs`, crate docs.

### The flush task (one per table)

The loop: wait for a trigger (size notification, age deadline, shutdown request). Then:

1. **Freeze**: `freeze()` under the memtable mutex. Nothing to flush → back to waiting.
2. **Prepare**, in `spawn_blocking`, per partition of the frozen view:
   - for `spans`, drop duplicate `(trace_id, span_id)` rows, keeping the greatest `ingest_time` (spec 03 §2; the tie-break on equal `ingest_time` is the later seq, and the last batch wins);
   - `parquet::write_file(table, level 0, batches)`.

   Tombstones and retention watermarks come with M5; trace partials with M3. Leave no code for them.
3. **Stage**: `committer.stage(paths)` for every output path. `HookPoint::FlushAfterStage`.
4. **Write**: `objects.put_durable` for every file, at most `flush.upload_concurrency` at a time. `HookPoint::FlushAfterWrite` once all are written.
5. **Commit**:

   ```rust
   CatalogEdit { add: <one FileMeta per file>, remove: [], flushed_seq: Some((table, frozen.max_seq)) }
   ```

   The publish closure drops `frozen` and publishes the new catalog with the memtable snapshot, as § M2.2 describes. `HookPoint::FlushAfterCommit`.
6. **Truncate**: delete the table's WAL segments that are entirely ≤ `flushed_seq` (below). `HookPoint::FlushAfterTruncate`.

**Failures:**
- A failure in steps 2–5 keeps `frozen` and logs table, partition count and error kind. The task retries after a backoff (1 s, doubling, capped at 60 s; reset on success). The retry reuses the same frozen view, with new file names (new staged rows). Old staged rows and objects are left to GC.
- Appends keep going into `active` until active + frozen hits `hard_limit` (backpressure, spec 03 §10).
- A failure in step 6 is logged and retried at the next flush. The commit already happened, so the data is safe.

### WAL truncation (`wal/mod.rs`, `segment.rs`)

`fn truncate_segments(dir, flushed_seq) -> io::Result<TruncateOutcome>`:
- list the segments in order;
- a segment `k` can be deleted when a later segment exists and `first_seq(k+1) − 1 ≤ flushed_seq`;
- the newest segment is never deleted, because the writer is appending to it;
- fsync the directory after the deletions.

At low traffic the newest segment can hold flushed and unflushed records for a while. That is bounded by `wal.segment_bytes`, and replay skips the flushed ones.

### `Storage::open`

The order (spec 12 § Process lifecycle, spec 03 §1):
1. lock;
2. `catalog.db` (migrations, `load`);
3. `Objects::new`, and remove local temp files;
4. replay each table with `after_seq = flushed_seq` from the catalog;
5. the initial `ReadState` (catalog plus memtables);
6. the writers, the flush tasks and the GC task (M2.8);
7. return.

`next_seq` after replay is already `max(last valid seq, after_seq) + 1` (M1.3).

`query()` returns the shared `RuntimeEnv` built at open:
- the object store registered;
- a `CacheManager` with the file metadata cache limited to `query.metadata_cache_bytes` (DataFusion 55's built-in cache: `CacheManagerConfig::with_metadata_cache_limit`).

Its memory pool stays the default until M6. Check whether DataFusion's cache also covers bloom filters and page indexes, and record the answer in the report (design § Open questions). Build a custom `ParquetFileReaderFactory` **only** if something spec 03 §4 needs is missing and a test shows it. Otherwise, update spec 03 §4 to say the built-in cache is used.

### `Storage::shutdown`

1. stop accepting appends;
2. drain the writers (as in M1);
3. ask each flush task to flush and wait up to 30 s in total (spec 12);
4. stop the flush and GC tasks;
5. release the lock.

A flush that doesn't finish in time leaves its data in the WAL, which is safe; log it.

### Crate docs

Replace M1's "not usable in production" section with M2's limits: no `traces`, no compaction (L0 files accumulate), no deletion or retention, no memory pool.

### Tests

- append → wait for the age flush (a short `flush.max_age` in tests) → files in the catalog and on disk, `flushed_seq` advanced, segments truncated.
- Reopen: rows come from files, and replay skips flushed records (count records replayed).
- **Size trigger:** a small `flush.max_bytes` flushes without waiting for the age.
- **Retry:** with `FaultStore` failing the first two puts, the flush eventually succeeds. No duplicate rows, and every staged path is either committed or left for GC.
- **Ambiguous upload:** `FaultStore` writes and then errors. The retry uses a new name, the first object stays staged, and the rows appear exactly once.
- **Backpressure during an outage:** with uploads failing, appends continue until active + frozen reaches `hard_limit`, then answer `Backpressure`. Once uploads recover, appends are accepted again.
- **Shutdown flushes:** after `shutdown` + reopen, nothing is replayed (`records = 0`).
- Truncation never deletes the newest segment, and keeps segments with unflushed records.

**Commit:** `storage: flush memtables to Parquet and truncate the WAL`

---

## M2.7 — Providers over files and memtable

**Goal:** spec 03 §6 for `spans` and `scores`: one scan over the files and the memtable of one `ReadState`.

**Edit:** `src/query/{mod.rs,spans.rs,scores.rs}`, `src/lib.rs`. `src/views/` belongs to A2. If the `ProjectQuery` contract needs to change, change the minimum and say so in the report.

### `SpansProvider::scan`

1. `time_bounds(filters, "start_time")` (M1), and the days it covers. M3 widens them by one day for the anchor; not now.
2. **Files:** `catalog.files(Spans, project, days)` with `[min_time, max_time]` overlapping the bounds.
3. **File scan:** a `DataSourceExec` from a `FileScanConfig`:
   - built with `ParquetSource`;
   - the predicate is the conjunction of `filters` (DataFusion uses it for statistics, page-index and bloom-filter pruning);
   - `pushdown_filters` and `enable_page_index` on;
   - file groups balanced by size over `target_partitions`;
   - each `PartitionedFile` carries the catalog's size, so no HEAD request is needed;
   - the registered object store URL;
   - `projection` as given.

   Rows the predicate can't fully evaluate stay, since filters remain `Inexact`.
4. **Memtable:** M1's memory plan over the same days and bounds.
5. **Union:** `UnionExec` of both. Either side may be empty; return only the non-empty one when the other is empty.

Use the DataFusion 55 APIs as they are (`FileScanConfigBuilder`, `ParquetSource`). Record surprises in the report.

### `ScoresProvider::scan`

The live rows: per `score_id`, the version with the greatest `updated_at`, then `ingest_time`; rows with `deleted` removed (spec 03 §6). M1 computed this eagerly; M2 builds a plan.

1. An inner raw provider (not registered anywhere) whose scan is the same file + memtable union as `spans`, on `scores` files and `created_at`.
2. In `scan`, build a `LogicalPlan`:
   1. scan the raw provider (`provider_as_source`), with the filters on `created_at` passed down for pruning;
   2. a window `row_number() OVER (PARTITION BY score_id ORDER BY updated_at DESC, ingest_time DESC)`;
   3. filter `rn = 1 AND NOT deleted`;
   4. project the stored columns (then `projection`).

   Turn it into a physical plan with the `Session` that `scan` receives.
3. Pruning by `created_at` is safe because every version of a score shares it (M1's comment and test stay).
4. Day-closed files (`fully_merged`) skip the window in M4. Not now.

Delete M1's eager last-writer-wins code.

### Tests

- Rows split between files and memtable return exactly once, by count and by id set.
- **Across a flush:**
  - pause at `FlushAfterWrite`: a query sees the rows in the memtable;
  - release to after the commit: a new query sees them in files;
  - at every pause point the row set is the same, never doubled, never missing.
- `start_time` bounds prune files: assert the number of files scanned on a multi-day dataset via `EXPLAIN ANALYZE` metrics or a test accessor.
- A `trace_id` equality query on files returns the right rows. Record in the report whether the bloom filter pruned.
- **Two projects:** files of one project are never scanned for the other.
- **Scores:** versions spread across two files and the memtable resolve to the latest; a `deleted` version in a later file hides the score; projections of only some columns work.
- A2's tests (`views/`) still pass unchanged.

**Commit:** `storage: read Parquet files and memtable together`

---

## M2.8 — Staged-object GC

**Goal:** spec 03 §5 crash safety: outputs written but never committed are removed without listing the bucket.

**Edit:** `src/gc.rs`, `src/catalog/committer.rs`, `src/lib.rs`, `src/hooks.rs`.

- A GC task runs at open (after replay) and then every 10 min (a constant).
- It reads the `staged_objects` rows older than 1 h (a constant, spec 03 §5). For each row:
  1. if the path is also in `data_files`, which can't happen (the commit deletes the staged row in the same transaction), log an error and keep the object;
  2. otherwise delete the object (not found is fine), then delete the row.
- These row deletes don't go through the committer (they change no published state), but they use the committer's connection through a small method on `Committer`, so SQLite sees one writer.
- `HookPoint::GcBeforeDelete`.
- Log counts and bytes, never paths at info level (paths contain project ids, which is fine at debug).

### Tests

- A staged row with an existing object older than the threshold is collected (inject the clock: the age threshold is a parameter in tests).
- A younger one is kept.
- A staged row whose object never got written is collected.
- An object listed in `data_files` is never deleted.
- A GC run concurrent with a flush never deletes the flush's freshly staged objects.

**Commit:** `storage: collect staged objects`

---

## M2.9 — Crash and durability tests

**Goal:** prove spec 03's crash invariants across every flush step (design § Durability items 2–5).

**Edit:** `tests/crash.rs`, `src/crash_tests.rs` (new, `#[cfg(test)]`), `src/hooks.rs`, `tests/s3.rs`.

### Random kills (`tests/crash.rs`, extend M1's harness)

- The child config:
  - `flush.max_bytes = 2MiB`;
  - `flush.max_age = 500ms`;
  - `flush.upload_concurrency = 2`;
  - a local object store in the test directory.

  Flushes then happen several times per second while the child runs.
- New invariants checked by the parent after each kill:
  - every `data_files` row's object exists on disk with the catalog's size;
  - `flushed_seq` (read with a plain SQLite connection, or by opening `Storage` and calling a test-only accessor) never decreases across iterations;
  - acknowledged rows appear **exactly once** across files and memtable (the duplicate check now spans both);
  - after a final open plus one GC pass with a zero age threshold, no `staged_objects` row is older than the run, and no temp file remains under `objects/`.

### Deterministic crash points (`src/crash_tests.rs`)

Random kills rarely hit the narrow windows. Add `Hooks::abort_at(point)`, test-only, which calls `std::process::abort()` when the point is reached.

A unit test in `crash_tests.rs` re-executes the crate's own unit-test binary (`std::env::current_exe()` with `--exact crash_tests::child --ignored`), passing the point through an env var. The child:
1. opens a `Storage` in a given directory;
2. appends a known set of batches and waits for their acks;
3. arms `abort_at(point)`;
4. triggers a flush.

The parent then reopens and checks the invariants above. Run it for each point:
- `FlushAfterFreeze`, `FlushAfterStage`;
- `ObjectWrite`, `ObjectSync`, `ObjectRename`, `ObjectDirSync` (mid-write, local);
- `FlushAfterWrite`, `CatalogCommit`, `FlushAfterCommit`, `FlushAfterTruncate`.

Every point must give: all acknowledged rows present exactly once, `catalog.db` consistent, and no row lost or doubled.

### Durability order

Use the hooks recorder (M2.3) to assert that, for each flushed file, the recorded sequence is:

> `ObjectWrite` → `ObjectSync` → `ObjectRename` → `ObjectDirSync` → (all files) → `CatalogCommit` → WAL segment deletion

and never a deletion before the commit.

### MinIO (`tests/s3.rs`, opt-in, D102)

Runs only when `LLMOBS_TEST_S3_ENDPOINT` is set; otherwise it returns at once and prints that it was skipped.
- Use `LLMOBS_TEST_S3_BUCKET`, a unique prefix per run, and `AWS_*` credentials.
- Steps:
  1. append, flush, query;
  2. reopen, query again;
  3. multipart (a file > 64 MiB: use a large `flush.max_bytes` and enough rows);
  4. delete the prefix at the end.
- Document how to start MinIO for it in a comment at the top of the file (one `docker run` line), and run it locally before opening the PR. Report that you did.

### Done when

- `just check` is green, and `just test` time grew by at most ~30 s over M1.
- `LLMOBS_CRASH_ITERATIONS=300` passes locally. Report the time.

**Commit:** `storage: add flush crash and durability tests`

---

## M2.10 — Specs, design and report

**Edit:** `docs/spec/03-storage.md`, `docs/plans/a1-storage/00-design.md`, crate docs.

- Update spec 03 wherever the code settled something the spec leaves open or got wrong. At least:
  - §4: the metadata cache (built-in or custom, M2.6);
  - §2: the retry and backoff of a failed flush;
  - §1: the WAL truncation rule (newest segment kept);
  - § Layout: the temp-file naming.

  Add a decision if anything is not a detail.
- `00-design.md` § Public API: reflect the `ProjectQuery` and config as they now are; tick M2's open questions (metadata cache, MinIO).
- No code changes in this sub-phase other than docs.

**Commit:** `docs: record what M2 settled`

---

## Report

At the end of M2, report:
- every public item of `llmobs-storage` that changed, against `00-design.md` § Public API;
- the exact versions of the new dependencies, and anything cargo-deny reported;
- the answers to the design's open questions (DataFusion's metadata cache and bloom filters; MinIO run);
- measured numbers, rough is fine:
  - flush time for 256 MiB of spans;
  - file size against memtable bytes;
  - appends per second during flushes;
- every deviation from this plan, and why;
- what the M3 plan should know (where trace partials hook into the flush, `file_sources`, what was awkward).

Then open **one PR** for the whole of M2.
