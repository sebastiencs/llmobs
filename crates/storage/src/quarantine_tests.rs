//! Quarantine end to end: verification reads, quarantine commits, the trace
//! rebuild, retention and drops, and deletions that cover a quarantined file
//! (M6.3, spec 03 §10, D-A1-10).

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use arrow::array::{Array, Int64Array, RecordBatch};
use object_store::ObjectStore;
use object_store::local::LocalFileSystem;
use object_store::memory::InMemory;
use parquet::file::metadata::ParquetMetaDataReader;
use sqlx::ConnectOptions;
use uuid::Uuid;

use crate::catalog::{FileId, FileMeta, FileTable, now_ns};
use crate::compaction::job::tests::{at, config_in, flush_cycle, project, quiet_compaction};
use crate::deletion::{Deletion, DeletionScope, DeletionStatus};
use crate::hooks::HookPoint;
use crate::test_support::{FaultStore, rows, span_row, span_rows};
use crate::verify::{Verdict, verify_file};
use crate::views::QueryContext;
use crate::{QueryOptions, Storage};

/// How a test damages a flushed object in place.
#[derive(Debug, Clone, Copy)]
enum Corruption {
    /// The first data page's header is overwritten: a decode error.
    Page,
    /// The trailing `PAR1` magic is overwritten: a footer error.
    Footer,
    /// The last byte is cut: the size differs from the catalog.
    Truncate,
}

fn object_path(data_dir: &Path, file: &FileMeta) -> std::path::PathBuf {
    data_dir.join("objects").join(&file.path)
}

/// Damages the object of `file` under `data_dir` as `how` says.
fn corrupt(data_dir: &Path, file: &FileMeta, how: Corruption) {
    let path = object_path(data_dir, file);
    let mut bytes = std::fs::read(&path).unwrap();
    match how {
        Corruption::Page => {
            let data = bytes::Bytes::from(bytes.clone());
            let mut reader = ParquetMetaDataReader::new();
            reader.try_parse(&data).unwrap();
            let metadata = reader.finish().unwrap();
            let offset =
                usize::try_from(metadata.row_group(0).column(0).data_page_offset()).unwrap();
            for byte in &mut bytes[offset..offset + 32] {
                *byte = 0xFF;
            }
        }
        Corruption::Footer => {
            let end = bytes.len();
            for byte in &mut bytes[end - 4..] {
                *byte = 0xFF;
            }
        }
        Corruption::Truncate => {
            bytes.pop();
        }
    }
    std::fs::write(&path, bytes).unwrap();
}

/// A read-only handle on `catalog.db`.
async fn catalog_db(data_dir: &Path) -> sqlx::SqliteConnection {
    sqlx::sqlite::SqliteConnectOptions::new()
        .filename(data_dir.join("catalog.db"))
        .read_only(true)
        .connect()
        .await
        .unwrap()
}

/// The `state` column of one catalog row.
async fn state_of(data_dir: &Path, id: FileId) -> String {
    let mut connection = catalog_db(data_dir).await;
    sqlx::query_scalar("SELECT state FROM data_files WHERE id = ?")
        .bind(id.to_string())
        .fetch_one(&mut connection)
        .await
        .unwrap()
}

/// The `traces` files derived from the `spans` file `spans`.
async fn traces_of(data_dir: &Path, spans: FileId) -> Vec<FileId> {
    let mut connection = catalog_db(data_dir).await;
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT traces_file_id FROM file_sources WHERE spans_file_id = ?")
            .bind(spans.to_string())
            .fetch_all(&mut connection)
            .await
            .unwrap();
    rows.iter()
        .map(|id| FileId(Uuid::parse_str(id).unwrap()))
        .collect()
}

/// The one-row result of a scalar query over the project's views.
async fn count(storage: &Storage, sql: &str) -> i64 {
    let query = storage
        .query(project(), QueryOptions::default())
        .await
        .unwrap();
    let ctx = QueryContext::new(query, None).unwrap();
    let batches: Vec<RecordBatch> = rows(&ctx, sql).await;
    batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<Int64Array>()
        .unwrap()
        .value(0)
}

/// The active spans file whose earliest start is `start` (one row per file).
fn spans_file_at(storage: &Storage, start: i64) -> Arc<FileMeta> {
    storage
        .catalog_files(FileTable::Spans)
        .into_iter()
        .find(|file| file.min_time == start)
        .unwrap()
}

