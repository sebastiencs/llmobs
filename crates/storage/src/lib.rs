//! Columnar storage for spans and scores: a write-ahead log, an in-memory
//! memtable, Parquet files, and a SQLite catalog, queried through DataFusion.
//!
//! See `docs/spec/03-storage.md`. The design and milestones are in
//! `docs/plans/a1-storage/00-design.md`.
//!
//! # What this crate does not do yet
//!
//! - Parquet footers and page indexes use DataFusion's built-in metadata cache.

#![warn(missing_docs)]

use std::fs::{File, OpenOptions};
use std::io;
use std::num::NonZeroUsize;
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use arrow::array::{Array, RecordBatch, StringArray};
use chrono::NaiveDate;
use datafusion::execution::runtime_env::{RuntimeEnv, RuntimeEnvBuilder};
use llmobs_core::ids::ProjectId;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::catalog::{Catalog, Committer, Registered};
use crate::config::{QueryConfig, StorageConfig, WalSync};
use crate::deletion::PendingDeletion;
use crate::error::{
    AppendError, DeletionError, MediaError, MemtableError, OpenError, QueryError, StorageError,
    WalError,
};
use crate::flush::FlushHandle;
use crate::hooks::{HookPoint, Hooks};
use crate::media::MediaGc;
use crate::memtable::{MemSnapshot, Memtable};
use crate::objects::Objects;
use crate::read_state::{Published, ReadState};
use crate::wal::Table;
use crate::wal::sync_dir;
use crate::wal::writer::{WalWriterHandle, WriterExit, WriterParams, spawn};
use crate::wal::{SegmentWriter, replay};

pub(crate) mod anchor;
pub(crate) mod catalog;
mod compaction;
pub mod config;
pub(crate) mod dedup;
pub(crate) mod deletion;
pub(crate) mod disk;
pub mod error;
mod filter;
pub(crate) mod flush;
pub(crate) mod gc;
pub(crate) mod media;
pub(crate) mod memory;
pub(crate) mod objects;
pub(crate) mod parquet;
pub(crate) mod partials;
pub(crate) mod projects;
pub mod query;
mod query_error;
mod query_text;
mod reads;
mod sql;
pub(crate) mod verify;
mod views;

pub use crate::compaction::job::CompactionReport;
pub use crate::compaction::rewrite::DeletionReport;
pub use crate::deletion::{Deletion, DeletionScope, DeletionStatus};
pub use crate::error::CompactionError;
pub use crate::gc::ObsoleteReport;
pub use crate::media::{MediaBlob, MediaRead, MediaReport};
pub use query_error::{InvalidQuery, Position, RunError};
pub use reads::{
    Bucket, BucketWidth, Kpi, KpiName, LATEST_SCORE_COLUMNS, ListScope, MAX_PAGE,
    MAX_SESSION_TURNS, MAX_TRACE_SPANS, MAX_USER_SESSIONS, MetricsData, MetricsGroupBy,
    MetricsRequest, ModelUsageRow, Page, Point, SPAN_LIST_COLUMNS, ScoreListRequest, ScoreTargets,
    Series, SeriesName, SessionDetailData, SessionListRequest, SessionSort, SortOrder,
    SpanListRequest, SpanSort, TraceDetailData, TraceListRequest, TraceSort, USER_COLUMNS,
    UserDetailData, UserListRequest, UserSort, latest_scores, metrics::metrics, score_list,
    session_detail, session_list, span_detail, span_list, trace_detail, trace_list,
    trace_list_plan, user_detail, user_list,
};
pub use sql::{EXPORT_MAX_ROWS, ExportFormat, SQL_MAX_ROWS, SqlColumn, SqlJson, SqlRequest};
pub use views::{
    ColumnInfo, ColumnKind, FunctionInfo, ProjectQuery, QueryContext, SqliteView, TimeWindow,
    ViewColumns, ViewName, function_catalog, view_catalog,
};

pub(crate) mod hooks;
pub(crate) mod memtable;
pub(crate) mod read_state;
pub(crate) mod wal;

#[cfg(test)]
mod crash_tests;
#[cfg(test)]
mod deletion_tests;
#[cfg(test)]
mod drop_tests;
#[cfg(test)]
mod quarantine_tests;
#[cfg(test)]
mod retention;
#[cfg(test)]
mod test_support;

/// One acknowledged append.
///
/// `seq` is the WAL sequence of that record. `rows` is the batch's row count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendReceipt {
    /// WAL sequence number of this record.
    pub seq: u64,
    /// Rows in the batch.
    pub rows: usize,
}

/// What [`Storage::health`] reports. Counters only: this does no I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageHealth {
    /// The spans writer has stopped after a WAL error.
    pub spans_wal_failed: bool,
    /// The scores writer has stopped after a WAL error.
    pub scores_wal_failed: bool,
    /// Accounted bytes of both memtables.
    pub memtable_bytes: u64,
    /// `storage.memtable.hard_limit`. Each table is checked against this on its own.
    pub memtable_hard_limit: u64,
}

/// What [`Storage::apply_retention`] removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionReport {
    /// Files obsoleted, all tables.
    pub files: u64,
    /// Their total size.
    pub bytes: u64,
    /// The watermark now stored: the later of the old one and the request.
    pub keep_from: NaiveDate,
}

/// Open data directory: the lock, both writers, and the published memtables.
#[derive(Debug)]
///
/// `Send + Sync`. The server holds this in an `Arc`.
pub struct Storage {
    hard_limit: u64,
    target_partitions: NonZeroUsize,
    /// `query.trace_lookback` in ns, the padded window around a trace range.
    lookback: i64,
    shutting_down: AtomicBool,
    /// Held for read by every [`Self::append`] until its WAL write returns and
    /// for write by [`Self::drop_project`] across its commit: no append of any
    /// project is between its `is_dropped` check and its WAL write while a
    /// drop commits (spec 03 §7).
    append_gate: tokio::sync::RwLock<()>,
    /// Released by [`Self::shutdown`] so another process can open the directory
    /// while this value is still alive.
    lock: Mutex<Option<File>>,
    /// Published memtables and catalog. `query` loads one snapshot.
    published: Arc<Published>,
    spans: WalWriterHandle,
    scores: WalWriterHandle,
    spans_flush: FlushHandle,
    scores_flush: FlushHandle,
    gc: gc::GcHandle,
    /// Free disk and compaction reservations (spec 03 §10).
    disk: Arc<disk::Disk>,
    disk_monitor: disk::DiskHandle,
    /// Media collection, cloned into the GC and compaction tasks.
    media: MediaGc,
    /// Shared by the background task and [`Self::compact`].
    compactor: Arc<compaction::job::Compactor>,
    compaction: compaction::task::CompactionHandle,
    /// Suspect files (spec 03 §10), shared with the compactor and the anchor rebuild.
    #[cfg_attr(not(test), allow(dead_code))]
    verify: Arc<verify::VerifyQueue>,
    /// Shared with the flush tasks and staged-object collection.
    #[cfg_attr(not(test), allow(dead_code))]
    objects: Arc<Objects>,
    /// Kept so tests can read `flushed_seq`. The flush tasks hold their own clones.
    #[cfg_attr(not(test), allow(dead_code))]
    committer: Arc<Committer>,
    runtime: Arc<RuntimeEnv>,
    /// Process-wide query memory (spec 03 §6). Each query's pool takes from it.
    memory: Arc<memory::SharedPool>,
    /// What [`Self::query`] admits each query with.
    query_memory: QueryMemory,
    /// Records replayed into the memtable at open. A flushed record is not counted.
    #[cfg_attr(not(test), allow(dead_code))]
    replayed: [u64; 2],
    /// Shared with the writers and the flush tasks. Tests arm faults here.
    #[allow(dead_code)]
    hooks: Hooks,
}

/// What [`Storage::query`] needs to admit a query (spec 03 §6).
#[derive(Debug, Clone, Copy)]
struct QueryMemory {
    /// Bytes a query takes from the shared pool before it starts.
    initial: usize,
    /// Most memory one query may hold.
    cap: usize,
    /// How long a query waits for its initial bytes.
    wait: std::time::Duration,
}

/// Optional hints for one [`Storage::query`] call.
#[derive(Debug, Clone, Copy, Default)]
pub struct QueryOptions {
    /// Time hint for a single-trace lookup (`?around=`), ns.
    ///
    /// When a lookup has no `start_time` bound, only files overlapping
    /// `[around - 1 day, around + 1 day]` are read (D154).
    pub around: Option<i64>,
}

impl Storage {
    /// Creates the directory layout, takes `data_dir/LOCK`, replays both WALs
    /// after `flushed_seq`, and starts the writers and flush tasks.
    ///
    /// Returns only after replay, so an append cannot run before recovery.
    /// Replay may push a memtable past `hard_limit`; later appends then answer
    /// [`AppendError::Backpressure`] until a flush makes room.
    pub async fn open(config: StorageConfig) -> Result<Self, OpenError> {
        Self::open_inner(config, None).await
    }

    /// [`Self::open`] with a remote store standing in for S3.
    #[cfg(test)]
    pub(crate) async fn open_with_store(
        config: StorageConfig,
        store: Arc<dyn object_store::ObjectStore>,
    ) -> Result<Self, OpenError> {
        Self::open_inner(config, Some(store)).await
    }

