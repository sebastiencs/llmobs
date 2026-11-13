//! End-to-end retention tests (M5.5, spec 03 §8).
//!
//! Days D−2, D−1 and D, with D = 2026-03-05 (the compaction tests' day). Each
//! day gets one span (so one `spans` and one `traces` file after a flush) and
//! one score with `created_at` on that day.

use std::path::Path;
use std::time::Duration;

use chrono::{Datelike, NaiveDate};
use llmobs_core::ids::ProjectId;

use crate::QueryOptions;
use crate::Storage;
use crate::catalog::{FileMeta, FileTable};
use crate::compaction::job::tests::{config_in, flush_cycle, project, quiet_compaction, wait_at};
use crate::hooks::HookPoint;
use crate::test_support::{rows, score_row, score_rows, span_row, span_rows};
use crate::views::QueryContext;
use crate::wal::Table;

const DAY_NS: i64 = 86_400_000_000_000;

/// Day 3, 4 or 5 of March 2026: D−2, D−1 and D.
fn day(which: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 3, which).unwrap_or_else(|| panic!("invalid day {which}"))
}

/// `date` at 12:00 UTC, in ns.
fn noon(date: NaiveDate) -> i64 {
    let midnight = (i64::from(date.num_days_from_ce()) - 719_163) * DAY_NS;
    midnight + 12 * 3_600 * 1_000_000_000
}

/// The second fixed test project, for the two-project test.
fn other_project() -> ProjectId {
    "018f6b4e-7c1a-7c1a-8c1a-000000000002".parse().unwrap()
}

/// The first `i64` of the first row of `sql`, over `project`'s providers.
async fn scalar(storage: &Storage, project: ProjectId, sql: &str) -> i64 {
    let query = storage
        .query(project, QueryOptions::default())
        .await
        .unwrap();
    let ctx = QueryContext::new(query, None).unwrap();
    let batches = rows(&ctx, sql).await;
    let column = batches[0]
        .column(0)
        .as_any()
        .downcast_ref::<arrow::array::Int64Array>()
        .unwrap();
    column.value(0)
}

/// The `data_files.state` values of the test project's files of `day`.
async fn day_states(dir: &Path, day: NaiveDate) -> Vec<String> {
    use sqlx::ConnectOptions;
    let mut connection = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(dir.join("catalog.db"))
        .read_only(true)
        .connect()
        .await
        .unwrap();
    sqlx::query_scalar(
        "SELECT state FROM data_files WHERE project_id = ? AND day = ? ORDER BY path",
    )
    .bind(project().to_string())
    .bind(day.format("%Y-%m-%d").to_string())
    .fetch_all(&mut connection)
    .await
    .unwrap()
}

/// Appends one span and one score per day in `3..=5`, then flushes by shutting
/// down and reopening: one `spans`, one `traces` and one `scores` file per day.
async fn three_days(config: &crate::config::StorageConfig) -> Storage {
    let storage = Storage::open(config.clone()).await.unwrap();
    let spans = vec![
        span_row(3, 0, noon(day(3)), noon(day(3))),
        span_row(4, 0, noon(day(4)), noon(day(4))),
        span_row(5, 0, noon(day(5)), noon(day(5))),
    ];
    storage
        .append_spans(project(), span_rows(&spans))
        .await
        .unwrap();
    let scores = vec![
        score_row(
            0x03,
            3,
            noon(day(3)),
            noon(day(3)),
            noon(day(3)),
            false,
            "score",
        ),
        score_row(
            0x04,
            4,
            noon(day(4)),
            noon(day(4)),
            noon(day(4)),
            false,
            "score",
        ),
        score_row(
            0x05,
            5,
            noon(day(5)),
            noon(day(5)),
            noon(day(5)),
            false,
            "score",
        ),
    ];
    storage
        .append_scores(project(), score_rows(&scores))
        .await
        .unwrap();
    storage.shutdown().await.unwrap();
    Storage::open(config.clone()).await.unwrap()
}

/// The test project's active files of the three tables, as owned copies: an
/// `Arc` held by the test would keep an obsolete file alive for
/// `collect_obsolete`'s in-use check.
fn active_files(storage: &Storage) -> Vec<FileMeta> {
    [FileTable::Spans, FileTable::Traces, FileTable::Scores]
        .into_iter()
        .flat_map(|table| storage.catalog_files(table))
        .map(|file| (*file).clone())
        .collect()
}

