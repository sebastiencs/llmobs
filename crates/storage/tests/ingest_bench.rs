//! M6.6 — Ingest benchmark (`tests/ingest_bench.rs`, ignored).
//!
//! Ingest rate, the flush drain and compaction throughput, on local disk or on
//! S3 (`LLMOBS_TEST_S3_ENDPOINT`, as `tests/s3.rs` reads it). Run by hand in
//! `--release`:
//!
//! ```bash
//! LLMOBS_INGEST_BENCH_DIR=/path/to/empty/dir cargo test --release \
//!   -p llmobs-storage --test ingest_bench -- --ignored --nocapture
//! ```
//!
//! `LLMOBS_INGEST_BENCH_DIR` is emptied first. `LLMOBS_INGEST_BENCH_SECS`
//! (default 60) is the ingest time and `LLMOBS_INGEST_BENCH_WRITERS` (default 8)
//! the number of appending tasks. With S3 the objects go under a fresh
//! `ingest-bench-<uuidv7>` prefix, which is deleted at the end.
//!
//! Compaction runs only as explicit passes: `compaction.interval` is 1 h (the maximum the config accepts), so the
//! background task (30 s by default) does not compact during the measurement.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arrow::array::RecordBatch;
use bytesize::ByteSize;
use futures::TryStreamExt;
use llmobs_core::batch::spans_to_record_batch;
use llmobs_core::enums::{SpanKind, SpanStatus};
use llmobs_core::ids::{ProjectId, SpanId, TraceId};
use llmobs_core::span::Span;
use llmobs_storage::config::{
    CompactionConfig, FlushConfig, MemtableConfig, ObjectStoreUrl, QueryConfig, S3Config,
    StorageConfig, WalConfig,
};
use llmobs_storage::error::AppendError;
use llmobs_storage::{CompactionReport, Storage};
use object_store::aws::AmazonS3Builder;
use object_store::{ObjectStore, ObjectStoreExt};

const SEED: u64 = 1;
const BATCH_SPANS: usize = 1_000;
const SPANS_PER_TRACE: usize = 10;
const LLM_IO_BYTES: usize = 10 * 1024;
const ATTRIBUTE_BYTES: usize = 300;
const POOL: usize = 16;
const SPAN_NS: i64 = 1_000_000;
const HOUR_NS: i64 = 3_600 * 1_000_000_000;
const MIB: f64 = 1024.0 * 1024.0;

// ---------------------------------------------------------------- RNG

/// Seeded xorshift64* — no `rand` dependency, identical for a given seed.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

// ---------------------------------------------------------------- data

fn project() -> ProjectId {
    "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
}

fn trace_id(index: u64) -> TraceId {
    let mut bytes = [0_u8; 16];
    bytes[8..].copy_from_slice(&(index + 1).to_be_bytes());
    TraceId::from_bytes(bytes).unwrap()
}

fn span_id(index: u64) -> SpanId {
    SpanId::from_bytes((index + 1).to_be_bytes()).unwrap()
}

fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64
}

/// Random lowercase words with spaces: compresses like prompt text.
fn text(rng: &mut Rng, len: usize) -> String {
    let mut out = String::with_capacity(len);
    while out.len() < len {
        let byte = if rng.below(6) == 0 {
            b' '
        } else {
            b'a' + rng.below(26) as u8
        };
        out.push(char::from(byte));
    }
    out
}

