//! One flush task per table.
//!
//! The task freezes the active memtable, writes one level-0 Parquet file per
//! partition, commits those files, and truncates WAL segments the commit
//! covers. A failure before the commit keeps the frozen view and retries with
//! new file names. Truncation failure is remembered and retried without
//! writing the files again.

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use arrow::array::{RecordBatch, TimestampNanosecondArray};
use arrow::compute::{concat_batches, min};
use arrow::datatypes::SchemaRef;
use bytes::Bytes;
use llmobs_core::ids::ProjectId;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

use crate::anchor::{AnchorPlan, Anchors};
use crate::catalog::{Catalog, CatalogEdit, Committer, FileId, FileMeta, FileTable};
use crate::dedup;
use crate::deletion::{Tombstones, applicable, covered_predicate, drop_covered};
use crate::error::FlushError;
use crate::hooks::{HookPoint, Hooks};
#[cfg(test)]
use crate::memtable::MemBatch;
use crate::memtable::{FrozenView, Memtable, PartitionKey};
use crate::objects::Objects;
use crate::parquet::{WrittenFile, write_file};
use crate::partials::partials_from_spans;
use crate::projects::ProjectStates;
use crate::read_state::{Published, ReadState};
use crate::wal::writer::WriterShared;
use crate::wal::{Table, truncate_segments};

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// How often an idle task looks for a memtable the size notify did not report.
///
/// The writer notifies only at `flush.max_bytes`. A smaller memtable still has
/// to start its age deadline.
const IDLE_POLL: Duration = Duration::from_millis(50);

struct PreparedFile {
    meta: FileMeta,
    bytes: Bytes,
}

/// Files to stage and write, plus the `(traces, spans)` pairs for the commit.
///
/// `files` is in partition order; each spans file is immediately followed by
/// its traces file.
struct Prepared {
    files: Vec<PreparedFile>,
    sources: Vec<(FileId, FileId)>,
    /// Planned anchor changes, applied only after the commit succeeds.
    anchor_plan: Option<AnchorPlan>,
}

struct FlushParams {
    table: Table,
    dir: PathBuf,
    shared: Arc<WriterShared>,
    committer: Arc<Committer>,
    objects: Arc<Objects>,
    published: Arc<Published>,
    hooks: Hooks,
    max_bytes: u64,
    max_age: Duration,
    upload_concurrency: usize,
    /// Present only on the spans flush task.
    anchors: Option<Arc<Mutex<Anchors>>>,
    stop: watch::Receiver<bool>,
    notify: Arc<Notify>,
}

/// What one flush task needs. Split out so `spawn` stays under the argument limit.
#[derive(Debug)]
pub(crate) struct FlushStart {
    /// Table this task flushes.
    pub table: Table,
    /// WAL directory for `table`.
    pub dir: PathBuf,
    /// Writer state, including the memtable and the size notify.
    pub shared: Arc<WriterShared>,
    /// Catalog writer. Shared with the other table's task.
    pub committer: Arc<Committer>,
    /// Where the Parquet files are written.
    pub objects: Arc<Objects>,
    /// Published memtables and catalog.
    pub published: Arc<Published>,
    /// Pause and fault points.
    pub hooks: Hooks,
    /// `flush.max_bytes`.
    pub max_bytes: u64,
    /// `flush.max_age`.
    pub max_age: Duration,
    /// `flush.upload_concurrency`.
    pub upload_concurrency: usize,
    /// The spans task's anchor state. `None` for scores.
    pub anchors: Option<Arc<Mutex<Anchors>>>,
}

