//! OpenAI Chat Completions, Responses, and Embeddings bodies.
//!
//! Field sets follow the `openai` 3.44 response types (the newest release older
//! than 7 days on 2026-11-11). Required fields the plan's sketches omit are
//! included and named in the K3 report.

use base64::Engine;
use base64::prelude::BASE64_STANDARD;
use serde_json::{Value, json};

use crate::scenario::{Finish, ToolCall, Turn, Usage};

use super::{Ids, Rendered, SseFrame, json_string, split_text};

/// Renders a Chat Completions response.
///
/// Streaming frames have no event name. `include_usage` adds `"usage": null` on
/// every frame except a final usage frame, matching `stream_options.include_usage`.
pub fn chat(turn: &Turn, ids: &Ids, stream: bool, include_usage: bool) -> Rendered {
    if stream {
        Rendered::Sse(chat_stream(turn, ids, include_usage))
    } else {
        Rendered::Json(chat_json(turn, ids))
    }
}

/// Renders a Responses API response.
///
/// Streaming events are named (`event:` equals the data's `type`) and carry a
/// `sequence_number` counting from 0. `Finish::Length` ends as `response.incomplete`.
pub fn responses(turn: &Turn, ids: &Ids, stream: bool) -> Rendered {
    let items = output_items(turn, ids);
    if stream {
        Rendered::Sse(responses_stream(turn, ids, &items))
    } else {
        Rendered::Json(response_body(turn, ids, &items, false))
    }
}

/// Renders an Embeddings response.
///
/// Vector `i` has `dims` values `((i * 7 + j) % 11) as f32 / 10.0 - 0.5`.
/// `base64` encodes those little-endian `f32` bytes, which is what the Python
/// client requests by default and then decodes.
pub fn embeddings(turn: &Turn, ids: &Ids, inputs: usize, base64: bool) -> Rendered {
    let _ = ids;
    let dims = turn.dims.unwrap_or(0);
    let data: Vec<Value> = (0..inputs)
        .map(|index| {
            let embedding = if base64 {
                Value::String(BASE64_STANDARD.encode(vector_bytes(index, dims)))
            } else {
                Value::Array(
                    (0..dims)
                        .map(|dim| json!(embedding_value(index, dim)))
                        .collect(),
                )
            };
            json!({
                "object": "embedding",
                "index": index,
                "embedding": embedding,
            })
        })
        .collect();
    Rendered::Json(json!({
        "object": "list",
        "data": data,
        "model": turn.model,
        "usage": {
            "prompt_tokens": turn.usage.input,
            "total_tokens": turn.usage.input,
        },
    }))
}

fn chat_json(turn: &Turn, ids: &Ids) -> Value {
    let message = turn.message.as_ref();
    let mut body = json!({
        "role": "assistant",
        "content": message.and_then(|message| message.text.clone()),
        "refusal": Value::Null,
    });
    if let Some(calls) = message.map(|message| &message.tool_calls)
        && !calls.is_empty()
    {
        body["tool_calls"] = json!(function_calls(calls));
    }
    json!({
        "id": ids.chat_completion(),
        "object": "chat.completion",
        "created": Ids::CREATED,
        "model": turn.model,
        "system_fingerprint": "fp_capture",
        "choices": [{
            "index": 0,
            "message": body,
            "logprobs": Value::Null,
            "finish_reason": finish_reason(turn.finish),
        }],
        "usage": chat_usage(&turn.usage),
    })
}

fn function_calls(calls: &[ToolCall]) -> Vec<Value> {
    calls
        .iter()
        .map(|call| {
            json!({
                "id": call.id,
                "type": "function",
                "function": {
                    "name": call.name,
                    "arguments": json_string(&call.arguments),
                },
            })
        })
        .collect()
}

