# Parseable storage layer — research notes for llmobs spec 03

Repo: `/home/sebastien/travaux/parseable`, HEAD `503fa56` (v3.2.1), last commit **2026-09-18**. Same dependency family as ours: DataFusion 55, arrow/parquet 59.2, object_store 0.13.2 (`Cargo.toml` lines 12-30). All paths below are relative to `src/`.

Shape in one paragraph: per-stream (= dataset/table), per-node **staging directory** of Arrow IPC stream files (`.arrows`), one file per (schema-hash, minute, custom partition values, host). Once a minute a "local sync" closes the previous minute's writers and converts each minute group into one Parquet file in staging. Every 30 s an "object store sync" uploads staged Parquet files, appends their stats to a per-day, per-node **JSON manifest** (zstd) on the object store, then updates the stream's **snapshot** (the list of manifests) in `stream.json`, then deletes the local file. Queries union an in-memory buffer + staged Parquet (only if the query touches the last 5 min) + optional hot-tier local copies + object-store Parquet chosen via manifest stats. There is **no WAL with checksums, no fsync, no compaction, and no row-level delete**.

---

## 1. Write path

- **Ack point.** The HTTP handler runs `Event::process` synchronously (`event/mod.rs:76-129`), which calls `Stream::push` (`parseable/streams.rs:695-744`). Under a per-stream `std::sync::Mutex<Writer>` it appends the RecordBatch to the minute's Arrow IPC file and to an in-memory buffer, then returns; the HTTP 200 follows. No queueing, no group commit.
- **Format/framing.** Plain Arrow IPC *stream* format, LZ4-frame compressed (`parseable/staging/writer.rs:139-167`). Framing is just IPC message lengths; **no checksums, no sequence numbers**. File name encodes schema hash, `date=/hour=/minute=`, custom partition values and hostname (`streams.rs:746-777`). A distinct field set (schema key = xxh3 of sorted field names, `event/mod.rs:148-156`) gets its own file, so schema churn multiplies files.
- **Durability.** After each batch the writer flushes its `BufWriter` into the kernel (`writer.rs:174-183`) — added in Sept 2026 (`8431226`) because a process crash used to lose the userspace buffer. **There is no `fsync`/`sync_data` anywhere in staging or conversion**; power loss loses whatever the page cache had.
- **Rotation.** Time-based: one file per minute slot (`OBJECT_STORE_DATA_GRANULARITY` = 1 min, `lib.rs:86-92`). Files are written as `*.arrows.part`; on `Drop` the writer writes the IPC end-of-stream marker and renames to `.arrows` (`writer.rs:186-211`). `take_flushable_disk` detaches writers whose minute is over (`writer.rs:116-128`).
- **Recovery.** At startup `recover_orphan_part_files` (`streams.rs:2557-2675`) scans `.part` files: empty → delete; Parquet magic → delete (rebuildable); otherwise scan IPC message boundaries, drop a truncated tail (`staging/reader.rs:483-538`), and if at least one complete batch exists rename to `.arrows`, else delete. Startup snapshots the `processing_*` dirs left by a crash and converts them oldest-minute-first, sequentially, to bound memory (`streams.rs:2680-2707`, `sync.rs:347+`, commit `396aac8`).
- **Cleanup.** Conversion moves eligible `.arrows` into a fresh `processing_<minute>` dir (atomic `mkdir` reservation, `streams.rs:950-1009`), writes `<name>.parquet.part`, validates it by re-opening the footer and checking `num_rows > 0` (`streams.rs:2162-2202`), renames to `.parquet`, then deletes the source Arrow files (`streams.rs:1864-1930`, `2205-2268`). No fsync before the rename or before deleting sources.
- **Corruption policy.** A file that fails structural validation or Arrow decode is **deleted whole** (`streams.rs:2271-2288`, `1903-1917`); the rest of the group proceeds (`da78e06`, `8431226`).

## 2. In-memory buffer