/// A running flush task. Shutdown asks it to finish; drop cancels it.
#[derive(Debug)]
pub(crate) struct FlushHandle {
    stop: watch::Sender<bool>,
    notify: Arc<Notify>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl FlushHandle {
    /// Wakes the task and tells it to flush once more, then exit.
    pub(crate) fn request_stop(&self) {
        let _ = self.stop.send(true);
        self.notify.notify_waiters();
    }

    /// Takes the task so the caller can wait for it.
    pub(crate) fn take_task(&self) -> Option<JoinHandle<()>> {
        self.task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Cancels the task. A task already taken by shutdown is left alone.
    pub(crate) fn abort(&self) {
        self.request_stop();
        if let Some(task) = self.take_task() {
            task.abort();
        }
    }
}

/// Starts the task. It loops until [`FlushHandle::request_stop`] and the
/// memtable has been committed, retrying a failed flush until then.
pub(crate) fn spawn(start: FlushStart) -> FlushHandle {
    let (stop_tx, stop) = watch::channel(false);
    let notify = Arc::clone(&start.shared.flush_notify);
    let params = FlushParams {
        table: start.table,
        dir: start.dir,
        shared: start.shared,
        committer: start.committer,
        objects: start.objects,
        published: start.published,
        hooks: start.hooks,
        max_bytes: start.max_bytes,
        max_age: start.max_age,
        upload_concurrency: start.upload_concurrency,
        anchors: start.anchors,
        stop,
        notify: Arc::clone(&notify),
    };
    let task = tokio::spawn(async move {
        let work: std::pin::Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(run(params));
        work.await;
    });
    FlushHandle {
        stop: stop_tx,
        notify,
        task: Mutex::new(Some(task)),
    }
}

async fn run(mut params: FlushParams) {
    let mut pending: Option<u64> = None;
    let mut backoff = INITIAL_BACKOFF;
    loop {
        if let Some(seq) = pending {
            match truncate_flushed(&params, seq).await {
                Ok(()) => pending = None,
                Err(error) => {
                    log_truncate(params.table, &error);
                    sleep_backoff(&mut params.stop, backoff).await;
                    backoff = next_backoff(backoff);
                    continue;
                }
            }
        }
        match flush_once(&params).await {
            Ok(None) => {
                backoff = INITIAL_BACKOFF;
                if stopping(&params.stop) {
                    break;
                }
                wait_until_due(&params).await;
            }
            Ok(Some(seq)) => {
                backoff = INITIAL_BACKOFF;
                match truncate_flushed(&params, seq).await {
                    Ok(()) => {}
                    Err(error) => {
                        log_truncate(params.table, &error);
                        pending = Some(seq);
                    }
                }
            }
            Err(_) => {
                sleep_backoff(&mut params.stop, backoff).await;
                backoff = next_backoff(backoff);
            }
        }
    }
}

fn stopping(stop: &watch::Receiver<bool>) -> bool {
    *stop.borrow()
}

fn next_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(MAX_BACKOFF)
}

async fn sleep_backoff(stop: &mut watch::Receiver<bool>, backoff: Duration) {
    tokio::select! {
        () = tokio::time::sleep(backoff) => {}
        result = stop.changed() => {
            let _ = result;
        }
    }
}

async fn wait_until_due(params: &FlushParams) {
    loop {
        if stopping(&params.stop) {
            return;
        }
        let notified = params.notify.notified();
        tokio::pin!(notified);
        let (ready, due) = flush_wait(&params.shared, params.max_bytes, params.max_age);
        if ready {
            return;
        }
        let sleep_for = due.unwrap_or(IDLE_POLL);
        let sleep = tokio::time::sleep(sleep_for);
        tokio::pin!(sleep);
        let mut stop = params.stop.clone();
        tokio::select! {
            () = &mut notified => {}
            () = &mut sleep => {}
            result = stop.changed() => {
                let _ = result;
            }
        }
    }
}

fn flush_due(memtable: &Memtable, params: &FlushParams) -> bool {
    if memtable.has_frozen() || stopping(&params.stop) {
        return true;
    }
    let over_size = u64::try_from(memtable.active_bytes()).unwrap_or(u64::MAX) >= params.max_bytes;
    let aged = memtable
        .active_oldest()
        .is_some_and(|oldest| oldest.elapsed() >= params.max_age);
    over_size || aged
}

fn flush_wait(
    shared: &WriterShared,
    max_bytes: u64,
    max_age: Duration,
) -> (bool, Option<Duration>) {
    let memtable = shared
        .memtable()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let over_size = u64::try_from(memtable.active_bytes()).unwrap_or(u64::MAX) >= max_bytes;
    let aged = memtable
        .active_oldest()
        .is_some_and(|oldest| oldest.elapsed() >= max_age);
    let ready = memtable.has_frozen() || over_size || aged;
    let due = memtable
        .active_oldest()
        .map(|oldest| max_age.saturating_sub(oldest.elapsed()));
    (ready, due)
}

/// `Some(seq)` was committed. `None` means there was nothing to freeze.
async fn flush_once(params: &FlushParams) -> Result<Option<u64>, FlushError> {
    let (fresh, view) = {
        let mut memtable = params
            .shared
            .memtable()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let triggered = flush_due(&memtable, params);
        let fresh = !memtable.has_frozen();
        let view = triggered.then(|| memtable.freeze()).flatten();
        (fresh && view.is_some(), view)
    };
    let Some(view) = view else {
        return Ok(None);
    };
    let state = params.published.load();
    let tombstones = Arc::clone(&state.tombstones);
    let projects = Arc::clone(&state.projects);
    let partitions = view.partitions.len();
    let max_seq = view.max_seq;
    if fresh {
        params.hooks.pause(HookPoint::FlushAfterFreeze);
        if params.hooks.fail(HookPoint::FlushAfterFreeze).is_some() {
            return Err(log_retry(params.table, partitions, FlushError::Fault));
        }
    }
    let table = params.table;
    let anchors = params.anchors.clone();
    let prepared = match tokio::task::spawn_blocking(move || {
        prepare(table, &view, anchors.as_deref(), &tombstones, &projects)
    })
    .await
    {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(error)) => return Err(log_retry(table, partitions, error)),
        Err(_) => return Err(log_retry(table, partitions, FlushError::Task)),
    };
    let Prepared {
        files,
        sources,
        anchor_plan,
    } = prepared;
    // A flush left with no file still commits: every row was covered by a
    // pending deletion (spec 03 §7), and the sequence must advance so the WAL
    // segments it covers can go.
    if !files.is_empty() {
        let paths: Vec<String> = files.iter().map(|file| file.meta.path.clone()).collect();
        if let Err(error) = params.committer.stage(&paths).await {
            return Err(log_retry(
                params.table,
                partitions,
                FlushError::stage(error),
            ));
        }
        params.hooks.pause(HookPoint::FlushAfterStage);
        if params.hooks.fail(HookPoint::FlushAfterStage).is_some() {
            return Err(log_retry(params.table, partitions, FlushError::Fault));
        }
        if let Err(error) = put_all(&params.objects, &files, params.upload_concurrency).await {
            return Err(log_retry(params.table, partitions, error));
        }
        params.hooks.pause(HookPoint::FlushAfterWrite);
        if params.hooks.fail(HookPoint::FlushAfterWrite).is_some() {
            return Err(log_retry(params.table, partitions, FlushError::Fault));
        }
    }
    let edit = CatalogEdit {
        add: files.into_iter().map(|file| file.meta).collect(),
        remove: Vec::new(),
        flushed_seq: Some((params.table, max_seq)),
        sources,
        raise: Vec::new(),
        quarantine: Vec::new(),
    };
    let published = Arc::clone(&params.published);
    let shared = Arc::clone(&params.shared);
    let table = params.table;
    if let Err(error) = params
        .committer
        .commit(&edit, move |catalog| {
            publish_flush(table, &shared, &published, catalog);
        })
        .await
    {
        return Err(log_retry(table, partitions, FlushError::commit(error)));
    }
    // The commit is durable. Only now may the anchor state change, so a failed
    // attempt retries from the same state and assigns the same days.
    if let (Some(anchors), Some(plan)) = (&params.anchors, anchor_plan) {
        anchors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .commit(plan);
    }
    params.hooks.pause(HookPoint::FlushAfterCommit);
    if let Some(error) = params.hooks.fail(HookPoint::FlushAfterCommit) {
        // The commit is durable. Retrying it would write a second copy.
        tracing::warn!(
            table = params.table.name(),
            %error,
            "flush commit hook failed after the catalog commit"
        );
    }
    Ok(Some(max_seq))
}

