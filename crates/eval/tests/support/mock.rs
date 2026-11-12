//! In-process provider and webhook. Scripted replies are served in order on every route.
//!
//! This file is an integration-test module, so clippy's test allow does not apply.
#![allow(clippy::expect_used)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use reqwest::Url;
use serde_json::Value;

/// One scripted response.
pub(crate) struct MockReply {
    /// HTTP status.
    pub(crate) status: u16,
    /// JSON body. Serialized as-is.
    pub(crate) body: Value,
    /// Wait before responding.
    pub(crate) delay: Duration,
}

/// A request the mock received.
#[derive(Clone)]
pub(crate) struct RecordedRequest {
    /// Path only, without the query.
    pub(crate) path: String,
    /// Query string, without `?`. `None` when the request had none.
    ///
    /// Read by the Gemini tests. The other adapter crates leave it unread.
    #[allow(dead_code)]
    pub(crate) query: Option<String>,
    /// Request headers.
    ///
    /// Read by the adapter tests. The mode tests leave the field unread.
    #[allow(dead_code)]
    pub(crate) headers: HeaderMap,
    /// Parsed JSON body, or `Null` when the body was not JSON.
    pub(crate) body: Value,
}

impl RecordedRequest {
    /// The header value, when it is valid UTF-8.
    ///
    /// Read by the adapter tests. The mode tests leave it unread.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

/// An axum server bound to `127.0.0.1:0`.
pub(crate) struct Mock {
    port: u16,
    queue: Arc<Mutex<VecDeque<Scripted>>>,
    seen: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl Mock {
    /// Binds a listener and serves it in the background.
    pub(crate) async fn start() -> Self {
        let queue = Arc::new(Mutex::new(VecDeque::new()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let state = App {
            queue: Arc::clone(&queue),
            seen: Arc::clone(&seen),
        };
        let router = Router::new()
            .route("/v1/chat/completions", post(handle))
            .route("/v1/messages", post(handle))
            .route("/v1beta/models/{*rest}", post(handle))
            .route("/webhook", post(handle))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock");
        let port = listener.local_addr().expect("mock port").port();
        tokio::spawn(async move {
            axum::serve(listener, router).await.expect("serve mock");
        });
        Self { port, queue, seen }
    }

    /// Base URL, including `/v1`, for an OpenAI-compatible connection.
    ///
    /// Each adapter test crate uses one of `url` and `origin`.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn url(&self) -> Url {
        Url::parse(&format!("http://127.0.0.1:{}/v1", self.port)).expect("mock url")
    }

    /// Origin without a version path, for a connection whose base is the host.
    ///
    /// Each adapter test crate uses one of `url` and `origin`.
    #[allow(dead_code)]
    #[must_use]
    pub(crate) fn origin(&self) -> Url {
        Url::parse(&format!("http://127.0.0.1:{}", self.port)).expect("mock origin")
    }

    /// Appends one scripted reply.
    pub(crate) fn enqueue(&self, reply: MockReply) {
        self.queue
            .lock()
            .expect("queue")
            .push_back(Scripted::Json(reply));
    }

    /// Appends a raw body. `location`, when set, is the `Location` header.
    ///
    /// The webhook tests send non-JSON bodies and a redirect. Adapter tests leave it unread.
    #[allow(dead_code)]
    pub(crate) fn enqueue_raw(&self, status: u16, body: Vec<u8>, location: Option<&'static str>) {
        self.queue.lock().expect("queue").push_back(Scripted::Raw {
            status,
            body,
            location,
        });
    }

    /// Requests received so far, in arrival order.
    #[must_use]
    pub(crate) fn requests(&self) -> Vec<RecordedRequest> {
        self.seen.lock().expect("seen").clone()
    }
}

enum Scripted {
    Json(MockReply),
    /// Non-JSON body, or a redirect with a `Location` header.
    ///
    /// Constructed by [`Mock::enqueue_raw`]. Adapter tests never build it.
    #[allow(dead_code)]
    Raw {
        status: u16,
        body: Vec<u8>,
        location: Option<&'static str>,
    },
}

#[derive(Clone)]
struct App {
    queue: Arc<Mutex<VecDeque<Scripted>>>,
    seen: Arc<Mutex<Vec<RecordedRequest>>>,
}

async fn handle(State(app): State<App>, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let parsed = serde_json::from_slice(&body).unwrap_or(Value::Null);
    app.seen.lock().expect("seen").push(RecordedRequest {
        path: uri.path().to_owned(),
        query: uri.query().map(str::to_owned),
        headers,
        body: parsed,
    });
    let reply = app.queue.lock().expect("queue").pop_front();
    let Some(reply) = reply else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"error":{"type":"mock","code":"empty","message":"no scripted reply"}}"#,
        )
            .into_response();
    };
    let (status, bytes, location) = match reply {
        Scripted::Json(reply) => {
            if !reply.delay.is_zero() {
                tokio::time::sleep(reply.delay).await;
            }
            let status =
                StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let bytes = serde_json::to_vec(&reply.body).unwrap_or_default();
            (status, bytes, None)
        }
        Scripted::Raw {
            status,
            body,
            location,
        } => {
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (status, body, location)
        }
    };
    let mut response = (status, bytes).into_response();
    if let Some(location) = location {
        response.headers_mut().insert(
            axum::http::header::LOCATION,
            axum::http::HeaderValue::from_static(location),
        );
    }
    response
}
