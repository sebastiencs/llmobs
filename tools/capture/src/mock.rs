//! Mock provider. One axum router serves the scripted turns in order.
//!
//! Request bodies are read only to pick the facts K5 names. They are not logged,
//! stored, or written into the error strings.

use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::{Body, Bytes, HttpBody};
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::post;
use http_body::Frame;
use serde_json::{Map, Value};
use tokio::time::Sleep;

use crate::scenario::{Api, Provider, Turn};
use crate::wire::{Ids, Rendered, openai};

/// Scripted provider. Requests take turns from the front of the queue.
#[derive(Debug)]
pub struct Mock {
    turns: Vec<Turn>,
    state: Mutex<StateInner>,
}

#[derive(Debug)]
struct StateInner {
    /// Next turn index. Advances for a served turn and for an API mismatch.
    next: usize,
    /// Take-turn attempts, including a request that finds the queue empty.
    requests: usize,
    served: Vec<Served>,
    errors: Vec<String>,
}

/// What [`Mock::finish`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockLog {
    /// Turns that were rendered, in serve order.
    pub served: Vec<Served>,
    /// Mismatches, exhausted requests, bad bodies, and turns left unserved.
    pub errors: Vec<String>,
}

/// One rendered response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    /// 0-based index into the scripted turns.
    pub turn: usize,
    /// Endpoint that was served. It matches the turn.
    pub api: Api,
    /// The response was `text/event-stream`.
    pub stream: bool,
}

