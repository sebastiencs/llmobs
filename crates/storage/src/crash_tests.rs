//! Deterministic flush crashes and the durability order.
//!
//! [`child`] is ignored. [`every_flush_hook_aborts_safely`] re-executes this
//! test binary with one hook armed, then reopens the directory. It is ignored
//! too, and `just test` runs it in a test process of its own.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use arrow::array::{Array, FixedSizeBinaryArray};
use bytesize::ByteSize;
use datafusion::execution::context::{SessionConfig, SessionContext};
use llmobs_core::batch::spans_to_record_batch;
use llmobs_core::enums::{SpanKind, SpanStatus};
use llmobs_core::ids::{ProjectId, SpanId, TraceId};
use llmobs_core::span::Span;

use crate::config::{FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig, WalConfig};
use crate::hooks::HookPoint;
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

fn project() -> ProjectId {
    "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
}

fn quiet_config(dir: &Path) -> StorageConfig {
    StorageConfig {
        data_dir: dir.to_path_buf(),
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
    }
}

fn parse_point(name: &str) -> Option<HookPoint> {
    POINTS
        .iter()
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

async fn stored_ids(storage: &crate::Storage) -> Vec<Vec<u8>> {
    let query = storage.query(project(), Default::default()).unwrap();
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

/// Run alone by `just test`. A spawned child holds a copy of every open file
/// descriptor until it execs, including other tests' `LOCK` files, so a
/// concurrent test that reopens its directory in that window gets `Locked`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "spawns processes; `just test` runs it in its own test process"]
async fn every_flush_hook_aborts_safely() {
    for (name, _) in POINTS {
        let dir = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("crash_tests::child")
            .arg("--ignored")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env("LLMOBS_FLUSH_CRASH_DIR", dir.path())
            .env("LLMOBS_FLUSH_CRASH_POINT", name)
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
            "{name} exited {status} instead of aborting"
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