/// Two spans files, one trace each, in one partition day. Returns the
/// storage and the config it was opened with.
async fn two_files(dir: &Path) -> (Storage, crate::config::StorageConfig) {
    let config = config_in(dir, quiet_compaction(10), Duration::from_secs(3_600));
    let storage = Storage::open(config.clone()).await.unwrap();
    let storage = flush_cycle(
        &config,
        storage,
        span_rows(&[span_row(0, 1, at(10, 0, 0), at(10, 0, 1))]),
    )
    .await;
    let storage = flush_cycle(
        &config,
        storage,
        span_rows(&[span_row(1, 2, at(10, 1, 0), at(10, 1, 1))]),
    )
    .await;
    assert_eq!(storage.catalog_files(FileTable::Spans).len(), 2);
    (storage, config)
}

fn local_store(dir: &Path) -> Arc<dyn ObjectStore> {
    Arc::new(LocalFileSystem::new_with_prefix(dir.join("objects")).unwrap())
}

#[tokio::test]
async fn verify_file_sorts_files() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = two_files(dir.path()).await;
    let file = storage.catalog_files(FileTable::Spans)[0].clone();
    let store = local_store(dir.path());
    assert_eq!(verify_file(&store, &file).await, Verdict::Healthy);

    let original = std::fs::read(object_path(dir.path(), &file)).unwrap();
    for (how, reason) in [
        (Corruption::Page, "decode failed"),
        (Corruption::Footer, "decode failed"),
        (Corruption::Truncate, "size differs from the catalog"),
    ] {
        corrupt(dir.path(), &file, how);
        assert_eq!(
            verify_file(&store, &file).await,
            Verdict::Corrupt { reason },
            "{how:?}"
        );
        std::fs::write(object_path(dir.path(), &file), &original).unwrap();
    }

    std::fs::remove_file(object_path(dir.path(), &file)).unwrap();
    assert_eq!(
        verify_file(&store, &file).await,
        Verdict::Corrupt {
            reason: "missing object"
        }
    );
    std::fs::write(object_path(dir.path(), &file), &original).unwrap();

    // A failing store is not corruption.
    let fault = Arc::new(FaultStore::new(Arc::clone(&store)));
    let failing: Arc<dyn ObjectStore> = fault.clone();
    fault.fail_reads(true);
    assert_eq!(verify_file(&failing, &file).await, Verdict::Unavailable);
    fault.fail_reads(false);
    assert_eq!(verify_file(&failing, &file).await, Verdict::Healthy);
    storage.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_corrupt_spans_file_is_quarantined_with_its_traces() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = two_files(dir.path()).await;
    let victim = spans_file_at(&storage, at(10, 0, 0));
    let victim_traces = traces_of(dir.path(), victim.id).await;
    assert_eq!(victim_traces.len(), 1);

    corrupt(dir.path(), &victim, Corruption::Page);
    assert_eq!(storage.verify_queue().suspect([victim.id], now_ns()), 1);
    let report = storage.verify_pass().await;
    assert_eq!(report.quarantined, 1);
    assert_eq!(report.failed, 0);

    let spans = storage.catalog_files(FileTable::Spans);
    assert_eq!(spans.len(), 1);
    assert_ne!(spans[0].id, victim.id);
    let traces = storage.catalog_files(FileTable::Traces);
    assert!(traces.iter().all(|file| file.id != victim_traces[0]));
    assert_eq!(state_of(dir.path(), victim.id).await, "quarantined");
    assert_eq!(state_of(dir.path(), victim_traces[0]).await, "quarantined");

    assert_eq!(count(&storage, "SELECT count(*) FROM spans").await, 1);
    assert_eq!(count(&storage, "SELECT count(*) FROM traces").await, 1);

    // Quarantined objects stay for inspection, GC included.
    storage.collect_obsolete(Duration::ZERO).await.unwrap();
    assert!(object_path(dir.path(), &victim).exists());

    // And the state survives a reopen.
    storage.shutdown().await.unwrap();
    let config = config_in(dir.path(), quiet_compaction(10), Duration::from_secs(3_600));
    let reopened = Storage::open(config).await.unwrap();
    assert_eq!(reopened.catalog_files(FileTable::Spans).len(), 1);
    assert_eq!(count(&reopened, "SELECT count(*) FROM spans").await, 1);
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_corrupt_traces_file_is_rebuilt() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = two_files(dir.path()).await;
    let spans = spans_file_at(&storage, at(10, 0, 0));
    let traces_id = traces_of(dir.path(), spans.id).await[0];
    let traces = storage
        .catalog_files(FileTable::Traces)
        .into_iter()
        .find(|file| file.id == traces_id)
        .unwrap();

    corrupt(dir.path(), &traces, Corruption::Page);
    assert_eq!(storage.verify_queue().suspect([traces_id], now_ns()), 1);
    let report = storage.verify_pass().await;
    assert_eq!(report.repaired, 1);
    assert_eq!(report.quarantined, 0);
    assert_eq!(report.failed, 0);

    assert_eq!(state_of(dir.path(), traces_id).await, "obsolete");
    assert_eq!(count(&storage, "SELECT count(*) FROM spans").await, 2);
    assert_eq!(count(&storage, "SELECT count(*) FROM traces").await, 2);
    let rebuilt = storage.catalog_files(FileTable::Traces);
    assert_eq!(rebuilt.len(), 2);
    assert!(rebuilt.iter().all(|file| file.id != traces_id));
    storage.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_failing_merge_suspects_its_inputs() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path(), quiet_compaction(2), Duration::from_secs(3_600));
    let mut storage = Storage::open(config.clone()).await.unwrap();
    for index in 0..3_u8 {
        let base = at(10, 0, 0) + i64::from(index) * 60 * 1_000_000_000;
        storage = flush_cycle(
            &config,
            storage,
            span_rows(&[span_row(
                index.into(),
                10 + index,
                base,
                base + 1_000_000_000,
            )]),
        )
        .await;
    }
    let files = storage.catalog_files(FileTable::Spans);
    assert_eq!(files.len(), 3);
    let victim = files[1].clone();
    corrupt(dir.path(), &victim, Corruption::Page);

    let report = storage.compact(at(11, 0, 0)).await.unwrap();
    assert_eq!(report.jobs, 0);
    assert_eq!(report.failed, 1);
    let mut suspects = storage.verify_queue().take(10);
    suspects.sort();
    let mut expected: Vec<FileId> = files.iter().map(|file| file.id).collect();
    expected.sort();
    assert_eq!(suspects, expected);
    storage.verify_queue().suspect(expected.clone(), now_ns());

    let verify = storage.verify_pass().await;
    assert_eq!(verify.quarantined, 1);
    assert_eq!(verify.healthy, 2);
    assert_eq!(storage.catalog_files(FileTable::Spans).len(), 2);

    // The two healthy files merge now.
    let report = storage.compact(at(11, 0, 0)).await.unwrap();
    assert_eq!(report.jobs, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(count(&storage, "SELECT count(*) FROM spans").await, 2);
    storage.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_healthy_file_is_not_verified_twice_within_an_hour() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = two_files(dir.path()).await;
    let file = storage.catalog_files(FileTable::Spans)[0].id;
    assert_eq!(storage.verify_queue().suspect([file], now_ns()), 1);
    let report = storage.verify_pass().await;
    assert_eq!(report.healthy, 1);
    assert_eq!(storage.verify_queue().suspect([file], now_ns()), 0);
    storage.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_store_outage_requeues() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path(), quiet_compaction(10), Duration::from_secs(3_600));
    let fault = Arc::new(FaultStore::new(Arc::new(InMemory::new())));
    let storage = Storage::open_with_store(config.clone(), fault.clone())
        .await
        .unwrap();
    storage
        .append_spans(
            project(),
            span_rows(&[span_row(0, 1, at(10, 0, 0), at(10, 0, 1))]),
        )
        .await
        .unwrap();
    storage.shutdown().await.unwrap();
    let storage = Storage::open_with_store(config, fault.clone())
        .await
        .unwrap();
    let file = storage.catalog_files(FileTable::Spans)[0].id;

    storage.verify_queue().suspect([file], now_ns());
    fault.fail_reads(true);
    let report = storage.verify_pass().await;
    assert_eq!(report.unavailable, 1);
    assert_eq!(report.quarantined, 0);
    assert_eq!(storage.verify_queue().take(10), vec![file]);

    fault.fail_reads(false);
    storage.verify_queue().requeue(file);
    let report = storage.verify_pass().await;
    assert_eq!(report.healthy, 1);
    storage.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_anchor_rebuild_skips_a_corrupt_traces_file() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, config) = two_files(dir.path()).await;
    let spans = spans_file_at(&storage, at(10, 0, 0));
    let traces_id = traces_of(dir.path(), spans.id).await[0];
    let traces = storage
        .catalog_files(FileTable::Traces)
        .into_iter()
        .find(|file| file.id == traces_id)
        .unwrap();
    storage.shutdown().await.unwrap();
    corrupt(dir.path(), &traces, Corruption::Footer);

    // The open succeeds; the unreadable traces file is a suspect.
    let storage = Storage::open(config).await.unwrap();
    let suspects = storage.verify_queue().take(10);
    assert!(suspects.contains(&traces_id), "{suspects:?}");
    storage.shutdown().await.unwrap();
}