- `MemWriter` (`writer.rs:214-318`): per stream, a "mutable" list of batches that is concatenated into the read buffer every 4096 rows; schema is the running merge of every schema key seen. Unbounded until cleared. Toggle `ENABLE_MEMORY_STAGING` (default true, `writer.rs:49-58`).
- **It is cleared on every local-sync flush** (`streams.rs:1168-1189`), including rows of the still-open current minute, which then live only in the `.arrows.part` file that queries never read.
- **Flush trigger:** wall clock only (60 s tick aligned to the minute, `sync.rs:238-300`); no size trigger, no global memory accounting, no backpressure tied to staging size. Up to `MAX_PERIODIC_LOCAL_SYNC_CYCLES` (default 2) cycles can overlap (`sync.rs:39-60`); periodic conversion runs 2 minute-groups at a time per stream (`streams.rs:1795-1815`).
- **Backpressure:** only a host-wide RSS check every 5 s that turns every request into 503 for `rejection_duration` (`handlers/http/resource_check.rs`); default threshold was raised to 100 % (`d309245`), i.e. effectively off. Staging disk is unbounded (`max_disk_usage` only guards the hot tier, `hottier.rs:1435-1466`). If S3 is down, staging Parquet grows forever.
- **Queries on unflushed data:** `get_staging_execution_plan` (`query/stream_schema_provider.rs:399-459`) clones the mem buffer into a reversed `MemTable` and scans staging `.parquet` files as one partition — but only when the query's upper bound is within the last 5 minutes (`is_within_staging_window`, `:1064-1088`, commit `3e02f29`). `.arrows` files are never queried.

## 3. Parquet files

`parquet_writer_props` (`streams.rs:1191-1266`):
- Row group: `max_row_group_row_count` = **262 144** rows (reduced from 1 M "for faster multi-threaded query", `cli.rs:474-482`).
- Compression: **LZ4_RAW** default, configurable (`cli.rs:493-499`).
- Time column encoded `DELTA_BINARY_PACKED`; custom partition columns `DELTA_BYTE_ARRAY`.
- Page size, dictionary, statistics: parquet-rs defaults (1 MiB pages, dictionary on, page-level stats → page index written).
- Bloom filter: only on `metric_name` for OTel-metrics streams (ndv 32768, position after row group, `streams.rs:1226-1240`). **No bloom filter on trace id.**
- Sort: `SortingColumn` metadata says time DESC (metrics: `metric_name` ASC, time DESC). Rows are actually ordered by k-merging IPC readers in reverse by each batch's first timestamp and reversing each batch (`staging/reader.rs:254-279`) — only truly sorted because `p_timestamp` (server receive time) is constant within a batch. Metrics get an explicit lexsort per row group (`streams.rs:1288-1323`). The query side only advertises ordering to DataFusion after proving file ranges don't overlap ("a single false claim here can produce incorrectly ordered query results", `stream_schema_provider.rs:220-262`).
- Target file size: none. **One Parquet per stream × minute × node** (× custom partition values), split into extra files when a minute has > 512 Arrow files (`streams.rs:181-228`).
- Object path: the first three dots of the staging name become slashes → `<tenant>/<stream>/date=YYYY-MM-DD/hour=HH/minute=MM/[k=v/]<host>.data.<ulid>.parquet` (`storage/object_storage.rs:1365-1384`).

## 4. Catalog / manifest

- **Two levels, both JSON objects on the object store** (`catalog/mod.rs`, `catalog/manifest.rs`, `catalog/snapshot.rs`):
  - `stream.json` (one per node in distributed mode) contains `snapshot.manifest_list`: entries `{manifest_path, time_lower_bound, time_upper_bound (one UTC day), counters}`.
  - `<stream>/date=D/<host>.manifest.json`: list of `File {file_path, num_rows, file_size, ingestion_size, columns[{name, min/max stats, null_count}], sort_order}` (`manifest.rs:144-176`), built by re-reading the local Parquet footer (`manifest.rs:193+`).