// 1. apply_retention drops the files of the days before the watermark.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retention_drops_the_files_of_old_days() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
    let storage = three_days(&config).await;
    assert_eq!(storage.catalog_files(Table::Spans).len(), 3);
    assert_eq!(storage.catalog_files(FileTable::Traces).len(), 3);
    assert_eq!(storage.catalog_files(Table::Scores).len(), 3);
    let dropped: Vec<FileMeta> = active_files(&storage)
        .into_iter()
        .filter(|file| file.day == day(3))
        .collect();
    assert_eq!(dropped.len(), 3, "one spans, one traces, one scores file");
    let bytes: u64 = dropped.iter().map(|file| file.size).sum();

    let report = storage.apply_retention(project(), day(4)).await.unwrap();
    assert_eq!(report.files, 3);
    assert_eq!(report.bytes, bytes);
    assert_eq!(report.keep_from, day(4));

    assert_eq!(
        day_states(dir.path(), day(3)).await,
        vec!["obsolete".to_owned(); 3]
    );
    for file in active_files(&storage) {
        assert!(file.day >= day(4), "a file of D-2 is still active");
        assert!(
            !dropped.iter().any(|dropped| dropped.id == file.id),
            "an obsoleted file is still in the catalog"
        );
    }
    for sql in [
        "SELECT count(*) FROM spans",
        "SELECT count(*) FROM traces",
        "SELECT count(*) FROM scores",
    ] {
        assert_eq!(scalar(&storage, project(), sql).await, 2, "{sql}");
    }

    let collected = storage.collect_obsolete(Duration::ZERO).await.unwrap();
    assert_eq!(collected.deleted, 3);
    for file in &dropped {
        assert!(
            !dir.path().join("objects").join(&file.path).exists(),
            "{} survived collection",
            file.path
        );
    }
    storage.shutdown().await.unwrap();
}

// 2. The watermark only rises, and survives a reopen.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_watermark_only_rises_and_survives_a_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
    let storage = Storage::open(config.clone()).await.unwrap();
    storage.apply_retention(project(), day(4)).await.unwrap();
    let report = storage.apply_retention(project(), day(3)).await.unwrap();
    assert_eq!(
        report,
        crate::RetentionReport {
            files: 0,
            bytes: 0,
            keep_from: day(4),
        }
    );
    storage.shutdown().await.unwrap();

    let storage = Storage::open(config).await.unwrap();
    let report = storage.apply_retention(project(), day(3)).await.unwrap();
    assert_eq!(
        report,
        crate::RetentionReport {
            files: 0,
            bytes: 0,
            keep_from: day(4),
        }
    );
    storage.shutdown().await.unwrap();
}

// 3. Late rows of a dropped day are hidden in the memtable and never flushed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn late_rows_of_a_dropped_day_are_hidden_and_never_flushed() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
    let storage = Storage::open(config.clone()).await.unwrap();
    storage.apply_retention(project(), day(4)).await.unwrap();
    let span = span_row(1, 0, noon(day(3)), noon(day(3)));
    storage
        .append_spans(project(), span_rows(&[span]))
        .await
        .unwrap();
    let score = score_row(
        0x33,
        1,
        noon(day(3)),
        noon(day(3)),
        noon(day(3)),
        false,
        "late",
    );
    storage
        .append_scores(project(), score_rows(&[score]))
        .await
        .unwrap();
    for sql in [
        "SELECT count(*) FROM spans",
        "SELECT count(*) FROM traces",
        "SELECT count(*) FROM scores",
    ] {
        assert_eq!(scalar(&storage, project(), sql).await, 0, "{sql}");
    }

    // The flush drops both partitions, commits its sequence and truncates the
    // WAL: nothing replays at the next open.
    storage.shutdown().await.unwrap();
    let storage = Storage::open(config).await.unwrap();
    assert_eq!(storage.replayed(Table::Spans), 0);
    assert_eq!(storage.replayed(Table::Scores), 0);
    assert_eq!(storage.published_rows(Table::Spans), 0);
    assert_eq!(storage.published_rows(Table::Scores), 0);
    assert!(active_files(&storage).is_empty());
    storage.shutdown().await.unwrap();
}

// 4. A flush prepared before the retention conflicts; its retry drops the
//    partition.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flush_prepared_before_the_retention_conflicts_and_drops_the_partition() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path(), quiet_compaction(8), Duration::from_millis(100));
    let storage = std::sync::Arc::new(Storage::open(config.clone()).await.unwrap());
    let hooks = storage.hooks().clone();
    hooks.hold(HookPoint::FlushAfterWrite);
    let span = span_row(1, 0, noon(day(3)), noon(day(3)));
    let receipt = storage
        .append_spans(project(), span_rows(&[span]))
        .await
        .unwrap();
    wait_at(&storage, HookPoint::FlushAfterWrite);
    // The parked flush prepared a spans file and its traces file of D-2.
    let staged = storage.staged_paths().await.unwrap();
    assert_eq!(staged.len(), 2, "{staged:?}");
    assert!(
        staged.iter().all(|path| path.contains("day=2026-03-03")),
        "{staged:?}"
    );

    let retention = {
        let storage = std::sync::Arc::clone(&storage);
        tokio::spawn(async move { storage.apply_retention(project(), day(4)).await })
    };
    retention.await.unwrap().unwrap();
    hooks.release(HookPoint::FlushAfterWrite);

    // The first commit was refused ("file day is before the retention
    // watermark") and its staged rows stayed; the retry drops the partition
    // and commits only its sequence.
    let started = std::time::Instant::now();
    loop {
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "the flush retry never committed"
        );
        if storage.flushed_seq(Table::Spans).await == receipt.seq
            && storage.published_rows(Table::Spans) == 0
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        storage.catalog_files(Table::Spans).is_empty(),
        "no file of D-2 is active"
    );
    assert!(storage.catalog_files(FileTable::Traces).is_empty());
    let staged = storage.staged_paths().await.unwrap();
    assert_eq!(staged.len(), 2, "the refused files stay staged: {staged:?}");
    assert!(
        staged.iter().all(|path| path.contains("day=2026-03-03")),
        "{staged:?}"
    );
    std::sync::Arc::try_unwrap(storage)
        .unwrap_or_else(|_| panic!("storage still shared"))
        .shutdown()
        .await
        .unwrap();
}

