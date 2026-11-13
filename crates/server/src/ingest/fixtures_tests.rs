//! Golden ingest test and concurrency tests for `POST /v1/traces` (G1.9).
//!
//! Every blessed case under `fixtures/otlp` is sent over HTTP, once as JSON and once as
//! protobuf, to a project of its own, and the stored rows must equal what `normalize`
//! produces for the same request.

use std::collections::{BTreeMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arrow::array::{Array, FixedSizeBinaryArray, RecordBatch};
use arrow::compute::{cast, concat_batches};
use arrow::datatypes::SchemaRef;
use axum::http::{Method, StatusCode};
use bytesize::ByteSize;
use datafusion::prelude::{SessionConfig, SessionContext};
use llmobs_core::batch::spans_to_record_batch;
use llmobs_core::ids::ProjectId;
use llmobs_core::span::Span;
use llmobs_core::time_serde::parse_rfc3339;
use llmobs_otel::{IngestContext, PriceRow, PriceTable, decode_json, normalize};
use llmobs_storage::config::{
    AnchorConfig, FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig, WalConfig,
};
use llmobs_storage::{QueryOptions, Storage};
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceResponse;
use prost::Message;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::clock::Clock;
use crate::config::{
    AuthSettings, IngestConfig, InitConfig, LogConfig, MediaConfig, PublicUrl, ServerConfig,
};
use crate::ingest::environment::rewrite_reserved_environment;
use crate::meta::MetaDb;
use crate::serve;
use crate::test_support::{Seeded, TestApp};

const FIXTURES: &str = "../../fixtures/otlp";
const DEFAULT_INGEST_TIME: &str = "2026-01-01T00:00:00.000000000Z";
const DEFAULT_MAX_ATTRIBUTE_BYTES: u64 = 8_388_608;
const DEFAULT_MEDIA_MIN_BYTES: u64 = 4_096;
const DEFAULT_MEDIA_MAX_BYTES: u64 = 33_554_432;
const OTLP_KEY: &str = "sk-llmobs-0123456789ABCDEFGHIJKLMNOPQRSTUV";

/// The `settings` that change the app's configuration. Cases with equal bounds share an app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Bounds {
    max_attribute_bytes: u64,
    media_min_bytes: u64,
    media_max_bytes: u64,
}

/// One blessed fixture case, loaded from its directory.
struct Case {
    /// `group/case`, for messages.
    label: String,
    /// The project slug: `group-case`.
    slug: String,
    bounds: Bounds,
    retention_days: Option<u32>,
    ingest_time: i64,
    /// `input.json`, the OTLP/JSON request.
    input: Vec<u8>,
    /// `expected.json`.
    expected: Value,
}

#[derive(Debug, Clone, Copy)]
enum Wire {
    Json,
    Protobuf,
}

impl Wire {
    fn name(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Protobuf => "protobuf",
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Self::Json => "application/json",
            Self::Protobuf => "application/x-protobuf",
        }
    }
}

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURES)
}

