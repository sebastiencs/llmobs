# A1 · M6a — Query memory, disk-full, quarantine

First half of milestone M6, **Resources and hardening** (`00-design.md` § Milestones). It needs M5 merged. It adds:
- a process-wide query memory pool with a per-query cap, spilling, and admission (M6.1);
- disk-full handling: ingestion refused under `storage.min_free_bytes`, compaction jobs that reserve disk (M6.2);
- quarantine of corrupt Parquet files, found by maintenance reads (M6.3) and by failed queries (M6.4).

`12-m6b-read-cache-benchmarks.md` has the S3 read cache and the benchmarks.

Decisions: D-A1-8 (admission), D-A1-9 (disk), D-A1-10 (quarantine) in `docs/mvp-decisions.md` § Track A1. The spec is `docs/spec/03-storage.md` § Layout, §4, §6 Resource limits, §7, §8, §10, already updated by the plan PR. Read those sections before starting.

## Conventions for every sub-phase

- **`just check-rust` passes before each commit.** Node is not always installed; when it is, run `just check`. Commit subjects are given per sub-phase. One commit per sub-phase.
- **Scope.** Edit only `crates/storage/` and `docs/`, plus the exceptions each sub-phase names (the root `Cargo.toml`/`Cargo.lock` in M6.1 and M6.2, `StorageConfig` literals in `crates/server` in M6.2).
- **Dependencies.**
  - M6.1 adds `sysinfo` (total RAM). M6.2 adds `rustix` (free disk). Their exact lines are in those sub-phases. Add each once to the root `[workspace.dependencies]` and use `{ workspace = true }` in `crates/storage/Cargo.toml`. Run `cargo deny check` (part of `just check-rust`); if it fails because of either crate, stop and report.
  - Everything else is in the pinned `datafusion` 55.2, `parquet`/`arrow` 59.3, `object_store` 0.13.2. The APIs this plan names were checked in the registry sources (`~/.cargo/registry/src/index.crates.io-*/`). If a named item doesn't exist or has another signature, stop and report; don't substitute.
