//! The end-to-end test (D-G-20): the SDK conformance fixtures go through a real
//! server over OTLP/HTTP protobuf, OTLP/HTTP JSON and OTLP/gRPC, and every trace
//! is read back through the API. The three read-backs must agree, and each
//! fixture's read-back is snapshotted.

// A failed setup step panics with its own message, as in the other integration tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

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

/// A project under test: its id, and the bearer key that ingests into it.
struct Project {
    label: &'static str,
    id: String,
    key: String,
}

/// The fixture directories whose names start with `sdk-`, sorted.
fn sdk_fixtures() -> Vec<PathBuf> {
    let root = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/otlp/llmobs"
    ));
    let mut dirs: Vec<PathBuf> = fs::read_dir(root)
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
    let config_path = dir.path().join("llmobs.toml");
    let data_dir = dir.path().join("data");
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
            "data_dir = {:?}\nhttp_addr = \"127.0.0.1:{}\"\notlp_http_addr = \"127.0.0.1:{}\"\ngrpc_addr = \"127.0.0.1:{}\"\npublic_url = \"{ORIGIN}\"\n\n[init]\nadmin_email = \"{ADMIN_EMAIL}\"\nadmin_password = \"{ADMIN_PASSWORD}\"\norg = \"Acme\"\nproject = \"Proto\"\nsecret_key = \"{INIT_KEY}\"\n",
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

    // The UI and unknown API paths.
    let root = client.get(format!("{http}/")).send().await.unwrap();
    if llmobs_ui::embedded() {
        assert_eq!(root.status(), 200);
        let content_type = root.headers()["content-type"].to_str().unwrap().to_owned();
        assert!(content_type.starts_with("text/html"), "{content_type}");
    } else {
        assert_eq!(root.status(), 503);
    }
    let missing = client
        .get(format!("{http}/api/v1/nope"))
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

    // Three projects, one per transport. The first is the init project.
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
    let mut projects = vec![Project {
        label: "protobuf",
        id: proto_id,
        key: INIT_KEY.to_owned(),
    }];
    for (label, name) in [("json", "Json"), ("grpc", "Grpc")] {
        let created = api
            .post("/api/v1/orgs/acme/projects", &json!({"name": name}))
            .await;
        let id = created["id"].as_str().unwrap().to_owned();
        let key_response = api
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

    let mut grpc_client = TraceServiceClient::connect(grpc.clone()).await.unwrap();
    for request in &requests {
        let proto = &projects[0];
        post_otlp_http(
            &client,
            format!("{http}/v1/traces"),
            &proto.key,
            "application/x-protobuf",
            request.encode_to_vec(),
        )
        .await;

        let json = &projects[1];
        post_otlp_http(
            &client,
            format!("{otlp}/v1/traces"),
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
            let listing = api
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

            let detail = api
                .get(&format!(
                    "/api/v1/projects/{}/traces/{trace_id}",
                    project.id
                ))
                .await;
            let mut spans = Vec::new();
            for item in detail["spans"].as_array().unwrap() {
                let span_id = item["span"]["span_id"].as_str().unwrap();
                spans.push(
                    api.get(&format!(
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

    drop(api);
    drop(client);
    running.shutdown().await.unwrap();
}
