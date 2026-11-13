//! Corrupt-file handling: which read errors make a file a suspect, the queue
//! of suspects, and the whole-file check that decides whether one is corrupt
//! (spec 03 §10, D-A1-10).

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use arrow::error::ArrowError;
use datafusion::error::DataFusionError;
use futures::StreamExt;
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt};
use parquet::arrow::arrow_reader::ArrowReaderOptions;
#[allow(deprecated)]
use parquet::arrow::async_reader::ParquetObjectReader;
use parquet::arrow::async_reader::ParquetRecordBatchStreamBuilder;
use parquet::errors::ParquetError;
use parquet::file::metadata::PageIndexPolicy;

use crate::catalog::{FileId, FileMeta};
use crate::query_error::root;

/// Whether `error` may come from a corrupt Parquet file rather than the object
/// store or the query (spec 03 §10). False positives only cost a verification.
pub(crate) fn may_be_corrupt(error: &DataFusionError) -> bool {
    match root(error) {
        DataFusionError::ParquetError(error) => !is_store_error(error),
        DataFusionError::ArrowError(error, _) => matches!(**error, ArrowError::ParquetError(_)),
        DataFusionError::External(error) => {
            error
                .downcast_ref::<ParquetError>()
                .is_some_and(|error| !is_store_error(error))
                || error
                    .downcast_ref::<ArrowError>()
                    .is_some_and(|error| matches!(error, ArrowError::ParquetError(_)))
        }
        _ => false,
    }
}

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

/// Files waiting for verification, and files verified healthy recently.
#[derive(Debug, Default)]
pub(crate) struct VerifyQueue {
    state: Mutex<QueueState>,
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
    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues every id that is not queued and not verified healthy within
    /// `HEALTHY_FOR` of `now`. Returns how many were queued.
    pub(crate) fn suspect(&self, ids: impl IntoIterator<Item = FileId>, now: i64) -> usize {
        let mut state = self.lock();
        let mut queued = 0_usize;
        let mut dropped = 0_usize;
        for id in ids {
            if state.queued.contains(&id) {
                continue;
            }
            if state
                .healthy
                .get(&id)
                .is_some_and(|at| now.saturating_sub(*at) < HEALTHY_FOR)
            {
                continue;
            }
            if state.pending.len() >= QUEUE_MAX {
                dropped += 1;
                continue;
            }
            state.pending.push_back(id);
            state.queued.insert(id);
            queued += 1;
        }
        drop(state);
        if dropped > 0 {
            tracing::warn!(dropped, "verify queue is full; suspect files dropped");
        }
        queued
    }

    /// Removes and returns up to `max` ids, oldest first.
    pub(crate) fn take(&self, max: usize) -> Vec<FileId> {
        let mut state = self.lock();
        let mut taken = Vec::new();
        while taken.len() < max {
            let Some(id) = state.pending.pop_front() else {
                break;
            };
            state.queued.remove(&id);
            taken.push(id);
        }
        taken
    }

    /// Puts `id` back at the end (unavailable, or claimed by a running job).
    pub(crate) fn requeue(&self, id: FileId) {
        let mut state = self.lock();
        if state.queued.insert(id) {
            state.pending.push_back(id);
        }
    }

    /// Remembers `id` as healthy at `now`, and forgets entries older than `HEALTHY_FOR`.
    pub(crate) fn healthy(&self, id: FileId, now: i64) {
        let mut state = self.lock();
        state.healthy.insert(id, now);
        state
            .healthy
            .retain(|_, at| now.saturating_sub(*at) < HEALTHY_FOR);
    }
}

/// What a whole-file read found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Every row group and column decodes, with the catalog's row count.
    Healthy,
    /// The file is corrupt. `reason` is static.
    Corrupt {
        /// Why, for the log.
        reason: &'static str,
    },
    /// The object store failed. The file may be fine.
    Unavailable,
}

