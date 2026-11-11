//! One writer task per table.
//!
//! Spec 03 §1. Appends are queued, grouped, written, and acknowledged in
//! sequence order. The task owns the open segment and is the only inserter
//! into the memtable. A write or sync that fails ends the task: the group is
//! truncated, its acks are `WalFailed`, and nothing is retried.

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use arrow::array::RecordBatch;
use llmobs_core::ids::ProjectId;
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::AppendReceipt;
use crate::config::WalSync;
use crate::error::{AppendError, WalError};
use crate::hooks::{HookPoint, Hooks};
use crate::memtable::Memtable;
use crate::read_state::{Published, ReadState};
use crate::wal::Table;
use crate::wal::record::encode;
use crate::wal::segment::SegmentWriter;

/// Queue depth for appends waiting on the writer.
///
/// A full channel makes `append` wait. That is the backpressure for a burst
/// that outruns group commit.
pub(crate) const CHANNEL_CAPACITY: usize = 1024;

/// One append waiting for the writer.
pub(crate) struct AppendRequest {
    /// Project that owns the rows.
    pub project: ProjectId,
    /// Rows to write. The schema is the table's schema.
    pub batch: RecordBatch,
    /// [`RecordBatch::get_array_memory_size`], used for group limits.
    pub bytes: usize,
    /// Completed when the group is acknowledged, or when it fails.
    pub ack: oneshot::Sender<Result<AppendReceipt, AppendError>>,
}

/// State the append path reads without taking the memtable lock.
#[derive(Debug)]
pub(crate) struct WriterShared {
    failed: AtomicBool,
    memtable_bytes: AtomicU64,
    memtable: Mutex<Memtable>,
    /// Wakes the table's flush task once the active memtable reaches `flush.max_bytes`.
    ///
    /// A notify with no waiter is kept, so a flush that is between waits still
    /// sees it. A memtable that is already over the threshold at open does not
    /// notify until the next group; the flush task also reads `active_bytes`.
    pub(crate) flush_notify: Arc<Notify>,
}

impl WriterShared {
    /// The writer task has stopped on an I/O error.
    pub(crate) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    /// Bytes of the active and frozen memtables, for the backpressure check.
    pub(crate) fn memtable_bytes(&self) -> u64 {
        self.memtable_bytes.load(Ordering::Acquire)
    }

    /// Stores the byte count last computed under the memtable lock.
    pub(crate) fn set_memtable_bytes(&self, bytes: u64) {
        self.memtable_bytes.store(bytes, Ordering::Release);
    }

    /// The live memtable. Only the writer inserts.
    pub(crate) fn memtable(&self) -> &Mutex<Memtable> {
        &self.memtable
    }
}

/// Handle to a running writer task.
///
/// The sender and the task sit behind a mutex so [`Self::shutdown`] can run
/// while other tasks still call [`Self::append`].
#[derive(Debug)]
pub(crate) struct WalWriterHandle {
    tx: Mutex<Option<mpsc::Sender<AppendRequest>>>,
    state: Arc<WriterShared>,
    task: Mutex<Option<JoinHandle<()>>>,
}

/// Why [`WalWriterHandle::shutdown`] did not finish cleanly.
#[derive(Debug)]
pub(crate) enum WriterExit {
    /// The task stopped after a WAL error.
    Failed,
    /// The task panicked.
    Panicked,
}

impl WalWriterHandle {
    /// Shared flags and the memtable.
    pub(crate) fn shared(&self) -> &Arc<WriterShared> {
        &self.state
    }

    /// Queues `batch` and waits for its acknowledgement.
    ///
    /// A closed channel is `ShuttingDown` when the writer is healthy, and
    /// `WalFailed` when the task has already failed. A dropped ack is
    /// `WalFailed`.
    pub(crate) async fn append(
        &self,
        project: ProjectId,
        batch: RecordBatch,
        bytes: usize,
    ) -> Result<AppendReceipt, AppendError> {
        if self.state.failed() {
            return Err(AppendError::WalFailed);
        }
        let (ack, rx) = oneshot::channel();
        let request = AppendRequest {
            project,
            batch,
            bytes,
            ack,
        };
        let tx = {
            let guard = self.tx.lock().unwrap_or_else(PoisonError::into_inner);
            guard.as_ref().cloned()
        };
        let Some(tx) = tx else {
            return Err(if self.state.failed() {
                AppendError::WalFailed
            } else {
                AppendError::ShuttingDown
            });
        };
        if tx.send(request).await.is_err() {
            return Err(if self.state.failed() {
                AppendError::WalFailed
            } else {
                AppendError::ShuttingDown
            });
        }
        rx.await.unwrap_or(Err(AppendError::WalFailed))
    }

