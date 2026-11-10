# A1 · M1 — Walking skeleton

Implementation plan for milestone M1 of the storage track, split into small sub-phases. Each sub-phase is done by one agent in one go, in order, and ends with `just check` green and one commit.

**Prerequisites:** the A1 design (`docs/plans/a1-storage/00-design.md`) and its spec edits are merged.

**Read first:**
- `AGENTS.md`, all of it.
- `docs/plans/a1-storage/00-design.md`: § Public API, § Crate layout, § Concurrency model, § Durability, § Testing.
- `docs/spec/03-storage.md`: § Layout, §1 Write-ahead log, §2 Memtable, §6 Query path (`SpansProvider`, `ScoresProvider`), §10.
- `docs/spec/01-data-model.md`: the `spans` and `scores` tables.
- `docs/spec/12-operations.md` § Configuration.
- `docs/mvp-decisions.md`: D59, D60, D64, D83, D87, D89, D90.
- `crates/core/src/{schema.rs,batch.rs,ids.rs}`: the Arrow schemas, `spans_to_record_batch` / `scores_to_record_batch`, `ProjectId`.

## What M1 is, and what it is not

M1 builds the write path up to the memtable, the read path over the memtable, and the test tools every later milestone reuses. When it is done:
- `Storage::open` locks the data directory and replays the WALs;
- `append_spans` / `append_scores` are durable and immediately queryable;
- `Storage::query` returns DataFusion providers over the memtable;
- a kill -9 harness proves no acknowledged row is lost.

**M1 is not usable in production, and the server must not wire it in yet.** There is no flush (M2), so:
- the WAL is never truncated and grows until restart;
- the memtable only grows; once it reaches `storage.memtable.hard_limit`, every append answers `Backpressure`;
- `shutdown` does not flush: data stays in the WAL and is replayed at the next open.

Each of these is stated in the crate docs (`lib.rs`) so nobody mistakes M1 for a working engine.

**Out of scope:** flush, Parquet, `catalog.db` (M2); traces and the trace anchor (M3); compaction (M4); deletion, retention, media (M5); the process-wide memory pool, disk-full handling and benchmarks (M6); user-facing views, the filter DSL and the SQL sandbox (A2).

## Decisions this plan relies on

| # | Summary |
|---|---|
| D59 | Each crate owns its config section; the server assembles the full config. |
| D60 | Storage consumes Arrow `RecordBatch`es in the `llmobs-core` schemas. |
| D83 | Storage owns `catalog.db` (not used in M1: `flushed_seq` is 0 until M2). |
| D87 | Durability and commit protocol. M1 covers the WAL half: fsync before ack, final WAL errors, no seq reuse. |
| D89 | Providers expose the stored schemas and are not registered by name; A2 builds the views. |
| D90 | `[storage]` section with sub-tables; sizes and durations as strings with units. |

User answers behind this plan:
- the read state is an `Arc` behind a `std::sync::RwLock` (no `arc-swap`);
- the views are built by A2 over raw providers;
- the config is nested, with human-readable units;
- the crash harness runs briefly in every `just test`, with a longer soak on demand.

## Versions

Add each to the root `[workspace.dependencies]`, then use `workspace = true` in `crates/storage/Cargo.toml`. These are the latest versions found when the plan was written. Check them again with `cargo info <crate>`, take the latest release that is at least 7 days old, and report the exact versions.

| Crate | Version | Where | Why |
|---|---|---|---|
| `crc32c` | 0.6.9 | storage | WAL record checksum |
| `bytesize` | 2.7.0, `features = ["serde"]` | storage | `"256MiB"` config values (D90) |
| `humantime-serde` | 1.1.1 | storage | `"120s"` config values (D90) |
| `async-trait` | 0.1.92 | storage | DataFusion 55's `TableProvider` is an `#[async_trait]` trait |
| `tempfile` | 3.27.0 | storage (dev) | test data directories |
| `proptest` | 1.12.0 | storage (dev) | codec and time-bound property tests |

Already in the workspace:
- `arrow`: enable the `ipc_compression` feature in `crates/storage/Cargo.toml` (it brings LZ4 and zstd for IPC);
- `datafusion`, `tokio`, `serde`, `thiserror`, `tracing`, `chrono`, `uuid`.

Do not add `arc-swap`, `parking_lot`, `crossbeam`, `fs2`/`fs4` or `anyhow`. Use `File::try_lock` from std (stable since Rust 1.89; the toolchain is 1.99) for the data-directory lock.

## Conventions for every sub-phase

- **Blocking I/O never runs on async worker threads.** File writes, fsyncs, directory listing and replay run in `tokio::task::spawn_blocking`. Tests use `#[tokio::test(flavor = "multi_thread")]`.
- **Errors:**
  - one `thiserror` enum per operation, in `src/error.rs`;
  - messages carry table names, segment file names, byte offsets, seq numbers and sizes, never row content or attribute values (AGENTS.md § Privacy);
  - I/O errors keep their source and say what was being done ("syncing WAL segment 00000000000000000042.wal").
