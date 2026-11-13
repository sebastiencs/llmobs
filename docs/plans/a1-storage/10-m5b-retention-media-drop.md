# A1 · M5b — Retention, media, project drop, and M5's crash and model tests

Second half of milestone M5, **Deletion, retention, project drop, media**. It needs `09-m5a-deletion.md` done, through M5.4. What it adds:
- `apply_retention`, with a watermark that stops late rows from recreating a dropped day;
- media blobs: `put_media`, `get_media`, and their GC;
- `drop_project`;
- the crash and model tests that are M5's exit criteria.

The conventions of `09-m5a-deletion.md` § Conventions apply unchanged, with these differences:
- **Dependency:** M5.6 adds `sha2 = { workspace = true }` to `crates/storage/Cargo.toml`. `sha2` 0.11.1 is already in the root `[workspace.dependencies]`; nothing changes there.
- **Allowed edits outside `crates/storage/` and `docs/`:** `justfile` in M5.8 (one line per new crash parent). M5b adds no `StorageConfig` field.

**M5 exit criteria** (`00-design.md` § Milestones):
- deleted rows vanish from queries at once (M5.2) and from files within `deletion.max_delay` (M5.4);
- the race test "a compaction started before a deletion commits after it, and the rows stay gone" passes (M5.4);
- crash tests cover deletion, retention, media and project drop (M5.8);
- the model test covers deletion and retention (M5.8).

## Shared pieces

### Committer: one commit path with extra SQL

Retention (M5.5) and project drop (M5.7) remove files **and** write other tables in the same transaction. Don't copy `Committer::commit`. Refactor it once, in M5.5:

```rust
/// What a commit writes besides the file edit, inside the same transaction.
enum Extra {
    None,
    /// M5.5: `INSERT OR REPLACE` the watermark (already the max of old and new).
    Retention { project: ProjectId, keep_from: NaiveDate },
    /// M5.7 adds this variant.
    Drop { project: ProjectId, now: i64 },
}

async fn commit_inner(inner: &mut CommitterInner, edit: &CatalogEdit, extra: &Extra)
    -> Result<Arc<Catalog>, CommitError>;
```

`commit_inner` is today's body of `commit` up to and including the SQLite commit and the in-memory updates (`retired`, `flushed`, `inner.catalog`), with the `Extra` SQL run after `write_edit` and before the `CatalogCommit` hook. It does not publish. `commit` becomes: lock, `commit_inner(&mut inner, edit, &Extra::None)`, `publish(&next)`. M5.5 adds only the `None` and `Retention` variants; M5.7 adds `Drop`.

### Project states (`src/projects.rs`, new, `pub(crate) mod projects;`)

```rust
/// Storage-side project state readers and jobs need: retention watermarks
/// (M5.5) and dropped projects (M5.7).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProjectStates {
    keep_from: HashMap<ProjectId, NaiveDate>,
}

impl ProjectStates {
    /// The project's retention watermark, if any.
    pub(crate) fn keep_from(&self, project: ProjectId) -> Option<NaiveDate>;
    /// A copy whose watermark for `project` is `max(current, keep_from)`.
    pub(crate) fn with_keep_from(&self, project: ProjectId, keep_from: NaiveDate) -> Self;
    /// Whether rows of `project` on `day` must be dropped.
    pub(crate) fn drops(&self, project: ProjectId, day: NaiveDate) -> bool;
}
```

`drops` is `keep_from(project).is_some_and(|keep| day < keep)` in M5.5; M5.7 adds `|| dropped`. `ReadState` gains `pub projects: Arc<ProjectStates>` in M5.5; every `ReadState` literal copies it from the current state, except `ReadState::empty()` (default) and `open_inner` (`committer.project_states().await?`, below).

---

## M5.5 — Retention

**Goal:** `apply_retention(project, keep_from)` obsoletes every file of the project in days before `keep_from`, in one commit, and records the watermark; from then on no row of an earlier day can be flushed or seen (spec 03 §8).

### Migration `0004_retention.sql`

```sql
CREATE TABLE retention_watermarks (
    project_id TEXT PRIMARY KEY,
    keep_from TEXT NOT NULL
);
```

`keep_from` is `YYYY-MM-DD`, like `data_files.day`. Raise the two migration counts to 4 and add `DROP TABLE retention_watermarks` to step 2 of `migration_applies_to_an_m4_catalog` (M5.1 § Existing migration tests). M5.6 and M5.7 do the same for theirs.

### Committer

