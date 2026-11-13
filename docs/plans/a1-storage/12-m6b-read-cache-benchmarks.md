# A1 · M6b — S3 read cache and benchmarks

Second half of milestone M6, **Resources and hardening**. It needs `11-m6a-resources-hardening.md` done (M6.1–M6.4). It adds:
- the on-disk S3 read cache (M6.5);
- the benchmarks recorded against spec 03's capacity check, on local disk and on S3 with a cold and a warm cache (M6.6).

Decisions: D-A1-11 (read cache), D-A1-12 (benchmarks). Spec: `docs/spec/03-storage.md` § Layout, §6 S3 read cache, § Capacity sanity check.

The conventions of `11-m6a-resources-hardening.md` § Conventions apply unchanged, with these differences:
- **Dependencies:** none. `crc32c` is already a dependency of `llmobs-storage`; `object_store`, `futures`, `bytes`, `chrono` too.
- **Allowed edits outside `crates/storage/` and `docs/`:** none. M6.5 adds a field to `QueryConfig`, whose literals all use `QueryConfig::default()`.

**M6 exit criteria** (`00-design.md` § Milestones):
- benchmarks recorded against spec 03's capacity check, on local disk and on S3 with a warm and a cold cache (M6.6);
- disk-full tests (M6.2) and corrupt-file tests (M6.3, M6.4).

---

## M6.5 — S3 read cache

Commit: `storage: cache S3 reads of data files on local disk (M6.5)`

### What it does (spec 03 §6, D-A1-11)

- With an S3 object store, every read of a data file (`tables/…`) goes through a **cache store** wrapping the S3 store. It serves byte ranges from **1 MiB blocks** kept under `<data_dir>/cache/`, fetching missing blocks from S3.
- Media blobs (`media/…`), writes, deletes and listings pass through unchanged; a delete also drops the object's blocks.
- Flush and compaction, after uploading a data file, also write its blocks into the cache (write-through): recent data is what gets read.
- The cache is bounded by `query.read_cache_bytes` (default 10% of the `data_dir` file system, at most 100 GiB; 0 turns it off). Eviction is least-recently-used by blocks.
- It survives a restart: open rebuilds the index from the files, oldest first by modification time. A hit doesn't touch the file, so after a restart the LRU order is the write order: accepted.
- Each block file ends with a CRC-32C of its bytes; a block that fails the check is deleted and fetched again.
- It never pushes the disk under the floor: it writes nothing while free disk is under `2 × storage.min_free_bytes`, and the disk monitor evicts blocks while free disk is under `storage.min_free_bytes`.
- In local mode there is no cache store: the files are already local.
- Queries, compaction and the media GC read through it (they share the registered store). Verification (M6.3) reads the S3 store directly, so a corrupt cache block can never make a healthy file look corrupt.

### Config (`QueryConfig`)

```rust
/// Bytes of the S3 read cache under `<data_dir>/cache`. Absent means 10% of
/// the file system holding `data_dir`, at most 100 GiB. Zero turns it off.
/// Unused with a local object store.
#[serde(default)]
pub read_cache_bytes: Option<ByteSize>,
```

Default `None`; no validation rule. TOML test: `read_cache_bytes = "0B"` and `"20GiB"` parse.

Resolved in `Storage::open_inner`, remote mode only: `Some(v)` → `v`; `None` → `min(total / 10, 100 GiB)` where `total` is `disk::total_bytes(&data_dir)?` (new, in `disk.rs`: `f_blocks × f_frsize` of `statvfs`, same error mapping as `Disk::open`). Zero → no cache store.

### Layout (`<data_dir>/cache/`)

```
cache/
  <file name>/            the data file's last path segment, e.g. 0-0192…7f.parquet
    size                  the object's size in bytes, decimal ASCII, no newline
    <block index>         block i holds bytes [i × 1 MiB, min((i + 1) × 1 MiB, size)),
                          then 4 bytes: CRC-32C of those bytes, little-endian
```

File names are unique (UUIDv7, spec 03 § Layout), so the last path segment identifies the object. `prepare_layout` does **not** create `cache/`; `ReadCache::open` does, in remote mode.

Writes go to `<dir>/.<name>.tmp-<uuidv7>` and are renamed into place. Nothing is fsynced: a block lost or torn by a crash fails its CRC or its length check and is fetched again.

### `ReadCache` (`src/read_cache.rs`, new, `pub(crate) mod read_cache;`)

