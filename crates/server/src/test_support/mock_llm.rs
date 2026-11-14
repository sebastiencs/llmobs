//! An in-process OpenAI-compatible provider for server tests (I1.6).
//!
//! `crates/eval/tests/support/mock.rs` is the model; this one is smaller and
//! lives in the server crate so the route tests can use it.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::Value;

/// One scripted reply.
pub(crate) struct Reply {
    /// HTTP status.
    pub(crate) status: u16,
    /// JSON body, serialized as-is.
    pub(crate) body: Value,
}

/// Serves `POST /v1/chat/completions` on `127.0.0.1:0`. Replies in order; the
/// last one repeats. Records each request's headers and JSON body. Aborts its
/// task on drop.
pub(crate) struct MockLlm {
    addr: SocketAddr,
    state: Arc<MockState>,
    task: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
struct MockState {
    /// Replies still to serve. The last one repeats.
    replies: Arc<Mutex<Vec<Reply>>>,
    /// Each request's headers and parsed JSON body, in arrival order.
    requests: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
}

impl MockLlm {
    /// Binds the listener and serves it in the background.
    pub(crate) async fn start(replies: Vec<Reply>) -> Self {
        let state = Arc::new(MockState {
            replies: Arc::new(Mutex::new(replies)),
            requests: Arc::new(Mutex::new(Vec::new())),
        });
        let router = Router::new()
            .route("/v1/chat/completions", post(handle))
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the mock provider");
        let addr = listener.local_addr().expect("the mock provider's address");
        let task = tokio::spawn(async move {
            // The MockLlm aborts this task when it is dropped.
            let _ = axum::serve(listener, router).await;
        });
        Self { addr, state, task }
    }

    /// `http://127.0.0.1:<port>/v1`, a connection's `base_url`.
    pub(crate) fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    /// Requests received so far, in arrival order.
    pub(crate) fn requests(&self) -> Vec<(HeaderMap, Value)> {
        self.state
            .requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for MockLlm {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(State(state): State<Arc<MockState>>, headers: HeaderMap, body: Bytes) -> Response {
    let parsed = serde_json::from_slice(&body).unwrap_or(Value::Null);
    state
        .requests
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((headers, parsed));
    let mut replies = state.replies.lock().unwrap_or_else(PoisonError::into_inner);
    // Replies are served in order; the last one repeats.
    let reply = match replies.len() {
        0 => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                r#"{"error":{"message":"no scripted reply"}}"#,
            )
                .into_response();
        }
        1 => clone_last(&replies),
        _ => replies.remove(0),
    };
    let status = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let bytes = serde_json::to_vec(&reply.body).unwrap_or_default();
    (status, bytes).into_response()
}

/// Clones the reply that repeats.
fn clone_last(replies: &[Reply]) -> Reply {
    let reply = replies.last().expect("the caller checked for one");
    Reply {
        status: reply.status,
        body: reply.body.clone(),
    }
}

/// A 200 chat completion whose message content is `content`, with usage 10 in
/// and 3 out.
pub(crate) fn completion(content: &str) -> Reply {
    Reply {
        status: 200,
        body: serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion",
            "model": "gpt-test",
            "choices": [{
                "index": 0,
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": content}
            }],
            "usage": {"prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13}
        }),
    }
}