impl Mock {
    /// Queues `turns` and starts at the first one.
    pub fn new(turns: Vec<Turn>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            turns,
            state: Mutex::new(StateInner {
                next: 0,
                requests: 0,
                served: Vec::new(),
                errors: Vec::new(),
            }),
        })
    }

    /// Routes the mock. The router holds its own `Arc` clone.
    pub fn router(self: &std::sync::Arc<Self>) -> axum::Router {
        let posted = post(handle).fallback(unknown);
        axum::Router::new()
            .route("/v1/chat/completions", posted.clone())
            .route("/v1/responses", posted.clone())
            .route("/v1/embeddings", posted.clone())
            .route("/v1/messages", posted.clone())
            .route("/v1beta/models/{model_action}", posted)
            .fallback(unknown)
            .with_state(std::sync::Arc::clone(self))
    }

    /// Snapshot of the log. Turns still queued are reported as `"N turns not served"`.
    pub fn finish(&self) -> MockLog {
        let mut state = self.lock();
        let remaining = self.turns.len().saturating_sub(state.next);
        if remaining > 0 {
            state.errors.push(format!("{remaining} turns not served"));
        }
        MockLog {
            served: state.served.clone(),
            errors: state.errors.clone(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, StateInner> {
        // A poisoned lock means a handler panicked. Keep the queue that was left.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn push_error(&self, message: String) {
        self.lock().errors.push(message);
    }
}

async fn handle(
    State(mock): State<std::sync::Arc<Mock>>,
    request: axum::extract::Request,
) -> Response {
    mock.respond(request).await
}

async fn unknown(
    State(mock): State<std::sync::Arc<Mock>>,
    request: axum::extract::Request,
) -> Response {
    let message = format!(
        "llmobs-capture mock: no route {} {}",
        request.method(),
        request.uri().path()
    );
    mock.push_error(message.clone());
    error_response(StatusCode::NOT_FOUND, &message)
}

impl Mock {
    async fn respond(&self, request: axum::extract::Request) -> Response {
        let label = format!("{} {}", request.method(), request.uri().path());
        let path = request.uri().path().to_owned();
        let query = request.uri().query().map(str::to_owned);
        let Some(route) = classify(&path) else {
            let message = format!("llmobs-capture mock: no route {label}");
            self.push_error(message.clone());
            return error_response(StatusCode::NOT_FOUND, &message);
        };
        let Ok(bytes) = axum::body::to_bytes(request.into_body(), usize::MAX).await else {
            self.push_error(label.clone());
            return error_response(StatusCode::BAD_REQUEST, &label);
        };
        let Ok(Value::Object(body)) = serde_json::from_slice(&bytes) else {
            self.push_error(label.clone());
            return error_response(StatusCode::BAD_REQUEST, &label);
        };
        if route.streams() && !alt_sse(query.as_deref()) {
            let message = format!("{label}: alt=sse is required");
            self.push_error(message.clone());
            return error_response(StatusCode::BAD_REQUEST, &message);
        }
        let Ok(facts) = read_facts(route, &body) else {
            self.push_error(label.clone());
            return error_response(StatusCode::BAD_REQUEST, &label);
        };
        let (index, turn) = match self.take(route) {
            Take::Turn { index, turn } => (index, turn),
            Take::Reject { status, message } => return error_response(status, &message),
        };
        self.lock().served.push(Served {
            turn: index,
            api: turn.api,
            stream: facts.stream,
        });
        let rendered = render(&turn, index, &facts);
        if !facts.stream {
            tokio::time::sleep(Duration::from_millis(turn.ttft_ms)).await;
        }
        match rendered {
            Rendered::Json(value) if !facts.stream => json_ok(&value, turn.api, index),
            Rendered::Sse(frames) if facts.stream => sse_ok(
                &frames,
                line_end(turn.api),
                Duration::from_millis(turn.ttft_ms),
                Duration::from_millis(turn.chunk_delay_ms),
                turn.api,
                index,
            ),
            Rendered::Json(_) | Rendered::Sse(_) => {
                let message = format!("turn {index}: response did not match the route");
                self.push_error(message.clone());
                error_response(StatusCode::INTERNAL_SERVER_ERROR, &message)
            }
        }
    }

    fn take(&self, route: Route) -> Take {
        let mut state = self.lock();
        state.requests += 1;
        let request_number = state.requests;
        if state.next >= self.turns.len() {
            let message = format!("request {request_number}: no scripted turn left");
            state.errors.push(message.clone());
            return Take::Reject {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                message,
            };
        }
        let index = state.next;
        state.next += 1;
        let turn = self.turns[index].clone();
        if turn.api != route.api() {
            let message = format!(
                "turn {index}: expected {}, got {}",
                api_name(turn.api),
                api_name(route.api())
            );
            state.errors.push(message.clone());
            return Take::Reject {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                message,
            };
        }
        Take::Turn { index, turn }
    }
}

enum Take {
    Turn { index: usize, turn: Turn },
    Reject { status: StatusCode, message: String },
}

#[derive(Clone, Copy)]
enum Route {
    Chat,
    Responses,
    Embeddings,
    Messages,
    Generate,
    /// `streamGenerateContent`. Streaming is the action, and it requires `alt=sse`.
    GenerateStream,
    Embed,
    EmbedBatch,
}

impl Route {
    fn api(self) -> Api {
        match self {
            Self::Chat => Api::OpenaiChat,
            Self::Responses => Api::OpenaiResponses,
            Self::Embeddings => Api::OpenaiEmbeddings,
            Self::Messages => Api::AnthropicMessages,
            Self::Generate | Self::GenerateStream => Api::GeminiGenerateContent,
            Self::Embed | Self::EmbedBatch => Api::GeminiEmbedContent,
        }
    }

    fn streams(self) -> bool {
        matches!(self, Self::GenerateStream)
    }
}

fn classify(path: &str) -> Option<Route> {
    match path {
        "/v1/chat/completions" => Some(Route::Chat),
        "/v1/responses" => Some(Route::Responses),
        "/v1/embeddings" => Some(Route::Embeddings),
        "/v1/messages" => Some(Route::Messages),
        other => {
            let action = other.strip_prefix("/v1beta/models/")?.rsplit_once(':')?.1;
            match action {
                "generateContent" => Some(Route::Generate),
                "streamGenerateContent" => Some(Route::GenerateStream),
                "embedContent" => Some(Route::Embed),
                "batchEmbedContents" => Some(Route::EmbedBatch),
                _ => None,
            }
        }
    }
}

struct Facts {
    stream: bool,
    include_usage: bool,
    inputs: usize,
    base64: bool,
    batch: bool,
}

fn read_facts(route: Route, body: &Map<String, Value>) -> Result<Facts, ()> {
    let stream_field = optional_bool(body, "stream")?;
    let include_usage = match body.get("stream_options") {
        None | Some(Value::Null) => false,
        Some(Value::Object(options)) => optional_bool(options, "include_usage")?,
        Some(_) => return Err(()),
    };
    let (inputs, base64, batch, stream) = match route {
        Route::Embeddings => (input_count(body)?, encoding_is_base64(body)?, false, false),
        Route::EmbedBatch => (array_len(body, "requests")?, false, true, false),
        Route::Embed => (1, false, false, false),
        Route::Generate => (0, false, false, false),
        Route::GenerateStream => (0, false, false, true),
        Route::Chat | Route::Responses | Route::Messages => (0, false, false, stream_field),
    };
    Ok(Facts {
        stream,
        include_usage,
        inputs,
        base64,
        batch,
    })
}

fn optional_bool(body: &Map<String, Value>, key: &str) -> Result<bool, ()> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(()),
    }
}

