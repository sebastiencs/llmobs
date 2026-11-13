//! End-to-end deletion tests: tombstones in reads, `register_deletion`,
//! `deletion_status` (M5.2, spec 03 §7).

use std::path::Path;
use std::sync::Arc;

use arrow::array::{Array, RecordBatch};
use bytesize::ByteSize;
use llmobs_core::ids::ProjectId;
use llmobs_core::score::ScoreTarget;
use uuid::Uuid;

use crate::QueryOptions;
use crate::config::{FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig, WalConfig};
use crate::deletion::{Deletion, DeletionScope, DeletionStatus};
use crate::error::DeletionError;
use crate::test_support::{rows, score_row, score_rows, span_row, span_rows};
use crate::views::QueryContext;

fn project() -> ProjectId {
    "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
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

fn traces_deletion(traces: &[u16], cutoff: i64) -> Deletion {
    Deletion {
        id: Uuid::now_v7(),
        project: project(),
        cutoff,
        scope: DeletionScope::Traces {
            trace_ids: traces
                .iter()
                .map(|trace| span_row(*trace, 0, 0, 0).trace_id)
                .collect(),
        },
    }
}

/// One fresh session over the latest published state.
async fn context(storage: &crate::Storage) -> QueryContext {
    QueryContext::new(
        storage
            .query(project(), QueryOptions::default())
            .await
            .unwrap(),
        None,
    )
    .unwrap()
}

/// The first `i64` of the first row of `sql`.
async fn scalar(storage: &crate::Storage, sql: &str) -> i64 {
    let batches = rows(&context(storage).await, sql).await;
    let column = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<arrow::array::Int64Array>()
        .unwrap();
    column.value(0)
}

/// Every string cell of `sql`'s result, one per line (EXPLAIN plans).
async fn text(storage: &crate::Storage, sql: &str) -> String {
    let batches = rows(&context(storage).await, sql).await;
    let mut text = String::new();
    for batch in &batches {
        for column in batch.columns() {
            let Some(values) = column.as_any().downcast_ref::<arrow::array::StringArray>() else {
                continue;
            };
            for index in 0..values.len() {
                text.push_str(values.value(index));
                text.push('\n');
            }
        }
    }
    text
}

/// `EXPLAIN` lines whose predicate names `ingest_time`: the tombstone filter.
///
/// DataFusion 55 pushes the gated file's `FilterExec` into the scan, so the
/// predicate appears as `predicate=…` on the `DataSourceExec` line; the plain
/// projection list alone does not count.
fn ingest_time_filters(plan: &str) -> usize {
    plan.lines()
        .filter(|line| {
            (line.contains("FilterExec") || line.contains("predicate="))
                && line.contains("ingest_time")
        })
        .count()
}

async fn span_count(storage: &crate::Storage, trace: u16) -> i64 {
    let hex = span_row(trace, 0, 0, 0).trace_id.to_hex();
    scalar(
        storage,
        &format!("SELECT count(*) FROM spans WHERE trace_id = '{hex}'"),
    )
    .await
}

async fn trace_count(storage: &crate::Storage, trace: u16) -> i64 {
    let hex = span_row(trace, 0, 0, 0).trace_id.to_hex();
    scalar(
        storage,
        &format!("SELECT count(*) FROM traces WHERE trace_id = '{hex}'"),
    )
    .await
}

/// The single flushed spans file of the project.
async fn spans_file(storage: &crate::Storage) -> crate::catalog::FileMeta {
    let query = storage
        .query(project(), QueryOptions::default())
        .await
        .unwrap();
    let files = query
        .spans
        .downcast_ref::<crate::query::SpansProvider>()
        .unwrap()
        .candidate_files(&[]);
    assert_eq!(files.len(), 1, "one flushed spans file");
    let file = &*files[0];
    file.clone()
}

/// The raw 16-byte trace ids stored in one spans Parquet object.
fn spans_trace_ids(dir: &Path, file: &crate::catalog::FileMeta) -> Vec<[u8; 16]> {
    use arrow::array::Array;

    let bytes = std::fs::read(dir.join("objects").join(&file.path)).unwrap();
    let builder = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
        bytes::Bytes::from(bytes),
    )
    .unwrap();
    let schema = Arc::clone(builder.schema());
    let batches: Vec<RecordBatch> = builder
        .build()
        .unwrap()
        .map(|batch| batch.unwrap())
        .collect();
    let all = arrow::compute::concat_batches(&schema, &batches).unwrap();
    let column = all
        .column_by_name(llmobs_core::schema::spans::TRACE_ID)
        .unwrap();
    let array = column
        .as_any()
        .downcast_ref::<arrow::array::FixedSizeBinaryArray>()
        .unwrap();
    (0..array.len())
        .map(|row| <[u8; 16]>::try_from(array.value(row)).unwrap())
        .collect()
}

