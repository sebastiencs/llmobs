//! Anthropic Messages bodies.
//!
//! Field sets follow the `anthropic` 2.4 response types (the newest release older
//! than 7 days on 2026-11-11). `anthropic` 2.8, the plan's orientation pin, requires
//! the same fields. Streaming events are named: the Python client drops a frame
//! whose `event:` line is missing.

use serde_json::{Value, json};

use crate::scenario::{Finish, ToolCall, Turn};

use super::{Ids, Rendered, SseFrame, json_string, split_text};

/// Renders a Messages response.
///
/// Content blocks are thinking, then text, then one `tool_use` per tool call.
/// `input_tokens` is `input − cache_read − cache_write`. A stream starts with
/// `output_tokens: 1` and ends with the real `output_tokens`.
pub fn messages(turn: &Turn, ids: &Ids, stream: bool) -> Rendered {
    if stream {
        Rendered::Sse(messages_stream(turn, ids))
    } else {
        let content = content_blocks(turn);
        Rendered::Json(message_body(
            turn,
            ids,
            &content,
            Some(stop_reason(turn.finish)),
            turn.usage.output,
        ))
    }
}

fn messages_stream(turn: &Turn, ids: &Ids) -> Vec<SseFrame> {
    let mut frames = vec![named(
        "message_start",
        &json!({
            "type": "message_start",
            "message": message_body(turn, ids, &[], None, 1),
        }),
    )];
    for (index, block) in blocks(turn).into_iter().enumerate() {
        frames.push(named(
            "content_block_start",
            &json!({
                "type": "content_block_start",
                "index": index,
                "content_block": block.start(),
            }),
        ));
        for delta in block.deltas(turn.chunks) {
            frames.push(named(
                "content_block_delta",
                &json!({
                    "type": "content_block_delta",
                    "index": index,
                    "delta": delta,
                }),
            ));
        }
        frames.push(named(
            "content_block_stop",
            &json!({"type": "content_block_stop", "index": index}),
        ));
    }
    frames.push(named(
        "message_delta",
        &json!({
            "type": "message_delta",
            "delta": {
                "stop_reason": stop_reason(turn.finish),
                "stop_sequence": Value::Null,
            },
            "usage": {"output_tokens": turn.usage.output},
        }),
    ));
    frames.push(named("message_stop", &json!({"type": "message_stop"})));
    frames
}

fn message_body(
    turn: &Turn,
    ids: &Ids,
    content: &[Value],
    stop: Option<&str>,
    output_tokens: u64,
) -> Value {
    json!({
        "id": ids.message(),
        "type": "message",
        "role": "assistant",
        "model": turn.model,
        "content": content,
        "stop_reason": stop,
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": turn
                .usage
                .input
                .saturating_sub(turn.usage.cache_read)
                .saturating_sub(turn.usage.cache_write),
            "output_tokens": output_tokens,
            "cache_read_input_tokens": turn.usage.cache_read,
            "cache_creation_input_tokens": turn.usage.cache_write,
        },
    })
}

fn content_blocks(turn: &Turn) -> Vec<Value> {
    blocks(turn)
        .into_iter()
        .map(|block| block.finished())
        .collect()
}

fn blocks(turn: &Turn) -> Vec<Block<'_>> {
    let Some(message) = turn.message.as_ref() else {
        return Vec::new();
    };
    let mut blocks = Vec::new();
    if let Some(reasoning) = message.reasoning.as_deref() {
        blocks.push(Block::Thinking(reasoning));
    }
    if let Some(text) = message.text.as_deref() {
        blocks.push(Block::Text(text));
    }
    blocks.extend(message.tool_calls.iter().map(Block::Tool));
    blocks
}