fn read_json(path: &Path) -> Value {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_slice(&bytes).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The subdirectories of `dir`, sorted by name.
fn subdirectories(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("fixtures directory is readable")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// `value` for `key` under `settings`, or `default` when the key is absent.
fn setting(settings: &Value, key: &str, default: u64) -> u64 {
    match settings.get(key) {
        None => default,
        Some(value) => value
            .as_u64()
            .unwrap_or_else(|| panic!("settings.{key} is not an integer")),
    }
}

fn load_case(group: &Path, dir: &Path) -> Case {
    let group_name = group.file_name().expect("group name").to_string_lossy();
    let case_name = dir.file_name().expect("case name").to_string_lossy();
    let case_json = read_json(&dir.join("case.json"));
    let settings = case_json.get("settings").cloned().unwrap_or(Value::Null);
    let retention_days = settings.get("retention_days").map(|value| {
        let days = value
            .as_u64()
            .expect("settings.retention_days is an integer");
        u32::try_from(days).expect("settings.retention_days fits in u32")
    });
    let ingest_time = case_json
        .pointer("/context/ingest_time")
        .and_then(Value::as_str)
        .unwrap_or(DEFAULT_INGEST_TIME);
    Case {
        label: format!("{group_name}/{case_name}"),
        slug: format!("{group_name}-{case_name}"),
        bounds: Bounds {
            max_attribute_bytes: setting(
                &settings,
                "max_attribute_bytes",
                DEFAULT_MAX_ATTRIBUTE_BYTES,
            ),
            media_min_bytes: setting(&settings, "media_min_bytes", DEFAULT_MEDIA_MIN_BYTES),
            media_max_bytes: setting(&settings, "media_max_bytes", DEFAULT_MEDIA_MAX_BYTES),
        },
        retention_days,
        ingest_time: parse_rfc3339(ingest_time).expect("context.ingest_time is RFC 3339"),
        input: std::fs::read(dir.join("input.json")).expect("input.json is readable"),
        expected: read_json(&dir.join("expected.json")),
    }
}

/// Every case that has an `expected.json`, and how many such files exist. A case without
/// one is awaiting bless (D118) and is skipped with a message.
fn load_cases(root: &Path) -> (Vec<Case>, usize) {
    let mut cases = Vec::new();
    let mut found = 0;
    for group in subdirectories(root) {
        for dir in subdirectories(&group) {
            if dir.join("expected.json").is_file() {
                found += 1;
                cases.push(load_case(&group, &dir));
            } else {
                let label = format!(
                    "{}/{}",
                    group.file_name().expect("group name").to_string_lossy(),
                    dir.file_name().expect("case name").to_string_lossy()
                );
                eprintln!("awaiting bless: {label}");
            }
        }
    }
    (cases, found)
}

/// The sort order of the golden comparison (both sides, and the SQL `ORDER BY`).
fn span_key(
    span: &Span,
) -> (
    llmobs_core::ids::TraceId,
    llmobs_core::ids::SpanId,
    i64,
    i64,
    &str,
) {
    (
        span.trace_id,
        span.span_id,
        span.start_time,
        span.end_time,
        span.name.as_str(),
    )
}

/// Every stored span of `project`, in the order of the expected batch.
async fn stored_batch(storage: &Storage, project: ProjectId) -> RecordBatch {
    let query = storage
        .query(project, QueryOptions::default())
        .await
        .expect("query opens");
    // The query's runtime carries the object store that the Parquet files are read from.
    let ctx = SessionContext::new_with_config_rt(SessionConfig::new(), query.runtime);
    ctx.register_table("raw_spans", query.spans)
        .expect("spans register");
    let frame = ctx
        .sql("SELECT * FROM raw_spans ORDER BY trace_id, span_id, start_time, end_time, name")
        .await
        .expect("sql");
    let schema: SchemaRef = Arc::new(frame.schema().as_arrow().clone());
    let batches = frame.collect().await.expect("collect");
    concat_batches(&schema, &batches).expect("stored batches concatenate")
}

/// `POST /v1/traces` through the router with the project's key.
async fn post_export(
    app: &TestApp,
    key: &str,
    wire: Wire,
    body: Vec<u8>,
) -> crate::test_support::TestResponse {
    app.request(Method::POST, "/v1/traces")
        .header("authorization", &format!("Bearer {key}"))
        .header("content-type", wire.content_type())
        .body(body)
        .send()
        .await
}

/// Sends `case` in `wire` form to a new project and compares what was stored with
/// `normalize`'s output for the same request.
async fn check_case(app: &TestApp, seeded: &Seeded, case: &Case, prices: &PriceTable, wire: Wire) {
    let label = format!("{} ({})", case.label, wire.name());
    let (project, key) = app
        .add_project(
            seeded,
            &format!("{}-{}", case.slug, wire.name()),
            case.retention_days,
        )
        .await;
    app.state.clock.set(case.ingest_time);
    let request = decode_json(&case.input)
        .unwrap_or_else(|error| panic!("{label}: input does not decode: {error}"));
    let body = match wire {
        Wire::Json => case.input.clone(),
        Wire::Protobuf => request.encode_to_vec(),
    };
    let response = post_export(app, &key, wire, body).await;
    assert_eq!(response.status, StatusCode::OK, "{label}: status");

    let ctx = IngestContext {
        project_id: project.id,
        ingest_time: case.ingest_time,
        retention_days: case.retention_days,
        limits: app.state.config.ingest.limits(&app.state.config.media),
    };
    let mut want = normalize(&request, &ctx, prices);
    rewrite_reserved_environment(&mut want.spans);
    want.spans.sort_by(|a, b| span_key(a).cmp(&span_key(b)));
    let expected = spans_to_record_batch(&want.spans)
        .unwrap_or_else(|_| panic!("{label}: expected batch does not build"));

    let stored = stored_batch(&app.state.storage, project.id).await;
    assert_eq!(stored.num_rows(), expected.num_rows(), "{label}: row count");
    for (index, field) in expected.schema().fields().iter().enumerate() {
        let name = field.name();
        let actual = stored
            .column_by_name(name)
            .unwrap_or_else(|| panic!("{label}: column {name} is not stored"));
        let actual = if actual.data_type() == field.data_type() {
            Arc::clone(actual)
        } else {
            cast(actual.as_ref(), field.data_type())
                .unwrap_or_else(|_| panic!("{label}: column {name} does not cast"))
        };
        assert!(
            expected.column(index).to_data() == actual.to_data(),
            "{label}: column {name} differs"
        );
    }

    let rejected = match wire {
        Wire::Json => response
            .json::<Value>()
            .pointer("/partialSuccess/rejectedSpans")
            .and_then(Value::as_str)
            .map_or(0, |count| {
                count.parse::<u64>().expect("rejectedSpans is a count")
            }),
        Wire::Protobuf => {
            let body = ExportTraceServiceResponse::decode(response.body.as_ref())
                .expect("protobuf response");
            body.partial_success.map_or(0, |partial| {
                u64::try_from(partial.rejected_spans).unwrap_or(0)
            })
        }
    };
    assert_eq!(
        Some(rejected),
        case.expected.get("rejected_spans").and_then(Value::as_u64),
        "{label}: rejected spans"
    );

    for entry in case
        .expected
        .get("media")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let hash = entry["sha256"].as_str().expect("media sha256");
        let mut sha = [0_u8; 32];
        hex::decode_to_slice(hash, &mut sha).expect("media sha256 is hex");
        let read = app
            .state
            .storage
            .get_media(project.id, &sha, None)
            .await
            .unwrap_or_else(|error| panic!("{label}: media {hash} is not stored: {error}"));
        assert_eq!(
            Some(read.size),
            entry["size_bytes"].as_u64(),
            "{label}: media size"
        );
        assert_eq!(
            Some(read.mime_type.as_str()),
            entry["mime_type"].as_str(),
            "{label}: media type"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_fixture_is_stored_as_normalized() {
    let started = Instant::now();
    let root = fixtures_root();
    let (cases, found) = load_cases(&root);
    assert_eq!(cases.len(), found, "every expected.json file is a case");
    assert!(found > 100, "only {found} blessed fixture cases");

    let rows: Vec<PriceRow> =
        serde_json::from_value(read_json(&root.join("prices.json"))).expect("prices.json parses");
    let mut groups: BTreeMap<Bounds, Vec<Case>> = BTreeMap::new();
    for case in cases {
        groups.entry(case.bounds).or_default().push(case);
    }

    let mut ran = 0_usize;
    for (bounds, group) in groups {
        let app = TestApp::with_config(|config| {
            config.ingest.max_attribute_bytes = ByteSize::b(bounds.max_attribute_bytes);
            config.media.min_bytes = ByteSize::b(bounds.media_min_bytes);
            config.media.max_bytes = ByteSize::b(bounds.media_max_bytes);
        })
        .await;
        let seeded = app.seed().await;
        let prices = PriceTable::new(rows.clone()).expect("prices.json compiles");
        app.state.ingest.replace_prices(prices.clone());
        for case in &group {
            check_case(&app, &seeded, case, &prices, Wire::Json).await;
            check_case(&app, &seeded, case, &prices, Wire::Protobuf).await;
            ran += 1;
        }
    }
    eprintln!(
        "golden: {ran} cases, JSON and protobuf, in {:?}",
        started.elapsed()
    );
    assert_eq!(ran, found, "every blessed case ran");
}

/// An OTLP/JSON export of `count` spans with ids `first..first + count`, each starting
/// five seconds before `now_ns` and ending one second before it.
fn export_json(first: u64, count: u64, now_ns: i64) -> String {
    let start = now_ns - 5_000_000_000;
    let end = now_ns - 1_000_000_000;
    let spans: Vec<String> = (first..first + count)
        .map(|id| {
            format!(
                r#"{{"traceId":"{id:032x}","spanId":"{id:016x}","name":"concurrent","kind":1,"startTimeUnixNano":"{start}","endTimeUnixNano":"{end}"}}"#
            )
        })
        .collect();
    format!(
        r#"{{"resourceSpans":[{{"scopeSpans":[{{"spans":[{}]}}]}}]}}"#,
        spans.join(",")
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn parallel_ingest_stores_every_span() {
    const TASKS: u64 = 8;
    const REQUESTS: u64 = 20;
    const SPANS: u64 = 50;

    let app = Arc::new(TestApp::new().await);
    let seeded = app.seed().await;
    let now = app.state.clock.now_ns();
    let mut tasks = Vec::new();
    for task in 0..TASKS {
        let app = Arc::clone(&app);
        let key = seeded.ingest_key.clone();
        tasks.push(tokio::spawn(async move {
            let mut statuses = Vec::new();
            for request in 0..REQUESTS {
                // Distinct trace and span ids across every task and request.
                let first = 1 + (task * REQUESTS + request) * SPANS;
                let body = export_json(first, SPANS, now);
                let response = post_export(&app, &key, Wire::Json, body.into_bytes()).await;
                statuses.push(response.status);
            }
            statuses
        }));
    }
    for task in tasks {
        let statuses = task.await.expect("ingest task");
        assert!(
            statuses.iter().all(|status| *status == StatusCode::OK),
            "statuses: {statuses:?}"
        );
    }
    let stored = stored_batch(&app.state.storage, seeded.project.id).await;
    assert_eq!(stored.num_rows(), (TASKS * REQUESTS * SPANS) as usize);
}

/// `ServerConfig` for a real server on an ephemeral port, with the key from `OTLP_KEY`.
fn serve_config(data_dir: &Path) -> ServerConfig {
    let mut config = ServerConfig {
        data_dir: data_dir.to_path_buf(),
        public_url: PublicUrl::default(),
        http_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        trusted_proxies: Vec::new(),
        otlp_http_addr: None,
        grpc_addr: None,
        storage: StorageConfig {
            data_dir: data_dir.to_path_buf(),
            object_store: None,
            s3: S3Config::default(),
            wal: WalConfig::default(),
            flush: FlushConfig::default(),
            memtable: MemtableConfig::default(),
            query: QueryConfig::default(),
            anchor: AnchorConfig::default(),
            gc: Default::default(),
            compaction: Default::default(),
            deletion: Default::default(),
            min_free_bytes: ByteSize::b(0),
        },
        ingest: IngestConfig::default(),
        media: MediaConfig::default(),
        auth: AuthSettings::default(),
        smtp_url: None,
        email_from: None,
        init: InitConfig::default(),
        log: LogConfig::default(),
    };
    config.init = serde_json::from_value(serde_json::json!({
        "admin_email": "ada@example.com",
        "admin_password": "llmobs-ok1",
        "org": "Acme",
        "project": "Support bot",
        "secret_key": OTLP_KEY,
    }))
    .expect("init config");
    config
}

/// One `POST /v1/traces` over a fresh TCP connection. `None` when the connection fails
/// or closes before a status line arrives.
async fn post_over_tcp(addr: SocketAddr, body: &str) -> Option<u16> {
    let mut stream = TcpStream::connect(addr).await.ok()?;
    let head = format!(
        "POST /v1/traces HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAuthorization: Bearer {OTLP_KEY}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await.ok()?;
    stream.write_all(body.as_bytes()).await.ok()?;
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut response))
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&response);
    let status_line = text.lines().next()?;
    status_line.split_whitespace().nth(1)?.parse().ok()
}

/// Span ids stored for `project`, read from the raw `spans` provider.
async fn stored_span_ids(storage: &Storage, project: ProjectId) -> HashSet<u64> {
    let query = storage
        .query(project, QueryOptions::default())
        .await
        .expect("query opens");
    // The query's runtime carries the object store that the Parquet files are read from.
    let ctx = SessionContext::new_with_config_rt(SessionConfig::new(), query.runtime);
    ctx.register_table("raw_spans", query.spans)
        .expect("spans register");
    let batches = ctx
        .sql("SELECT span_id FROM raw_spans")
        .await
        .expect("sql")
        .collect()
        .await
        .expect("collect");
    let mut ids = HashSet::new();
    for batch in &batches {
        let column = batch.column_by_name("span_id").expect("span_id column");
        let array = column
            .as_any()
            .downcast_ref::<FixedSizeBinaryArray>()
            .expect("span_id is fixed-size binary");
        for index in 0..array.len() {
            let bytes = <[u8; 8]>::try_from(array.value(index)).expect("span ids are 8 bytes");
            ids.insert(u64::from_be_bytes(bytes));
        }
    }
    ids
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_keeps_every_acknowledged_span() {
    const TASKS: u64 = 4;
    const SPANS: u64 = 10;

    let dir = tempfile::tempdir().expect("temp dir");
    let config = serve_config(dir.path());
    let storage_config = config.storage_config();
    let running = serve::bind(config).await.expect("server binds");
    let addr = running.local_addr;
    let now = Clock::System.now_ns();

    let mut tasks = Vec::new();
    for task in 0..TASKS {
        tasks.push(tokio::spawn(async move {
            let mut acknowledged = Vec::new();
            let mut unexpected = Vec::new();
            for request in 0.. {
                let first = 1 + (task * 1_000_000 + request) * SPANS;
                match post_over_tcp(addr, &export_json(first, SPANS, now)).await {
                    Some(200) => acknowledged.extend(first..first + SPANS),
                    Some(status) => {
                        unexpected.push(status);
                        break;
                    }
                    // The listener is gone: shutdown has begun.
                    None => break,
                }
            }
            (acknowledged, unexpected)
        }));
    }

    tokio::time::sleep(Duration::from_millis(200)).await;
    running.shutdown().await.expect("shutdown is clean");

    let mut acknowledged = HashSet::new();
    for task in tasks {
        let (ids, unexpected) = task.await.expect("ingest task");
        assert!(unexpected.is_empty(), "unexpected statuses: {unexpected:?}");
        acknowledged.extend(ids);
    }
    assert!(
        !acknowledged.is_empty(),
        "no request was acknowledged before shutdown"
    );

    let meta = MetaDb::open(dir.path()).await.expect("meta.db reopens");
    let project: String = sqlx::query_scalar("SELECT id FROM projects")
        .fetch_one(meta.reader())
        .await
        .expect("the init project");
    meta.close().await;
    let project: ProjectId = project.parse().expect("project id");

    let storage = Storage::open(storage_config)
        .await
        .expect("storage reopens after shutdown");
    let stored = stored_span_ids(&storage, project).await;
    let missing: Vec<u64> = acknowledged.difference(&stored).copied().collect();
    assert!(
        missing.is_empty(),
        "{} of {} acknowledged spans are missing",
        missing.len(),
        acknowledged.len()
    );
    storage.shutdown().await.expect("storage shuts down");
}