- **No panics on disk input.** WAL bytes are untrusted: every length, seq, kind and IPC message is checked. Arithmetic on values read from disk uses checked operations.
- **`unwrap`/`expect`** only in tests, or with an invariant comment.
- **Logging** with `tracing`:
  - `info` for open, replay summaries and segment rotation;
  - `warn` for a truncated torn tail;
  - `error` for WAL failures.

  Fields are IDs, counts and sizes only.
- **Public items** get doc comments; `lib.rs` gets `#![warn(missing_docs)]` like `llmobs-core`.
- **Tests** live next to the code (`#[cfg(test)] mod tests`) except the crash harness (`crates/storage/tests/crash.rs`). Every test uses its own `tempfile::TempDir`.

## Final layout after M1

```
crates/storage/
  Cargo.toml
  src/
    lib.rs          Storage, AppendReceipt, StorageHealth, ProjectQuery; crate docs incl. the M1 limits
    config.rs       StorageConfig, WalConfig, WalSync, MemtableConfig, QueryConfig
    error.rs        OpenError, AppendError, QueryError, WalError
    hooks.rs        test hooks: pause points and injected faults (no-ops outside tests)
    wal/
      mod.rs        Table (Spans | Scores), shared helpers (sync_dir, segment names)
      record.rs     record encode/decode
      segment.rs    segment writer (append, sync, rotate)
      replay.rs     segment reader and replay
      writer.rs     the WAL writer task (group commit)
    memtable.rs     partitions, accounting, coalescing, snapshots
    read_state.rs   ReadState and its publisher
    query/
      mod.rs        ProjectQuery construction, time-bound extraction
      spans.rs      SpansProvider
      scores.rs     ScoresProvider
  tests/
    crash.rs        kill -9 harness
```

Merge or split modules if the code shows a better cut, and say so in the report. No other public items than those listed in each sub-phase.

---

## M1.1 — Dependencies, errors and configuration

**Goal:** the crate compiles with its dependencies, its error types and a fully tested `StorageConfig` with the fields M1 uses.

**Edit:** root `Cargo.toml`, `Cargo.lock`, `crates/storage/Cargo.toml`, `crates/storage/src/{lib.rs,config.rs,error.rs}`.

### Steps

1. Add the dependencies of § Versions. Run `just deny`.
2. `config.rs`: `StorageConfig`, the `[storage]` section. `serde::Deserialize` with `#[serde(deny_unknown_fields, default)]` on every struct; `Default` gives the spec defaults. Only the fields M1 uses; later milestones add theirs.

   | Field | Type | Default | Notes |
   |---|---|---|---|
   | `data_dir` | `PathBuf` | none, required | Set by the server from the top-level `data_dir` (12); still a field so storage is usable alone. |
   | `wal.sync` | `WalSync` | `Always` | `"always"`, or a duration string > 0 (`"5ms"`) → `Interval(Duration)`. Any other string is an error naming the field. |
   | `wal.segment_bytes` | `ByteSize` | `64MiB` | Must be ≥ 1 MiB. |
   | `wal.group_max_bytes` | `ByteSize` | `8MiB` | Must be ≥ 64 KiB. |
   | `wal.group_max_wait` | `Duration` (humantime) | `2ms` | Must be ≤ 1 s. |
   | `wal.skip_corrupt` | `bool` | `false` | Spec 03 §1 `--wal-skip-corrupt`; the CLI flag sets it. |
   | `memtable.hard_limit` | `ByteSize` | `1GiB` | Must be ≥ 16 MiB. |
   | `query.target_partitions` | `usize` | `std::thread::available_parallelism()` | Must be ≥ 1. |

   - `StorageConfig::validate(&self) -> Result<(), ConfigError>` checks the bounds above. `ConfigError` names the field and the rule ("storage.wal.segment_bytes: must be at least 1MiB"), never the raw input.
   - `data_dir` is required, so `StorageConfig` has no `Default`; the sub-sections do.
3. `error.rs`:
   - `ConfigError`;
   - `OpenError`: `Config(ConfigError)`, `Locked { path }`, `Io { action: &'static str, path, source }`, `WalCorrupt { table, segment, offset }`, `WalReplay(WalError)`;
   - `AppendError`: `InvalidBatch { reason: &'static str }`, `Backpressure`, `WalFailed`, `ShuttingDown`;
   - `QueryError` (wraps `DataFusionError`);
   - `WalError` (internal to the WAL modules, mapped into the others).

   Add variants as the later sub-phases need them, not before.
4. `lib.rs`: crate docs (what the crate is, spec 03 pointer, the "M1 is not usable in production" paragraph from this plan), `#![warn(missing_docs)]`, module declarations for `config` and `error` only.

### Tests

