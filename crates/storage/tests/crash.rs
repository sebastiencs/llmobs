//! Kill -9 harness for the WAL.
//!
//! `crash_recovery` is what `cargo test` runs. It spawns `crash_child`, which
//! is ignored unless this parent starts it. M1 checks acknowledged batches,
//! atomic records, unique span ids and increasing sequence numbers. Catalog
//! and `flushed_seq` checks wait for M2.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arrow::array::{Array, FixedSizeBinaryArray, StringArray};
use bytesize::ByteSize;
use datafusion::execution::context::SessionContext;
use llmobs_core::batch::spans_to_record_batch;
use llmobs_core::enums::{SpanKind, SpanStatus};
use llmobs_core::ids::{ProjectId, SpanId, TraceId};
use llmobs_core::span::Span;
use llmobs_storage::config::{
    FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig, WalConfig, WalSync,
};
use llmobs_storage::{ProjectQuery, Storage};

const DEFAULT_ITERATIONS: u32 = 25;
const DEFAULT_SEED: u64 = 0xA1C5_5EED;

fn project() -> ProjectId {
    "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
}

fn child_config(dir: &Path) -> StorageConfig {
    StorageConfig {
        data_dir: dir.to_path_buf(),
        object_store: None,
        s3: S3Config::default(),
        wal: WalConfig {
            segment_bytes: ByteSize::mib(1),
            ..WalConfig::default()
        },
        flush: FlushConfig {
            max_bytes: ByteSize::mib(128),
            ..FlushConfig::default()
        },
        memtable: MemtableConfig {
            hard_limit: ByteSize::mib(256),
        },
        query: QueryConfig::default(),
    }
}

fn span_batch(batch_id: u64, rows: usize) -> arrow::array::RecordBatch {
    let project_id = project();
    let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736"
        .parse::<TraceId>()
        .unwrap();
    let spans: Vec<Span> = (0..rows)
        .map(|row| {
            let mut bytes = (batch_id.saturating_add(1) << 8).to_be_bytes();
            bytes[7] = u8::try_from(row).unwrap_or(0);
            let start = 1_000 + i64::try_from(row).unwrap_or(0);
            Span {
                project_id,
                trace_id,
                span_id: SpanId::from_bytes(bytes).unwrap(),
                parent_span_id: None,
                name: format!("b{batch_id}/{row}"),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "run by crash_recovery as a child process"]
async fn crash_child() {
    let Ok(dir) = std::env::var("LLMOBS_CRASH_CHILD_DIR") else {
        return;
    };
    let run_id: u64 = std::env::var("LLMOBS_CRASH_RUN")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let storage = Arc::new(Storage::open(child_config(Path::new(&dir))).await.unwrap());
    let next = Arc::new(AtomicU64::new(0));
    let log_path = Path::new(&dir).join("acked.log");
    // A kill during writeln leaves a partial line. Cut it back before appending,
    // or this process's first line completes that torn tail into a false ack.
    drop_torn_ack(&log_path);
    let log = Arc::new(Mutex::new(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .unwrap(),
    ));
    for _ in 0..4 {
        let storage = Arc::clone(&storage);
        let next = Arc::clone(&next);
        let log = Arc::clone(&log);
        tokio::spawn(async move {
            loop {
                let n = next.fetch_add(1, Ordering::Relaxed);
                let batch_id = run_id * 1_000_000 + n;
                let rows = usize::try_from(n % 50).unwrap_or(0) + 1;
                let batch = span_batch(batch_id, rows);
                let Ok(receipt) = storage.append_spans(project(), batch).await else {
                    break;
                };
                let mut file = log.lock().unwrap_or_else(|poison| poison.into_inner());
                writeln!(file, "{batch_id} {rows} {}", receipt.seq).unwrap();
                file.sync_data().unwrap();
            }
        });
    }
    std::future::pending::<()>().await;
}

#[test]
fn crash_recovery() {
    let iterations: u32 = std::env::var("LLMOBS_CRASH_ITERATIONS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_ITERATIONS);
    let seed: u64 = std::env::var("LLMOBS_CRASH_SEED")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_SEED);
    let dir = tempfile::tempdir().unwrap();
    let mut state = seed;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    for run in 0..iterations {
        let before = complete_acks(&dir.path().join("acked.log"));
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("crash_child")
            .arg("--ignored")
            .arg("--nocapture")
            .arg("--test-threads=1")
            .env("LLMOBS_CRASH_CHILD_DIR", dir.path())
            .env("LLMOBS_CRASH_RUN", run.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let delay = 50 + (xorshift(&mut state) % 351);
        std::thread::sleep(Duration::from_millis(delay));
        let status = match child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => {
                let _ = child.kill();
                child.wait().unwrap()
            }
            Err(error) => panic!("waiting for the crash child: {error}"),
        };
        assert!(
            status.code().is_none(),
            "crash child exited {status} at iteration {run}, seed {seed}"
        );
        let check = runtime.block_on(check_dir(dir.path(), &before));
        assert!(
            check.is_ok(),
            "crash harness failed at iteration {run}, seed {seed}: {}",
            check.unwrap_err()
        );
    }
}

fn xorshift(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}

struct Ack {
    batch_id: u64,
    rows: usize,
    seq: u64,
}

/// Drops a trailing partial line left by `kill -9` during `writeln`.
fn drop_torn_ack(path: &Path) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    let keep = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |pos| pos + 1);
    if keep != bytes.len() {
        file.set_len(u64::try_from(keep).unwrap()).unwrap();
        file.sync_all().unwrap();
    }
}

fn complete_acks(path: &Path) -> Vec<Ack> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => panic!("reading acked log: {error}"),
    };
    let mut text = String::new();
    file.read_to_string(&mut text).unwrap();
    let valid = match text.rfind('\n') {
        Some(end) => &text[..=end],
        None => "",
    };
    valid
        .lines()
        .map(|line| {
            let mut parts = line.split(' ');
            let batch_id = parts.next().unwrap().parse().unwrap();
            let rows = parts.next().unwrap().parse().unwrap();
            let seq = parts.next().unwrap().parse().unwrap();
            Ack {
                batch_id,
                rows,
                seq,
            }
        })
        .collect()
}

