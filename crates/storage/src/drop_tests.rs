//! End-to-end project drop tests (M5.7, spec 03 §7).
//!
//! Two projects: P (`project()`, dropped) and Q (`kept_project()`, kept),
//! each with one flushed span, one flushed score and one stored blob.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use chrono::NaiveDate;
use llmobs_core::ids::ProjectId;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::QueryOptions;
use crate::Storage;
use crate::catalog::{FileId, FileMeta, FileTable};
use crate::compaction::job::tests::{at, config_in, project, quiet_compaction, wait_at};
use crate::deletion::{Deletion, DeletionScope, DeletionStatus};
use crate::error::{AppendError, MediaError, StorageError};
use crate::hooks::HookPoint;
use crate::media::MediaBlob;
use crate::objects::Objects;
use crate::test_support::{rows, score_row, score_rows, span_row, span_rows};
use crate::views::QueryContext;
use crate::wal::Table;

/// The second fixed test project, kept while P is dropped.
fn kept_project() -> ProjectId {
    "018f6b4e-7c1a-7c1a-8c1a-000000000002".parse().unwrap()
}

fn sha256_of(bytes: &Bytes) -> [u8; 32] {
    Sha256::digest(bytes.as_ref()).into()
}

fn blob_of(bytes: &[u8], mime_type: &str) -> MediaBlob {
    let bytes = Bytes::copy_from_slice(bytes);
    MediaBlob {
        sha256: sha256_of(&bytes),
        mime_type: mime_type.to_owned(),
        bytes,
    }
}

/// The blobs [`two_projects`] stores: P's and Q's.
fn p_blob() -> MediaBlob {
    blob_of(b"media-of-p", "image/png")
}

fn q_blob() -> MediaBlob {
    blob_of(b"media-of-q", "image/png")
}

/// The blob's object path under the local object root.
fn object_file(dir: &Path, which: ProjectId, sha: &[u8; 32]) -> PathBuf {
    dir.join("objects")
        .join(Objects::media_path(which, &hex::encode(sha)))
}

async fn connect(dir: &Path) -> sqlx::SqliteConnection {
    use sqlx::ConnectOptions;
    sqlx::sqlite::SqliteConnectOptions::new()
        .filename(dir.join("catalog.db"))
        .read_only(true)
        .connect()
        .await
        .unwrap()
}

/// The first `i64` of the first row of `sql`, over `which`'s providers.
async fn scalar(storage: &Storage, which: ProjectId, sql: &str) -> i64 {
    let query = storage.query(which, QueryOptions::default()).await.unwrap();
    let ctx = QueryContext::new(query, None).unwrap();
    let batches = rows(&ctx, sql).await;
    let column = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<arrow::array::Int64Array>()
        .unwrap();
    column.value(0)
}

/// The `projects.state` of `which`.
async fn project_state(dir: &Path, which: ProjectId) -> String {
    let mut connection = connect(dir).await;
    sqlx::query_scalar("SELECT state FROM projects WHERE project_id = ?")
        .bind(which.to_string())
        .fetch_one(&mut connection)
        .await
        .unwrap()
}

/// `media` rows of `which`.
async fn media_rows(dir: &Path, which: ProjectId) -> i64 {
    let mut connection = connect(dir).await;
    sqlx::query_scalar("SELECT COUNT(*) FROM media WHERE project_id = ?")
        .bind(which.to_string())
        .fetch_one(&mut connection)
        .await
        .unwrap()
}

/// `data_files` rows of `which`, any state.
async fn data_files(dir: &Path, which: ProjectId) -> i64 {
    let mut connection = connect(dir).await;
    sqlx::query_scalar("SELECT COUNT(*) FROM data_files WHERE project_id = ?")
        .bind(which.to_string())
        .fetch_one(&mut connection)
        .await
        .unwrap()
}

/// `(state, obsoleted_at)` of one file's row.
async fn file_state(dir: &Path, id: FileId) -> (String, Option<i64>) {
    let mut connection = connect(dir).await;
    sqlx::query_as("SELECT state, obsoleted_at FROM data_files WHERE id = ?")
        .bind(id.to_string())
        .fetch_one(&mut connection)
        .await
        .unwrap()
}