```rust
/// Block size of the S3 read cache.
pub(crate) const BLOCK: u64 = 1 << 20;

/// The on-disk block cache (spec 03 §6). Cheap to share: every field is behind `Arc` or a lock.
#[derive(Debug)]
pub(crate) struct ReadCache {
    dir: PathBuf,
    capacity: u64,
    disk: Arc<Disk>,
    min_free: u64,
    state: Mutex<CacheState>, // std::sync::Mutex
    hits: AtomicU64,
    misses: AtomicU64,
}

#[derive(Debug, Default)]
struct CacheState {
    /// Block → (last use tick, bytes on disk including the CRC).
    blocks: HashMap<(String, u64), (u64, u64)>,
    /// Last use tick → block, for eviction in LRU order.
    order: BTreeMap<u64, (String, u64)>,
    next_tick: u64,
    bytes: u64,
    /// File name → object size, for the files with a `size` file.
    sizes: HashMap<String, u64>,
}

/// Counters for the benchmarks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReadCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub bytes: u64,
    pub blocks: u64,
}

impl ReadCache {
    /// Creates `dir`, removes temp files, and rebuilds the index: every block
    /// file is checked for its name only (not its CRC), ordered by mtime, then
    /// the oldest are evicted down to `capacity`. A `size` file that doesn't
    /// parse is removed with its directory. Blocking: call on `spawn_blocking`.
    pub(crate) fn open(dir: PathBuf, capacity: u64, disk: Arc<Disk>, min_free: u64)
        -> Result<Self, OpenError>;

    /// The object size, if known.
    fn size(&self, name: &str) -> Option<u64>;
    /// Records and writes the `size` file. Blocking.
    fn set_size(&self, name: &str, size: u64) -> io::Result<()>;

    /// Reads the cached blocks among `indexes`. A block whose file is
    /// missing, has the wrong length (`BLOCK + 4`, or `size − i × BLOCK + 4`
    /// for the last), or fails its CRC is dropped from the index (file
    /// removed) and left out. Marks the found ones used. Blocking.
    fn read_blocks(&self, name: &str, indexes: &[u64]) -> Vec<(u64, Bytes)>;

    /// Writes blocks, unless free disk is under `2 × min_free` (then skips
    /// all). A block already in the index is skipped (a concurrent fetch
    /// wrote it). Evicts LRU blocks while over `capacity`. Errors are logged
    /// at `debug` with the file name and skipped. File IO happens outside the
    /// `state` lock: write the file, then lock to index it. Blocking.
    fn write_blocks(&self, name: &str, blocks: &[(u64, Bytes)]);

    /// Drops every block of `name` and its directory. Blocking.
    pub(crate) fn evict_file(&self, name: &str);

    /// Evicts LRU blocks until at least `bytes` are freed. Blocking.
    pub(crate) fn shed(&self, bytes: u64);

    /// Write-through for a data file just uploaded: its size and every block.
    /// Blocking.
    pub(crate) fn insert_bytes(&self, name: &str, bytes: &Bytes);
    /// The same from a local file of `size` bytes, read 1 MiB at a time. Blocking.
    pub(crate) fn insert_file(&self, name: &str, file: &mut std::fs::File, size: u64);

    pub(crate) fn stats(&self) -> ReadCacheStats;
}
```

`hits` counts blocks served from disk, `misses` blocks fetched from S3.

The `name` of an object path is its last segment (`ObjectPath::filename()`). Only paths whose first segment is `tables` are cached: `fn cached_name(location: &ObjectPath) -> Option<String>`.

### `CacheStore` (`read_cache.rs`)

```rust
/// The S3 store with data-file range reads served from [`ReadCache`].
#[derive(Debug)]
pub(crate) struct CacheStore {
    inner: Arc<dyn ObjectStore>,
    cache: Arc<ReadCache>,
}

impl std::fmt::Display for CacheStore {
    // "ReadCache(<inner>)"
}

#[async_trait::async_trait]
impl ObjectStore for CacheStore { … }
```