fn chat_stream(turn: &Turn, ids: &Ids, include_usage: bool) -> Vec<SseFrame> {
    let mut chunks = Vec::new();
    chunks.push(chat_choice(
        &json!({"role": "assistant", "content": ""}),
        None,
    ));
    if let Some(text) = turn
        .message
        .as_ref()
        .and_then(|message| message.text.as_deref())
    {
        for piece in split_text(text, turn.chunks) {
            chunks.push(chat_choice(&json!({"content": piece}), None));
        }
    }
    if let Some(calls) = turn.message.as_ref().map(|message| &message.tool_calls) {
        for (index, call) in calls.iter().enumerate() {
            chunks.push(chat_choice(
                &json!({
                    "tool_calls": [{
                        "index": index,
                        "id": call.id,
                        "type": "function",
                        "function": {"name": call.name, "arguments": ""},
                    }],
                }),
                None,
            ));
            let arguments = json_string(&call.arguments);
            for piece in split_text(&arguments, turn.chunks) {
                chunks.push(chat_choice(
                    &json!({
                        "tool_calls": [{
                            "index": index,
                            "function": {"arguments": piece},
                        }],
                    }),
                    None,
                ));
            }
        }
    }
    chunks.push(chat_choice(&json!({}), Some(finish_reason(turn.finish))));

    let usage = chat_usage(&turn.usage);
    let mut frames: Vec<SseFrame> = chunks
        .into_iter()
        .map(|choice| {
            let usage = include_usage.then_some(Value::Null);
            sse_data(&chat_chunk(turn, ids, &json!([choice]), usage.as_ref()))
        })
        .collect();
    if include_usage {
        frames.push(sse_data(&chat_chunk(turn, ids, &json!([]), Some(&usage))));
    }
    frames.push(SseFrame {
        event: None,
        data: "[DONE]".to_owned(),
    });
    frames
}

fn chat_choice(delta: &Value, finish_reason: Option<&str>) -> Value {
    json!({
        "index": 0,
        "delta": delta,
        "logprobs": Value::Null,
        "finish_reason": finish_reason,
    })
}

fn chat_chunk(turn: &Turn, ids: &Ids, choices: &Value, usage: Option<&Value>) -> Value {
    let mut chunk = json!({
        "id": ids.chat_completion(),
        "object": "chat.completion.chunk",
        "created": Ids::CREATED,
        "model": turn.model,
        "system_fingerprint": "fp_capture",
        "choices": choices,
    });
    if let Some(usage) = usage {
        chunk["usage"] = usage.clone();
    }
    chunk
}

fn chat_usage(usage: &Usage) -> Value {
    json!({
        "prompt_tokens": usage.input,
        "completion_tokens": usage.output,
        "total_tokens": usage.input.saturating_add(usage.output),
        "prompt_tokens_details": {
            "cached_tokens": usage.cache_read,
            "audio_tokens": 0,
        },
        "completion_tokens_details": {
            "reasoning_tokens": usage.reasoning,
            "audio_tokens": 0,
            "accepted_prediction_tokens": 0,
            "rejected_prediction_tokens": 0,
        },
    })
}

fn finish_reason(finish: Option<Finish>) -> &'static str {
    match finish {
        Some(Finish::ToolCalls) => "tool_calls",
        Some(Finish::Length) => "length",
        Some(Finish::Stop) | None => "stop",
    }
}

struct Item {
    kind: ItemKind,
}

enum ItemKind {
    Reasoning {
        id: String,
        text: String,
    },
    Message {
        id: String,
        text: String,
    },
    Function {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
    },
}

fn output_items(turn: &Turn, ids: &Ids) -> Vec<Item> {
    let mut items = Vec::new();
    if let Some(message) = &turn.message {
        if let Some(text) = &message.reasoning {
            items.push(Item {
                kind: ItemKind::Reasoning {
                    id: ids.reasoning(),
                    text: text.clone(),
                },
            });
        }
        if let Some(text) = &message.text {
            items.push(Item {
                kind: ItemKind::Message {
                    id: ids.message(),
                    text: text.clone(),
                },
            });
        }
        for (index, call) in message.tool_calls.iter().enumerate() {
            items.push(Item {
                kind: ItemKind::Function {
                    id: ids.function_call(index),
                    call_id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: json_string(&call.arguments),
                },
            });
        }
    }
    items
}

