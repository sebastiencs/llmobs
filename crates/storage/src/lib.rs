//! Columnar storage for spans and scores: a write-ahead log, an in-memory
//! memtable, Parquet files, and a SQLite catalog, queried through DataFusion.
//!
//! See `docs/spec/03-storage.md`. The design and milestones are in
//! `docs/plans/a1-storage/00-design.md`.
//!
//! # What this crate does not do yet
//!
//! - retention, media blobs and project drop are absent; deletion hides rows
//!   from queries, and its flush, compaction and rewriter erasures land next;
//! - the query runtime uses DataFusion's default memory pool.
//!   Parquet footers and page indexes use DataFusion's built-in metadata cache.

#![warn(missing_docs)]

use std::fs::{File, OpenOptions};
use std::io;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use arrow::array::{Array, RecordBatch, StringArray};
use datafusion::execution::runtime_env::{RuntimeEnv, RuntimeEnvBuilder};
use llmobs_core::ids::ProjectId;
use uuid::Uuid;

use crate::catalog::{Committer, Registered};
use crate::config::StorageConfig;
use crate::config::WalSync;
use crate::error::{
    AppendError, DeletionError, MemtableError, OpenError, QueryError, StorageError, WalError,
};
use crate::flush::FlushHandle;
use crate::hooks::Hooks;
use crate::memtable::Memtable;
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
pub mod error;
mod filter;
pub(crate) mod flush;
pub(crate) mod gc;
pub(crate) mod objects;
pub(crate) mod parquet;
pub(crate) mod partials;
pub mod query;
mod query_error;
mod query_text;
mod reads;
mod sql;
mod views;

