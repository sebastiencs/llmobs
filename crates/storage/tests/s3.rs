//! Remote puts against a real S3-compatible server (D119).
//!
//! Skipped unless `LLMOBS_TEST_S3_ENDPOINT` is set. Also set
//! `LLMOBS_TEST_S3_BUCKET`, `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`.
//! The bucket must exist.
//!
//! Any S3-compatible server works. The `minio/minio` image is no longer
//! published; Versity Gateway serves a directory as S3. Start it (the
//! `--tmpfs` gives it a writable `/data`):
//!
//! ```text
//! docker run --rm --name llmobs-s3 -p 7070:7070 --tmpfs /data:rw,mode=1777,size=2g -e ROOT_ACCESS_KEY=llmobs -e ROOT_SECRET_KEY=llmobs-secret -e VGW_BACKEND=posix -e VGW_BACKEND_ARGS=/data -e VGW_ARGS="--port :7070" versity/versitygw:v1.11.0
//! ```
//!
//! Create the bucket with curl's built-in request signing:
//!
//! ```text
//! curl --aws-sigv4 "aws:amz:us-east-1:s3" --user llmobs:llmobs-secret -X PUT http://127.0.0.1:7070/llmobs-test
//! ```
//!
//! Then run:
//!
//! ```text
//! LLMOBS_TEST_S3_ENDPOINT=http://127.0.0.1:7070 LLMOBS_TEST_S3_BUCKET=llmobs-test AWS_ACCESS_KEY_ID=llmobs AWS_SECRET_ACCESS_KEY=llmobs-secret cargo test -p llmobs-storage --test s3 -- --nocapture
//! ```

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::time::Duration;

use arrow::array::{Array, StringArray};
use bytesize::ByteSize;
use datafusion::execution::context::{SessionConfig, SessionContext};
use futures::TryStreamExt;
use llmobs_core::batch::spans_to_record_batch;
use llmobs_core::enums::{SpanKind, SpanStatus};
use llmobs_core::ids::{ProjectId, SpanId, TraceId};
use llmobs_core::span::Span;
use llmobs_storage::config::{
    FlushConfig, MemtableConfig, ObjectStoreUrl, QueryConfig, S3Config, StorageConfig, WalConfig,
};
use llmobs_storage::{ProjectQuery, Storage};
use object_store::aws::AmazonS3Builder;
use object_store::{ObjectStore, ObjectStoreExt};
use sqlx::ConnectOptions;
use sqlx::sqlite::SqliteConnectOptions;

const MULTIPART_MIN: i64 = 64 * 1024 * 1024;

fn project() -> ProjectId {
    "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
}

fn config(dir: &std::path::Path, bucket: &str, prefix: &str, endpoint: &str) -> StorageConfig {
    StorageConfig {
        data_dir: dir.to_path_buf(),
        object_store: Some(ObjectStoreUrl::S3 {
            bucket: bucket.to_owned(),
            prefix: prefix.to_owned(),
        }),
        s3: S3Config {
            region: Some("us-east-1".to_owned()),
            endpoint: Some(endpoint.to_owned()),
            force_path_style: true,
            allow_http: endpoint.starts_with("http://"),
        },
        wal: WalConfig::default(),
        flush: FlushConfig {
            max_bytes: ByteSize::mib(512),
            max_age: Duration::from_secs(60 * 60),
            upload_concurrency: 2,
        },
        memtable: MemtableConfig {
            hard_limit: ByteSize::gib(1),
        },
        query: QueryConfig::default(),
    }
}

