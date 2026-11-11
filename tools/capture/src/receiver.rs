//! OTLP HTTP receiver for one capture run.
//!
//! Accepts trace exports and counts log and metric posts without storing them.
//! Error strings name the route, the content type, or the body size. They never
//! include the body, authorization, or span payloads.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::Response;
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use prost::Message;
use tokio::sync::Notify;
use tokio::time::{Instant, sleep_until};

/// Spec 02's gRPC message limit. The plan uses it for this HTTP receiver too.
const BODY_LIMIT: usize = 32 * 1024 * 1024;

const COMPRESSED: &str = "compressed export: set OTEL_EXPORTER_OTLP_COMPRESSION=none";

/// Collects OTLP posts for one capture.
///
/// `Debug` prints counts and the error strings, not the span payloads.
pub struct Receiver {
    inner: Mutex<Inner>,
    notify: Notify,
}

struct Inner {
    exports: Vec<ExportTraceServiceRequest>,
    other_signals: usize,
    errors: Vec<String>,
    /// Last accepted trace export. Log and metric posts do not update it.
    last_export_at: Option<Instant>,
}

/// What the receiver stored, as a snapshot. A second `finish` returns the same data.
///
/// `Debug` prints the export count, not the span payloads.
pub struct Received {
    /// Trace exports that decoded, in arrival order.
    pub exports: Vec<ExportTraceServiceRequest>,
    /// `POST`s to `/v1/logs` or `/v1/metrics` that were accepted and discarded.
    pub other_signals: usize,
    /// Refusals and unknown routes. Never a body, a header secret, or a span.
    pub errors: Vec<String>,
}

enum Signal {
    Traces,
    Logs,
    Metrics,
}

enum Kind {
    Protobuf,
    Json,
}

enum ReadFail {
    TooLarge,
    Failed,
}

impl Receiver {
    /// Empty receiver. The `Arc` is shared with the router.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                exports: Vec::new(),
                other_signals: 0,
                errors: Vec::new(),
                last_export_at: None,
            }),
            notify: Notify::new(),
        })
    }

    /// Router that accepts any path. Mount it or serve it on its own.
    pub fn router(self: &Arc<Self>) -> axum::Router {
        axum::Router::new()
            .fallback(handle)
            .with_state(Arc::clone(self))
    }

    /// Returns when `quiet` has passed since the last accepted trace export, or
    /// when `max` has passed since this call.
    ///
    /// With no trace export yet, the quiet period starts at this call. A log or
    /// metric post does not move it. The lock is not held across the sleep.
    pub async fn wait_idle(&self, quiet: Duration, max: Duration) {
        let started = Instant::now();
        let deadline = started.checked_add(max);
        loop {
            // Register before reading the clock so an export in between is not lost.
            let notified = self.notify.notified();
            let now = Instant::now();
            if deadline.is_some_and(|limit| now >= limit) {
                return;
            }
            let last = self.lock().last_export_at.unwrap_or(started);
            let quiet_at = last.checked_add(quiet);
            if quiet_at.is_some_and(|at| now >= at) {
                return;
            }
            let Some(wake) = earliest(quiet_at, deadline) else {
                return;
            };
            tokio::select! {
                () = sleep_until(wake) => {}
                () = notified => {}
            }
        }
    }

    /// Snapshot of exports, the other-signal count, and the error log.
    pub fn finish(&self) -> Received {
        let state = self.lock();
        Received {
            exports: state.exports.clone(),
            other_signals: state.other_signals,
            errors: state.errors.clone(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn fail(&self, status: StatusCode, message: String) -> Response {
        self.lock().errors.push(message);
        empty(status)
    }

    fn accept_trace(&self, content_type: Option<&HeaderValue>, bytes: &[u8]) -> Response {
        let Some(header) = content_type else {
            return self.fail(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported content type: missing".to_owned(),
            );
        };
        let Some(kind) = kind_of(header) else {
            return self.fail(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                format!(
                    "unsupported content type: {}",
                    content_type_label(Some(header))
                ),
            );
        };
        // Both error Displays can echo input (`serde_json`) or a type URL (`prost`).
        // The plan logs the content type and the size only.
        let decoded = match kind {
            Kind::Protobuf => ExportTraceServiceRequest::decode(bytes).ok(),
            Kind::Json => serde_json::from_slice(bytes).ok(),
        };
        let Some(export) = decoded else {
            return self.fail(
                StatusCode::BAD_REQUEST,
                format!(
                    "decode failed: {} ({} bytes)",
                    content_type_label(Some(header)),
                    bytes.len()
                ),
            );
        };
        {
            let mut state = self.lock();
            state.exports.push(export);
            state.last_export_at = Some(Instant::now());
        }
        self.notify.notify_waiters();
        with_body(header, encoded_response(&kind))
    }
}

impl std::fmt::Debug for Receiver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock();
        f.debug_struct("Receiver")
            .field("exports", &state.exports.len())
            .field("last_export_at", &state.last_export_at)
            .field("other_signals", &state.other_signals)
            .field("errors", &state.errors)
            .finish()
    }
}