- `pub(crate) async fn project_states(&self) -> Result<ProjectStates, CatalogError>`: reads `retention_watermarks`. A bad project id or date is `CatalogError::corrupt("retention watermark")`.
- `pub(crate) async fn apply_retention<F>(&self, project: ProjectId, keep_from: NaiveDate, publish: F) -> Result<RetentionReport, CommitError> where F: FnOnce(&Arc<Catalog>, NaiveDate)`, under the mutex:
  1. `stored` = the project's current watermark (`SELECT keep_from FROM retention_watermarks WHERE project_id = ?`); `watermark = max(stored, keep_from)`.
  2. `files` = every active file of the project, in all three tables, with `day < watermark`, from `inner.catalog` (`Catalog::files(table, project, NaiveDate::MIN..=watermark.pred_opt())`; when `pred_opt()` is `None` there is no earlier day and `files` is empty).
  3. `edit = CatalogEdit { remove: files with FileState::Active, every other field empty }`. A `spans` file and its `traces` file share their day, so the pair check of `write_edit` holds.
  4. `commit_inner(&mut inner, &edit, &Extra::Retention { project, keep_from: watermark })`. `write_edit` marks the removed files `obsolete` with `obsoleted_at` = its own `now_ns()`, as for every commit; nothing changes there.
  5. `publish(&next, watermark)`.
  6. Return `RetentionReport { files: edit.remove.len(), bytes: Σ size, keep_from: watermark }`.
- **`write_edit` refuses late files.** For each added file: `SELECT keep_from FROM retention_watermarks WHERE project_id = ?`; a watermark with `file.day < keep_from` is `CommitError::conflict("file day is before the retention watermark")`. A flush or a compaction job that prepared its files before the watermark then fails with a conflict and retries.

### API (`lib.rs`)

```rust
/// What `apply_retention` removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionReport {
    /// Files obsoleted, all tables.
    pub files: u64,
    /// Their total size.
    pub bytes: u64,
    /// The watermark now stored: the later of the old one and the request.
    pub keep_from: NaiveDate,
}

/// Drops every partition of `project` before `keep_from` (a UTC day), and drops
/// late rows for those days from then on (spec 03 §8). A `keep_from` earlier
/// than the stored watermark changes nothing.
pub async fn apply_retention(&self, project: ProjectId, keep_from: NaiveDate)
    -> Result<RetentionReport, StorageError>;
```

- `StorageError` gains `Commit(#[from] CommitError)` and `Catalog(#[from] CatalogError)`. Its doc comment's second paragraph ("M1 only returns this from `shutdown`") becomes "Returned by `shutdown`, `apply_retention` and (M5.7) `drop_project`."
- The publish closure: `published.replace(|state| ReadState { catalog: Arc::clone(next), projects: Arc::new(state.projects.with_keep_from(project, watermark)), ..state.clone() })`.
- Log `info!(project = %project, keep_from = %watermark, files, bytes, "retention applied")` when `files > 0`.

### Flush

`flush_once` loads `projects` from the published state together with `tombstones` (M5.3), on every attempt. In `prepare`, a partition whose `projects.drops(key.project, key.day)` is skipped before anything else (no dedup, no file).

### Queries

`filter_snapshot` (M5.2) becomes `project_snapshot(table, snapshot, project, keep_from: Option<NaiveDate>, deletions)`: it also leaves out the project's partitions with `day < keep_from`. `Storage::query` builds the copies when the project has a memtable partition whose day is before its watermark, **or** pending deletions; otherwise it passes the snapshots unchanged.

Files need nothing: retention removed them from the catalog in the same commit.

**Known limit (spec 03 §8).** Memtable `spans` partitions are keyed by each row's own `start_time` day, while a flush writes them under the trace's anchor day (§ Trace anchor). A span within the anchor window of midnight, right at the watermark, can therefore be hidden in the memtable and kept by the flush, or the reverse. Nothing to build; the flush and the committer use the anchor day, which is what retention means.

### Tests (`src/retention.rs` test module, or `lib.rs` tests)

Days D−2, D−1 and D, with D = 2026-03-05. Each day gets spans (one `spans` and one `traces` file after a flush) and a score with `created_at` on that day.

1. **Drop old days:** `apply_retention(project, D−1)`:
   - the report counts the 3 files of D−2 and their sizes, and `keep_from = D−1`;
   - those files are `obsolete` in SQLite; `catalog_files` has none of them;
   - queries on `spans`, `traces` and `scores` return only D−1 and D rows;
   - `collect_obsolete(Duration::ZERO)` deletes the 3 objects.
2. **Watermark only rises:** `apply_retention(project, D−2)` afterwards reports `files = 0, keep_from = D−1`. After a reopen, the same.
3. **Late rows are dropped:** after the watermark, append a span and a score of D−2. A query does not show them (memtable). After a flush, no file of D−2 exists and the WAL is truncated.
4. **Flush race:** append a span of D−2; hold `FlushAfterWrite`; in another task, `apply_retention(project, D−1)`; release. The first commit fails with the watermark conflict, the retry drops the partition, and no file of D−2 is active.
5. **Compaction race:** two L0 files of D−2; hold `CompactionAfterWrite` on their L0 merge; `apply_retention(project, D−1)` commits; release: `compact` reports `failed = 1, jobs = 0`; `collect_staged(Duration::ZERO)` removes the job's staged outputs.
6. **Another project is untouched.**

**Commit:** `storage: apply retention watermarks`

---

## M5.6 — Media blobs

**Goal:** `put_media` stores a blob once per `(project, sha256)`, durably, with its row in `media`; `get_media` reads it, whole or a byte range; media GC deletes blobs no span references (spec 03 §9, D-A1-6).

