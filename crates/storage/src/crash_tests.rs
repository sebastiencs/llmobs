//! Deterministic crash children and the durability order.
//!
//! Every `*_child` test is ignored: it re-executes this test binary with one
//! hook armed to abort the process, and its `every_*_hook_aborts_safely`
//! parent reopens the directory and finishes the interrupted work. The
//! parents are ignored too, and `just test` runs each in a test process of
//! its own.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use arrow::array::{Array, FixedSizeBinaryArray};
use bytes::Bytes;
use bytesize::ByteSize;
use datafusion::execution::context::{SessionConfig, SessionContext};
use llmobs_core::batch::spans_to_record_batch;
use llmobs_core::enums::{SpanKind, SpanStatus};
use llmobs_core::ids::{ProjectId, SpanId, TraceId};
use llmobs_core::span::Span;
use sha2::{Digest, Sha256};

use crate::catalog::FileTable;
use crate::config::{FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig, WalConfig};
use crate::deletion::{Deletion, DeletionScope, DeletionStatus};
use crate::error::AppendError;
use crate::hooks::HookPoint;
use crate::media::MediaBlob;
use crate::wal::{Table, spans_at};

const POINTS: &[(&str, HookPoint)] = &[
    ("FlushAfterFreeze", HookPoint::FlushAfterFreeze),
    ("FlushAfterStage", HookPoint::FlushAfterStage),
    ("ObjectWrite", HookPoint::ObjectWrite),
    ("ObjectSync", HookPoint::ObjectSync),
    ("ObjectRename", HookPoint::ObjectRename),
    ("ObjectDirSync", HookPoint::ObjectDirSync),
    ("FlushAfterWrite", HookPoint::FlushAfterWrite),
    ("CatalogCommit", HookPoint::CatalogCommit),
    ("FlushAfterCommit", HookPoint::FlushAfterCommit),
    ("FlushAfterTruncate", HookPoint::FlushAfterTruncate),
];

const COMPACTION_POINTS: &[(&str, HookPoint)] = &[
    ("CompactionAfterStage", HookPoint::CompactionAfterStage),
    ("ObjectSync", HookPoint::ObjectSync),
    ("ObjectRename", HookPoint::ObjectRename),
    ("ObjectDirSync", HookPoint::ObjectDirSync),
    ("CompactionAfterWrite", HookPoint::CompactionAfterWrite),
    ("CatalogCommit", HookPoint::CatalogCommit),
    ("CompactionAfterCommit", HookPoint::CompactionAfterCommit),
    ("GcObsoleteBeforeDelete", HookPoint::GcObsoleteBeforeDelete),
    ("GcObsoleteAfterDelete", HookPoint::GcObsoleteAfterDelete),
];

const DELETION_POINTS: &[(&str, HookPoint)] = &[
    ("CatalogCommit", HookPoint::CatalogCommit),
    ("CompactionAfterStage", HookPoint::CompactionAfterStage),
    ("ObjectSync", HookPoint::ObjectSync),
    ("ObjectRename", HookPoint::ObjectRename),
    ("CompactionAfterWrite", HookPoint::CompactionAfterWrite),
    ("CompactionAfterCommit", HookPoint::CompactionAfterCommit),
    ("GcObsoleteBeforeDelete", HookPoint::GcObsoleteBeforeDelete),
    ("GcObsoleteAfterDelete", HookPoint::GcObsoleteAfterDelete),
];

const MEDIA_POINTS: &[(&str, HookPoint)] = &[
    ("ObjectSync", HookPoint::ObjectSync),
    ("ObjectRename", HookPoint::ObjectRename),
    ("MediaAfterPut", HookPoint::MediaAfterPut),
];

const DROP_POINTS: &[(&str, HookPoint)] = &[
    ("CatalogCommit", HookPoint::CatalogCommit),
    ("MediaBeforeDelete", HookPoint::MediaBeforeDelete),
];

fn project() -> ProjectId {
    "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
}