/// One batch of `BATCH_SPANS` spans, `SPANS_PER_TRACE` per trace. 10% are LLM
/// spans with 10 KiB input and output; the rest carry a 300-byte attribute and
/// no input or output (spec 03's mix).
fn build_batch(rng: &mut Rng, pool: &[String], traces: &AtomicU64, now: i64) -> Vec<Span> {
    let per_batch = BATCH_SPANS / SPANS_PER_TRACE;
    let first = traces.fetch_add(per_batch as u64, Ordering::Relaxed);
    let mut spans = Vec::with_capacity(BATCH_SPANS);
    for t in 0..per_batch {
        let trace = trace_id(first + t as u64);
        for s in 0..SPANS_PER_TRACE {
            let llm = rng.below(10) == 0;
            let (input, output, attributes) = if llm {
                (
                    Some(pool[rng.below(POOL)].clone()),
                    Some(pool[rng.below(POOL)].clone()),
                    BTreeMap::new(),
                )
            } else {
                let mut attributes = BTreeMap::new();
                attributes.insert(
                    "bench.payload".to_owned(),
                    pool[rng.below(POOL)][..ATTRIBUTE_BYTES].to_owned(),
                );
                (None, None, attributes)
            };
            spans.push(Span {
                project_id: project(),
                trace_id: trace,
                span_id: span_id(s as u64),
                parent_span_id: (s > 0).then(|| span_id(0)),
                name: format!("span-{s}"),
                span_kind: if llm { SpanKind::Llm } else { SpanKind::Span },
                start_time: now,
                end_time: now + SPAN_NS,
                status: SpanStatus::Unset,
                status_message: None,
                service_name: None,
                session_id: None,
                user_id: None,
                environment: None,
                release: None,
                tags: Vec::new(),
                metadata: Default::default(),
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
                input,
                input_format: None,
                output,
                output_format: None,
                input_preview: None,
                output_preview: None,
                system_instructions: None,
                tool_definitions: None,
                attributes,
                resource_attributes: Default::default(),
                scope_name: None,
                scope_version: None,
                events: Vec::new(),
                links: Vec::new(),
                media_refs: Vec::new(),
                truncated: Vec::new(),
                conventions: Vec::new(),
                ingest_time: now,
                schema_version: 1,
            });
        }
    }
    spans
}

// ---------------------------------------------------------------- config

/// The S3 target (D119), when `LLMOBS_TEST_S3_ENDPOINT` is set.
struct S3Target {
    endpoint: String,
    bucket: String,
    prefix: String,
}

fn s3_target() -> Option<S3Target> {
    let endpoint = std::env::var("LLMOBS_TEST_S3_ENDPOINT").ok()?;
    let bucket = std::env::var("LLMOBS_TEST_S3_BUCKET").expect("LLMOBS_TEST_S3_BUCKET");
    Some(S3Target {
        endpoint,
        bucket,
        prefix: format!("ingest-bench-{}", uuid::Uuid::now_v7()),
    })
}

fn config(dir: &Path, s3: Option<&S3Target>) -> StorageConfig {
    let (object_store, s3_config) = match s3 {
        Some(t) => (
            Some(ObjectStoreUrl::S3 {
                bucket: t.bucket.clone(),
                prefix: t.prefix.clone(),
            }),
            S3Config {
                region: Some("us-east-1".to_owned()),
                endpoint: Some(t.endpoint.clone()),
                force_path_style: true,
                allow_http: t.endpoint.starts_with("http://"),
            },
        ),
        None => (None, S3Config::default()),
    };
    StorageConfig {
        data_dir: dir.to_path_buf(),
        min_free_bytes: ByteSize::b(0),
        object_store,
        s3: s3_config,
        wal: WalConfig::default(),
        flush: FlushConfig {
            max_bytes: ByteSize::mib(256),
            ..FlushConfig::default()
        },
        memtable: MemtableConfig {
            hard_limit: ByteSize::gib(1),
        },
        query: QueryConfig::default(),
        anchor: Default::default(),
        gc: Default::default(),
        compaction: CompactionConfig {
            interval: Duration::from_secs(3600),
            ..CompactionConfig::default()
        },
        deletion: Default::default(),
    }
}

// ---------------------------------------------------------------- S3 helpers