fn log_retry(table: Table, partitions: usize, error: FlushError) -> FlushError {
    tracing::warn!(
        table = table.name(),
        partitions,
        %error,
        "flush failed; retrying"
    );
    error
}

fn publish_flush(
    table: Table,
    shared: &WriterShared,
    published: &Published,
    catalog: &Arc<Catalog>,
) {
    // Lock order: memtable, then `Published`. The committer mutex is already held.
    let mut memtable = shared
        .memtable()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    memtable.drop_frozen();
    let snapshot = memtable.snapshot();
    let bytes = u64::try_from(memtable.bytes()).unwrap_or(u64::MAX);
    published.replace(|state| match table {
        Table::Spans => ReadState {
            spans: Arc::clone(&snapshot),
            scores: Arc::clone(&state.scores),
            catalog: Arc::clone(catalog),
            tombstones: Arc::clone(&state.tombstones),
            projects: Arc::clone(&state.projects),
        },
        Table::Scores => ReadState {
            spans: Arc::clone(&state.spans),
            scores: Arc::clone(&snapshot),
            catalog: Arc::clone(catalog),
            tombstones: Arc::clone(&state.tombstones),
            projects: Arc::clone(&state.projects),
        },
    });
    shared.set_memtable_bytes(bytes);
}

fn prepare(
    table: Table,
    view: &FrozenView,
    anchors: Option<&Mutex<Anchors>>,
    tombstones: &Tombstones,
    projects: &ProjectStates,
) -> Result<Prepared, FlushError> {
    let file_table = FileTable::from(table);
    let known_seq = tombstones.known_seq();
    let mut files = Vec::new();
    let mut sources = Vec::new();
    let mut anchor_plan = None;
    match table {
        Table::Spans => {
            let anchors = anchors.ok_or_else(|| FlushError::dedup("missing anchor state"))?;
            let (partitions, plan) = anchors
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .partition(view)?;
            anchor_plan = Some(plan);
            for partition in partitions {
                // A partition of a day the project's retention watermark has
                // dropped never reaches a file (spec 03 §8).
                if projects.drops(partition.key.project, partition.key.day) {
                    continue;
                }
                // Rows covered when the flush starts never reach a file
                // (spec 03 §7). The partition's rows go through the filter
                // before the duplicate collapsing.
                let min_ingest = min_ingest_time(&partition.batch, table)?;
                let batch = uncovered_rows(
                    file_table,
                    &llmobs_core::schema::spans_schema(),
                    &partition.batch,
                    tombstones,
                    partition.key.project,
                    min_ingest,
                )?;
                if batch.num_rows() == 0 {
                    continue;
                }
                let batch = dedup::latest_spans(&batch)
                    .map_err(|error| FlushError::dedup(error.reason()))?;
                push_spans_files(&mut files, &mut sources, &partition.key, batch, known_seq)?;
            }
        }
        Table::Scores => {
            for (key, batches) in view.partitions.iter() {
                if projects.drops(key.project, key.day) {
                    continue;
                }
                if batches.is_empty() {
                    continue;
                }
                let refs: Vec<&RecordBatch> =
                    batches.iter().map(|batch| batch.batch.as_ref()).collect();
                let combined = concat_batches(&llmobs_core::schema::scores_schema(), refs)
                    .map_err(|source| {
                        FlushError::prepare(crate::error::ParquetWriteError::arrow(source))
                    })?;
                let min_ingest = min_ingest_time(&combined, table)?;
                let combined = uncovered_rows(
                    file_table,
                    &llmobs_core::schema::scores_schema(),
                    &combined,
                    tombstones,
                    key.project,
                    min_ingest,
                )?;
                if combined.num_rows() == 0 {
                    continue;
                }
                let written = write_file(file_table, 0, &[Arc::new(combined)])
                    .map_err(FlushError::prepare)?;
                let (path, id) = Objects::table_file_path(file_table, key.project, key.day, 0);
                files.push(prepared_file(id, file_table, key, path, written, known_seq));
            }
        }
    }
    Ok(Prepared {
        files,
        sources,
        anchor_plan,
    })
}