The `ObjectStore` trait of object_store 0.13.2 (`#[async_trait]`, bound `Display + Send + Sync + Debug + 'static`) has these required methods; implement each:
- `put_opts(&self, location: &Path, payload: PutPayload, opts: PutOptions) -> Result<PutResult>` → `inner`;
- `put_multipart_opts(&self, location: &Path, opts: PutMultipartOptions) -> Result<Box<dyn MultipartUpload>>` → `inner`;
- `get_opts(&self, location: &Path, options: GetOptions) -> Result<GetResult>` → below;
- `get_ranges(&self, location: &Path, ranges: &[Range<u64>]) -> Result<Vec<Bytes>>` (provided, override it) → below;
- `delete_stream(&self, locations: BoxStream<'static, Result<Path>>) -> BoxStream<'static, Result<Path>>` → `inner.delete_stream(locations)` followed by `.then(move |result| { let cache = Arc::clone(&cache); async move { if let Ok(path) = &result && let Some(name) = cached_name(path) { let _ = tokio::task::spawn_blocking(move || cache.evict_file(&name)).await; } result } }).boxed()`: each deleted data file's blocks are gone before the delete is reported;
- `list`, `list_with_delimiter`, `copy_opts` → `inner`.

`list_with_offset` and `rename_opts` keep the trait's defaults.

**`get_opts`.** Served from the cache only when all hold: `cached_name(location)` is `Some`; `options.range` is `Some(GetRange::Bounded(range))`; `options.head` is false; `if_match`, `if_none_match`, `if_modified_since`, `if_unmodified_since` and `version` are `None`. Otherwise `inner.get_opts(location, options)`.
When served: `let (size, mut parts) = self.read(location, &name, std::slice::from_ref(&range)).await?; let bytes = parts.remove(0);` and return

```rust
GetResult {
    payload: GetResultPayload::Stream(futures::stream::once(async move { Ok(bytes) }).boxed()),
    meta: ObjectMeta {
        location: location.clone(),
        // Data files are immutable; the cache keeps no modification time.
        last_modified: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        size,
        e_tag: None,
        version: None,
    },
    range: range.start..range.end.min(size),
    attributes: Attributes::default(),
}
```

**`get_ranges`.** For a cached name: `self.read(location, name, ranges)`; otherwise `inner.get_ranges`.