/// Copied from `tests/s3.rs`.
async fn delete_prefix(endpoint: &str, bucket: &str, prefix: &str) -> Result<(), String> {
    let http = endpoint.starts_with("http://");
    let store = AmazonS3Builder::from_env()
        .with_bucket_name(bucket)
        .with_endpoint(endpoint)
        .with_region("us-east-1")
        .with_virtual_hosted_style_request(false)
        .with_allow_http(http)
        .build()
        .map_err(|error| error.to_string())?;
    let root = object_store::path::Path::from(prefix);
    let locations: Vec<object_store::path::Path> = store
        .list(Some(&root))
        .map_ok(|meta| meta.location)
        .try_collect()
        .await
        .map_err(|error| error.to_string())?;
    for location in locations {
        store
            .delete(&location)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Total size of the objects under the bench prefix.
async fn s3_bytes(target: &S3Target) -> Result<u64, String> {
    let http = target.endpoint.starts_with("http://");
    let store = AmazonS3Builder::from_env()
        .with_bucket_name(&target.bucket)
        .with_endpoint(&target.endpoint)
        .with_region("us-east-1")
        .with_virtual_hosted_style_request(false)
        .with_allow_http(http)
        .build()
        .map_err(|error| error.to_string())?;
    let root = object_store::path::Path::from(target.prefix.as_str());
    let metas: Vec<object_store::ObjectMeta> = store
        .list(Some(&root))
        .try_collect()
        .await
        .map_err(|error| error.to_string())?;
    Ok(metas.iter().map(|meta| meta.size).sum())
}

// ---------------------------------------------------------------- machine

fn read_line(path: &str, prefix: &str) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .find(|line| line.starts_with(prefix))
        .map(|line| {
            line[prefix.len()..]
                .trim()
                .trim_start_matches(':')
                .trim()
                .to_owned()
        })
        .unwrap_or_else(|| "unknown".to_owned())
}

fn command(args: &[&str]) -> String {
    Command::new(args[0])
        .args(&args[1..])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| "unknown".to_owned())
}

fn machine() -> String {
    let cpu = read_line("/proc/cpuinfo", "model name");
    let cores = std::thread::available_parallelism().map_or(0, |n| n.get());
    let mem = read_line("/proc/meminfo", "MemTotal:");
    let disk = command(&["lsblk", "-d", "-o", "NAME,ROTA,MODEL"])
        .lines()
        .nth(1)
        .unwrap_or("unknown")
        .trim()
        .to_owned();
    let kernel = command(&["uname", "-r"]);
    let commit = command(&["git", "rev-parse", "HEAD"]);
    format!(
        "CPU {cpu}, {cores} threads, RAM {mem}, disk [{disk}], kernel {kernel}, commit {}",
        &commit[..commit.len().min(12)]
    )
}

fn dir_bytes(dir: &Path) -> u64 {
    fn walk(path: &Path, total: &mut u64) {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, total);
            } else if let Ok(meta) = p.metadata() {
                *total += meta.len();
            }
        }
    }
    let mut total = 0;
    walk(dir, &mut total);
    total
}

// ---------------------------------------------------------------- ingest

#[derive(Default, Clone, Copy)]
struct Counts {
    spans: u64,
    arrow_bytes: u64,
    backpressure: u64,
    last_ingest: i64,
}

impl Counts {
    fn add(&mut self, other: Counts) {
        self.spans += other.spans;
        self.arrow_bytes += other.arrow_bytes;
        self.backpressure += other.backpressure;
        self.last_ingest = self.last_ingest.max(other.last_ingest);
    }
}