impl std::fmt::Debug for Received {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Received")
            .field("exports", &self.exports.len())
            .field("other_signals", &self.other_signals)
            .field("errors", &self.errors)
            .finish()
    }
}

fn earliest(quiet_at: Option<Instant>, deadline: Option<Instant>) -> Option<Instant> {
    match (quiet_at, deadline) {
        (Some(quiet_at), Some(deadline)) => Some(quiet_at.min(deadline)),
        (Some(instant), None) | (None, Some(instant)) => Some(instant),
        (None, None) => None,
    }
}

fn accepted(method: &Method, path: &str) -> Option<Signal> {
    if method != Method::POST {
        return None;
    }
    if path.ends_with("/v1/traces") {
        Some(Signal::Traces)
    } else if path.ends_with("/v1/logs") {
        Some(Signal::Logs)
    } else if path.ends_with("/v1/metrics") {
        Some(Signal::Metrics)
    } else {
        None
    }
}

fn is_identity(headers: &HeaderMap) -> bool {
    for value in headers.get_all(header::CONTENT_ENCODING) {
        let Ok(text) = value.to_str() else {
            return false;
        };
        if !text.trim().eq_ignore_ascii_case("identity") {
            return false;
        }
    }
    true
}

fn media_type(value: &HeaderValue) -> Option<&str> {
    let text = value.to_str().ok()?;
    let media = text.split(';').next()?.trim();
    if media.is_empty() { None } else { Some(media) }
}

fn kind_of(value: &HeaderValue) -> Option<Kind> {
    let media = media_type(value)?;
    if media.eq_ignore_ascii_case("application/x-protobuf") {
        Some(Kind::Protobuf)
    } else if media.eq_ignore_ascii_case("application/json") {
        Some(Kind::Json)
    } else {
        None
    }
}

fn content_type_label(value: Option<&HeaderValue>) -> String {
    let Some(value) = value else {
        return "missing".to_owned();
    };
    if value.to_str().is_err() {
        return "invalid".to_owned();
    }
    media_type(value).unwrap_or("missing").to_owned()
}

fn encoded_response(kind: &Kind) -> Vec<u8> {
    match kind {
        Kind::Protobuf => ExportTraceServiceResponse::default().encode_to_vec(),
        // Proto3 JSON omits an unset `partial_success`. The generated serde would
        // emit `{"partialSuccess":null}` because the struct has no skip attribute.
        Kind::Json => b"{}".to_vec(),
    }
}