- `"256MiB"`, `"1GiB"` and `"120s"` parse.
- `"always"` and `"5ms"` parse for `wal.sync`; `"0ms"`, `"never"` and `""` fail with the field name.
- An unknown key at each level fails (`deny_unknown_fields`).
- Each bound of the table fails `validate` with its message.
- An empty `[storage]` with only `data_dir` gives every default.
- `ConfigError` messages don't echo the rejected value (test with a long random string).

### Done when

`just check` is green, and `cargo tree -p llmobs-storage` shows the new crates at the versions reported.

**Commit:** `storage: add dependencies, errors and configuration`

---

## M1.2 — WAL record codec

**Goal:** encode one record and decode a buffer of records exactly as spec 03 §1 describes, rejecting every malformed input without panicking.

**Edit:** `crates/storage/src/wal/{mod.rs,record.rs}`, `lib.rs` (module declaration).

### Format (spec 03 §1)

```
u32  payload_len      little-endian
u32  crc32c(seq || kind || payload)   seq as 8 little-endian bytes, kind as 1 byte
u64  seq              little-endian
u8   kind             1 = row batch; any other value is invalid
[..] payload          Arrow IPC stream: schema message + exactly one record batch,
                      written with IpcWriteOptions LZ4_FRAME compression
```

All integers are little-endian; spec 03 §1 doesn't say, so add "little-endian" to it in this commit.

### Steps

1. `wal/mod.rs`: `pub(crate) enum Table { Spans, Scores }` with `name()` (`"spans"`, `"scores"`) and `schema()` (`llmobs_core::schema::{spans_schema, scores_schema}`).
2. `record.rs`:
   - `pub(crate) const HEADER_LEN: usize = 17;`
   - `pub(crate) const MAX_PAYLOAD: u32 = 256 MiB;` A record bigger than that is rejected on write and treated as invalid on read. That stops a corrupt length from causing a 4 GiB allocation.
   - `fn encode(seq: u64, batch: &RecordBatch, out: &mut Vec<u8>) -> Result<(), WalError>` appends one record to `out`. It writes the IPC stream into a reused buffer, computes the CRC over seq, kind and payload, then writes the header and payload.
   - `fn decode_next(buf: &[u8], offset: usize) -> Decoded`, where:

     ```rust
     enum Decoded {
         Record { seq: u64, payload: Range<usize>, next: usize },
         End,                       // offset == buf.len()
         Short { offset: usize },   // header or payload extends past the buffer
         Invalid { offset: usize, reason: &'static str },  // bad CRC, bad kind, oversized length
     }
     ```

     It only frames and checks the CRC; it does not parse the IPC.
   - `fn read_batch(payload: &[u8], table: Table) -> Result<RecordBatch, WalError>` uses an Arrow `StreamReader`. It requires exactly one batch, and a schema whose fields equal `table.schema()`'s fields (compare fields, not metadata). Anything else is `WalError::InvalidPayload { reason }`.

### Tests

- A round-trip of a spans batch and a scores batch built with `llmobs_core::batch` from a few `Span`/`Score` values covering nulls, nested lists and maps.
- Several records in one buffer decode in order, and `End` comes after the last one.
- Every truncation point of a two-record buffer (each byte length from 0 to full) gives `Record`s for the complete records, then `Short` (or `End` exactly at a boundary).
- Flipping any single bit in a record gives `Invalid` (CRC), except bits of `payload_len`, which may give `Short` or `Invalid`. Never a panic, never a wrong `Record`.
- `kind = 2` gives `Invalid`, and `payload_len > MAX_PAYLOAD` gives `Invalid` without allocating.
- A payload with two batches, with zero batches, or with the wrong schema fails `read_batch`.
- A proptest: random byte buffers never panic `decode_next`/`read_batch`.

**Commit:** `storage: add WAL record codec`

---

## M1.3 — WAL segments and replay

**Goal:** write records to segment files with rotation and directory fsync, and replay a WAL directory with the torn-tail and corruption rules of spec 03 §1.

**Edit:** `crates/storage/src/wal/{mod.rs,segment.rs,replay.rs}`, `crates/storage/src/hooks.rs`, `docs/spec/03-storage.md` (segment name example).

### Segment files

- Name: the seq of the segment's first record, zero-padded to 20 digits, plus `.wal` (`00000000000000000042.wal`). Lexical order equals numeric order. Update spec 03's layout example (12 digits today) to 20 digits.
- `fn segment_name(first_seq: u64) -> String`, and `fn parse_segment_name(&str) -> Option<u64>`. Files in `wal/<table>/` that don't parse are ignored with a `warn`, never deleted.
- `fn sync_dir(path: &Path) -> io::Result<()>` opens the directory and calls `sync_all`.

### `segment.rs`: `SegmentWriter`

```rust
pub(crate) struct SegmentWriter { dir: PathBuf, file: File, name: String, len: u64, segment_bytes: u64 }
```