async fn write_until(
    storage: Arc<Storage>,
    deadline: Instant,
    seed: u64,
    pool: Arc<Vec<String>>,
    traces: Arc<AtomicU64>,
) -> Counts {
    let mut rng = Rng::new(seed);
    let mut counts = Counts::default();
    while Instant::now() < deadline {
        let now = now_ns();
        let spans = build_batch(&mut rng, &pool, &traces, now);
        let batch: RecordBatch = spans_to_record_batch(&spans).unwrap();
        drop(spans);
        counts.arrow_bytes += batch.get_array_memory_size() as u64;
        loop {
            match storage.append_spans(project(), batch.clone()).await {
                Ok(_) => break,
                Err(AppendError::Backpressure) => {
                    counts.backpressure += 1;
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("append failed: {error}"),
            }
        }
        counts.spans += batch.num_rows() as u64;
        counts.last_ingest = counts.last_ingest.max(now);
    }
    counts
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "run by hand: LLMOBS_INGEST_BENCH_DIR=<empty dir> cargo test --release --test ingest_bench -- --ignored --nocapture"]
async fn ingest_benchmark() {
    let dir =
        PathBuf::from(std::env::var("LLMOBS_INGEST_BENCH_DIR").expect("LLMOBS_INGEST_BENCH_DIR"));
    let secs: u64 = std::env::var("LLMOBS_INGEST_BENCH_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(60);
    let writers: usize = std::env::var("LLMOBS_INGEST_BENCH_WRITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8);

    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(&dir).unwrap();

    let s3 = s3_target();
    let config = config(&dir, s3.as_ref());
    let storage = Arc::new(Storage::open(config.clone()).await.unwrap());

    let mut rng = Rng::new(SEED);
    let pool: Arc<Vec<String>> = Arc::new(
        (0..POOL)
            .map(|_| text(&mut rng, LLM_IO_BYTES))
            .collect::<Vec<_>>(),
    );
    let traces = Arc::new(AtomicU64::new(0));

    // 1. Ingest.
    let begun = Instant::now();
    let deadline = begun + Duration::from_secs(secs);
    let handles: Vec<_> = (0..writers)
        .map(|w| {
            tokio::spawn(write_until(
                Arc::clone(&storage),
                deadline,
                SEED + w as u64,
                Arc::clone(&pool),
                Arc::clone(&traces),
            ))
        })
        .collect();
    let mut total = Counts::default();
    for handle in handles {
        total.add(handle.await.unwrap());
    }
    let ingest_secs = begun.elapsed().as_secs_f64();
    println!(
        "ingest: {} spans in {:.1} s = {:.0} spans/s, {:.1} MiB/s Arrow, {:.0} Arrow bytes per span, {} backpressure retries",
        total.spans,
        ingest_secs,
        total.spans as f64 / ingest_secs,
        total.arrow_bytes as f64 / MIB / ingest_secs,
        total.arrow_bytes as f64 / total.spans.max(1) as f64,
        total.backpressure,
    );

    // 2. Drain: flush everything, then reopen.
    let drained = Instant::now();
    storage.shutdown().await.unwrap();
    let drain_secs = drained.elapsed().as_secs_f64();
    drop(storage);
    let storage = Storage::open(config.clone()).await.unwrap();
    let flushed = match &s3 {
        Some(t) => s3_bytes(t).await.unwrap(),
        None => dir_bytes(&dir.join("objects")),
    };
    println!("drain: shutdown flushed in {drain_secs:.2} s; flushed object bytes {flushed}");

    // 3. Compaction: explicit passes until one commits nothing.
    let now = total.last_ingest + 3 * HOUR_NS;
    let begun = Instant::now();
    let mut passes = 0_u64;
    let mut jobs = 0_u64;
    let mut failed = 0_u64;
    let mut input_bytes = 0_u64;
    let mut output_bytes = 0_u64;
    loop {
        let report: CompactionReport = storage.compact(now).await.unwrap();
        passes += 1;
        jobs += report.jobs;
        failed += report.failed;
        input_bytes += report.input_bytes;
        output_bytes += report.output_bytes;
        if report.jobs == 0 {
            break;
        }
    }
    let compact_secs = begun.elapsed().as_secs_f64();
    storage.shutdown().await.unwrap();
    drop(storage);

    println!(
        "compaction: {passes} passes, {jobs} jobs, {failed} failed, {compact_secs:.1} s, input {input_bytes} bytes ({:.1} MiB/s), output {output_bytes} bytes",
        input_bytes as f64 / MIB / compact_secs,
    );
    println!(
        "write amplification: {:.2} = (flush {flushed} + compaction output {output_bytes}) / flush {flushed}",
        (flushed as f64 + output_bytes as f64) / flushed.max(1) as f64,
    );

    if let Some(t) = &s3 {
        delete_prefix(&t.endpoint, &t.bucket, &t.prefix)
            .await
            .unwrap();
        println!("deleted prefix {}", t.prefix);
    }

    println!("\n## M6.6 ingest benchmark\n");
    println!("Machine: {}", machine());
    println!(
        "Store: {}, ingest {secs} s, {writers} writers, batches of {BATCH_SPANS} spans, {SPANS_PER_TRACE} spans per trace, flush.max_bytes 256 MiB, memtable.hard_limit 1 GiB, min_free_bytes 0, compaction.interval 1 h",
        if s3.is_some() { "S3" } else { "local disk" },
    );
}