    /// Clones the queue sender. Dropping every clone is what ends the task.
    fn sender(&self) -> Option<mpsc::Sender<AppendRequest>> {
        self.tx
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .cloned()
    }

    /// Drops the sender and cancels the task.
    ///
    /// [`Storage`](crate::Storage)'s `Drop` uses this so a dropped value does
    /// not keep appending. Shutdown takes the task first; a later abort is a
    /// no-op.
    pub(crate) fn abort(&self) {
        drop(
            self.tx
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        if let Some(task) = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            task.abort();
        }
    }

    /// Drops the sender and waits for the task to drain and exit.
    ///
    /// Interval mode syncs a dirty segment before the task finishes. The
    /// caller still has to drop every other sender; this drops the one the
    /// handle owns. A second call finds the task already finished. The failure
    /// flag is read after the task exits, so a sync that fails while shutting
    /// down is reported.
    pub(crate) async fn shutdown(&self) -> Result<(), WriterExit> {
        let state = Arc::clone(&self.state);
        drop(
            self.tx
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        let task = self
            .task
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match task {
            None if state.failed() => Err(WriterExit::Failed),
            None => Ok(()),
            Some(task) => match task.await {
                Ok(()) if state.failed() => Err(WriterExit::Failed),
                Ok(()) => Ok(()),
                Err(_) => Err(WriterExit::Panicked),
            },
        }
    }
}

/// Inputs for [`spawn`]. The segment is already created at `next_seq`.
pub(crate) struct WriterParams {
    /// Table this task writes.
    pub table: Table,
    /// Directory of this table's segments.
    pub dir: PathBuf,
    /// Open segment named with `next_seq`.
    pub segment: SegmentWriter,
    /// Sequence number of the next record.
    pub next_seq: u64,
    /// When to call `fdatasync`.
    pub sync: WalSync,
    /// Soft cap on a group's `AppendRequest::bytes` sum.
    pub group_max_bytes: u64,
    /// How long to wait for more requests after the first of a group.
    pub group_max_wait: Duration,
    /// Rotate once a segment reaches this size. A group may exceed it.
    pub segment_bytes: u64,
    /// Notify the flush task when the active memtable reaches this many bytes.
    pub flush_max_bytes: u64,
    /// Pause and fault points. A no-op outside tests.
    pub hooks: Hooks,
    /// Read state this table publishes into.
    pub published: Arc<Published>,
    /// Memtable restored by replay, or empty.
    pub memtable: Memtable,
}

/// Spawns the writer on the current runtime.
pub(crate) fn spawn(mut params: WriterParams) -> WalWriterHandle {
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    let bytes = u64::try_from(params.memtable.bytes()).unwrap_or(u64::MAX);
    // `run` reads the memtable from `state`. The empty table left behind is dropped.
    let memtable = std::mem::replace(&mut params.memtable, Memtable::new(params.table));
    let state = Arc::new(WriterShared {
        failed: AtomicBool::new(false),
        memtable_bytes: AtomicU64::new(bytes),
        memtable: Mutex::new(memtable),
        flush_notify: Arc::new(Notify::new()),
    });
    let task_state = Arc::clone(&state);
    let task = tokio::spawn(run(params, rx, task_state));
    WalWriterHandle {
        tx: Mutex::new(Some(tx)),
        state,
        task: Mutex::new(Some(task)),
    }
}

struct Prepared {
    project: ProjectId,
    batch: RecordBatch,
    bytes: usize,
    rows: usize,
    seq: u64,
    ack: oneshot::Sender<Result<AppendReceipt, AppendError>>,
}

struct Written {
    segment: SegmentWriter,
    /// Previous segment, when this commit rotated. Its bytes past `mark` are
    /// the group, and they are not acknowledged yet.
    previous: Option<PathBuf>,
    mark: u64,
    /// `fdatasync` ran. In interval mode the new group itself is still dirty.
    did_sync: bool,
    /// The group was synced before the ack. Only `WalSync::Always`.
    group_synced: bool,
}

struct IoJob {
    segment: SegmentWriter,
    buf: Vec<u8>,
    sync_first: bool,
    sync_after: bool,
    dir: PathBuf,
    next_seq: u64,
    segment_bytes: u64,
    hooks: Hooks,
}

/// A segment kept after a failed write or sync, so the task can log and stop.
struct KeptSegment {
    segment: SegmentWriter,
    error: WalError,
}

impl IoJob {
    /// Writes the group. On failure the segment is back at `mark` unless the
    /// failure was a sync of already-acknowledged bytes, which adds nothing.
    fn commit(mut self) -> Result<Written, Box<KeptSegment>> {
        let mark = self.segment.len();
        if self.sync_first
            && let Err(error) = self.segment.sync()
        {
            return Err(Box::new(KeptSegment {
                segment: self.segment,
                error,
            }));
        }
        if let Err(error) = self.segment.write(&self.buf) {
            rewind_to(&mut self.segment, mark);
            return Err(Box::new(KeptSegment {
                segment: self.segment,
                error,
            }));
        }
        if self.sync_after
            && let Err(error) = self.segment.sync()
        {
            rewind_to(&mut self.segment, mark);
            return Err(Box::new(KeptSegment {
                segment: self.segment,
                error,
            }));
        }
        if self.segment.needs_rotation() {
            let old_path = self.segment.path();
            match SegmentWriter::create(
                &self.dir,
                self.next_seq,
                self.segment_bytes,
                copy_hooks(&self.hooks),
            ) {
                Ok(next) => {
                    return Ok(Written {
                        segment: next,
                        previous: Some(old_path),
                        mark,
                        did_sync: self.sync_first || self.sync_after,
                        group_synced: self.sync_after,
                    });
                }
                Err(error) => {
                    rewind_to(&mut self.segment, mark);
                    return Err(Box::new(KeptSegment {
                        segment: self.segment,
                        error,
                    }));
                }
            }
        }
        Ok(Written {
            segment: self.segment,
            previous: None,
            mark,
            did_sync: self.sync_first || self.sync_after,
            group_synced: self.sync_after,
        })
    }
}

/// Production hooks are `Copy`. Tests share one value and have to clone it.
fn copy_hooks(hooks: &Hooks) -> Hooks {
    #[allow(clippy::clone_on_copy)]
    hooks.clone()
}

fn rewind_to(segment: &mut SegmentWriter, mark: u64) {
    if segment.len() == mark {
        return;
    }
    if let Err(err) = segment.rewind(mark) {
        tracing::error!(%err, "truncating unacknowledged WAL group");
    }
}

/// Drops a group that was written and then not acknowledged.
fn discard_unacked(written: Written) {
    if let Some(old) = written.previous {
        let new_path = written.segment.path();
        drop(written.segment);
        if let Err(err) = truncate_path(&old, written.mark) {
            tracing::error!(%err, "truncating unacknowledged WAL group");
        }
        if let Err(err) = std::fs::remove_file(&new_path) {
            tracing::error!(%err, "removing empty WAL segment");
        }
        return;
    }
    let mut segment = written.segment;
    rewind_to(&mut segment, written.mark);
}

fn truncate_path(path: &Path, len: u64) -> Result<(), WalError> {
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|source| WalError::io("truncating WAL segment", path, source))?;
    file.set_len(len)
        .map_err(|source| WalError::io("truncating WAL segment", path, source))?;
    file.sync_all()
        .map_err(|source| WalError::io("syncing WAL segment", path, source))?;
    Ok(())
}

