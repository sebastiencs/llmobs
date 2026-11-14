//! The end-to-end test (D-G-20): the SDK conformance fixtures go through a real
//! server over OTLP/HTTP protobuf, OTLP/HTTP JSON and OTLP/gRPC, and every trace
//! is read back through the API. The three read-backs must agree, and each
//! fixture's read-back is snapshotted. A second test covers the live streams and
//! every read route over real HTTP.

// A failed setup step panics with its own message, as in the other integration tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use llmobs_core::ids::TraceId;
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, trace_service_client::TraceServiceClient,
};
use prost::Message;
use serde_json::{Value, json};

const ORIGIN: &str = "http://127.0.0.1:8080";
const ADMIN_EMAIL: &str = "ada@example.com";
const ADMIN_PASSWORD: &str = "llmobs-e2e-1";
const INIT_KEY: &str = "sk-llmobs-0123456789ABCDEFGHIJKLMNOPQRSTUV";
/// Fields that differ between runs or between projects, replaced at any depth.
const REDACTED_KEYS: &[&str] = &["ingest_time", "project_id", "score_id"];

/// A session-authenticated API client.
struct Api {
    client: reqwest::Client,
    base: String,
    cookie: String,
}

impl Api {
    async fn get(&self, path: &str) -> Value {
        let response = self
            .client
            .get(format!("{}{path}", self.base))
            .header("cookie", &self.cookie)
            .send()
            .await
            .unwrap();
        read_json(response).await
    }

    /// A cookie-authenticated `GET` whose response the caller reads itself, such as a stream.
    async fn get_raw(&self, path: &str, headers: &[(&str, &str)]) -> reqwest::Response {
        let mut request = self
            .client
            .get(format!("{}{path}", self.base))
            .header("cookie", &self.cookie);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request.send().await.unwrap()
    }

    async fn post(&self, path: &str, body: &Value) -> Value {
        let response = self
            .client
            .post(format!("{}{path}", self.base))
            .header("cookie", &self.cookie)
            .header("origin", ORIGIN)
            .json(body)
            .send()
            .await
            .unwrap();
        read_json(response).await
    }
}

async fn read_json(response: reqwest::Response) -> Value {
    let status = response.status();
    let text = response.text().await.unwrap();
    assert!(status.is_success(), "{status}: {text}");
    serde_json::from_str(&text).unwrap()
}

/// Reads `response` until its text contains `needle`, within `wait`. Returns the text read.
async fn read_until(response: &mut reqwest::Response, needle: &str, wait: Duration) -> String {
    let deadline = tokio::time::Instant::now() + wait;
    let mut text = String::new();
    while !text.contains(needle) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let chunk = tokio::time::timeout(remaining, response.chunk())
            .await
            .unwrap_or_else(|_| panic!("no {needle:?} within {wait:?}; read: {text}"))
            .unwrap()
            .unwrap_or_else(|| panic!("the stream ended before {needle:?}; read: {text}"));
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    text
}

/// A running server with the admin signed in.
struct Server {
    running: llmobs_server::serve::Running,
    http: String,
    otlp: String,
    grpc: String,
    client: reqwest::Client,
    api: Api,
    /// The `Proto` project, which `INIT_KEY` ingests into.
    proto_id: String,
}