enum Block<'a> {
    Thinking(&'a str),
    Text(&'a str),
    Tool(&'a ToolCall),
}

impl Block<'_> {
    fn start(&self) -> Value {
        match self {
            Self::Thinking(_) => json!({"type": "thinking", "thinking": "", "signature": ""}),
            Self::Text(_) => json!({"type": "text", "text": ""}),
            Self::Tool(call) => json!({
                "type": "tool_use",
                "id": call.id,
                "name": call.name,
                "input": {},
            }),
        }
    }

    fn finished(&self) -> Value {
        match self {
            Self::Thinking(thinking) => json!({
                "type": "thinking",
                "thinking": thinking,
                "signature": "capture-signature",
            }),
            Self::Text(text) => json!({"type": "text", "text": text}),
            Self::Tool(call) => json!({
                "type": "tool_use",
                "id": call.id,
                "name": call.name,
                "input": call.arguments,
            }),
        }
    }

    fn deltas(&self, chunks: u32) -> Vec<Value> {
        match self {
            Self::Thinking(thinking) => {
                let mut deltas: Vec<Value> = split_text(thinking, chunks)
                    .into_iter()
                    .map(|piece| json!({"type": "thinking_delta", "thinking": piece}))
                    .collect();
                deltas.push(json!({"type": "signature_delta", "signature": "capture-signature"}));
                deltas
            }
            Self::Text(text) => split_text(text, chunks)
                .into_iter()
                .map(|piece| json!({"type": "text_delta", "text": piece}))
                .collect(),
            Self::Tool(call) => {
                let arguments = json_string(&call.arguments);
                split_text(&arguments, chunks)
                    .into_iter()
                    .map(|piece| json!({"type": "input_json_delta", "partial_json": piece}))
                    .collect()
            }
        }
    }
}

fn stop_reason(finish: Option<Finish>) -> &'static str {
    match finish {
        Some(Finish::ToolCalls) => "tool_use",
        Some(Finish::Length) => "max_tokens",
        Some(Finish::Stop) | None => "end_turn",
    }
}