### Migration `0005_media.sql`

```sql
CREATE TABLE media (
    project_id TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    size_bytes INTEGER NOT NULL,
    first_seen_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    PRIMARY KEY (project_id, sha256)
);
```

`sha256` is 64 lowercase hex characters, the form `media_refs` holds. Raise the migration counts to 5 and add `DROP TABLE media` to step 2 of `migration_applies_to_an_m4_catalog`.

### Types (`src/media.rs`, new, `pub(crate) mod media;`; public types re-exported from `lib.rs`)

```rust
/// One extracted media blob (spec 02 § Media).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaBlob {
    /// SHA-256 of `bytes`. `put_media` checks it.
    pub sha256: [u8; 32],
    /// MIME type, ASCII, 1 to 255 bytes.
    pub mime_type: String,
    pub bytes: Bytes,
}

/// `get_media`'s answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaRead {
    pub mime_type: String,
    /// Size of the whole blob, whatever the range.
    pub size: u64,
    /// The requested bytes.
    pub bytes: Bytes,
}

/// What one media collection did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MediaReport {
    pub deleted: u64,
    pub bytes: u64,
}
```

`MediaError` in `error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    #[error("invalid media: {reason}")]
    Invalid { reason: &'static str },
    #[error("media hash does not match its bytes")]
    HashMismatch,
    #[error("media not found")]
    NotFound,
    #[error("media range is outside the blob")]
    InvalidRange,
    #[error(transparent)]
    Object(#[from] ObjectError),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error("media query: {0}")]
    Query(#[from] datafusion::error::DataFusionError),
    /// An injected test fault.
    #[error("injected fault")]
    Fault,
}
```

M5.7 adds a `ProjectDropped` variant. Every variant gets a doc comment. `MediaError::Object` displays an object path, which holds the blob's hash, so media code never logs `%error`: it logs `error = error.label()`, where `MediaError::label(&self) -> &'static str` returns the variant name in snake case (`"invalid"`, `"hash_mismatch"`, `"not_found"`, `"invalid_range"`, `"object"`, `"catalog"`, `"query"`, `"fault"`, and `"project_dropped"` from M5.7). A `QueryError` from `project_query` (below) maps to `MediaError::Query` (`QueryError::DataFusion(error) => MediaError::Query(error)`; it has no other variant).

### Objects (`objects.rs`)

- `pub(crate) fn media_path(project: ProjectId, sha256_hex: &str) -> String` = `media/<project>/<sha256_hex>`.
- `pub(crate) async fn put_media(&self, path: &str, bytes: Bytes) -> Result<(), ObjectError>`:
  - local: `put_durable`; `ObjectError::AlreadyExists` is `Ok(())` (same content: the name is its hash);
  - remote: `put_remote` (an overwrite with identical bytes is harmless; no conditional put, so no S3 conditional-put setting is needed).
- `pub(crate) async fn get_range(&self, path: &str, range: Option<Range<u64>>) -> Result<Bytes, ObjectError>`: `store.get_opts` with `GetOptions { range: range.map(GetRange::Bounded), ..Default::default() }`, then `.bytes()`. Errors are `ObjectError::store("reading object", path, source)`. `GetRange::Bounded` holds a `Range<u64>` in `object_store` 0.13.
- `remove_temp_files` walks `root/tables` today (`objects.rs`, around line 402). Make it walk `root/media` too, with the same rule (names starting with `.` and containing `.tmp-`), so a crash during a local media write leaves no temp file with user bytes behind. A missing `media` directory is not an error.

### Committer

```rust
/// Refreshes `last_seen_at` to `max(last_seen_at, now)`. `true` when the row exists.
pub(crate) async fn touch_media(&self, project: ProjectId, sha: &str, now: i64) -> Result<bool, CatalogError>;
/// `INSERT OR REPLACE INTO staged_objects (path, created_at)`: two puts of one blob may race.
pub(crate) async fn stage_media(&self, path: &str, now: i64) -> Result<(), CatalogError>;
/// In one transaction: `INSERT OR IGNORE` the media row (`first_seen_at = last_seen_at = now`),
/// then `DELETE FROM staged_objects WHERE path = ?`.
pub(crate) async fn insert_media(&self, project: ProjectId, sha: &str, mime_type: &str, size: u64, path: &str, now: i64) -> Result<(), CatalogError>;
/// `(mime_type, size)`, or `None`.
pub(crate) async fn media_row(&self, project: ProjectId, sha: &str) -> Result<Option<(String, u64)>, CatalogError>;
/// Projects with at least one media row, in id order.
pub(crate) async fn media_projects(&self) -> Result<Vec<ProjectId>, CatalogError>;
/// Hashes of the project's rows with `last_seen_at < before`, in hash order.
pub(crate) async fn stale_media(&self, project: ProjectId, before: i64) -> Result<Vec<String>, CatalogError>;
/// Under the mutex, in this order:
/// 1. if the row is gone or has `last_seen_at >= before`, return `Ok(None)`;
/// 2. in one transaction, delete the `media` row and `INSERT OR REPLACE` a
///    `staged_objects` row for its path with `created_at = 0`;
/// 3. hook `MediaBeforeDelete` (pause, then fail → `MediaError::Fault`);
/// 4. run `delete` (the object);
/// 5. delete the `staged_objects` row.
/// Returns the bytes deleted. A crash after step 2 leaves a staged path with
/// no row, which staged-object GC deletes; never a row without its object.
pub(crate) async fn collect_media<F, Fut>(&self, project: ProjectId, sha: &str, before: i64, hooks: &Hooks, delete: F)
    -> Result<Option<u64>, MediaError>