async fn handle(
    State(receiver): State<Arc<Receiver>>,
    request: axum::extract::Request,
) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let Some(signal) = accepted(&method, &path) else {
        return receiver.fail(
            StatusCode::NOT_FOUND,
            format!("llmobs-capture receiver: no route {method} {path}"),
        );
    };
    if !is_identity(request.headers()) {
        return receiver.fail(StatusCode::UNSUPPORTED_MEDIA_TYPE, COMPRESSED.to_owned());
    }
    let content_type = request.headers().get(header::CONTENT_TYPE).cloned();
    let bytes = match read_limited(request.into_body()).await {
        Ok(bytes) => bytes,
        Err(ReadFail::TooLarge) => {
            return receiver.fail(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body exceeds 32 MiB".to_owned(),
            );
        }
        Err(ReadFail::Failed) => {
            return receiver.fail(StatusCode::BAD_REQUEST, "body read failed".to_owned());
        }
    };
    match signal {
        Signal::Logs | Signal::Metrics => {
            receiver.lock().other_signals += 1;
            empty(StatusCode::OK)
        }
        Signal::Traces => receiver.accept_trace(content_type.as_ref(), &bytes),
    }
}

async fn read_limited(mut body: Body) -> Result<Vec<u8>, ReadFail> {
    use http_body::Body as HttpBody;
    let mut out = Vec::new();
    while let Some(frame) = std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await {
        let frame = frame.map_err(|_| ReadFail::Failed)?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        if out.len().saturating_add(data.len()) > BODY_LIMIT {
            return Err(ReadFail::TooLarge);
        }
        out.extend_from_slice(&data);
    }
    Ok(out)
}