    async fn open_inner(
        config: StorageConfig,
        remote: Option<Arc<dyn object_store::ObjectStore>>,
    ) -> Result<Self, OpenError> {
        config.validate()?;
        let data_dir = config.data_dir.clone();
        let hard_limit = config.memtable.hard_limit.0;
        // `validate` rejects 0. A missed check still yields one partition.
        let target_partitions =
            NonZeroUsize::new(config.query.target_partitions).unwrap_or(NonZeroUsize::MIN);
        let tune = WriterTune {
            sync: config.wal.sync,
            group_max_bytes: config.wal.group_max_bytes.0,
            group_max_wait: config.wal.group_max_wait,
            segment_bytes: config.wal.segment_bytes.0,
            flush_max_bytes: config.flush.max_bytes.0,
        };
        let max_age = config.flush.max_age;
        let upload_concurrency = config.flush.upload_concurrency;
        // The padded trace window uses `query.trace_lookback` itself (D90, D86).
        let lookback = i64::try_from(config.query.trace_lookback.as_nanos()).unwrap_or(i64::MAX);
        // Resolve the anchor window once; `validate` has bounded both sources.
        let window = config.anchor.window.unwrap_or(config.query.trace_lookback);
        let window_ns = i64::try_from(window.as_nanos()).unwrap_or(i64::MAX);
        #[allow(clippy::default_constructed_unit_structs)]
        let hooks = Hooks::default();
        let layout_dir = data_dir.clone();
        let lock = tokio::task::spawn_blocking(move || prepare_layout(&layout_dir))
            .await
            .map_err(|_| {
                open_io(
                    "opening data directory",
                    &data_dir,
                    io::Error::other("open task panicked"),
                )
            })??;
        let committer = Arc::new(Committer::open(&data_dir, share_hooks(&hooks)).await?);
        let objects = open_objects(&config, &hooks, remote).await?;
        let mut disk_paths = vec![data_dir.clone()];
        disk_paths.extend(objects.local_root().map(Path::to_path_buf));
        let disk = Arc::new(disk::Disk::open(disk_paths, config.min_free_bytes.0)?);
        let objects_for_cleanup = Arc::clone(&objects);
        let removed = tokio::task::spawn_blocking(move || objects_for_cleanup.remove_temp_files())
            .await
            .map_err(|_| {
                open_io(
                    "removing temporary object files",
                    &data_dir,
                    io::Error::other("cleanup task panicked"),
                )
            })??;
        if removed > 0 {
            tracing::info!(removed, "removed leftover object temp files");
        }
        // Spill directories of a crashed process. `LOCK` is held (taken by
        // `prepare_layout`), so no live storage uses them.
        let spill_dir = data_dir.join("tmp").join("query");
        let spill_for_cleanup = spill_dir.clone();
        let removed = tokio::task::spawn_blocking(move || remove_spill_files(&spill_for_cleanup))
            .await
            .map_err(|_| {
                open_io(
                    "removing query spill files",
                    &spill_dir,
                    io::Error::other("cleanup task panicked"),
                )
            })??;
        if removed > 0 {
            tracing::info!(removed, "removed leftover query spill files");
        }
        let runtime = build_runtime(config.query.metadata_cache_bytes.0, &spill_dir)?;
        objects.register(&runtime);
        let (query_memory, total_memory) = resolve_query_memory(&config.query);
        let memory = Arc::new(memory::SharedPool::new(total_memory));
        let spans_flushed = committer.flushed_seq(Table::Spans).await;
        let scores_flushed = committer.flushed_seq(Table::Scores).await;
        let replay_dir = data_dir.clone();
        let skip_corrupt = config.wal.skip_corrupt;
        let segment_bytes = config.wal.segment_bytes.0;
        let replay_hooks = share_hooks(&hooks);
        let recovered = tokio::task::spawn_blocking(move || {
            recover_tables(
                &replay_dir,
                spans_flushed,
                scores_flushed,
                skip_corrupt,
                segment_bytes,
                &replay_hooks,
            )
        })
        .await
        .map_err(|_| {
            open_io(
                "replaying WAL",
                &data_dir,
                io::Error::other("replay task panicked"),
            )
        })??;
        let catalog = committer.catalog().await;
        let tombstones = committer.tombstones().await;
        let projects = Arc::new(committer.project_states().await?);
        let verify = Arc::new(verify::VerifyQueue::default());
        let mut anchors = anchor::Anchors::new(window_ns);
        anchors.rebuild(&catalog, &objects, &verify).await?;
        let anchors = Arc::new(Mutex::new(anchors));
        let published = Arc::new(Published::new(ReadState {
            spans: recovered.spans.memtable.snapshot(),
            scores: recovered.scores.memtable.snapshot(),
            catalog,
            tombstones,
            projects,
        }));
        // Compaction gets a runtime of its own, so its memory pool is
        // independent of the query runtime's.
        let compactor = {
            let memory = usize::try_from(config.compaction.max_memory.0).unwrap_or(usize::MAX);
            let runtime = RuntimeEnvBuilder::new()
                .with_memory_pool(Arc::new(
                    datafusion::execution::memory_pool::GreedyMemoryPool::new(memory),
                ))
                .build_arc()
                .map_err(|error| {
                    tracing::error!(%error, "building the compaction runtime");
                    OpenError::Runtime
                })?;
            objects.register(&runtime);
            Arc::new(compaction::job::Compactor::new(
                Arc::clone(&committer),
                Arc::clone(&objects),
                Arc::clone(&disk),
                Arc::clone(&verify),
                Arc::clone(&published),
                compaction::merge::MergeContext {
                    runtime,
                    batch_size: 8192,
                },
                compaction::plan::Policy::from(&config.compaction),
                compaction::job::CompactorLimits {
                    threads: config.compaction.threads,
                    max_delay: i64::try_from(config.deletion.max_delay.as_nanos())
                        .unwrap_or(i64::MAX),
                },
                share_hooks(&hooks),
            ))
        };
        let spans = spawn_writer(
            Table::Spans,
            &data_dir,
            recovered.spans,
            &tune,
            &published,
            &hooks,
        );
        let scores = spawn_writer(
            Table::Scores,
            &data_dir,
            recovered.scores,
            &tune,
            &published,
            &hooks,
        );
        let spans_flush = flush::spawn(flush::FlushStart {
            table: Table::Spans,
            dir: data_dir.join("wal").join(Table::Spans.name()),
            shared: Arc::clone(spans.shared()),
            committer: Arc::clone(&committer),
            objects: Arc::clone(&objects),
            published: Arc::clone(&published),
            hooks: share_hooks(&hooks),
            max_bytes: tune.flush_max_bytes,
            max_age,
            upload_concurrency,
            anchors: Some(Arc::clone(&anchors)),
        });
        let scores_flush = flush::spawn(flush::FlushStart {
            table: Table::Scores,
            dir: data_dir.join("wal").join(Table::Scores.name()),
            shared: Arc::clone(scores.shared()),
            committer: Arc::clone(&committer),
            objects: Arc::clone(&objects),
            published: Arc::clone(&published),
            hooks: share_hooks(&hooks),
            max_bytes: tune.flush_max_bytes,
            max_age,
            upload_concurrency,
            anchors: None,
        });
        if let Err(error) =
            gc::collect_once(&committer, &objects, &hooks, catalog::now_ns(), gc::MAX_AGE).await
        {
            // The error names the object key. Keys contain project ids.
            tracing::warn!("staged object collection failed");
            tracing::debug!(%error, "staged object collection error");
        }
        if let Err(error) = gc::collect_obsolete(
            &committer,
            &objects,
            &hooks,
            catalog::now_ns(),
            config.gc.grace,
        )
        .await
        {
            // The error names the object key. Keys contain project ids.
            tracing::warn!("obsolete file collection failed");
            tracing::debug!(%error, "obsolete file collection error");
        }
        let media = MediaGc {
            committer: Arc::clone(&committer),
            objects: Arc::clone(&objects),
            published: Arc::clone(&published),
            runtime: Arc::clone(&runtime),
            target_partitions,
            lookback,
            hooks: share_hooks(&hooks),
        };
        // Finish drops interrupted by a crash or a failed media step, after
        // the first obsolete-file collection (spec 03 §7).
        media.finish_drops().await;
        let gc = gc::spawn(gc::GcStart {
            committer: Arc::clone(&committer),
            objects: Arc::clone(&objects),
            hooks: share_hooks(&hooks),
            grace: config.gc.grace,
            media: MediaGc::clone(&media),
        });
        let disk_monitor = disk::spawn(Arc::clone(&disk));
        let compaction = compaction::task::spawn(
            Arc::clone(&compactor),
            MediaGc::clone(&media),
            config.compaction.interval,
        );
        Ok(Self {
            hard_limit,
            target_partitions,
            lookback,
            shutting_down: AtomicBool::new(false),
            append_gate: tokio::sync::RwLock::new(()),
            lock: Mutex::new(Some(lock)),
            published,
            spans,
            scores,
            spans_flush,
            scores_flush,
            gc,
            disk,
            disk_monitor,
            media,
            compactor,
            compaction,
            verify,
            objects,
            committer,
            runtime,
            memory,
            query_memory,
            replayed: [recovered.spans_records, recovered.scores_records],
            hooks,
        })
    }

    /// Appends a spans batch. `Ok` means the record is durable per `wal.sync`
    /// and visible to the next read of the published memtable.
    pub async fn append_spans(
        &self,
        project: ProjectId,
        batch: RecordBatch,
    ) -> Result<AppendReceipt, AppendError> {
        self.append(Table::Spans, project, batch).await
    }

    /// Appends a scores batch. Same durability rule as [`Self::append_spans`].
    pub async fn append_scores(
        &self,
        project: ProjectId,
        batch: RecordBatch,
    ) -> Result<AppendReceipt, AppendError> {
        self.append(Table::Scores, project, batch).await
    }

    /// Admits a query, then returns providers for `project` over one published
    /// snapshot, with a runtime of the query's own: its memory pool takes from
    /// the process-wide pool, up to `query.max_memory` (spec 03 §6).
    ///
    /// Waits up to `query.admission_wait` for `query.initial_memory`; then fails
    /// with [`QueryError::Busy`]. The snapshot is loaded after admission, so a
    /// query that waited sees the newest data.
    ///
    /// No DataFusion session is built. The query layer builds one from
    /// [`ProjectQuery::runtime`] and [`ProjectQuery::target_partitions`].
    ///
    /// The snapshot's pending deletions hide their rows from every provider
    /// (spec 03 §7): the memtable snapshots are filtered copies, and the
    /// providers carry the tombstones for the file side. A memtable partition
    /// of a day before the project's retention watermark is left out of the
    /// copies the same way (spec 03 §8); the files were removed from the
    /// catalog by the retention commit itself.
    pub async fn query(
        &self,
        project: ProjectId,
        options: QueryOptions,
    ) -> Result<ProjectQuery, QueryError> {
        let deadline = tokio::time::Instant::now() + self.query_memory.wait;
        let pool = self
            .memory
            .admit(self.query_memory.initial, self.query_memory.cap, deadline)
            .await
            .ok_or(QueryError::Busy)?;
        // Shares the metadata cache, the object stores and the spill directory
        // with the base runtime, and swaps only the memory pool.
        let runtime = RuntimeEnvBuilder::from_runtime_env(&self.runtime)
            .with_memory_pool(pool)
            .build_arc()?;
        project_query(
            &self.published.load(),
            project,
            options,
            &runtime,
            self.target_partitions,
            self.lookback,
        )
    }

    /// Registers a `traces` or `user` deletion. Idempotent by `deletion.id`.
    /// Rows in scope are hidden from every query once this returns (spec 03 §7).
    pub async fn register_deletion(&self, deletion: Deletion) -> Result<(), DeletionError> {
        let published = Arc::clone(&self.published);
        let registered = self
            .committer
            .register_deletion(&deletion, catalog::now_ns(), move |tombstones| {
                read_state::publish_tombstones(&published, tombstones);
            })
            .await?;
        if let Registered::New { seq } = registered {
            // Never log the scope: its ids name the person the deletion is about.
            tracing::info!(
                deletion_id = %deletion.id,
                project = %deletion.project,
                seq,
                "deletion registered"
            );
        }
        Ok(())
    }

