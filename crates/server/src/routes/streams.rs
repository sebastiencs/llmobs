//! Live SSE streams (spec 09 § SSE; D-G-12, D-G-34).
//!
//! A handler validates, takes a slot, subscribes to the bus, spawns one task and returns at
//! once. The task owns the slot and the bus receiver and never holds an `AppState`.

use std::str::FromStr;
use std::sync::Arc;

use axum::extract::{Extension, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use llmobs_core::ids::{ProjectId, TraceId};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until};

use crate::auth::principal::Principal;
use crate::auth::roles::ProjectPerm;
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::live::{LiveEvent, StreamOwner, StreamSlot, StreamTiming, shut};

/// Events the task may have queued for one client before it waits.
const CLIENT_BUFFER: usize = 16;

/// The live stream routes.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![RouteDef {
        method: Method::GET,
        path: "/api/v1/projects/{project}/traces/{trace_id}/stream",
        access: Access::Project {
            perm: ProjectPerm::View,
            session_only: false,
        },
        documented: true,
        handler: get(stream_trace),
    }]
}

#[derive(Debug, Deserialize)]
struct StreamTracePath {
    trace_id: String,
}

async fn stream_trace(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Path(path): error::Path<StreamTracePath>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let trace_id = TraceId::from_str(&path.trace_id).map_err(|_| ApiError::BadRequest {
        detail: Some("invalid trace_id".to_owned()),
        errors: Vec::new(),
    })?;
    let slot = take_slot(&state, &principal)?;
    let stream = SpanStream {
        project: access.project.id,
        trace_id,
        resume: has_last_event_id(&headers),
        bus: state.live.subscribe(),
        closed: state.live.closed(),
        timing: state.live.timing,
        slot,
    };
    let (tx, rx) = mpsc::channel(CLIENT_BUFFER);
    tokio::spawn(run_span_stream(stream, tx));
    Ok(sse_response(rx))
}

/// What the span stream's task needs. The task holds no `AppState`.
struct SpanStream {
    project: ProjectId,
    trace_id: TraceId,
    /// The request sent `Last-Event-ID`: the client missed events, so it is told to refetch.
    resume: bool,
    bus: broadcast::Receiver<Arc<LiveEvent>>,
    closed: watch::Receiver<bool>,
    timing: StreamTiming,
    /// Held for the task's life; dropping it frees the owner's slot.
    slot: StreamSlot,
}

async fn run_span_stream(stream: SpanStream, tx: mpsc::Sender<Event>) {
    let SpanStream {
        project,
        trace_id,
        resume,
        mut bus,
        mut closed,
        timing,
        slot: _slot,
    } = stream;
    if resume && tx.send(resync()).await.is_err() {
        return;
    }
    let mut ping = interval_at(Instant::now() + timing.ping, timing.ping);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut idle = Instant::now() + timing.idle;
    loop {
        tokio::select! {
            biased;
            () = shut(&mut closed) => return,
            () = tx.closed() => return,
            () = sleep_until(idle) => return,
            _ = ping.tick() => {
                if tx.send(ping_event()).await.is_err() {
                    return;
                }
            }
            received = bus.recv() => match received {
                Ok(event) => {
                    if event.project_id != project {
                        continue;
                    }
                    for span in event.spans.iter().filter(|span| span.trace_id == trace_id) {
                        let Some(message) = json_event("span", event.id, span) else {
                            tracing::warn!("a span event could not be encoded");
                            continue;
                        };
                        if tx.send(message).await.is_err() {
                            return;
                        }
                        idle = Instant::now() + timing.idle;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    if tx.send(resync()).await.is_err() {
                        return;
                    }
                }
                Err(RecvError::Closed) => return,
            },
        }
    }
}

/// Takes a stream slot for the caller, or answers `429` (D-G-34).
fn take_slot(state: &AppState, principal: &Principal) -> Result<StreamSlot, ApiError> {
    state
        .live
        .acquire(owner(principal))
        .ok_or(ApiError::RateLimited { retry_after_s: 1 })
}

fn owner(principal: &Principal) -> StreamOwner {
    match principal {
        Principal::Session { user, .. } | Principal::Pat { user, .. } => StreamOwner::User(user.id),
        Principal::ApiKey { key_id, .. } => StreamOwner::Key(*key_id),
    }
}

fn has_last_event_id(headers: &HeaderMap) -> bool {
    headers.contains_key("last-event-id")
}

/// The response body of a stream task. `X-Accel-Buffering: no` stops proxies buffering it.
fn sse_response(rx: mpsc::Receiver<Event>) -> Response {
    let body = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|event| (Ok::<_, std::convert::Infallible>(event), rx))
    });
    let headers = [(
        HeaderName::from_static("x-accel-buffering"),
        HeaderValue::from_static("no"),
    )];
    (headers, Sse::new(body)).into_response()
}