/// `(deletion_traces, deletion_users, deletion_sessions)` rows of `id`.
async fn scope_counts(dir: &Path, id: Uuid) -> (i64, i64, i64) {
    let mut connection = connect(dir).await;
    let traces: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM deletion_traces WHERE deletion_id = ?")
            .bind(id.to_string())
            .fetch_one(&mut connection)
            .await
            .unwrap();
    let users: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM deletion_users WHERE deletion_id = ?")
            .bind(id.to_string())
            .fetch_one(&mut connection)
            .await
            .unwrap();
    let sessions: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM deletion_sessions WHERE deletion_id = ?")
            .bind(id.to_string())
            .fetch_one(&mut connection)
            .await
            .unwrap();
    (traces, users, sessions)
}

/// P and Q, each with one flushed span, one flushed score and one stored
/// blob: one `spans`, one `traces` and one `scores` file per project after the
/// shutdown flush and the reopen.
/// One span of `which`, as an appendable batch.
fn span_batch(which: ProjectId, trace: u16, time: i64) -> arrow::array::RecordBatch {
    let mut span = span_row(trace, 0, time, time);
    span.project_id = which;
    span_rows(&[span])
}

/// One score of `which`, as an appendable batch.
fn score_batch(
    which: ProjectId,
    score_id: u128,
    trace: u16,
    time: i64,
    name: &str,
) -> arrow::array::RecordBatch {
    let mut score = score_row(score_id, trace, time, time, time, false, name);
    score.project_id = which;
    score_rows(&[score])
}

async fn two_projects(config: &crate::config::StorageConfig) -> Storage {
    let storage = Storage::open(config.clone()).await.unwrap();
    let time = at(0, 0, 0);
    for (which, score_id, blob) in [
        (project(), 0x01_u128, p_blob()),
        (kept_project(), 0x02_u128, q_blob()),
    ] {
        storage
            .append_spans(which, span_batch(which, 1, time))
            .await
            .unwrap();
        storage
            .append_scores(which, score_batch(which, score_id, 1, time, "score"))
            .await
            .unwrap();
        storage.put_media(which, blob).await.unwrap();
    }
    storage.shutdown().await.unwrap();
    Storage::open(config.clone()).await.unwrap()
}

/// The project's active files of the three tables, as owned copies: an `Arc`
/// held by the test would keep an obsolete file alive for
/// `collect_obsolete`'s in-use check.
fn active_files(storage: &Storage, which: ProjectId) -> Vec<FileMeta> {
    [FileTable::Spans, FileTable::Traces, FileTable::Scores]
        .into_iter()
        .flat_map(|table| storage.catalog_files(table))
        .map(|file| (*file).clone())
        .filter(|file| file.project == which)
        .collect()
}

/// `SELECT count(*)` over `spans`, `traces` and `scores` of both projects.
async fn assert_counts(storage: &Storage, p: i64, q: i64) {
    for sql in [
        "SELECT count(*) FROM spans",
        "SELECT count(*) FROM traces",
        "SELECT count(*) FROM scores",
    ] {
        assert_eq!(scalar(storage, project(), sql).await, p, "{sql} of P");
        assert_eq!(scalar(storage, kept_project(), sql).await, q, "{sql} of Q");
    }
}