    /// `Pending` or `Applied`. An unknown id is [`DeletionError::NotFound`].
    pub async fn deletion_status(&self, id: Uuid) -> Result<DeletionStatus, DeletionError> {
        self.committer
            .deletion_status(id)
            .await?
            .ok_or(DeletionError::NotFound)
    }

    /// Drops every partition of `project` before `keep_from` (a UTC day), and
    /// drops late rows for those days from then on (spec 03 §8). A `keep_from`
    /// earlier than the stored watermark changes nothing.
    pub async fn apply_retention(
        &self,
        project: ProjectId,
        keep_from: NaiveDate,
    ) -> Result<RetentionReport, StorageError> {
        let published = Arc::clone(&self.published);
        let report = self
            .committer
            .apply_retention(project, keep_from, move |next, watermark| {
                published.replace(|state| ReadState {
                    catalog: Arc::clone(next),
                    projects: Arc::new(state.projects.with_keep_from(project, watermark)),
                    ..state.clone()
                });
            })
            .await?;
        if report.files > 0 {
            tracing::info!(
                project = %project,
                keep_from = %report.keep_from,
                files = report.files,
                bytes = report.bytes,
                "retention applied"
            );
        }
        Ok(report)
    }

    /// Hides the project at once and refuses its appends, then deletes its
    /// media. Its files are obsoleted in the same commit and removed by GC.
    /// Idempotent. An error after the commit leaves the project dropped; GC
    /// finishes the media.
    pub async fn drop_project(&self, project: ProjectId) -> Result<(), StorageError> {
        // No append of any project runs between its `is_dropped` check and its
        // WAL write while the drop commits (spec 03 §7).
        let gate = self.append_gate.write().await;
        let published = Arc::clone(&self.published);
        let dropped = self
            .committer
            .drop_project(project, catalog::now_ns(), move |catalog, tombstones| {
                published.replace(|state| ReadState {
                    catalog: Arc::clone(catalog),
                    tombstones: Arc::clone(tombstones),
                    projects: Arc::new(state.projects.with_dropped(project)),
                    ..state.clone()
                });
            })
            .await?;
        drop(gate);
        // A second call finishes a drop whose media step failed, whether the
        // commit above was the first or a no-op.
        self.media.finish_drop(project).await?;
        if dropped {
            tracing::info!(project = %project, "project dropped");
        }
        Ok(())
    }

    /// Stores `blob` under `media/<project>/<sha256>` once (spec 03 §9, D-A1-6).
    /// Writing an existing hash refreshes its `last_seen_at` and writes nothing
    /// else. A dropped project is refused (spec 03 §7).
    ///
    /// The blob's object is durable before this returns; its `media` row and
    /// the staged row's removal share one transaction.
    pub async fn put_media(&self, project: ProjectId, blob: MediaBlob) -> Result<(), MediaError> {
        if self.published.load().projects.is_dropped(project) {
            return Err(MediaError::ProjectDropped);
        }
        if self.disk.low() {
            return Err(MediaError::DiskFull);
        }
        let now = catalog::now_ns();
        if blob.mime_type.is_empty() || blob.mime_type.len() > 255 || !blob.mime_type.is_ascii() {
            return Err(MediaError::Invalid {
                reason: "mime type",
            });
        }
        if blob.bytes.is_empty() {
            return Err(MediaError::Invalid {
                reason: "empty blob",
            });
        }
        let digest = Sha256::digest(blob.bytes.as_ref());
        if digest.as_slice() != blob.sha256.as_slice() {
            return Err(MediaError::HashMismatch);
        }
        let sha = hex::encode(blob.sha256);
        if self.committer.touch_media(project, &sha, now).await? {
            return Ok(());
        }
        let path = Objects::media_path(project, &sha);
        self.committer.stage_media(&path, now).await?;
        let size = u64::try_from(blob.bytes.len()).unwrap_or(u64::MAX);
        self.objects.put_media(&path, blob.bytes).await?;
        self.hooks.pause(HookPoint::MediaAfterPut);
        if self.hooks.fail(HookPoint::MediaAfterPut).is_some() {
            return Err(MediaError::Fault);
        }
        // A put that passed its `is_dropped` check before a drop committed is
        // caught here (spec 03 §7); the staged row stays for GC.
        if !self
            .committer
            .insert_media(project, &sha, &blob.mime_type, size, &path, now)
            .await?
        {
            return Err(MediaError::ProjectDropped);
        }
        Ok(())
    }

    /// The blob of `sha256`, whole or `range` of it (end exclusive), with its
    /// MIME type and the whole blob's size (spec 03 §9). A dropped project has
    /// nothing to read (spec 03 §7).
    pub async fn get_media(
        &self,
        project: ProjectId,
        sha256: &[u8; 32],
        range: Option<Range<u64>>,
    ) -> Result<MediaRead, MediaError> {
        if self.published.load().projects.is_dropped(project) {
            return Err(MediaError::NotFound);
        }
        let sha = hex::encode(sha256);
        let Some((mime_type, size)) = self.committer.media_row(project, &sha).await? else {
            return Err(MediaError::NotFound);
        };
        if let Some(range) = &range
            && (range.start >= range.end || range.end > size)
        {
            return Err(MediaError::InvalidRange);
        }
        let path = Objects::media_path(project, &sha);
        let bytes = self.objects.get_range(&path, range).await?;
        Ok(MediaRead {
            mime_type,
            size,
            bytes,
        })
    }

    /// Writer flags and memtable accounting. Does not touch the disk.
    pub fn health(&self) -> StorageHealth {
        let spans = self.spans.shared().memtable_bytes();
        let scores = self.scores.shared().memtable_bytes();
        StorageHealth {
            spans_wal_failed: self.spans.shared().failed(),
            scores_wal_failed: self.scores.shared().failed(),
            memtable_bytes: spans.saturating_add(scores),
            memtable_hard_limit: self.hard_limit,
        }
    }

    /// One staged-object collection, with an explicit age.
    ///
    /// The background task uses one hour. Tests pass a shorter age. A zero age
    /// collects every row whose `created_at` is before this call.
    #[doc(hidden)]
    pub async fn collect_staged(&self, max_age: std::time::Duration) -> Result<(), error::GcError> {
        gc::collect_once(
            &self.committer,
            &self.objects,
            &self.hooks,
            catalog::now_ns(),
            max_age,
        )
        .await
    }

    /// One obsolete-file collection with this grace. The background task uses
    /// `gc.grace`.
    #[doc(hidden)]
    pub async fn collect_obsolete(
        &self,
        grace: std::time::Duration,
    ) -> Result<ObsoleteReport, error::GcError> {
        gc::collect_obsolete(
            &self.committer,
            &self.objects,
            &self.hooks,
            catalog::now_ns(),
            grace,
        )
        .await
    }

    /// One compaction pass at `now` (ns), run to completion. The background
    /// task passes the wall clock every `compaction.interval`.
    #[doc(hidden)]
    pub async fn compact(&self, now: i64) -> Result<CompactionReport, CompactionError> {
        self.compactor.pass(now).await
    }

    /// One deletion pass at `now` (ns), run to completion: rewrites, raises, and
    /// marks applied. The background task runs one after each compaction pass.
    /// After a deletion is applied, its project's unreferenced media is
    /// collected at the same `now` (D-A1-6); a media error changes nothing here.
    #[doc(hidden)]
    pub async fn apply_deletions(&self, now: i64) -> Result<DeletionReport, CompactionError> {
        let report = self.compactor.deletion_pass(now).await?;
        for project in &report.applied_projects {
            if let Err(error) = self.media.collect(*project, now).await {
                // The label only: the error names an object path, which holds
                // the blob's hash (spec 03 §9).
                tracing::warn!(
                    project = %project,
                    error = error.label(),
                    "media collection failed"
                );
            }
        }
        Ok(report)
    }

    /// One media collection over every project at `now` (ns). The background
    /// task runs one daily.
    #[doc(hidden)]
    pub async fn collect_media(&self, now: i64) -> Result<MediaReport, MediaError> {
        self.media.collect_all(now).await
    }

    /// Replaces the compaction policy read at the start of each pass.
    #[cfg(test)]
    pub(crate) fn set_policy(&self, policy: compaction::plan::Policy) {
        self.compactor.set_policy(policy);
    }

    /// Files currently claimed by a compaction job.
    #[cfg(test)]
    pub(crate) fn compaction_claims(&self) -> usize {
        self.compactor.claimed()
    }

    /// The suspect files waiting for the verify pass.
    #[cfg(test)]
    pub(crate) fn verify_queue(&self) -> &verify::VerifyQueue {
        &self.verify
    }

    /// One verify pass at the wall clock, run to completion.
    #[cfg(test)]
    pub(crate) async fn verify_pass(&self) -> compaction::verify_pass::VerifyReport {
        self.compactor.verify_pass(catalog::now_ns()).await
    }

    /// Commits `edit` and publishes the catalog, for tests that race a job.
    #[cfg(test)]
    pub(crate) async fn commit_test_edit(
        &self,
        edit: &catalog::CatalogEdit,
    ) -> Result<Arc<Catalog>, error::CommitError> {
        let published = Arc::clone(&self.published);
        self.committer
            .commit(edit, move |next| {
                read_state::publish_catalog(&published, next);
            })
            .await
    }