async fn sync_segment(mut segment: SegmentWriter) -> Result<SegmentWriter, Box<KeptSegment>> {
    let joined = tokio::task::spawn_blocking(move || match segment.sync() {
        Ok(()) => Ok(segment),
        Err(error) => Err(Box::new(KeptSegment { segment, error })),
    })
    .await;
    match joined {
        Ok(result) => result,
        // A panic in the blocking task is a bug. Propagate it; the segment
        // was dropped with the task. `spawn_blocking` is not cancelled.
        Err(err) => std::panic::resume_unwind(err.into_panic()),
    }
}

async fn run(
    params: WriterParams,
    mut rx: mpsc::Receiver<AppendRequest>,
    shared: Arc<WriterShared>,
) {
    let WriterParams {
        table,
        dir,
        segment,
        mut next_seq,
        sync,
        group_max_bytes,
        group_max_wait,
        segment_bytes,
        flush_max_bytes,
        hooks,
        published,
        memtable: _,
    } = params;
    let mut segment = segment;
    let mut pending: Option<AppendRequest> = None;
    // A fast burst must not sync every group. The interval is measured from
    // task start, then from each sync.
    let mut last_sync = tokio::time::Instant::now();
    let mut dirty = false;
    let interval = match sync {
        WalSync::Always => None,
        WalSync::Interval(duration) => Some(duration),
    };

    loop {
        let first = if let Some(request) = pending.take() {
            request
        } else {
            let sync_at = last_sync + interval.unwrap_or(Duration::from_secs(3600));
            let idle_sync = interval.is_some() && dirty;
            tokio::select! {
                biased;
                incoming = rx.recv() => {
                    match incoming {
                        Some(request) => request,
                        None => {
                            if dirty {
                                match sync_segment(segment).await {
                                    Ok(_) => {}
                                    Err(kept) => {
                                        segment = kept.segment;
                                        let err = kept.error;
                                        tracing::error!(
                                            table = table.name(),
                                            segment = segment.name(),
                                            %err,
                                            "WAL sync failed"
                                        );
                                        fail_queued(&shared, &mut rx, table, segment.name(), next_seq, next_seq);
                                    }
                                }
                            }
                            break;
                        }
                    }
                }
                _ = tokio::time::sleep_until(sync_at), if idle_sync => {
                    match sync_segment(segment).await {
                        Ok(synced) => {
                            segment = synced;
                            dirty = false;
                            last_sync = tokio::time::Instant::now();
                            continue;
                        }
                        Err(kept) => {
                            segment = kept.segment;
                            let err = kept.error;
                            tracing::error!(
                                table = table.name(),
                                segment = segment.name(),
                                %err,
                                "WAL sync failed"
                            );
                            fail_queued(&shared, &mut rx, table, segment.name(), next_seq, next_seq);
                            break;
                        }
                    }
                }
            }
        };

        let arrived = tokio::time::Instant::now();
        let deadline = arrived + group_max_wait;
        let mut requests = vec![first];
        let mut group_bytes = as_u64(requests[0].bytes);
        let mut closed = false;
        while group_bytes < group_max_bytes {
            match tokio::time::timeout_at(deadline, rx.recv()).await {
                Ok(Some(request)) => {
                    let next = group_bytes.saturating_add(as_u64(request.bytes));
                    // A group may exceed the cap only when it holds one request.
                    if next > group_max_bytes {
                        pending = Some(request);
                        break;
                    }
                    group_bytes = next;
                    requests.push(request);
                }
                Ok(None) => {
                    closed = true;
                    break;
                }
                Err(_elapsed) => break,
            }
        }

        let mut encoded = Vec::new();
        let mut prepared = Vec::with_capacity(requests.len());
        for request in requests {
            let seq = next_seq;
            let before = encoded.len();
            match encode(seq, &request.batch, &mut encoded) {
                Ok(()) => {
                    next_seq = next_seq.saturating_add(1);
                    prepared.push(Prepared {
                        project: request.project,
                        rows: request.batch.num_rows(),
                        batch: request.batch,
                        bytes: request.bytes,
                        seq,
                        ack: request.ack,
                    });
                }
                Err(_) => {
                    encoded.truncate(before);
                    let _ = request.ack.send(Err(AppendError::InvalidBatch {
                        reason: "unencodable batch",
                    }));
                }
            }
        }
        if prepared.is_empty() {
            if closed {
                break;
            }
            continue;
        }

        let first_seq = prepared[0].seq;
        let last_seq = prepared[prepared.len() - 1].seq;
        let sync_after = interval.is_none();
        let sync_first = match interval {
            Some(interval) => dirty && last_sync.elapsed() >= interval,
            None => false,
        };
        let segment_label = segment.name().to_owned();
        let job = IoJob {
            segment,
            buf: encoded,
            sync_first,
            sync_after,
            dir: dir.clone(),
            next_seq,
            segment_bytes,
            hooks: copy_hooks(&hooks),
        };
        let written = match tokio::task::spawn_blocking(move || job.commit()).await {
            Ok(Ok(written)) => written,
            Ok(Err(kept)) => {
                segment = kept.segment;
                let err = kept.error;
                tracing::error!(
                    table = table.name(),
                    segment = segment.name(),
                    first_seq,
                    last_seq,
                    %err,
                    "WAL group failed"
                );
                fail_group(&shared, prepared);
                fail_queued(&shared, &mut rx, table, segment.name(), first_seq, last_seq);
                break;
            }
            Err(_join) => {
                tracing::error!(
                    table = table.name(),
                    segment = segment_label.as_str(),
                    first_seq,
                    last_seq,
                    "WAL group failed"
                );
                fail_group(&shared, prepared);
                fail_queued(&shared, &mut rx, table, &segment_label, first_seq, last_seq);
                break;
            }
        };

        let noted_bytes = prepared
            .iter()
            .fold(0u64, |sum, item| sum.saturating_add(as_u64(item.bytes)));
        hooks.note_group(prepared.len(), noted_bytes);
        hooks.pause(HookPoint::AfterWalSync);

        if let Err(err) = insert_all(&shared, &prepared, flush_max_bytes) {
            tracing::error!(
                table = table.name(),
                segment = written.segment.name(),
                first_seq,
                last_seq,
                %err,
                "WAL group failed"
            );
            discard_unacked(written);
            fail_group(&shared, prepared);
            fail_queued(&shared, &mut rx, table, &segment_label, first_seq, last_seq);
            break;
        }
        hooks.pause(HookPoint::BeforePublish);
        publish(table, &shared, &published);
        hooks.pause(HookPoint::AfterPublish);
        for item in prepared {
            let _ = item.ack.send(Ok(AppendReceipt {
                seq: item.seq,
                rows: item.rows,
            }));
        }

        if written.did_sync {
            last_sync = tokio::time::Instant::now();
        }
        dirty = !written.group_synced;
        segment = written.segment;

        if closed {
            if dirty {
                match sync_segment(segment).await {
                    Ok(_) => {}
                    Err(kept) => {
                        segment = kept.segment;
                        let err = kept.error;
                        tracing::error!(
                            table = table.name(),
                            segment = segment.name(),
                            %err,
                            "WAL sync failed"
                        );
                        fail_queued(&shared, &mut rx, table, segment.name(), next_seq, next_seq);
                    }
                }
            }
            break;
        }
    }
}