fn spans(rows: usize, payload: usize) -> arrow::array::RecordBatch {
    let project_id = project();
    let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736"
        .parse::<TraceId>()
        .unwrap();
    let spans: Vec<Span> = (0..rows)
        .map(|row| {
            let mut bytes = (u64::try_from(row).unwrap_or(0).saturating_add(1)).to_be_bytes();
            if bytes == [0; 8] {
                bytes[7] = 1;
            }
            let start = 1_000 + i64::try_from(row).unwrap_or(0);
            Span {
                project_id,
                trace_id,
                span_id: SpanId::from_bytes(bytes).unwrap(),
                parent_span_id: None,
                name: format!("s3/{row}"),
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
                output: (payload > 0).then(|| filler(row, payload)),
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

fn filler(row: usize, len: usize) -> String {
    let mut state = u64::try_from(row)
        .unwrap_or(1)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut out = String::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        for byte in state.to_le_bytes() {
            out.push(char::from(b'!' + (byte % 94)));
            if out.len() == len {
                break;
            }
        }
    }
    out
}

async fn span_count(query: &ProjectQuery) -> usize {
    let ctx = SessionContext::new_with_config_rt(
        SessionConfig::new(),
        std::sync::Arc::clone(&query.runtime),
    );
    ctx.register_table("spans_t", std::sync::Arc::clone(&query.spans))
        .unwrap();
    let batches = ctx
        .sql("SELECT name FROM spans_t")
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();
    batches
        .iter()
        .map(|batch| {
            batch
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .len()
        })
        .sum()
}

async fn largest_object(dir: &std::path::Path) -> Result<i64, String> {
    let mut connection = SqliteConnectOptions::new()
        .filename(dir.join("catalog.db"))
        .read_only(true)
        .busy_timeout(Duration::from_secs(5))
        .connect()
        .await
        .map_err(|error| error.to_string())?;
    let size: Option<i64> = sqlx::query_scalar("SELECT MAX(size_bytes) FROM data_files")
        .fetch_one(&mut connection)
        .await
        .map_err(|error| error.to_string())?;
    size.ok_or_else(|| "catalog has no files".to_owned())
}

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s3_flush_round_trip() {
    let Ok(endpoint) = std::env::var("LLMOBS_TEST_S3_ENDPOINT") else {
        println!("skipped: LLMOBS_TEST_S3_ENDPOINT is not set");
        return;
    };
    let bucket = std::env::var("LLMOBS_TEST_S3_BUCKET").expect("LLMOBS_TEST_S3_BUCKET");
    let prefix = format!("m29-{}", now_ms());
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path(), &bucket, &prefix, &endpoint);
    let outcome = round_trip(&config).await;
    let deleted = delete_prefix(&endpoint, &bucket, &prefix).await;
    outcome.unwrap();
    deleted.unwrap();
}

async fn round_trip(config: &StorageConfig) -> Result<(), String> {
    let storage = Storage::open(config.clone())
        .await
        .map_err(|error| error.to_string())?;
    storage
        .append_spans(project(), spans(4, 0))
        .await
        .map_err(|error| error.to_string())?;
    storage
        .shutdown()
        .await
        .map_err(|error| error.to_string())?;
    let count = span_count(
        &storage
            .query(project())
            .map_err(|error| error.to_string())?,
    )
    .await;
    if count != 4 {
        return Err(format!("rows after the first flush: {count}"));
    }
    drop(storage);

    let storage = Storage::open(config.clone())
        .await
        .map_err(|error| error.to_string())?;
    let count = span_count(
        &storage
            .query(project())
            .map_err(|error| error.to_string())?,
    )
    .await;
    if count != 4 {
        return Err(format!("rows after reopen: {count}"));
    }
    // 12_000 rows of 8 KiB do not compress under the 64 MiB multipart cutoff.
    storage
        .append_spans(project(), spans(12_000, 8 * 1024))
        .await
        .map_err(|error| error.to_string())?;
    storage
        .shutdown()
        .await
        .map_err(|error| error.to_string())?;
    let count = span_count(
        &storage
            .query(project())
            .map_err(|error| error.to_string())?,
    )
    .await;
    if count != 12_004 {
        return Err(format!("rows after the multipart flush: {count}"));
    }
    drop(storage);
    let size = largest_object(&config.data_dir).await?;
    if size <= MULTIPART_MIN {
        return Err(format!(
            "largest object is {size} bytes, multipart starts above {MULTIPART_MIN}"
        ));
    }
    Ok(())
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