    /// Stops writers, flushes both memtables, and releases the data directory lock.
    ///
    /// Other tasks may still hold this value and be inside `append`. An append
    /// that has not been acknowledged returns [`AppendError::ShuttingDown`].
    /// A query must not run after this returns: the lock, and with it the
    /// spill directory, belongs to the next [`Self::open`].
    /// A second call reports the same writer failure as the first, or `Ok`
    /// when the writers already stopped cleanly.
    ///
    /// Each flush task gets up to 30 seconds, together, to commit. A flush that
    /// is still running is cancelled; its rows stay in the WAL and are replayed
    /// on the next [`Self::open`].
    pub async fn shutdown(&self) -> Result<(), StorageError> {
        self.shutting_down.store(true, Ordering::Release);
        // Compaction stops first, so a job never commits against a catalog
        // another shutdown step is closing. Jobs see `stopping` before their
        // next slice or their commit and give up; staged outputs and temp
        // files are GC's and startup cleanup's problem.
        self.compactor.stop();
        self.compaction
            .stop_and_wait(std::time::Duration::from_secs(30))
            .await;
        self.gc.abort();
        self.disk_monitor.abort();
        let spans = self.spans.shutdown().await;
        let scores = self.scores.shutdown().await;
        self.finish_flushes().await;
        drop(
            self.lock
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        writer_exit("spans", &spans)?;
        writer_exit("scores", &scores)?;
        Ok(())
    }

    async fn finish_flushes(&self) {
        self.spans_flush.request_stop();
        self.scores_flush.request_stop();
        let spans = self.spans_flush.take_task();
        let scores = self.scores_flush.take_task();
        let spans_abort = spans.as_ref().map(tokio::task::JoinHandle::abort_handle);
        let scores_abort = scores.as_ref().map(tokio::task::JoinHandle::abort_handle);
        let wait = async move {
            if let Some(task) = spans {
                let _ = task.await;
            }
            if let Some(task) = scores {
                let _ = task.await;
            }
        };
        tokio::pin!(wait);
        if tokio::time::timeout(std::time::Duration::from_secs(30), &mut wait)
            .await
            .is_err()
        {
            tracing::warn!(
                "flush did not finish before shutdown; acknowledged rows remain in the WAL"
            );
            if let Some(handle) = spans_abort {
                handle.abort();
            }
            if let Some(handle) = scores_abort {
                handle.abort();
            }
            let _ = wait.await;
        }
    }

    async fn append(
        &self,
        table: Table,
        project: ProjectId,
        batch: RecordBatch,
    ) -> Result<AppendReceipt, AppendError> {
        // Held until `writer.append` returns, so `drop_project`'s write guard
        // waits for every in-flight append and holds new ones while it
        // commits (spec 03 §7).
        let _gate = self.append_gate.read().await;
        let writer = self.writer(table);
        if writer.shared().failed() {
            return Err(AppendError::WalFailed);
        }
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(AppendError::ShuttingDown);
        }
        if self.published.load().projects.is_dropped(project) {
            return Err(AppendError::ProjectDropped);
        }
        if self.disk.low() {
            return Err(AppendError::DiskFull);
        }
        validate_batch(table, &project, &batch)?;
        let bytes = batch.get_array_memory_size();
        let adding = u64::try_from(bytes).unwrap_or(u64::MAX);
        if writer.shared().memtable_bytes().saturating_add(adding) > self.hard_limit {
            return Err(AppendError::Backpressure);
        }
        writer.append(project, batch, bytes).await
    }

    fn writer(&self, table: Table) -> &WalWriterHandle {
        match table {
            Table::Spans => &self.spans,
            Table::Scores => &self.scores,
        }
    }

    /// Free disk and the reservations of the compaction jobs.
    #[cfg(test)]
    pub(crate) fn disk(&self) -> &disk::Disk {
        &self.disk
    }

    /// Pause and fault points shared with the writers. A no-op outside tests.
    #[cfg(test)]
    pub(crate) fn hooks(&self) -> &Hooks {
        &self.hooks
    }

    /// Rows currently published for `table`.
    #[cfg(test)]
    pub(crate) fn published_rows(&self, table: Table) -> usize {
        let state = self.published.load();
        let snapshot = match table {
            Table::Spans => &state.spans,
            Table::Scores => &state.scores,
        };
        snapshot
            .partitions
            .values()
            .flatten()
            .map(|batch| batch.batch.num_rows())
            .sum()
    }

    /// Active catalog files for `table`, including `traces`.
    #[cfg(test)]
    pub(crate) fn catalog_files(
        &self,
        table: impl Into<catalog::FileTable>,
    ) -> Vec<Arc<catalog::FileMeta>> {
        self.published.load().catalog.test_files(table.into())
    }

    /// Paths of the active files of a file table, including `traces`.
    #[cfg(test)]
    pub(crate) fn catalog_paths(&self, table: catalog::FileTable) -> Vec<String> {
        self.published
            .load()
            .catalog
            .test_files(table)
            .iter()
            .map(|file| file.path.clone())
            .collect()
    }

    /// Rows in the active catalog files of `table`.
    #[cfg(test)]
    pub(crate) fn catalog_rows(&self, table: Table) -> u64 {
        self.catalog_files(table).iter().map(|file| file.rows).sum()
    }

    /// WAL records replayed into the memtable at the last open.
    #[cfg(test)]
    fn replayed(&self, table: Table) -> u64 {
        self.replayed[catalog::table_index(table)]
    }

    /// `flushed_seq` stored for `table`.
    #[cfg(test)]
    async fn flushed_seq(&self, table: Table) -> u64 {
        self.committer.flushed_seq(table).await
    }

    /// Paths left in `staged_objects`.
    #[cfg(test)]
    async fn staged_paths(&self) -> Result<Vec<String>, error::CatalogError> {
        self.committer.staged_paths().await
    }
}

impl Drop for Storage {
    fn drop(&mut self) {
        self.spans.abort();
        self.scores.abort();
        self.spans_flush.abort();
        self.scores_flush.abort();
        self.compactor.stop();
        self.compaction.abort();
        self.gc.abort();
        self.disk_monitor.abort();
    }
}

/// The body of [`Storage::query`]: providers for `project` over one published
/// `state`. `Storage::query` and the media collection's live set both call it.
pub(crate) fn project_query(
    state: &ReadState,
    project: ProjectId,
    options: QueryOptions,
    runtime: &Arc<RuntimeEnv>,
    target_partitions: NonZeroUsize,
    lookback: i64,
) -> Result<ProjectQuery, QueryError> {
    let partitions = target_partitions.get();
    // A dropped project gets empty tables (spec 03 §7): the drop commit
    // removed its files from the catalog, its memtable partitions are dropped
    // at flush, and no file of it can be committed again, so nothing of it is
    // read at all.
    if state.projects.is_dropped(project) {
        let spans_snapshot = Arc::new(MemSnapshot::default());
        let scores_snapshot = Arc::new(MemSnapshot::default());
        let catalog = Arc::new(Catalog::empty());
        let no_deletions: Arc<[Arc<PendingDeletion>]> = Arc::from(Vec::new());
        return Ok(ProjectQuery {
            spans: Arc::new(query::SpansProvider::new(
                project,
                Arc::clone(&spans_snapshot),
                Arc::clone(&catalog),
                partitions,
                options.around,
                Arc::clone(&no_deletions),
            )),
            traces: Arc::new(query::TracesProvider::new(
                project,
                Arc::clone(&catalog),
                spans_snapshot,
                options,
                lookback,
                partitions,
                Arc::clone(&no_deletions),
            )),
            scores: Arc::new(query::ScoresProvider::new(
                project,
                scores_snapshot,
                catalog,
                partitions,
                no_deletions,
            )),
            runtime: Arc::clone(runtime),
            target_partitions,
            optimizer_rules: vec![Arc::new(query::TraceListRule)],
        });
    }
    let deletions = state.tombstones.for_project(project);
    let keep_from = state.projects.keep_from(project);
    let has_late = |snapshot: &MemSnapshot| {
        keep_from.is_some_and(|keep| {
            snapshot
                .partitions
                .keys()
                .any(|key| key.project == project && key.day < keep)
        })
    };
    let (spans_snapshot, scores_snapshot) = if deletions.is_empty()
        && !has_late(&state.spans)
        && !has_late(&state.scores)
    {
        (Arc::clone(&state.spans), Arc::clone(&state.scores))
    } else {
        (
            deletion::project_snapshot(Table::Spans, &state.spans, project, keep_from, &deletions)?,
            deletion::project_snapshot(
                Table::Scores,
                &state.scores,
                project,
                keep_from,
                &deletions,
            )?,
        )
    };
    let spans = Arc::new(query::SpansProvider::new(
        project,
        Arc::clone(&spans_snapshot),
        Arc::clone(&state.catalog),
        partitions,
        options.around,
        Arc::clone(&deletions),
    ));
    let scores = Arc::new(query::ScoresProvider::new(
        project,
        scores_snapshot,
        Arc::clone(&state.catalog),
        partitions,
        Arc::clone(&deletions),
    ));
    // The traces provider reads the filtered spans snapshot too, so its
    // memtable partials are computed from the rows that remain.
    let traces = Arc::new(query::TracesProvider::new(
        project,
        Arc::clone(&state.catalog),
        spans_snapshot,
        options,
        lookback,
        partitions,
        deletions,
    ));
    Ok(ProjectQuery {
        spans,
        traces,
        scores,
        runtime: Arc::clone(runtime),
        target_partitions,
        optimizer_rules: vec![Arc::new(query::TraceListRule)],
    })
}

struct Recovered {
    spans: RecoveredTable,
    scores: RecoveredTable,
    spans_records: u64,
    scores_records: u64,
}

struct RecoveredTable {
    memtable: Memtable,
    segment: SegmentWriter,
    next_seq: u64,
}

fn prepare_layout(data_dir: &Path) -> Result<File, OpenError> {
    ensure_dir(data_dir)?;
    let wal = data_dir.join("wal");
    ensure_dir(&wal)?;
    ensure_dir(&wal.join(Table::Spans.name()))?;
    ensure_dir(&wal.join(Table::Scores.name()))?;
    let tmp = data_dir.join("tmp");
    ensure_dir(&tmp)?;
    ensure_dir(&tmp.join("query"))?;
    lock_dir(data_dir)
}

