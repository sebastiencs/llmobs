//! Kill -9 harness for the WAL and flush.
//!
//! `crash_recovery` is what `cargo test` runs. It spawns `crash_child`, which
//! is ignored unless this parent starts it. After each kill the parent checks
//! acknowledged rows, catalog files, and `flushed_seq`. A last open collects
//! staged objects with a zero age.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arrow::array::{Array, FixedSizeBinaryArray, Int64Array, StringArray};
use bytesize::ByteSize;
use chrono::{Datelike, NaiveDate};
use datafusion::execution::context::{SessionConfig, SessionContext};
use llmobs_core::batch::spans_to_record_batch;
use llmobs_core::enums::{SpanKind, SpanStatus};
use llmobs_core::ids::{ProjectId, SpanId, TraceId};
use llmobs_core::span::Span;
use llmobs_storage::config::{
    FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig, WalConfig, WalSync,
};
use llmobs_storage::{ProjectQuery, Storage};
use parquet::arrow::ProjectionMask;
use parquet::arrow::arrow_reader::{ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder};
use sqlx::ConnectOptions;
use sqlx::sqlite::SqliteConnectOptions;

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
            max_bytes: ByteSize::mib(2),
            max_age: Duration::from_millis(500),
            upload_concurrency: 2,
        },
        memtable: MemtableConfig {
            hard_limit: ByteSize::mib(256),
        },
        query: QueryConfig::default(),
        anchor: Default::default(),
    }
}

/// 2026-03-05 23:30 UTC, in nanoseconds. The stream spreads over the hour
/// `[D 23:30, D+1 00:30)`, so every row is within `anchor.window` (1 h) of
/// the D+1 midnight.
fn anchor_start() -> i64 {
    let day = NaiveDate::from_ymd_opt(2026, 3, 5).unwrap();
    let days = i64::from(day.num_days_from_ce()) - 719_163;
    days * 86_400_000_000_000 + 23 * 3_600 * 1_000_000_000 + 30 * 60 * 1_000_000_000
}