/// Reads `file` whole from `store`: every row group, every column.
pub(crate) async fn verify_file(store: &Arc<dyn ObjectStore>, file: &FileMeta) -> Verdict {
    let path = ObjectPath::from(file.path.as_str());
    match store.head(&path).await {
        Ok(meta) => {
            if meta.size != file.size {
                return Verdict::Corrupt {
                    reason: "size differs from the catalog",
                };
            }
        }
        Err(object_store::Error::NotFound { .. }) => {
            return Verdict::Corrupt {
                reason: "missing object",
            };
        }
        Err(_) => return Verdict::Unavailable,
    }
    let failed = |error: &ParquetError| {
        if is_store_error(error) {
            Verdict::Unavailable
        } else {
            Verdict::Corrupt {
                reason: "decode failed",
            }
        }
    };
    #[allow(deprecated)]
    let reader = ParquetObjectReader::new(Arc::clone(store), path).with_file_size(file.size);
    let options = ArrowReaderOptions::new().with_page_index_policy(PageIndexPolicy::Required);
    let builder = match ParquetRecordBatchStreamBuilder::new_with_options(reader, options).await {
        Ok(builder) => builder,
        Err(error) => return failed(&error),
    };
    let mut stream = match builder.build() {
        Ok(stream) => stream,
        Err(error) => return failed(&error),
    };
    let mut rows = 0_u64;
    while let Some(batch) = stream.next().await {
        match batch {
            Ok(batch) => {
                rows = rows.saturating_add(u64::try_from(batch.num_rows()).unwrap_or(u64::MAX));
            }
            Err(error) => return failed(&error),
        }
    }
    if rows != file.rows {
        return Verdict::Corrupt {
            reason: "row count differs from the catalog",
        };
    }
    Verdict::Healthy
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::error::ArrowError;
    use datafusion::error::DataFusionError;
    use parquet::errors::ParquetError;
    use uuid::Uuid;

    use super::*;

    fn store_error() -> object_store::Error {
        object_store::Error::Generic {
            store: "test",
            source: Box::new(std::io::Error::other("unreachable")),
        }
    }

    fn missing() -> object_store::Error {
        object_store::Error::NotFound {
            path: "tables/spans/x".to_owned(),
            source: Box::new(std::io::Error::other("missing")),
        }
    }

    fn parquet_error() -> ParquetError {
        ParquetError::General("bad page".to_owned())
    }

    fn file_id(last: u128) -> FileId {
        FileId(Uuid::from_u128(last))
    }

    #[test]
    fn a_parquet_error_is_corruption_unless_the_store_failed() {
        assert!(may_be_corrupt(&DataFusionError::ParquetError(Box::new(
            parquet_error()
        ))));
        assert!(!may_be_corrupt(&DataFusionError::ParquetError(Box::new(
            ParquetError::External(Box::new(store_error()))
        ))));
        assert!(may_be_corrupt(&DataFusionError::ParquetError(Box::new(
            ParquetError::External(Box::new(missing()))
        ))));
    }

    #[test]
    fn an_arrow_wrapped_parquet_error_is_corruption() {
        let error = DataFusionError::ArrowError(
            Box::new(ArrowError::ParquetError("bad page".to_owned())),
            None,
        );
        assert!(may_be_corrupt(&error));
        let other = DataFusionError::ArrowError(Box::new(ArrowError::DivideByZero), None);
        assert!(!may_be_corrupt(&other));
    }

    #[test]
    fn an_external_parquet_or_arrow_error_is_corruption() {
        let parquet = DataFusionError::External(Box::new(parquet_error()));
        assert!(may_be_corrupt(&parquet));
        let arrow =
            DataFusionError::External(Box::new(ArrowError::ParquetError("bad page".to_owned())));
        assert!(may_be_corrupt(&arrow));
        let store =
            DataFusionError::External(Box::new(ParquetError::External(Box::new(store_error()))));
        assert!(!may_be_corrupt(&store));
    }

    #[test]
    fn wrappers_are_looked_through() {
        let inner = DataFusionError::ParquetError(Box::new(parquet_error()));
        let context = DataFusionError::Context("reading".to_owned(), Box::new(inner));
        let shared = DataFusionError::Shared(Arc::new(context));
        assert!(may_be_corrupt(&shared));
        let store = DataFusionError::Shared(Arc::new(DataFusionError::Context(
            "reading".to_owned(),
            Box::new(DataFusionError::ParquetError(Box::new(
                ParquetError::External(Box::new(store_error())),
            ))),
        )));
        assert!(!may_be_corrupt(&store));
    }

    #[test]
    fn other_errors_are_not_corruption() {
        assert!(!may_be_corrupt(&DataFusionError::ObjectStore(Box::new(
            store_error()
        ))));
        assert!(!may_be_corrupt(&DataFusionError::ResourcesExhausted(
            "full".to_owned()
        )));
        assert!(!may_be_corrupt(&DataFusionError::Plan("bad".to_owned())));
    }

    #[test]
    fn a_store_error_is_not_corruption_but_a_missing_object_is_a_suspect() {
        assert!(is_store_error(&ParquetError::External(Box::new(
            store_error()
        ))));
        assert!(!is_store_error(&ParquetError::External(
            Box::new(missing())
        )));
        assert!(!is_store_error(&parquet_error()));
    }

    #[test]
    fn suspects_wait_once_and_healthy_files_are_skipped_for_an_hour() {
        let queue = VerifyQueue::default();
        let now = 10 * HEALTHY_FOR;
        assert_eq!(queue.suspect([file_id(1), file_id(2), file_id(1)], now), 2);
        assert_eq!(queue.suspect([file_id(1)], now), 0);
        assert_eq!(queue.take(1), vec![file_id(1)]);
        // Taken, so a new suspect queues it again.
        assert_eq!(queue.suspect([file_id(1)], now), 1);
        assert_eq!(queue.take(10), vec![file_id(2), file_id(1)]);

        queue.healthy(file_id(3), now);
        assert_eq!(queue.suspect([file_id(3)], now + HEALTHY_FOR - 1), 0);
        assert_eq!(queue.suspect([file_id(3)], now + HEALTHY_FOR), 1);
    }

    #[test]
    fn a_requeued_file_goes_to_the_end() {
        let queue = VerifyQueue::default();
        queue.suspect([file_id(1), file_id(2)], 0);
        let first = queue.take(1);
        queue.requeue(first[0]);
        queue.requeue(first[0]);
        assert_eq!(queue.take(10), vec![file_id(2), file_id(1)]);
    }

    #[test]
    fn a_full_queue_drops_new_suspects() {
        let queue = VerifyQueue::default();
        let ids = (0..QUEUE_MAX as u128 + 5).map(|n| file_id(n + 1));
        assert_eq!(queue.suspect(ids, 0), QUEUE_MAX);
        assert_eq!(queue.take(usize::MAX).len(), QUEUE_MAX);
    }
}