/// Writes the config plus `extra_config` (after the `[init]` table) into `dir`, starts the
/// server with the embedded UI and signs in.
async fn start(dir: &Path, extra_config: &str) -> Server {
    let config_path = dir.join("llmobs.toml");
    let data_dir = dir.join("data");
    // The config rejects equal listen addresses, so `:0` on all three would fail.
    // Three ports are reserved together (held until all are read) and then released.
    let reserved: Vec<std::net::TcpListener> = (0..3)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    let ports: Vec<u16> = reserved
        .iter()
        .map(|listener| listener.local_addr().unwrap().port())
        .collect();
    drop(reserved);
    fs::write(
        &config_path,
        format!(
            "data_dir = {:?}\nhttp_addr = \"127.0.0.1:{}\"\notlp_http_addr = \"127.0.0.1:{}\"\ngrpc_addr = \"127.0.0.1:{}\"\npublic_url = \"{ORIGIN}\"\n\n[init]\nadmin_email = \"{ADMIN_EMAIL}\"\nadmin_password = \"{ADMIN_PASSWORD}\"\norg = \"Acme\"\nproject = \"Proto\"\nsecret_key = \"{INIT_KEY}\"\n{extra_config}",
            data_dir.display().to_string(),
            ports[0],
            ports[1],
            ports[2],
        ),
    )
    .unwrap();
    let config = llmobs_server::config::ServerConfig::load(Some(&config_path)).unwrap();
    let running = llmobs_server::serve::bind(config, Some(llmobs_ui::router()))
        .await
        .unwrap();
    let http = format!("http://{}", running.local_addr);
    let otlp = format!("http://{}", running.otlp_local_addr.unwrap());
    let grpc = format!("http://{}", running.grpc_local_addr.unwrap());
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    // Sign in. Every later request carries the session cookie.
    let login = client
        .post(format!("{http}/api/auth/login"))
        .header("origin", ORIGIN)
        .json(&json!({"email": ADMIN_EMAIL, "password": ADMIN_PASSWORD}))
        .send()
        .await
        .unwrap();
    assert!(login.status().is_success(), "login: {}", login.status());
    let cookie = login.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let api = Api {
        client: client.clone(),
        base: http.clone(),
        cookie,
    };

    let listed = api.get("/api/v1/orgs/acme/projects").await;
    let proto_id = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|project| project["name"] == "Proto")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    Server {
        running,
        http,
        otlp,
        grpc,
        client,
        api,
        proto_id,
    }
}

/// A project under test: its id, and the bearer key that ingests into it.
struct Project {
    label: &'static str,
    id: String,
    key: String,
}

/// The `fixtures/otlp/llmobs` directory.
fn fixtures_root() -> PathBuf {
    PathBuf::from(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/otlp/llmobs"
    ))
}

/// The fixture directories whose names start with `sdk-`, sorted.
fn sdk_fixtures() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(fixtures_root())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("sdk-"))
        })
        .collect();
    dirs.sort();
    dirs
}

/// The fixture `name`'s `input.json`, with every span's trace id rewritten to `trace`
/// (32 hex digits), so each trace is the test's own.
fn fixture_request(name: &str, trace: &str) -> ExportTraceServiceRequest {
    let bytes = fs::read(fixtures_root().join(name).join("input.json")).unwrap();
    let mut request: ExportTraceServiceRequest = serde_json::from_slice(&bytes).unwrap();
    let trace_id = TraceId::from_str(trace).unwrap().as_bytes().to_vec();
    for resource in &mut request.resource_spans {
        for scope in &mut resource.scope_spans {
            for span in &mut scope.spans {
                span.trace_id = trace_id.clone();
            }
        }
    }
    request
}

/// Sixteen bytes: fifteen zeros, then `index + 1`. The fixtures all use trace `…01`,
/// so each fixture gets its own trace.
fn trace_id_for(index: usize) -> Vec<u8> {
    let mut id = vec![0u8; 16];
    id[15] = u8::try_from(index + 1).unwrap();
    id
}

fn redact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                if REDACTED_KEYS.contains(&key.as_str()) {
                    *child = json!("[redacted]");
                } else {
                    redact(child);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact),
        _ => {}
    }
}