fn empty(status: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

fn with_body(content_type: &HeaderValue, body: Vec<u8>) -> Response {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = StatusCode::OK;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, content_type.clone());
    response
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, header};
    use http_body_util::BodyExt;
    use opentelemetry_proto::tonic::collector::trace::v1::{
        ExportTraceServiceRequest, ExportTraceServiceResponse,
    };
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
    use prost::Message;
    use tokio::time::Instant;
    use tower::ServiceExt;

    use super::*;

    fn sample() -> ExportTraceServiceRequest {
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        trace_id: vec![1; 16],
                        span_id: vec![2; 8],
                        name: "capture".to_owned(),
                        ..Span::default()
                    }],
                    ..ScopeSpans::default()
                }],
                ..ResourceSpans::default()
            }],
        }
    }

    fn request(
        method: &str,
        uri: &str,
        content_type: Option<&str>,
        encoding: Option<&str>,
        body: Vec<u8>,
    ) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(content_type) = content_type {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }
        if let Some(encoding) = encoding {
            builder = builder.header(header::CONTENT_ENCODING, encoding);
        }
        builder.body(Body::from(body)).unwrap()
    }

    async fn send(receiver: &Arc<Receiver>, request: Request<Body>) -> Response {
        receiver.router().oneshot(request).await.unwrap()
    }

    async fn bytes(response: Response) -> Vec<u8> {
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec()
    }

    #[tokio::test]
    async fn protobuf_and_json_of_the_same_request_match() {
        let receiver = Receiver::new();
        let request_message = sample();
        let protobuf = request_message.encode_to_vec();
        let json = serde_json::to_vec(&request_message).unwrap();

        let proto = send(
            &receiver,
            request(
                "POST",
                "/v1/traces",
                Some("application/x-protobuf"),
                Some("identity"),
                protobuf,
            ),
        )
        .await;
        assert_eq!(proto.status(), 200);
        assert_eq!(
            proto.headers()[header::CONTENT_TYPE],
            "application/x-protobuf"
        );
        let proto_body = bytes(proto).await;
        assert_eq!(
            proto_body,
            ExportTraceServiceResponse::default().encode_to_vec()
        );
        assert!(proto_body.is_empty());

        let json_response = send(
            &receiver,
            Request::builder()
                .method("POST")
                .uri("/v1/traces?timeout=1")
                .header(header::CONTENT_TYPE, "application/json; charset=utf-8")
                .header(header::AUTHORIZATION, "Bearer sk-llmobs-secret")
                .body(Body::from(json))
                .unwrap(),
        )
        .await;
        assert_eq!(json_response.status(), 200);
        assert_eq!(
            json_response.headers()[header::CONTENT_TYPE],
            "application/json; charset=utf-8"
        );
        assert_eq!(bytes(json_response).await, b"{}");

        let log = receiver.finish();
        assert_eq!(log.exports.len(), 2);
        assert_eq!(log.exports[0], log.exports[1]);
        assert_eq!(log.exports[0], request_message);
        assert!(log.errors.is_empty(), "{:?}", log.errors);
        let again = receiver.finish();
        assert_eq!(again.exports, log.exports);
    }

    #[tokio::test]
    async fn langfuse_path_is_accepted() {
        let receiver = Receiver::new();
        let response = send(
            &receiver,
            request(
                "POST",
                "/api/public/otel/v1/traces",
                Some("application/x-protobuf"),
                None,
                sample().encode_to_vec(),
            ),
        )
        .await;
        assert_eq!(response.status(), 200);
        let log = receiver.finish();
        assert_eq!(log.exports, vec![sample()]);
        assert!(log.errors.is_empty());
    }

    #[tokio::test]
    async fn gzip_is_refused() {
        let receiver = Receiver::new();
        let response = send(
            &receiver,
            request(
                "POST",
                "/v1/traces",
                Some("application/x-protobuf"),
                Some("gzip"),
                b"SECRET-GZIP".to_vec(),
            ),
        )
        .await;
        assert_eq!(response.status(), 415);
        assert!(bytes(response).await.is_empty());
        let log = receiver.finish();
        assert!(log.exports.is_empty());
        assert_eq!(log.errors, vec![COMPRESSED.to_owned()]);
        assert!(log.errors.iter().all(|error| !error.contains("SECRET")));
    }

    #[tokio::test]
    async fn garbage_is_refused() {
        let receiver = Receiver::new();
        let json = br#"{"resourceSpans":"SECRET-PAYLOAD"}"#.to_vec();
        let json_response = send(
            &receiver,
            request(
                "POST",
                "/v1/traces",
                Some("application/json"),
                None,
                json.clone(),
            ),
        )
        .await;
        assert_eq!(json_response.status(), 400);
        let proto = b"SECRET".to_vec();
        let proto_response = send(
            &receiver,
            request(
                "POST",
                "/v1/traces",
                Some("application/x-protobuf"),
                None,
                proto.clone(),
            ),
        )
        .await;
        assert_eq!(proto_response.status(), 400);
        let log = receiver.finish();
        assert!(log.exports.is_empty());
        assert_eq!(
            log.errors,
            vec![
                format!("decode failed: application/json ({} bytes)", json.len()),
                format!(
                    "decode failed: application/x-protobuf ({} bytes)",
                    proto.len()
                ),
            ]
        );
        assert!(log.errors.iter().all(|error| !error.contains("SECRET")));
    }

    #[tokio::test]
    async fn logs_and_metrics_are_counted_and_discarded() {
        let receiver = Receiver::new();
        let logs = send(
            &receiver,
            request("POST", "/v1/logs", None, None, b"not-a-log".to_vec()),
        )
        .await;
        assert_eq!(logs.status(), 200);
        assert!(bytes(logs).await.is_empty());
        let metrics = send(
            &receiver,
            request(
                "POST",
                "/api/public/otel/v1/metrics",
                Some("application/json"),
                None,
                b"{}".to_vec(),
            ),
        )
        .await;
        assert_eq!(metrics.status(), 200);
        let log = receiver.finish();
        assert_eq!(log.other_signals, 2);
        assert!(log.exports.is_empty());
        assert!(log.errors.is_empty());
    }

    #[tokio::test]
    async fn unknown_routes_and_content_types_are_logged_without_the_body() {
        let receiver = Receiver::new();
        let get = send(
            &receiver,
            request("GET", "/v1/traces", None, None, b"SECRET".to_vec()),
        )
        .await;
        assert_eq!(get.status(), 404);
        let extra = send(
            &receiver,
            request(
                "POST",
                "/v1/traces/extra",
                Some("text/plain"),
                None,
                b"SECRET".to_vec(),
            ),
        )
        .await;
        assert_eq!(extra.status(), 404);
        let plain = send(
            &receiver,
            request(
                "POST",
                "/v1/traces",
                Some("text/plain; charset=utf-8"),
                None,
                b"SECRET".to_vec(),
            ),
        )
        .await;
        assert_eq!(plain.status(), 415);
        let missing = send(
            &receiver,
            request("POST", "/v1/traces", None, None, b"SECRET".to_vec()),
        )
        .await;
        assert_eq!(missing.status(), 415);
        let log = receiver.finish();
        assert!(log.exports.is_empty());
        assert_eq!(
            log.errors,
            vec![
                "llmobs-capture receiver: no route GET /v1/traces".to_owned(),
                "llmobs-capture receiver: no route POST /v1/traces/extra".to_owned(),
                "unsupported content type: text/plain".to_owned(),
                "unsupported content type: missing".to_owned(),
            ]
        );
        assert!(log.errors.iter().all(|error| !error.contains("SECRET")));
    }

    #[tokio::test]
    async fn body_over_32_mib_is_refused() {
        let receiver = Receiver::new();
        let response = send(
            &receiver,
            request(
                "POST",
                "/v1/traces",
                Some("application/x-protobuf"),
                None,
                vec![0; BODY_LIMIT + 1],
            ),
        )
        .await;
        assert_eq!(response.status(), 413);
        let log = receiver.finish();
        assert!(log.exports.is_empty());
        assert_eq!(log.errors, vec!["body exceeds 32 MiB".to_owned()]);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_idle_returns_after_the_quiet_period() {
        let receiver = Receiver::new();
        let waiting = Arc::clone(&receiver);
        let task = tokio::spawn(async move {
            waiting
                .wait_idle(Duration::from_millis(50), Duration::from_millis(200))
                .await;
        });
        let start = Instant::now();
        // Let the wait record its start before the clock moves. `advance` jumps
        // first and only then polls the task.
        tokio::time::advance(Duration::ZERO).await;
        assert_eq!(start.elapsed(), Duration::ZERO);
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_millis(10)).await;
        let logs = send(
            &receiver,
            request("POST", "/v1/logs", None, None, Vec::new()),
        )
        .await;
        assert_eq!(logs.status(), 200);
        tokio::time::advance(Duration::from_millis(39)).await;
        assert_eq!(start.elapsed(), Duration::from_millis(49));
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(start.elapsed(), Duration::from_millis(50));
        // `advance` moves the clock and does not wait for the sleep to be polled.
        // Awaiting the task polls it. The elapsed time stays 50 ms when that poll
        // finds the quiet period already over, so a later deadline cannot hide here.
        task.await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_millis(50));
        assert_eq!(receiver.finish().other_signals, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn wait_idle_returns_at_max_while_exports_arrive() {
        let receiver = Receiver::new();
        let waiting = Arc::clone(&receiver);
        let task = tokio::spawn(async move {
            waiting
                .wait_idle(Duration::from_millis(40), Duration::from_millis(100))
                .await;
        });
        let start = Instant::now();
        tokio::time::advance(Duration::ZERO).await;
        assert_eq!(start.elapsed(), Duration::ZERO);
        assert!(!task.is_finished());

        let mut now_ms = 0;
        for at_ms in [0, 30, 60, 90] {
            if at_ms > now_ms {
                tokio::time::advance(Duration::from_millis(at_ms - now_ms)).await;
                now_ms = at_ms;
            }
            assert_eq!(start.elapsed(), Duration::from_millis(now_ms));
            assert!(!task.is_finished(), "returned at {now_ms} ms");
            let response = send(
                &receiver,
                request(
                    "POST",
                    "/v1/traces",
                    Some("application/json"),
                    None,
                    b"{}".to_vec(),
                ),
            )
            .await;
            assert_eq!(response.status(), 200);
            assert_eq!(start.elapsed(), Duration::from_millis(now_ms));
            assert!(
                !task.is_finished(),
                "export finished the wait at {now_ms} ms"
            );
        }
        tokio::time::advance(Duration::from_millis(9)).await;
        assert_eq!(start.elapsed(), Duration::from_millis(99));
        assert!(!task.is_finished());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(start.elapsed(), Duration::from_millis(100));
        task.await.unwrap();
        assert_eq!(start.elapsed(), Duration::from_millis(100));
        assert_eq!(receiver.finish().exports.len(), 4);
    }
}