// 5. A compaction started before the retention loses its commit; its staged
//    outputs are collected.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_compaction_started_before_the_retention_fails_and_is_collected() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path(), quiet_compaction(2), Duration::from_secs(3_600));
    let mut storage = Storage::open(config.clone()).await.unwrap();
    for index in 0..2_u8 {
        let time = noon(day(3));
        storage = flush_cycle(
            &config,
            storage,
            span_rows(&[span_row(1, 10 + index, time, time + 1)]),
        )
        .await;
    }
    assert_eq!(storage.catalog_files(Table::Spans).len(), 2);

    let storage = std::sync::Arc::new(storage);
    let hooks = storage.hooks().clone();
    hooks.hold(HookPoint::CompactionAfterWrite);
    let racing = {
        let storage = std::sync::Arc::clone(&storage);
        tokio::spawn(async move { storage.compact(noon(day(5))).await })
    };
    wait_at(&storage, HookPoint::CompactionAfterWrite);

    let report = storage.apply_retention(project(), day(4)).await.unwrap();
    assert_eq!(report.files, 4, "two spans files and their traces files");
    assert_eq!(report.keep_from, day(4));

    hooks.release(HookPoint::CompactionAfterWrite);
    let compacted = racing.await.unwrap().unwrap();
    assert_eq!((compacted.jobs, compacted.failed), (0, 1), "{compacted:?}");
    assert!(storage.catalog_files(Table::Spans).is_empty());
    assert!(storage.catalog_files(FileTable::Traces).is_empty());

    storage.collect_staged(Duration::ZERO).await.unwrap();
    assert!(
        storage.staged_paths().await.unwrap().is_empty(),
        "the job's staged outputs are collected"
    );
    std::sync::Arc::try_unwrap(storage)
        .unwrap_or_else(|_| panic!("storage still shared"))
        .shutdown()
        .await
        .unwrap();
}

// 6. Another project is untouched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_project_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
    let storage = Storage::open(config.clone()).await.unwrap();
    let mine = span_row(1, 0, noon(day(3)), noon(day(3)));
    let mut theirs = span_row(2, 0, noon(day(3)), noon(day(3)));
    theirs.project_id = other_project();
    let mut their_score = score_row(
        0x44,
        2,
        noon(day(3)),
        noon(day(3)),
        noon(day(3)),
        false,
        "theirs",
    );
    their_score.project_id = other_project();
    storage
        .append_spans(project(), span_rows(&[mine]))
        .await
        .unwrap();
    storage
        .append_spans(other_project(), span_rows(&[theirs]))
        .await
        .unwrap();
    storage
        .append_scores(other_project(), score_rows(&[their_score]))
        .await
        .unwrap();
    storage.shutdown().await.unwrap();

    let storage = Storage::open(config.clone()).await.unwrap();
    let report = storage.apply_retention(project(), day(4)).await.unwrap();
    assert_eq!(report.files, 2, "the spans file and its traces file");
    assert_eq!(storage.catalog_files(Table::Spans).len(), 1);
    assert_eq!(storage.catalog_files(FileTable::Traces).len(), 1);
    assert_eq!(storage.catalog_files(Table::Scores).len(), 1);
    assert_eq!(
        scalar(&storage, other_project(), "SELECT count(*) FROM spans").await,
        1
    );
    assert_eq!(
        scalar(&storage, project(), "SELECT count(*) FROM spans").await,
        0
    );

    // Q has no watermark: its late rows of the same day still reach a file.
    let mut late = span_row(3, 0, noon(day(3)), noon(day(3)));
    late.project_id = other_project();
    storage
        .append_spans(other_project(), span_rows(&[late]))
        .await
        .unwrap();
    storage.shutdown().await.unwrap();
    let storage = Storage::open(config).await.unwrap();
    assert_eq!(storage.catalog_files(Table::Spans).len(), 2);
    assert_eq!(storage.catalog_files(FileTable::Traces).len(), 2);
    assert_eq!(
        scalar(&storage, other_project(), "SELECT count(*) FROM spans").await,
        2
    );
    storage.shutdown().await.unwrap();
}