/// Removes every entry of `spill_dir`: the spill directories of a process that
/// crashed. DataFusion removes its own on a clean drop. Returns how many were
/// removed; a missing entry is not an error.
fn remove_spill_files(spill_dir: &Path) -> Result<usize, OpenError> {
    let entries = std::fs::read_dir(spill_dir)
        .map_err(|source| open_io("removing query spill files", spill_dir, source))?;
    let mut removed = 0;
    for entry in entries {
        let entry =
            entry.map_err(|source| open_io("removing query spill files", spill_dir, source))?;
        let path = entry.path();
        let is_dir = entry
            .file_type()
            .map_err(|source| open_io("removing query spill files", &path, source))?
            .is_dir();
        let result = if is_dir {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        match result {
            Ok(()) => removed += 1,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(open_io("removing query spill files", &path, source)),
        }
    }
    Ok(removed)
}

/// The query memory limits (spec 03 §6): the shared pool's size and each
/// query's admission. The shared size is `query.max_memory_total`, or the
/// machine's default when absent. On a machine smaller than `max_memory`, it
/// and the per-query cap shrink to the machine's memory, but never below the
/// initial bytes.
fn resolve_query_memory(query: &QueryConfig) -> (QueryMemory, usize) {
    let mut total = query
        .max_memory_total
        .map_or_else(memory::default_total_memory, |total| total.0);
    let mut cap = query.max_memory.0;
    if total < cap {
        cap = total.max(query.initial_memory.0);
        total = cap;
        tracing::info!(total, cap, "query memory reduced to the machine's memory");
    }
    let to_usize = |bytes: u64| usize::try_from(bytes).unwrap_or(usize::MAX);
    (
        QueryMemory {
            initial: to_usize(query.initial_memory.0),
            cap: to_usize(cap),
            wait: query.admission_wait,
        },
        to_usize(total),
    )
}

fn recover_tables(
    data_dir: &Path,
    spans_flushed: u64,
    scores_flushed: u64,
    skip_corrupt: bool,
    segment_bytes: u64,
    hooks: &Hooks,
) -> Result<Recovered, OpenError> {
    let wal = data_dir.join("wal");
    let spans = recover_table(
        &wal.join(Table::Spans.name()),
        Table::Spans,
        spans_flushed,
        skip_corrupt,
        segment_bytes,
        hooks,
    )?;
    let scores = recover_table(
        &wal.join(Table::Scores.name()),
        Table::Scores,
        scores_flushed,
        skip_corrupt,
        segment_bytes,
        hooks,
    )?;
    Ok(Recovered {
        spans_records: spans.records,
        scores_records: scores.records,
        spans: spans.table,
        scores: scores.table,
    })
}

struct ReplayTable {
    table: RecoveredTable,
    records: u64,
}

fn recover_table(
    dir: &Path,
    table: Table,
    after_seq: u64,
    skip_corrupt: bool,
    segment_bytes: u64,
    hooks: &Hooks,
) -> Result<ReplayTable, OpenError> {
    let mut memtable = Memtable::new(table);
    let outcome = replay(dir, table, after_seq, skip_corrupt, &mut |seq, batch| {
        if batch.num_rows() == 0 {
            return Ok(());
        }
        let project = project_of(&batch)?;
        memtable
            .insert(project, batch, seq)
            .map_err(|err| memtable_to_wal(&err))
    })
    .map_err(replay_error)?;
    tracing::info!(
        table = table.name(),
        records = outcome.records,
        truncated_bytes = outcome.truncated_bytes,
        skipped_bytes = outcome.skipped_bytes,
        next_seq = outcome.next_seq,
        memtable_bytes = memtable.bytes(),
        "replayed WAL"
    );
    let segment = SegmentWriter::create(dir, outcome.next_seq, segment_bytes, share_hooks(hooks))?;
    Ok(ReplayTable {
        records: outcome.records,
        table: RecoveredTable {
            memtable,
            segment,
            next_seq: outcome.next_seq,
        },
    })
}

async fn open_objects(
    config: &StorageConfig,
    hooks: &Hooks,
    remote: Option<Arc<dyn object_store::ObjectStore>>,
) -> Result<Arc<Objects>, OpenError> {
    #[cfg(test)]
    if let Some(store) = remote {
        return Ok(Arc::new(Objects::remote_for_test(
            store,
            &config.data_dir,
            share_hooks(hooks),
        )?));
    }
    #[cfg(not(test))]
    let _ = remote;
    Ok(Arc::new(Objects::new(config, share_hooks(hooks)).await?))
}

/// The base runtime: the metadata cache, the object stores (registered by the
/// caller) and the spill directory. Its memory pool is DataFusion's default
/// (unbounded): it runs the media collection, and each query's runtime shares
/// its spill directory and caches (spec 03 §6).
fn build_runtime(
    metadata_cache_bytes: u64,
    spill_dir: &Path,
) -> Result<Arc<RuntimeEnv>, OpenError> {
    let limit = usize::try_from(metadata_cache_bytes).unwrap_or(usize::MAX);
    let runtime = RuntimeEnvBuilder::new()
        .with_metadata_cache_limit(limit)
        .with_temp_file_path(spill_dir)
        .build()
        .map_err(|error| {
            tracing::error!(%error, "building the query runtime");
            OpenError::Runtime
        })?;
    Ok(Arc::new(runtime))
}

struct WriterTune {
    sync: WalSync,
    group_max_bytes: u64,
    group_max_wait: std::time::Duration,
    segment_bytes: u64,
    flush_max_bytes: u64,
}

fn spawn_writer(
    table: Table,
    data_dir: &Path,
    recovered: RecoveredTable,
    tune: &WriterTune,
    published: &Arc<Published>,
    hooks: &Hooks,
) -> WalWriterHandle {
    spawn(WriterParams {
        table,
        dir: data_dir.join("wal").join(table.name()),
        segment: recovered.segment,
        next_seq: recovered.next_seq,
        sync: tune.sync,
        group_max_bytes: tune.group_max_bytes,
        group_max_wait: tune.group_max_wait,
        segment_bytes: tune.segment_bytes,
        flush_max_bytes: tune.flush_max_bytes,
        hooks: share_hooks(hooks),
        published: Arc::clone(published),
        memtable: recovered.memtable,
    })
}

fn project_of(batch: &RecordBatch) -> Result<ProjectId, WalError> {
    let Some(column) = batch.column_by_name(llmobs_core::schema::spans::PROJECT_ID) else {
        return Err(WalError::InvalidPayload {
            reason: "batch is missing project_id",
        });
    };
    let Some(values) = column.as_any().downcast_ref::<StringArray>() else {
        return Err(WalError::InvalidPayload {
            reason: "project_id has an unexpected type",
        });
    };
    if values.is_empty() || values.null_count() > 0 {
        return Err(WalError::InvalidPayload {
            reason: "project_id is null",
        });
    }
    let first = values.value(0);
    for row in 1..values.len() {
        if values.value(row) != first {
            return Err(WalError::InvalidPayload {
                reason: "batch contains more than one project",
            });
        }
    }
    first.parse().map_err(|_| WalError::InvalidPayload {
        reason: "invalid project id",
    })
}

fn memtable_to_wal(err: &MemtableError) -> WalError {
    let reason = match err {
        MemtableError::TimeOutOfRange => "time column is outside the supported range",
        MemtableError::MissingColumn { .. } => "batch is missing a required column",
        MemtableError::BadColumn { .. } => "time column has an unexpected type or a null",
        MemtableError::Arrow(_) => "building a memtable batch",
    };
    WalError::InvalidPayload { reason }
}

fn replay_error(err: WalError) -> OpenError {
    match err {
        WalError::Corrupt {
            table,
            segment,
            offset,
        } => OpenError::WalCorrupt {
            table,
            segment,
            offset,
        },
        other => OpenError::WalReplay(other),
    }
}

fn validate_batch(
    table: Table,
    project: &ProjectId,
    batch: &RecordBatch,
) -> Result<(), AppendError> {
    if batch.num_rows() == 0 {
        return Err(AppendError::InvalidBatch {
            reason: "batch has no rows",
        });
    }
    if batch.schema().fields() != table.schema().fields() {
        return Err(AppendError::InvalidBatch {
            reason: "batch schema does not match the table",
        });
    }
    let Some(values) = batch
        .column_by_name(llmobs_core::schema::spans::PROJECT_ID)
        .and_then(|column| column.as_any().downcast_ref::<StringArray>())
    else {
        return Err(AppendError::InvalidBatch {
            reason: "batch schema does not match the table",
        });
    };
    let expected = project.to_string();
    for row in 0..values.len() {
        if values.is_null(row) || values.value(row) != expected {
            return Err(AppendError::InvalidBatch {
                reason: "batch project_id does not match the project",
            });
        }
    }
    Ok(())
}

fn ensure_dir(path: &Path) -> Result<(), OpenError> {
    match std::fs::create_dir(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                sync_dir(parent).map_err(|source| open_io("syncing directory", parent, source))?;
            }
            Ok(())
        }
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(open_io("creating directory", path, source)),
    }
}

fn lock_dir(data_dir: &Path) -> Result<File, OpenError> {
    let path = data_dir.join("LOCK");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|source| open_io("opening lock file", &path, source))?;
    match file.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return Err(OpenError::Locked {
                path: data_dir.to_path_buf(),
            });
        }
        Err(std::fs::TryLockError::Error(source)) => {
            return Err(open_io("locking data directory", data_dir, source));
        }
    }
    sync_dir(data_dir).map_err(|source| open_io("syncing directory", data_dir, source))?;
    Ok(file)
}