fn insert_all(
    shared: &WriterShared,
    prepared: &[Prepared],
    flush_max_bytes: u64,
) -> Result<(), crate::error::MemtableError> {
    let notify = {
        let mut memtable = shared
            .memtable
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for item in prepared {
            memtable.insert(item.project, item.batch.clone(), item.seq)?;
        }
        let bytes = u64::try_from(memtable.bytes()).unwrap_or(u64::MAX);
        let active = u64::try_from(memtable.active_bytes()).unwrap_or(u64::MAX);
        shared.set_memtable_bytes(bytes);
        active >= flush_max_bytes
    };
    if notify {
        shared.flush_notify.notify_one();
    }
    Ok(())
}

fn publish(table: Table, shared: &WriterShared, published: &Published) {
    // Lock order: this table's memtable, then `Published`.
    // `Committer::commit` holds the committer mutex across its `publish`
    // closure, and that closure may take these two. Never take `Published`
    // first, and never take a memtable lock while waiting for the committer.
    let memtable = shared
        .memtable
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let snapshot = memtable.snapshot();
    published.replace(|state| match table {
        Table::Spans => ReadState {
            spans: snapshot.clone(),
            scores: Arc::clone(&state.scores),
            catalog: Arc::clone(&state.catalog),
        },
        Table::Scores => ReadState {
            spans: Arc::clone(&state.spans),
            scores: snapshot.clone(),
            catalog: Arc::clone(&state.catalog),
        },
    });
    drop(memtable);
}