/// The minimum `ingest_time` of `batch`. An empty, missing or all-null column
/// fails the flush: the covered-row rule cannot skip the batch.
fn min_ingest_time(batch: &RecordBatch, table: Table) -> Result<i64, FlushError> {
    let column = table.ingest_column();
    let missing = || FlushError::dedup("missing ingest time");
    let array = batch
        .column_by_name(column)
        .ok_or_else(missing)?
        .as_any()
        .downcast_ref::<TimestampNanosecondArray>()
        .ok_or_else(missing)?;
    min(array).ok_or_else(missing)
}

/// The rows of `batch` no pending deletion of `project` covers. Batches no
/// deletion can cover are returned as they are.
fn uncovered_rows(
    table: FileTable,
    schema: &SchemaRef,
    batch: &RecordBatch,
    tombstones: &Tombstones,
    project: ProjectId,
    min_ingest: i64,
) -> Result<RecordBatch, FlushError> {
    let deletions = applicable(&tombstones.for_project(project), None, min_ingest);
    if deletions.is_empty() {
        return Ok(batch.clone());
    }
    let Some(predicate) =
        covered_predicate(table, schema, &deletions).map_err(|_| FlushError::Deletion {
            reason: "building the deletion filter",
        })?
    else {
        return Ok(batch.clone());
    };
    drop_covered(batch, &predicate).map_err(|_| FlushError::Deletion {
        reason: "applying the deletion filter",
    })
}

