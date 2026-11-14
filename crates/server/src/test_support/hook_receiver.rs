//! An HTTP endpoint for delivery tests: answers scripted statuses and records every request.

use std::collections::VecDeque;
use std::sync::{Mutex, PoisonError};

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;

/// One recorded request.
#[derive(Debug, Clone)]
pub(crate) struct Received {
    pub(crate) path: String,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Vec<u8>,
}

struct Shared {
    /// Statuses still to answer. The last one repeats; 200 when none was scripted.
    statuses: Mutex<VecDeque<u16>>,
    received: Mutex<Vec<Received>>,
}

/// Serves any path on 127.0.0.1:0. Answers the scripted statuses in order; the last one repeats
/// (200 when none was scripted). Aborts its task on drop.
pub(crate) struct HookReceiver {
    addr: std::net::SocketAddr,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<()>,
}

impl HookReceiver {
    pub(crate) async fn start(statuses: Vec<u16>) -> Self {
        let shared = Arc::new(Shared {
            statuses: Mutex::new(statuses.into()),
            received: Mutex::new(Vec::new()),
        });
        let app = Router::new()
            .fallback(answer)
            .with_state(Arc::clone(&shared));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the hook receiver");
        let addr = listener.local_addr().expect("the hook receiver's address");
        let task = tokio::spawn(async move {
            // The HookReceiver aborts this task when it is dropped.
            let _ = axum::serve(listener, app).await;
        });
        Self { addr, shared, task }
    }

    /// `http://127.0.0.1:<port><path>`.
    pub(crate) fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub(crate) fn received(&self) -> Vec<Received> {
        lock(&self.shared.received).clone()
    }
}

impl Drop for HookReceiver {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn answer(State(shared): State<Arc<Shared>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, 1 << 20)
        .await
        .map(|bytes| bytes.to_vec())
        .unwrap_or_default();
    lock(&shared.received).push(Received {
        path: parts.uri.path().to_owned(),
        headers: parts.headers,
        body,
    });
    let status = {
        let mut statuses = lock(&shared.statuses);
        if statuses.len() > 1 {
            statuses.pop_front().unwrap_or(200)
        } else {
            statuses.front().copied().unwrap_or(200)
        }
    };
    StatusCode::from_u16(status)
        .unwrap_or(StatusCode::OK)
        .into_response()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