- **Commit protocol** (`object_storage.rs:1062-1098`, `catalog/mod.rs:109-400`): upload each Parquet (≤100 concurrent) → HEAD to compare size (`object_storage.rs:283-319`, `48f97e7`) → GET this node's day manifest, upsert entries by `file_path` (`apply_change`, idempotent on retry) → PUT manifest → PUT `stream.json` if a new day manifest was created → delete local files. Not atomic, but ordered so a crash leaves at worst an uploaded-but-unlisted object that is re-uploaded under the same key next cycle. One failed upload aborts the stream's whole cycle (`object_storage.rs:1209-1244`).
- **Single writer per manifest** (each node owns its own manifest file name) instead of conditional PUTs. Bug `#1739` (`dc189de`, Aug 2026): ownership was matched by substring, so `node` matched `prod-node`, overwrote another writer's manifest and orphaned every earlier Parquet; fix compares the exact last path segment (`object_storage.rs:1690`, comment at `catalog/mod.rs:266-273`) and shipped a 935-line recovery script.
- **Scale pain:** the day manifest is rewritten in full every 30 s and holds stats for every column of every per-minute file. They had to zstd it (`cac761e`, 15-25×, `manifest.rs:52-64`), decode on the blocking pool with a semaphore because one decode is "hundreds of milliseconds" (`manifest.rs:66-130`), fetch large manifests with ranged GETs pinned by ETag because "these objects are rewritten while being read" and ranges could splice two versions (`storage/mod.rs:90-250`), and parallelize pruning + dropping of tens of thousands of `File` structs (`stream_schema_provider.rs:690-711`).
- **Consistent snapshot:** none. A query reads `stream.json`, then each manifest at its own time; a missing manifest is logged and skipped (`stream_schema_provider.rs:668-675`).
- **Orphans:** no GC of unlisted objects. Locally, `UPLOADED_STAGED_FILES_PENDING_CLEANUP` prevents re-uploading files whose local delete failed (`object_storage.rs:68-94`, `1300-1344`).
- **LocalFS backend writes metadata non-atomically:** `put_object` is a direct `fs::write` (no temp+rename, no fsync, `storage/localfs.rs:386-398`); `upload_multipart` reads the whole file into memory first (`:133-143`).

## 5. Compaction

**None.** `grep -i compact` finds only comments; the git log has no compaction commit. Small files are mitigated only by merging all Arrow files of a minute into one Parquet (`649c000`, `chunk_arrow_file_groups`). Result: ≥1440 files/stream/node/day, all listed in an ever-growing manifest — the root cause of the manifest-scale work above.

## 6. Query

- **Session:** one global `SessionContext` (`query/mod.rs:101-106`) with one DataFusion schema per tenant (`:239-251`). Config (`:260-345`): `pushdown_filters`, `reorder_filters`, page index, `schema_force_view_types`, `binary_as_string`, dynamic filter pushdown + TopK, file-stream work stealing, `metadata_size_hint` 2 MiB (footers of wide streams are 1-2 MiB), predicate cache 1 GiB, and DataFusion's **built-in** file metadata cache (`with_metadata_cache_limit`, 50 MB default, `cli.rs:39,466-471`).
- **Limits:** one shared memory pool for all queries, default 85 % of host/cgroup RAM (`query/mod.rs:266-281`); spill via default `DiskManager`; per-query timeout `sql_timeout` default 300 s (`query/mod.rs:200-218`); admission control waits up to 10 s for host memory to drop below a threshold, else 503 (`d9de7fb`, `query/mod.rs:170-196`); `target_partitions` default 4 × cores (`cli.rs:221-229`).
- **`StandardTableProvider::scan`** (`stream_schema_provider.rs:744-882`): read `stream.json` → extract time bounds from filters → (staging plan if within 5 min) → filter manifest list by day bounds → fetch manifests (16 concurrent) → prune files by per-file column min/max for simple `col op literal` filters (`:1163-1300`) → apply LIMIT at file level using `num_rows` → subtract files present in the hot tier and scan them from local disk → build a `ParquetSource` scan over the rest with file groups balanced by bytes (LPT scheduling, `:173-206`) and aggregate statistics → `UnionExec` of everything (`:489-506`). Filters on the time column at minute boundaries are reported `Exact`, others `Inexact` (`:888-906`, `:1090-1106`).
- **Known races, admitted in code:** a staged Parquet can be uploaded and deleted between listing and open (commented-out note at `:437-444`), and during the window between manifest commit and local delete a file is visible in both staging and manifest (double count). Data being converted (mem cleared, Parquet not yet written) is invisible.