/// Writes a spans file and the traces partials derived from exactly its rows.
fn push_spans_files(
    files: &mut Vec<PreparedFile>,
    sources: &mut Vec<(FileId, FileId)>,
    key: &PartitionKey,
    batch: RecordBatch,
    known_seq: u64,
) -> Result<(), FlushError> {
    let batch = Arc::new(batch);
    let spans_written =
        write_file(FileTable::Spans, 0, &[Arc::clone(&batch)]).map_err(FlushError::prepare)?;
    let partials = partials_from_spans(&batch).map_err(FlushError::partials)?;
    let traces_written =
        write_file(FileTable::Traces, 0, &[Arc::new(partials)]).map_err(FlushError::prepare)?;
    let (spans_path, spans_id) =
        Objects::table_file_path(FileTable::Spans, key.project, key.day, 0);
    let (traces_path, traces_id) =
        Objects::table_file_path(FileTable::Traces, key.project, key.day, 0);
    files.push(prepared_file(
        spans_id,
        FileTable::Spans,
        key,
        spans_path,
        spans_written,
        known_seq,
    ));
    files.push(prepared_file(
        traces_id,
        FileTable::Traces,
        key,
        traces_path,
        traces_written,
        known_seq,
    ));
    sources.push((traces_id, spans_id));
    Ok(())
}

fn prepared_file(
    id: FileId,
    table: FileTable,
    key: &PartitionKey,
    path: String,
    written: WrittenFile,
    known_seq: u64,
) -> PreparedFile {
    let size = u64::try_from(written.bytes.len()).unwrap_or(u64::MAX);
    PreparedFile {
        meta: FileMeta {
            id,
            table,
            project: key.project,
            day: key.day,
            level: 0,
            path,
            size,
            footer_size: written.footer_size,
            rows: written.rows,
            min_time: written.min_time,
            max_time: written.max_time,
            min_ingest_time: written.min_ingest_time,
            max_ingest_time: written.max_ingest_time,
            fully_merged: false,
            deletions_applied_seq: known_seq,
        },
        bytes: written.bytes,
    }
}

/// Keeps the greatest `ingest_time` for each `(trace_id, span_id)`.
///
/// Batches are in WAL order. An equal `ingest_time` keeps the later row.
#[cfg(test)]
fn dedup_spans(batches: &[MemBatch]) -> Result<RecordBatch, FlushError> {
    let schema = batches[0].batch.schema();
    let refs: Vec<&RecordBatch> = batches.iter().map(|batch| batch.batch.as_ref()).collect();
    let combined = concat_batches(&schema, refs)
        .map_err(|source| FlushError::prepare(crate::error::ParquetWriteError::arrow(source)))?;
    dedup::latest_spans(&combined).map_err(|error| FlushError::dedup(error.reason()))
}

async fn put_all(
    objects: &Arc<Objects>,
    files: &[PreparedFile],
    concurrency: usize,
) -> Result<(), FlushError> {
    let concurrency = concurrency.max(1);
    let mut start = 0;
    while start < files.len() {
        let end = (start + concurrency).min(files.len());
        put_chunk(objects, &files[start..end]).await?;
        start = end;
    }
    Ok(())
}

async fn put_chunk(objects: &Arc<Objects>, files: &[PreparedFile]) -> Result<(), FlushError> {
    let mut set = tokio::task::JoinSet::new();
    for file in files {
        let objects = Arc::clone(objects);
        let path = file.meta.path.clone();
        let bytes = file.bytes.clone();
        set.spawn(async move { objects.put_durable(&path, bytes).await });
    }
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok(Ok(())) => {}
            Ok(Err(error)) => return Err(FlushError::write(error)),
            Err(_) => return Err(FlushError::Task),
        }
    }
    Ok(())
}

async fn truncate_flushed(params: &FlushParams, seq: u64) -> Result<(), std::io::Error> {
    let dir = params.dir.clone();
    let outcome = tokio::task::spawn_blocking(move || truncate_segments(&dir, seq))
        .await
        .map_err(std::io::Error::other)??;
    params.hooks.pause(HookPoint::FlushAfterTruncate);
    if let Some(error) = params.hooks.fail(HookPoint::FlushAfterTruncate) {
        tracing::warn!(
            table = params.table.name(),
            deleted = outcome.deleted,
            %error,
            "flush truncate hook failed after the directory was updated"
        );
    }
    Ok(())
}