pub use crate::compaction::job::CompactionReport;
pub use crate::deletion::{Deletion, DeletionScope, DeletionStatus};
pub use crate::error::CompactionError;
pub use crate::gc::ObsoleteReport;
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
    /// Shared by the background task and [`Self::compact`].
    compactor: Arc<compaction::job::Compactor>,
    compaction: compaction::task::CompactionHandle,
    /// Shared with the flush tasks and staged-object collection.
    #[cfg_attr(not(test), allow(dead_code))]
    objects: Arc<Objects>,
    /// Kept so tests can read `flushed_seq`. The flush tasks hold their own clones.
    #[cfg_attr(not(test), allow(dead_code))]
    committer: Arc<Committer>,
    runtime: Arc<RuntimeEnv>,
    /// Records replayed into the memtable at open. A flushed record is not counted.
    #[cfg_attr(not(test), allow(dead_code))]
    replayed: [u64; 2],
    /// Shared with the writers and the flush tasks. Tests arm faults here.
    #[allow(dead_code)]
    hooks: Hooks,
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
        let runtime = build_runtime(config.query.metadata_cache_bytes.0)?;
        objects.register(&runtime);
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
        let mut anchors = anchor::Anchors::new(window_ns);
        anchors.rebuild(&catalog, &objects).await?;
        let anchors = Arc::new(Mutex::new(anchors));
        let published = Arc::new(Published::new(ReadState {
            spans: recovered.spans.memtable.snapshot(),
            scores: recovered.scores.memtable.snapshot(),
            catalog,
            tombstones,
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
                Arc::clone(&published),
                compaction::merge::MergeContext {
                    runtime,
                    batch_size: 8192,
                },
                compaction::plan::Policy::from(&config.compaction),
                config.compaction.threads,
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
        let gc = gc::spawn(gc::GcStart {
            committer: Arc::clone(&committer),
            objects: Arc::clone(&objects),
            hooks: share_hooks(&hooks),
            grace: config.gc.grace,
        });
        let compaction =
            compaction::task::spawn(Arc::clone(&compactor), config.compaction.interval);
        Ok(Self {
            hard_limit,
            target_partitions,
            lookback,
            shutting_down: AtomicBool::new(false),
            lock: Mutex::new(Some(lock)),
            published,
            spans,
            scores,
            spans_flush,
            scores_flush,
            gc,
            compactor,
            compaction,
            objects,
            committer,
            runtime,
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

    /// Providers for `project`, both bound to one published snapshot.
    ///
    /// No DataFusion session is built. The query layer builds one from
    /// [`ProjectQuery::runtime`] and [`ProjectQuery::target_partitions`].
    /// The runtime is the one built at open. Its memory pool stays the default
    /// until a later milestone installs one.
    ///
    /// The snapshot's pending deletions hide their rows from every provider
    /// (spec 03 §7): the memtable snapshots are filtered copies, and the
    /// providers carry the tombstones for the file side.
    pub fn query(
        &self,
        project: ProjectId,
        options: QueryOptions,
    ) -> Result<ProjectQuery, QueryError> {
        let state = self.published.load();
        let partitions = self.target_partitions.get();
        let deletions = state.tombstones.for_project(project);
        let (spans_snapshot, scores_snapshot) = if deletions.is_empty() {
            (Arc::clone(&state.spans), Arc::clone(&state.scores))
        } else {
            (
                deletion::filter_snapshot(Table::Spans, &state.spans, project, &deletions)?,
                deletion::filter_snapshot(Table::Scores, &state.scores, project, &deletions)?,
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
            self.lookback,
            partitions,
            deletions,
        ));
        Ok(ProjectQuery {
            spans,
            traces,
            scores,
            runtime: Arc::clone(&self.runtime),
            target_partitions: self.target_partitions,
            optimizer_rules: vec![Arc::new(query::TraceListRule)],
        })
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

    /// Commits `edit` and publishes the catalog, for tests that race a job.
    #[cfg(test)]
    pub(crate) async fn commit_test_edit(
        &self,
        edit: &catalog::CatalogEdit,
    ) -> Result<Arc<catalog::Catalog>, error::CommitError> {
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
        let writer = self.writer(table);
        if writer.shared().failed() {
            return Err(AppendError::WalFailed);
        }
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(AppendError::ShuttingDown);
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
    }
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
    lock_dir(data_dir)
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

fn build_runtime(metadata_cache_bytes: u64) -> Result<Arc<RuntimeEnv>, OpenError> {
    let limit = usize::try_from(metadata_cache_bytes).unwrap_or(usize::MAX);
    let runtime = RuntimeEnvBuilder::new()
        .with_metadata_cache_limit(limit)
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
    use crate::config::{FlushConfig, MemtableConfig, QueryConfig, S3Config, WalConfig};
    use crate::error::StorageError;
    use crate::hooks::HookPoint;
    use crate::partials::partials_from_spans;
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
            object_store: None,
            s3: S3Config::default(),
            wal: WalConfig::default(),
            flush: FlushConfig::default(),
            memtable: MemtableConfig::default(),
            query: QueryConfig::default(),
            anchor: Default::default(),
            gc: Default::default(),
            compaction: Default::default(),
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
        let during = storage.query(project(), Default::default()).unwrap();
        assert!(span_names(Arc::clone(&during.spans)).await.is_empty());
        storage.hooks().release(HookPoint::AfterWalSync);
        append.await.unwrap().unwrap();
        let after = storage.query(project(), Default::default()).unwrap();
        assert_eq!(span_names(after.spans).await.len(), 1);
        assert!(span_names(during.spans).await.is_empty());
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
            let names =
                span_names(storage.query(project(), Default::default()).unwrap().spans).await;
            assert!(!names.iter().any(|name| name == "lost"));
            tokio::task::yield_now().await;
        }
        assert_eq!(append.await.unwrap().unwrap_err(), AppendError::WalFailed);
        let names = span_names(storage.query(project(), Default::default()).unwrap().spans).await;
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
        let first = storage.query(project(), Default::default()).unwrap();
        let second = storage.query(project(), Default::default()).unwrap();
        assert!(Arc::ptr_eq(&first.runtime, &second.runtime));
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
