//! An in-process webhook receiver for evaluator tests (I2.7).
//!
//! Like [`super::mock_llm::MockLlm`]: it serves one route, answers the same way every time, and
//! records what it received.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use serde_json::Value;

/// Serves `POST /hook` on `127.0.0.1:0` and answers `status` with `body` every time.
/// Records each request's headers and raw body. Aborts its task on drop.
pub(crate) struct MockWebhook {
    addr: SocketAddr,
    state: Arc<MockState>,
    task: tokio::task::JoinHandle<()>,
}

struct MockState {
    status: u16,
    body: Value,
    /// Each request's headers and raw body, in arrival order.
    requests: Mutex<Vec<(HeaderMap, Bytes)>>,
}

impl MockWebhook {
    /// Binds the listener and serves it in the background.
    pub(crate) async fn start(status: u16, body: Value) -> Self {
        let state = Arc::new(MockState {
            status,
            body,
            requests: Mutex::new(Vec::new()),
        });
        let router = Router::new()
            .route("/hook", post(handle))
            .with_state(Arc::clone(&state));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the mock webhook");
        let addr = listener.local_addr().expect("the mock webhook's address");
        let task = tokio::spawn(async move {
            // The MockWebhook aborts this task when it is dropped.
            let _ = axum::serve(listener, router).await;
        });
        Self { addr, state, task }
    }

    /// `http://127.0.0.1:<port>/hook`, a webhook evaluator's URL.
    pub(crate) fn url(&self) -> String {
        format!("http://{}/hook", self.addr)
    }

    /// Requests received so far, in arrival order.
    pub(crate) fn requests(&self) -> Vec<(HeaderMap, Bytes)> {
        self.state
            .requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Drop for MockWebhook {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(State(state): State<Arc<MockState>>, headers: HeaderMap, body: Bytes) -> Response {
    state
        .requests
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push((headers, body));
    let status = StatusCode::from_u16(state.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(state.body.clone())).into_response()
}