- `create(dir, first_seq, segment_bytes)` opens with `create_new(true)` and `append(true)`, then calls `sync_dir(dir)`.
- `write(&mut self, bytes: &[u8]) -> io::Result<()>` is `write_all` plus `len` bookkeeping.
- `sync(&mut self) -> io::Result<()>` is `sync_data`.
- `needs_rotation(&self) -> bool` is `len >= segment_bytes`. The writer task (M1.5) rotates *between* groups, never inside one: after a group's write and sync, if `needs_rotation`, it creates the next segment, named after the next seq. A group may therefore make a segment exceed `segment_bytes`. That is fine and documented.
- Both I/O calls go through the test hooks (below) so tests can inject errors.

### `replay.rs`

```rust
pub(crate) struct ReplayOutcome { pub next_seq: u64, pub records: u64, pub truncated_bytes: u64, pub skipped_bytes: u64 }

pub(crate) fn replay(
    dir: &Path, table: Table, after_seq: u64, skip_corrupt: bool,
    on_batch: &mut dyn FnMut(u64, RecordBatch) -> Result<(), WalError>,
) -> Result<ReplayOutcome, WalError>
```

1. List the segments, sort by first seq, and read each one fully into memory. Segments are at most `segment_bytes` plus one group, so this is bounded. Do not memory-map: that would need `unsafe`, which is forbidden.
2. Decode records in order:
   - Enforce strictly increasing seqs across all segments. A seq ≤ the previous one is corruption.
   - Skip records with seq ≤ `after_seq` without calling `read_batch`. In M1 `after_seq` is 0; M2 passes `flushed_seq`.
   - Call `on_batch(seq, batch)` for the others.
3. On `Short` or `Invalid`:
   - **In the last segment:** a torn tail. Truncate the file at that offset (`set_len`, then `sync_all`), add the dropped length to `truncated_bytes`, `warn` with segment name, offset and bytes dropped, and stop.
   - **In any earlier segment:** corruption. Return `WalError::Corrupt { table, segment, offset }`, unless `skip_corrupt`. In that case, `error`-log it, add the rest of that segment to `skipped_bytes`, and continue with the next segment.
4. `read_batch` failing on a record whose CRC is valid is corruption, wherever it is: the CRC matched, so the bytes are what was written. Same `skip_corrupt` handling (skip that record).
5. `next_seq = max(last valid seq, after_seq) + 1` (spec 03 §1). For an empty directory, `after_seq + 1`.

### `hooks.rs`: test seam

Under `#[cfg(test)]`, a `Hooks` value is shared by a `Storage` instance's tasks:
- `pause(point)` blocks until the test releases that point;
- `fail(point) -> Option<io::Error>` returns an error the test armed.

Points are an enum: `WalWrite`, `WalSync`, `AfterWalSync`, `BeforePublish`; later milestones add theirs. Outside `cfg(test)` the type is a zero-sized struct whose methods are `#[inline]` no-ops, so production code calls them unconditionally with no cost. Each `Storage` has its own `Hooks`, so parallel tests don't interfere. No global state.

### Tests

- Write records across several segments with a tiny `segment_bytes` and replay them all, in order, with the right `next_seq`.
- `after_seq` skips exactly the records at or below it.
- **Torn tail:** cut the last segment at every byte offset of its last record. Replay truncates to the last full record, `truncated_bytes` matches, and a second replay finds nothing to truncate.
- **Corruption in a middle segment:** a bit flip returns `Corrupt` with the right segment and offset. With `skip_corrupt`, replay continues and reports `skipped_bytes`.
- Non-increasing seqs give `Corrupt`.
- Unknown files in the directory are ignored.
- `segment_name` / `parse_segment_name` round-trip; names that are too short, not numeric or have the wrong extension don't parse.
- An injected `WalWrite` / `WalSync` error surfaces from `write` / `sync`.

**Commit:** `storage: add WAL segments and replay`

---

## M1.4 — Memtable

**Goal:** per-table in-memory storage, partitioned by `(project, day)`, with exact memory accounting, coalescing and cheap snapshots.

**Edit:** `crates/storage/src/memtable.rs`, `lib.rs`.

### Types

```rust
pub(crate) struct PartitionKey { pub project: ProjectId, pub day: NaiveDate }  // Ord: project, then day

pub(crate) struct MemBatch {
    pub batch: Arc<RecordBatch>,
    pub min_time: i64, pub max_time: i64,   // the table's time column: spans start_time, scores created_at
    pub bytes: usize,                       // batch.get_array_memory_size()
}

pub(crate) struct Memtable { table: Table, partitions: BTreeMap<PartitionKey, Vec<MemBatch>>, bytes: usize }

pub(crate) struct MemSnapshot { pub partitions: BTreeMap<PartitionKey, Vec<MemBatch>> }  // clones of the Arcs
```

### Behaviour