where F: FnOnce() -> Fut, Fut: Future<Output = Result<u64, ObjectError>>;
```

**Staged-object GC must not delete a stored blob, nor a blob being written.** Two changes to `Committer::collect_staged_path`:
- It gains a `before: i64` parameter, third: `collect_staged_path(path, hooks, before, delete)`. Its caller in `gc.rs` (`collect_once`) passes the same `cutoff` it gave `staged_before`, and its tests pass a cut-off after their staging time; and its existing `SELECT 1 FROM staged_objects WHERE path = ?` becomes `... WHERE path = ? AND created_at < ?`. A `put_media` that re-staged the path since the listing (`INSERT OR REPLACE` sets a new `created_at`) is then left alone: return `StagedRemoval::Gone`.
- After the `data_files` check: when `path` has the form `media/<project>/<64 hex>` (split on `/`; anything else is not a media path) and `SELECT 1 FROM media WHERE project_id = ? AND sha256 = ?` finds a row, delete only the `staged_objects` row and return `StagedRemoval::Published`.

### `Storage::put_media` and `Storage::get_media`

```rust
/// Stores `blob` under `media/<project>/<sha256>` once. Writing an existing hash
/// refreshes its `last_seen_at` and writes nothing else.
pub async fn put_media(&self, project: ProjectId, blob: MediaBlob) -> Result<(), MediaError>;

/// The blob, or `range` of it (end exclusive).
pub async fn get_media(&self, project: ProjectId, sha256: &[u8; 32], range: Option<Range<u64>>)
    -> Result<MediaRead, MediaError>;
```

**`put_media`, in order:**
1. Validate: `mime_type` empty, longer than 255 bytes, or not ASCII → `Invalid { reason: "mime type" }`; empty `bytes` → `Invalid { reason: "empty blob" }`.
2. `sha2::Sha256::digest(&blob.bytes)` must equal `blob.sha256`, else `HashMismatch`.
3. `sha = hex::encode(blob.sha256)` (`hex` is already a storage dependency).
4. `touch_media(project, &sha, now)` → `true`: return `Ok(())`.
5. `path = Objects::media_path(project, &sha)`; `stage_media(&path, now)`.
6. `objects.put_media(&path, blob.bytes)`.
7. Hook `MediaAfterPut` (new; pause, then fail → `MediaError::Fault`).
8. `insert_media(project, &sha, &blob.mime_type, size, &path, now)`.

`now` is `catalog::now_ns()`, read once at the start.

**`get_media`:** `media_row` → `None` is `NotFound`. A range with `start >= end` or `end > size` is `InvalidRange`. Then `objects.get_range(&path, range)`; return `MediaRead { mime_type, size, bytes }`.

### Media GC (`media.rs`)

```rust
/// Blobs are kept this long after their last `put_media`, so a span appended
/// right after its blob is never raced (spec 03 §9).
pub(crate) const MEDIA_GRACE: Duration = Duration::from_secs(60 * 60);
/// Interval of the background media collection.
pub(crate) const MEDIA_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// What a collection needs. Cloned into the GC and compaction tasks.
#[derive(Clone)]
pub(crate) struct MediaGc {
    committer: Arc<Committer>,
    objects: Arc<Objects>,
    published: Arc<Published>,
    runtime: Arc<RuntimeEnv>,
    target_partitions: NonZeroUsize,
    lookback: i64,
    hooks: Hooks,
}

