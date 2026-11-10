//! Columnar storage for spans and scores: a write-ahead log, an in-memory
//! memtable, and DataFusion table providers.
//!
//! See `docs/spec/03-storage.md`. The design and milestones are in
//! `docs/plans/a1-storage/00-design.md`.
//!
//! # M1 is not usable in production
//!
//! The server must not wire this crate in yet. There is no flush, so:
//!
//! - the WAL is never truncated and grows until the process restarts;
//! - the memtable only grows; once it reaches `storage.memtable.hard_limit`,
//!   every append answers [`error::AppendError::Backpressure`];
//! - shutdown does not flush: data stays in the WAL and is replayed at the next open.

#![warn(missing_docs)]

use std::fs::{File, OpenOptions};
use std::io;
use std::num::NonZeroUsize;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use arrow::array::{Array, RecordBatch, StringArray};
use llmobs_core::ids::ProjectId;

use crate::config::StorageConfig;
use crate::config::WalSync;
use crate::error::{AppendError, MemtableError, OpenError, QueryError, StorageError, WalError};
use crate::hooks::Hooks;
use crate::memtable::Memtable;
use crate::read_state::{Published, ReadState};
use crate::wal::Table;
use crate::wal::sync_dir;
use crate::wal::writer::{WalWriterHandle, WriterExit, WriterParams, spawn};
use crate::wal::{SegmentWriter, replay};

pub mod config;
pub mod error;
pub mod query;

pub use query::ProjectQuery;

pub(crate) mod hooks;
pub(crate) mod memtable;
pub(crate) mod read_state;
pub(crate) mod wal;

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
    shutting_down: AtomicBool,
    lock: File,
    /// Published memtables. `query` loads one snapshot for both providers.
    published: Arc<Published>,
    spans: WalWriterHandle,
    scores: WalWriterHandle,
    /// Shared with the writers. Tests arm faults here; production hooks are empty.
    #[allow(dead_code)]
    hooks: Hooks,
}

impl Storage {
    /// Creates the directory layout, takes `data_dir/LOCK`, replays both WALs,
    /// and starts the writers.
    ///
    /// Returns only after replay, so an append cannot run before recovery.
    /// `after_seq` is 0 until M2 reads `flushed_seq` from `catalog.db`. Replay
    /// may push a memtable past `hard_limit`; later appends then answer
    /// [`AppendError::Backpressure`] until the process restarts. M1 has no flush,
    /// so it cannot make room.
    pub async fn open(config: StorageConfig) -> Result<Self, OpenError> {
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
        };
        #[allow(clippy::default_constructed_unit_structs)]
        let hooks = Hooks::default();
        let for_disk = share_hooks(&hooks);
        let recovered = tokio::task::spawn_blocking(move || recover(&config, &for_disk))
            .await
            .map_err(|_| {
                open_io(
                    "opening data directory",
                    &data_dir,
                    io::Error::other("open task panicked"),
                )
            })??;