- `insert(&mut self, project: ProjectId, batch: RecordBatch)`:
  - Compute each row's UTC day from the time column: `div_euclid(86_400_000_000_000)` days since the epoch, then `NaiveDate`. A value outside chrono's range is impossible for valid input, but returns an error instead of panicking.
  - If every row has the same day (the common case), store the batch as is. Otherwise split it with one boolean mask per distinct day (`arrow::compute::filter_record_batch`).
  - In M1, spans use their own `start_time` day. M3 replaces this with the trace anchor (spec 03 § Trace anchor). Leave a doc comment saying so, but no code for it.
- **Coalescing** (spec 03 §2): after an insert, if the partition holds more than 64 batches smaller than 1 MiB, concatenate those small batches, in order, into one (`arrow::compute::concat_batches`). The large batches stay as they are. Snapshots already taken keep their old `Arc`s, so this never changes what a running query sees.
- **Accounting:** `bytes` is the sum of `get_array_memory_size()` over stored batches, updated on insert and on coalescing. `bytes()` returns it.
- `snapshot(&self) -> Arc<MemSnapshot>` clones the map of `Arc`s. Its cost is proportional to the number of stored batches, which coalescing keeps small. Add a test that bounds it: 10 000 one-row inserts into one partition leave at most 64 + a few batches.

### Tests

- A batch spanning three days lands in three partitions, with correct `min_time`/`max_time` and row counts.
- Times before 1970 (negative) land in the right day.
- Coalescing: 100 small inserts give at most 64 small batches plus coalesced ones. Row order and total row count are preserved, and `bytes` equals a recomputation over the stored batches.
- A snapshot taken before coalescing still returns the original batches.

**Commit:** `storage: add memtable`

---

## M1.5 — WAL writer task

**Goal:** the single writer per table of spec 03 §1. Group commit, memtable insertion in seq order, read-state publication, acks, both sync modes, and final failure on I/O errors.

**Edit:** `crates/storage/src/wal/writer.rs`, `crates/storage/src/read_state.rs`, `crates/storage/src/hooks.rs`, `lib.rs`.

### `read_state.rs`

```rust
pub(crate) struct ReadState { pub spans: Arc<MemSnapshot>, pub scores: Arc<MemSnapshot> }

pub(crate) struct Published(std::sync::RwLock<Arc<ReadState>>);
```

- `load(&self) -> Arc<ReadState>`: read lock, clone the `Arc`, unlock.
- `replace(&self, f: impl FnOnce(&ReadState) -> ReadState)`: write lock, build the new state from the old, store it.
- A poisoned lock (a panic while holding it) is recovered with `into_inner()`. The state is replaced atomically, so it is never half-written. Comment this invariant.
- M2 adds the catalog to `ReadState`, M5 the tombstones.

### `writer.rs`

```rust
pub(crate) struct AppendRequest { pub project: ProjectId, pub batch: RecordBatch, pub bytes: usize, pub ack: oneshot::Sender<Result<AppendReceipt, AppendError>> }

pub(crate) struct WalWriterHandle { tx: mpsc::Sender<AppendRequest>, state: Arc<WriterShared>, task: JoinHandle<()> }

struct WriterShared {
    failed: AtomicBool,
    memtable_bytes: AtomicU64,   // mirror of the memtable's accounting, for the cheap backpressure check
    memtable: Mutex<Memtable>,   // only the writer inserts; M2's flush freezes it under this lock
}
```

- The channel is a bounded `tokio::sync::mpsc` of capacity 1024 (a constant with a doc comment). A full channel makes appenders wait, which is the intended backpressure for bursts.
- The task loop, per group:
  1. Await the first request. When the channel is closed and empty, finish (shutdown).
  2. Keep receiving with `tokio::time::timeout_at(first_arrival + group_max_wait, rx.recv())` until the group's `bytes` reach `group_max_bytes` or the deadline passes.
  3. Assign consecutive seqs from `next_seq`.
  4. In `spawn_blocking`, with the `SegmentWriter` moved in and returned:
     - encode every record into one buffer;
     - `write` it (the segment writer consults the `WalWrite` fault hook, M1.3);
     - with `WalSync::Always`, `sync` (`WalSync` fault hook);
     - rotate if needed.

     The closure always runs to completion: a spawned blocking task can't be aborted, which gives the "not cancellable" property of spec 03 §1.
  5. `hooks.pause(AfterWalSync)`.
  6. Insert every batch into the memtable in seq order (one lock), update `memtable_bytes`, then `hooks.pause(BeforePublish)`, then publish the table's new snapshot.
  7. Send `Ok(AppendReceipt { seq, rows })` to each ack.
- **Interval mode** (`WalSync::Interval(d)`): step 4 writes without syncing. A sync runs inside the next step-4 closure once `d` has elapsed since the last sync. When the writer is idle, a `tokio::time::sleep` arm of a `select!` triggers a standalone sync job. Shutdown syncs before finishing.
- **Failure:** any error from step 4 (or a failed standalone sync):
  - set `failed`;
  - complete every ack of the group with `WalFailed`;
  - `error`-log table, segment and seq range;
  - close the receiver and fail every queued request with `WalFailed`;
  - end the task.

  There is no retry. The segment may hold a partial group: it is a torn tail for the next replay, and those records were never acknowledged.
