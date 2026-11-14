//! Test helpers of the query commands (and of H10's traces and projects tests): a real server
//! with two personal access tokens and one trace, a canned HTTP server, and a writer that fails
//! like a closed pipe. `#[cfg(test)]` only.

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use llmobs_server::config::ServerConfig;
use llmobs_server::serve::{self, Running};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::credentials;

const ORIGIN: &str = "http://127.0.0.1:8080";
const EMAIL: &str = "ada@example.com";
const PASSWORD: &str = "h9-cli-query-secret";
const KEY: &str = "sk-llmobs-H9CLIQUERY0123456789ABCDEFGHIJKL";

/// A running server with one ingested trace and two personal access tokens.
pub(crate) struct TestServer {
    pub(crate) url: String,
    /// `pat-…` with `["read", "write"]`, owned by the org owner.
    pub(crate) token: String,
    /// `pat-…` with `["read"]`, same user.
    pub(crate) read_token: String,
    /// `acme/app`.
    pub(crate) project: String,
    /// Init secret key of project `acme/app`, scopes `ingest, read, write`.
    pub(crate) key: String,
    running: Running,
    dir: tempfile::TempDir,
}

impl TestServer {
    /// Starts a server, creates the tokens, and ingests `sdk-llm-span` as trace `…01`.
    pub(crate) async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let config_path = dir.path().join("llmobs.toml");
        std::fs::write(
            &config_path,
            format!(
                "data_dir = {:?}\nhttp_addr = \"127.0.0.1:0\"\notlp_http_addr = \"\"\ngrpc_addr = \"\"\npublic_url = \"{ORIGIN}\"\n\n[init]\nadmin_email = \"{EMAIL}\"\nadmin_password = \"{PASSWORD}\"\norg = \"Acme\"\nproject = \"App\"\nsecret_key = \"{KEY}\"\n",
                data.display().to_string()
            ),
        )
        .unwrap();
        let config = ServerConfig::load(Some(&config_path)).unwrap();
        let running = serve::bind(config, None).await.unwrap();
        let url = format!("http://{}", running.local_addr);

        let client = reqwest::Client::new();
        let login = client
            .post(format!("{url}/api/auth/login"))
            .header("origin", ORIGIN)
            .json(&json!({"email": EMAIL, "password": PASSWORD}))
            .send()
            .await
            .unwrap();
        assert!(login.status().is_success(), "{}", login.status());
        let cookie = login
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split_once(';')
            .unwrap()
            .0
            .to_owned();
        let token = create_token(&client, &url, &cookie, "cli", &["read", "write"]).await;
        let read_token = create_token(&client, &url, &cookie, "cli-read", &["read"]).await;

        let server = Self {
            url,
            token,
            read_token,
            project: "acme/app".to_owned(),
            key: KEY.to_owned(),
            running,
            dir,
        };
        server
            .ingest("sdk-llm-span", "00000000000000000000000000000001")
            .await;
        server
    }

    pub(crate) async fn stop(self) {
        let TestServer { running, .. } = self;
        running.shutdown().await.unwrap();
    }

    /// A credentials file in the temp dir holding `url → token`.
    pub(crate) fn credentials(&self) -> PathBuf {
        let path = self.dir.path().join("llmobs").join("credentials.toml");
        let mut file = credentials::CredentialsFile::default();
        file.servers.insert(
            self.url.clone(),
            credentials::SavedToken {
                token: self.token.clone(),
            },
        );
        credentials::save(&path, &file).unwrap();
        path
    }

    /// Posts `fixtures/otlp/llmobs/<fixture>/input.json` to `/v1/traces` as OTLP/JSON with
    /// `key`, after setting every span's `traceId` to `trace_id` (32 lowercase hex digits).
    pub(crate) async fn ingest(&self, fixture: &str, trace_id: &str) {
        let path = format!(
            "{}/../../fixtures/otlp/llmobs/{fixture}/input.json",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut body: Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        set_trace_id(&mut body, trace_id);
        let response = reqwest::Client::new()
            .post(format!("{}/v1/traces", self.url))
            .header("authorization", format!("Bearer {}", self.key))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "{}", response.status());
    }
}