## 7. Deletes and retention

- **No row-level delete** of any kind. Only whole-stream deletion and day-granular retention.
- **Retention** (`storage/retention.rs:43-100, 211-260`): daily at 00:00 (and at startup), list `date=` prefixes, remove those days from the snapshot first, then `delete_prefix` each day. No grace period for in-flight queries.
- **Stream delete** (`handlers/http/logstream.rs:52-94`): object-store delete, then local staging, hot tier, memory. Since `6459688` a `.tombstones/<tenant>/<stream>/.tombstone` marker makes the deletion resumable after a crash and hides the stream (`object_storage.rs:1500-1570`, `parseable/mod.rs:484`).

## 8. Tenancy

Directory prefix everywhere: staging `<staging>/<tenant>/<stream>` (`cli.rs:720-727`), objects `<tenant>/<stream>/…`, tombstones per tenant; DataFusion schema name = tenant. Isolation relies on the catalog lookup scoping (`GlobalSchemaProvider` built per tenant) plus RBAC on table names. Streams map is `tenant → stream → Arc<Stream>` (`streams.rs:2759-2795`).

## 9. Local disk vs object storage

- Own trait `ObjectStorage` (`storage/object_storage.rs:334-470`) with S3/GCS/Azure built on `object_store` (wrapped in `LimitStore` + a metrics layer, `storage/s3.rs:354-380`) and a hand-written `LocalFS` (not `object_store::LocalFileSystem`). Staging always local even with `drive` mode, so local mode copies every file twice.
- **Hot tier** (`hottier.rs`, 2 182 lines, query nodes): per-stream size budget, downloads newest manifest files first with chunked ranged GETs, evicts oldest, persists a local checkpoint (`hottier/local_state.rs`). It replaced an older "local cache" + "query cache" removed in `5ce9d2b` (1 453 lines deleted).

## 10. Concurrency

Separate tokio runtimes for sync (`FLUSH_AND_CONVERT_RUNTIME`, `sync.rs:35`) and queries (`QUERY_RUNTIME`, `query/mod.rs:98`); conversion in `spawn_blocking` + a rayon pool for column encoding; per-stream std `Mutex<Writer>` held across the disk write (so ingest into one stream is serialized and does blocking I/O on an async worker); semaphores: object-store sync single-flight, local sync ≤ 2 overlapping, upload ≤ 100, manifest decode ≤ cores. Data loss bug `1afa318` (2025): two concurrent first-writes created two `Stream` objects for the same name and one writer's data was dropped — fixed with get-or-create under the map's write lock (`streams.rs:2771-2795`).

## 11. Failure handling and testing

Unit tests only (54 files with tests): truncated-tail IPC recovery (`staging/reader.rs:713-770`), orphan `.part` recovery, corrupt bodies removed without blocking other groups, startup grouping across processing dirs, concurrent `get_or_create` (`streams.rs:2958-4263`). No fault injection, no fsync/power-loss tests, no concurrency stress tests of query-vs-sync, no benchmarks in-repo (only an optional `hotpath` profiling feature, `Cargo.toml:101,232`). Integration testing is docker-compose based (`docker-compose-*-test.yaml`).

## 12. Lessons from the git log