// 1. A drop refuses the project's appends, empties its queries, obsoletes its
//    files and deletes its media.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_drop_hides_the_project_and_deletes_its_data() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
    let storage = two_projects(&config).await;
    assert_eq!(active_files(&storage, project()).len(), 3);
    assert_eq!(active_files(&storage, kept_project()).len(), 3);
    let dropped_files = active_files(&storage, project());
    let p_sha = p_blob().sha256;
    let q_sha = q_blob().sha256;

    storage.drop_project(project()).await.unwrap();

    // Appends of P are refused; Q's still work.
    let time = at(1, 0, 0);
    for error in [
        storage
            .append_spans(project(), span_batch(project(), 2, time))
            .await
            .unwrap_err(),
        storage
            .append_scores(project(), score_batch(project(), 0x11, 2, time, "late"))
            .await
            .unwrap_err(),
    ] {
        assert_eq!(error, AppendError::ProjectDropped);
    }
    storage
        .append_spans(kept_project(), span_batch(kept_project(), 2, time))
        .await
        .unwrap();
    storage
        .append_scores(
            kept_project(),
            score_batch(kept_project(), 0x12, 2, time, "kept"),
        )
        .await
        .unwrap();

    // Every query of P returns no row; Q's still see their rows.
    assert_counts(&storage, 0, 2).await;

    // P's files are obsolete with `obsoleted_at` set, and GC deletes them.
    for file in &dropped_files {
        let (state, obsoleted_at) = file_state(dir.path(), file.id).await;
        assert_eq!(state, "obsolete", "{}", file.path);
        assert!(obsoleted_at.is_some(), "{} has no obsoleted_at", file.path);
    }
    let collected = storage.collect_obsolete(Duration::ZERO).await.unwrap();
    assert_eq!(collected.deleted, 3);
    for file in &dropped_files {
        assert!(
            !dir.path().join("objects").join(&file.path).exists(),
            "{} survived collection",
            file.path
        );
    }

    // P's blob object and row are gone; `projects` has P as `dropped`.
    assert!(!object_file(dir.path(), project(), &p_sha).exists());
    assert!(object_file(dir.path(), kept_project(), &q_sha).is_file());
    assert_eq!(media_rows(dir.path(), project()).await, 0);
    assert_eq!(media_rows(dir.path(), kept_project()).await, 1);
    assert_eq!(project_state(dir.path(), project()).await, "dropped");

    // Media of a dropped project is refused.
    assert!(
        matches!(
            storage
                .put_media(project(), blob_of(b"late", "image/png"))
                .await,
            Err(MediaError::ProjectDropped)
        ),
        "a put passed"
    );
    assert!(
        matches!(
            storage.get_media(project(), &p_sha, None).await,
            Err(MediaError::NotFound)
        ),
        "a dropped project's blob was found"
    );
    let read = storage
        .get_media(kept_project(), &q_sha, None)
        .await
        .unwrap();
    assert_eq!(read.bytes, Bytes::from_static(b"media-of-q"));
    storage.shutdown().await.unwrap();
}

// 2. A reopen keeps the project dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dropped_project_stays_dropped_after_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
    let storage = two_projects(&config).await;
    storage.drop_project(project()).await.unwrap();
    storage.shutdown().await.unwrap();

    let storage = Storage::open(config).await.unwrap();
    assert_eq!(
        storage
            .append_spans(
                project(),
                span_rows(&[span_row(1, 0, at(0, 0, 0), at(0, 0, 0))])
            )
            .await
            .unwrap_err(),
        AppendError::ProjectDropped
    );
    assert_counts(&storage, 0, 1).await;
    assert_eq!(project_state(dir.path(), project()).await, "dropped");
    storage.shutdown().await.unwrap();
}

// 3a. Rows still in the memtable when the project is dropped never reach a
//     file: the shutdown flush drops them and commits only its sequence.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_shutdown_flush_drops_the_rows_of_a_dropped_project() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
    let storage = Storage::open(config.clone()).await.unwrap();
    let time = at(0, 0, 0);
    let receipt = storage
        .append_spans(project(), span_rows(&[span_row(1, 0, time, time)]))
        .await
        .unwrap();
    storage.drop_project(project()).await.unwrap();
    storage.shutdown().await.unwrap();

    let storage = Storage::open(config).await.unwrap();
    assert_eq!(storage.replayed(Table::Spans), 0, "the WAL was truncated");
    assert_eq!(storage.flushed_seq(Table::Spans).await, receipt.seq);
    assert_eq!(data_files(dir.path(), project()).await, 0);
    assert_eq!(
        scalar(&storage, project(), "SELECT count(*) FROM spans").await,
        0
    );
    storage.shutdown().await.unwrap();
}