#[tokio::test]
async fn retention_retires_quarantined_files_of_dropped_days() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = two_files(dir.path()).await;
    let victim = spans_file_at(&storage, at(10, 0, 0));
    let traces_id = traces_of(dir.path(), victim.id).await[0];
    corrupt(dir.path(), &victim, Corruption::Page);
    storage.verify_queue().suspect([victim.id], now_ns());
    assert_eq!(storage.verify_pass().await.quarantined, 1);

    // The file's day is kept: nothing retires.
    let next = victim.day.succ_opt().unwrap();
    storage
        .apply_retention(project(), victim.day)
        .await
        .unwrap();
    assert_eq!(state_of(dir.path(), victim.id).await, "quarantined");

    // Dropping its day retires it and its traces file.
    storage.apply_retention(project(), next).await.unwrap();
    assert_eq!(state_of(dir.path(), victim.id).await, "obsolete");
    assert_eq!(state_of(dir.path(), traces_id).await, "obsolete");
    storage.shutdown().await.unwrap();
}

#[tokio::test]
async fn dropping_a_project_retires_its_quarantined_files() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = two_files(dir.path()).await;
    let victim = spans_file_at(&storage, at(10, 0, 0));
    corrupt(dir.path(), &victim, Corruption::Page);
    storage.verify_queue().suspect([victim.id], now_ns());
    assert_eq!(storage.verify_pass().await.quarantined, 1);

    storage.drop_project(project()).await.unwrap();
    assert_eq!(state_of(dir.path(), victim.id).await, "obsolete");
    storage.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_pending_deletion_retires_quarantined_files_it_may_cover() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = two_files(dir.path()).await;
    let victim = spans_file_at(&storage, at(10, 0, 0));
    let traces_id = traces_of(dir.path(), victim.id).await[0];
    corrupt(dir.path(), &victim, Corruption::Page);
    storage.verify_queue().suspect([victim.id], now_ns());
    assert_eq!(storage.verify_pass().await.quarantined, 1);

    let deletion = Deletion {
        id: Uuid::now_v7(),
        project: project(),
        cutoff: at(23, 0, 0),
        scope: DeletionScope::Traces {
            trace_ids: vec![span_row(0, 0, 0, 0).trace_id],
        },
    };
    let id = deletion.id;
    storage.register_deletion(deletion).await.unwrap();
    let report = storage.apply_deletions(now_ns()).await.unwrap();
    assert_eq!(report.failed, 0);
    assert_eq!(state_of(dir.path(), victim.id).await, "obsolete");
    assert_eq!(state_of(dir.path(), traces_id).await, "obsolete");
    assert!(matches!(
        storage.deletion_status(id).await.unwrap(),
        DeletionStatus::Applied { .. }
    ));
    storage.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_failed_quarantine_commit_requeues() {
    let dir = tempfile::tempdir().unwrap();
    let (storage, _) = two_files(dir.path()).await;
    let victim = spans_file_at(&storage, at(10, 0, 0));
    corrupt(dir.path(), &victim, Corruption::Page);
    storage.verify_queue().suspect([victim.id], now_ns());

    storage.hooks().arm_fail(HookPoint::QuarantineBeforeCommit);
    let report = storage.verify_pass().await;
    assert_eq!(report.failed, 1);
    assert_eq!(report.quarantined, 0);
    assert_eq!(storage.verify_queue().take(10), vec![victim.id]);
    assert_eq!(state_of(dir.path(), victim.id).await, "active");
    storage.shutdown().await.unwrap();
}