| Commit | Date | What happened | Lesson for us |
|---|---|---|---|
| `50ec933` Fix-78-Corrupt-Parquet-S3, `7f7d0f2` 0-sized parquet, `6346928` write `.part` then rename | 2022–2025 | Zero-length / invalid Parquet uploaded or left in staging | Classic symptom of rename-without-fsync and no validation. Write temp → fsync → rename → fsync dir before deleting inputs. |
| `48f97e7` validate before/after upload | 2025-09 | Size mismatch after upload | Verify after PUT (size/ETag) before committing to catalog. |
| `da78e06`, `8431226` corrupt staging files | 2024–2026 | One bad Arrow file panicked/blocked the sync of everything; later they delete the whole file | Per-record CRC so damage is contained to records, not files; never let one bad unit block the pipeline. |
| `8431226` flush after each batch | 2026-09 | Acked batches sat in `BufWriter` and were lost on process crash | Ack only after bytes are at least in the kernel — ideally after fdatasync (our group commit). |
| `cb94785`, `396aac8` startup backlog | 2026 | `.part` files ignored at startup; then backlog conversion starved the server | Replay must be bounded (oldest first, sequential) and must not block new ingest. |
| `ebc48eb`, `cfd1348`, `94460c3` | 2025–2026 | Conversion bursts ate all cores; overlapping cycles raced on the same files | Bounded flush/compaction parallelism and explicit ownership of inputs (they used atomic dir reservation). |
| `1afa318` data loss at stream creation | 2025-03 | Racing creation lost one writer's data | Get-or-create of per-tenant state must be atomic. |
| `dc189de` / issue #1739 manifest overwrite | 2026-08 | Substring ownership match overwrote another node's manifest, orphaning files | Read-modify-write of a shared metadata blob is fragile; a transactional catalog (SQLite) avoids the whole class. |
| `cac761e` zstd manifest + ranged/ETag reads | 2026-07 | Manifests got huge and slow to decode; torn reads during rewrite | Per-file stats for every column × minute-files × days does not scale as JSON; keep the catalog small and indexed. |
| `4d28d23` remove in-memory ingestion (2023) → `236c3ec`/`3e02f29` re-add mem buffer for last 5 min (2025) | — | Memory instability, then freshness complaints | An in-memory read path is needed, but it must be bounded and accounted. |
| `5ce9d2b` deprecate caching → hot tier | 2024-12 | Two caches replaced by one size-budgeted tier | One cache with a byte budget; eviction by age. |
| `e8323ad`, `d9204c2`, `f240dde` schema conflicts | 2025–2026 | Dynamic per-attribute columns hit type conflicts (renamed `x_utf8`, `x_bool`) and a 250→1000 column cap | Don't promote arbitrary attributes to columns. |

## 13. Recommendations for llmobs (mapped to spec 03)

**§1 WAL — keep our design; Parseable shows the cost of skipping it.** No CRC, no fsync, whole-file deletion on corruption, and data invisible during conversion are all things our WAL + CRC + group-commit + `flushed_seq` design avoids. Adopt: the *truncated-tail detection by declared length vs file length* (`staging/reader.rs:483-538`) is the same idea as our "short read at the tail = torn write" rule — keep a unit test for "complete records before a torn tail are recovered". Adopt: replay oldest-first and bounded.

**§2 Memtable/Flush — confirmed and one gap.**
- Our step 3–4 atomic swap (frozen stays visible until its files are in the snapshot) fixes exactly the visibility hole Parseable has (mem cleared before Parquet exists; double visibility between commit and local delete). Keep it, and add a test that a query running during flush sees each row exactly once.
- **Missing in spec: fsync of the Parquet output before the SQLite commit that advances `flushed_seq`** (and of its directory). Our pinned `object_store` 0.13 `LocalFileSystem` never fsyncs (0.14 adds `with_fsync`, see `object_store-0.14.2/src/local.rs:308-322`). Without it, a power loss after commit can leave a zero-length Parquet with its WAL already deleted — Parseable's `7f7d0f2` symptom. Spec should say: local writes go through our own temp-file + `sync_all` + rename + dir fsync (or we wait for object_store 0.14 with the family bump); for S3 a completed PUT is durable.
- Size-based triggers and a hard memory limit with 429 are right; Parseable's host-RSS polling is a poor substitute. Also bound disk: §10 already has `min_free_bytes`; add that the WAL backlog itself is bounded when the object store is down (Parseable's staging grows forever).

**§3 Parquet files.**
- Consider row groups smaller than 64 MiB/100k for scan parallelism — Parseable cut 1M→262k rows for "faster multi-threaded query". With page index + sort order our 100k rows is fine; keep it configurable.
- Bloom filter on `trace_id` is essential: Parseable stores trace id as hex text with no bloom and resolves a trace with a two-step query over the user's time range (`handlers/http/traces.rs:256-325`). Store IDs as `FixedSizeBinary(16/8)`, not hex strings.
- Only declare `sorting_columns` when the writer actually sorted; Parseable's ordering claim depends on an accident (constant timestamp per batch). Add a debug assertion / test that the written order matches the declared order.
- Their footers reach 1–2 MiB on wide schemas; set `metadata_size_hint` accordingly. Our fixed, narrow schema with a single `attributes` map column keeps footers small.