async fn post_otlp_http(
    client: &reqwest::Client,
    url: String,
    key: &str,
    content_type: &str,
    body: Vec<u8>,
) {
    let response = client
        .post(url)
        .header("content-type", content_type)
        .header("authorization", format!("Bearer {key}"))
        .body(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    assert!(status.is_success(), "OTLP/HTTP export: {status}");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_sdk_fixture_round_trips_over_every_transport() {
    let dir = tempfile::tempdir().unwrap();
    let server = start(dir.path(), "").await;

    if std::env::var_os("E2E_REQUIRE_UI").is_some() {
        assert!(
            llmobs_ui::embedded(),
            "E2E_REQUIRE_UI is set, but web/dist was not embedded: build the web first"
        );
    }

    // The UI and unknown API paths.
    let root = server
        .client
        .get(format!("{}/", server.http))
        .send()
        .await
        .unwrap();
    if llmobs_ui::embedded() {
        assert_eq!(root.status(), 200);
        let content_type = root.headers()["content-type"].to_str().unwrap().to_owned();
        assert!(content_type.starts_with("text/html"), "{content_type}");
        // An app deep link whose last segment has a dot (F1).
        let deep = server
            .client
            .get(format!(
                "{}/acme/proto/users/alice%40example.com",
                server.http
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(deep.status(), 200);
        let content_type = deep.headers()["content-type"].to_str().unwrap().to_owned();
        assert!(content_type.starts_with("text/html"), "{content_type}");
    } else {
        assert_eq!(root.status(), 503);
    }
    let missing = server
        .client
        .get(format!("{}/api/v1/nope", server.http))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    let content_type = missing.headers()["content-type"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        content_type.starts_with("application/problem+json"),
        "{content_type}"
    );

    // Three projects, one per transport. The first is the init project.
    let mut projects = vec![Project {
        label: "protobuf",
        id: server.proto_id.clone(),
        key: INIT_KEY.to_owned(),
    }];
    for (label, name) in [("json", "Json"), ("grpc", "Grpc")] {
        let created = server
            .api
            .post("/api/v1/orgs/acme/projects", &json!({"name": name}))
            .await;
        let id = created["id"].as_str().unwrap().to_owned();
        let key_response = server
            .api
            .post(
                &format!("/api/v1/projects/{id}/api-keys"),
                &json!({"kind": "secret", "scopes": ["ingest", "read"]}),
            )
            .await;
        let key = key_response["secret"].as_str().unwrap().to_owned();
        projects.push(Project { label, id, key });
    }

    // Send every fixture to every project, each over its own transport.
    let fixtures = sdk_fixtures();
    let names: Vec<String> = fixtures
        .iter()
        .map(|dir| dir.file_name().unwrap().to_str().unwrap().to_owned())
        .collect();
    let requests: Vec<ExportTraceServiceRequest> = fixtures
        .iter()
        .enumerate()
        .map(|(index, dir)| {
            let bytes = fs::read(dir.join("input.json")).unwrap();
            let mut request: ExportTraceServiceRequest = serde_json::from_slice(&bytes).unwrap();
            let trace_id = trace_id_for(index);
            for resource in &mut request.resource_spans {
                for scope in &mut resource.scope_spans {
                    for span in &mut scope.spans {
                        span.trace_id = trace_id.clone();
                    }
                }
            }
            request
        })
        .collect();

    let mut grpc_client = TraceServiceClient::connect(server.grpc.clone())
        .await
        .unwrap();
    for request in &requests {
        let proto = &projects[0];
        post_otlp_http(
            &server.client,
            format!("{}/v1/traces", server.http),
            &proto.key,
            "application/x-protobuf",
            request.encode_to_vec(),
        )
        .await;

        let json = &projects[1];
        post_otlp_http(
            &server.client,
            format!("{}/v1/traces", server.otlp),
            &json.key,
            "application/json",
            serde_json::to_vec(request).unwrap(),
        )
        .await;

        let grpc_project = &projects[2];
        let mut grpc_request = tonic::Request::new(request.clone());
        grpc_request.metadata_mut().insert(
            "authorization",
            format!("Bearer {}", grpc_project.key)
                .parse()
                .expect("ASCII token"),
        );
        grpc_client.export(grpc_request).await.unwrap();
    }
    drop(grpc_client);

    // Read every trace back through the API, then compare the three projects.
    for (index, name) in names.iter().enumerate() {
        let trace_id = format!("{:032x}", index + 1);
        let mut read_backs = Vec::new();
        for project in &projects {
            let listing = server
                .api
                .get(&format!(
                    "/api/v1/projects/{}/traces?include_eval=true&from=2025-12-31T00:00:00Z&to=2026-01-02T00:00:00Z",
                    project.id
                ))
                .await;
            assert!(
                listing["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|trace| trace["trace_id"] == trace_id),
                "{name}: {} project does not list trace {trace_id}",
                project.label
            );

            let detail = server
                .api
                .get(&format!(
                    "/api/v1/projects/{}/traces/{trace_id}",
                    project.id
                ))
                .await;
            let mut spans = Vec::new();
            for item in detail["spans"].as_array().unwrap() {
                let span_id = item["span"]["span_id"].as_str().unwrap();
                spans.push(
                    server
                        .api
                        .get(&format!(
                            "/api/v1/projects/{}/traces/{trace_id}/spans/{span_id}",
                            project.id
                        ))
                        .await,
                );
            }
            let mut value = json!({"trace": detail, "spans": spans});
            redact(&mut value);
            read_backs.push(value);
        }

        let protobuf = &read_backs[0];
        assert_eq!(
            &read_backs[1], protobuf,
            "{name}: OTLP/JSON read-back differs from protobuf"
        );
        assert_eq!(
            &read_backs[2], protobuf,
            "{name}: OTLP/gRPC read-back differs from protobuf"
        );
        insta::assert_json_snapshot!(name.as_str(), protobuf);
    }

    drop(server.api);
    drop(server.client);
    server.running.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn live_streams_and_every_read_route_work_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    // The media fixture's blob is 70 bytes; the default `media.min_bytes` (4 KiB) would keep
    // it inline, and `GET /media/…` below would be a 404.
    let server = start(dir.path(), "\n[media]\nmin_bytes = \"1B\"\n").await;
    let project = format!("/api/v1/projects/{}", server.proto_id);
    let context_trace = format!("{:032x}", 1);
    let media_trace = format!("{:032x}", 2);

    // The streams open before anything is sent, so the events below must be live.
    let mut traces = server
        .api
        .get_raw(&format!("{project}/traces/stream"), &[])
        .await;
    assert_eq!(traces.status(), 200);
    let content_type = traces.headers()["content-type"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(
        content_type.starts_with("text/event-stream"),
        "{content_type}"
    );
    let mut spans = server
        .api
        .get_raw(&format!("{project}/traces/{context_trace}/stream"), &[])
        .await;
    assert_eq!(spans.status(), 200);
    // A `Last-Event-ID` gets a `resync` first, so the client refetches.
    let mut resumed = server
        .api
        .get_raw(
            &format!("{project}/traces/stream"),
            &[("last-event-id", "1")],
        )
        .await;
    assert_eq!(resumed.status(), 200);
    read_until(&mut resumed, "event: resync", Duration::from_secs(5)).await;
    drop(resumed);

    // The fixture with the session and user, and the one with the media blob.
    for (name, trace) in [
        ("sdk-context", &context_trace),
        ("sdk-media-bytes", &media_trace),
    ] {
        let request = fixture_request(name, trace);
        post_otlp_http(
            &server.client,
            format!("{}/v1/traces", server.http),
            INIT_KEY,
            "application/x-protobuf",
            request.encode_to_vec(),
        )
        .await;
    }

    let span_events = read_until(&mut spans, "event: span", Duration::from_secs(10)).await;
    assert!(
        span_events.contains(&context_trace),
        "no span event for {context_trace}: {span_events}"
    );
    let trace_events = read_until(&mut traces, &context_trace, Duration::from_secs(10)).await;
    assert!(
        trace_events.contains("event: trace"),
        "no trace event for {context_trace}: {trace_events}"
    );
    drop(spans);
    drop(traces);

    // A score write with the init key, then one read of every remaining route.
    let score = server
        .client
        .post(format!("{}/api/v1/scores", server.http))
        .header("authorization", format!("Bearer {INIT_KEY}"))
        .json(&json!({
            "scores": [{
                "target": {"type": "trace", "trace_id": context_trace.clone()},
                "name": "e2e",
                "value": {"data_type": "numeric", "value": 1.0}
            }]
        }))
        .send()
        .await
        .unwrap();
    let status = score.status();
    let text = score.text().await.unwrap();
    assert_eq!(status, 200, "{status}: {text}");

    let window = "from=2025-12-31T00:00:00Z&to=2026-01-02T00:00:00Z";
    let sessions = server
        .api
        .get(&format!("{project}/sessions?{window}"))
        .await;
    assert!(
        sessions["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|session| session["session_id"] == "chat-42"),
        "{sessions}"
    );
    server.api.get(&format!("{project}/sessions/chat-42")).await;
    let users = server.api.get(&format!("{project}/users?{window}")).await;
    assert!(
        users["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|user| user["user_id"] == "u-17"),
        "{users}"
    );
    server.api.get(&format!("{project}/users/u-17")).await;
    let scores = server
        .api
        .get(&format!("{project}/scores?trace_id={context_trace}"))
        .await;
    assert!(
        scores["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|score| score["name"] == "e2e"),
        "{scores}"
    );
    server.api.get(&format!("{project}/metrics?{window}")).await;
    server.api.get(&format!("{project}/schema")).await;

    let media = server
        .api
        .get_raw(
            &format!(
                "{project}/media/497790947d4666760ce38f3c00e852c71fdb66cae849bae8e9ede352719e1581"
            ),
            &[],
        )
        .await;
    assert_eq!(media.status(), 200);
    let bytes = media.bytes().await.unwrap();
    assert_eq!(bytes.len(), 70);

    drop(server.api);
    drop(server.client);
    server.running.shutdown().await.unwrap();
}
