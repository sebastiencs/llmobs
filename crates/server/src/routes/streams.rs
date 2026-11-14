//! Live SSE streams (spec 09 § SSE; D-G-12, D-G-34).
//!
//! A handler validates, takes a slot, subscribes to the bus, spawns one task and returns at
//! once. The task owns the slot and the bus receiver and never holds an `AppState`.

use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Extension, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method};
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use llmobs_core::ids::{ProjectId, TraceId};
use llmobs_storage::{
    MAX_REFRESH_IDS, QueryContext, RunError, Storage, TimeWindow, TraceRefreshRequest,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until};

use crate::api::convert;
use crate::api::traces::{TraceStreamParams, TraceSummary};
use crate::auth::principal::Principal;
use crate::auth::roles::ProjectPerm;
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::live::{LiveEvent, StreamOwner, StreamSlot, StreamTiming, shut};
use crate::routes::read::{attach_trace_scores, open_query, open_query_on};

/// Events the task may have queued for one client before it waits.
const CLIENT_BUFFER: usize = 16;

/// Time one re-read of the trace stream may take (D-G-34).
pub(crate) const REFRESH_TIMEOUT: Duration = Duration::from_secs(10);

/// Traces with a search match the stream remembers before it forgets them (D-G-33).
const MAX_MATCHED: usize = 10_000;

/// The widest `start_time` span one search re-read covers, in nanoseconds (D-G-33).
const MAX_SEARCH_SPAN_NS: i64 = 86_400_000_000_000;

/// The traces waiting for their next re-read, with the least and the greatest `start_time` of
/// the spans that arrived for each.
type Pending = HashMap<TraceId, (i64, i64)>;

/// The live stream routes.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: Method::GET,
            path: "/api/v1/projects/{project}/traces/stream",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(stream_traces),
        },
        RouteDef {
            method: Method::GET,
            path: "/api/v1/projects/{project}/traces/{trace_id}/stream",
            access: Access::Project {
                perm: ProjectPerm::View,
                session_only: false,
            },
            documented: true,
            handler: get(stream_trace),
        },
    ]
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

async fn stream_traces(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<ProjectAccess>,
    error::Query(params): error::Query<TraceStreamParams>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let slot = take_slot(&state, &principal)?;
    // Subscribed before the filter is checked, so no event is missed. Both drop on an error.
    let bus = state.live.subscribe();
    let project = access.project.id;
    check_filter(&state, project, &params).await?;
    let stream = TraceStream {
        project,
        params,
        resume: has_last_event_id(&headers),
        storage: Arc::clone(&state.storage),
        bus,
        closed: state.live.closed(),
        timing: state.live.timing,
        slot,
    };
    let (tx, rx) = mpsc::channel(CLIENT_BUFFER);
    tokio::spawn(run_trace_stream(stream, tx));
    Ok(sse_response(rx))
}

/// Parses the filter (D-G-22). An invalid one is `400 invalid_filter`, so no stream opens.
async fn check_filter(
    state: &AppState,
    project: ProjectId,
    params: &TraceStreamParams,
) -> Result<(), ApiError> {
    let ctx = open_query(state, project, None).await?;
    llmobs_storage::trace_refresh(&ctx, refresh_request(params, &[], None)).await?;
    Ok(())
}

/// What the trace stream's task needs. The task holds no `AppState`.
struct TraceStream {
    project: ProjectId,
    params: TraceStreamParams,
    /// The request sent `Last-Event-ID`: the client missed events, so it is told to refetch.
    resume: bool,
    storage: Arc<Storage>,
    bus: broadcast::Receiver<Arc<LiveEvent>>,
    closed: watch::Receiver<bool>,
    timing: StreamTiming,
    /// Held for the task's life; dropping it frees the owner's slot.
    slot: StreamSlot,
}