fn open_io(action: &'static str, path: &Path, source: io::Error) -> OpenError {
    OpenError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

fn writer_exit(table: &'static str, exit: &Result<(), WriterExit>) -> Result<(), StorageError> {
    match exit {
        Ok(()) => Ok(()),
        Err(WriterExit::Failed) => Err(StorageError::WalFailed { table }),
        Err(WriterExit::Panicked) => Err(StorageError::WriterPanicked { table }),
    }
}

fn share_hooks(hooks: &Hooks) -> Hooks {
    // Production hooks are `Copy`. Tests share one value and have to clone it.
    #[allow(clippy::clone_on_copy)]
    hooks.clone()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use ::parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use arrow::array::{ArrayRef, FixedSizeBinaryBuilder, StringArray};
    use arrow::compute::concat_batches;
    use arrow::datatypes::{DataType, Field, Schema};
    use bytesize::ByteSize;
    use llmobs_core::batch::scores_to_record_batch;
    use llmobs_core::enums::ScoreSource;
    use llmobs_core::ids::{ProjectId, TraceId};
    use llmobs_core::schema::{spans, spans_schema};
    use llmobs_core::score::{Score, ScoreTarget, ScoreValue};
    use object_store::ObjectStoreExt;
    use sqlx::ConnectOptions;
    use sqlx::sqlite::SqliteConnectOptions;

    use super::*;
    use crate::catalog::FileTable;
    use crate::compaction::job::tests as jobs;
    use crate::config::{FlushConfig, MemtableConfig, QueryConfig, S3Config, WalConfig};
    use crate::error::StorageError;
    use crate::hooks::HookPoint;
    use crate::media::MediaBlob;
    use crate::partials::partials_from_spans;
    use crate::test_support::{span_row, span_rows};
    use crate::views::QueryContext;
    use crate::wal::{parse_segment_name, spans_at, spans_keyed, spans_named};

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn other_project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000002".parse().unwrap()
    }

    fn config_in(dir: &Path) -> StorageConfig {
        StorageConfig {
            data_dir: dir.to_path_buf(),
            min_free_bytes: ByteSize::b(0),
            object_store: None,
            s3: S3Config::default(),
            wal: WalConfig::default(),
            flush: FlushConfig::default(),
            memtable: MemtableConfig::default(),
            query: QueryConfig::default(),
            anchor: Default::default(),
            gc: Default::default(),
            compaction: Default::default(),
            deletion: Default::default(),
        }
    }

    /// Three distinct traces spread over two UTC days (one partition per day).
    fn three_traces_two_days() -> RecordBatch {
        const DAY_NS: i64 = 86_400_000_000_000;
        let base = spans_at(&[1_000, 1_100, DAY_NS + 2_000]);
        let mut ids = FixedSizeBinaryBuilder::with_capacity(3, 16);
        for last in [1_u8, 2, 3] {
            let mut bytes = [0_u8; 16];
            bytes[15] = last;
            ids.append_value(bytes).unwrap();
        }
        let mut columns: Vec<ArrayRef> = base.columns().to_vec();
        let index = base.schema().index_of(spans::TRACE_ID).unwrap();
        columns[index] = Arc::new(ids.finish());
        RecordBatch::try_new(base.schema(), columns).unwrap()
    }

    /// Reads one Parquet object back with the sync reader.
    fn read_parquet(dir: &Path, path: &str) -> RecordBatch {
        let file = File::open(dir.join("objects").join(path)).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).unwrap();
        let schema = Arc::clone(builder.schema());
        let batches: Vec<RecordBatch> = builder
            .build()
            .unwrap()
            .map(|batch| batch.unwrap())
            .collect();
        concat_batches(&schema, &batches).unwrap()
    }

    /// Compares two batches column by column; the stored file's schema also
    /// carries Parquet KV metadata, so whole-batch equality is too strict.
    fn assert_same_rows(actual: &RecordBatch, expected: &RecordBatch, label: &str) {
        assert_eq!(actual.num_rows(), expected.num_rows(), "{label}: rows");
        for field in expected.schema().fields() {
            let name = field.name();
            assert_eq!(
                actual.column_by_name(name),
                expected.column_by_name(name),
                "{label}: {name}"
            );
        }
    }

    /// `(spans path, traces path)` for every `file_sources` row.
    async fn source_pairs(dir: &Path) -> Vec<(String, String)> {
        let mut connection = SqliteConnectOptions::new()
            .filename(dir.join("catalog.db"))
            .read_only(true)
            .connect()
            .await
            .unwrap();
        let rows = sqlx::query(
            "SELECT s.path AS spans_path, t.path AS traces_path
             FROM file_sources fs
             JOIN data_files s ON s.id = fs.spans_file_id
             JOIN data_files t ON t.id = fs.traces_file_id",
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        rows.iter()
            .map(|row| {
                (
                    sqlx::Row::try_get(row, "spans_path").unwrap(),
                    sqlx::Row::try_get(row, "traces_path").unwrap(),
                )
            })
            .collect()
    }

    fn score_batch(name: &str) -> RecordBatch {
        let score = Score {
            project_id: project(),
            score_id: "018f6b4e-7c1a-7c1a-8c1a-0000000000ab".parse().unwrap(),
            target: ScoreTarget::Trace {
                trace_id: "4bf92f3577b34da6a3ce929d0e0e4736"
                    .parse::<TraceId>()
                    .unwrap(),
            },
            session_id: None,
            user_id: None,
            name: name.to_owned(),
            value: ScoreValue::Numeric(0.5),
            reason: None,
            source: ScoreSource::Api,
            evaluator_id: None,
            rule_id: None,
            experiment_id: None,
            dataset_item_id: None,
            author_user_id: None,
            metadata: std::collections::BTreeMap::new(),
            created_at: 5,
            updated_at: 6,
            deleted: false,
            ingest_time: 7,
        };
        scores_to_record_batch(&[score]).unwrap()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn reopen_after_drop_continues_sequence_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path());
        let storage = Storage::open(config.clone()).await.unwrap();
        let span = storage
            .append_spans(project(), spans_at(&[1]))
            .await
            .unwrap();
        let score = storage
            .append_scores(project(), score_batch("accuracy"))
            .await
            .unwrap();
        assert_eq!(span.seq, 1);
        assert_eq!(score.seq, 1);
        drop(storage);
        tokio::task::yield_now().await;
        let storage = Storage::open(config).await.unwrap();
        assert_eq!(storage.published_rows(Table::Spans), 1);
        assert_eq!(storage.published_rows(Table::Scores), 1);
        let span = storage
            .append_spans(project(), spans_at(&[2]))
            .await
            .unwrap();
        let score = storage
            .append_scores(project(), score_batch("accuracy"))
            .await
            .unwrap();
        assert_eq!((span.seq, score.seq), (2, 2));
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_second_open_is_locked_until_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path());
        let first = Storage::open(config.clone()).await.unwrap();
        let error = Storage::open(config.clone()).await.unwrap_err();
        assert!(matches!(error, OpenError::Locked { .. }), "{error}");
        first.shutdown().await.unwrap();
        let second = Storage::open(config).await.unwrap();
        second.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn invalid_batches_use_static_reasons() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(config_in(dir.path())).await.unwrap();
        let marker = "marker-7f3a9c2e-not-a-real-row";

        let empty = RecordBatch::new_empty(spans_schema());
        let error = storage.append_spans(project(), empty).await.unwrap_err();
        assert_eq!(
            error,
            AppendError::InvalidBatch {
                reason: "batch has no rows",
            }
        );
        assert!(!error.to_string().contains(marker));

        let schema = Arc::new(Schema::new(vec![Field::new(marker, DataType::Utf8, true)]));
        let wrong_schema = RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(vec![Some(marker)]))],
        )
        .unwrap();
        let error = storage
            .append_spans(project(), wrong_schema)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            AppendError::InvalidBatch {
                reason: "batch schema does not match the table",
            }
        );
        assert!(!error.to_string().contains(marker));

        let wrong_project = spans_named(&[1], marker);
        let error = storage
            .append_spans(other_project(), wrong_project)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            AppendError::InvalidBatch {
                reason: "batch project_id does not match the project",
            }
        );
        assert!(!error.to_string().contains(marker));
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn backpressure_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_in(dir.path());
        config.memtable.hard_limit = ByteSize::mib(16);
        config.flush.max_bytes = ByteSize::mib(1);
        let storage = Storage::open(config.clone()).await.unwrap();
        let huge = spans_named(&[1], &"m".repeat(17 * 1024 * 1024));
        let error = storage.append_spans(project(), huge).await.unwrap_err();
        assert_eq!(error, AppendError::Backpressure);
        assert_eq!(storage.published_rows(Table::Spans), 0);
        storage.shutdown().await.unwrap();
        let storage = Storage::open(config).await.unwrap();
        let receipt = storage
            .append_spans(project(), spans_at(&[2]))
            .await
            .unwrap();
        assert_eq!(receipt.seq, 1);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_wal_write_error_fails_the_writer_and_replays_acked_rows() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path());
        let storage = Storage::open(config.clone()).await.unwrap();
        storage
            .append_spans(project(), spans_at(&[1]))
            .await
            .unwrap();
        storage.hooks().arm_fail(HookPoint::WalWrite);
        let error = storage
            .append_spans(project(), spans_at(&[2]))
            .await
            .unwrap_err();
        assert_eq!(error, AppendError::WalFailed);
        assert!(storage.health().spans_wal_failed);
        assert!(!storage.health().scores_wal_failed);
        let shutdown = storage.shutdown().await.unwrap_err();
        assert!(matches!(
            shutdown,
            StorageError::WalFailed { table: "spans" }
        ));
        let storage = Storage::open(config).await.unwrap();
        // Shutdown flushed the acknowledged row, so replay has nothing to insert.
        assert_eq!(storage.replayed(Table::Spans), 0);
        assert_eq!(storage.published_rows(Table::Spans), 0);
        assert_eq!(storage.catalog_rows(Table::Spans), 1);
        let receipt = storage
            .append_spans(project(), spans_at(&[3]))
            .await
            .unwrap();
        assert_eq!(receipt.seq, 2);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_torn_tail_is_dropped_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path());
        let storage = Storage::open(config.clone()).await.unwrap();
        storage
            .append_spans(project(), spans_at(&[1]))
            .await
            .unwrap();
        storage
            .append_spans(project(), spans_at(&[2]))
            .await
            .unwrap();
        // Drop aborts the flush task. Shutdown would commit the rows and the
        // following corruption would no longer be in the replayed WAL.
        drop(storage);
        let segment = span_segments(dir.path())
            .into_iter()
            .find(|path| path.metadata().unwrap().len() > 0)
            .unwrap();
        let len = segment.metadata().unwrap().len();
        let file = OpenOptions::new().write(true).open(&segment).unwrap();
        file.set_len(len - 1).unwrap();
        drop(file);
        let storage = Storage::open(config).await.unwrap();
        assert_eq!(storage.published_rows(Table::Spans), 1);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_corrupt_middle_segment_fails_open_unless_skip_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_in(dir.path());
        config.wal.segment_bytes = ByteSize::mib(1);
        let storage = Storage::open(config.clone()).await.unwrap();
        // Repeated bytes compress under LZ4 and never fill a segment.
        let big = incompressible(2 * 1024 * 1024);
        storage
            .append_spans(project(), spans_named(&[1], &big))
            .await
            .unwrap();
        storage
            .append_spans(project(), spans_named(&[2], &big))
            .await
            .unwrap();
        storage
            .append_spans(project(), spans_at(&[3]))
            .await
            .unwrap();
        // Drop leaves the WAL unflushed so the corrupt middle segment is still there.
        drop(storage);

        let mut segments = span_segments(dir.path());
        segments.retain(|path| path.metadata().unwrap().len() > 0);
        segments.sort();
        assert!(segments.len() >= 3, "{segments:?}");
        let middle = &segments[1];
        let mut bytes = std::fs::read(middle).unwrap();
        bytes[30] ^= 0xff;
        std::fs::write(middle, &bytes).unwrap();

        let error = Storage::open(config.clone()).await.unwrap_err();
        assert!(
            matches!(error, OpenError::WalCorrupt { table: "spans", .. }),
            "{error}"
        );

        config.wal.skip_corrupt = true;
        let storage = Storage::open(config).await.unwrap();
        assert_eq!(storage.published_rows(Table::Spans), 2);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_appends_and_health_do_not_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(config_in(dir.path())).await.unwrap());
        let done = Arc::new(AtomicBool::new(false));
        let health = {
            let storage = Arc::clone(&storage);
            let done = Arc::clone(&done);
            tokio::spawn(async move {
                while !done.load(Ordering::Relaxed) {
                    let _ = storage.health();
                    tokio::task::yield_now().await;
                }
            })
        };
        let mut tasks = Vec::new();
        for start in 0..32 {
            let storage = Arc::clone(&storage);
            tasks.push(tokio::spawn(async move {
                storage
                    .append_spans(project(), spans_at(&[start]))
                    .await
                    .unwrap();
            }));
        }
        let finished = tokio::time::timeout(Duration::from_secs(15), async {
            for task in tasks {
                task.await.unwrap();
            }
        })
        .await;
        done.store(true, Ordering::Relaxed);
        health.await.unwrap();
        assert!(finished.is_ok(), "append and health deadlocked");
        let storage = Arc::try_unwrap(storage).unwrap();
        assert_eq!(storage.published_rows(Table::Spans), 32);
        storage.shutdown().await.unwrap();
    }

    async fn span_names(spans: Arc<dyn datafusion::catalog::TableProvider>) -> Vec<String> {
        let ctx = datafusion::execution::context::SessionContext::new();
        ctx.register_table("spans_t", spans).unwrap();
        let batches = ctx
            .sql("SELECT name FROM spans_t")
            .await
            .unwrap()
            .collect()
            .await
            .unwrap();
        let mut names = Vec::new();
        for batch in batches {
            let column = batch
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            for index in 0..column.len() {
                names.push(column.value(index).to_owned());
            }
        }
        names
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_query_during_append_stays_on_its_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(config_in(dir.path())).await.unwrap());
        storage.hooks().hold(HookPoint::AfterWalSync);
        let append = {
            let storage = Arc::clone(&storage);
            tokio::spawn(async move { storage.append_spans(project(), spans_at(&[1])).await })
        };
        let started = std::time::Instant::now();
        while storage.hooks().waiting_at(HookPoint::AfterWalSync) == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "writer never reached AfterWalSync"
            );
            tokio::task::yield_now().await;
        }
        let during = storage.query(project(), Default::default()).await.unwrap();
        assert!(span_names(Arc::clone(&during.spans)).await.is_empty());
        storage.hooks().release(HookPoint::AfterWalSync);
        append.await.unwrap().unwrap();
        let after = storage.query(project(), Default::default()).await.unwrap();
        assert_eq!(span_names(after.spans).await.len(), 1);
        assert!(span_names(during.spans).await.is_empty());
        storage.shutdown().await.unwrap();
    }

    /// Config whose query memory is `mib` MiB for the shared pool, the per-query
    /// cap and the initial bytes, with a short admission wait.
    fn config_with_query_memory(dir: &Path, mib: u64) -> StorageConfig {
        let mut config = config_in(dir);
        config.query.max_memory_total = Some(ByteSize::mib(mib));
        config.query.max_memory = ByteSize::mib(mib);
        config.query.initial_memory = ByteSize::mib(mib);
        config.query.admission_wait = Duration::from_millis(100);
        config
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn appends_fail_while_the_disk_is_low() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_in(dir.path());
        config.min_free_bytes = ByteSize::b(1000);
        let storage = Storage::open(config).await.unwrap();
        let bytes = bytes::Bytes::from_static(b"media bytes");
        let blob = MediaBlob {
            sha256: Sha256::digest(&bytes).into(),
            mime_type: "text/plain".to_owned(),
            bytes,
        };

        storage.disk().set_free_for_test(Some(10));
        assert_eq!(
            storage
                .append_spans(project(), spans_at(&[1]))
                .await
                .unwrap_err(),
            AppendError::DiskFull
        );
        assert_eq!(
            storage
                .append_scores(project(), score_batch("accuracy"))
                .await
                .unwrap_err(),
            AppendError::DiskFull
        );
        assert!(matches!(
            storage.put_media(project(), blob.clone()).await,
            Err(MediaError::DiskFull)
        ));

        storage.disk().set_free_for_test(Some(10_000));
        storage
            .append_spans(project(), spans_at(&[1]))
            .await
            .unwrap();
        storage
            .append_scores(project(), score_batch("accuracy"))
            .await
            .unwrap();
        storage.put_media(project(), blob).await.unwrap();
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_compaction_job_that_does_not_fit_is_deferred() {
        let dir = tempfile::tempdir().unwrap();
        let config = jobs::config_in(
            dir.path(),
            jobs::quiet_compaction(2),
            Duration::from_secs(3_600),
        );
        let mut storage = Storage::open(config.clone()).await.unwrap();
        for index in 0..2_u8 {
            let base = jobs::at(10, 0, 0) + i64::from(index) * 60 * 1_000_000_000;
            let batch = span_rows(&[span_row(0, 10 + index, base, base + 1_000_000_000)]);
            storage = jobs::flush_cycle(&config, storage, batch).await;
        }
        let before = storage.catalog_paths(FileTable::Spans);
        assert_eq!(before.len(), 2);

        storage.disk().set_free_for_test(Some(0));
        let report = storage.compact(jobs::at(11, 0, 0)).await.unwrap();
        assert_eq!((report.jobs, report.failed), (0, 1), "{report:?}");
        assert_eq!(storage.catalog_paths(FileTable::Spans), before);

        storage.disk().set_free_for_test(None);
        let report = storage.compact(jobs::at(11, 0, 0)).await.unwrap();
        assert_eq!((report.jobs, report.failed), (1, 0), "{report:?}");
        assert_eq!(storage.catalog_files(Table::Spans).len(), 1);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_query_waits_then_fails_as_busy() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(config_with_query_memory(dir.path(), 2))
            .await
            .unwrap();
        let held = storage.query(project(), Default::default()).await.unwrap();
        let busy = storage.query(project(), Default::default()).await;
        assert!(matches!(busy, Err(QueryError::Busy)), "a second query fits");
        drop(held);
        storage.query(project(), Default::default()).await.unwrap();
        storage.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_query_that_cannot_spill_fails_as_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_with_query_memory(dir.path(), 1);
        let storage = Storage::open(config.clone()).await.unwrap();
        // One trace of many spans with long names: the join's build side holds
        // all of them, and a hash join does not spill.
        let spans: Vec<_> = (1..=20_000_u64)
            .map(|index| {
                let mut span = span_row(1, 0, i64::try_from(index).unwrap(), 1);
                span.span_id = llmobs_core::ids::SpanId::from_bytes(index.to_be_bytes()).unwrap();
                span.name = "n".repeat(200);
                span
            })
            .collect();
        storage
            .append_spans(project(), span_rows(&spans))
            .await
            .unwrap();
        // Shutdown flushes the memtable to a Parquet file.
        storage.shutdown().await.unwrap();

        let storage = Storage::open(config).await.unwrap();
        let ctx = QueryContext::new(
            storage.query(project(), Default::default()).await.unwrap(),
            None,
        )
        .unwrap();
        let params = serde_json::Map::new();
        let result = ctx
            .sql_json(SqlRequest {
                query: "SELECT a.name AS a_name, b.name AS b_name FROM spans a JOIN spans b ON a.trace_id = b.trace_id LIMIT 1",
                params: &params,
                limit: 1,
                timeout: Duration::from_secs(30),
            })
            .await;
        match result {
            Err(RunError::TooLarge) => {}
            Err(other) => panic!("expected too large, got {other:?}"),
            Ok(_) => panic!("the join fit in one megabyte"),
        }
        drop(ctx);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn open_removes_leftover_spill_directories() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join("tmp").join("query").join("datafusion-old");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("x"), b"spill").unwrap();
        let storage = Storage::open(config_in(dir.path())).await.unwrap();
        assert!(!stale.exists(), "the leftover spill directory is removed");
        let entries: Vec<String> = std::fs::read_dir(dir.path().join("tmp").join("query"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert!(entries[0].starts_with("datafusion-"), "{entries:?}");
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failed_sync_is_never_visible() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(Storage::open(config_in(dir.path())).await.unwrap());
        storage
            .append_spans(project(), spans_named(&[1], "kept"))
            .await
            .unwrap();
        storage.hooks().arm_fail(HookPoint::WalSync);
        let append = {
            let storage = Arc::clone(&storage);
            tokio::spawn(async move {
                storage
                    .append_spans(project(), spans_named(&[2], "lost"))
                    .await
            })
        };
        while !append.is_finished() {
            let names = span_names(
                storage
                    .query(project(), Default::default())
                    .await
                    .unwrap()
                    .spans,
            )
            .await;
            assert!(!names.iter().any(|name| name == "lost"));
            tokio::task::yield_now().await;
        }
        assert_eq!(append.await.unwrap().unwrap_err(), AppendError::WalFailed);
        let names = span_names(
            storage
                .query(project(), Default::default())
                .await
                .unwrap()
                .spans,
        )
        .await;
        assert_eq!(names, vec!["kept".to_owned()]);
        let shutdown = storage.shutdown().await.unwrap_err();
        assert!(matches!(
            shutdown,
            StorageError::WalFailed { table: "spans" }
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_during_appends_keeps_only_acknowledged_rows() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path());
        let storage = Arc::new(Storage::open(config.clone()).await.unwrap());
        let mut tasks = Vec::new();
        for index in 0..8_u8 {
            let storage = Arc::clone(&storage);
            tasks.push(tokio::spawn(async move {
                // A distinct span id per append: a flush keeps one row per
                // `(trace_id, span_id)`, so equal ids would merge the rows.
                let batch = spans_keyed(&[i64::from(index)], &format!("s{index}"), index + 1);
                storage.append_spans(project(), batch).await
            }));
        }
        tokio::task::yield_now().await;
        storage.shutdown().await.unwrap();
        let mut acknowledged = 0;
        for task in tasks {
            match task.await.unwrap() {
                Ok(_) => acknowledged += 1,
                Err(AppendError::ShuttingDown) => {}
                Err(error) => panic!("{error}"),
            }
        }
        drop(storage);
        let storage = Storage::open(config).await.unwrap();
        assert_eq!(storage.replayed(Table::Spans), 0);
        assert_eq!(storage.published_rows(Table::Spans), 0);
        assert_eq!(
            storage.catalog_rows(Table::Spans),
            u64::try_from(acknowledged).unwrap_or(u64::MAX)
        );
        storage.shutdown().await.unwrap();
    }

    async fn wait_for_catalog(storage: &Storage, table: Table, rows: u64) {
        let started = std::time::Instant::now();
        loop {
            if storage.catalog_rows(table) == rows && storage.published_rows(table) == 0 {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "flush did not commit {rows} rows"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_aged_memtable_is_flushed_and_skipped_on_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_in(dir.path());
        config.flush.max_age = Duration::from_millis(100);
        let storage = Storage::open(config.clone()).await.unwrap();
        let first = storage.query(project(), Default::default()).await.unwrap();
        let second = storage.query(project(), Default::default()).await.unwrap();
        // Each query has a runtime of its own (spec 03 §6).
        assert!(!Arc::ptr_eq(&first.runtime, &second.runtime));
        let receipt = storage
            .append_spans(project(), spans_at(&[1]))
            .await
            .unwrap();
        let started = std::time::Instant::now();
        wait_for_catalog(&storage, Table::Spans, 1).await;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(storage.flushed_seq(Table::Spans).await, receipt.seq);
        let files = storage.catalog_files(Table::Spans);
        assert_eq!(files.len(), 1);
        assert!(!files[0].fully_merged);
        assert_eq!(files[0].level, 0);
        let object = dir.path().join("objects").join(&files[0].path);
        assert_eq!(object.metadata().unwrap().len(), files[0].size);
        assert!(storage.staged_paths().await.unwrap().is_empty());
        storage.shutdown().await.unwrap();

        let storage = Storage::open(config).await.unwrap();
        assert_eq!(storage.replayed(Table::Spans), 0);
        assert_eq!(storage.published_rows(Table::Spans), 0);
        assert_eq!(storage.catalog_rows(Table::Spans), 1);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_large_memtable_flushes_without_waiting_for_age() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_in(dir.path());
        config.flush.max_bytes = ByteSize::mib(1);
        config.flush.max_age = Duration::from_secs(60 * 60);
        let storage = Storage::open(config).await.unwrap();
        let started = std::time::Instant::now();
        storage
            .append_spans(project(), spans_named(&[1], &"m".repeat(1_200_000)))
            .await
            .unwrap();
        wait_for_catalog(&storage, Table::Spans, 1).await;
        assert!(started.elapsed() < Duration::from_secs(10));
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_flushes_before_returning() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_in(dir.path());
        config.flush.max_age = Duration::from_secs(60 * 60);
        let storage = Storage::open(config.clone()).await.unwrap();
        storage
            .append_spans(project(), spans_at(&[1]))
            .await
            .unwrap();
        storage.shutdown().await.unwrap();
        let storage = Storage::open(config).await.unwrap();
        assert_eq!(storage.replayed(Table::Spans), 0);
        assert_eq!(storage.published_rows(Table::Spans), 0);
        assert_eq!(storage.catalog_rows(Table::Spans), 1);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn truncation_deletes_covered_segments_and_keeps_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_in(dir.path());
        config.wal.segment_bytes = ByteSize::mib(1);
        config.flush.max_bytes = ByteSize::mib(1);
        config.flush.max_age = Duration::from_secs(60 * 60);
        config.memtable.hard_limit = ByteSize::mib(64);
        let storage = Storage::open(config).await.unwrap();
        storage.hooks().hold(HookPoint::FlushAfterFreeze);
        let big = incompressible(2 * 1024 * 1024);
        storage
            .append_spans(project(), spans_named(&[1], &big))
            .await
            .unwrap();
        storage
            .append_spans(project(), spans_named(&[2], &big))
            .await
            .unwrap();
        let before = span_segments(dir.path());
        assert!(before.len() >= 2, "{before:?}");
        storage.hooks().release(HookPoint::FlushAfterFreeze);
        storage.shutdown().await.unwrap();
        let after = span_segments(dir.path());
        let oldest = before.iter().min().unwrap();
        let newest = before.iter().max().unwrap();
        assert!(!after.contains(oldest), "deleted {oldest:?} from {after:?}");
        assert!(after.contains(newest), "kept {newest:?} in {after:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_flush_retries_failed_uploads_without_duplicating_rows() {
        let dir = tempfile::tempdir().unwrap();
        let inner = Arc::new(object_store::memory::InMemory::new());
        let fault = test_support::FaultStore::new(Arc::clone(&inner) as _);
        fault.enqueue(test_support::FaultKind::FailBefore);
        fault.enqueue(test_support::FaultKind::FailBefore);
        let mut config = config_in(dir.path());
        config.flush.max_age = Duration::from_millis(100);
        config.flush.upload_concurrency = 1;
        let storage = Storage::open_with_store(config, Arc::new(fault))
            .await
            .unwrap();
        storage
            .append_spans(project(), spans_at(&[1]))
            .await
            .unwrap();
        wait_for_catalog(&storage, Table::Spans, 1).await;
        let committed: Vec<String> = storage
            .catalog_files(Table::Spans)
            .iter()
            .map(|file| file.path.clone())
            .collect();
        // Each failed attempt staged a spans file and its traces file.
        let staged = storage.staged_paths().await.unwrap();
        assert_eq!(staged.len(), 4, "{staged:?}");
        assert!(committed.iter().all(|path| !staged.contains(path)));
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_ambiguous_upload_is_retried_under_a_new_name() {
        let dir = tempfile::tempdir().unwrap();
        let inner = Arc::new(object_store::memory::InMemory::new());
        let fault = test_support::FaultStore::new(Arc::clone(&inner) as _);
        fault.enqueue(test_support::FaultKind::FailAfter);
        let mut config = config_in(dir.path());
        config.flush.max_age = Duration::from_millis(100);
        config.flush.upload_concurrency = 1;
        let storage = Storage::open_with_store(config, Arc::new(fault))
            .await
            .unwrap();
        storage
            .append_spans(project(), spans_at(&[1]))
            .await
            .unwrap();
        wait_for_catalog(&storage, Table::Spans, 1).await;
        let committed = storage.catalog_files(Table::Spans);
        assert_eq!(committed.len(), 1);
        // One failed attempt staged a spans file and its traces file. With
        // serial uploads the ambiguous failure hits the first (spans) object,
        // so only that object was written before the error.
        let staged = storage.staged_paths().await.unwrap();
        assert_eq!(staged.len(), 2, "{staged:?}");
        assert!(staged.iter().all(|path| path != &committed[0].path));
        let spans_staged = staged
            .iter()
            .find(|path| path.starts_with("tables/spans/"))
            .unwrap();
        inner
            .get(&object_store::path::Path::from(spans_staged.as_str()))
            .await
            .unwrap();
        inner
            .get(&object_store::path::Path::from(committed[0].path.as_str()))
            .await
            .unwrap();
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_flush_writes_a_traces_file_per_spans_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_in(dir.path());
        config.flush.max_age = Duration::from_millis(100);
        let storage = Storage::open(config.clone()).await.unwrap();
        storage
            .append_spans(project(), three_traces_two_days())
            .await
            .unwrap();
        wait_for_catalog(&storage, Table::Spans, 3).await;
        assert_eq!(storage.catalog_paths(FileTable::Spans).len(), 2);
        assert_eq!(storage.catalog_paths(FileTable::Traces).len(), 2);
        let pairs = source_pairs(dir.path()).await;
        assert_eq!(pairs.len(), 2, "{pairs:?}");
        for (spans_path, traces_path) in &pairs {
            let spans_batch = read_parquet(dir.path(), spans_path);
            let expected = partials_from_spans(&spans_batch).unwrap();
            let actual = read_parquet(dir.path(), traces_path);
            assert_same_rows(&actual, &expected, traces_path);
        }
        storage.shutdown().await.unwrap();

        // Reopening loads the catalog (the open-time sources check passes) and
        // the traces files are in the published read state.
        let storage = Storage::open(config).await.unwrap();
        assert_eq!(storage.catalog_paths(FileTable::Spans).len(), 2);
        assert_eq!(storage.catalog_paths(FileTable::Traces).len(), 2);
        assert_eq!(source_pairs(dir.path()).await.len(), 2);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_ambiguous_traces_upload_is_retried_under_new_names() {
        let dir = tempfile::tempdir().unwrap();
        let inner = Arc::new(object_store::memory::InMemory::new());
        let fault = test_support::FaultStore::new(Arc::clone(&inner) as _);
        // Serial uploads: the spans upload consumes the delay, the traces
        // upload writes the object and then fails ambiguously.
        fault.enqueue(test_support::FaultKind::Delay(Duration::ZERO));
        fault.enqueue(test_support::FaultKind::FailAfter);
        let mut config = config_in(dir.path());
        config.flush.max_age = Duration::from_millis(100);
        config.flush.upload_concurrency = 1;
        let storage = Storage::open_with_store(config, Arc::new(fault))
            .await
            .unwrap();
        storage
            .append_spans(project(), spans_at(&[1]))
            .await
            .unwrap();
        wait_for_catalog(&storage, Table::Spans, 1).await;
        let committed = [
            storage.catalog_paths(FileTable::Spans),
            storage.catalog_paths(FileTable::Traces),
        ]
        .concat();
        assert_eq!(committed.len(), 2, "{committed:?}");
        let staged = storage.staged_paths().await.unwrap();
        assert_eq!(staged.len(), 2, "{staged:?}");
        assert!(staged.iter().all(|path| !committed.contains(path)));
        // The first attempt's spans object succeeded and its traces object was
        // written before the ambiguous failure; both are staged orphans.
        for path in &staged {
            inner
                .get(&object_store::path::Path::from(path.as_str()))
                .await
                .unwrap();
        }
        storage.collect_staged(Duration::ZERO).await.unwrap();
        assert!(storage.staged_paths().await.unwrap().is_empty());
        for path in &staged {
            assert!(
                inner
                    .get(&object_store::path::Path::from(path.as_str()))
                    .await
                    .is_err(),
                "{path} survived collection"
            );
        }
        for path in &committed {
            inner
                .get(&object_store::path::Path::from(path.as_str()))
                .await
                .unwrap();
        }
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn failed_uploads_backpressure_until_the_store_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let inner = Arc::new(object_store::memory::InMemory::new());
        let fault = test_support::FaultStore::new(Arc::clone(&inner) as _);
        fault.hold_failures();
        let mut config = config_in(dir.path());
        config.memtable.hard_limit = ByteSize::mib(16);
        config.flush.max_bytes = ByteSize::mib(1);
        config.flush.max_age = Duration::from_secs(60 * 60);
        config.flush.upload_concurrency = 1;
        let storage = Storage::open_with_store(config, Arc::new(fault.clone()))
            .await
            .unwrap();
        let payload = incompressible(1024 * 1024);
        let mut accepted = 0_i64;
        loop {
            match storage
                .append_spans(project(), spans_named(&[accepted], &payload))
                .await
            {
                Ok(_) => accepted += 1,
                Err(AppendError::Backpressure) => break,
                Err(error) => panic!("{error}"),
            }
            assert!(accepted < 64, "backpressure never tripped");
        }
        assert!(accepted > 0);
        fault.release();
        let started = std::time::Instant::now();
        loop {
            match storage.append_spans(project(), spans_at(&[99])).await {
                Ok(_) => break,
                Err(AppendError::Backpressure) => {
                    assert!(started.elapsed() < Duration::from_secs(30));
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => panic!("{error}"),
            }
        }
        storage.shutdown().await.unwrap();
    }

    fn incompressible(len: usize) -> String {
        let mut out = String::with_capacity(len);
        let mut state = 0x1234_5678_u64;
        for _ in 0..len {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            out.push(char::from(b'a' + ((state >> 33) as u8 % 26)));
        }
        out
    }

    fn span_segments(data_dir: &Path) -> Vec<std::path::PathBuf> {
        let mut paths = Vec::new();
        for entry in std::fs::read_dir(data_dir.join("wal/spans")).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            if parse_segment_name(&name.to_string_lossy()).is_some() {
                paths.push(entry.path());
            }
        }
        paths
    }
}