// 3b. Rows that only live in the WAL when the project is dropped are replayed
//     and dropped by the next flush: no file of the project is ever committed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wal_rows_of_a_dropped_project_never_reach_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
    let storage = Storage::open(config.clone()).await.unwrap();
    let time = at(0, 0, 0);
    storage
        .append_spans(project(), span_rows(&[span_row(1, 0, time, time)]))
        .await
        .unwrap();
    // No shutdown: the WAL keeps the row.
    drop(storage);
    tokio::task::yield_now().await;

    let storage = Storage::open(config.clone()).await.unwrap();
    assert_eq!(storage.published_rows(Table::Spans), 1, "the WAL replayed");
    storage.drop_project(project()).await.unwrap();
    storage.shutdown().await.unwrap();

    let storage = Storage::open(config).await.unwrap();
    assert_eq!(storage.replayed(Table::Spans), 0);
    assert_eq!(data_files(dir.path(), project()).await, 0);
    assert_eq!(
        scalar(&storage, project(), "SELECT count(*) FROM spans").await,
        0
    );
    storage.shutdown().await.unwrap();
}

// 4. The append gate: an append in flight holds the drop until it returns, and
//    the append itself is acknowledged. Run 10 times.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_drop_waits_for_an_append_in_flight() {
    for _ in 0..10 {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
        let storage = Arc::new(Storage::open(config).await.unwrap());
        let hooks = storage.hooks().clone();
        hooks.hold(HookPoint::AfterWalSync);
        let time = at(0, 0, 0);
        let append = {
            let storage = Arc::clone(&storage);
            tokio::spawn(async move {
                storage
                    .append_spans(project(), span_rows(&[span_row(1, 0, time, time)]))
                    .await
            })
        };
        wait_at(&storage, HookPoint::AfterWalSync);
        let dropping = {
            let storage = Arc::clone(&storage);
            tokio::spawn(async move { storage.drop_project(project()).await })
        };
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            !dropping.is_finished(),
            "the drop returned while an append held the gate"
        );
        hooks.release(HookPoint::AfterWalSync);
        append.await.unwrap().unwrap();
        dropping.await.unwrap().unwrap();
        assert_eq!(
            scalar(&storage, project(), "SELECT count(*) FROM spans").await,
            0,
            "P's row is visible"
        );
        Arc::try_unwrap(storage)
            .unwrap_or_else(|_| panic!("storage still shared"))
            .shutdown()
            .await
            .unwrap();
    }
}

// 5. A flush prepared before the drop conflicts ("project is dropped"); its
//    retry writes the kept project's file only. Run 10 times.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flush_prepared_before_the_drop_conflicts() {
    for _ in 0..10 {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(8), Duration::from_millis(100));
        let storage = Arc::new(Storage::open(config.clone()).await.unwrap());
        let hooks = storage.hooks().clone();
        hooks.hold(HookPoint::FlushAfterWrite);
        let time = at(0, 0, 0);
        storage
            .append_spans(project(), span_batch(project(), 1, time))
            .await
            .unwrap();
        let q_receipt = storage
            .append_spans(kept_project(), span_batch(kept_project(), 1, time))
            .await
            .unwrap();
        wait_at(&storage, HookPoint::FlushAfterWrite);
        // The parked flush prepared a spans file and a traces file per project.
        let staged = storage.staged_paths().await.unwrap();
        assert_eq!(staged.len(), 4, "{staged:?}");

        // The drop waits for the committer only, not for the parked flush.
        let dropping = {
            let storage = Arc::clone(&storage);
            tokio::spawn(async move { storage.drop_project(project()).await })
        };
        dropping.await.unwrap().unwrap();
        hooks.release(HookPoint::FlushAfterWrite);

        // The first commit was refused and its staged rows stayed; the retry
        // drops P's partition and commits Q's file with the whole sequence.
        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "the flush retry never committed"
            );
            if storage.flushed_seq(Table::Spans).await == q_receipt.seq
                && storage.published_rows(Table::Spans) == 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            active_files(&storage, project()).is_empty(),
            "P has an active file"
        );
        let kept = active_files(&storage, kept_project());
        assert_eq!(
            kept.len(),
            2,
            "one spans file and its traces file: {kept:?}"
        );
        let leftover: Vec<_> = storage
            .staged_paths()
            .await
            .unwrap()
            .into_iter()
            .filter(|path| path.contains(&format!("project={}", project())))
            .collect();
        assert_eq!(
            leftover.len(),
            2,
            "the refused files stay staged: {leftover:?}"
        );
        assert_eq!(
            scalar(&storage, kept_project(), "SELECT count(*) FROM spans").await,
            1
        );
        Arc::try_unwrap(storage)
            .unwrap_or_else(|_| panic!("storage still shared"))
            .shutdown()
            .await
            .unwrap();
    }
}