fn log_truncate(table: Table, error: &std::io::Error) {
    tracing::warn!(
        table = table.name(),
        %error,
        "wal truncate failed; retrying on the next flush"
    );
}

#[cfg(test)]
mod tests {
    use bytesize::ByteSize;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use arrow::array::Array;
    use chrono::{Datelike, NaiveDate};
    use llmobs_core::ids::ProjectId;

    use super::dedup_spans;
    use crate::Storage;
    use crate::anchor::DAY_NS;
    use crate::catalog::FileTable;
    use crate::config::{
        FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig, WalConfig,
    };
    use crate::hooks::HookPoint;
    use crate::memtable::MemBatch;
    use crate::test_support::{object_rows, trace_ids_of};
    use crate::wal::{Table, spans_named};

    fn batch(starts: &[i64], name: &str) -> MemBatch {
        MemBatch::from_batch(Table::Spans, spans_named(starts, name)).unwrap()
    }

    fn names(batch: &arrow::array::RecordBatch) -> Vec<String> {
        let column = batch
            .column_by_name(llmobs_core::schema::spans::NAME)
            .unwrap()
            .as_any()
            .downcast_ref::<arrow::array::StringArray>()
            .unwrap();
        (0..column.len())
            .map(|index| column.value(index).to_owned())
            .collect()
    }