fn item_json(item: &Item, done: bool) -> Value {
    match &item.kind {
        ItemKind::Reasoning { id, text } => {
            let summary = if done {
                json!([{ "type": "summary_text", "text": text }])
            } else {
                json!([])
            };
            json!({"type": "reasoning", "id": id, "summary": summary})
        }
        ItemKind::Message { id, text } => {
            let content = if done {
                json!([{ "type": "output_text", "text": text, "annotations": [] }])
            } else {
                json!([])
            };
            json!({
                "type": "message",
                "id": id,
                "status": if done { "completed" } else { "in_progress" },
                "role": "assistant",
                "content": content,
            })
        }
        ItemKind::Function {
            id,
            call_id,
            name,
            arguments,
        } => json!({
            "type": "function_call",
            "id": id,
            "call_id": call_id,
            "name": name,
            "arguments": if done { arguments.as_str() } else { "" },
            "status": if done { "completed" } else { "in_progress" },
        }),
    }
}

fn response_body(turn: &Turn, ids: &Ids, items: &[Item], in_progress: bool) -> Value {
    let output: Vec<Value> = if in_progress {
        Vec::new()
    } else {
        items.iter().map(|item| item_json(item, true)).collect()
    };
    let status = if in_progress {
        "in_progress"
    } else if turn.finish == Some(Finish::Length) {
        "incomplete"
    } else {
        "completed"
    };
    let mut body = json!({
        "id": ids.response(),
        "object": "response",
        "created_at": Ids::CREATED,
        "model": turn.model,
        "status": status,
        "output": output,
        "parallel_tool_calls": true,
        "tool_choice": "auto",
        "tools": [],
    });
    if !in_progress {
        body["incomplete_details"] = if status == "incomplete" {
            json!({"reason": "max_output_tokens"})
        } else {
            Value::Null
        };
        body["usage"] = responses_usage(&turn.usage);
    }
    body
}

fn responses_usage(usage: &Usage) -> Value {
    json!({
        "input_tokens": usage.input,
        "input_tokens_details": {
            "cached_tokens": usage.cache_read,
            "cache_write_tokens": usage.cache_write,
        },
        "output_tokens": usage.output,
        "output_tokens_details": {
            "reasoning_tokens": usage.reasoning,
        },
        "total_tokens": usage.input.saturating_add(usage.output),
    })
}

fn responses_stream(turn: &Turn, ids: &Ids, items: &[Item]) -> Vec<SseFrame> {
    let mut frames = Vec::new();
    let mut sequence = 0_u64;
    for status in ["response.created", "response.in_progress"] {
        push_event(
            &mut frames,
            &mut sequence,
            status,
            json!({"response": response_body(turn, ids, items, true)}),
        );
    }
    for (output_index, item) in items.iter().enumerate() {
        push_event(
            &mut frames,
            &mut sequence,
            "response.output_item.added",
            json!({"output_index": output_index, "item": item_json(item, false)}),
        );
        stream_item(turn, &mut frames, &mut sequence, output_index, item);
        push_event(
            &mut frames,
            &mut sequence,
            "response.output_item.done",
            json!({"output_index": output_index, "item": item_json(item, true)}),
        );
    }
    let final_type = if turn.finish == Some(Finish::Length) {
        "response.incomplete"
    } else {
        "response.completed"
    };
    push_event(
        &mut frames,
        &mut sequence,
        final_type,
        json!({"response": response_body(turn, ids, items, false)}),
    );
    frames
}