// 6. A pending deletion of P is applied by the drop, its scope rows are
//    erased and its tombstones are gone; a registration for P afterwards is
//    stored applied and publishes nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pending_deletion_of_a_dropped_project_becomes_applied() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
    let storage = two_projects(&config).await;
    let time = at(0, 0, 0);
    let span = span_row(1, 0, time, time);
    let pending = Deletion {
        id: Uuid::now_v7(),
        project: project(),
        cutoff: time + 1,
        scope: DeletionScope::Traces {
            trace_ids: vec![span.trace_id],
        },
    };
    storage.register_deletion(pending.clone()).await.unwrap();
    let user = Deletion {
        id: Uuid::now_v7(),
        project: project(),
        cutoff: time + 1,
        scope: DeletionScope::User {
            user_id: "user-of-p".to_owned(),
            trace_ids: vec![span.trace_id],
            session_ids: vec!["session-of-p".to_owned()],
        },
    };
    storage.register_deletion(user.clone()).await.unwrap();
    assert_eq!(
        storage.deletion_status(pending.id).await.unwrap(),
        DeletionStatus::Pending
    );
    assert_eq!(
        storage.deletion_status(user.id).await.unwrap(),
        DeletionStatus::Pending
    );
    assert_eq!(
        scope_counts(dir.path(), user.id).await,
        (1, 1, 1),
        "the user deletion stored no scope rows"
    );

    storage.drop_project(project()).await.unwrap();
    assert!(
        matches!(
            storage.deletion_status(pending.id).await.unwrap(),
            DeletionStatus::Applied { .. }
        ),
        "the pending deletion was not applied by the drop"
    );
    assert!(
        matches!(
            storage.deletion_status(user.id).await.unwrap(),
            DeletionStatus::Applied { .. }
        ),
        "the user deletion was not applied by the drop"
    );
    // The drop erases the ids of the person with the data (spec 03 §7).
    for id in [pending.id, user.id] {
        assert_eq!(
            scope_counts(dir.path(), id).await,
            (0, 0, 0),
            "the drop kept {id}'s scope rows"
        );
    }
    assert!(
        storage
            .published
            .load()
            .tombstones
            .for_project(project())
            .is_empty(),
        "the project's tombstones are gone"
    );

    let after = Deletion {
        id: Uuid::now_v7(),
        project: project(),
        cutoff: time + 2,
        scope: DeletionScope::Traces {
            trace_ids: vec![span.trace_id],
        },
    };
    storage.register_deletion(after.clone()).await.unwrap();
    assert!(
        matches!(
            storage.deletion_status(after.id).await.unwrap(),
            DeletionStatus::Applied { .. }
        ),
        "a deletion of a dropped project was not stored applied"
    );
    assert!(
        storage
            .published
            .load()
            .tombstones
            .for_project(project())
            .is_empty(),
        "the dropped project's registration published tombstones"
    );
    storage.shutdown().await.unwrap();
}