/// An event whose data is the JSON of `value`, or `None` when it cannot be encoded.
fn json_event(name: &str, id: u64, value: &impl Serialize) -> Option<Event> {
    let data = serde_json::to_string(value).ok()?;
    Some(Event::default().event(name).id(id.to_string()).data(data))
}

fn ping_event() -> Event {
    Event::default().event("ping").data("{}")
}

fn resync() -> Event {
    Event::default().event("resync").data("{}")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::http::StatusCode;
    use llmobs_core::ids::{SpanId, TraceId};
    use serde_json::Value;

    use crate::api::traces::SpanSummary;
    use crate::live::{StreamOwner, StreamSlot};
    use crate::meta::users;
    use crate::test_support::{Seeded, TestApp, dataset_span};

    const TRACE_1: &str = "a1000000000000000000000000000001";
    const TRACE_2: &str = "a1000000000000000000000000000002";
    const WAIT: Duration = Duration::from_secs(2);

    fn stream_path(seeded: &Seeded, trace: &str) -> String {
        format!(
            "/api/v1/projects/{}/traces/{trace}/stream",
            seeded.project.id
        )
    }

    /// A span of `trace` with span id `span`, committed to the seeded project.
    async fn commit_one(app: &TestApp, seeded: &Seeded, trace: &str, span: &str) {
        let mut row = dataset_span(0);
        row.trace_id = trace.parse::<TraceId>().unwrap();
        row.span_id = span.parse::<SpanId>().unwrap();
        row.parent_span_id = None;
        app.commit_spans(&seeded.project, vec![row]).await;
    }

    async fn user_id(app: &TestApp) -> uuid::Uuid {
        users::user_by_email(app.state.meta.reader(), "ada@example.com")
            .await
            .unwrap()
            .expect("seeded user exists")
            .id
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_span_stream_sends_spans_of_its_trace_only() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut stream = app
            .request(axum::http::Method::GET, &stream_path(&seeded, TRACE_1))
            .header("cookie", &seeded.cookie)
            .send_stream()
            .await;
        assert_eq!(stream.status, StatusCode::OK);
        assert!(
            stream.headers["content-type"]
                .to_str()
                .unwrap()
                .starts_with("text/event-stream")
        );
        assert_eq!(stream.headers["cache-control"], "no-cache");
        assert_eq!(stream.headers["x-accel-buffering"], "no");

        commit_one(&app, &seeded, TRACE_1, "b100000000000001").await;
        let event = stream
            .next_non_ping(WAIT)
            .await
            .expect("a span event arrives");
        assert_eq!(event.event, "span");
        assert!(event.id.is_some());
        let span: SpanSummary = serde_json::from_str(&event.data).unwrap();
        assert_eq!(span.trace_id.to_string(), TRACE_1);
        assert_eq!(span.span_id.to_string(), "b100000000000001");

        commit_one(&app, &seeded, TRACE_2, "b100000000000002").await;
        assert!(
            stream
                .next_non_ping(Duration::from_millis(300))
                .await
                .is_none(),
            "a span of another trace is not sent"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_ping_arrives_within_a_second() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut stream = app
            .request(axum::http::Method::GET, &stream_path(&seeded, TRACE_1))
            .header("cookie", &seeded.cookie)
            .send_stream()
            .await;
        let event = stream
            .next_event(Duration::from_secs(1))
            .await
            .expect("a ping arrives");
        assert_eq!(event.event, "ping");
        assert_eq!(event.data, "{}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn last_event_id_gets_resync_first() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut stream = app
            .request(axum::http::Method::GET, &stream_path(&seeded, TRACE_1))
            .header("cookie", &seeded.cookie)
            .header("last-event-id", "7")
            .send_stream()
            .await;
        let event = stream
            .next_event(Duration::from_secs(1))
            .await
            .expect("a resync arrives first");
        assert_eq!(event.event, "resync");
        assert_eq!(event.data, "{}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_idle_stream_ends() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut stream = app
            .request(axum::http::Method::GET, &stream_path(&seeded, TRACE_1))
            .header("cookie", &seeded.cookie)
            .send_stream()
            .await;
        assert!(stream.body_closed(Duration::from_secs(3)).await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_ends_an_open_stream() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut stream = app
            .request(axum::http::Method::GET, &stream_path(&seeded, TRACE_1))
            .header("cookie", &seeded.cookie)
            .send_stream()
            .await;
        app.state.live.close();
        assert!(stream.body_closed(Duration::from_secs(1)).await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn slots_are_limited_per_user() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let owner = StreamOwner::User(user_id(&app).await);
        let mut held: Vec<StreamSlot> = (0..100)
            .map(|_| app.state.live.acquire(owner).expect("a slot is free"))
            .collect();

        let refused = app
            .request(axum::http::Method::GET, &stream_path(&seeded, TRACE_1))
            .header("cookie", &seeded.cookie)
            .send()
            .await;
        assert_eq!(refused.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(refused.header("retry-after"), Some("1"));
        assert_eq!(refused.json::<Value>()["code"], "rate_limited");

        held.pop();
        let admitted = app
            .request(axum::http::Method::GET, &stream_path(&seeded, TRACE_1))
            .header("cookie", &seeded.cookie)
            .send_stream()
            .await;
        assert_eq!(admitted.status, StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn dropping_the_stream_frees_its_slot() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let owner = StreamOwner::User(user_id(&app).await);
        let stream = app
            .request(axum::http::Method::GET, &stream_path(&seeded, TRACE_1))
            .header("cookie", &seeded.cookie)
            .send_stream()
            .await;
        let mut held: Vec<StreamSlot> = (0..99)
            .map(|_| app.state.live.acquire(owner).expect("a slot is free"))
            .collect();
        assert!(app.state.live.acquire(owner).is_none());

        drop(stream);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        loop {
            if let Some(slot) = app.state.live.acquire(owner) {
                held.push(slot);
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the slot of a dropped stream is freed"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(app.state.live.acquire(owner).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn access_and_ids_follow_the_route() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let malformed = app
            .request(
                axum::http::Method::GET,
                &format!("/api/v1/projects/{}/traces/zz/stream", seeded.project.id),
            )
            .header("cookie", &seeded.cookie)
            .send()
            .await;
        assert_eq!(malformed.status, StatusCode::BAD_REQUEST);

        let read_key = app
            .request(axum::http::Method::GET, &stream_path(&seeded, TRACE_1))
            .header("authorization", &format!("Bearer {}", seeded.read_key))
            .send_stream()
            .await;
        assert_eq!(read_key.status, StatusCode::OK);

        let anonymous = app
            .request(axum::http::Method::GET, &stream_path(&seeded, TRACE_1))
            .send()
            .await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    }
}