async fn check_dir(dir: &Path, before: &[Ack]) -> Result<(), String> {
    let storage = Storage::open(child_config(dir))
        .await
        .map_err(|error| error.to_string())?;
    let stored = stored_rows(&storage.query(project()).unwrap()).await;
    storage
        .shutdown()
        .await
        .map_err(|error| error.to_string())?;

    let mut by_batch: BTreeMap<u64, BTreeSet<u32>> = BTreeMap::new();
    let mut span_ids = HashSet::new();
    for (name, span_id) in &stored {
        if !span_ids.insert(span_id.clone()) {
            return Err(format!("span id {span_id:?} appears twice"));
        }
        let Some((batch_id, row)) = parse_batch_name(name) else {
            return Err(format!("unexpected span name length {}", name.len()));
        };
        by_batch.entry(batch_id).or_default().insert(row);
    }
    for (batch_id, rows) in &by_batch {
        let expected: BTreeSet<u32> = (0..u32::try_from(rows.len()).unwrap_or(u32::MAX)).collect();
        if rows != &expected {
            return Err(format!("batch {batch_id} is partial"));
        }
    }

    let acks = complete_acks(&dir.join("acked.log"));
    if acks.len() < before.len() {
        return Err("acked log lost a completed line".to_owned());
    }
    let previous_max = before.iter().map(|ack| ack.seq).max().unwrap_or(0);
    let mut seqs = HashSet::new();
    for ack in &acks {
        if !seqs.insert(ack.seq) {
            let matches: Vec<String> = acks
                .iter()
                .filter(|other| other.seq == ack.seq)
                .map(|other| format!("{} {} {}", other.batch_id, other.rows, other.seq))
                .collect();
            return Err(format!(
                "sequence {} appears twice ({})",
                ack.seq,
                matches.join("; ")
            ));
        }
    }
    for ack in acks.iter().skip(before.len()) {
        if ack.seq <= previous_max {
            return Err(format!(
                "sequence {} does not follow {previous_max}",
                ack.seq
            ));
        }
        let found = by_batch.get(&ack.batch_id).map(BTreeSet::len).unwrap_or(0);
        if found != ack.rows {
            return Err(format!(
                "acked batch {} has {found} rows, log says {}",
                ack.batch_id, ack.rows
            ));
        }
    }
    Ok(())
}

fn parse_batch_name(name: &str) -> Option<(u64, u32)> {
    let rest = name.strip_prefix('b')?;
    let (batch_id, row) = rest.split_once('/')?;
    Some((batch_id.parse().ok()?, row.parse().ok()?))
}

async fn stored_rows(query: &ProjectQuery) -> Vec<(String, Vec<u8>)> {
    let ctx = SessionContext::new();
    ctx.register_table("spans_t", Arc::clone(&query.spans))
        .unwrap();
    let batches = ctx
        .sql("SELECT name, span_id FROM spans_t")
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();
    let mut rows = Vec::new();
    for batch in batches {
        let names = batch
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let ids = batch
            .column(1)
            .as_any()
            .downcast_ref::<FixedSizeBinaryArray>()
            .unwrap();
        for index in 0..batch.num_rows() {
            rows.push((names.value(index).to_owned(), ids.value(index).to_vec()));
        }
    }
    rows
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "manual append throughput measurement"]
async fn append_throughput() {
    for sync in [WalSync::Always, WalSync::Interval(Duration::from_millis(5))] {
        let dir = tempfile::tempdir().unwrap();
        let mut config = child_config(dir.path());
        config.wal.sync = sync;
        let storage = Arc::new(Storage::open(config).await.unwrap());
        let started = std::time::Instant::now();
        let mut tasks = Vec::new();
        for task in 0..4 {
            let storage = Arc::clone(&storage);
            tasks.push(tokio::spawn(async move {
                for n in 0..100 {
                    let batch_id = u64::try_from(task).unwrap_or(0) * 1_000 + n;
                    storage
                        .append_spans(project(), span_batch(batch_id, 50))
                        .await
                        .unwrap();
                }
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        let elapsed = started.elapsed().as_secs_f64();
        let appends = 400.0 / elapsed;
        println!("sync {sync:?}: {appends:.0} appends/s ({elapsed:.2}s for 400 batches)");
        storage.shutdown().await.unwrap();
    }
}