fn quiet_config(dir: &Path) -> StorageConfig {
    StorageConfig {
        data_dir: dir.to_path_buf(),
        min_free_bytes: ByteSize::b(0),
        object_store: None,
        s3: S3Config::default(),
        wal: WalConfig::default(),
        flush: FlushConfig {
            // Stay under the age and size triggers until `shutdown` flushes.
            max_bytes: ByteSize::mib(256),
            max_age: Duration::from_secs(60 * 60),
            upload_concurrency: 2,
        },
        memtable: MemtableConfig::default(),
        query: QueryConfig::default(),
        anchor: Default::default(),
        gc: Default::default(),
        compaction: Default::default(),
        deletion: Default::default(),
    }
}

fn parse_point(name: &str) -> Option<HookPoint> {
    const LISTS: &[&[(&str, HookPoint)]] = &[
        POINTS,
        COMPACTION_POINTS,
        DELETION_POINTS,
        MEDIA_POINTS,
        DROP_POINTS,
    ];
    LISTS
        .iter()
        .flat_map(|list| list.iter())
        .find(|(label, _)| *label == name)
        .map(|(_, point)| *point)
}

/// Two batches, four rows, distinct span ids.
fn batches() -> Vec<arrow::array::RecordBatch> {
    (0..2).map(batch_rows).collect()
}

fn batch_rows(batch: u8) -> arrow::array::RecordBatch {
    let project_id = project();
    let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736"
        .parse::<TraceId>()
        .unwrap();
    let spans: Vec<Span> = (0..2_u8)
        .map(|row| {
            let mut bytes = [0_u8; 8];
            bytes[0] = batch.saturating_add(1);
            bytes[7] = row.saturating_add(1);
            let start = 1_000 + i64::from(batch) * 10 + i64::from(row);
            Span {
                project_id,
                trace_id,
                span_id: SpanId::from_bytes(bytes).unwrap(),
                parent_span_id: None,
                name: format!("b{batch}/{row}"),
                span_kind: SpanKind::Span,
                start_time: start,
                end_time: start + 100,
                status: SpanStatus::Ok,
                status_message: None,
                service_name: None,
                session_id: None,
                user_id: None,
                environment: None,
                release: None,
                tags: Vec::new(),
                metadata: BTreeMap::new(),
                provider: None,
                request_model: None,
                response_model: None,
                operation: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
                total_tokens: None,
                cost_input: None,
                cost_output: None,
                cost_total: None,
                cost_source: None,
                ttft_ns: None,
                request_params: None,
                input: None,
                input_format: None,
                output: None,
                output_format: None,
                input_preview: None,
                output_preview: None,
                system_instructions: None,
                tool_definitions: None,
                attributes: BTreeMap::new(),
                resource_attributes: BTreeMap::new(),
                scope_name: None,
                scope_version: None,
                events: Vec::new(),
                links: Vec::new(),
                media_refs: Vec::new(),
                truncated: Vec::new(),
                conventions: Vec::new(),
                ingest_time: start + 200,
                schema_version: 1,
            }
        })
        .collect();
    spans_to_record_batch(&spans).unwrap()
}

fn expected_ids() -> Vec<Vec<u8>> {
    let mut ids = Vec::new();
    for batch in 0..2 {
        for row in 0..2 {
            let mut bytes = [0_u8; 8];
            bytes[0] = batch + 1;
            bytes[7] = row + 1;
            ids.push(bytes.to_vec());
        }
    }
    ids.sort();
    ids
}

/// The sorted span ids `compaction_child` appends in its step 1: the same
/// `span_row` calls, so the expectation cannot drift from what it writes.
fn compaction_span_ids() -> Vec<Vec<u8>> {
    let mut ids = Vec::new();
    for index in 0..3_u8 {
        for span in [index * 2, index * 2 + 1] {
            ids.push(
                crate::test_support::span_row(0, span, 0, 0)
                    .span_id
                    .as_bytes()
                    .to_vec(),
            );
        }
    }
    ids.sort();
    ids
}

async fn stored_ids(storage: &crate::Storage) -> Vec<Vec<u8>> {
    let query = storage.query(project(), Default::default()).await.unwrap();
    let ctx = SessionContext::new_with_config_rt(
        SessionConfig::new(),
        std::sync::Arc::clone(&query.runtime),
    );
    ctx.register_table("spans_t", std::sync::Arc::clone(&query.spans))
        .unwrap();
    let batches = ctx
        .sql("SELECT span_id FROM spans_t")
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();
    let mut ids = Vec::new();
    for batch in batches {
        let column = batch
            .column(0)
            .as_any()
            .downcast_ref::<FixedSizeBinaryArray>()
            .unwrap();
        for index in 0..batch.num_rows() {
            ids.push(column.value(index).to_vec());
        }
    }
    ids.sort();
    ids
}