fn input_count(body: &Map<String, Value>) -> Result<usize, ()> {
    match body.get("input") {
        Some(Value::String(_)) => Ok(1),
        Some(Value::Array(items)) => Ok(items.len()),
        _ => Err(()),
    }
}

fn array_len(body: &Map<String, Value>, key: &str) -> Result<usize, ()> {
    match body.get(key) {
        Some(Value::Array(items)) => Ok(items.len()),
        _ => Err(()),
    }
}

fn encoding_is_base64(body: &Map<String, Value>) -> Result<bool, ()> {
    match body.get("encoding_format") {
        None | Some(Value::Null) => Ok(false),
        Some(Value::String(value)) => Ok(value == "base64"),
        Some(_) => Err(()),
    }
}

fn alt_sse(query: Option<&str>) -> bool {
    query.is_some_and(|query| {
        query.split('&').any(|pair| {
            let mut parts = pair.splitn(2, '=');
            parts.next() == Some("alt") && parts.next() == Some("sse")
        })
    })
}

fn render(turn: &Turn, index: usize, facts: &Facts) -> Rendered {
    let ids = Ids { turn: index };
    match turn.api {
        Api::OpenaiChat => openai::chat(turn, &ids, facts.stream, facts.include_usage),
        Api::OpenaiResponses => openai::responses(turn, &ids, facts.stream),
        Api::OpenaiEmbeddings => openai::embeddings(turn, &ids, facts.inputs, facts.base64),
        Api::AnthropicMessages => crate::wire::anthropic::messages(turn, &ids, facts.stream),
        Api::GeminiGenerateContent => {
            crate::wire::gemini::generate_content(turn, &ids, facts.stream)
        }
        Api::GeminiEmbedContent => {
            crate::wire::gemini::embed_content(turn, facts.inputs, facts.batch)
        }
    }
}

fn api_name(api: Api) -> &'static str {
    match api {
        Api::OpenaiChat => "openai.chat",
        Api::OpenaiResponses => "openai.responses",
        Api::OpenaiEmbeddings => "openai.embeddings",
        Api::AnthropicMessages => "anthropic.messages",
        Api::GeminiGenerateContent => "gemini.generate_content",
        Api::GeminiEmbedContent => "gemini.embed_content",
    }
}

fn line_end(api: Api) -> &'static str {
    match api.provider() {
        Provider::Gemini => "\r\n",
        Provider::OpenAi | Provider::Anthropic => "\n",
    }
}

fn json_ok(value: &Value, api: Api, index: usize) -> Response {
    let bytes = serde_json::to_vec(value).unwrap_or_else(|_| Vec::new());
    ready(
        StatusCode::OK,
        "application/json",
        None,
        Body::from(bytes),
        api,
        index,
    )
}

fn sse_ok(
    frames: &[crate::wire::SseFrame],
    line_end: &str,
    ttft: Duration,
    gap: Duration,
    api: Api,
    index: usize,
) -> Response {
    let frames = frames
        .iter()
        .map(|frame| Bytes::from(frame.encode(line_end)))
        .collect::<Vec<_>>()
        .into_iter();
    let body = Body::new(SseBody {
        frames,
        ttft,
        gap,
        sleep: None,
        first: true,
    });
    ready(
        StatusCode::OK,
        "text/event-stream",
        Some("no-cache"),
        body,
        api,
        index,
    )
}