**§4 Catalog.** Strong confirmation of SQLite over object-store JSON manifests (§12 rows on #1739 and zstd). Notes: (a) DataFusion 55 has a built-in file metadata cache in `RuntimeEnv` (`with_metadata_cache_limit`); our custom `ParquetFileReaderFactory` may only be needed for bloom filters — check before building it. (b) Record per-file `min/max` only for the columns we prune on (time, ingest time), not every column — their per-column stats are what bloated manifests.

**§5 Compaction.** Parseable has none and pays for it in file counts and manifest size. Our L0→L1 policy is needed. Their per-minute files are what we would get from `flush.max_age` alone at low volume; the "newest L0 older than 10 min" trigger is the right guard.

**§6 Query path.** Adopt: balance file groups by bytes (LPT) and preserve order inside groups; advertise output ordering only when proven by non-overlapping file stats; enable `pushdown_filters`, `reorder_filters`, page index, dynamic filter pushdown/TopK; `schema_force_view_types`. Avoid: one global session/memory pool shared by all queries, a 5-minute heuristic deciding whether unflushed data is scanned (always include memtable batches whose time range overlaps), and listing local files at plan time (use the snapshot). Admission control before execution (they wait ≤10 s then 503) is worth copying in addition to per-query pools.

**§7 Deletion.** Nothing to learn for row deletes (they have none). Their stream tombstone marker written *before* deleting and removed after, discovered by listing at startup, matches our `deleting` state for project/org deletion; make sure our project deletion is resumable after crash in the same way.

**§8 Retention.** Same day-drop model. Their retention deletes objects immediately after the snapshot update; our `gc.grace` before physical delete is better (queries on an older snapshot keep working). Note day-granular retention by `start_time` day vs Parseable partitioning by *ingest* minute: theirs never needs late-data handling; ours gets late spans into old days (fine, but retention of a day must also drop late L0 files there — it does, since it's by partition).

**§10 Failure handling.** Add rows: "Parquet write succeeded but upload size/etag mismatch → don't commit, retry" (their `48f97e7`); "staging/WAL backlog while object store is down" with an explicit cap; "startup replay must not starve ingest". Add the missing durability test: kill -9 and simulated power loss (drop page cache / fault-injecting fs) between Parquet write, SQLite commit and WAL deletion.

### OTel span → columns, and lessons for our span schema
Flattening (`otel/traces.rs:70-433`, `otel/otel_utils.rs:32-100`): protobuf → `serde_json::Map` per span → arrow-json inference. Each span row gets fixed columns (`span_trace_id`/`span_span_id`/`span_parent_span_id` as **hex strings**, `span_name`, `span_kind` + `_description`, start/end as RFC3339 strings plus `_epoch` columns, `span_duration_ns`, status code/description/message, flags, dropped counts, `scope_*`, `resource_*`), and **every resource, scope and span attribute becomes its own top-level column** named by its key; event and link attributes get `event_`/`link_` prefixes. Spans with N events are **exploded into N rows** (span fields copied onto each), so every trace query needs `GROUP BY span_span_id` dedup (`handlers/http/traces.rs:567-606`). Int attributes are stringified, arrays JSON-encoded, nested kvlists flattened. Time partition is `p_timestamp` = server receive time, not span start.

Lessons: (1) our fixed schema with typed `trace_id/span_id` binary columns, events/links as nested lists, and attributes in one map/JSON column is the right call — Parseable's dynamic columns led to type-conflict renames (`x_utf8`), column caps (1000) and per-schema-key file splits; (2) keep a small set of promoted, typed columns (`user_id`, `session_id`, model, tokens, cost) as we do; (3) never explode child records into parent rows; (4) keep both `start_time` and `ingest_time` — Parseable only has ingest time for partitioning, which is simpler for late data but makes "traces in this time range" depend on arrival time.