fn stream_item(
    turn: &Turn,
    frames: &mut Vec<SseFrame>,
    sequence: &mut u64,
    output_index: usize,
    item: &Item,
) {
    match &item.kind {
        ItemKind::Reasoning { id, text } => {
            push_event(
                frames,
                sequence,
                "response.reasoning_summary_part.added",
                json!({
                    "item_id": id,
                    "output_index": output_index,
                    "summary_index": 0,
                    "part": {"type": "summary_text", "text": ""},
                }),
            );
            for piece in split_text(text, turn.chunks) {
                push_event(
                    frames,
                    sequence,
                    "response.reasoning_summary_text.delta",
                    json!({
                        "item_id": id,
                        "output_index": output_index,
                        "summary_index": 0,
                        "delta": piece,
                    }),
                );
            }
            push_event(
                frames,
                sequence,
                "response.reasoning_summary_text.done",
                json!({
                    "item_id": id,
                    "output_index": output_index,
                    "summary_index": 0,
                    "text": text,
                }),
            );
            push_event(
                frames,
                sequence,
                "response.reasoning_summary_part.done",
                json!({
                    "item_id": id,
                    "output_index": output_index,
                    "summary_index": 0,
                    "part": {"type": "summary_text", "text": text},
                }),
            );
        }
        ItemKind::Message { id, text } => {
            push_event(
                frames,
                sequence,
                "response.content_part.added",
                json!({
                    "content_index": 0,
                    "item_id": id,
                    "output_index": output_index,
                    "part": {"type": "output_text", "text": "", "annotations": []},
                }),
            );
            for piece in split_text(text, turn.chunks) {
                push_event(
                    frames,
                    sequence,
                    "response.output_text.delta",
                    json!({
                        "content_index": 0,
                        "item_id": id,
                        "output_index": output_index,
                        "logprobs": [],
                        "delta": piece,
                    }),
                );
            }
            push_event(
                frames,
                sequence,
                "response.output_text.done",
                json!({
                    "content_index": 0,
                    "item_id": id,
                    "output_index": output_index,
                    "logprobs": [],
                    "text": text,
                }),
            );
            push_event(
                frames,
                sequence,
                "response.content_part.done",
                json!({
                    "content_index": 0,
                    "item_id": id,
                    "output_index": output_index,
                    "part": {"type": "output_text", "text": text, "annotations": []},
                }),
            );
        }
        ItemKind::Function { id, arguments, .. } => {
            for piece in split_text(arguments, turn.chunks) {
                push_event(
                    frames,
                    sequence,
                    "response.function_call_arguments.delta",
                    json!({
                        "item_id": id,
                        "output_index": output_index,
                        "delta": piece,
                    }),
                );
            }
            push_event(
                frames,
                sequence,
                "response.function_call_arguments.done",
                json!({
                    "item_id": id,
                    "output_index": output_index,
                    "arguments": arguments,
                }),
            );
        }
    }
}

fn push_event(frames: &mut Vec<SseFrame>, sequence: &mut u64, kind: &str, mut body: Value) {
    let Some(map) = body.as_object_mut() else {
        return;
    };
    map.insert("type".to_owned(), json!(kind));
    map.insert("sequence_number".to_owned(), json!(*sequence));
    *sequence += 1;
    frames.push(SseFrame {
        event: Some(kind.to_owned()),
        data: body.to_string(),
    });
}

fn sse_data(value: &Value) -> SseFrame {
    SseFrame {
        event: None,
        data: value.to_string(),
    }
}

fn embedding_value(input: usize, dim: u32) -> f32 {
    let mixed = input.saturating_mul(7).saturating_add(dim as usize) % 11;
    mixed as f32 / 10.0 - 0.5
}