fn ready(
    status: StatusCode,
    content_type: &'static str,
    cache_control: Option<&'static str>,
    body: Body,
    api: Api,
    index: usize,
) -> Response {
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type);
    if let Some(cache_control) = cache_control {
        response = response.header(header::CACHE_CONTROL, cache_control);
    }
    if let Some(name) = request_id_header(api) {
        let id = format!("req_capture_{}", index + 1);
        if let Ok(value) = HeaderValue::from_str(&id) {
            response = response.header(name, value);
        }
    }
    // A fixed date keeps response headers stable. The Google GenAI SDK copies
    // them into output.value, and the process clock would change every capture.
    response = response.header(header::DATE, "Thu, 01 Jan 2026 00:00:00 GMT");
    response
        .body(body)
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

fn request_id_header(api: Api) -> Option<&'static str> {
    match api.provider() {
        Provider::OpenAi => Some("x-request-id"),
        Provider::Anthropic => Some("request-id"),
        Provider::Gemini => None,
    }
}

fn error_response(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({"error": {"message": message}});
    let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| Vec::new());
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(bytes))
        .unwrap_or_else(|_| Response::new(Body::empty()))
}

struct SseBody {
    frames: std::vec::IntoIter<Bytes>,
    ttft: Duration,
    gap: Duration,
    /// `Sleep` is not `Unpin`. The box keeps this body `Unpin` so the handler can poll it.
    sleep: Option<Pin<Box<Sleep>>>,
    first: bool,
}

impl HttpBody for SseBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        loop {
            if this.sleep.is_some() {
                let ready = this
                    .sleep
                    .as_mut()
                    .map(|sleep| sleep.as_mut().poll(cx))
                    .unwrap_or(Poll::Pending);
                match ready {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(()) => this.sleep = None,
                }
                let Some(frame) = this.frames.next() else {
                    return Poll::Ready(None);
                };
                return Poll::Ready(Some(Ok(Frame::data(frame))));
            }
            if this.frames.as_slice().is_empty() {
                return Poll::Ready(None);
            }
            let delay = if this.first { this.ttft } else { this.gap };
            this.first = false;
            this.sleep = Some(Box::pin(tokio::time::sleep(delay)));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::scenario::{Finish, Message, Usage};
    use crate::wire::SseFrame;

    fn chat_turn(api: Api, ttft_ms: u64, chunk_delay_ms: u64) -> Turn {
        Turn {
            api,
            model: "mock-model".to_owned(),
            message: Some(Message {
                text: Some("Hi".to_owned()),
                reasoning: None,
                tool_calls: Vec::new(),
            }),
            finish: Some(Finish::Stop),
            usage: Usage {
                input: 8,
                output: 2,
                ..Usage::default()
            },
            dims: None,
            ttft_ms,
            chunk_delay_ms,
            chunks: 1,
        }
    }

    fn embed_turn(api: Api) -> Turn {
        Turn {
            api,
            model: "mock-embed".to_owned(),
            message: None,
            finish: None,
            usage: Usage {
                input: if api == Api::GeminiEmbedContent { 0 } else { 4 },
                ..Usage::default()
            },
            dims: Some(2),
            ttft_ms: 0,
            chunk_delay_ms: 0,
            chunks: 1,
        }
    }