- **`StorageConfig` literals.** M6.1 changes `QueryConfig` (no literal change: every literal uses `QueryConfig::default()` or `Default::default()`; check with `grep -rn "QueryConfig {" crates/`). M6.2 adds a top-level field to `StorageConfig`: run `grep -rn "StorageConfig {" crates/` and add the field to every literal in the same commit, with the value M6.2 gives.
- **`FileMeta`, `ReadState` literals.** No sub-phase adds a field to either.
- **`ProjectQuery` literals.** M6.4 adds a field. Run `grep -rn "ProjectQuery {" crates/` and update every literal in the same commit: 13 in `crates/storage/src` (they use `Suspects::detached()`) and one in `crates/storage/tests/trace_bench.rs` (`no_rule`, an external crate that can't call `pub(crate)` items: it copies the field from the `ProjectQuery` it starts from, `suspects: query.suspects.clone()`).
- **Hook points.** A new `HookPoint` variant needs its arm in `HookPoint::index` and `COUNT` raised by one (`hooks.rs`). This plan names every new hook point; add no others.
- **Logs.** File ids, project ids, table names, days, byte counts and static reasons only. Never a row value, never an object path of a media blob, never a DataFusion error's text from a plan that holds the covered-row predicate (use `CompactionError::label()` as M5 does). A Parquet error's text may name an object path of a data file: that is allowed (data file paths hold ids and days only).
- **Pause points in tests.** Tests that hold a hook while another task runs use `#[tokio::test(flavor = "multi_thread", worker_threads = 4)]`.
- **Errors.** `thiserror` enums. No `unwrap`/`expect` outside tests unless a comment states the invariant. Never panic on bytes read from disk or the network.
- **Time.** `i64` ns since the epoch, UTC; "now" is `catalog::now_ns()` unless the function takes `now`.
- **What not to build in M6a.**
  - The S3 read cache and the benchmarks: M6b.
  - Server code (mapping `QueryError::Busy` to 503, `AppendError::DiskFull` to 503, `/readyz`): server track.
  - A periodic scrub of every file: not chosen (D-A1-10).
- **Report at the end of each sub-phase** (in your final message):
  - what was done;
  - any deviation from this plan, and why;
  - the exact version of any dependency added;
  - anything left undone or unverified;
  - files changed outside `crates/storage/` and `docs/`.

---

## M6.1 — Query memory pool and admission

Commit: `storage: share one query memory pool and admit queries (M6.1)`

### What it does (spec 03 §6 Resource limits, D-A1-8)

- One **shared pool** of `query.max_memory_total` bytes for every query of the process.
- `Storage::query` becomes `async`. Before it builds the providers it **admits** the query: it takes `query.initial_memory` bytes from the shared pool, waiting up to `query.admission_wait` for them. If they never fit, it returns `QueryError::Busy`.
- Each admitted query gets a **query pool** (a `MemoryPool`) in a `RuntimeEnv` of its own. Its first `initial_memory` bytes come from what admission took; beyond that it takes from the shared pool, up to `query.max_memory` in total. When it can't grow, `try_grow` fails with `DataFusionError::ResourcesExhausted`: sorts, repartitions and spillable aggregates spill to `<data_dir>/tmp/query/`; operators that can't spill fail the query, which A2's `classify` already maps to `RunError::TooLarge` (422).
- When the query's `RuntimeEnv` is dropped (every Arc to it: the session, `TaskContext`s of finished streams), the query pool gives everything it holds back to the shared pool and wakes queries waiting for admission.
- Compaction keeps its own runtime and its own `compaction.max_memory` pool (unchanged). The media GC's live-set query keeps using the base runtime (no admission, unbounded pool): it projects one list column and holds no operator state.

### Config (`config.rs`, `QueryConfig`)

Add four fields, with these serde attributes and docs. Update the `QueryConfig` doc comment to "`[storage.query]`." (drop "fields used before the memory pool lands (M6)").

```rust
/// Memory shared by all queries. Absent means half of the machine's memory
/// (its cgroup limit when lower), at most 8 GiB, resolved at open.
#[serde(default)]
pub max_memory_total: Option<ByteSize>,
/// Most memory one query may hold. Default 2 GiB.
#[serde(default = "default_query_max_memory")]
pub max_memory: ByteSize,
/// Memory a query takes from the shared pool before it starts. Default 64 MiB.
#[serde(default = "default_initial_memory")]
pub initial_memory: ByteSize,
/// How long a query waits for its initial memory before failing as busy. Default 5 s.
#[serde(default = "default_admission_wait", with = "humantime_serde")]
pub admission_wait: Duration,
```

`Default for QueryConfig` sets `max_memory_total: None` and the three defaults.

`StorageConfig::validate` adds, after the existing `query` checks, in this order:
- `max_memory < 1MiB` → `ConfigError::invalid("storage.query.max_memory", "must be at least 1MiB")`;
- `initial_memory < 1MiB` → `("storage.query.initial_memory", "must be at least 1MiB")`;
- `initial_memory > max_memory` → `("storage.query.initial_memory", "must be at most storage.query.max_memory")`;
- `max_memory_total` is `Some(total)` and `total < max_memory` → `("storage.query.max_memory_total", "must be at least storage.query.max_memory")`;
- `admission_wait > 60s` → `("storage.query.admission_wait", "must be at most 60s")`.

Tests in `config.rs`: one per rule (the error's key), plus a TOML round trip that sets all four fields (`max_memory_total = "1GiB"`, `max_memory = "256MiB"`, `initial_memory = "8MiB"`, `admission_wait = "2s"`).

### Resolving `max_memory_total` (`src/memory.rs`, new, `pub(crate) mod memory;`)

Dependency, root `Cargo.toml` `[workspace.dependencies]`, with a comment line `# Total memory for the query pool default (spec 03 §6).`:

```toml
sysinfo = { version = "0.39.6", default-features = false, features = ["system"] }
```

`crates/storage/Cargo.toml`: `sysinfo = { workspace = true }` (keep the list alphabetical).

```rust
/// `query.max_memory_total` when the config leaves it out: half of the
/// machine's memory, or of its cgroup limit when that is lower, at most 8 GiB.
/// Zero when the OS reports nothing; `Storage::open` then uses `max_memory`.
pub(crate) fn default_total_memory() -> u64 {
    use sysinfo::{MemoryRefreshKind, RefreshKind, System};
    let system = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let mut total = system.total_memory();
    if let Some(limits) = system.cgroup_limits()
        && limits.total_memory > 0
    {
        total = total.min(limits.total_memory);
    }
    (total / 2).min(8 << 30)
}
```

`total_memory()` and `CGroupLimits::total_memory` are bytes in sysinfo 0.39.6. `cgroup_limits()` is `None` outside Linux.

In `Storage::open_inner`, the resolved values are:
- `total = config.query.max_memory_total.map(|v| v.0).unwrap_or_else(memory::default_total_memory)`;
- if `total < config.query.max_memory.0` (only possible when defaulted, on a small machine): `cap = total.max(config.query.initial_memory.0)` and `total = cap`, and log once `tracing::info!(total, cap, "query memory reduced to the machine's memory")`; otherwise `cap = config.query.max_memory.0`.
- `initial = config.query.initial_memory.0`.

Convert with `usize::try_from(..).unwrap_or(usize::MAX)`.

### The shared pool and the query pool (`src/memory.rs`)

```rust
/// Memory shared by every query (spec 03 §6). Counts bytes; it holds none.
#[derive(Debug)]
pub(crate) struct SharedPool {
    limit: usize,
    used: AtomicUsize,
    /// Notified whenever bytes are given back.
    freed: tokio::sync::Notify,
}

impl SharedPool {
    pub(crate) fn new(limit: usize) -> Self;
    /// Takes `bytes` if `used + bytes <= limit`. A compare-exchange loop on `used`.
    fn try_take(&self, bytes: usize) -> bool;
    /// Takes `bytes` even past the limit (`MemoryPool::grow` cannot fail).
    fn force_take(&self, bytes: usize);
    /// Gives `bytes` back (saturating at 0) and calls `freed.notify_waiters()`.
    fn give_back(&self, bytes: usize);
    /// Bytes taken now.
    pub(crate) fn used(&self) -> usize;

    /// Takes `initial` bytes for a new query, waiting until `deadline` for
    /// them. `None` when they never fit.
    pub(crate) async fn admit(
        self: &Arc<Self>,
        initial: usize,
        cap: usize,
        deadline: tokio::time::Instant,
    ) -> Option<Arc<QueryPool>>;
}
```

`admit` loop, exactly (no lost wake-up):
1. `let notified = self.freed.notified(); tokio::pin!(notified); notified.as_mut().enable();`
2. if `self.try_take(initial)`: return `Some(Arc::new(QueryPool::new(Arc::clone(self), initial, cap)))`;
3. `if tokio::time::timeout_at(deadline, notified).await.is_err() { return None; }`
4. go back to 1.

```rust
/// One query's pool: a `MemoryPool` over the shared pool, capped at `cap`.
#[derive(Debug)]
pub(crate) struct QueryPool {
    shared: Arc<SharedPool>,
    /// Taken at admission; never given back before the pool is dropped.
    initial: usize,
    cap: usize,
    state: Mutex<PoolState>, // std::sync::Mutex
}

#[derive(Debug, Default)]
struct PoolState {
    /// Bytes the query's reservations hold.
    used: usize,
    /// Bytes taken from the shared pool: `max(initial, used)` at rest.
    held: usize,
}
```

`QueryPool::new(shared, initial, cap)` sets `held = initial`, `used = 0`.

`impl datafusion::execution::memory_pool::MemoryPool for QueryPool`:
- `fn name(&self) -> &str { "llmobs_query" }`
- `fn grow(&self, _: &MemoryReservation, additional: usize)`: lock; `used += additional`; if `used > held` { `shared.force_take(used - held)`; `held = used` }.
- `fn try_grow(&self, _: &MemoryReservation, additional: usize) -> datafusion::error::Result<()>`: lock; `let next = used.saturating_add(additional)`;
  - `next > cap` → `Err(DataFusionError::ResourcesExhausted(format!("the query needs more than its {cap} bytes")))`;
  - `next > held` and `!shared.try_take(next - held)` → `Err(DataFusionError::ResourcesExhausted("the shared query memory is in use".to_owned()))`;
  - otherwise `held = held.max(next)`, `used = next`, `Ok(())`.
- `fn shrink(&self, _: &MemoryReservation, shrink: usize)`: lock; `used = used.saturating_sub(shrink)`; `let keep = used.max(initial)`; if `held > keep` { `shared.give_back(held - keep)`; `held = keep` }.
- `fn reserved(&self) -> usize`: `used`.
- `fn memory_limit(&self) -> MemoryLimit`: `MemoryLimit::Finite(cap)`.
- `register`/`unregister`: keep the trait's defaults.

`impl Drop for QueryPool`: `shared.give_back(held)` (read through `get_mut`, poison recovered with `PoisonError::into_inner`).

`MemoryPool` requires `Any + Send + Sync + Debug + Display` (`datafusion-execution-55.2.0/src/memory_pool/mod.rs:188`): add `impl std::fmt::Display for QueryPool` writing `llmobs_query(cap={cap})`.

Lock poisoning everywhere: `.lock().unwrap_or_else(PoisonError::into_inner)`.

### Runtimes (`lib.rs`)

- `prepare_layout` also creates `<data_dir>/tmp` and `<data_dir>/tmp/query` (with `ensure_dir`, like `wal`).
- Before `build_runtime`, `open_inner` empties `<data_dir>/tmp/query` on `spawn_blocking`: for each entry, `remove_dir_all` for a directory and `remove_file` for a file; a missing entry is fine; any other error is `OpenError` through `open_io("removing query spill files", &path, error)`. These are spill directories of a process that crashed (DataFusion removes its own on a clean drop). `LOCK` is held from before this step until `shutdown`, so no live `Storage` of this process or another uses them; a `Storage` value kept alive after its `shutdown` must not run queries (documented on `shutdown`: add that sentence to its doc comment). Log `tracing::info!(removed, "removed leftover query spill files")` when `removed > 0`.
- `build_runtime(metadata_cache_bytes, spill_dir: &Path)` adds `.with_temp_file_path(spill_dir)` to the builder. The base runtime keeps DataFusion's default memory pool (unbounded) and DataFusion's default `max_temp_directory_size` (100 GiB). It is used directly only by the media GC and by tests.
- `Storage` gets two fields: `memory: Arc<memory::SharedPool>` and `query_memory: QueryMemory`, where

```rust
/// What `Storage::query` needs to admit a query (spec 03 §6).
#[derive(Debug, Clone, Copy)]
struct QueryMemory {
    initial: usize,
    cap: usize,
    wait: Duration,
}
```

### `Storage::query` (`lib.rs`)

```rust
/// Admits a query, then returns providers for `project` over one published
/// snapshot, with a runtime of the query's own: its memory pool takes from
/// the process-wide pool, up to `query.max_memory` (spec 03 §6).
///
/// Waits up to `query.admission_wait` for `query.initial_memory`; then fails
/// with [`QueryError::Busy`].
pub async fn query(
    &self,
    project: ProjectId,
    options: QueryOptions,
) -> Result<ProjectQuery, QueryError>
```

Body:
1. `let deadline = tokio::time::Instant::now() + self.query_memory.wait;`
2. `let pool = self.memory.admit(initial, cap, deadline).await.ok_or(QueryError::Busy)?;`
3. `let runtime = RuntimeEnvBuilder::from_runtime_env(&self.runtime).with_memory_pool(pool).build_arc()?` — the error maps through `QueryError::DataFusion`. This shares the metadata cache, the object stores and the spill directory with the base runtime and swaps only the pool.
4. `project_query(&self.published.load(), project, options, &runtime, self.target_partitions, self.lookback)` (unchanged function). Load the snapshot **after** admission, so a query that waited sees the newest data.

Update the doc comment of `Storage::query` (it still says "the runtime is the one built at open. Its memory pool stays the default until a later milestone installs one"). Update `ProjectQuery::runtime`'s doc in `views/mod.rs` to "This query's runtime: its own memory pool over the process-wide one, and the shared spill directory and caches."

### Errors (`error.rs`)

```rust
pub enum QueryError {
    /// DataFusion rejected the session or the scan.
    #[error(transparent)]
    DataFusion(#[from] datafusion::error::DataFusionError),
    /// No query memory came free within `query.admission_wait`. The server
    /// maps this to 503 `busy` (spec 09).
    #[error("too many queries are running")]
    Busy,
}
```

`media.rs` `live_set` matches `QueryError` exhaustively: map `QueryError::Busy` to `MediaError::Query(DataFusionError::Internal("query admission in the media collection".to_owned()))` — unreachable in practice, since `live_set` calls `project_query` directly, not `Storage::query`. Write it as a match arm with that comment.

### Callers

Every `storage.query(...)` call becomes `storage.query(...).await` (all are in tests, including `crates/storage/tests/trace_bench.rs`). Run `grep -rn "\.query(" crates/storage` and fix each compile error. Don't change what the tests assert.

### Tests

In `memory.rs` (`#[tokio::test]`). Build every pool with `shared.admit(initial, cap, tokio::time::Instant::now()).await.unwrap()` (so the initial bytes are taken), then `let as_dyn: Arc<dyn MemoryPool> = pool.clone();` and a reservation `MemoryConsumer::new("test").register(&as_dyn)`. "Drop the pool" means dropping the reservation, `as_dyn` and the test's `Arc<QueryPool>`.
- `a_query_pool_fails_past_its_cap`: shared 100 MiB, initial 1 MiB, cap 10 MiB. `try_grow(9 MiB)` ok, `try_grow(2 MiB)` is `ResourcesExhausted`; `reserved()` is 9 MiB; `shared.used()` is 9 MiB.
- `a_query_pool_fails_when_the_shared_pool_is_full`: shared 10 MiB; pool A (initial 1 MiB, cap 10 MiB) grows to 9 MiB; pool B (initial 1 MiB, cap 10 MiB) is admitted: `shared.used()` is 10 MiB. B's `try_grow(2 MiB)` fails; `try_grow(1 MiB)` succeeds (it uses B's initial bytes).
- `shrinking_keeps_the_initial_bytes`: grow 5 MiB over initial 1 MiB, shrink 5 MiB: `shared.used()` back to 1 MiB.
- `dropping_a_query_pool_gives_everything_back`: grow 5 MiB, drop the reservation and the pool: `shared.used()` is 0.
- `admission_waits_for_memory_to_come_back` (`start_paused = true`): shared 2 MiB, initial 2 MiB. Admit A. Spawn admit B with a 5 s deadline. Advance 1 s, drop A: B returns `Some`.
- `admission_gives_up_at_the_deadline` (`start_paused = true`): shared 2 MiB, A admitted, B with a 100 ms deadline returns `None`.

In `lib.rs` tests:
- `a_query_waits_then_fails_as_busy`: config with `max_memory_total = 2MiB`, `max_memory = 2MiB`, `initial_memory = 2MiB`, `admission_wait = 100ms`. Hold one `ProjectQuery`; a second `storage.query` is `Err(QueryError::Busy)`; drop the first; a third succeeds.
- `a_query_that_cannot_spill_fails_as_too_large`: config `max_memory = 1MiB`, `initial_memory = 1MiB`. Append 20,000 spans in one trace with 200-byte names (`test_support` helpers), flush. Run through `QueryContext::new(storage.query(..).await?, None)` and A2's SQL entry point (`sql_json`, the same one `sql/run.rs` tests use) the query `SELECT a.name, b.name FROM spans a JOIN spans b ON a.trace_id = b.trace_id LIMIT 1` (the names are projected, so the build side holds them). Assert `RunError::TooLarge`. (The hash join's build side reserves without spilling.) If it does not fail, double the span count once; if it still does not fail, stop and report.
- `open_removes_leftover_spill_directories`: create `<data_dir>/tmp/query/datafusion-old/x` before open; after open it is gone, and `<data_dir>/tmp/query` holds exactly one `datafusion-*` directory (the new runtime's).

---

## M6.2 — Disk-full

Commit: `storage: refuse ingestion on a full disk and reserve disk for compaction (M6.2)`

### What it does (spec 03 §10, D-A1-9)

- A **disk monitor** measures free bytes every second: the minimum of the free space (`f_bavail × f_frsize`) of the file systems of `data_dir` and, in local mode, of the object store root.
- While free bytes are under `storage.min_free_bytes`, `append_spans`, `append_scores` and `put_media` fail with `DiskFull` (the server maps it to 503). Flush, compaction, GC, retention and deletions keep running: they reclaim space.
- Every compaction job (merges and rewrites, in both modes: outputs stream through a local temp file in both, D-A1-2) first **reserves** the sum of its `spans`/`scores` input sizes against free bytes minus what running jobs already reserved. A job that doesn't fit fails with `CompactionError::DiskFull` before it reads anything; a later pass retries it. It does not wait (D-A1-9): its input claims are released when `run` returns, so it blocks no rewrite of those files until the next pass.

### Config

`StorageConfig` gets a top-level field, right after `data_dir`:

```rust
/// Ingestion is refused while free disk is under this (spec 03 §10). Default 2 GiB.
#[serde(default = "default_min_free_bytes")]
pub min_free_bytes: ByteSize,
```

No validation rule (0 disables the check). Every `StorageConfig` literal gets `min_free_bytes: ByteSize::b(0)`: the test modules of `crates/storage/src` (`grep -rn "StorageConfig {" crates/storage/src`), the integration tests `crates/storage/tests/s3.rs`, `crates/storage/tests/trace_bench.rs` and `crates/storage/tests/crash.rs`, and `crates/server` (`src/serve.rs` tests and `src/test_support/mod.rs`, which already depend on `bytesize`; import `bytesize::ByteSize` where it isn't). Tests must not depend on the machine's free disk. Add a TOML test that `min_free_bytes = "5GiB"` parses and that the default is 2 GiB.

### Free space (`src/disk.rs`, new, `pub(crate) mod disk;`)

Dependency, root `Cargo.toml`, with a comment `# Free disk for the disk-full check (spec 03 §10).`:

```toml
rustix = { version = "1.1.6", default-features = false, features = ["std", "fs"] }
```

`crates/storage/Cargo.toml`: `rustix = { workspace = true }`. `rustix::fs` is Unix-only; the project builds for Linux and macOS only (spec 12), so no `cfg` is needed.

```rust
/// Free disk, refreshed by the monitor, and the bytes compaction reserved.
#[derive(Debug)]
pub(crate) struct Disk {
    /// `data_dir`, plus the object store root in local mode.
    paths: Vec<PathBuf>,
    min_free: u64,
    free: AtomicU64,
    reserved: Mutex<u64>, // std::sync::Mutex
    /// Tests set this to stand in for `statvfs`.
    #[cfg(test)]
    fake_free: Mutex<Option<u64>>,
}

impl Disk {
    /// Measures once. A `statvfs` error is `OpenError` via `open_io("measuring free disk", path, error)`.
    pub(crate) fn open(paths: Vec<PathBuf>, min_free: u64) -> Result<Self, OpenError>;
    /// Measures again and stores the result. Blocking: callers use `spawn_blocking`.
    pub(crate) fn refresh(&self) -> io::Result<u64>;
    /// The last measured free bytes.
    pub(crate) fn free(&self) -> u64;
    /// Whether ingestion must be refused: `free() < min_free`.
    pub(crate) fn low(&self) -> bool;
    /// Reserves `bytes` when `free() - reserved >= bytes` (saturating). The
    /// reservation is given back when dropped.
    pub(crate) fn try_reserve(self: &Arc<Self>, bytes: u64) -> Option<DiskReservation>;
    #[cfg(test)]
    pub(crate) fn set_free_for_test(&self, free: Option<u64>);
}

/// Bytes a compaction job holds against free disk.
#[derive(Debug)]
pub(crate) struct DiskReservation { disk: Arc<Disk>, bytes: u64 }
impl Drop for DiskReservation { /* reserved -= bytes, saturating */ }

/// Free bytes of the file system holding `path`: `f_bavail * f_frsize`.
fn free_bytes(path: &Path) -> io::Result<u64> {
    let stat = rustix::fs::statvfs(path)?;
    Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
}
```

`refresh`: with `fake_free` set, store and return it; otherwise the minimum of `free_bytes` over `paths` (`rustix::io::Errno` converts into `io::Error` with `?`). `set_free_for_test(v)` sets `fake_free = v` and then calls `refresh()` (ignoring an error), so `free` changes immediately in both directions.

`Storage::open_inner` builds `Arc<Disk>` right after `open_objects`, with `paths = [data_dir]` plus the local root. Add `pub(crate) fn local_root(&self) -> Option<&Path>` to `Objects` (the `Kind::Local { root }` path, `None` for remote).

### The monitor task (`disk.rs`)

```rust
/// Refreshes `disk` every second until stopped.
pub(crate) fn spawn(disk: Arc<Disk>) -> DiskHandle;
```

Mirror `gc::spawn`/`GcHandle` (a `watch` stop channel and an `abort` method). Each tick runs `refresh` on `spawn_blocking`. On error, log `tracing::warn!(%error, "measuring free disk failed")` only when the previous tick succeeded (keep a `bool`), and keep the last value. When `low()` changes, log once: `tracing::warn!(free, min_free, "free disk is under storage.min_free_bytes; ingestion is refused")` going low, `tracing::info!(free, "free disk is back above storage.min_free_bytes")` going back. `Storage` holds the `DiskHandle`; `shutdown` and `Drop for Storage` call `abort()` next to `self.gc.abort()`.

### Ingestion

- `AppendError::DiskFull` — `#[error("free disk is under storage.min_free_bytes")]`, doc "The server maps it to 503." In `Storage::append`, check `self.disk.low()` right after the `ProjectDropped` check.
- `MediaError::DiskFull` — same text; `label()` returns `"disk_full"`. In `Storage::put_media`, check it right after the `ProjectDropped` check.

### Compaction

- `Compactor` gets `pub(super) disk: Arc<Disk>` (a new `Compactor::new` parameter after `objects`).
- `CompactionError::DiskFull` — `#[error("not enough free disk for the job")]`, label `"disk_full"`.
- At the very top of `Compactor::run`, before it claims the derived `traces` files or reads anything (`pass` and the deletion pass already claimed the inputs; those claims are released when `run` returns, as today):
  ```rust
  let bytes = job.inputs.iter().fold(0_u64, |sum, file| sum.saturating_add(file.size));
  let _reservation = self.disk.try_reserve(bytes).ok_or(CompactionError::DiskFull)?;
  ```
  The reservation lives until `run` returns. This covers L0 merges, day closes and rewrites.
- Where `pass` and `check_and_rewrite` log a failed job: a `DiskFull` error logs at `tracing::info!` with the same fields and the message `"compaction job deferred: not enough free disk"`, not at `warn`. It still counts as `failed`.

### Tests (`disk.rs` and `lib.rs`)

- `disk.rs`: `try_reserve` grants up to `free`, refuses past it, and gives back on drop (fake free 100: reserve 60 ok, reserve 50 refused, drop the 60, reserve 50 ok). `free_bytes` of a temp dir is greater than 0.
- `appends_fail_while_the_disk_is_low`: open with `min_free_bytes = 1000`; `set_free_for_test(Some(10))`: `append_spans` and `append_scores` are `Err(AppendError::DiskFull)` and `put_media` is `Err(MediaError::DiskFull)`; `set_free_for_test(Some(10_000))`: all three succeed.
- `a_compaction_job_that_does_not_fit_is_deferred`: two flushed L0 files due for an L0 merge (reuse the M4 test setup that forces a merge with `set_policy`); `set_free_for_test(Some(0))`: `compact()` reports `failed = 1`, `jobs = 0`, files unchanged; `set_free_for_test(None)` then `compact()` merges.
- `a_rewrite_that_does_not_fit_is_deferred`: one flushed file with a covered row and a registered deletion; free 0: `apply_deletions` reports `failed = 1`, the deletion stays pending; free restored: applied.
- Storage needs a test accessor `#[cfg(test)] pub(crate) fn disk(&self) -> &Disk`.

---

## M6.3 — Quarantine: verification and maintenance reads

Commit: `storage: verify suspect files and quarantine corrupt ones (M6.3)`

### What it does (spec 03 §10, §4, D-A1-10)

- A **suspect** is a file id some read failed on with an error that may mean corruption. Suspects go into a **verify queue**.
- After the deletion pass, the compaction task runs a **verify pass**: each suspect is read again, whole, straight from the object store. The outcome is one of:
  - **healthy**: the read succeeds with the catalog's row count; remembered as healthy for 1 h, so the same file isn't re-read for every failing query;
  - **corrupt**: a decode error, a wrong size or row count, or a missing object;
  - **unavailable**: the object store failed; the file goes back in the queue for the next pass.
- A corrupt `spans` file is **quarantined** with its `traces` file; a corrupt `scores` file alone. Quarantined files leave the catalog snapshot and every read state; their objects stay for inspection. Their rows are lost (spec 03 §10).
- A corrupt `traces` file is **not** quarantined: its `spans` source is rewritten by a `JobKind::Rewrite` job, which recomputes the `traces` file and obsoletes the corrupt one. No data is lost.
- Quarantined files leave for good (state `obsolete`, collected by GC) when retention drops their day, when their project is dropped, or when a pending deletion may cover them (their bytes can't be rewritten, so they are deleted instead).

Suspects come from (this sub-phase): compaction and rewrite jobs, the deletion pass's check scan, and the trace-anchor rebuild at open. M6.4 adds failed queries and the media GC.

### Which errors make a file a suspect (`src/verify.rs`, new, `pub(crate) mod verify;`)

```rust
/// Whether `error` may come from a corrupt Parquet file rather than the object
/// store or the query (spec 03 §10). False positives only cost a verification.
pub(crate) fn may_be_corrupt(error: &DataFusionError) -> bool
```

Unwrap `Context`, `Diagnostic` and `Shared` exactly as `query_error::root` does (make `root` `pub(crate)` and call it). Then on the root:
- `DataFusionError::ParquetError(error)` → `!is_store_error(error)`;
- `DataFusionError::ArrowError(error, _)` → `matches!(**error, ArrowError::ParquetError(_))`;
- `DataFusionError::External(error)` → `error.downcast_ref::<ParquetError>().is_some_and(|e| !is_store_error(e))` or `error.downcast_ref::<ArrowError>().is_some_and(|e| matches!(e, ArrowError::ParquetError(_)))`;
- anything else → `false` (`ObjectStore`, `ResourcesExhausted`, plan errors, user arithmetic errors).

Don't look through `ArrowError::ExternalError` wrappers: DataFusion 55.2 doesn't use them on the Parquet read path.

```rust
/// A `ParquetError::External` holding an `object_store::Error` other than
/// `NotFound`: the store failed, the file may be fine. A missing object is
/// not a store failure: it makes the file a suspect, and verification
/// finds it corrupt.
pub(crate) fn is_store_error(error: &ParquetError) -> bool {
    match error {
        ParquetError::External(source) => source
            .downcast_ref::<object_store::Error>()
            .is_some_and(|error| !matches!(error, object_store::Error::NotFound { .. })),
        _ => false,
    }
}
```

Why (checked in the 55.2/59.3 sources): a corrupt page surfaces as `DataFusionError::ArrowError(ArrowError::ParquetError(text))` or `DataFusionError::ParquetError(..)`; a corrupt footer as `DataFusionError::ParquetError(ParquetError::General("Failed to fetch metadata for file …"))`, which also covers a footer fetch that failed in the store (the original error is flattened to text, so that case is a false positive that verification sorts out); a failed page fetch as `ParquetError::External(object_store::Error)`.

Unit tests: one case per bullet, including a `Shared(Context(..))` wrapping, and `ObjectStore`/`ResourcesExhausted`/`ArrowError::DivideByZero` → false.

### The verify queue (`verify.rs`)

```rust
/// Files waiting for verification, and files verified healthy recently.
#[derive(Debug, Default)]
pub(crate) struct VerifyQueue {
    state: Mutex<QueueState>, // std::sync::Mutex
}

#[derive(Debug, Default)]
struct QueueState {
    pending: VecDeque<FileId>,
    queued: HashSet<FileId>,
    /// File id → when it was last verified healthy, ns.
    healthy: HashMap<FileId, i64>,
}

/// At most this many files wait; more suspects are dropped (and logged once per call).
pub(crate) const QUEUE_MAX: usize = 4096;
/// A file verified healthy is not queued again for this long.
pub(crate) const HEALTHY_FOR: i64 = 60 * 60 * 1_000_000_000;

impl VerifyQueue {
    /// Queues every id that is not queued and not verified healthy within
    /// `HEALTHY_FOR` of `now`. Returns how many were queued.
    pub(crate) fn suspect(&self, ids: impl IntoIterator<Item = FileId>, now: i64) -> usize;
    /// Removes and returns up to `max` ids, oldest first.
    pub(crate) fn take(&self, max: usize) -> Vec<FileId>;
    /// Puts `id` back at the end (unavailable, or claimed by a running job).
    pub(crate) fn requeue(&self, id: FileId);
    /// Remembers `id` as healthy at `now`, and forgets entries older than `HEALTHY_FOR`.
    pub(crate) fn healthy(&self, id: FileId, now: i64);
}
```

When `suspect` drops ids because the queue is full, it logs `tracing::warn!(dropped, "verify queue is full; suspect files dropped")`. `Storage` owns one `Arc<VerifyQueue>`, shared with the `Compactor` (new field `pub(super) verify: Arc<VerifyQueue>`, a new `Compactor::new` parameter after `disk`), the anchor rebuild, and (M6.4) queries and the media GC.

### Reading a file whole (`verify.rs`)

```rust
pub(crate) enum Verdict {
    Healthy,
    Corrupt { reason: &'static str },
    Unavailable,
}

/// Reads `file` whole from `store`: every row group, every column.
pub(crate) async fn verify_file(store: &Arc<dyn ObjectStore>, file: &FileMeta) -> Verdict
```

Steps, in order:
1. `store.head(&ObjectPath::from(file.path.as_str()))` (`ObjectStoreExt::head`): `Err(object_store::Error::NotFound { .. })` → `Corrupt { reason: "missing object" }`; other `Err` → `Unavailable`; `meta.size != file.size` → `Corrupt { reason: "size differs from the catalog" }`.
2. `ParquetObjectReader::new(Arc::clone(store), path).with_file_size(file.size)` (with `#[allow(deprecated)]`, as `anchor.rs` and `compaction/slice.rs` do), then `ParquetRecordBatchStreamBuilder::new_with_options(reader, ArrowReaderOptions::new().with_page_index_policy(PageIndexPolicy::Required)).await` (the page index too, since queries load it: the same options API `parquet.rs` already uses near line 681; every file is written with page statistics), `.build()`, and drain the stream counting rows. Any `Err(error)`: `is_store_error(&error)` → `Unavailable`, else `Corrupt { reason: "decode failed" }`.
3. Total rows `!= file.rows` → `Corrupt { reason: "row count differs from the catalog" }`. Otherwise `Healthy`.

`store` is the object store as registered (M6b wraps it with the read cache; M6.5 passes the unwrapped store here).

### Catalog: the quarantine edit (`catalog/committer.rs`)

`CatalogEdit` gets a field:

```rust
/// Active files to mark `quarantined` (spec 03 §10). A `spans` file must
/// come with its active `traces` file, like a removal.
pub quarantine: Vec<FileId>,
```

Add `quarantine: Vec::new()` to every `CatalogEdit` literal (`grep -rn "CatalogEdit {" crates/storage/src`). In `write_edit`:
- each quarantined id must be `active`, else `CommitError::conflict("quarantined file is not active")`;
- an id in both `remove` and `quarantine`, or in both `raise` and `quarantine` → `CommitError::conflict("quarantined file is also removed or raised")`;
- the dependants rule ("a removed spans file keeps its traces file", "a removed traces file keeps a source") checks the union of `remove` and `quarantine` ids, for ids of both lists;
- after the `remove` UPDATEs: `UPDATE data_files SET state = 'quarantined' WHERE id = ?` for each (no `obsoleted_at`).

`commit_inner` updates the in-memory catalog with `Catalog::apply(edit)`, which returns the new catalog and the removed files, and puts the removed files in `inner.retired` (GC's reference counts). Make `Catalog::apply` also drop the `quarantine` ids from the new catalog, **without** returning them among the removed files: quarantined files are not retired, and GC never deletes them. A quarantine id absent from the in-memory catalog is the same error `apply` returns for an absent `remove` id.

Add `Default` to `CatalogEdit`'s derive (every field is a `Vec` or an `Option`), so new edits can be written `CatalogEdit { quarantine: ids, ..Default::default() }`.

New committer methods:

```rust
/// The `spans` sources of an active `traces` file.
pub(crate) async fn traces_sources(&self, traces: FileId) -> Result<Vec<FileId>, CommitError>;

/// Quarantined `spans` and `scores` files of `project`, with what the
/// deletion rule needs: `(id, table, min_ingest_time, deletions_applied_seq)`.
pub(crate) async fn quarantined(&self, project: ProjectId)
    -> Result<Vec<(FileId, FileTable, i64, u64)>, CatalogError>;

/// Marks quarantined files `obsolete` (GC deletes them), with the
/// quarantined `traces` files whose sources are among them.
pub(crate) async fn retire_quarantined(&self, ids: &[FileId]) -> Result<(), CatalogError>;
```

`retire_quarantined` runs in one transaction: `UPDATE data_files SET state = 'obsolete', obsoleted_at = ? WHERE id = ? AND state = 'quarantined'` for each id, then the same for every `traces` file whose `file_sources` rows point at one of them and whose state is `quarantined`. It doesn't touch the in-memory catalog (quarantined files aren't in it).

Retention and drop (`Extra::Retention`, `Extra::Drop` SQL in `commit_inner`). The `obsoleted_at` value is the `created_at` that `write_edit` uses for the edit's own removals (`now_ns()` at commit); if it isn't reachable from the `Extra` SQL, call `now_ns()` there:
- retention adds `UPDATE data_files SET state = 'obsolete', obsoleted_at = ? WHERE state = 'quarantined' AND project_id = ? AND day < ?` (same `keep_from` text as the file selection);
- drop adds the same without the `day` condition.

### Verify pass (`compaction/verify_pass.rs`, new, `mod verify_pass;` in `compaction/mod.rs`)

```rust
/// What one verify pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VerifyReport {
    pub(crate) healthy: u64,
    pub(crate) quarantined: u64,
    pub(crate) repaired: u64,
    pub(crate) unavailable: u64,
    pub(crate) failed: u64,
}

/// Files verified per pass.
const VERIFY_PER_PASS: usize = 16;

impl Compactor {
    pub(crate) async fn verify_pass(self: &Arc<Self>, now: i64) -> VerifyReport;
}
```

`policy` is read once at the start, `*self.policy.lock().unwrap_or_else(PoisonError::into_inner)`, as `deletion_pass` does. For each id from `self.verify.take(VERIFY_PER_PASS)`, sequentially:
0. If `self.stopping` is set: `requeue` this id and the remaining ones, and return the report.
1. Find it among the active files of `self.committer.catalog().await` (search `files_all` of the three tables). Absent → skip (already replaced or quarantined).
2. Claimed by a running job → `requeue`, continue. Otherwise claim it (and, for `spans`, its `traces` file from `derived_traces`) with a `ClaimGuard` for the rest of this file's handling. A `derived_traces` or `traces_sources` error → `failed += 1`, `requeue`, continue.
3. `verify_file(&self.objects.store(), &file).await`:
   - `Healthy` → `self.verify.healthy(id, now)`; `healthy += 1`.
   - `Unavailable` → `requeue`; `unavailable += 1`.
   - `Corrupt { reason }` → log `tracing::error!(file_id = %id, table, project, day, reason, "corrupt data file")`, then by table:
     - `Spans` or `Scores`: commit `CatalogEdit { quarantine: [id] + (spans: its traces file), ..Default }` with `publish_catalog` like the raise commit in `rewrite.rs`; log `tracing::error!(file_id, table, project, day, rows = file.rows, "file quarantined; its rows are lost")`; `quarantined += 1`. A commit error → `failed += 1`, `requeue`.
     - `Traces`: `traces_sources(id)`. Claim every source `spans` id in one step under the claims lock with a new `ClaimGuard` (if any is already claimed: `requeue(id)`, continue). Then drop this file's own guard (the traces id): `Compactor::run` claims the derived `traces` file itself and fails with "traces file claimed" if it is held. Keep the sources' guard across the runs. For each source, find its `FileMeta` in the catalog and run `self.run(Job { table: FileTable::Spans, project, day, kind: JobKind::Rewrite, inputs: vec![source], whole_partition: false, drop_deleted: false }, policy)`; all ok → log `tracing::warn!(file_id, project, day, "corrupt traces file rebuilt from its spans")`, `repaired += 1`; an error → `failed += 1` and `requeue(id)` (a corrupt source becomes a suspect itself, below). `run`'s `derived_traces` check passes: the `traces` file is active and its only source is the job's input (one `traces` file per `spans` file, D-A1-1).

The compaction task (`compaction/task.rs`) runs `compactor.verify_pass(now_ns()).await` after the media step, and logs the report at `debug` when any count is non-zero, like the other passes.

### Suspects from maintenance reads

- **Compaction and rewrite jobs.** In `Compactor::run`, wrap the body: when it returns `Err(CompactionError::Read { source })` and `may_be_corrupt(&source)`, call `self.verify.suspect(job input ids, now_ns())`. (`time_values` already maps Parquet errors to `CompactionError::Read { source: DataFusionError::ParquetError(..) }`.) A spans job's `traces` files are not read by the merge and are not suspects.
- **The deletion pass's check scan.** In `check_and_rewrite`, when `holds_covered` returns `Err(CompactionError::Read { source })` with `may_be_corrupt(&source)`, suspect the file before returning the error.
- **The trace-anchor rebuild at open** (`anchor.rs`). Move the per-file body of `rebuild` into `async fn rebuild_file(&mut self, file, objects, since) -> Result<usize, AnchorError>` (rows read). In the loop:
  - `Ok(rows)` → add to `rows_read`;
  - `Err(AnchorError::Parquet(error))` with `verify::is_store_error(&error)` → return the error (open fails, as today: the store is down);
  - any other `Err` → `tracing::warn!(file_id = %file.id, "skipping an unreadable traces file in the anchor rebuild")`, `verify.suspect([file.id], now_ns())`, continue. Anchor entries already collected from that file stay (anchors are hints); only its rows are missing from the `rows` count of the final log line.
  `rebuild` takes `verify: &VerifyQueue` as a new parameter.

### Quarantined files and deletions (`compaction/rewrite.rs`)

In `deletion_pass`, after step 3 (candidates) and before the applied check, for each project with pending deletions:
- `self.committer.quarantined(project)`; keep those a pending deletion may cover (`!applicable(&deletions, Some(das), min_ingest_time).is_empty()`, the same rule as for active files);
- if any: `retire_quarantined(&ids)`; log `tracing::warn!(project, files, "quarantined files deleted for a pending deletion")`.
An error is logged with `error = error.label()`-style static text (`"retiring quarantined files failed"`) and counted in `report.failed`.

The "applied" check already looks at active files only, so a quarantined file never blocks a deletion; retiring the ones it may cover makes their bytes go with GC like any obsolete file.

### Hook point

`HookPoint::QuarantineBeforeCommit`: in `verify_pass`, after the verdict `Corrupt` and before the quarantine commit (`pause` and `fail`; on `fail`, count `failed` and `requeue`).

### Tests (`src/quarantine_tests.rs`, new, `#[cfg(test)] mod quarantine_tests;`)

Setup: open a local storage with a config copied from `deletion_tests::config_in` (local object store under the temp dir), append with the `test_support` span helpers, flush. Tests that force an L0 merge use `set_policy` with `l0_files = 2`, as the M4 tests do.

**`FaultStore` (`test_support.rs`)** today fails only writes. Add `pub(crate) fn fail_reads(&self, on: bool)` (an `AtomicBool`): while on, `get_opts` returns the same injected error the write faults return, so `head`, `get_range` and `get_ranges` fail too. Tests that need it open with `Storage::open_with_store(config, Arc::new(FaultStore::new(Arc::new(InMemory::new()))))` and keep a clone of the `Arc<FaultStore>`.

**Corrupting a file** — one helper, `fn corrupt(data_dir: &Path, file: &FileMeta, how: Corruption)`, on the local object `<data_dir>/objects/<file.path>`:
- `Corruption::Page`: read the footer with `parquet::file::metadata::ParquetMetaDataReader` (from the file's bytes), take `row_group(0).column(0).data_page_offset()`, and overwrite the 32 bytes there (the page header) with `0xFF`. Size unchanged; the decode fails.
- `Corruption::Footer`: overwrite the trailing 4-byte `PAR1` magic with `0xFF`. Size unchanged.
- For "size differs from the catalog", truncate the object instead, the way `compaction/rewrite.rs` tests overwrite a whole file (near line 1211).

- `verify_file_sorts_files`: healthy file → `Healthy`; `Page` → `Corrupt { reason: "decode failed" }`; `Footer` → `Corrupt { reason: "decode failed" }`; truncated → `Corrupt { reason: "size differs from the catalog" }`; deleted object → `Corrupt { reason: "missing object" }`; through a `FaultStore` with `fail_reads(true)` → `Unavailable`.
- `a_corrupt_spans_file_is_quarantined_with_its_traces`: two flushed spans files; corrupt one (`Page`); suspect it; `verify_pass`: report `quarantined = 1`; the catalog has neither it nor its traces file; `data_files.state` is `quarantined` for both; queries return only the other file's rows; the object is still on disk; a GC run (`collect_obsolete` with grace 0) leaves it.
- `a_corrupt_traces_file_is_rebuilt`: corrupt a traces file (`Page`); suspect it; `verify_pass`: `repaired = 1`; the spans rows are all there, the traces view returns every trace, the old traces file is `obsolete`.
- `a_failing_merge_suspects_its_inputs`: three L0 files, one corrupt (`Page`); `compact()` fails the job; the queue holds the three ids; `verify_pass` quarantines the corrupt one; the next `compact()` merges the other two.
- `a_healthy_file_is_not_verified_twice_within_an_hour`: suspect a healthy file, `verify_pass` (healthy 1), suspect again: `suspect` returns 0.
- `a_store_outage_requeues`: storage on a `FaultStore`; flush one file; suspect it; `fail_reads(true)`; `verify_pass`: `unavailable = 1`, the id is queued again.
- `the_anchor_rebuild_skips_a_corrupt_traces_file`: corrupt a traces file (`Footer`), reopen: open succeeds, the id is queued.
- `retention_and_drop_retire_quarantined_files`: quarantine a file of day D; `apply_retention(keep_from = D + 1)`: its state is `obsolete`; same with `drop_project` for another project.
- `a_pending_deletion_retires_quarantined_files_it_may_cover`: quarantine a spans file, register a deletion covering it, `apply_deletions`: the file and its traces file are `obsolete`, and the deletion is applied.
- `a_failed_quarantine_commit_requeues` with `QuarantineBeforeCommit` failing.

Storage test accessors as needed: `#[cfg(test)] pub(crate) fn verify_queue(&self) -> &VerifyQueue` and `#[cfg(test)] pub(crate) async fn verify_pass(&self) -> VerifyReport`.

---

## M6.4 — Quarantine: failed queries report their files

Commit: `storage: queue the files of a query that failed to decode (M6.4)`

### What it does (spec 03 §10, D-A1-10)

A query records which files its scans read. When it fails with an error `may_be_corrupt` accepts, those files become suspects, and the next verify pass sorts them out. That catches a corrupt file compaction never reads again (an old, fully merged day).

### Scanned files (`verify.rs`)

```rust
/// The files one query's scans read, and the queue to report them to.
#[derive(Debug, Clone)]
pub struct Suspects {
    inner: Arc<SuspectsInner>,
}

#[derive(Debug)]
struct SuspectsInner {
    files: Mutex<HashSet<FileId>>, // std::sync::Mutex
    queue: Arc<VerifyQueue>,
}

impl Suspects {
    pub(crate) fn new(queue: Arc<VerifyQueue>) -> Self;
    /// Not connected to any storage: reports go nowhere. For `ProjectQuery`
    /// values built by tests.
    pub(crate) fn detached() -> Self;  // a fresh, private VerifyQueue
    /// Records the files a scan reads.
    pub(crate) fn record(&self, files: &[Arc<FileMeta>]);
    /// Queues the recorded files when `error` may come from a corrupt file.
    pub(crate) fn report(&self, error: &DataFusionError);
}
```

`report` logs `tracing::warn!(files, "a query failed to decode; its files are queued for verification")` when it queues any. `Suspects` is `pub` because it is a field of the public `ProjectQuery`; its constructors stay `pub(crate)`. Re-export nothing.

### Plumbing

- `ProjectQuery` (`views/mod.rs`) gets `pub suspects: Suspects` with the doc "Files this query's scans read; a decode failure queues them for verification (spec 03 §10)."
- `project_query` (`lib.rs`) takes `verify: &Arc<VerifyQueue>`, builds one `Suspects::new(Arc::clone(verify))`, passes a clone to each provider constructor (new last parameter `suspects: Suspects` of `SpansProvider::new`, `ScoresProvider::new`, `TracesProvider::new`), and puts it in the returned `ProjectQuery`. The dropped-project branch passes clones of the same value too (it reads no file).
- Each provider stores its `Suspects` in a field. Add a `suspects: &Suspects` parameter to both `query/mod.rs::file_plan` and `query/traces.rs::file_plan` (the traces one takes `(schema, input_indices, files, …)` and has no `ScanUnion`), threaded from the providers' fields; for `query/mod.rs` add a `suspects: &'a Suspects` field to `ScanUnion` and fill it where the spans and scores providers build their `ScanUnion`. Each `file_plan` calls `suspects.record(files)`. These are the only places a Parquet file scan is built for a query.
- Test-built `ProjectQuery` literals use `Suspects::detached()`.
- `QueryContext` (`views/mod.rs`) stores `suspects: Suspects` from `source.suspects` in `new`.
- A2's error mapping: add to `impl QueryContext` (in `query_error.rs` or `views/mod.rs`)

  ```rust
  /// [`classify`], after queueing this query's files when `error` may come
  /// from a corrupt file (spec 03 §10).
  pub(crate) fn fail(&self, error: DataFusionError) -> RunError {
      self.suspects.report(&error);
      classify(error)
  }
  ```

  The rule: **every error of a DataFusion execution call** goes through `ctx.fail` instead of `classify`. Execution calls are `DataFrame::collect`, `DataFrame::count`, `DataFrame::execute_stream`, `datafusion::physical_plan::collect`/`execute_stream`, and items of a stream they returned. Planning calls (building a `DataFrame`: `filter`, `select`, `aggregate`, `sort`, `limit`, `with_column`, `create_physical_plan`, parsing) keep `classify`: they read no file.
  - Run `grep -n "\.collect()\|\.count()\|execute_stream\|collect(stream" src/reads/*.rs src/sql/*.rs` (excluding iterator `.collect()` on plain Rust collections) to find the execution calls.
  - Where the function doing the call has no `QueryContext` in scope, add a `ctx: &QueryContext` parameter and pass it from the caller. Today that is at least `reads/traces.rs::finish` (the trace-list `collect`); check `reads/metrics.rs` helpers that call `collect()` the same way.
  - The SQL export stream in `sql/run.rs` (`poll_inner`) holds a `Suspects` clone and calls `suspects.report(&error)` before `classify(error)`.
  - Leave `classify` itself and its tests unchanged.
- Media GC (`media.rs` `live_set`): pass `&self.verify` to `project_query` (`MediaGc` gets `verify: Arc<VerifyQueue>`; add it to the `MediaGc` literal in `lib.rs` and to any in `media.rs` tests), and on the `collect().await` error call `query.suspects.report(&error)` before mapping it.

### Tests

- `verify.rs`: `report` queues recorded files only for a decode-like error, and not for `ResourcesExhausted`.
- `a_query_on_a_corrupt_file_queues_it`: two flushed spans files, corrupt one; a spans read through A2 (the same call `reads/traces.rs` tests use for a trace list, or `sql_json` with `SELECT * FROM spans`) fails; the queue holds both files' ids (the query read both); `verify_pass` quarantines the corrupt one; the same query then succeeds with the other file's rows.
- `a_user_error_queues_nothing`: `SELECT 1 / 0 FROM spans` over healthy files fails; the queue is empty.