fn fail_group(shared: &WriterShared, prepared: Vec<Prepared>) {
    // Set the flag before the acks, so a caller that observes `WalFailed`
    // also observes `failed`.
    shared.failed.store(true, Ordering::Release);
    for item in prepared {
        let _ = item.ack.send(Err(AppendError::WalFailed));
    }
}

fn fail_queued(
    shared: &WriterShared,
    rx: &mut mpsc::Receiver<AppendRequest>,
    table: Table,
    segment: &str,
    first_seq: u64,
    last_seq: u64,
) {
    shared.failed.store(true, Ordering::Release);
    tracing::error!(
        table = table.name(),
        segment,
        first_seq,
        last_seq,
        "WAL writer stopped"
    );
    while let Ok(request) = rx.try_recv() {
        let _ = request.ack.send(Err(AppendError::WalFailed));
    }
}

fn as_u64(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::Duration;

    use arrow::array::TimestampNanosecondArray;
    use llmobs_core::ids::ProjectId;
    use llmobs_core::schema::spans::START_TIME;

    use super::*;
    use crate::config::WalSync;
    use crate::hooks::HookPoint;
    use crate::read_state::{Published, ReadState};
    use crate::wal::replay::replay;
    use crate::wal::{Table, parse_segment_name, spans_at};

    struct Running {
        handle: WalWriterHandle,
        published: Arc<Published>,
        dir: tempfile::TempDir,
        hooks: Hooks,
    }

    fn open_writer(
        sync: WalSync,
        group_max_bytes: u64,
        group_max_wait: Duration,
        segment_bytes: u64,
    ) -> Running {
        open_writer_with_flush(
            sync,
            group_max_bytes,
            group_max_wait,
            segment_bytes,
            u64::MAX,
        )
    }

    fn open_writer_with_flush(
        sync: WalSync,
        group_max_bytes: u64,
        group_max_wait: Duration,
        segment_bytes: u64,
        flush_max_bytes: u64,
    ) -> Running {
        let dir = tempfile::tempdir().unwrap();
        let hooks = Hooks::default();
        let segment = SegmentWriter::create(dir.path(), 1, segment_bytes, hooks.clone()).unwrap();
        let published = Arc::new(Published::new(ReadState::empty()));
        let handle = spawn(WriterParams {
            table: Table::Spans,
            dir: dir.path().to_path_buf(),
            segment,
            next_seq: 1,
            sync,
            group_max_bytes,
            group_max_wait,
            segment_bytes,
            flush_max_bytes,
            hooks: hooks.clone(),
            published: Arc::clone(&published),
            memtable: Memtable::new(Table::Spans),
        });
        Running {
            handle,
            published,
            dir,
            hooks,
        }
    }

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    async fn append_at(handle: &WalWriterHandle, start: i64) -> Result<AppendReceipt, AppendError> {
        let batch = spans_at(&[start]);
        let bytes = batch.get_array_memory_size();
        handle.append(project(), batch, bytes).await
    }

    fn batch_bytes() -> u64 {
        u64::try_from(spans_at(&[0]).get_array_memory_size()).unwrap()
    }

    async fn wait_for(mut ready: impl FnMut() -> bool) {
        let start = std::time::Instant::now();
        while !ready() {
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "timed out waiting for the writer"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn published_rows(published: &Published) -> usize {
        published
            .load()
            .spans
            .partitions
            .values()
            .flatten()
            .map(|batch| batch.batch.num_rows())
            .sum()
    }

    fn memtable_starts(shared: &WriterShared) -> Vec<i64> {
        let guard = shared.memtable().lock().unwrap();
        let mut times = Vec::new();
        for batches in guard.partitions().values() {
            for batch in batches {
                let column = batch
                    .batch
                    .column_by_name(START_TIME)
                    .expect("start_time column");
                let array = column
                    .as_any()
                    .downcast_ref::<TimestampNanosecondArray>()
                    .expect("start_time is timestamp ns");
                for row in 0..array.len() {
                    times.push(array.value(row));
                }
            }
        }
        times
    }

    fn replay_seqs(dir: &Path) -> Vec<u64> {
        let mut seqs = Vec::new();
        replay(dir, Table::Spans, 0, false, &mut |seq, _batch| {
            seqs.push(seq);
            Ok(())
        })
        .unwrap();
        seqs
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_appends_land_in_seq_order() {
        let running = open_writer(
            WalSync::Always,
            u64::MAX,
            Duration::from_millis(50),
            64 * 1024 * 1024,
        );
        let handle = Arc::new(running.handle);
        let mut tasks = Vec::new();
        for start in 0..16 {
            let handle = Arc::clone(&handle);
            tasks.push(tokio::spawn(async move {
                let receipt = append_at(&handle, start).await.unwrap();
                (start, receipt)
            }));
        }
        let mut pairs = Vec::new();
        for task in tasks {
            pairs.push(task.await.unwrap());
        }
        pairs.sort_by_key(|(_, receipt)| receipt.seq);
        let seqs: Vec<u64> = pairs.iter().map(|(_, receipt)| receipt.seq).collect();
        assert_eq!(seqs, (1..=16).collect::<Vec<_>>());
        let expected: Vec<i64> = pairs.iter().map(|(start, _)| *start).collect();
        assert_eq!(memtable_starts(handle.shared()), expected);
        assert_eq!(
            handle.shared().memtable_bytes(),
            u64::try_from(handle.shared().memtable().lock().unwrap().bytes()).unwrap()
        );
        Arc::try_unwrap(handle).unwrap().shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn one_group_carries_several_requests() {
        let running = open_writer(
            WalSync::Always,
            u64::MAX,
            Duration::from_secs(1),
            64 * 1024 * 1024,
        );
        running.hooks.hold(HookPoint::AfterWalSync);
        let handle = Arc::new(running.handle);
        let mut tasks = Vec::new();
        for start in 0..8 {
            let handle = Arc::clone(&handle);
            tasks.push(tokio::spawn(async move {
                append_at(&handle, start).await.unwrap()
            }));
        }
        let hooks = running.hooks.clone();
        wait_for(|| hooks.groups().len() == 1 && hooks.waiting_at(HookPoint::AfterWalSync) > 0)
            .await;
        assert_eq!(hooks.groups()[0].0, 8);
        hooks.release(HookPoint::AfterWalSync);
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(hooks.groups().len(), 1);
        Arc::try_unwrap(handle).unwrap().shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_group_stays_within_group_max_bytes() {
        let size = batch_bytes();
        let running = open_writer(
            WalSync::Always,
            size * 2,
            Duration::from_millis(500),
            64 * 1024 * 1024,
        );
        let handle = Arc::new(running.handle);
        let mut tasks = Vec::new();
        for start in 0..4 {
            let handle = Arc::clone(&handle);
            tasks.push(tokio::spawn(async move {
                append_at(&handle, start).await.unwrap()
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        let groups = running.hooks.groups();
        assert_eq!(groups.len(), 2, "groups: {groups:?}");
        for (requests, bytes) in groups {
            assert_eq!(requests, 2);
            assert!(bytes <= size * 2);
        }
        Arc::try_unwrap(handle).unwrap().shutdown().await.unwrap();

        let running = open_writer(
            WalSync::Always,
            1,
            Duration::from_millis(10),
            64 * 1024 * 1024,
        );
        append_at(&running.handle, 1).await.unwrap();
        let groups = running.hooks.groups();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, 1);
        assert!(groups[0].1 > 1);
        running.handle.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_batch_is_visible_after_publish_and_before_the_ack() {
        let running = open_writer(
            WalSync::Always,
            u64::MAX,
            Duration::from_millis(20),
            64 * 1024 * 1024,
        );
        running.hooks.hold(HookPoint::AfterWalSync);
        running.hooks.hold(HookPoint::AfterPublish);
        let handle = Arc::new(running.handle);
        let append = {
            let handle = Arc::clone(&handle);
            tokio::spawn(async move { append_at(&handle, 5).await.unwrap() })
        };
        let hooks = running.hooks.clone();
        wait_for(|| hooks.waiting_at(HookPoint::AfterWalSync) > 0).await;
        assert_eq!(published_rows(&running.published), 0);
        hooks.release(HookPoint::AfterWalSync);
        wait_for(|| hooks.waiting_at(HookPoint::AfterPublish) > 0).await;
        assert_eq!(published_rows(&running.published), 1);
        assert!(!append.is_finished());
        hooks.release(HookPoint::AfterPublish);
        let receipt = append.await.unwrap();
        assert_eq!(receipt.rows, 1);
        assert_eq!(published_rows(&running.published), 1);
        Arc::try_unwrap(handle).unwrap().shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_write_error_acks_wal_failed_and_replay_keeps_acked_records() {
        let size = batch_bytes();
        let running = open_writer(
            WalSync::Always,
            size,
            Duration::from_millis(50),
            64 * 1024 * 1024,
        );
        let first = append_at(&running.handle, 1).await.unwrap();
        running.hooks.arm_fail(HookPoint::WalWrite);
        let handle = Arc::new(running.handle);
        let mut tasks = Vec::new();
        for start in 2..5 {
            let handle = Arc::clone(&handle);
            tasks.push(tokio::spawn(async move { append_at(&handle, start).await }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap(), Err(AppendError::WalFailed));
        }
        assert!(handle.shared().failed());
        assert_eq!(append_at(&handle, 9).await, Err(AppendError::WalFailed));
        assert_eq!(replay_seqs(running.dir.path()), vec![first.seq]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_sync_error_acks_wal_failed_and_replay_keeps_acked_records() {
        let size = batch_bytes();
        let running = open_writer(
            WalSync::Always,
            size,
            Duration::from_millis(50),
            64 * 1024 * 1024,
        );
        append_at(&running.handle, 1).await.unwrap();
        running.hooks.arm_fail(HookPoint::WalSync);
        let handle = Arc::new(running.handle);
        let mut tasks = Vec::new();
        for start in 2..5 {
            let handle = Arc::clone(&handle);
            tasks.push(tokio::spawn(async move { append_at(&handle, start).await }));
        }
        for task in tasks {
            assert_eq!(task.await.unwrap(), Err(AppendError::WalFailed));
        }
        assert!(handle.shared().failed());
        assert_eq!(replay_seqs(running.dir.path()), vec![1]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn interval_mode_syncs_while_idle_and_not_per_group() {
        let running = open_writer(
            WalSync::Interval(Duration::from_millis(200)),
            u64::MAX,
            Duration::from_millis(5),
            64 * 1024 * 1024,
        );
        for start in 0..3 {
            append_at(&running.handle, start).await.unwrap();
        }
        assert_eq!(running.hooks.hits(HookPoint::WalSync), 0);
        let hooks = running.hooks.clone();
        wait_for(|| hooks.hits(HookPoint::WalSync) >= 1).await;
        let after_idle = hooks.hits(HookPoint::WalSync);
        append_at(&running.handle, 10).await.unwrap();
        assert_eq!(hooks.hits(HookPoint::WalSync), after_idle);
        running.handle.shutdown().await.unwrap();
        assert!(hooks.hits(HookPoint::WalSync) > after_idle);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_idle_sync_error_keeps_acked_records() {
        let running = open_writer(
            WalSync::Interval(Duration::from_millis(50)),
            u64::MAX,
            Duration::from_millis(5),
            64 * 1024 * 1024,
        );
        running.hooks.arm_fail(HookPoint::WalSync);
        append_at(&running.handle, 1).await.unwrap();
        let hooks = running.hooks.clone();
        wait_for(|| hooks.hits(HookPoint::WalSync) >= 1).await;
        assert!(running.handle.shared().failed());
        assert_eq!(replay_seqs(running.dir.path()), vec![1]);
        assert_eq!(
            append_at(&running.handle, 2).await,
            Err(AppendError::WalFailed)
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn rotation_names_each_segment_with_its_first_seq() {
        let running = open_writer(WalSync::Always, u64::MAX, Duration::from_millis(5), 1);
        for start in 0..3 {
            append_at(&running.handle, start).await.unwrap();
        }
        running.handle.shutdown().await.unwrap();
        let mut nonempty = Vec::new();
        for entry in std::fs::read_dir(running.dir.path()).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if parse_segment_name(&name).is_some() && entry.metadata().unwrap().len() > 0 {
                nonempty.push(name.into_owned());
            }
        }
        nonempty.sort();
        let seqs: Vec<u64> = nonempty
            .iter()
            .map(|name| parse_segment_name(name).unwrap())
            .collect();
        assert_eq!(seqs, vec![1, 2, 3]);
        assert_eq!(replay_seqs(running.dir.path()), vec![1, 2, 3]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_drains_the_queued_group_and_ends_the_task() {
        let running = open_writer(
            WalSync::Always,
            u64::MAX,
            Duration::from_secs(1),
            64 * 1024 * 1024,
        );
        let hooks = running.hooks.clone();
        hooks.hold(HookPoint::AfterWalSync);
        let sender = running.handle.sender().unwrap();
        let state = Arc::clone(running.handle.shared());
        let mut tasks = Vec::new();
        for start in 0..4 {
            let sender = sender.clone();
            let state = Arc::clone(&state);
            tasks.push(tokio::spawn(async move {
                append_sender(&sender, &state, start).await
            }));
        }
        drop(sender);
        wait_for(|| hooks.groups().first().is_some_and(|group| group.0 == 4)).await;
        let shutdown = tokio::spawn(async move { running.handle.shutdown().await });
        hooks.release(HookPoint::AfterWalSync);
        for task in tasks {
            assert!(task.await.unwrap().is_ok());
        }
        shutdown.await.unwrap().unwrap();
    }

    async fn append_sender(
        tx: &mpsc::Sender<AppendRequest>,
        state: &WriterShared,
        start: i64,
    ) -> Result<AppendReceipt, AppendError> {
        if state.failed() {
            return Err(AppendError::WalFailed);
        }
        let batch = spans_at(&[start]);
        let bytes = batch.get_array_memory_size();
        let (ack, rx) = oneshot::channel();
        tx.send(AppendRequest {
            project: project(),
            batch,
            bytes,
            ack,
        })
        .await
        .map_err(|_| {
            if state.failed() {
                AppendError::WalFailed
            } else {
                AppendError::ShuttingDown
            }
        })?;
        rx.await.unwrap_or(Err(AppendError::WalFailed))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_writer_notifies_at_the_size_threshold() {
        let size = batch_bytes();
        let quiet = open_writer_with_flush(
            WalSync::Always,
            u64::MAX,
            Duration::from_millis(5),
            64 * 1024 * 1024,
            size.saturating_add(1),
        );
        append_at(&quiet.handle, 1).await.unwrap();
        let missed = tokio::time::timeout(
            Duration::from_millis(50),
            quiet.handle.shared().flush_notify.notified(),
        )
        .await;
        assert!(missed.is_err(), "notified below the threshold");

        let loud = open_writer_with_flush(
            WalSync::Always,
            u64::MAX,
            Duration::from_millis(5),
            64 * 1024 * 1024,
            size,
        );
        append_at(&loud.handle, 1).await.unwrap();
        tokio::time::timeout(
            Duration::from_secs(2),
            loud.handle.shared().flush_notify.notified(),
        )
        .await
        .expect("notified at the threshold");
    }
}