async fn run_trace_stream(stream: TraceStream, tx: mpsc::Sender<Event>) {
    let TraceStream {
        project,
        params,
        resume,
        storage,
        mut bus,
        mut closed,
        timing,
        slot: _slot,
    } = stream;
    let search = params
        .search
        .as_deref()
        .filter(|text| !text.trim().is_empty());
    if resume && tx.send(resync()).await.is_err() {
        return;
    }
    let mut ping = interval_at(Instant::now() + timing.ping, timing.ping);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut tick = interval_at(Instant::now() + timing.tick, timing.tick);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut pending = Pending::new();
    let mut matched = HashSet::new();
    let mut last_id = 0_u64;
    loop {
        tokio::select! {
            biased;
            () = shut(&mut closed) => return,
            () = tx.closed() => return,
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
                    last_id = event.id;
                    for span in &event.spans {
                        note(&mut pending, span.trace_id, span.start_time, span.start_time);
                    }
                    if pending.len() > MAX_REFRESH_IDS {
                        pending.clear();
                        if tx.send(resync()).await.is_err() {
                            return;
                        }
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    pending.clear();
                    if tx.send(resync()).await.is_err() {
                        return;
                    }
                }
                Err(RecvError::Closed) => return,
            },
            _ = tick.tick() => {
                if pending.is_empty() {
                    continue;
                }
                let batch = std::mem::take(&mut pending);
                let through = last_id;
                let outcome = tokio::select! {
                    biased;
                    () = shut(&mut closed) => return,
                    () = tx.closed() => return,
                    outcome = refresh(&storage, project, &params, search, &batch, &mut matched) => outcome,
                };
                match outcome {
                    Ok(refreshed) => {
                        if refreshed.overflowed && tx.send(resync()).await.is_err() {
                            return;
                        }
                        for row in &refreshed.rows {
                            let Some(message) = json_event("trace", through, row) else {
                                tracing::warn!("a trace event could not be encoded");
                                continue;
                            };
                            if tx.send(message).await.is_err() {
                                return;
                            }
                        }
                    }
                    Err(RefreshError::Retry) => {
                        // Busy or timed out: the traces wait for the next tick, with no resync.
                        // `pending` was emptied by the take and the bus is not read during the
                        // refresh, so the merged map is the batch again: at most 500 ids.
                        for (id, (low, high)) in batch {
                            note(&mut pending, id, low, high);
                        }
                    }
                    Err(RefreshError::Resync) => {
                        if tx.send(resync()).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

/// Records that `trace` got a span starting between `low` and `high`.
fn note(pending: &mut Pending, trace: TraceId, low: i64, high: i64) {
    pending
        .entry(trace)
        .and_modify(|range| {
            range.0 = range.0.min(low);
            range.1 = range.1.max(high);
        })
        .or_insert((low, high));
}

/// What one re-read produced.
struct Refreshed {
    rows: Vec<TraceSummary>,
    /// The search memory was full and has been cleared, so the client must refetch (D-G-33).
    overflowed: bool,
}

/// Why a re-read failed. `Retry` keeps the traces for the next tick; `Resync` asks the client to
/// refetch.
enum RefreshError {
    Retry,
    Resync,
}

/// Re-reads the merged summaries of the traces in `batch` (D-G-13, D-G-33). `matched` holds the
/// traces a search has matched; they are re-read without the search.
async fn refresh(
    storage: &Storage,
    project: ProjectId,
    params: &TraceStreamParams,
    search: Option<&str>,
    batch: &Pending,
    matched: &mut HashSet<TraceId>,
) -> Result<Refreshed, RefreshError> {
    let ctx = open_query_on(storage, project, None)
        .await
        .map_err(|error| match error {
            ApiError::Unavailable { .. } => RefreshError::Retry,
            _ => {
                tracing::warn!("a trace stream could not open a query");
                RefreshError::Resync
            }
        })?;
    let mut overflowed = false;
    let mut rows = match search {
        None => {
            let ids: Vec<TraceId> = batch.keys().copied().collect();
            read_summaries(&ctx, params, &ids, None).await?
        }
        Some(text) => {
            let (known, fresh): (Vec<TraceId>, Vec<TraceId>) =
                batch.keys().copied().partition(|id| matched.contains(id));
            let mut rows = if known.is_empty() {
                Vec::new()
            } else {
                read_summaries(&ctx, params, &known, None).await?
            };
            if !fresh.is_empty() {
                let window = search_window(batch, &fresh).ok_or(RefreshError::Resync)?;
                let found = read_summaries(&ctx, params, &fresh, Some((text, window))).await?;
                // Only a re-read that succeeded may change what the stream remembers.
                let found_ids: Vec<TraceId> = found.iter().map(|row| row.trace_id).collect();
                if matched.len().saturating_add(found_ids.len()) > MAX_MATCHED {
                    matched.clear();
                    overflowed = true;
                }
                matched.extend(found_ids);
                rows.extend(found);
            }
            rows
        }
    };
    attach_trace_scores(&ctx, &mut rows).await.map_err(|_| {
        tracing::warn!("a trace stream could not read scores");
        RefreshError::Resync
    })?;
    Ok(Refreshed { rows, overflowed })
}

/// One `trace_refresh` of `ids`, as trace summaries.
async fn read_summaries(
    ctx: &QueryContext,
    params: &TraceStreamParams,
    ids: &[TraceId],
    search: Option<(&str, TimeWindow)>,
) -> Result<Vec<TraceSummary>, RefreshError> {
    let texts: Vec<String> = ids.iter().map(TraceId::to_string).collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    let batches = llmobs_storage::trace_refresh(ctx, refresh_request(params, &refs, search))
        .await
        .map_err(|error| {
            tracing::warn!(kind = error.kind(), "a trace stream re-read failed");
            match error {
                RunError::Timeout | RunError::TooLarge => RefreshError::Retry,
                _ => RefreshError::Resync,
            }
        })?;
    convert::trace_summaries(&batches).map_err(|_| {
        tracing::warn!("a trace stream row could not be converted");
        RefreshError::Resync
    })
}

fn refresh_request<'a>(
    params: &'a TraceStreamParams,
    trace_ids: &'a [&'a str],
    search: Option<(&'a str, TimeWindow)>,
) -> TraceRefreshRequest<'a> {
    TraceRefreshRequest {
        filter: params.filter.as_deref(),
        search,
        trace_ids,
        include_eval: params.include_eval,
        include_experiments: params.include_experiments,
        timeout: REFRESH_TIMEOUT,
    }
}

/// The `start_time` window a search re-read of `ids` covers: from the earliest start (at most a
/// day before the latest) to one nanosecond past the latest (D-G-33).
fn search_window(batch: &Pending, ids: &[TraceId]) -> Option<TimeWindow> {
    let (low, high) = ids
        .iter()
        .filter_map(|id| batch.get(id))
        .fold((i64::MAX, i64::MIN), |(low, high), &(first, last)| {
            (low.min(first), high.max(last))
        });
    let to = high.saturating_add(1);
    let from = low.max(high.saturating_sub(MAX_SEARCH_SPAN_NS));
    TimeWindow::new(from, to).ok()
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

    use crate::api::traces::{SpanSummary, TraceSummary};
    use crate::live::{StreamOwner, StreamSlot};
    use crate::meta::users;
    use crate::test_support::{Seeded, TestApp, TestStream, assert_logs_clean, dataset_span};

    const TRACE_1: &str = "a1000000000000000000000000000001";
    const TRACE_2: &str = "a1000000000000000000000000000002";
    const WAIT: Duration = Duration::from_secs(2);
    const TRACE_WAIT: Duration = Duration::from_secs(3);

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

    /// The trace id of `number`, 32 hex digits.
    fn trace_id(number: u64) -> String {
        format!("a1{number:030x}")
    }

    fn traces_stream_path(seeded: &Seeded, query: &str) -> String {
        format!(
            "/api/v1/projects/{}/traces/stream{query}",
            seeded.project.id
        )
    }

    async fn open_traces(app: &TestApp, seeded: &Seeded, query: &str) -> TestStream {
        app.request(axum::http::Method::GET, &traces_stream_path(seeded, query))
            .header("cookie", &seeded.cookie)
            .send_stream()
            .await
    }

    /// A span of `trace` named `name` in `environment`, committed to the seeded project.
    async fn commit_span(
        app: &TestApp,
        seeded: &Seeded,
        trace: &str,
        span: &str,
        name: &str,
        environment: &str,
    ) {
        let mut row = dataset_span(0);
        row.trace_id = trace.parse::<TraceId>().unwrap();
        row.span_id = span.parse::<SpanId>().unwrap();
        row.parent_span_id = None;
        row.name = name.to_owned();
        row.environment = Some(environment.to_owned());
        app.commit_spans(&seeded.project, vec![row]).await;
    }

    /// The next non-ping event, read as a `trace` summary.
    async fn next_trace(stream: &mut TestStream) -> TraceSummary {
        let event = stream
            .next_non_ping(TRACE_WAIT)
            .await
            .expect("a trace event arrives");
        assert_eq!(event.event, "trace");
        assert!(event.id.is_some());
        serde_json::from_str(&event.data).unwrap()
    }

    /// Reads events until a `resync` arrives; false when the body ends or `wait` passes first.
    async fn until_resync(stream: &mut TestStream, wait: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            match stream.next_event(remaining).await {
                Some(event) if event.event == "resync" => return true,
                Some(_) => {}
                None => return false,
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_trace_stream_sends_merged_summaries() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut stream = open_traces(&app, &seeded, "").await;
        assert_eq!(stream.status, StatusCode::OK);
        assert_eq!(stream.headers["x-accel-buffering"], "no");

        let fresh = trace_id(0xa);
        commit_span(
            &app,
            &seeded,
            &fresh,
            "b100000000000010",
            "first",
            "production",
        )
        .await;
        let first = next_trace(&mut stream).await;
        assert_eq!(first.trace_id.to_string(), fresh);
        assert_eq!(first.span_count, 1);

        commit_span(
            &app,
            &seeded,
            &fresh,
            "b100000000000011",
            "second",
            "production",
        )
        .await;
        let merged = next_trace(&mut stream).await;
        assert_eq!(merged.trace_id.to_string(), fresh);
        assert_eq!(merged.span_count, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_filter_that_matches_nothing_sends_nothing() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut stream = open_traces(&app, &seeded, "?filter=name%20%3D%20'never'").await;
        commit_span(
            &app,
            &seeded,
            &trace_id(0xb),
            "b100000000000020",
            "first",
            "production",
        )
        .await;
        assert!(
            stream
                .next_non_ping(Duration::from_millis(500))
                .await
                .is_none(),
            "a trace the filter rejects is not sent"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_invalid_filter_is_refused_before_the_stream_opens() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let response = app
            .request(
                axum::http::Method::GET,
                &traces_stream_path(&seeded, "?filter=name%20%3D"),
            )
            .header("cookie", &seeded.cookie)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::BAD_REQUEST);
        assert_eq!(response.json::<Value>()["code"], "invalid_filter");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn evaluation_traces_need_include_eval() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut default = open_traces(&app, &seeded, "").await;
        let mut including = open_traces(&app, &seeded, "?include_eval=true").await;

        let judged = trace_id(0xc);
        commit_span(
            &app,
            &seeded,
            &judged,
            "b100000000000030",
            "judge",
            "llmobs-eval",
        )
        .await;
        let shown = next_trace(&mut including).await;
        assert_eq!(shown.trace_id.to_string(), judged);
        assert!(
            default
                .next_non_ping(Duration::from_millis(500))
                .await
                .is_none(),
            "an evaluation trace is excluded by default"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_sends_a_trace_on_every_later_update() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut stream = open_traces(&app, &seeded, "?search=needle-x").await;

        let matched = trace_id(0xd);
        commit_span(
            &app,
            &seeded,
            &matched,
            "b100000000000040",
            "needle-x tool",
            "production",
        )
        .await;
        let first = next_trace(&mut stream).await;
        assert_eq!(first.trace_id.to_string(), matched);

        commit_span(
            &app,
            &seeded,
            &matched,
            "b100000000000041",
            "plain",
            "production",
        )
        .await;
        let again = next_trace(&mut stream).await;
        assert_eq!(again.trace_id.to_string(), matched);
        assert_eq!(again.span_count, 2);

        let other = trace_id(0xe);
        commit_span(
            &app,
            &seeded,
            &other,
            "b100000000000042",
            "plain",
            "production",
        )
        .await;
        assert!(
            stream
                .next_non_ping(Duration::from_millis(500))
                .await
                .is_none(),
            "a trace without a match is not sent"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_burst_of_new_traces_gets_resync() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut stream = open_traces(&app, &seeded, "").await;
        // 1 100 spans publish as events of 500, 500 and 100 (`MAX_EVENT_SPANS`): the second
        // event takes the pending map past `MAX_REFRESH_IDS`.
        let spans: Vec<_> = (1..=1100_u64)
            .map(|number| {
                let mut row = dataset_span(0);
                row.trace_id = trace_id(number).parse::<TraceId>().unwrap();
                row.span_id = format!("b2{number:014x}").parse::<SpanId>().unwrap();
                row.parent_span_id = None;
                row.environment = Some("production".to_owned());
                row
            })
            .collect();
        app.commit_spans(&seeded.project, spans).await;
        assert!(until_resync(&mut stream, Duration::from_secs(3)).await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn trace_last_event_id_gets_resync_first() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut stream = app
            .request(axum::http::Method::GET, &traces_stream_path(&seeded, ""))
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
    async fn shutdown_ends_an_open_trace_stream() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut stream = open_traces(&app, &seeded, "").await;
        app.state.live.close();
        assert!(stream.body_closed(Duration::from_secs(1)).await);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn search_text_is_never_logged() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        assert_logs_clean(&["needle-x"], async {
            let mut stream = open_traces(&app, &seeded, "?search=needle-x").await;
            commit_span(
                &app,
                &seeded,
                &trace_id(0xf),
                "b100000000000050",
                "needle-x tool",
                "production",
            )
            .await;
            let event = next_trace(&mut stream).await;
            assert_eq!(event.span_count, 1);
        })
        .await;
    }
}