    fn request(method: &str, uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    async fn send(mock: &std::sync::Arc<Mock>, method: &str, uri: &str, body: &str) -> Response {
        mock.router()
            .oneshot(request(method, uri, body))
            .await
            .unwrap()
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

    fn encoded(frames: &[SseFrame], line_end: &str) -> Vec<u8> {
        frames
            .iter()
            .flat_map(|frame| frame.encode(line_end).into_bytes())
            .collect()
    }

    #[tokio::test]
    async fn each_route_matches_the_renderer() {
        let cases = [
            (
                "/v1/chat/completions",
                r#"{"stream":false}"#,
                chat_turn(Api::OpenaiChat, 0, 0),
                false,
            ),
            (
                "/v1/chat/completions",
                r#"{"stream":true,"stream_options":{"include_usage":true}}"#,
                chat_turn(Api::OpenaiChat, 0, 0),
                true,
            ),
            (
                "/v1/responses",
                r#"{"stream":false}"#,
                chat_turn(Api::OpenaiResponses, 0, 0),
                false,
            ),
            (
                "/v1/responses",
                r#"{"stream":true}"#,
                chat_turn(Api::OpenaiResponses, 0, 0),
                true,
            ),
            (
                "/v1/embeddings",
                r#"{"input":["a","b"],"encoding_format":"base64"}"#,
                embed_turn(Api::OpenaiEmbeddings),
                false,
            ),
            (
                "/v1/embeddings",
                r#"{"input":"one"}"#,
                embed_turn(Api::OpenaiEmbeddings),
                false,
            ),
            (
                "/v1/messages",
                "{}",
                chat_turn(Api::AnthropicMessages, 0, 0),
                false,
            ),
            (
                "/v1/messages",
                r#"{"stream":true}"#,
                chat_turn(Api::AnthropicMessages, 0, 0),
                true,
            ),
            (
                "/v1beta/models/gemini-2.5-flash:generateContent",
                "{}",
                chat_turn(Api::GeminiGenerateContent, 0, 0),
                false,
            ),
            (
                "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse",
                "{}",
                chat_turn(Api::GeminiGenerateContent, 0, 0),
                true,
            ),
            (
                "/v1beta/models/gemini-embedding-001:embedContent",
                "{}",
                embed_turn(Api::GeminiEmbedContent),
                false,
            ),
            (
                "/v1beta/models/gemini-embedding-001:batchEmbedContents",
                r#"{"requests":[{},{}]}"#,
                embed_turn(Api::GeminiEmbedContent),
                false,
            ),
        ];
        for (uri, body, turn, stream) in cases {
            let mock = Mock::new(vec![turn.clone()]);
            let response = send(&mock, "POST", uri, body).await;
            assert_eq!(response.status(), 200, "{uri}");
            let headers = response.headers().clone();
            match turn.api.provider() {
                Provider::OpenAi => {
                    assert_eq!(headers["x-request-id"], "req_capture_1", "{uri}");
                    assert!(headers.get("request-id").is_none());
                }
                Provider::Anthropic => {
                    assert_eq!(headers["request-id"], "req_capture_1", "{uri}");
                    assert!(headers.get("x-request-id").is_none());
                }
                Provider::Gemini => {
                    assert!(headers.get("x-request-id").is_none());
                    assert!(headers.get("request-id").is_none());
                }
            }
            assert_eq!(
                headers[header::DATE],
                "Thu, 01 Jan 2026 00:00:00 GMT",
                "{uri}"
            );
            let rendered = expected(&turn, body, stream);
            let payload = bytes(response).await;
            match rendered {
                Rendered::Json(value) => {
                    assert_eq!(headers[header::CONTENT_TYPE], "application/json", "{uri}");
                    assert!(headers.get(header::CACHE_CONTROL).is_none(), "{uri}");
                    assert_eq!(payload, serde_json::to_vec(&value).unwrap(), "{uri}");
                }
                Rendered::Sse(frames) => {
                    assert_eq!(headers[header::CONTENT_TYPE], "text/event-stream", "{uri}");
                    assert_eq!(headers[header::CACHE_CONTROL], "no-cache", "{uri}");
                    assert_eq!(payload, encoded(&frames, line_end(turn.api)), "{uri}");
                }
            }
            let log = mock.finish();
            assert_eq!(
                log.served,
                vec![Served {
                    turn: 0,
                    api: turn.api,
                    stream
                }]
            );
            assert!(log.errors.is_empty(), "{uri}: {:?}", log.errors);
        }
    }

    fn expected(turn: &Turn, body: &str, stream: bool) -> Rendered {
        let ids = Ids { turn: 0 };
        let value: Value = serde_json::from_str(body).unwrap();
        match turn.api {
            Api::OpenaiChat => {
                let include_usage = value["stream_options"]["include_usage"] == json!(true);
                openai::chat(turn, &ids, stream, include_usage)
            }
            Api::OpenaiResponses => openai::responses(turn, &ids, stream),
            Api::OpenaiEmbeddings => {
                let inputs = match &value["input"] {
                    Value::String(_) => 1,
                    Value::Array(items) => items.len(),
                    _ => 0,
                };
                let base64 = value["encoding_format"] == json!("base64");
                openai::embeddings(turn, &ids, inputs, base64)
            }
            Api::AnthropicMessages => crate::wire::anthropic::messages(turn, &ids, stream),
            Api::GeminiGenerateContent => crate::wire::gemini::generate_content(turn, &ids, stream),
            Api::GeminiEmbedContent => {
                let batch = value.get("requests").is_some();
                let inputs = value["requests"].as_array().map(Vec::len).unwrap_or(1);
                crate::wire::gemini::embed_content(turn, inputs, batch)
            }
        }
    }

    #[tokio::test]
    async fn mismatch_consumes_the_turn_and_the_next_request_continues() {
        let mock = Mock::new(vec![
            chat_turn(Api::OpenaiChat, 0, 0),
            chat_turn(Api::OpenaiChat, 0, 0),
        ]);
        let wrong = send(&mock, "POST", "/v1/messages", "{}").await;
        assert_eq!(wrong.status(), 500);
        let wrong_body: Value = serde_json::from_slice(&bytes(wrong).await).unwrap();
        assert_eq!(
            wrong_body["error"]["message"],
            "turn 0: expected openai.chat, got anthropic.messages"
        );
        let ok = send(&mock, "POST", "/v1/chat/completions", "{}").await;
        assert_eq!(ok.status(), 200);
        assert_eq!(ok.headers()["x-request-id"], "req_capture_2");
        let log = mock.finish();
        assert_eq!(
            log.served,
            vec![Served {
                turn: 1,
                api: Api::OpenaiChat,
                stream: false
            }]
        );
        assert_eq!(
            log.errors,
            vec!["turn 0: expected openai.chat, got anthropic.messages"]
        );
    }

    #[tokio::test]
    async fn exhausted_requests_and_unserved_turns_are_logged() {
        let mock = Mock::new(vec![
            chat_turn(Api::OpenaiChat, 0, 0),
            chat_turn(Api::OpenaiChat, 0, 0),
        ]);
        let first = send(&mock, "POST", "/v1/chat/completions", "{}").await;
        assert_eq!(first.status(), 200);
        let second = send(&mock, "POST", "/v1/chat/completions", "{}").await;
        assert_eq!(second.status(), 200);
        let third = send(&mock, "POST", "/v1/responses", "{}").await;
        assert_eq!(third.status(), 500);
        let log = mock.finish();
        assert_eq!(log.served.len(), 2);
        assert_eq!(log.errors, vec!["request 3: no scripted turn left"]);
    }

    #[tokio::test]
    async fn finish_reports_turns_left_in_the_queue() {
        let mock = Mock::new(vec![
            chat_turn(Api::OpenaiChat, 0, 0),
            chat_turn(Api::OpenaiResponses, 0, 0),
        ]);
        let response = send(&mock, "POST", "/v1/chat/completions", "{}").await;
        assert_eq!(response.status(), 200);
        let log = mock.finish();
        assert_eq!(log.errors, vec!["1 turns not served"]);
    }

    #[tokio::test]
    async fn unknown_routes_are_404_and_logged() {
        let mock = Mock::new(vec![chat_turn(Api::OpenaiChat, 0, 0)]);
        let get = send(&mock, "GET", "/v1/chat/completions", "").await;
        assert_eq!(get.status(), 404);
        let get_body: Value = serde_json::from_slice(&bytes(get).await).unwrap();
        assert_eq!(
            get_body["error"]["message"],
            "llmobs-capture mock: no route GET /v1/chat/completions"
        );
        let missing = send(&mock, "POST", "/v1/other", "{}").await;
        assert_eq!(missing.status(), 404);
        let action = send(
            &mock,
            "POST",
            "/v1beta/models/gemini-2.5-flash:countTokens",
            "{}",
        )
        .await;
        assert_eq!(action.status(), 404);
        let log = mock.finish();
        assert!(log.served.is_empty());
        assert_eq!(
            log.errors,
            vec![
                "llmobs-capture mock: no route GET /v1/chat/completions",
                "llmobs-capture mock: no route POST /v1/other",
                "llmobs-capture mock: no route POST /v1beta/models/gemini-2.5-flash:countTokens",
                "1 turns not served",
            ]
        );
    }

    #[tokio::test]
    async fn bad_json_and_gemini_stream_without_sse_do_not_consume_a_turn() {
        let mock = Mock::new(vec![chat_turn(Api::GeminiGenerateContent, 0, 0)]);
        let bad = send(&mock, "POST", "/v1/chat/completions", "not-json").await;
        assert_eq!(bad.status(), 400);
        let bad_body: Value = serde_json::from_slice(&bytes(bad).await).unwrap();
        assert_eq!(bad_body["error"]["message"], "POST /v1/chat/completions");
        let stream = send(
            &mock,
            "POST",
            "/v1beta/models/gemini-2.5-flash:streamGenerateContent",
            "{}",
        )
        .await;
        assert_eq!(stream.status(), 400);
        let stream_body: Value = serde_json::from_slice(&bytes(stream).await).unwrap();
        assert_eq!(
            stream_body["error"]["message"],
            "POST /v1beta/models/gemini-2.5-flash:streamGenerateContent: alt=sse is required"
        );
        let ok = send(
            &mock,
            "POST",
            "/v1beta/models/gemini-2.5-flash:generateContent",
            "{}",
        )
        .await;
        assert_eq!(ok.status(), 200);
        let log = mock.finish();
        assert_eq!(log.served.len(), 1);
        assert_eq!(
            log.errors,
            vec![
                "POST /v1/chat/completions",
                "POST /v1beta/models/gemini-2.5-flash:streamGenerateContent: alt=sse is required",
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn delays_sit_before_the_json_body_and_between_frames() {
        let mock = Mock::new(vec![chat_turn(Api::OpenaiChat, 40, 15)]);
        let router = mock.router();
        let pending = tokio::spawn(async move {
            router
                .oneshot(request("POST", "/v1/chat/completions", "{}"))
                .await
                .unwrap()
        });
        tokio::time::advance(Duration::from_millis(39)).await;
        assert!(!pending.is_finished());
        tokio::time::advance(Duration::from_millis(1)).await;
        let response = pending.await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");

        let mock = Mock::new(vec![chat_turn(Api::OpenaiChat, 40, 15)]);
        let response = send(&mock, "POST", "/v1/chat/completions", r#"{"stream":true}"#).await;
        assert_eq!(response.status(), 200);
        let expected = match openai::chat(
            &chat_turn(Api::OpenaiChat, 40, 15),
            &Ids { turn: 0 },
            true,
            false,
        ) {
            Rendered::Sse(frames) => encoded(&frames, "\n"),
            Rendered::Json(_) => panic!("stream"),
        };
        let mut body = response.into_body();
        let mut collected = Vec::new();
        let mut first = true;
        loop {
            let (frame, next) = next_frame(body, if first { 39 } else { 14 }).await;
            body = next;
            let Some(frame) = frame else {
                break;
            };
            collected.extend_from_slice(&frame);
            first = false;
            if collected.len() == expected.len() {
                let end = tokio::spawn(async move { body.frame().await });
                tokio::time::advance(Duration::ZERO).await;
                assert!(end.is_finished(), "delay after the last frame");
                assert!(end.await.unwrap().is_none());
                break;
            }
        }
        assert_eq!(collected, expected);
    }

    async fn next_frame(mut body: Body, early_ms: u64) -> (Option<Vec<u8>>, Body) {
        let pending = tokio::spawn(async move {
            let frame = body.frame().await;
            (frame, body)
        });
        tokio::time::advance(Duration::from_millis(early_ms)).await;
        assert!(!pending.is_finished());
        tokio::time::advance(Duration::from_millis(1)).await;
        let (frame, body) = pending.await.unwrap();
        let bytes = match frame {
            Some(Ok(frame)) => frame.into_data().ok().map(|bytes| bytes.to_vec()),
            Some(Err(_)) | None => None,
        };
        (bytes, body)
    }
}