    #[test]
    fn dedup_keeps_the_greatest_ingest_time_and_the_later_tie() {
        let older = batch(&[10], "old");
        let newer = batch(&[500], "new");
        let kept = dedup_spans(&[older, newer]).unwrap();
        assert_eq!(names(&kept), vec!["new".to_owned()]);

        let first = batch(&[10], "first");
        let second = batch(&[10], "second");
        let tied = dedup_spans(&[first, second]).unwrap();
        assert_eq!(names(&tied), vec!["second".to_owned()]);

        let both = batch(&[10, 11], "both");
        let distinct = dedup_spans(&[both]).unwrap();
        assert_eq!(distinct.num_rows(), 2);
    }

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 3, 5).unwrap()
    }

    fn at(day: NaiveDate, hour: i64, minute: i64, second: i64) -> i64 {
        (i64::from(day.num_days_from_ce()) - 719_163) * DAY_NS
            + (hour * 3_600 + minute * 60 + second) * 1_000_000_000
    }

    fn anchor_config(dir: &Path) -> StorageConfig {
        StorageConfig {
            data_dir: dir.to_path_buf(),
            min_free_bytes: ByteSize::b(0),
            object_store: None,
            s3: S3Config::default(),
            wal: WalConfig::default(),
            flush: FlushConfig {
                max_age: Duration::from_millis(100),
                ..FlushConfig::default()
            },
            memtable: MemtableConfig::default(),
            query: QueryConfig::default(),
            anchor: Default::default(),
            gc: Default::default(),
            compaction: Default::default(),
            deletion: Default::default(),
        }
    }

    /// A slow-age config: nothing flushes until shutdown, so a test can
    /// register a deletion before the flush starts.
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

    /// One `Traces` deletion of the given traces, cut off at `cutoff`.
    fn traces_deletion(traces: &[u16], cutoff: i64) -> crate::deletion::Deletion {
        crate::deletion::Deletion {
            id: uuid::Uuid::now_v7(),
            project: project(),
            cutoff,
            scope: crate::deletion::DeletionScope::Traces {
                trace_ids: traces
                    .iter()
                    .map(|trace| crate::test_support::span_row(*trace, 0, 0, 0).trace_id)
                    .collect(),
            },
        }
    }

    fn values_of(rows: &arrow::array::RecordBatch, column: &str) -> Vec<String> {
        use arrow::array::Array;

        let column = rows.column_by_name(column).unwrap();
        let array = column
            .as_any()
            .downcast_ref::<arrow::array::StringArray>()
            .unwrap();
        (0..array.len())
            .map(|row| array.value(row).to_owned())
            .collect()
    }

    async fn wait_rows(storage: &Storage, rows: u64) {
        let started = Instant::now();
        loop {
            if storage.catalog_rows(Table::Spans) == rows
                && storage.published_rows(Table::Spans) == 0
            {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "flush did not commit {rows} rows"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn span_days(storage: &Storage) -> Vec<NaiveDate> {
        storage
            .catalog_files(Table::Spans)
            .iter()
            .map(|file| file.day)
            .collect()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failed_commit_retries_with_the_same_anchor_days() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(anchor_config(dir.path())).await.unwrap();
        // Arm the failure on the FIRST flush — the one that creates the anchor
        // entry. A failed attempt must not write it; the retry recomputes the
        // same days with an untouched anchor state and commits it.
        storage.hooks().arm_fail(HookPoint::CatalogCommit);
        let early = at(day(), 23, 59, 50);
        storage
            .append_spans(project(), spans_named(&[early], "a"))
            .await
            .unwrap();
        wait_rows(&storage, 1).await;
        let late = at(day().succ_opt().unwrap(), 0, 0, 10);
        storage
            .append_spans(project(), spans_named(&[late], "b"))
            .await
            .unwrap();
        wait_rows(&storage, 2).await;
        assert_eq!(span_days(&storage), vec![day(), day()]);
        // Both a spans and a traces file are committed for each day.
        assert_eq!(storage.catalog_paths(FileTable::Spans).len(), 2);
        assert_eq!(storage.catalog_paths(FileTable::Traces).len(), 2);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn rebuild_uses_a_partials_end_time_near_midnight() {
        let dir = tempfile::tempdir().unwrap();
        let config = anchor_config(dir.path());
        let storage = Storage::open(config.clone()).await.unwrap();
        // One flush: root at 22:00, child at 23:30. At runtime the child's
        // start makes the entry; the partial's start_time is 22:00, so only
        // its end_time (23:30) restores the entry after a restart.
        let root = at(day(), 22, 0, 0);
        let child = at(day(), 23, 30, 0);
        storage
            .append_spans(project(), spans_named(&[root, child], "a"))
            .await
            .unwrap();
        wait_rows(&storage, 2).await;
        drop(storage);
        let storage = Storage::open(config).await.unwrap();
        let late = at(day().succ_opt().unwrap(), 0, 0, 10);
        storage
            .append_spans(project(), spans_named(&[late], "b"))
            .await
            .unwrap();
        wait_rows(&storage, 3).await;
        assert_eq!(span_days(&storage), vec![day(), day()]);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn restart_rebuilds_the_anchor_entry() {
        let dir = tempfile::tempdir().unwrap();
        let config = anchor_config(dir.path());
        let storage = Storage::open(config.clone()).await.unwrap();
        let early = at(day(), 23, 59, 50);
        storage
            .append_spans(project(), spans_named(&[early], "a"))
            .await
            .unwrap();
        wait_rows(&storage, 1).await;
        drop(storage);
        // The rebuild reads the traces file and restores the entry, so the
        // late row after the reopen still lands in day D.
        let storage = Storage::open(config).await.unwrap();
        assert_eq!(span_days(&storage), vec![day()]);
        let late = at(day().succ_opt().unwrap(), 0, 0, 10);
        storage
            .append_spans(project(), spans_named(&[late], "b"))
            .await
            .unwrap();
        wait_rows(&storage, 2).await;
        assert_eq!(span_days(&storage), vec![day(), day()]);
        storage.shutdown().await.unwrap();
    }

    /// The ingest time the deletion tests use for start and `ingest_time`.
    fn ingest() -> i64 {
        at(day(), 12, 0, 0)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_flush_drops_the_rows_a_pending_deletion_covers() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path());
        let storage = Storage::open(config.clone()).await.unwrap();
        let rows = crate::test_support::span_rows(&[
            crate::test_support::span_row(1, 0, ingest(), ingest()),
            crate::test_support::span_row(2, 0, ingest(), ingest()),
        ]);
        storage.append_spans(project(), rows).await.unwrap();
        storage
            .register_deletion(traces_deletion(&[1], ingest()))
            .await
            .unwrap();
        storage.shutdown().await.unwrap();

        let storage = Storage::open(config).await.unwrap();
        let spans_files = storage.catalog_files(Table::Spans);
        assert_eq!(spans_files.len(), 1);
        assert_eq!(spans_files[0].deletions_applied_seq, 1);
        assert_eq!(
            trace_ids_of(&object_rows(dir.path(), &spans_files[0])),
            vec![
                *crate::test_support::span_row(2, 0, 0, 0)
                    .trace_id
                    .as_bytes()
            ],
            "the file holds only trace 2"
        );
        let traces_files = storage.catalog_files(FileTable::Traces);
        assert_eq!(traces_files.len(), 1);
        assert_eq!(traces_files[0].rows, 1, "one partial, for trace 2");
        assert_eq!(traces_files[0].deletions_applied_seq, 1);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_flush_where_every_row_is_covered_commits_only_its_sequence() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path());
        let storage = Storage::open(config.clone()).await.unwrap();
        let rows = crate::test_support::span_rows(&[crate::test_support::span_row(
            1,
            0,
            ingest(),
            ingest(),
        )]);
        let receipt = storage.append_spans(project(), rows).await.unwrap();
        storage
            .register_deletion(traces_deletion(&[1], ingest()))
            .await
            .unwrap();
        storage.shutdown().await.unwrap();

        let storage = Storage::open(config).await.unwrap();
        assert!(storage.catalog_files(Table::Spans).is_empty());
        assert!(storage.catalog_files(FileTable::Traces).is_empty());
        assert_eq!(storage.catalog_rows(Table::Spans), 0);
        // The empty flush advanced the sequence and dropped the frozen rows:
        // nothing replays at open, so the WAL it covered is gone for good.
        assert_eq!(storage.flushed_seq(Table::Spans).await, receipt.seq);
        assert_eq!(storage.published_rows(Table::Spans), 0);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_flush_keeps_a_session_score_and_drops_a_covered_one() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path());
        let storage = Storage::open(config.clone()).await.unwrap();
        let covered = crate::test_support::score_row(
            0x11,
            1,
            ingest(),
            ingest(),
            ingest(),
            false,
            "on-trace-1",
        );
        let mut session = crate::test_support::score_row(
            0x22,
            0,
            ingest(),
            ingest(),
            ingest(),
            false,
            "session-score",
        );
        session.target = llmobs_core::score::ScoreTarget::Session {
            session_id: "s".to_owned(),
        };
        storage
            .append_scores(
                project(),
                crate::test_support::score_rows(&[covered, session]),
            )
            .await
            .unwrap();
        storage
            .register_deletion(traces_deletion(&[1], ingest()))
            .await
            .unwrap();
        storage.shutdown().await.unwrap();

        let storage = Storage::open(config).await.unwrap();
        let files = storage.catalog_files(Table::Scores);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].deletions_applied_seq, 1);
        assert_eq!(
            values_of(
                &object_rows(dir.path(), &files[0]),
                llmobs_core::schema::scores::NAME
            ),
            vec!["session-score".to_owned()],
            "the score on trace 1 is not flushed; the session score is"
        );
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_flush_prepared_before_a_registration_keeps_its_rows() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(anchor_config(dir.path())).await.unwrap();
        let hooks = storage.hooks().clone();
        hooks.hold(HookPoint::FlushAfterStage);
        storage
            .append_spans(
                project(),
                crate::test_support::span_rows(&[crate::test_support::span_row(
                    1,
                    0,
                    ingest(),
                    ingest(),
                )]),
            )
            .await
            .unwrap();
        let started = Instant::now();
        while storage.hooks().waiting_at(HookPoint::FlushAfterStage) == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "flush did not reach FlushAfterStage"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Registered while the flush is parked: it was prepared before the
        // registration, so the file keeps the row and records no sequence.
        storage
            .register_deletion(traces_deletion(&[1], ingest()))
            .await
            .unwrap();
        hooks.release(HookPoint::FlushAfterStage);

        let started = Instant::now();
        loop {
            let files = storage.catalog_files(Table::Spans);
            if files.len() == 1 && storage.published_rows(Table::Spans) == 0 {
                assert_eq!(files[0].deletions_applied_seq, 0);
                assert_eq!(
                    trace_ids_of(&object_rows(dir.path(), &files[0])),
                    vec![
                        *crate::test_support::span_row(1, 0, 0, 0)
                            .trace_id
                            .as_bytes()
                    ],
                    "the file was prepared before the registration"
                );
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "flush did not commit"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // The tombstones still hide the row: the file is gated at query time.
        let query = storage.query(project(), Default::default()).await.unwrap();
        let ctx = datafusion::execution::context::SessionContext::new_with_config_rt(
            datafusion::execution::context::SessionConfig::new(),
            Arc::clone(&query.runtime),
        );
        ctx.register_table("spans_t", query.spans).unwrap();
        let counted = ctx
            .sql("SELECT count(*) FROM spans_t")
            .await
            .unwrap()
            .collect()
            .await
            .unwrap();
        let total = counted[0]
            .column(0)
            .as_any()
            .downcast_ref::<arrow::array::Int64Array>()
            .unwrap()
            .value(0);
        assert_eq!(total, 0);
        storage.shutdown().await.unwrap();
    }
}