        let published = Arc::new(Published::new(ReadState {
            spans: recovered.spans.memtable.snapshot(),
            scores: recovered.scores.memtable.snapshot(),
        }));
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
        Ok(Self {
            hard_limit,
            target_partitions,
            shutting_down: AtomicBool::new(false),
            lock: recovered.lock,
            published,
            spans,
            scores,
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
    /// M1's runtime is the default; M6 installs the memory pool.
    pub fn query(&self, project: ProjectId) -> Result<ProjectQuery, QueryError> {
        let state = self.published.load();
        let partitions = self.target_partitions.get();
        let spans = Arc::new(query::SpansProvider::new(
            project,
            Arc::clone(&state.spans),
            partitions,
        ));
        let scores = Arc::new(query::ScoresProvider::new(
            project,
            Arc::clone(&state.scores),
            partitions,
        ));
        Ok(ProjectQuery {
            spans,
            scores,
            runtime: Arc::new(datafusion::execution::runtime_env::RuntimeEnv::default()),
            target_partitions: self.target_partitions,
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

    /// Stops both writers and releases the data directory lock.
    ///
    /// M1 does not flush. Acknowledged rows stay in the WAL and are replayed
    /// on the next [`Self::open`]. M2 flushes the memtables here.
    pub async fn shutdown(self) -> Result<(), StorageError> {
        self.shutting_down.store(true, Ordering::Release);
        let spans = self.spans.shutdown().await;
        let scores = self.scores.shutdown().await;
        drop(self.lock);
        writer_exit("spans", &spans)?;
        writer_exit("scores", &scores)?;
        Ok(())
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
    fn published_rows(&self, table: Table) -> usize {
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
}

struct Recovered {
    lock: File,
    spans: RecoveredTable,
    scores: RecoveredTable,
}

struct RecoveredTable {
    memtable: Memtable,
    segment: SegmentWriter,
    next_seq: u64,
}

fn recover(config: &StorageConfig, hooks: &Hooks) -> Result<Recovered, OpenError> {
    let data_dir = &config.data_dir;
    ensure_dir(data_dir)?;
    let wal = data_dir.join("wal");
    ensure_dir(&wal)?;
    let spans_dir = wal.join(Table::Spans.name());
    let scores_dir = wal.join(Table::Scores.name());
    ensure_dir(&spans_dir)?;
    ensure_dir(&scores_dir)?;
    let lock = lock_dir(data_dir)?;
    let spans = recover_table(&spans_dir, Table::Spans, config, hooks)?;
    let scores = recover_table(&scores_dir, Table::Scores, config, hooks)?;
    Ok(Recovered {
        lock,
        spans,
        scores,
    })
}

fn recover_table(
    dir: &Path,
    table: Table,
    config: &StorageConfig,
    hooks: &Hooks,
) -> Result<RecoveredTable, OpenError> {
    let mut memtable = Memtable::new(table);
    // M2 replaces this 0 with `flushed_seq` from catalog.db.
    let outcome = replay(
        dir,
        table,
        0,
        config.wal.skip_corrupt,
        &mut |_seq, batch| {
            if batch.num_rows() == 0 {
                return Ok(());
            }
            let project = project_of(&batch)?;
            memtable
                .insert(project, batch)
                .map_err(|err| memtable_to_wal(&err))
        },
    )
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
    let segment = SegmentWriter::create(
        dir,
        outcome.next_seq,
        config.wal.segment_bytes.0,
        share_hooks(hooks),
    )?;
    Ok(RecoveredTable {
        memtable,
        segment,
        next_seq: outcome.next_seq,
    })
}

struct WriterTune {
    sync: WalSync,
    group_max_bytes: u64,
    group_max_wait: std::time::Duration,
    segment_bytes: u64,
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

    use arrow::array::StringArray;
    use arrow::datatypes::{DataType, Field, Schema};
    use bytesize::ByteSize;
    use llmobs_core::batch::scores_to_record_batch;
    use llmobs_core::enums::ScoreSource;
    use llmobs_core::ids::{ProjectId, TraceId};
    use llmobs_core::schema::spans_schema;
    use llmobs_core::score::{Score, ScoreTarget, ScoreValue};

    use super::*;
    use crate::config::{MemtableConfig, QueryConfig, WalConfig};
    use crate::error::StorageError;
    use crate::hooks::HookPoint;
    use crate::wal::{parse_segment_name, spans_at, spans_named};

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn other_project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000002".parse().unwrap()
    }

    fn config_in(dir: &Path) -> StorageConfig {
        StorageConfig {
            data_dir: dir.to_path_buf(),
            wal: WalConfig::default(),
            memtable: MemtableConfig::default(),
            query: QueryConfig::default(),
        }
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
        assert_eq!(storage.published_rows(Table::Spans), 1);
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
        storage.shutdown().await.unwrap();
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
        storage.shutdown().await.unwrap();

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