fn objects_match_catalog(storage: &crate::Storage, dir: &Path) {
    for table in [Table::Spans, Table::Scores] {
        for file in storage.catalog_files(table) {
            let path = dir.join("objects").join(&file.path);
            let meta = std::fs::metadata(&path)
                .unwrap_or_else(|error| panic!("catalog file {} is missing: {error}", file.path));
            assert_eq!(meta.len(), file.size, "size of {}", file.path);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run by every_flush_hook_aborts_safely"]
async fn child() {
    let Ok(dir) = std::env::var("LLMOBS_FLUSH_CRASH_DIR") else {
        return;
    };
    let name = std::env::var("LLMOBS_FLUSH_CRASH_POINT").unwrap();
    let point = parse_point(&name).unwrap_or_else(|| panic!("unknown crash point {name}"));
    let storage = crate::Storage::open(quiet_config(Path::new(&dir)))
        .await
        .unwrap();
    for batch in batches() {
        storage.append_spans(project(), batch).await.unwrap();
    }
    storage.hooks().abort_at(point);
    storage.shutdown().await.unwrap();
    panic!("shutdown finished without reaching {name}");
}

/// Spawns the `test` crash child with `dir` and `name`, and returns only once
/// it has aborted by signal. A point the child's operations never reach exits
/// with a code (its `panic!`) and fails the assertion.
fn run_child(test: &str, dir_var: &str, point_var: &str, name: &str, dir: &Path) {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(test)
        .arg("--ignored")
        .arg("--nocapture")
        .arg("--test-threads=1")
        .env(dir_var, dir)
        .env(point_var, name)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let started = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > Duration::from_secs(60) => {
                let _ = child.kill();
                panic!("{name} did not abort within 60s");
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => panic!("waiting for {name}: {error}"),
        }
    };
    assert!(
        status.code().is_none(),
        "{name} exited {status} instead of aborting (point not reached?)"
    );
}

/// Run alone by `just test`. A spawned child holds a copy of every open file
/// descriptor until it execs, including other tests' `LOCK` files, so a
/// concurrent test that reopens its directory in that window gets `Locked`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns processes; `just test` runs it in its own test process"]
async fn every_flush_hook_aborts_safely() {
    for (name, _) in POINTS {
        let dir = tempfile::tempdir().unwrap();
        run_child(
            "crash_tests::child",
            "LLMOBS_FLUSH_CRASH_DIR",
            "LLMOBS_FLUSH_CRASH_POINT",
            name,
            dir.path(),
        );

        let storage = crate::Storage::open(quiet_config(dir.path()))
            .await
            .unwrap_or_else(|error| panic!("{name}: reopen failed: {error}"));
        let ids = stored_ids(&storage).await;
        assert_eq!(ids, expected_ids(), "{name}: acknowledged rows");
        objects_match_catalog(&storage, dir.path());
        drop(storage);
    }
}

/// `quiet_config` with hand-run compaction passes: interval 1 h, two L0 files.
fn compaction_config(dir: &Path) -> StorageConfig {
    let mut config = quiet_config(dir);
    config.compaction.interval = Duration::from_secs(3_600);
    config.compaction.l0_files = 2;
    config
}

/// Day D at `hour:minute`, in ns.
fn crash_at(hour: i64, minute: i64) -> i64 {
    use chrono::Datelike;
    let day = chrono::NaiveDate::from_ymd_opt(2026, 3, 5).unwrap();
    (i64::from(day.num_days_from_ce()) - 719_163) * 86_400_000_000_000
        + (hour * 3_600 + minute * 60) * 1_000_000_000
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run by every_compaction_hook_aborts_safely"]
async fn compaction_child() {
    let Ok(dir) = std::env::var("LLMOBS_COMPACTION_CRASH_DIR") else {
        return;
    };
    let name = std::env::var("LLMOBS_COMPACTION_CRASH_POINT").unwrap();
    let point = parse_point(&name).unwrap_or_else(|| panic!("unknown crash point {name}"));

    // Three L0 spans files and three traces files of one trace.
    for index in 0..3_u8 {
        let storage = crate::Storage::open(compaction_config(Path::new(&dir)))
            .await
            .unwrap();
        let start = crash_at(10, 0) + i64::from(index) * 60 * 1_000_000_000;
        let spans = vec![
            crate::test_support::span_row(0, index * 2, start, start + 1_000_000_000),
            crate::test_support::span_row(
                0,
                index * 2 + 1,
                start + 1_000_000_000,
                start + 2_000_000_000,
            ),
        ];
        storage
            .append_spans(project(), crate::test_support::span_rows(&spans))
            .await
            .unwrap();
        storage.shutdown().await.unwrap();
    }

    let storage = crate::Storage::open(compaction_config(Path::new(&dir)))
        .await
        .unwrap();
    storage.hooks().abort_at(point);
    storage.compact(crash_at(12, 0)).await.unwrap();
    storage.collect_obsolete(Duration::ZERO).await.unwrap();
    panic!("compaction finished without reaching {name}");
}

/// Run alone by `just test`, like [`every_flush_hook_aborts_safely`]: a spawned
/// child holds a copy of every open file descriptor until it execs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns processes; `just test` runs it in its own test process"]
async fn every_compaction_hook_aborts_safely() {
    for (name, _) in COMPACTION_POINTS {
        let dir = tempfile::tempdir().unwrap();
        run_child(
            "crash_tests::compaction_child",
            "LLMOBS_COMPACTION_CRASH_DIR",
            "LLMOBS_COMPACTION_CRASH_POINT",
            name,
            dir.path(),
        );

        // The reopen runs the open-time `file_sources` check and the startup
        // temp-file cleanup.
        let storage = crate::Storage::open(compaction_config(dir.path()))
            .await
            .unwrap_or_else(|error| panic!("{name}: reopen failed: {error}"));
        let ids = stored_ids(&storage).await;
        assert_eq!(ids, compaction_span_ids(), "{name}: acknowledged rows");
        objects_match_catalog(&storage, dir.path());

        // Redo the interrupted work, then collect everything it left.
        for pass in 0..5 {
            let report = storage.compact(crash_at(12, 0)).await.unwrap();
            assert_eq!(report.failed, 0, "{name}: pass {pass} failed a job");
            if report.jobs == 0 {
                break;
            }
        }
        storage.collect_obsolete(Duration::ZERO).await.unwrap();
        storage.collect_staged(Duration::ZERO).await.unwrap();

        let ids = stored_ids(&storage).await;
        assert_eq!(ids, compaction_span_ids(), "{name}: rows after the redo");
        let spans_files = storage.catalog_files(Table::Spans);
        assert_eq!(spans_files.len(), 1, "{name}: one spans file");
        assert_eq!(spans_files[0].level, 1, "{name}: the spans file is level 1");
        let traces_files = storage.catalog_files(FileTable::Traces);
        assert_eq!(traces_files.len(), 1, "{name}: one traces file");
        assert_eq!(
            traces_files[0].level, 1,
            "{name}: the traces file is level 1"
        );
        storage.shutdown().await.unwrap();
        drop(storage);

        let (obsolete, staged) = catalog_counts(dir.path()).await;
        assert_eq!(obsolete, 0, "{name}: obsolete rows remain");
        assert_eq!(staged, 0, "{name}: staged rows remain");
        let temps = temp_files(&dir.path().join("objects"));
        assert!(
            temps.is_empty(),
            "{name}: {} temp files remain",
            temps.len()
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run by every_deletion_hook_aborts_safely"]
async fn deletion_child() {
    let Ok(dir) = std::env::var("LLMOBS_DELETION_CRASH_DIR") else {
        return;
    };
    let name = std::env::var("LLMOBS_DELETION_CRASH_POINT").unwrap();
    let point = parse_point(&name).unwrap_or_else(|| panic!("unknown crash point {name}"));

    // Three L0 spans files and three traces files, one span of trace 0 and
    // one of trace 1 each.
    for index in 0..3_u8 {
        let storage = crate::Storage::open(compaction_config(Path::new(&dir)))
            .await
            .unwrap();
        let start = crash_at(10, 0) + i64::from(index) * 60 * 1_000_000_000;
        let spans = vec![
            crate::test_support::span_row(0, index, start, start + 1_000_000_000),
            crate::test_support::span_row(1, index, start + 1_000_000_000, start + 2_000_000_000),
        ];
        storage
            .append_spans(project(), crate::test_support::span_rows(&spans))
            .await
            .unwrap();
        storage.shutdown().await.unwrap();
    }

    let storage = crate::Storage::open(compaction_config(Path::new(&dir)))
        .await
        .unwrap();
    storage.hooks().abort_at(point);
    storage.register_deletion(trace_deletion()).await.unwrap();
    storage.apply_deletions(crash_at(12, 0)).await.unwrap();
    storage.collect_obsolete(Duration::ZERO).await.unwrap();
    panic!("deletion pass finished without reaching {name}");
}

/// Run alone by `just test`, like [`every_flush_hook_aborts_safely`]: a spawned
/// child holds a copy of every open file descriptor until it execs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns processes; `just test` runs it in its own test process"]
async fn every_deletion_hook_aborts_safely() {
    for (name, _) in DELETION_POINTS {
        let dir = tempfile::tempdir().unwrap();
        run_child(
            "crash_tests::deletion_child",
            "LLMOBS_DELETION_CRASH_DIR",
            "LLMOBS_DELETION_CRASH_POINT",
            name,
            dir.path(),
        );

        let storage = crate::Storage::open(compaction_config(dir.path()))
            .await
            .unwrap_or_else(|error| panic!("{name}: reopen failed: {error}"));
        // New when the crash came before the registration commit, a no-op
        // after it. Either way trace 1 is hidden from both tables at once.
        storage.register_deletion(trace_deletion()).await.unwrap();
        let trace0 = trace_bytes(0);
        let trace1 = trace_bytes(1);
        let spans = stored_trace_ids(&storage, false).await;
        assert!(
            !spans.contains(&trace1),
            "{name}: a trace 1 span is visible"
        );
        assert_eq!(
            spans.iter().filter(|trace| **trace == trace0).count(),
            3,
            "{name}: trace 0 spans"
        );
        let traces = stored_trace_ids(&storage, true).await;
        assert!(
            !traces.contains(&trace1),
            "{name}: a trace 1 partial is visible"
        );
        objects_match_catalog(&storage, dir.path());

        // Finish the interrupted work, then collect everything it left.
        for pass in 0..5 {
            let report = storage.apply_deletions(crash_at(12, 0)).await.unwrap();
            assert_eq!(report.failed, 0, "{name}: pass {pass} failed a job");
            if report.applied == 1 || deletion_applied(&storage, name).await {
                break;
            }
        }
        storage.collect_obsolete(Duration::ZERO).await.unwrap();
        storage.collect_staged(Duration::ZERO).await.unwrap();

        // Trace 0 is intact, in the queries and in the files on disk.
        let ids = stored_ids(&storage).await;
        assert_eq!(ids, deletion_span_ids(), "{name}: acknowledged rows");
        for file in storage.catalog_files(Table::Spans) {
            let rows = crate::test_support::object_rows(dir.path(), &file);
            let covered = crate::test_support::trace_ids_of(&rows)
                .into_iter()
                .any(|trace| trace.as_slice() == trace1.as_slice());
            assert!(!covered, "{name}: {} holds a trace 1 row", file.path);
        }
        assert!(
            deletion_applied(&storage, name).await,
            "{name}: the deletion is not applied"
        );

        let (obsolete, staged) = catalog_counts(dir.path()).await;
        assert_eq!(obsolete, 0, "{name}: obsolete rows remain");
        assert_eq!(staged, 0, "{name}: staged rows remain");
        let temps = temp_files(&dir.path().join("objects"));
        assert!(
            temps.is_empty(),
            "{name}: {} temp files remain",
            temps.len()
        );
        storage.shutdown().await.unwrap();
        drop(storage);
    }
}

/// Whether the child's deletion is `Applied`.
async fn deletion_applied(storage: &crate::Storage, name: &str) -> bool {
    match storage.deletion_status(deletion_id()).await.unwrap() {
        DeletionStatus::Applied { .. } => true,
        status => {
            assert!(
                matches!(status, DeletionStatus::Pending),
                "{name}: the deletion status is {status:?}"
            );
            false
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run by every_media_hook_aborts_safely"]
async fn media_child() {
    let Ok(dir) = std::env::var("LLMOBS_MEDIA_CRASH_DIR") else {
        return;
    };
    let name = std::env::var("LLMOBS_MEDIA_CRASH_POINT").unwrap();
    let point = parse_point(&name).unwrap_or_else(|| panic!("unknown crash point {name}"));
    let storage = crate::Storage::open(quiet_config(Path::new(&dir)))
        .await
        .unwrap();
    storage.hooks().abort_at(point);
    storage.put_media(project(), media_blob()).await.unwrap();
    panic!("media put finished without reaching {name}");
}

/// Run alone by `just test`, like [`every_flush_hook_aborts_safely`]: a spawned
/// child holds a copy of every open file descriptor until it execs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns processes; `just test` runs it in its own test process"]
async fn every_media_hook_aborts_safely() {
    for (name, _) in MEDIA_POINTS {
        let dir = tempfile::tempdir().unwrap();
        run_child(
            "crash_tests::media_child",
            "LLMOBS_MEDIA_CRASH_DIR",
            "LLMOBS_MEDIA_CRASH_POINT",
            name,
            dir.path(),
        );

        // The reopen runs the startup temp-file cleanup, under `media/` too.
        let storage = crate::Storage::open(quiet_config(dir.path()))
            .await
            .unwrap_or_else(|error| panic!("{name}: reopen failed: {error}"));
        let temps = temp_files(&dir.path().join("objects"));
        assert!(
            temps.is_empty(),
            "{name}: {} temp files remain",
            temps.len()
        );
        storage.collect_staged(Duration::ZERO).await.unwrap();
        let (rows, objects) = media_state(dir.path()).await;
        assert_eq!(rows, objects, "{name}: media rows and objects disagree");

        storage.put_media(project(), media_blob()).await.unwrap();
        let (rows, objects) = media_state(dir.path()).await;
        assert_eq!(
            rows, objects,
            "{name}: media rows and objects disagree after the put"
        );
        let read = storage
            .get_media(project(), &media_blob().sha256, None)
            .await
            .unwrap();
        assert_eq!(read.bytes, media_blob().bytes, "{name}: the blob's bytes");
        assert_eq!(
            read.mime_type,
            media_blob().mime_type,
            "{name}: the blob's mime type"
        );
        assert_eq!(
            read.size,
            media_blob().bytes.len() as u64,
            "{name}: the blob's size"
        );
        let (_, staged) = catalog_counts(dir.path()).await;
        assert_eq!(staged, 0, "{name}: staged rows remain");
        storage.shutdown().await.unwrap();
        drop(storage);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run by every_drop_hook_aborts_safely"]
async fn drop_child() {
    let Ok(dir) = std::env::var("LLMOBS_DROP_CRASH_DIR") else {
        return;
    };
    let name = std::env::var("LLMOBS_DROP_CRASH_POINT").unwrap();
    let point = parse_point(&name).unwrap_or_else(|| panic!("unknown crash point {name}"));

    // One flush cycle with spans of P, and one blob of P.
    let storage = crate::Storage::open(quiet_config(Path::new(&dir)))
        .await
        .unwrap();
    let start = crash_at(10, 0);
    let spans = vec![crate::test_support::span_row(
        0,
        0,
        start,
        start + 1_000_000_000,
    )];
    storage
        .append_spans(project(), crate::test_support::span_rows(&spans))
        .await
        .unwrap();
    storage.put_media(project(), media_blob()).await.unwrap();
    storage.shutdown().await.unwrap();

    let storage = crate::Storage::open(quiet_config(Path::new(&dir)))
        .await
        .unwrap();
    storage.hooks().abort_at(point);
    storage.drop_project(project()).await.unwrap();
    panic!("project drop finished without reaching {name}");
}

/// Run alone by `just test`, like [`every_flush_hook_aborts_safely`]: a spawned
/// child holds a copy of every open file descriptor until it execs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns processes; `just test` runs it in its own test process"]
async fn every_drop_hook_aborts_safely() {
    for (name, _) in DROP_POINTS {
        let dir = tempfile::tempdir().unwrap();
        run_child(
            "crash_tests::drop_child",
            "LLMOBS_DROP_CRASH_DIR",
            "LLMOBS_DROP_CRASH_POINT",
            name,
            dir.path(),
        );

        // The reopen finishes a crash after the drop commit; the second
        // `drop_project` is idempotent either way.
        let storage = crate::Storage::open(quiet_config(dir.path()))
            .await
            .unwrap_or_else(|error| panic!("{name}: reopen failed: {error}"));
        storage.drop_project(project()).await.unwrap();
        assert!(
            stored_ids(&storage).await.is_empty(),
            "{name}: P's spans are visible"
        );
        assert!(
            stored_trace_ids(&storage, true).await.is_empty(),
            "{name}: P's partials are visible"
        );
        let error = storage
            .append_spans(
                project(),
                crate::test_support::span_rows(&[crate::test_support::span_row(0, 40, 0, 0)]),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, AppendError::ProjectDropped),
            "{name}: P's appends are not refused"
        );
        storage.collect_obsolete(Duration::ZERO).await.unwrap();
        let leftovers = project_objects(dir.path(), project());
        assert!(
            leftovers.is_empty(),
            "{name}: {} objects of P remain",
            leftovers.len()
        );
        let temps = temp_files(&dir.path().join("objects"));
        assert!(
            temps.is_empty(),
            "{name}: {} temp files remain",
            temps.len()
        );
        assert_eq!(
            project_state(dir.path(), project()).await,
            "dropped",
            "{name}: P is not dropped"
        );
        let (obsolete, staged) = catalog_counts(dir.path()).await;
        assert_eq!(obsolete, 0, "{name}: obsolete rows remain");
        assert_eq!(staged, 0, "{name}: staged rows remain");
        storage.shutdown().await.unwrap();
        drop(storage);
    }
}

/// `obsolete` and `staged_objects` row counts of `catalog.db`.
async fn catalog_counts(dir: &Path) -> (i64, i64) {
    let mut connection = connect_readonly(dir).await;
    let obsolete = sqlx::query_scalar("SELECT COUNT(*) FROM data_files WHERE state = 'obsolete'")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    let staged = sqlx::query_scalar("SELECT COUNT(*) FROM staged_objects")
        .fetch_one(&mut connection)
        .await
        .unwrap();
    (obsolete, staged)
}

/// The `trace_id` bytes of every row of the project's `spans` (`traces` is
/// false) or `traces` provider, sorted.
async fn stored_trace_ids(storage: &crate::Storage, traces: bool) -> Vec<Vec<u8>> {
    let query = storage.query(project(), Default::default()).await.unwrap();
    let ctx = SessionContext::new_with_config_rt(
        SessionConfig::new(),
        std::sync::Arc::clone(&query.runtime),
    );
    let provider = if traces { query.traces } else { query.spans };
    ctx.register_table("t", provider).unwrap();
    let batches = ctx
        .sql("SELECT trace_id FROM t")
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();
    let mut ids = Vec::new();
    for batch in batches {
        let column = batch
            .column(0)
            .as_any()
            .downcast_ref::<FixedSizeBinaryArray>()
            .unwrap();
        for index in 0..batch.num_rows() {
            ids.push(column.value(index).to_vec());
        }
    }
    ids.sort();
    ids
}

/// The `trace_id` bytes `span_row` builds for `trace`.
fn trace_bytes(trace: u16) -> Vec<u8> {
    crate::test_support::span_row(trace, 0, 0, 0)
        .trace_id
        .as_bytes()
        .to_vec()
}

/// The deletion id `deletion_child` and its parent share: the parent's
/// re-registration is a no-op exactly when the child's crash came after the
/// registration committed.
fn deletion_id() -> uuid::Uuid {
    uuid::Uuid::from_u128(0x018f_6b4e_7c1a_7c1a_8c1a_0000_0000_00d1)
}

/// One `Traces` deletion of trace 1, cut off at 12:00 of day D.
fn trace_deletion() -> Deletion {
    Deletion {
        id: deletion_id(),
        project: project(),
        cutoff: crash_at(12, 0),
        scope: DeletionScope::Traces {
            trace_ids: vec![crate::test_support::span_row(1, 0, 0, 0).trace_id],
        },
    }
}

/// The sorted span ids `deletion_child` appends for trace 0: one per cycle.
fn deletion_span_ids() -> Vec<Vec<u8>> {
    let mut ids: Vec<Vec<u8>> = (0..3_u8)
        .map(|index| {
            crate::test_support::span_row(0, index, 0, 0)
                .span_id
                .as_bytes()
                .to_vec()
        })
        .collect();
    ids.sort();
    ids
}

/// The blob `media_child` stores.
fn media_blob() -> MediaBlob {
    let bytes = Bytes::from_static(b"crash-child-media-blob");
    MediaBlob {
        sha256: Sha256::digest(bytes.as_ref()).into(),
        mime_type: "image/png".to_owned(),
        bytes,
    }
}

/// Every file under `dir`, recursively. A missing `dir` is empty.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_under(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// Every file under `objects/tables/*/project=<project>` and
/// `objects/media/<project>`: all of `project`'s objects, whatever their state
/// in the catalog.
fn project_objects(dir: &Path, project: ProjectId) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for table in [FileTable::Spans, FileTable::Scores, FileTable::Traces] {
        found.extend(files_under(
            &dir.join("objects")
                .join("tables")
                .join(table.as_str())
                .join(format!("project={project}")),
        ));
    }
    found.extend(files_under(
        &dir.join("objects").join("media").join(project.to_string()),
    ));
    found
}

/// The `media` rows' object paths and the files under `objects/media/`, both
/// sorted: equal exactly when no object is without a row and no row without
/// an object.
async fn media_state(dir: &Path) -> (Vec<String>, Vec<String>) {
    let rows: Vec<String> = sqlx_strings(
        dir,
        "SELECT 'media/' || project_id || '/' || sha256 FROM media",
    )
    .await;
    let objects_root = dir.join("objects");
    let mut objects: Vec<String> = files_under(&objects_root.join("media"))
        .into_iter()
        .map(|path| {
            path.strip_prefix(&objects_root)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    objects.sort();
    (rows, objects)
}

/// Every `String` of `sql`, sorted, over a read-only `catalog.db`.
async fn sqlx_strings(dir: &Path, sql: &'static str) -> Vec<String> {
    let mut connection = connect_readonly(dir).await;
    let mut rows: Vec<String> = sqlx::query_scalar(sql)
        .fetch_all(&mut connection)
        .await
        .unwrap();
    rows.sort();
    rows
}

/// The `projects.state` of `project`.
async fn project_state(dir: &Path, project: ProjectId) -> String {
    let mut connection = connect_readonly(dir).await;
    sqlx::query_scalar("SELECT state FROM projects WHERE project_id = ?")
        .bind(project.to_string())
        .fetch_one(&mut connection)
        .await
        .unwrap()
}

/// A read-only connection to `catalog.db`.
async fn connect_readonly(dir: &Path) -> sqlx::SqliteConnection {
    use sqlx::ConnectOptions;
    sqlx::sqlite::SqliteConnectOptions::new()
        .filename(dir.join("catalog.db"))
        .read_only(true)
        .connect()
        .await
        .unwrap()
}

/// Every file under `dir` whose name marks a leftover temp file.
fn temp_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(temp_files(&path));
        } else {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') && name.contains(".tmp-") {
                found.push(path);
            }
        }
    }
    found
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_flush_syncs_the_file_before_the_catalog_commit() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = quiet_config(dir.path());
    // Serial uploads make the two objects' hook order deterministic.
    config.flush.upload_concurrency = 1;
    let storage = crate::Storage::open(config).await.unwrap();
    storage
        .append_spans(project(), spans_at(&[1]))
        .await
        .unwrap();
    storage.shutdown().await.unwrap();
    let steps: Vec<HookPoint> = storage
        .hooks()
        .points()
        .into_iter()
        .filter(|point| {
            matches!(
                point,
                HookPoint::ObjectWrite
                    | HookPoint::ObjectSync
                    | HookPoint::ObjectRename
                    | HookPoint::ObjectDirSync
                    | HookPoint::CatalogCommit
                    | HookPoint::FlushAfterTruncate
            )
        })
        .collect();
    assert_eq!(
        steps,
        vec![
            // The spans file, then the traces file derived from it.
            HookPoint::ObjectWrite,
            HookPoint::ObjectSync,
            HookPoint::ObjectRename,
            HookPoint::ObjectDirSync,
            HookPoint::ObjectWrite,
            HookPoint::ObjectSync,
            HookPoint::ObjectRename,
            HookPoint::ObjectDirSync,
            HookPoint::CatalogCommit,
            HookPoint::FlushAfterTruncate,
        ],
        "durability order: {steps:?}"
    );
}