impl MediaGc {
    /// One project's unreferenced blobs.
    pub(crate) async fn collect(&self, project: ProjectId, now: i64) -> Result<MediaReport, MediaError>;
    /// Every project with media rows.
    pub(crate) async fn collect_all(&self, now: i64) -> Result<MediaReport, MediaError>;
}
```

Give `MediaGc` a manual `Debug` (it holds no secret; print nothing but the type name) if a `Debug` derive is needed by `Storage`.

**The live set.** Factor the body of `Storage::query` into a free function `pub(crate) fn project_query(state: &ReadState, project: ProjectId, options: QueryOptions, runtime: &Arc<RuntimeEnv>, target_partitions: NonZeroUsize, lookback: i64) -> Result<ProjectQuery, QueryError>` in `lib.rs`; `Storage::query` calls it with `self.published.load()`. `collect` calls it with `self.published.load()` and `QueryOptions::default()`, registers `query.spans` in a `SessionContext::new_with_config_rt(SessionConfig::new().with_target_partitions(n), runtime)`, and collects `SELECT media_refs FROM <table>` (use `ctx.read_table(Arc::clone(&query.spans))?.select_columns(&["media_refs"])?.collect().await?`). Every string in every `media_refs` list goes into a `HashSet<String>`. Rows hidden by tombstones don't count: the provider hides them.

**`collect(project, now)`:** `before = now - MEDIA_GRACE` (saturating). For each hash of `stale_media(project, before)` not in the live set: `committer.collect_media(project, &sha, before, &hooks, || objects.delete_len(&path))`; `Some(bytes)` adds to the report. Log `info!(project = %project, deleted, bytes, "media collected")` when `deleted > 0`. Errors are logged by the callers as `warn!(project = %project, error = error.label(), "media collection failed")`.

**`collect_all(now)`:** `collect` for each of `media_projects()`, summing the reports. M5.7 skips dropped projects.

**When it runs:**
- **GC task (`gc.rs`):** `GcStart` gains `media: MediaGc`. The task keeps `next_media: tokio::time::Instant`, starting at open + `MEDIA_INTERVAL`. On a tick at or after it, run `collect_all(now_ns())` after the staged and obsolete collections, log an error at `warn!` (no hash), and set `next_media` to now + `MEDIA_INTERVAL`.
- **After a deletion is applied (D-A1-6):** the compaction task gets a `MediaGc` (new `spawn` argument). After each `deletion_pass`, for each project in `report.applied_projects`, run `collect(project, now)` with the same `now` the pass used, and log an error at `warn!` (label only). `Storage::apply_deletions(now)` does the same before returning, **with its own `now` argument**, so tests control the media grace too. A media error there is logged and does not change the returned `DeletionReport`.
- **Hidden API:**

```rust
/// One media collection over every project at `now` (ns). The background task runs one daily.
#[doc(hidden)]
pub async fn collect_media(&self, now: i64) -> Result<MediaReport, MediaError>;
```

### Hook points

`MediaAfterPut` and `MediaBeforeDelete` (`COUNT` + 2).

### Tests (`media.rs`)

1. **Round trip:** `put_media` then `get_media` returns the bytes and MIME type; the object is at `objects/media/<project>/<hex>`; a range `2..5` returns those 3 bytes and the full `size`.
2. **Errors:** a wrong hash is `HashMismatch` and writes no object and no row; bad MIME types and an empty blob are `Invalid`; `get_media` of an unknown hash is `NotFound`; ranges `5..5` and `0..size+1` are `InvalidRange`.
3. **Idempotent:** a second `put_media` of the same blob writes nothing new and raises `last_seen_at` (read the row).
4. **Concurrent puts:** 8 tasks put the same blob at once: all `Ok`, one row, one object, no `staged_objects` row left.
5. **Crash between put and insert** (in-process, with the fault): fail `MediaAfterPut`: `put_media` is `Fault`; the object exists and is staged; `collect_staged(Duration::ZERO)` deletes it and its staged row.
6. **A stale staged row of a stored blob:** store a blob; then call `committer.stage_media(&path, 1)` directly (as a crashed second put would have left it); `collect_staged(Duration::ZERO)` removes only the staged row; the object and the `media` row stay; `get_media` still works. (A real second `put_media` returns at `touch_media` before staging, so the fault can't produce this case.)
7. **A put racing staged GC** (a `committer.rs` unit test, since the race sits between GC's listing and its per-path check): `stage_media(&path, 100)`; `stage_media(&path, 1_000)` (a second put re-staged it); `collect_staged_path(&path, &hooks, 500, delete)`, as GC would after listing the path with cut-off 500: it returns `StagedRemoval::Gone`, never calls `delete`, and the staged row stays.
8. **No temp file after a crash in a media write:** covered by the M5.8 crash parent (`remove_temp_files` walks `media/`).
9. **GC:**
   - a blob referenced by a span in a file is kept; one referenced only by a memtable span is kept;
   - an unreferenced blob with `last_seen_at` older than `MEDIA_GRACE` is deleted (object and row), and `MediaReport` counts it;
   - an unreferenced blob seen within `MEDIA_GRACE` is kept.

   Control time with the `now` argument of `collect_media`, and set `last_seen_at` by calling `put_media` (which uses the wall clock) before choosing `now`.
10. **GC after a deletion:** a blob referenced only by spans of trace 1; register `Traces { [1] }`; flush; `apply_deletions(now)` with `now` past `MEDIA_GRACE` from the put: the deletion is applied and the blob is gone.

**Commit:** `storage: store media blobs and collect unreferenced ones`

---

## M5.7 — Project drop

**Goal:** `drop_project` hides the project at once, refuses its appends from then on, obsoletes its files, deletes its media, and remembers it across restarts (spec 03 §7, D-A1-7).

### Migration `0006_projects.sql`

```sql
CREATE TABLE projects (
    project_id TEXT PRIMARY KEY,
    state TEXT NOT NULL,
    dropped_at INTEGER NOT NULL
);
```

`state` is `'dropping'` (files obsoleted, media not yet deleted) or `'dropped'` (done).

### Project states

- `ProjectStates` gains `dropped: HashSet<ProjectId>` (both `dropping` and `dropped` projects), `pub(crate) fn is_dropped(&self, project) -> bool`, and `pub(crate) fn with_dropped(&self, project) -> Self` (adds it and removes its watermark).
- `drops(project, day)` is `self.is_dropped(project) || (watermark rule)`.
- `Committer::project_states` also reads `projects`.

### Committer

- `Extra::Drop { project, now }`: `INSERT INTO projects (project_id, state, dropped_at) VALUES (?, 'dropping', ?)`; `DELETE FROM retention_watermarks WHERE project_id = ?`; `UPDATE deletions SET status = 'applied', applied_at = ? WHERE project_id = ? AND status = 'pending'`.
- `pub(crate) async fn drop_project<F>(&self, project: ProjectId, now: i64, publish: F) -> Result<bool, CommitError> where F: FnOnce(&Arc<Catalog>, &Arc<Tombstones>)`:
  1. `SELECT 1 FROM projects WHERE project_id = ?`: present → `Ok(false)` (already dropped; no commit, no publish).
  2. `edit` removes every active file of the project in all three tables (from `inner.catalog`).
  3. `commit_inner(&mut inner, &edit, &Extra::Drop { project, now })`.
  4. `inner.tombstones = Arc::new(inner.tombstones.without_project(project))` (new `Tombstones` method: removes every deletion of the project; `known_seq` unchanged).
  5. `publish(&next, &inner.tombstones)`; `Ok(true)`.
- **`write_edit` refuses files of dropped projects:** for each added file, `SELECT 1 FROM projects WHERE project_id = ?` → `CommitError::conflict("project is dropped")`. Check it before the watermark check.
- **`register_deletion` of a dropped project** (M5.1's method): after the idempotency check, if the project is in `projects`, insert only the `deletions` row, already applied (`status = 'applied'`, `applied_at = now`), with its `deletion_seq`, and **no** scope rows (an applied request keeps none, M5.1). Then `inner.tombstones = Arc::new(inner.tombstones.with_known_seq(seq))` and **don't** call `publish`: no pending deletion changed, and readers use `known_seq` only to label new files, where a value that lags behind an already-applied deletion is safe (it only makes a file a candidate when a pending deletion has a higher sequence). Add `Tombstones::with_known_seq` and `Tombstones::without_project` (M5.1 lists their signatures) in this sub-phase.
- **`apply_retention` of a dropped project:** in `Committer::apply_retention`, under the mutex and before anything else, `SELECT 1 FROM projects WHERE project_id = ?`; present → return `Ok(RetentionReport { files: 0, bytes: 0, keep_from })` with no commit and no `publish`.
- **`insert_media` refuses a dropped project:** it now returns `Result<bool, CatalogError>`; inside its transaction, `SELECT 1 FROM projects WHERE project_id = ?` → roll back and return `Ok(false)`, leaving the staged row (staged-object GC deletes the object later). `put_media` maps `false` to `MediaError::ProjectDropped`. This closes the race where a put passed the `is_dropped` check before the drop committed and would otherwise insert a row after `finish_drop` deleted the project's media.
- `pub(crate) async fn media_of(&self, project) -> Result<Vec<String>, CatalogError>` (hashes in order), `pub(crate) async fn delete_media_row(&self, project, sha) -> Result<(), CatalogError>`, `pub(crate) async fn dropping_projects(&self) -> Result<Vec<ProjectId>, CatalogError>`, and `pub(crate) async fn finish_drop(&self, project) -> Result<(), CatalogError>` (`UPDATE projects SET state = 'dropped' WHERE project_id = ?`).

### `Storage`

- **Append gate:** `append_gate: tokio::sync::RwLock<()>`. `append` takes the read guard first and holds it until `writer.append(..)` returns. Under it, after the `ShuttingDown` check: if `self.published.load().projects.is_dropped(project)`, return the new `AppendError::ProjectDropped` (`#[error("project is dropped")]`; the server maps it to 404, like the API does for a dropped project).
- **`drop_project`:**