**`read(&self, location: &Path, name: &str, ranges: &[Range<u64>]) -> object_store::Result<(u64, Vec<Bytes>)>`** returns the object size and one `Bytes` per range, in order:
1. The size: `cache.size(name)`, or `inner.head(location).await?.size`, then `set_size` on `spawn_blocking` (an error is logged at `debug` and ignored).
2. If any range has `start >= end` or `start >= size`: return `inner.get_ranges(location, ranges).await` with `size` (the store produces the exact error). A range with `end > size` is clamped to `size` (S3's own behaviour for a bounded range).
3. The block indexes the clamped ranges touch, sorted and deduplicated.
4. `read_blocks` on `spawn_blocking` (a `JoinError` counts as all missing).
5. The missing indexes, grouped into runs of consecutive indexes; each run is the byte range `[first × BLOCK, min((last + 1) × BLOCK, size))`. Fetch all runs with one `inner.get_ranges(location, &runs).await?`. Split each run's bytes into blocks. Add `misses`, then `write_blocks` on `spawn_blocking` and **await** it (a `JoinError` is ignored), so a read that returns has its blocks on disk.
6. Add the found count to `hits`. Assemble each requested range from the blocks: one block → `Bytes::slice`; several → copy into a `BytesMut` of the range's length.

### Wiring (`objects.rs`, `lib.rs`)

- `Objects` gets `raw: Arc<dyn ObjectStore>` (the store without the cache; equal to `store` in local mode or without a cache) and `cache: Option<Arc<ReadCache>>`. Add `pub(crate) fn raw_store(&self) -> Arc<dyn ObjectStore>` and `pub(crate) fn read_cache(&self) -> Option<&Arc<ReadCache>>`.
- `Objects::new` and `Objects::remote_for_test` set `raw = Arc::clone(&store)` and `cache: None`.
- Construction order in `open_inner`: `open_objects` now returns `Objects` (not `Arc<Objects>`); the `Disk` is opened (it needs `objects.local_root()`); then, remote mode with a non-zero capacity, `ReadCache::open` (on `spawn_blocking`) and `objects = objects.with_read_cache(cache)` (`fn with_read_cache(self, cache: Arc<ReadCache>) -> Self`, which sets `store = Arc::new(CacheStore { inner: Arc::clone(&raw), cache: Arc::clone(&cache) })` and `cache = Some(cache)`); then `let objects = Arc::new(objects);`, and only then the existing clone for `remove_temp_files` and the `register` calls. `register` (query and compaction runtimes) uses `store`, so both read through the cache.
- Verification (`verify_pass`) uses `self.objects.raw_store()` instead of `store()`; the anchor rebuild and `time_values` keep `store()`.
- Write-through, remote mode with a cache, data files only (`cached_name` of the key is `Some`):
  - `put_durable`: after `put_remote` succeeds, `cache.insert_bytes(name, &bytes)` on `spawn_blocking` (clone the `Bytes` before the upload; an error or `JoinError` is ignored).
  - `publish_output` (remote branch, which is async and moves the `File` into the upload): after `upload` is `Ok` and **before** `remove_spill_file`, when a cache exists and `cached_name` of the key is `Some`, run `tokio::task::spawn_blocking` that reopens `temp_for_cleanup` with `std::fs::File::open` and calls `cache.insert_file(&name, &mut file, size)`; ignore its errors and `JoinError`. The removal of the spill file stays as it is.
- The disk monitor (`disk::spawn`) takes `Option<Arc<ReadCache>>`; each tick, after `refresh`, when the cache is present and `free < min_free`: `cache.shed(min_free - free + BLOCK)` on `spawn_blocking`.
- `Storage` gets `#[doc(hidden)] pub fn read_cache_stats(&self) -> Option<ReadCacheStats>` (the benchmarks use it). Re-export `ReadCacheStats` from `lib.rs` next to the other report types.

### Tests (`read_cache.rs`)

Use `object_store::memory::InMemory` as the inner store, wrapped in the crate's `FaultStore` (`test_support`) where a test needs to count or fail requests. Add a `get_opts` call counter to `FaultStore` (`pub(crate) fn gets(&self) -> u64`): `head`, `get_range` and the provided `get_ranges` all go through `get_opts`, so it counts every read.

- `a_range_is_served_from_blocks_after_the_first_read`: a 3.5 MiB object under `tables/…`; read `[1 MiB − 10, 2 MiB + 10)` (blocks 0, 1 and 2): correct bytes, `misses = 3`; read it again: same bytes, no inner `get_opts` call (count them), `hits = 3`.
- `the_last_block_is_short_and_ranges_past_the_end_are_clamped`: read `[3 MiB, 4 MiB)` of the 3.5 MiB object: 512 KiB returned; `GetResult.range` is `3 MiB..3.5 MiB`.
- `a_start_past_the_end_is_the_stores_error`: `[4 MiB, 5 MiB)` returns the inner store's error.
- `media_and_whole_reads_pass_through`: `media/…` ranges and a `get_opts` without a range never touch the cache directory.
- `a_corrupt_block_is_fetched_again`: flip a byte in a block file; the next read is correct, counts a miss, and rewrites the block.
- `eviction_keeps_the_cache_under_capacity`: capacity 2 blocks (+ CRC bytes); read 3 blocks, then the first again: the first was evicted (miss), the cache holds 2 blocks.
- `the_index_survives_a_reopen`: read blocks, drop, `ReadCache::open` again: the same reads are hits.
- `a_delete_evicts_the_file`: `delete` through the `CacheStore` removes `cache/<name>/`.
- `nothing_is_written_while_the_disk_is_low`: fake free under `2 × min_free`: reads succeed, no block file appears.
- `shed_frees_the_requested_bytes`.
- In `lib.rs`, with the S3 test store (`Storage::open_with_store`, as the M2 S3 tests do): `a_flushed_file_is_cached_on_write`: after a flush, `read_cache_stats().blocks > 0`; a query then reads with `misses = 0`. And `verification_reads_the_store_not_the_cache`: corrupt a cached block's bytes **and** its CRC consistently (rewrite the block file with a new valid CRC over altered bytes), suspect the file, `verify_pass`: `healthy = 1`.

---

## M6.6 — Benchmarks

Commit: `storage: benchmark ingest, compaction and S3 reads (M6.6)`

### What it does (D-A1-12)

The benchmarks are `#[ignore]` integration tests, like `crates/storage/tests/trace_bench.rs` (M3): run by hand in `--release`, results written to `docs/research/storage/benchmarks.md`. There are no criterion micro-benchmarks and no `tools/bench-storage` crate (D-A1-12 replaces that line of `00-design.md` § Testing): the engine's internals are `pub(crate)`, so a micro-benchmark would need them made public, and the end-to-end numbers are what spec 03's capacity check is about.

### Ingest benchmark (`crates/storage/tests/ingest_bench.rs`, new)

```bash
LLMOBS_INGEST_BENCH_DIR=/path/to/empty/dir cargo test --release \
    -p llmobs-storage --test ingest_bench -- --ignored --nocapture
```

Environment:
- `LLMOBS_INGEST_BENCH_DIR` (required): a directory it empties first.
- `LLMOBS_INGEST_BENCH_SECS` (default 60): how long to ingest.
- `LLMOBS_INGEST_BENCH_WRITERS` (default 8): concurrent appending tasks.
- `LLMOBS_TEST_S3_ENDPOINT` (+ `LLMOBS_TEST_S3_BUCKET` and the `AWS_*` variables), as `tests/s3.rs` reads them: when set, the object store is `s3://<bucket>/ingest-bench-<uuidv7>`, with the S3 settings exactly as `tests/s3.rs` builds them (copy that code, including `allow_http = endpoint.starts_with("http://")`). At the end, delete the prefix with a copy of `delete_prefix` from `tests/s3.rs`.

Data: spans built with `llmobs_core::span::Span` and `llmobs_core::batch::spans_to_record_batch`, as `trace_bench.rs` does. A seeded xorshift (copy `Rng` from `trace_bench.rs`). Batches of 1,000 spans, 10 spans per trace, `ingest_time` and `start_time` = now. Span sizes follow spec 03's mix: 10% "LLM" spans whose input and output are 10 KiB each of ASCII text, 90% spans with 300-byte attributes and no input/output (about 3 KiB average once normalized). Print the measured average `get_array_memory_size()` per span.

Config: defaults, except `min_free_bytes = 0`, `flush.max_bytes = 256MiB`, `memtable.hard_limit = 1GiB`.

Steps and printed results:
1. **Ingest**: `WRITERS` tasks append until `SECS` elapse. `Backpressure` is counted and retried after 10 ms. Print spans/s, MiB/s (Arrow bytes), backpressure count.
2. **Drain**: `shutdown()` flushes; time it. Then reopen.
3. **Compaction**: call `compact(now)` with `now` = the latest ingest time + 3 h (so day close is due) in a loop until a pass reports `jobs = 0`. Print total time, input and output bytes (from `CompactionReport`), MiB/s of input, and write amplification = (bytes written by flush + compaction outputs) / bytes written by flush. Bytes written by flush = the size of the object directory (local) or the sum of listed object sizes (S3) before compaction.
4. Print the machine description the way `trace_bench.rs` does (copy `machine()`).

### Query benchmark on S3 (`crates/storage/tests/trace_bench.rs`)

Add S3 support to the existing harness:
- When `LLMOBS_TEST_S3_ENDPOINT` is set, the object store is `s3://<bucket>/trace-bench` (fixed prefix, so the generated dataset is reused across runs, like the local directory), with the S3 settings copied from `tests/s3.rs`. The manifest records the endpoint and bucket; a mismatch, or no manifest, regenerates: it first deletes everything under the `trace-bench` prefix (a copy of `delete_prefix` from `tests/s3.rs`) and empties the local directory.
- `LLMOBS_TRACE_BENCH_CACHE` = `cold` (default with S3), `warm` or `off`:
  - `cold`: delete `<dir>/cache` before opening; the first run of each query is the cold sample (S3 and empty cache), the 20 later runs are warm;
  - `warm`: keep `<dir>/cache`; first run the whole query set once untimed, then measure;
  - `off`: `query.read_cache_bytes = 0`: every run reads S3.
- After each query set, print `read_cache_stats()`.
- Local runs are unchanged.

### Results (`docs/research/storage/benchmarks.md`, new)

Record, for each run: the command, the machine, the dataset size, and the tables printed. Runs:
1. ingest, local disk;
2. ingest, S3 (Versity Gateway, below);
3. trace_bench, local disk (`LLMOBS_TRACE_BENCH_TRACES=1000000`, or the default 5,000,000 if the machine generates it within an hour);
4. trace_bench, S3, `cold`;
5. trace_bench, S3, `warm`;
6. trace_bench, S3, `off`.

Then a section **"Against the capacity check"** comparing with spec 03 § Capacity sanity check, number by number: ingest at 50k spans/s of ~3 KiB, the flush cadence at 256 MiB, compressed output of 20–30 MiB/s. State each as measured / not reached / not measured. Don't tune the engine in this sub-phase: report what was measured.

**S3 server.** The user starts it; the implementing agent does not run docker. Before runs 2, 4, 5 and 6, stop and ask the user to start Versity Gateway with the command in `crates/storage/tests/s3.rs` (with a larger `--tmpfs` size: `size=20g`, or a directory volume) and to give the `LLMOBS_TEST_S3_*` and `AWS_*` values. Do the local runs first.

A cold-cache number against a local S3 server understates real S3 latency: say so in the results.