/// `(iteration, trace index)` identifies a trace; span ids come from the batch
/// counter. Each trace's rows are appended in three consecutive batches, far
/// enough apart that a kill usually splits them across flushes.
fn span_batch(batch_id: u64, rows: usize) -> arrow::array::RecordBatch {
    let project_id = project();
    let sequence = batch_id % 1_000_000;
    let run_id = batch_id / 1_000_000;
    let trace_index = sequence / 3;
    let mut trace_bytes = [0_u8; 16];
    trace_bytes[..8].copy_from_slice(&run_id.to_be_bytes());
    trace_bytes[8..].copy_from_slice(&(trace_index + 1).to_be_bytes());
    let trace_id = TraceId::from_bytes(trace_bytes).unwrap();
    // Three consecutive batches of one trace land at 23:30, 23:50 and 00:10,
    // so every trace straddles the D+1 midnight.
    let start_base =
        anchor_start() + i64::try_from((sequence * 1_200) % 3_600).unwrap() * 1_000_000_000;
    let spans: Vec<Span> = (0..rows)
        .map(|row| {
            // A counter in the upper seven bytes; the row owns the last byte.
            // Byte 6 is never zero, so the id is never all-zero.
            let mut bytes = (batch_id.saturating_add(1) << 8).to_be_bytes();
            bytes[7] = u8::try_from(row).unwrap_or(0);
            let start = start_base + i64::try_from(row).unwrap_or(0) * 1_000_000_000;
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
                ingest_time: start + 1_000_000_000,
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
    let mut flushed = (0_u64, 0_u64);
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
        let check = runtime.block_on(check_dir(dir.path(), &before, flushed));
        match check {
            Ok(next) => flushed = next,
            Err(error) => panic!("crash harness failed at iteration {run}, seed {seed}: {error}"),
        }
    }
    runtime
        .block_on(collect_crash_orphans(dir.path()))
        .unwrap_or_else(|error| panic!("staged collection after the crash run: {error}"));
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

async fn check_dir(dir: &Path, before: &[Ack], previous: (u64, u64)) -> Result<(u64, u64), String> {
    let storage = Storage::open(child_config(dir))
        .await
        .map_err(|error| error.to_string())?;
    let stored = stored_rows(
        &storage
            .query(project(), Default::default())
            .map_err(|error| error.to_string())?,
    )
    .await;
    // Drop leaves the rows in the WAL. Shutdown would flush them, and the next
    // iteration of this directory would not see them in the memtable.
    drop(storage);
    let catalog = read_catalog(dir).await?;
    // `Storage::open` succeeded above, so the open-time `file_sources` check
    // passed. Every committed source pair must still agree with its objects.
    for (spans_path, traces_path) in &catalog.sources {
        check_source_pair(dir, spans_path, traces_path)?;
    }
    anchor_days_are_singular(dir, &catalog)?;
    for (path, size) in &catalog.files {
        let file = dir.join("objects").join(path);
        let meta = std::fs::metadata(&file).map_err(|error| format!("{path}: {error}"))?;
        if meta.len() != *size {
            return Err(format!(
                "{path} is {} bytes, catalog says {size}",
                meta.len()
            ));
        }
    }
    if catalog.spans_seq < previous.0 || catalog.scores_seq < previous.1 {
        return Err(format!(
            "flushed_seq went from ({}, {}) to ({}, {})",
            previous.0, previous.1, catalog.spans_seq, catalog.scores_seq
        ));
    }

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
    Ok((catalog.spans_seq, catalog.scores_seq))
}

struct CatalogView {
    files: Vec<(String, u64)>,
    spans_seq: u64,
    scores_seq: u64,
    staged_at: Vec<i64>,
    /// `(spans path, traces path)` for every `file_sources` row.
    sources: Vec<(String, String)>,
    /// `(path, partition day)` of every active spans file.
    spans_files: Vec<(String, String)>,
}

async fn read_catalog(dir: &Path) -> Result<CatalogView, String> {
    let path = dir.join("catalog.db");
    let mut connection = SqliteConnectOptions::new()
        .filename(&path)
        .read_only(true)
        .busy_timeout(Duration::from_secs(5))
        .connect()
        .await
        .map_err(|error| error.to_string())?;
    let file_rows = sqlx::query("SELECT path, size_bytes FROM data_files")
        .fetch_all(&mut connection)
        .await
        .map_err(|error| error.to_string())?;
    let mut files = Vec::new();
    for row in file_rows {
        let path: String = sqlx::Row::try_get(&row, "path").map_err(|error| error.to_string())?;
        let size: i64 =
            sqlx::Row::try_get(&row, "size_bytes").map_err(|error| error.to_string())?;
        let size = u64::try_from(size).map_err(|_| format!("{path} has a negative size"))?;
        files.push((path, size));
    }
    let seq_rows = sqlx::query("SELECT table_name, flushed_seq FROM table_state")
        .fetch_all(&mut connection)
        .await
        .map_err(|error| error.to_string())?;
    let mut spans_seq = 0_u64;
    let mut scores_seq = 0_u64;
    for row in seq_rows {
        let table: String =
            sqlx::Row::try_get(&row, "table_name").map_err(|error| error.to_string())?;
        let seq: i64 =
            sqlx::Row::try_get(&row, "flushed_seq").map_err(|error| error.to_string())?;
        let seq = u64::try_from(seq).map_err(|_| format!("{table} flushed_seq is negative"))?;
        match table.as_str() {
            "spans" => spans_seq = seq,
            "scores" => scores_seq = seq,
            _ => return Err(format!("unknown table_state row {table}")),
        }
    }
    let staged_at = sqlx::query_scalar("SELECT created_at FROM staged_objects")
        .fetch_all(&mut connection)
        .await
        .map_err(|error| error.to_string())?;
    let source_rows = sqlx::query(
        "SELECT s.path AS spans_path, t.path AS traces_path
         FROM file_sources fs
         JOIN data_files s ON s.id = fs.spans_file_id
         JOIN data_files t ON t.id = fs.traces_file_id",
    )
    .fetch_all(&mut connection)
    .await
    .map_err(|error| error.to_string())?;
    let mut sources = Vec::new();
    for row in source_rows {
        let spans_path: String =
            sqlx::Row::try_get(&row, "spans_path").map_err(|error| error.to_string())?;
        let traces_path: String =
            sqlx::Row::try_get(&row, "traces_path").map_err(|error| error.to_string())?;
        sources.push((spans_path, traces_path));
    }
    let spans_rows = sqlx::query(
        "SELECT path, day FROM data_files
         WHERE table_name = 'spans' AND state = 'active'",
    )
    .fetch_all(&mut connection)
    .await
    .map_err(|error| error.to_string())?;
    let mut spans_files = Vec::new();
    for row in spans_rows {
        let path: String = sqlx::Row::try_get(&row, "path").map_err(|error| error.to_string())?;
        let day: String = sqlx::Row::try_get(&row, "day").map_err(|error| error.to_string())?;
        spans_files.push((path, day));
    }
    Ok(CatalogView {
        files,
        spans_seq,
        scores_seq,
        staged_at,
        sources,
        spans_files,
    })
}

/// Every trace's rows must live in exactly one partition day. All rows are
/// near one midnight, so the anchor must hold for all of them (D86).
fn anchor_days_are_singular(dir: &Path, catalog: &CatalogView) -> Result<(), String> {
    let mut seen: BTreeMap<Vec<u8>, String> = BTreeMap::new();
    for (path, day) in &catalog.spans_files {
        let reader = projected_reader(dir, path, &["trace_id"])?;
        for batch in reader {
            let batch = batch.map_err(|error| format!("{path}: {error}"))?;
            let ids = batch
                .column_by_name("trace_id")
                .and_then(|column| column.as_any().downcast_ref::<FixedSizeBinaryArray>())
                .ok_or_else(|| format!("{path}: no trace_id column"))?;
            for row in 0..batch.num_rows() {
                let id = ids.value(row).to_vec();
                match seen.get(&id) {
                    Some(other) if other != day => {
                        return Err(format!("trace appears in partition days {other} and {day}"));
                    }
                    _ => {
                        seen.insert(id, day.clone());
                    }
                }
            }
        }
    }
    Ok(())
}

/// The sync Parquet reader over a local object, projected to `columns`.
fn projected_reader(
    dir: &Path,
    path: &str,
    columns: &[&str],
) -> Result<ParquetRecordBatchReader, String> {
    let file = std::fs::File::open(dir.join("objects").join(path))
        .map_err(|error| format!("{path}: {error}"))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|error| format!("{path}: {error}"))?;
    let mask = ProjectionMask::columns(builder.parquet_schema(), columns.iter().copied());
    builder
        .with_projection(mask)
        .build()
        .map_err(|error| format!("{path}: {error}"))
}

fn object_trace_ids(dir: &Path, path: &str) -> Result<(BTreeSet<Vec<u8>>, usize), String> {
    let reader = projected_reader(dir, path, &["trace_id"])?;
    let mut ids = BTreeSet::new();
    let mut rows = 0;
    for batch in reader {
        let batch = batch.map_err(|error| format!("{path}: {error}"))?;
        let column = batch
            .column_by_name("trace_id")
            .ok_or_else(|| format!("{path}: no trace_id column"))?;
        let ids_column = column
            .as_any()
            .downcast_ref::<FixedSizeBinaryArray>()
            .ok_or_else(|| format!("{path}: trace_id is not fixed-size binary"))?;
        for index in 0..batch.num_rows() {
            ids.insert(ids_column.value(index).to_vec());
        }
        rows += batch.num_rows();
    }
    Ok((ids, rows))
}

fn object_partials(dir: &Path, path: &str) -> Result<(BTreeSet<Vec<u8>>, i64), String> {
    let reader = projected_reader(dir, path, &["trace_id", "span_count"])?;
    let mut ids = BTreeSet::new();
    let mut total = 0_i64;
    for batch in reader {
        let batch = batch.map_err(|error| format!("{path}: {error}"))?;
        let ids_column = batch
            .column_by_name("trace_id")
            .and_then(|column| column.as_any().downcast_ref::<FixedSizeBinaryArray>())
            .ok_or_else(|| format!("{path}: no trace_id column"))?;
        let counts = batch
            .column_by_name("span_count")
            .and_then(|column| column.as_any().downcast_ref::<Int64Array>())
            .ok_or_else(|| format!("{path}: no span_count column"))?;
        for index in 0..batch.num_rows() {
            ids.insert(ids_column.value(index).to_vec());
            total = total.saturating_add(counts.value(index));
        }
    }
    Ok((ids, total))
}

/// spec 03 §3: a traces file's `sum(span_count)` equals its spans file's row
/// count, and both hold the same set of trace ids.
fn check_source_pair(dir: &Path, spans_path: &str, traces_path: &str) -> Result<(), String> {
    let (spans_ids, spans_rows) = object_trace_ids(dir, spans_path)?;
    let (traces_ids, total) = object_partials(dir, traces_path)?;
    let spans_rows = i64::try_from(spans_rows)
        .map_err(|_| format!("{spans_path}: row count does not fit in i64"))?;
    if total != spans_rows {
        return Err(format!(
            "{traces_path}: sum(span_count) {total} != {spans_rows} rows in {spans_path}"
        ));
    }
    if spans_ids != traces_ids {
        return Err(format!(
            "{traces_path}: trace ids differ from {spans_path} ({:?} vs {:?})",
            traces_ids.len(),
            spans_ids.len()
        ));
    }
    Ok(())
}

fn now_ns() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Opens the directory, flushes what the last kill left in the WAL, then
/// collects every staged row written before this call.
async fn collect_crash_orphans(dir: &Path) -> Result<(), String> {
    let started = now_ns();
    let storage = Storage::open(child_config(dir))
        .await
        .map_err(|error| error.to_string())?;
    storage
        .shutdown()
        .await
        .map_err(|error| error.to_string())?;
    storage
        .collect_staged(Duration::ZERO)
        .await
        .map_err(|error| error.to_string())?;
    drop(storage);
    let catalog = read_catalog(dir).await?;
    for created_at in catalog.staged_at {
        if created_at < started {
            return Err(format!(
                "staged row from before the final open survived collection ({created_at})"
            ));
        }
    }
    let temps = temp_files(&dir.join("objects"));
    if !temps.is_empty() {
        return Err(format!(
            "{} temp file(s) remain under objects/",
            temps.len()
        ));
    }
    Ok(())
}

fn temp_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return found,
        Err(_) => return vec![dir.to_path_buf()],
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(temp_files(&path));
        } else if is_temp_name(&entry.file_name()) {
            found.push(path);
        }
    }
    found
}

fn is_temp_name(name: &std::ffi::OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    name.starts_with('.') && name.contains(".tmp-")
}

fn parse_batch_name(name: &str) -> Option<(u64, u32)> {
    let rest = name.strip_prefix('b')?;
    let (batch_id, row) = rest.split_once('/')?;
    Some((batch_id.parse().ok()?, row.parse().ok()?))
}

async fn stored_rows(query: &ProjectQuery) -> Vec<(String, Vec<u8>)> {
    let ctx = SessionContext::new_with_config_rt(SessionConfig::new(), Arc::clone(&query.runtime));
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