```rust
/// Hides the project at once and refuses its appends, then deletes its media.
/// Its files are obsoleted in the same commit and removed by GC. Idempotent.
/// An error after the commit leaves the project dropped; GC finishes the media.
pub async fn drop_project(&self, project: ProjectId) -> Result<(), StorageError>;
```

  1. Take the append gate's **write** guard: no append of any project is between its check and its WAL write.
  2. `committer.drop_project(project, now, |catalog, tombstones| published.replace(|state| ReadState { catalog: Arc::clone(catalog), tombstones: Arc::clone(tombstones), projects: Arc::new(state.projects.with_dropped(project)), ..state.clone() }))`.
  3. Release the gate.
  4. `finish_drop(project)` (below), **whether step 2 returned `true` or `false`**: a second `drop_project` call finishes a drop whose media step failed. Its error is returned as `StorageError::Media(#[from] MediaError)` (new variant).
  5. Log `info!(project = %project, "project dropped")` on the first drop.

- **`finish_drop`** (a function in `media.rs` on `MediaGc`, `pub(crate) async fn finish_drop(&self, project: ProjectId) -> Result<(), MediaError>`): for each hash of `media_of(project)`: hook `MediaBeforeDelete` (pause, then fail → `Fault`), `objects.delete(&media_path)`, `delete_media_row`. Then `committer.finish_drop(project)`.
- **GC task:** each tick, after the other collections, for each of `dropping_projects()`: `media.finish_drop(project)`; log an error at `warn!`. At open, run it once too, after the first obsolete-file collection.
- **Queries:** in `project_query`, a dropped project gets providers over `Arc::new(MemSnapshot::default())` (add `Default` to `MemSnapshot`'s derive list; it has no partitions) for both tables, `Arc::new(Catalog::empty())`, and no deletions.
- **Flush:** `drops` already covers it (M5.5).
- **`apply_retention`** of a dropped project returns `Ok(RetentionReport { files: 0, bytes: 0, keep_from })` without writing (the committer check above).
- **Media:** `put_media` of a dropped project is the new `MediaError::ProjectDropped`: checked first from `published.load().projects`, and again by `insert_media` (above). `get_media` of a dropped project is `NotFound`. `MediaGc::collect_all` skips dropped projects (read `published.load().projects`).

### Tests (`lib.rs` tests or a `src/drop_tests.rs` module)

Two projects, P (dropped) and Q (kept), each with flushed spans, scores and a blob.

1. **Drop:**
   - `append_spans(P, ..)` and `append_scores(P, ..)` are `ProjectDropped`; Q's appends work;
   - every query of P returns no row; Q's queries are unchanged;
   - P's files are `obsolete` with `obsoleted_at` set; `collect_obsolete(Duration::ZERO)` deletes their objects;
   - P's blob object and row are gone; `projects` has P as `dropped`;
   - `put_media(P, ..)` is `ProjectDropped`, `get_media(P, ..)` is `NotFound`.
2. **Restart:** after a reopen, P's appends are still refused and its queries empty.
3. **Unflushed rows:**
   - append to P, drop P, `shutdown`: no file of P is committed; the shutdown flush drops P's rows, commits only `flushed_seq` (M5.3), and the WAL is truncated;
   - a second case replays the WAL: append to P, then end the session with `drop(storage)` **without** `shutdown` (the WAL keeps the rows), reopen, drop P, then `shutdown` and reopen: no file of P was ever committed and P's queries are empty.
4. **The gate:** hold `AfterWalSync`; start an append to P in a task; start `drop_project(P)` in another; after 200 ms the drop has not returned; release: the append returns `Ok`, then the drop returns; P's rows are never visible.
5. **Flush race:** rows of P and Q in one memtable; hold `FlushAfterWrite`; drop P in another task (it waits for the committer only, not for the flush); release: the flush commit conflicts ("project is dropped"), the retry writes Q's file only; P has no active file.
6. **Deletions:** a pending deletion of P becomes `Applied` at the drop and its tombstones are gone; `register_deletion` for P afterwards returns `Ok` and its status is `Applied`.
7. **Unfinished drop:** fail `MediaBeforeDelete` once: `drop_project` returns `Err(StorageError::Media(Fault))`; P is still dropped (appends refused, queries empty) and `dropping`; a second `drop_project(P)` returns `Ok` and P becomes `dropped`.
8. **Idempotent:** dropping twice is `Ok`; `apply_retention(P, ..)` returns `files = 0`.
9. **A media put racing the drop:** `put_media(P, blob)` with `MediaAfterPut` held (the put has passed its `is_dropped` check and written the object); `drop_project(P)` in another task completes (its `finish_drop` sees no media row yet); release: `put_media` returns `ProjectDropped`, no `media` row exists for P, and `collect_staged(Duration::ZERO)` deletes the object.

Add `DROP TABLE projects` to step 2 of `migration_applies_to_an_m4_catalog` and raise the migration counts to 6.

**Commit:** `storage: drop projects`

---

## M5.8 — Crash and model tests, docs

**Goal:** crashes at every new step leave a consistent directory that finishes the work on reopen, and a randomized model covers deletion and retention.

### Crash children (`crash_tests.rs`)

Follow `compaction_child` / `every_compaction_hook_aborts_safely`: a `#[ignore]` child test reading its directory and point from env vars, aborting at the point (`hooks().abort_at(point)`), and a `#[ignore]` parent that spawns it once per point and checks the directory. Add each point list to `parse_point`'s search.

**Deletion** (`deletion_child`, `every_deletion_hook_aborts_safely`, env `LLMOBS_DELETION_CRASH_DIR` / `_POINT`):
- **Child:** 3 flush cycles (like `compaction_child`), each appending one span of trace 0 and one of trace 1 (`span_row(0, i, ..)`, `span_row(1, i, ..)`), then open with `compaction_config`, `abort_at(point)`, `register_deletion(Traces { [trace 1] })` with cut-off `crash_at(12, 0)`, `apply_deletions(crash_at(12, 0))`, `collect_obsolete(Duration::ZERO)`, then `panic!` (point not reached).
- **Points:** `CatalogCommit` (the registration transaction is the first commit after `abort_at`), `CompactionAfterStage`, `ObjectSync`, `ObjectRename`, `CompactionAfterWrite`, `CompactionAfterCommit`, `GcObsoleteBeforeDelete`, `GcObsoleteAfterDelete`.
- **Parent, after the child aborted:** reopen; `register_deletion` again with the same id (it is new if the crash came before the registration committed); no trace 1 row in `spans` or `traces` queries; `objects_match_catalog`; `apply_deletions` until `applied = 1` or the deletion is `Applied` (at most 5 passes, each with `failed == 0`); `collect_obsolete(Duration::ZERO)`, `collect_staged(Duration::ZERO)`; every span of trace 0 is still there; no Parquet object of an active `spans` file holds a trace 1 row; `deletion_status` is `Applied`.

**Media** (`media_child`, `every_media_hook_aborts_safely`):
- **Child:** open, `abort_at(point)`, `put_media(blob)`, `panic!`.
- **Points:** `ObjectSync`, `ObjectRename`, `MediaAfterPut`. (`CatalogCommit` is not among them: the media insert does not run that hook.)
- **Parent:** reopen (startup removes temp files under `media/` too); no temp file is left (`temp_files()` from `crash_tests.rs`, extended to walk `media/`); `collect_staged(Duration::ZERO)`; no `media/` object without a `media` row and no row without an object; `put_media` again succeeds; `get_media` returns the bytes.

**Drop** (`drop_child`, `every_drop_hook_aborts_safely`):
- **Child:** one flush cycle with spans of P, one blob of P; open; `abort_at(point)`; `drop_project(P)`; `panic!`.
- **Points:** `CatalogCommit`, `MediaBeforeDelete`.
- **Parent:** reopen; `drop_project(P)` returns `Ok`; P's queries are empty and appends refused; `collect_obsolete(Duration::ZERO)`; no object and no temp file under `tables/*/project=P` or `media/P`; `projects` has P as `dropped`.

**`justfile`:** add one line per new parent to the `test` recipe, in the same form as the two existing ones:

```
cargo test --workspace --all-features -- --ignored --exact crash_tests::every_deletion_hook_aborts_safely
cargo test --workspace --all-features -- --ignored --exact crash_tests::every_media_hook_aborts_safely
cargo test --workspace --all-features -- --ignored --exact crash_tests::every_drop_hook_aborts_safely
```

`tests/crash.rs` (the randomized harness) is unchanged in M5.

### Model test (`compaction/model.rs`)

Extend the M4 model:
- **New ops:**
  - `Delete { trace: u8 }` (`0..6`): registers `Traces { [trace_id of trace] }` with a fresh UUIDv7 and cut-off = the op's `ingest` value (`clock + index`). Model: remove every key of that trace (all have `ingest ≤ cutoff`, since later appends use a later `index`).
  - `ApplyDeletions`: `apply_deletions(clock)`, with `failed == 0`.
  - `Retention { days: u8 }` (`0..4`): `apply_retention(project, D + days)`. Model: `keep_from = max(keep_from, D + days)`; remove every key whose trace day (`D + trace % 3`) is before it.
- **`Append` under a watermark:** a key whose trace day is before the model's `keep_from` is not inserted into the model (storage drops it at flush and hides it before).
- **Check after every op:** unchanged (`check_spans` reads the `spans` provider, which applies tombstones and the memtable watermark).
- **Final steps,** replacing the M4 ones, in this order:
  1. `shutdown`, then `open` (as in M4): the memtable is flushed, so no batch blocks "applied";
  2. set the final clock exactly as M4 does (`max(clock, D+6) + close_delay + 1 ms`);
  3. `apply_deletions(clock)` until every registered deletion's `deletion_status` is `Applied`, at most 10 passes, each with `failed == 0`; fail the test if any is still `Pending` after them;
  4. the M4 compaction loop at that clock (`compact` until `jobs == 0`, each `failed == 0`);
  5. `collect_obsolete(Duration::ZERO)`;
  6. the M4 checks (one row per model key at the model's version, `partial_count == 1` per trace, span counts per trace). They run after step 3 on purpose: while a deletion is pending, the `traces` view hides a covered trace's partials whole, and those counts would not match;
  7. read every active `spans` Parquet object: no row of a deleted trace with `ingest_time ≤` that deletion's cut-off remains.
- Keep the op weights so a case of 40 ops has about 3 deletions and 1 retention on average (`prop_oneof!` weights).

### Docs

- `lib.rs` crate doc: remove the line "deletion and retention are absent;".
- `docs/plans/a1-storage/00-design.md` is already updated for M5; change nothing there unless you deviated.

### Tests

The three crash parents pass 20 times in a row each; the model test passes 20 runs with `PROPTEST_CASES=64`.

**Commit:** `storage: test deletion, retention, media and drop under crashes and in the model`