// 7. A failed media delete leaves the project dropped and `dropping`; a second
//    `drop_project` finishes the drop.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_media_delete_leaves_the_drop_unfinished() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
    let storage = two_projects(&config).await;
    let p_sha = p_blob().sha256;
    storage.hooks().arm_fail(HookPoint::MediaBeforeDelete);

    let error = storage.drop_project(project()).await.unwrap_err();
    assert!(
        matches!(error, StorageError::Media(MediaError::Fault)),
        "{error}"
    );
    // P is still dropped: appends refused, queries empty — and `dropping`.
    let time = at(1, 0, 0);
    assert_eq!(
        storage
            .append_spans(project(), span_rows(&[span_row(2, 0, time, time)]))
            .await
            .unwrap_err(),
        AppendError::ProjectDropped
    );
    assert_eq!(
        scalar(&storage, project(), "SELECT count(*) FROM spans").await,
        0
    );
    assert_eq!(project_state(dir.path(), project()).await, "dropping");
    // The fault fired before the delete: the blob is still there.
    assert!(object_file(dir.path(), project(), &p_sha).is_file());
    assert_eq!(media_rows(dir.path(), project()).await, 1);

    storage.drop_project(project()).await.unwrap();
    assert_eq!(project_state(dir.path(), project()).await, "dropped");
    assert!(!object_file(dir.path(), project(), &p_sha).exists());
    assert_eq!(media_rows(dir.path(), project()).await, 0);
    storage.shutdown().await.unwrap();
}

// 8. Dropping twice is `Ok`, and retention of a dropped project writes
//    nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_second_drop_is_ok_and_retention_reports_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
    let storage = two_projects(&config).await;
    storage.drop_project(project()).await.unwrap();
    storage.drop_project(project()).await.unwrap();

    let keep_from = NaiveDate::from_ymd_opt(2026, 3, 4).unwrap();
    let report = storage.apply_retention(project(), keep_from).await.unwrap();
    assert_eq!(
        report,
        crate::RetentionReport {
            files: 0,
            bytes: 0,
            keep_from,
        }
    );
    let mut connection = connect(dir.path()).await;
    let watermarks: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM retention_watermarks WHERE project_id = ?")
            .bind(project().to_string())
            .fetch_one(&mut connection)
            .await
            .unwrap();
    assert_eq!(watermarks, 0, "retention of a dropped project wrote a row");
    storage.shutdown().await.unwrap();
}

// 9. A media put that raced the drop never lands its row: the insert refuses
//    the dropped project and staged-object GC deletes the object. Run 10
//    times.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_media_put_racing_the_drop_never_lands_its_row() {
    for _ in 0..10 {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
        let storage = Arc::new(Storage::open(config.clone()).await.unwrap());
        let hooks = storage.hooks().clone();
        hooks.hold(HookPoint::MediaAfterPut);
        let blob = blob_of(b"racing-media", "image/png");
        let sha = blob.sha256;
        let put = {
            let storage = Arc::clone(&storage);
            tokio::spawn(async move { storage.put_media(project(), blob).await })
        };
        // The put has passed its `is_dropped` check and written the object.
        wait_at(&storage, HookPoint::MediaAfterPut);

        // The drop completes: its `finish_drop` sees no media row yet.
        let dropping = {
            let storage = Arc::clone(&storage);
            tokio::spawn(async move { storage.drop_project(project()).await })
        };
        dropping.await.unwrap().unwrap();
        hooks.release(HookPoint::MediaAfterPut);
        assert!(
            matches!(put.await.unwrap().unwrap_err(), MediaError::ProjectDropped),
            "the racing put was not refused"
        );

        assert_eq!(media_rows(dir.path(), project()).await, 0);
        assert!(
            object_file(dir.path(), project(), &sha).is_file(),
            "the object was deleted before GC"
        );
        storage.collect_staged(Duration::ZERO).await.unwrap();
        assert!(!object_file(dir.path(), project(), &sha).exists());
        assert!(storage.staged_paths().await.unwrap().is_empty());
        Arc::try_unwrap(storage)
            .unwrap_or_else(|_| panic!("storage still shared"))
            .shutdown()
            .await
            .unwrap();
    }
}