- An encoding error (a batch that IPC can't write) fails only that request with `InvalidBatch { reason: "unencodable batch" }` and leaves the writer healthy. Encode each request before step 4 so a bad one never reaches the file.

### Tests (unit tests with a memtable and hooks; no `Storage` yet)

- Concurrent appends from 16 tasks all succeed. Seqs are unique and consecutive, and the memtable holds every row in seq order.
- **Grouping:** with `AfterWalSync` paused, appends queue up; after release, one group carries several requests. Assert with a test-only counter of groups, exposed through hooks.
- **Group limits:** a group never exceeds `group_max_bytes` unless it holds a single request.
- **Visibility:** while paused at `AfterWalSync`, a read of `Published` doesn't contain the batch. After release and ack, it does, before the ack is received.
- **Write error:** the failing group's acks get `WalFailed`, queued requests too, and `failed` is set. A replay of the directory returns exactly the records acknowledged before the failure.
- A sync error behaves the same.
- **Interval mode:** acks arrive without a sync per group (count syncs through hooks), and a sync happens within about the interval while idle.
- **Rotation:** with a tiny `segment_bytes`, several segments are created, each starting with the right seq.
- **Shutdown:** dropping every sender drains the queue, acks everything, and ends the task.

**Commit:** `storage: add WAL writer with group commit`

---

## M1.6 — `Storage`: open, append, health, shutdown

**Goal:** the public entry points of `00-design.md` § Public API that M1 covers.

**Edit:** `crates/storage/src/lib.rs`, `error.rs`, `wal/mod.rs`.

### Public API added here

```rust
pub struct Storage { /* config, LOCK file, Published, one WalWriterHandle per table, hooks */ }

pub struct AppendReceipt { pub seq: u64, pub rows: usize }

pub struct StorageHealth { pub spans_wal_failed: bool, pub scores_wal_failed: bool, pub memtable_bytes: u64, pub memtable_hard_limit: u64 }

impl Storage {
    pub async fn open(config: StorageConfig) -> Result<Storage, OpenError>;
    pub async fn append_spans(&self, project: ProjectId, batch: RecordBatch) -> Result<AppendReceipt, AppendError>;
    pub async fn append_scores(&self, project: ProjectId, batch: RecordBatch) -> Result<AppendReceipt, AppendError>;
    pub fn health(&self) -> StorageHealth;
    pub async fn shutdown(self) -> Result<(), StorageError>;
}
```

`StorageError` (new in `error.rs`): a WAL failure during shutdown, or a writer task that panicked. `Storage` is `Send + Sync`; the server holds it in an `Arc`.

### `open`

1. `config.validate()`.
2. In `spawn_blocking`:
   - create `data_dir`, `data_dir/wal`, `wal/spans` and `wal/scores` if missing, fsyncing each parent directory after a creation;
   - open or create `data_dir/LOCK` and `try_lock()` it. `WouldBlock` gives `OpenError::Locked`. Keep the `File` in `Storage`: the lock lives as long as it does (spec 12).
3. For each table, `replay(dir, table, 0, skip_corrupt, insert into a fresh Memtable)`. `after_seq` is 0 until M2 reads `flushed_seq` from `catalog.db`; leave a comment. Replay may push the memtable past `hard_limit`. Acknowledged data is never dropped, so appends then answer `Backpressure` until restart (an M1 limit).
4. Create a fresh segment for each table at `next_seq` (spec 03 §1: writing never resumes inside a replayed segment), publish the initial `ReadState`, and spawn both writer tasks.
5. `info`-log per table: records replayed, bytes truncated, bytes skipped, next seq, memtable bytes.

Appends cannot run before `open` returns: there is no `Storage` value before that.

### `append_spans` / `append_scores`

1. Check `failed` → `WalFailed`, and shutdown in progress → `ShuttingDown`.
2. Validate the batch, returning `InvalidBatch` with a static reason:
   - at least one row;
   - fields equal to the table's schema fields;
   - every `project_id` value equal to `project.to_string()`.
3. `memtable_bytes + batch.get_array_memory_size() > hard_limit` → `Backpressure`.
4. Send the request and await the ack. A closed channel maps to `WalFailed` or `ShuttingDown` from the shared flags. A dropped ack sender (writer panicked) maps to `WalFailed`.

### `health`

Reads the flags and the byte counters. No I/O.

### `shutdown`

- Set the shutting-down flag, drop both senders, and await both tasks. The writers drain, ack and, in interval mode, sync.
- A writer that ended with a failure turns into `StorageError`.
- Release the lock by dropping the file.
- No flush in M1: state it in the doc comment, with the M2 pointer.

### Tests

- open → append → drop → reopen: every acknowledged row is back and seqs continue after the last one.
- A second `open` on the same directory while the first is alive fails with `Locked`, and succeeds after `shutdown`.
- Each `InvalidBatch` reason triggers; none of the messages contains row data (check with a batch whose strings are unique markers).
- Backpressure: a small `hard_limit` and appends until `Backpressure`. Nothing is written to the WAL for the rejected batch.
- After an injected WAL write error (through the storage's hooks):
  - every append gets `WalFailed`;
  - `health` shows it;
  - `shutdown` returns an error;
  - a reopen replays exactly the acknowledged rows.
- A torn tail written by hand (truncated last record) is dropped at open and logged.
- A corrupt middle segment fails open with `WalCorrupt`, and opens with `wal.skip_corrupt = true`.
- Appends from 32 concurrent tasks interleave with `health` calls without deadlock (a smoke test with a timeout).

**Commit:** `storage: add Storage open, append and shutdown`

---

## M1.7 — Query providers over the memtable

**Goal:** `Storage::query` returns a configured DataFusion context and the `spans` and `scores` providers, reading one `ReadState` (spec 03 §6, D89).

**Edit:** `crates/storage/src/query/{mod.rs,spans.rs,scores.rs}`, `lib.rs`, `error.rs`.

### Public API added here

```rust
pub struct ProjectQuery {
    pub ctx: SessionContext,
    pub spans: Arc<SpansProvider>,
    pub scores: Arc<ScoresProvider>,
}
pub struct SpansProvider { /* project, Arc<MemSnapshot> */ }
pub struct ScoresProvider { /* project, Arc<MemSnapshot> */ }

impl Storage {
    pub fn query(&self, project: ProjectId) -> Result<ProjectQuery, QueryError>;
}
```

- `query` loads one `ReadState` and builds both providers from it, so both see the same moment.
- The `SessionContext` is created with `target_partitions = config.query.target_partitions`. Nothing is registered in it (D89).
- The design's `QueryOptions` arrives with the memory pool in M6; don't add an empty struct now.

### Time bounds (`query/mod.rs`)

`fn time_bounds(filters: &[Expr], column: &str) -> TimeBounds { min: Option<i64>, max: Option<i64> }`, both inclusive, in ns. It extracts what it can from top-level conjuncts:
- `col op literal` and `literal op col` for `=, <, <=, >, >=`;
- `col BETWEEN a AND b`;
- `AND` (recursively).

Literals:
- `ScalarValue::TimestampNanosecond/Microsecond/Millisecond/Second` (any timezone, interpreted as UTC instants), converted to ns with checked arithmetic;
- `Utf8` RFC 3339 strings, if DataFusion leaves them uncast, parsed with `llmobs_core::time_serde::parse_rfc3339`.

Anything else (`OR`, functions on the column, casts of the column, `NOT`) contributes no bound. Strict comparisons adjust by 1 ns. Contradictory bounds give an empty scan, not an error.

### `SpansProvider`

`TableProvider` (`#[async_trait]`):
- `schema()`: `spans_schema()`. `table_type()`: `Base`.
- `supports_filters_pushdown`: `Inexact` for every filter. DataFusion keeps a `FilterExec` above the scan, and the provider uses filters only for pruning.
- `scan(state, projection, filters, limit)`:
  1. `time_bounds(filters, "start_time")`.
  2. Select the project's partitions whose day lies within the bounds. Days are exact in M1; M3 widens them by one day (§ Trace anchor). Then select the batches whose `[min_time, max_time]` overlaps the bounds.
  3. Distribute the selected batches over at most `target_partitions` partitions, balancing by row count.
  4. Return `MemorySourceConfig::try_new_exec(&partitions, schema, projection)`, then `.with_limit(limit)` if DataFusion allows a limit there (filters are inexact, so pass the limit only when `filters` is empty).

  `MemoryExec` was replaced by `MemorySourceConfig` + `DataSourceExec` in recent DataFusion; use what DataFusion 55 provides.

### `ScoresProvider`

Same shape, with `scores_schema()` and the time column `created_at`. Spec 03 §6 requires live scores: the latest version of each `score_id` by `updated_at`, without `deleted` rows.

In M1 the provider computes this eagerly in `scan`, over the selected memtable batches:
1. Concatenate the batches.
2. `lexsort_to_indices` by `score_id` ascending, `updated_at` descending, `ingest_time` descending.
3. Take the first row of each `score_id` run, then drop the rows with `deleted = true`.
4. Serve the result through `MemorySourceConfig`.

All versions of one score share `created_at` (only `updated_at` changes), so pruning by `created_at` never separates a score from its other versions. Add a comment, and a test that fails if that assumption is broken by test data. M2 replaces the eager step with a plan over files and memtable; say so in a doc comment.

### Tests

Register the providers in the returned `ctx` under test names and run SQL.
- Appended spans come back with `SELECT count(*)`, by `trace_id` equality (binary literal), and with projections of a few columns.
- **Time pruning:**
  - `start_time >= X` on a multi-day dataset scans only the right days (count batches through a test-only accessor on the provider, or inspect the physical plan's partitions);
  - `OR` and function-wrapped predicates still return correct results, just unpruned;
  - a proptest compares `time_bounds` + pruning with an unpruned scan: same rows for random ranges.
- **Two projects:** each `ProjectQuery` sees only its own rows.
- **Snapshot isolation:** a `ProjectQuery` taken before an append doesn't see the new rows; one taken after does.
- **Scores:** three versions of a score return the latest; a later `deleted = true` version hides it; a tie on `updated_at` is broken by `ingest_time`.
- `time_bounds` unit tests for every operator, strictness, each literal type and contradictory bounds.

**Commit:** `storage: add spans and scores providers over the memtable`

---

## M1.8 — Crash harness and race tests

**Goal:** the kill -9 harness and the pause-point race tests of `00-design.md` § Testing, run by `just test`.

**Edit:** `crates/storage/tests/crash.rs`, `crates/storage/src/lib.rs` (race tests), `AGENTS.md` (one line, see below), `docs/plans/a1-storage/00-design.md` (only if the harness differs from its § Testing).

### Crash harness (`tests/crash.rs`)

One integration test file with two tests.

**`crash_child`**, `#[ignore = "run by crash_recovery as a child process"]`:
1. Returns at once unless `LLMOBS_CRASH_CHILD_DIR` is set.
2. Opens a `Storage` in that directory with `wal.segment_bytes = 1MiB` (the minimum, to force rotations) and `memtable.hard_limit = 256MiB`.
3. Runs 4 tasks that each append, in a loop, deterministic span batches of 1–50 rows. Batch ids are unique per run (`run_id * 1_000_000 + n`, with `run_id` from `LLMOBS_CRASH_RUN`). The batch id goes into each span's `name` (`"b<batch_id>/<row>"`) and into its `span_id`.
4. After each acknowledged batch, appends `"<batch_id> <rows> <seq>\n"` to `acked.log` in the directory, and calls `sync_data` on it. `seq` comes from the `AppendReceipt`.
5. Never returns.

**`crash_recovery`**, a normal test:
- For `i` in `0..iterations` (env `LLMOBS_CRASH_ITERATIONS`, default 25):
  1. Spawn `std::env::current_exe()` with `--exact crash_child --ignored --nocapture --test-threads=1` and the env vars.
  2. Sleep a pseudo-random 50–400 ms. Use a small xorshift seeded from `LLMOBS_CRASH_SEED`, default fixed, and print the seed on failure.
  3. `kill()` the child (SIGKILL) and `wait()`.
  4. Reopen in the parent and check the invariants:
     - every batch listed in `acked.log` (ignoring a final partial line) is present with exactly its row count;
     - every batch in storage is either complete or absent (a WAL record is atomic);
     - no `span_id` appears twice;
     - seqs keep increasing across runs: every seq in a run's log lines is greater than every seq of the previous runs, and no seq appears twice.
  5. `shutdown` the parent's storage before the next iteration.
- All iterations share one directory, so the WAL accumulates many segments and torn tails across runs, as a real crash loop would.
- Budget: the default run takes about 15 s. If it takes more than 30 s in CI, lower the default and say so in the report.

Add one line to `AGENTS.md` § Tests: "`LLMOBS_CRASH_ITERATIONS=N just test` runs the storage crash harness longer (default 25)."

### Race tests (unit tests in `lib.rs`, using `Storage`'s hooks)

- **Query during an append:**
  1. Pause at `AfterWalSync`.
  2. Start an append. A `query` sees nothing.
  3. Release; the append returns.
  4. A new `query` sees the rows, and the `ProjectQuery` from step 2 still sees nothing.
- **No visibility before durability:** with `WalSync` armed to fail, a concurrent `query` never sees the failing batch, before or after the failure.
- **Shutdown during appends:** start 8 appending tasks, call `shutdown` mid-way. Every append either succeeded (and is present after reopen) or got `ShuttingDown`; none is lost or duplicated.

### Done when

- `just check` is green, and `just test` time grew by at most ~20 s.
- `LLMOBS_CRASH_ITERATIONS=500 cargo test -p llmobs-storage --test crash` passes locally. Report how long it took.

**Commit:** `storage: add crash harness and race tests`

---

## After M1

In the report of M1.8, also list:
- every public item of `llmobs-storage`, compared with `00-design.md` § Public API (only differences, with reasons);
- the exact versions of the new dependencies;
- measured numbers, if cheap to get: appends per second with 4 tasks and 50-row batches on the dev machine, in both sync modes. A rough number from the crash child is enough;
- anything the M2 plan should know (APIs that were awkward, DataFusion surprises).

The M2 plan is written after M1 is merged, from the code as it is.