fn vector_bytes(input: usize, dims: u32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(dims as usize * 4);
    for dim in 0..dims {
        bytes.extend(embedding_value(input, dim).to_le_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{Api, Message, ToolCall, Turn, Usage};

    fn message(text: Option<&str>, calls: Vec<ToolCall>) -> Message {
        Message {
            text: text.map(str::to_owned),
            reasoning: None,
            tool_calls: calls,
        }
    }

    fn tool(id: &str, name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments,
        }
    }

    fn sample(chunks: u32, body: Option<Message>, finish: Finish) -> Turn {
        Turn {
            api: Api::OpenaiChat,
            model: "gpt-4o-mini-2024-07-18".to_owned(),
            message: body,
            finish: Some(finish),
            usage: Usage {
                input: 82,
                output: 17,
                cache_read: 4,
                cache_write: 0,
                reasoning: 3,
            },
            dims: None,
            ttft_ms: 30,
            chunk_delay_ms: 5,
            chunks,
        }
    }

    fn as_json(rendered: Rendered) -> Value {
        match rendered {
            Rendered::Json(value) => value,
            Rendered::Sse(_) => panic!("expected a JSON body"),
        }
    }

    fn as_sse(rendered: Rendered) -> Vec<SseFrame> {
        match rendered {
            Rendered::Sse(frames) => frames,
            Rendered::Json(_) => panic!("expected a stream"),
        }
    }

    fn parse_data(frame: &SseFrame) -> Value {
        serde_json::from_str(&frame.data).expect("json frame")
    }

    const IDS: Ids = Ids { turn: 0 };

    #[test]
    fn chat_completion_json() {
        let turn = sample(3, Some(message(Some("hello"), Vec::new())), Finish::Stop);
        assert_eq!(
            as_json(chat(&turn, &IDS, false, false)),
            json!({
                "id": "chatcmpl-capture-1",
                "object": "chat.completion",
                "created": 1_767_225_600,
                "model": "gpt-4o-mini-2024-07-18",
                "system_fingerprint": "fp_capture",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "hello", "refusal": null},
                    "logprobs": null,
                    "finish_reason": "stop",
                }],
                "usage": {
                    "prompt_tokens": 82,
                    "completion_tokens": 17,
                    "total_tokens": 99,
                    "prompt_tokens_details": {"cached_tokens": 4, "audio_tokens": 0},
                    "completion_tokens_details": {
                        "reasoning_tokens": 3,
                        "audio_tokens": 0,
                        "accepted_prediction_tokens": 0,
                        "rejected_prediction_tokens": 0,
                    },
                },
            })
        );
    }

    #[test]
    fn chat_tool_call_json_omits_empty_tool_calls() {
        let calls = vec![tool(
            "call_weather_1",
            "get_weather",
            json!({"city": "Paris"}),
        )];
        let turn = sample(3, Some(message(None, calls)), Finish::ToolCalls);
        let body = as_json(chat(&turn, &IDS, false, false));
        assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(body["choices"][0]["message"]["content"], Value::Null);
        assert_eq!(
            body["choices"][0]["message"]["tool_calls"],
            json!([{
                "id": "call_weather_1",
                "type": "function",
                "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"},
            }])
        );

        let plain = sample(3, Some(message(Some("hello"), Vec::new())), Finish::Stop);
        let plain = as_json(chat(&plain, &IDS, false, false));
        assert!(plain["choices"][0]["message"].get("tool_calls").is_none());
    }

    #[test]
    fn chat_stream_frames() {
        let text = sample(3, Some(message(Some("hello"), Vec::new())), Finish::Stop);
        let frames = as_sse(chat(&text, &IDS, true, false));
        assert!(frames.iter().all(|frame| frame.event.is_none()));
        assert_eq!(content_deltas(&frames), "hello");
        assert_eq!(content_pieces(&frames), vec!["", "he", "ll", "o"]);
        let finish = parse_data(&frames[frames.len() - 2]);
        assert_eq!(finish["choices"][0]["finish_reason"], "stop");
        assert!(finish.get("usage").is_none());
        assert_eq!(frames.last().expect("done").data, "[DONE]");

        let length = sample(1, Some(message(Some("hello"), Vec::new())), Finish::Length);
        let frames = as_sse(chat(&length, &IDS, true, false));
        let finish = parse_data(&frames[frames.len() - 2]);
        assert_eq!(finish["choices"][0]["finish_reason"], "length");
        assert_eq!(
            parse_data(&frames[1])["choices"][0]["delta"]["content"],
            "hello"
        );
    }

    #[test]
    fn chat_stream_tool_calls_and_usage() {
        let calls = vec![tool(
            "call_weather_1",
            "get_weather",
            json!({"city": "Paris"}),
        )];
        let only_tools = sample(2, Some(message(None, calls.clone())), Finish::ToolCalls);
        let frames = as_sse(chat(&only_tools, &IDS, true, false));
        assert_eq!(argument_deltas(&frames), "{\"city\":\"Paris\"}");
        assert_eq!(content_deltas(&frames), "");

        let both = sample(2, Some(message(Some("hi"), calls)), Finish::ToolCalls);
        let frames = as_sse(chat(&both, &IDS, true, true));
        assert_eq!(content_deltas(&frames), "hi");
        assert_eq!(argument_deltas(&frames), "{\"city\":\"Paris\"}");
        let json_frames: Vec<Value> = frames
            .iter()
            .filter(|frame| frame.data != "[DONE]")
            .map(parse_data)
            .collect();
        for frame in &json_frames[..json_frames.len() - 1] {
            assert_eq!(frame["usage"], Value::Null);
        }
        let usage = json_frames.last().expect("usage");
        assert_eq!(usage["choices"], json!([]));
        assert_eq!(usage["usage"]["prompt_tokens"], 82);
        assert_eq!(usage["usage"]["completion_tokens"], 17);
        assert_eq!(frames.last().expect("done").data, "[DONE]");

        let without = as_sse(chat(&both, &IDS, true, false));
        assert!(without.iter().all(|frame| {
            serde_json::from_str::<Value>(&frame.data)
                .map(|value| value.get("usage").is_none())
                .unwrap_or(true)
        }));
    }

    fn content_pieces(frames: &[SseFrame]) -> Vec<String> {
        frames
            .iter()
            .filter_map(|frame| serde_json::from_str::<Value>(&frame.data).ok())
            .filter_map(|value| {
                value["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(str::to_owned)
            })
            .collect()
    }

    fn content_deltas(frames: &[SseFrame]) -> String {
        content_pieces(frames).concat()
    }

    fn argument_deltas(frames: &[SseFrame]) -> String {
        frames
            .iter()
            .filter_map(|frame| serde_json::from_str::<Value>(&frame.data).ok())
            .filter_map(|value| {
                value["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"]
                    .as_str()
                    .map(str::to_owned)
            })
            .collect()
    }

    #[test]
    fn responses_json() {
        let mut turn = sample(
            2,
            Some(Message {
                text: Some("hello".to_owned()),
                reasoning: Some("think".to_owned()),
                tool_calls: vec![tool(
                    "call_weather_1",
                    "get_weather",
                    json!({"city": "Paris"}),
                )],
            }),
            Finish::Stop,
        );
        turn.api = Api::OpenaiResponses;
        assert_eq!(
            as_json(responses(&turn, &IDS, false)),
            json!({
                "id": "resp_capture_1",
                "object": "response",
                "created_at": 1_767_225_600,
                "model": "gpt-4o-mini-2024-07-18",
                "status": "completed",
                "incomplete_details": null,
                "parallel_tool_calls": true,
                "tool_choice": "auto",
                "tools": [],
                "output": [
                    {
                        "type": "reasoning",
                        "id": "rs_capture_1",
                        "summary": [{"type": "summary_text", "text": "think"}],
                    },
                    {
                        "type": "message",
                        "id": "msg_capture_1",
                        "status": "completed",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": "hello",
                            "annotations": [],
                        }],
                    },
                    {
                        "type": "function_call",
                        "id": "fc_capture_1_1",
                        "call_id": "call_weather_1",
                        "name": "get_weather",
                        "arguments": "{\"city\":\"Paris\"}",
                        "status": "completed",
                    },
                ],
                "usage": {
                    "input_tokens": 82,
                    "input_tokens_details": {"cached_tokens": 4, "cache_write_tokens": 0},
                    "output_tokens": 17,
                    "output_tokens_details": {"reasoning_tokens": 3},
                    "total_tokens": 99,
                },
            })
        );

        turn.finish = Some(Finish::Length);
        let body = as_json(responses(&turn, &IDS, false));
        assert_eq!(body["status"], "incomplete");
        assert_eq!(body["incomplete_details"]["reason"], "max_output_tokens");
    }

    #[test]
    fn responses_stream_events() {
        let mut turn = sample(
            2,
            Some(Message {
                text: Some("cd".to_owned()),
                reasoning: Some("ab".to_owned()),
                tool_calls: vec![tool("call_1", "get_weather", json!({"city": "Paris"}))],
            }),
            Finish::Stop,
        );
        turn.api = Api::OpenaiResponses;
        let frames = as_sse(responses(&turn, &IDS, true));
        let events: Vec<Value> = frames.iter().map(parse_data).collect();
        for (index, (frame, event)) in frames.iter().zip(&events).enumerate() {
            let kind = event["type"].as_str().expect("type");
            assert_eq!(frame.event.as_deref(), Some(kind));
            assert_eq!(event["sequence_number"], index as u64);
        }
        assert_eq!(events[0]["type"], "response.created");
        assert_eq!(events[1]["type"], "response.in_progress");
        assert_eq!(events[0]["response"]["status"], "in_progress");
        assert_eq!(events[0]["response"]["output"], json!([]));
        let deltas = |kind: &str| -> String {
            events
                .iter()
                .filter(|event| event["type"] == kind)
                .filter_map(|event| event["delta"].as_str())
                .collect()
        };
        assert_eq!(deltas("response.reasoning_summary_text.delta"), "ab");
        assert_eq!(deltas("response.output_text.delta"), "cd");
        assert_eq!(
            deltas("response.function_call_arguments.delta"),
            "{\"city\":\"Paris\"}"
        );
        let added = events
            .iter()
            .find(|event| event["type"] == "response.output_item.added")
            .expect("added");
        assert_eq!(added["item"]["summary"], json!([]));
        let done = events
            .iter()
            .find(|event| event["type"] == "response.output_text.delta")
            .expect("text delta");
        assert_eq!(done["logprobs"], json!([]));
        let last = events.last().expect("final");
        assert_eq!(last["type"], "response.completed");
        assert_eq!(
            last["response"]["output"].as_array().expect("output").len(),
            3
        );

        turn.finish = Some(Finish::Length);
        let frames = as_sse(responses(&turn, &IDS, true));
        let last = parse_data(frames.last().expect("final"));
        assert_eq!(last["type"], "response.incomplete");
        assert_eq!(last["response"]["status"], "incomplete");
        assert_eq!(
            last["response"]["incomplete_details"]["reason"],
            "max_output_tokens"
        );
    }

    #[test]
    fn embeddings_float_and_base64() {
        let mut turn = sample(1, None, Finish::Stop);
        turn.api = Api::OpenaiEmbeddings;
        turn.dims = Some(2);
        turn.usage.input = 6;
        let body = as_json(embeddings(&turn, &IDS, 2, false));
        assert_eq!(body["object"], "list");
        assert_eq!(body["model"], "gpt-4o-mini-2024-07-18");
        assert_eq!(
            body["usage"],
            json!({"prompt_tokens": 6, "total_tokens": 6})
        );
        assert_eq!(body["data"][0]["object"], "embedding");
        assert_eq!(body["data"][0]["index"], 0);
        assert_eq!(body["data"][1]["index"], 1);
        assert_eq!(body["data"][0]["embedding"][0], json!(-0.5));
        assert_eq!(
            body["data"][0]["embedding"][1].as_f64().expect("number") as f32,
            embedding_value(0, 1)
        );
        assert_eq!(
            body["data"][1]["embedding"][0].as_f64().expect("number") as f32,
            embedding_value(1, 0)
        );

        let encoded = as_json(embeddings(&turn, &IDS, 2, true));
        for index in 0..2 {
            let text = encoded["data"][index]["embedding"]
                .as_str()
                .expect("base64");
            let bytes = BASE64_STANDARD.decode(text).expect("decode");
            assert_eq!(bytes, vector_bytes(index, 2));
        }
    }
}