async fn create_token(
    client: &reqwest::Client,
    url: &str,
    cookie: &str,
    name: &str,
    scopes: &[&str],
) -> String {
    let response = client
        .post(format!("{url}/api/v1/me/tokens"))
        .header("origin", ORIGIN)
        .header(reqwest::header::COOKIE, cookie)
        .json(&json!({"name": name, "scopes": scopes}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 201);
    let body: Value = response.json().await.unwrap();
    body["secret"].as_str().unwrap().to_owned()
}

/// Sets every `traceId` in the document, at any depth.
fn set_trace_id(value: &mut Value, trace_id: &str) {
    match value {
        Value::Object(map) => {
            if let Some(id) = map.get_mut("traceId") {
                *id = Value::String(trace_id.to_owned());
            }
            for child in map.values_mut() {
                set_trace_id(child, trace_id);
            }
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|item| set_trace_id(item, trace_id)),
        _ => {}
    }
}

/// One request the canned server received.
#[derive(Debug, Clone)]
pub(crate) struct Recorded {
    /// The request target: path and query, as sent.
    pub(crate) target: String,
    /// The request line and headers, as sent.
    pub(crate) head: String,
    /// The body, as UTF-8 (lossy).
    pub(crate) body: String,
}

/// A local HTTP server that answers with fixed bytes. Dropping it stops it.
pub(crate) struct Canned {
    pub(crate) url: String,
    recorded: Arc<Mutex<Vec<Recorded>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Canned {
    /// Answers one connection per entry of `responses`, in order, writing each entry's bytes
    /// verbatim. Connections past the last entry are dropped without an answer.
    pub(crate) async fn raw(responses: Vec<Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&recorded);
        let task = tokio::spawn(async move {
            let mut responses = responses.into_iter();
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let Some(response) = responses.next() else {
                    continue;
                };
                let request = read_request(&mut socket).await;
                log.lock().unwrap().push(request);
                let _ = socket.write_all(&response).await;
                let _ = socket.shutdown().await;
            }
        });
        Self {
            url,
            recorded,
            task,
        }
    }

    /// `raw` with a JSON answer of each `(status, body)`.
    pub(crate) async fn json(responses: Vec<(u16, String)>) -> Self {
        let raw: Vec<Vec<u8>> = responses
            .into_iter()
            .map(|(status, body)| {
                format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .into_bytes()
            })
            .collect();
        Self::raw(raw).await
    }

    /// The requests received so far, in order.
    pub(crate) fn recorded(&self) -> Vec<Recorded> {
        self.recorded.lock().unwrap().clone()
    }
}

impl Drop for Canned {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Reads one request: its head, and the body that its `Content-Length` announces.
async fn read_request(socket: &mut TcpStream) -> Recorded {
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(position) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        let mut chunk = [0u8; 4096];
        let read = socket.read(&mut chunk).await.unwrap_or(0);
        if read == 0 {
            break buf.len();
        }
        buf.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end.min(buf.len())]).into_owned();
    let length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    let body_start = (head_end + 4).min(buf.len());
    while buf.len() < body_start + length {
        let mut chunk = [0u8; 4096];
        let read = socket.read(&mut chunk).await.unwrap_or(0);
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
    }
    let body_end = (body_start + length).min(buf.len());
    let target = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default()
        .to_owned();
    Recorded {
        target,
        head,
        body: String::from_utf8_lossy(&buf[body_start..body_end]).into_owned(),
    }
}

/// A writer whose writes (`on_write`) or flushes fail with `BrokenPipe`, as a closed stdout does.
pub(crate) struct BrokenPipe {
    pub(crate) on_write: bool,
}

impl Write for BrokenPipe {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.on_write {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        } else {
            Ok(data.len())
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.on_write {
            Ok(())
        } else {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }
}