/// Appends one span per `(trace, span)` plus a session-target score, then
/// flushes by shutting down and reopening. Everything is ingested at `INGEST`.
const INGEST: i64 = 1_000_000_000_000;

async fn flushed_with_score(dir: &tempfile::TempDir, spans: &[(u16, u8)]) -> crate::Storage {
    let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
    let rows: Vec<_> = spans
        .iter()
        .map(|(trace, span)| span_row(*trace, *span, INGEST, INGEST))
        .collect();
    storage
        .append_spans(project(), span_rows(&rows))
        .await
        .unwrap();
    let mut score = score_row(0x99, 0, INGEST, INGEST, INGEST, false, "session-score");
    score.target = ScoreTarget::Session {
        session_id: "session-of-another-user".to_owned(),
    };
    storage
        .append_scores(project(), score_rows(&[score]))
        .await
        .unwrap();
    storage.shutdown().await.unwrap();
    crate::Storage::open(config_in(dir.path())).await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_traces_deletion_hides_memtable_rows_up_to_the_cutoff() {
    let dir = tempfile::tempdir().unwrap();
    let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
    storage
        .append_spans(
            project(),
            span_rows(&[
                span_row(1, 0, INGEST, INGEST),
                span_row(2, 0, INGEST, INGEST),
            ]),
        )
        .await
        .unwrap();
    storage
        .register_deletion(traces_deletion(&[1], INGEST))
        .await
        .unwrap();
    assert_eq!(span_count(&storage, 1).await, 0);
    assert_eq!(span_count(&storage, 2).await, 1);

    // A row appended after the registration with a later ingest time is new
    // data, not covered by the deletion (spec 03 §7).
    storage
        .append_spans(project(), span_rows(&[span_row(1, 1, INGEST, INGEST + 1)]))
        .await
        .unwrap();
    assert_eq!(span_count(&storage, 1).await, 1);
    storage.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_traces_deletion_hides_flushed_rows_and_keeps_a_session_score() {
    let dir = tempfile::tempdir().unwrap();
    let storage = flushed_with_score(&dir, &[(1, 0), (2, 0)]).await;
    // Registered after the flush: the file still holds trace 1's row, only the
    // query path hides it (the rewriter is M5.4).
    storage
        .register_deletion(traces_deletion(&[1], INGEST))
        .await
        .unwrap();

    let stored = spans_trace_ids(dir.path(), &spans_file(&storage).await);
    let trace_one = *span_row(1, 0, 0, 0).trace_id.as_bytes();
    assert!(stored.contains(&trace_one), "the file still holds trace 1");

    assert_eq!(span_count(&storage, 1).await, 0);
    assert_eq!(span_count(&storage, 2).await, 1);
    assert_eq!(
        scalar(&storage, "SELECT count(*) FROM spans").await,
        1,
        "only trace 2's span remains visible"
    );
    assert_eq!(
        scalar(
            &storage,
            "SELECT count(*) FROM (SELECT span_id FROM spans LIMIT 1)"
        )
        .await,
        1,
        "a LIMIT 1 query is right"
    );
    assert_eq!(trace_count(&storage, 1).await, 0);
    assert_eq!(trace_count(&storage, 2).await, 1);
    assert_eq!(
        scalar(&storage, "SELECT count(*) FROM scores").await,
        1,
        "a session-target score stays visible"
    );
    storage.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_user_deletion_hides_user_rows_and_session_scores_only() {
    let dir = tempfile::tempdir().unwrap();
    let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
    let mut mine = span_row(1, 0, INGEST, INGEST);
    mine.user_id = Some("u".to_owned());
    let no_one = span_row(2, 0, INGEST, INGEST);
    let mut shared = span_row(3, 0, INGEST, INGEST);
    shared.user_id = Some("v".to_owned());
    shared.session_id = Some("s".to_owned());
    storage
        .append_spans(project(), span_rows(&[mine, no_one, shared]))
        .await
        .unwrap();
    let mut session_score = score_row(0x11, 0, INGEST, INGEST, INGEST, false, "score");
    session_score.target = ScoreTarget::Session {
        session_id: "s".to_owned(),
    };
    storage
        .append_scores(project(), score_rows(&[session_score]))
        .await
        .unwrap();

    storage
        .register_deletion(Deletion {
            id: Uuid::now_v7(),
            project: project(),
            cutoff: INGEST,
            scope: DeletionScope::User {
                user_id: "u".to_owned(),
                trace_ids: Vec::new(),
                session_ids: vec!["s".to_owned()],
            },
        })
        .await
        .unwrap();

    assert_eq!(
        scalar(&storage, "SELECT count(*) FROM spans WHERE user_id = 'u'").await,
        0,
        "the user's spans are hidden"
    );
    assert_eq!(
        scalar(&storage, "SELECT count(*) FROM spans WHERE user_id IS NULL").await,
        1,
        "spans with no user stay"
    );
    assert_eq!(
        scalar(
            &storage,
            "SELECT count(*) FROM spans WHERE session_id = 's'"
        )
        .await,
        1,
        "another user's span in the session stays"
    );
    assert_eq!(
        scalar(&storage, "SELECT count(*) FROM scores").await,
        0,
        "the session's scores are hidden"
    );
    storage.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_raised_file_escapes_the_tombstone_filter() {
    let dir = tempfile::tempdir().unwrap();
    let storage = flushed_with_score(&dir, &[(1, 0)]).await;
    storage
        .register_deletion(traces_deletion(&[1], INGEST))
        .await
        .unwrap();

    let gated = text(&storage, "EXPLAIN SELECT * FROM spans").await;
    assert!(
        ingest_time_filters(&gated) > 0,
        "before the raise the gated file is filtered:\n{gated}"
    );

    // The rewriter's bookkeeping (M5.4) raised to the known sequence; the test
    // forces it to observe the plan change.
    let file = spans_file(&storage).await.id;
    storage
        .commit_test_edit(&crate::catalog::CatalogEdit {
            add: Vec::new(),
            remove: Vec::new(),
            flushed_seq: None,
            sources: Vec::new(),
            raise: vec![(file, 1)],
        })
        .await
        .unwrap();

    let clean = text(&storage, "EXPLAIN SELECT * FROM spans").await;
    assert_eq!(
        ingest_time_filters(&clean),
        0,
        "after the raise the clean file is not filtered:\n{clean}"
    );
    storage.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_limit_over_clean_and_gated_sides_sits_above_the_union() {
    let dir = tempfile::tempdir().unwrap();
    // Two flushes: one file holds the covered trace 2, the other the visible
    // traces 3 and 4. Nothing stays in the memtable.
    let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
    storage
        .append_spans(project(), span_rows(&[span_row(2, 0, INGEST, INGEST)]))
        .await
        .unwrap();
    storage.shutdown().await.unwrap();
    let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
    storage
        .append_spans(
            project(),
            span_rows(&[
                span_row(3, 0, INGEST, INGEST),
                span_row(4, 0, INGEST, INGEST),
            ]),
        )
        .await
        .unwrap();
    storage.shutdown().await.unwrap();
    let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
    storage
        .register_deletion(traces_deletion(&[2], INGEST))
        .await
        .unwrap();

    // Raise the file with no covered row: it becomes the clean side.
    let query = storage
        .query(project(), QueryOptions::default())
        .await
        .unwrap();
    let files = query
        .spans
        .downcast_ref::<crate::query::SpansProvider>()
        .unwrap()
        .candidate_files(&[]);
    assert_eq!(files.len(), 2);
    let covered = *span_row(2, 0, 0, 0).trace_id.as_bytes();
    let clean = files
        .iter()
        .find(|file| !spans_trace_ids(dir.path(), file).contains(&covered))
        .expect("one file holds no covered row");
    storage
        .commit_test_edit(&crate::catalog::CatalogEdit {
            add: Vec::new(),
            remove: Vec::new(),
            flushed_seq: None,
            sources: Vec::new(),
            raise: vec![(clean.id, 1)],
        })
        .await
        .unwrap();

    assert_eq!(
        scalar(
            &storage,
            "SELECT count(*) FROM (SELECT span_id FROM spans LIMIT 1)"
        )
        .await,
        1,
        "the limit returns exactly one row"
    );
    assert_eq!(scalar(&storage, "SELECT count(*) FROM spans").await, 2);
    // DataFusion 55 fuses the `GlobalLimitExec` scan_union builds over the
    // union into a `CoalescePartitionsExec: fetch=1` node; either name above
    // the `UnionExec` is the limit applied after both sides.
    let plan = text(&storage, "EXPLAIN SELECT span_id FROM spans LIMIT 1").await;
    let lines: Vec<&str> = plan.lines().collect();
    let union = lines
        .iter()
        .position(|line| line.contains("UnionExec"))
        .expect("a union of the two file sides");
    let limit_above_union = lines[..union].iter().any(|line| {
        line.contains("GlobalLimitExec")
            || (line.contains("CoalescePartitionsExec") && line.contains("fetch=1"))
    });
    assert!(
        limit_above_union,
        "the limit sits above the union, not on the clean side:\n{plan}"
    );
    storage.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hidden_rows_stay_hidden_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
    storage
        .append_spans(
            project(),
            span_rows(&[
                span_row(1, 0, INGEST, INGEST),
                span_row(2, 0, INGEST, INGEST),
            ]),
        )
        .await
        .unwrap();
    storage
        .register_deletion(traces_deletion(&[1], INGEST))
        .await
        .unwrap();
    storage.shutdown().await.unwrap();

    let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
    assert_eq!(span_count(&storage, 1).await, 0);
    assert_eq!(span_count(&storage, 2).await, 1);
    storage.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn register_deletion_is_idempotent_and_status_reports_the_state() {
    let dir = tempfile::tempdir().unwrap();
    let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
    let request = traces_deletion(&[1], 1_000);
    storage.register_deletion(request.clone()).await.unwrap();
    storage.register_deletion(request.clone()).await.unwrap();
    assert_eq!(
        storage.deletion_status(request.id).await.unwrap(),
        DeletionStatus::Pending
    );
    let error = storage.deletion_status(Uuid::now_v7()).await.unwrap_err();
    assert!(matches!(error, DeletionError::NotFound), "{error}");
    let mut invalid = traces_deletion(&[1], 0);
    invalid.cutoff = 0;
    let error = storage.register_deletion(invalid).await.unwrap_err();
    assert!(matches!(error, DeletionError::Invalid { .. }), "{error}");
    storage.shutdown().await.unwrap();
}