fn named(event: &str, body: &Value) -> SseFrame {
    SseFrame {
        event: Some(event.to_owned()),
        data: body.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{Api, Message, ToolCall, Usage};

    const IDS: Ids = Ids { turn: 0 };

    fn turn(message: Message, finish: Finish, usage: Usage) -> Turn {
        Turn {
            api: Api::AnthropicMessages,
            model: "claude-sonnet-4-5".to_owned(),
            message: Some(message),
            finish: Some(finish),
            usage,
            dims: None,
            ttft_ms: 30,
            chunk_delay_ms: 5,
            chunks: 3,
        }
    }

    fn text(text: &str) -> Message {
        Message {
            text: Some(text.to_owned()),
            reasoning: None,
            tool_calls: Vec::new(),
        }
    }

    fn cached() -> Usage {
        Usage {
            input: 2100,
            output: 40,
            cache_read: 1800,
            cache_write: 200,
            reasoning: 0,
        }
    }

    fn as_json(rendered: Rendered) -> Value {
        match rendered {
            Rendered::Json(value) => value,
            Rendered::Sse(_) => panic!("expected JSON"),
        }
    }

    fn as_sse(rendered: Rendered) -> Vec<SseFrame> {
        match rendered {
            Rendered::Sse(frames) => frames,
            Rendered::Json(_) => panic!("expected SSE"),
        }
    }

    fn parse(frame: &SseFrame) -> Value {
        serde_json::from_str(&frame.data).expect("json data")
    }

    #[test]
    fn messages_json_converts_cache_tokens() {
        let body = as_json(messages(
            &turn(text("Hello"), Finish::Stop, cached()),
            &IDS,
            false,
        ));
        assert_eq!(
            body,
            json!({
                "id": "msg_capture_1",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-5",
                "content": [{"type": "text", "text": "Hello"}],
                "stop_reason": "end_turn",
                "stop_sequence": null,
                "usage": {
                    "input_tokens": 100,
                    "output_tokens": 40,
                    "cache_read_input_tokens": 1800,
                    "cache_creation_input_tokens": 200,
                },
            })
        );
    }

    #[test]
    fn messages_json_orders_thinking_text_and_tool_use() {
        let body = as_json(messages(
            &turn(
                Message {
                    text: Some("Paris".to_owned()),
                    reasoning: Some("Look it up.".to_owned()),
                    tool_calls: vec![ToolCall {
                        id: "toolu_1".to_owned(),
                        name: "get_weather".to_owned(),
                        arguments: json!({"city": "Paris"}),
                    }],
                },
                Finish::ToolCalls,
                Usage {
                    input: 12,
                    output: 8,
                    ..Usage::default()
                },
            ),
            &IDS,
            false,
        ));
        assert_eq!(
            body["content"],
            json!([
                {"type": "thinking", "thinking": "Look it up.", "signature": "capture-signature"},
                {"type": "text", "text": "Paris"},
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "get_weather",
                    "input": {"city": "Paris"},
                },
            ])
        );
        assert_eq!(body["stop_reason"], "tool_use");
        assert_eq!(body["usage"]["input_tokens"], 12);
        assert_eq!(body["usage"]["cache_read_input_tokens"], 0);
        assert_eq!(body["usage"]["cache_creation_input_tokens"], 0);
    }

    #[test]
    fn messages_json_length_and_missing_finish() {
        let mut sample = turn(text("cut"), Finish::Length, Usage::default());
        assert_eq!(
            as_json(messages(&sample, &IDS, false))["stop_reason"],
            "max_tokens"
        );
        sample.finish = None;
        assert_eq!(
            as_json(messages(&sample, &IDS, false))["stop_reason"],
            "end_turn"
        );
    }

    #[test]
    fn messages_stream_text_deltas() {
        let mut sample = turn(text("hello"), Finish::Stop, cached());
        sample.chunks = 3;
        let frames = as_sse(messages(&sample, &IDS, true));
        let events: Vec<Value> = frames.iter().map(parse).collect();
        for (frame, event) in frames.iter().zip(&events) {
            let kind = event["type"].as_str().expect("type");
            assert_eq!(frame.event.as_deref(), Some(kind));
            assert!(frame.encode("\n").starts_with(&format!("event: {kind}\n")));
        }
        assert_eq!(events[0]["type"], "message_start");
        assert_eq!(events[0]["message"]["content"], json!([]));
        assert_eq!(events[0]["message"]["stop_reason"], Value::Null);
        assert_eq!(events[0]["message"]["usage"]["input_tokens"], 100);
        assert_eq!(events[0]["message"]["usage"]["output_tokens"], 1);
        assert_eq!(
            events[0]["message"]["usage"]["cache_read_input_tokens"],
            1800
        );
        assert_eq!(
            events[0]["message"]["usage"]["cache_creation_input_tokens"],
            200
        );

        let texts: String = events
            .iter()
            .filter(|event| event["delta"]["type"] == "text_delta")
            .filter_map(|event| event["delta"]["text"].as_str())
            .collect();
        assert_eq!(texts, "hello");
        assert_eq!(events[1]["content_block"]["text"], "");
        assert_eq!(events.last().expect("stop")["type"], "message_stop");
        let delta = events.iter().rev().nth(1).expect("message_delta");
        assert_eq!(delta["type"], "message_delta");
        assert_eq!(delta["delta"]["stop_reason"], "end_turn");
        assert_eq!(delta["delta"]["stop_sequence"], Value::Null);
        assert_eq!(delta["usage"], json!({"output_tokens": 40}));
    }

    #[test]
    fn messages_stream_thinking_then_tool_json() {
        let mut sample = turn(
            Message {
                text: None,
                reasoning: Some("ab".to_owned()),
                tool_calls: vec![ToolCall {
                    id: "toolu_1".to_owned(),
                    name: "get_weather".to_owned(),
                    arguments: json!({"city": "Paris"}),
                }],
            },
            Finish::ToolCalls,
            Usage {
                output: 9,
                ..Usage::default()
            },
        );
        sample.chunks = 2;
        let events: Vec<Value> = as_sse(messages(&sample, &IDS, true))
            .iter()
            .map(parse)
            .collect();
        assert_eq!(events[1]["content_block"]["thinking"], "");
        assert_eq!(events[1]["content_block"]["signature"], "");
        let thinking: String = events
            .iter()
            .filter(|event| event["delta"]["type"] == "thinking_delta")
            .filter_map(|event| event["delta"]["thinking"].as_str())
            .collect();
        assert_eq!(thinking, "ab");
        let signatures: Vec<&str> = events
            .iter()
            .filter(|event| event["delta"]["type"] == "signature_delta")
            .filter_map(|event| event["delta"]["signature"].as_str())
            .collect();
        assert_eq!(signatures, vec!["capture-signature"]);

        let tool_start = events
            .iter()
            .find(|event| event["content_block"]["type"] == "tool_use")
            .expect("tool start");
        assert_eq!(tool_start["index"], 1);
        assert_eq!(tool_start["content_block"]["input"], json!({}));
        let partial: String = events
            .iter()
            .filter(|event| event["index"] == 1 && event["delta"]["type"] == "input_json_delta")
            .filter_map(|event| event["delta"]["partial_json"].as_str())
            .collect();
        assert_eq!(partial, "{\"city\":\"Paris\"}");
        let delta = events.iter().rev().nth(1).expect("message_delta");
        assert_eq!(delta["delta"]["stop_reason"], "tool_use");
        assert_eq!(delta["usage"]["output_tokens"], 9);
    }
}
