//! Gemini `generateContent` and `embedContent` bodies.
//!
//! Field sets follow the `google-genai` 2.34 response types (the newest release
//! older than 7 days on 2026-11-11). `google-genai` 3.1, the plan's orientation pin,
//! adds no required field. JSON keys are the camelCase wire names. Streaming frames
//! have no event name; the server encodes them with `"\r\n"`.

use serde_json::{Map, Value, json};

use crate::scenario::{Finish, ToolCall, Turn, Usage};

use super::{Ids, Rendered, SseFrame, split_text};

/// Renders a `generateContent` response.
///
/// Parts are reasoning (`thought: true`), then text, then one `functionCall` per
/// tool call. `ToolCalls` finishes as `STOP`. A stream sends one part per text
/// piece with prompt-only usage, then a last frame that carries every function
/// call, `finishReason`, and the full `usageMetadata`.
pub fn generate_content(turn: &Turn, ids: &Ids, stream: bool) -> Rendered {
    if stream {
        Rendered::Sse(generate_stream(turn, ids))
    } else {
        let parts = parts(turn);
        let candidates = vec![candidate(&parts, Some(finish_reason(turn.finish)))];
        let usage = usage_metadata(&turn.usage, true);
        Rendered::Json(response(turn, ids, &candidates, &usage))
    }
}

/// Renders an `embedContent` or `batchEmbedContents` response.
///
/// Vector `i` has `dims` values `((i * 7 + j) % 11) as f32 / 10.0 - 0.5`, the same
/// formula as OpenAI embeddings. `batch` selects `embeddings`; otherwise the body
/// is the single `embedding` for input 0.
pub fn embed_content(turn: &Turn, inputs: usize, batch: bool) -> Rendered {
    let dims = turn.dims.unwrap_or(0);
    let body = if batch {
        let embeddings: Vec<Value> = (0..inputs)
            .map(|index| json!({"values": vector(index, dims)}))
            .collect();
        json!({"embeddings": embeddings})
    } else {
        json!({"embedding": {"values": vector(0, dims)}})
    };
    Rendered::Json(body)
}

fn generate_stream(turn: &Turn, ids: &Ids) -> Vec<SseFrame> {
    let mut frames = Vec::new();
    if let Some(message) = turn.message.as_ref() {
        if let Some(reasoning) = message.reasoning.as_deref() {
            for piece in split_text(reasoning, turn.chunks) {
                frames.push(piece_frame(turn, ids, piece, true));
            }
        }
        if let Some(text) = message.text.as_deref() {
            for piece in split_text(text, turn.chunks) {
                frames.push(piece_frame(turn, ids, piece, false));
            }
        }
    }
    let calls = turn
        .message
        .as_ref()
        .map(|message| message.tool_calls.as_slice())
        .unwrap_or(&[]);
    let call_parts: Vec<Value> = calls.iter().map(function_call).collect();
    let candidates = vec![candidate(&call_parts, Some(finish_reason(turn.finish)))];
    let usage = usage_metadata(&turn.usage, true);
    frames.push(data(&response(turn, ids, &candidates, &usage)));
    frames
}

fn piece_frame(turn: &Turn, ids: &Ids, text: &str, thought: bool) -> SseFrame {
    let part = if thought {
        json!({"text": text, "thought": true})
    } else {
        json!({"text": text})
    };
    let parts = vec![part];
    let candidates = vec![candidate(&parts, None)];
    let usage = usage_metadata(&turn.usage, false);
    data(&response(turn, ids, &candidates, &usage))
}

fn response(turn: &Turn, ids: &Ids, candidates: &[Value], usage: &Value) -> Value {
    json!({
        "candidates": candidates,
        "usageMetadata": usage,
        "modelVersion": turn.model,
        "responseId": format!("capture-{}", ids.turn + 1),
    })
}

fn candidate(parts: &[Value], finish: Option<&str>) -> Value {
    let mut candidate = json!({
        "content": {"role": "model", "parts": parts},
        "index": 0,
    });
    if let Some(finish) = finish {
        candidate["finishReason"] = json!(finish);
    }
    candidate
}

fn parts(turn: &Turn) -> Vec<Value> {
    let Some(message) = turn.message.as_ref() else {
        return Vec::new();
    };
    let mut parts = Vec::new();
    if let Some(reasoning) = message.reasoning.as_deref() {
        parts.push(json!({"text": reasoning, "thought": true}));
    }
    if let Some(text) = message.text.as_deref() {
        parts.push(json!({"text": text}));
    }
    parts.extend(message.tool_calls.iter().map(function_call));
    parts
}

fn function_call(call: &ToolCall) -> Value {
    json!({
        "functionCall": {
            "id": call.id,
            "name": call.name,
            "args": call.arguments,
        },
    })
}

fn usage_metadata(usage: &Usage, full: bool) -> Value {
    if !full {
        return json!({
            "promptTokenCount": usage.input,
            "totalTokenCount": usage.input,
        });
    }
    let mut metadata = Map::new();
    metadata.insert("promptTokenCount".to_owned(), json!(usage.input));
    if usage.cache_read != 0 {
        metadata.insert(
            "cachedContentTokenCount".to_owned(),
            json!(usage.cache_read),
        );
    }
    metadata.insert(
        "candidatesTokenCount".to_owned(),
        json!(usage.output.saturating_sub(usage.reasoning)),
    );
    if usage.reasoning != 0 {
        metadata.insert("thoughtsTokenCount".to_owned(), json!(usage.reasoning));
    }
    metadata.insert(
        "totalTokenCount".to_owned(),
        json!(usage.input.saturating_add(usage.output)),
    );
    Value::Object(metadata)
}

fn finish_reason(finish: Option<Finish>) -> &'static str {
    match finish {
        Some(Finish::Length) => "MAX_TOKENS",
        Some(Finish::Stop) | Some(Finish::ToolCalls) | None => "STOP",
    }
}

fn data(value: &Value) -> SseFrame {
    SseFrame {
        event: None,
        data: value.to_string(),
    }
}

fn embedding_value(input: usize, dim: u32) -> f32 {
    let mixed = input.saturating_mul(7).saturating_add(dim as usize) % 11;
    mixed as f32 / 10.0 - 0.5
}

fn vector(input: usize, dims: u32) -> Vec<Value> {
    (0..dims)
        .map(|dim| json!(embedding_value(input, dim)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{Api, Message, ToolCall};

    const IDS: Ids = Ids { turn: 0 };

    fn turn(message: Option<Message>, finish: Option<Finish>, usage: Usage) -> Turn {
        Turn {
            api: Api::GeminiGenerateContent,
            model: "gemini-2.5-flash".to_owned(),
            message,
            finish,
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
    fn generate_content_json_splits_usage() {
        let body = as_json(generate_content(
            &turn(
                Some(Message {
                    text: Some("Paris".to_owned()),
                    reasoning: Some("Look it up.".to_owned()),
                    tool_calls: vec![ToolCall {
                        id: "get_weather_1".to_owned(),
                        name: "get_weather".to_owned(),
                        arguments: json!({"city": "Paris"}),
                    }],
                }),
                Some(Finish::ToolCalls),
                Usage {
                    input: 2100,
                    output: 40,
                    cache_read: 1800,
                    cache_write: 0,
                    reasoning: 12,
                },
            ),
            &IDS,
            false,
        ));
        assert_eq!(body["modelVersion"], "gemini-2.5-flash");
        assert_eq!(body["responseId"], "capture-1");
        assert_eq!(
            body["candidates"][0],
            json!({
                "content": {
                    "role": "model",
                    "parts": [
                        {"text": "Look it up.", "thought": true},
                        {"text": "Paris"},
                        {"functionCall": {
                            "id": "get_weather_1",
                            "name": "get_weather",
                            "args": {"city": "Paris"},
                        }},
                    ],
                },
                "index": 0,
                "finishReason": "STOP",
            })
        );
        assert_eq!(
            body["usageMetadata"],
            json!({
                "promptTokenCount": 2100,
                "cachedContentTokenCount": 1800,
                "candidatesTokenCount": 28,
                "thoughtsTokenCount": 12,
                "totalTokenCount": 2140,
            })
        );
    }

    #[test]
    fn generate_content_json_omits_zero_cache_and_thoughts() {
        let body = as_json(generate_content(
            &turn(
                Some(text("Hi")),
                Some(Finish::Stop),
                Usage {
                    input: 5,
                    output: 1,
                    ..Usage::default()
                },
            ),
            &IDS,
            false,
        ));
        let usage = &body["usageMetadata"];
        assert!(usage.get("cachedContentTokenCount").is_none());
        assert!(usage.get("thoughtsTokenCount").is_none());
        assert_eq!(usage["candidatesTokenCount"], 1);
        assert_eq!(usage["totalTokenCount"], 6);
        assert_eq!(body["candidates"][0]["finishReason"], "STOP");
        assert_eq!(
            body["candidates"][0]["content"]["parts"],
            json!([{"text": "Hi"}])
        );
    }

    #[test]
    fn generate_content_json_length_and_missing_finish() {
        let mut sample = turn(Some(text("cut")), Some(Finish::Length), Usage::default());
        assert_eq!(
            as_json(generate_content(&sample, &IDS, false))["candidates"][0]["finishReason"],
            "MAX_TOKENS"
        );
        sample.finish = None;
        assert_eq!(
            as_json(generate_content(&sample, &IDS, false))["candidates"][0]["finishReason"],
            "STOP"
        );
    }

    #[test]
    fn generate_stream_pieces_then_final_usage() {
        let mut sample = turn(
            Some(Message {
                text: Some("cd".to_owned()),
                reasoning: Some("ab".to_owned()),
                tool_calls: vec![ToolCall {
                    id: "get_weather_1".to_owned(),
                    name: "get_weather".to_owned(),
                    arguments: json!({"city": "Paris"}),
                }],
            }),
            Some(Finish::ToolCalls),
            Usage {
                input: 10,
                output: 7,
                cache_read: 4,
                cache_write: 0,
                reasoning: 2,
            },
        );
        sample.chunks = 2;
        let frames = as_sse(generate_content(&sample, &IDS, true));
        assert!(frames.iter().all(|frame| frame.event.is_none()));
        assert!(frames.iter().all(|frame| {
            let encoded = frame.encode("\r\n");
            encoded.starts_with("data: ")
                && !encoded.contains("event:")
                && encoded.ends_with("\r\n\r\n")
        }));
        let events: Vec<Value> = frames.iter().map(parse).collect();
        assert_eq!(events.len(), 5);
        let texts: Vec<&str> = events
            .iter()
            .take(4)
            .map(|event| {
                event["candidates"][0]["content"]["parts"][0]["text"]
                    .as_str()
                    .expect("text")
            })
            .collect();
        assert_eq!(texts, vec!["a", "b", "c", "d"]);
        assert_eq!(
            events[0]["candidates"][0]["content"]["parts"][0]["thought"],
            true
        );
        assert!(
            events[2]["candidates"][0]["content"]["parts"][0]
                .get("thought")
                .is_none()
        );
        for event in &events[..4] {
            assert!(event["candidates"][0].get("finishReason").is_none());
            assert_eq!(
                event["usageMetadata"],
                json!({"promptTokenCount": 10, "totalTokenCount": 10})
            );
            assert_eq!(event["responseId"], "capture-1");
        }
        let last = events.last().expect("final");
        assert_eq!(
            last["candidates"][0]["content"]["parts"],
            json!([{
                "functionCall": {
                    "id": "get_weather_1",
                    "name": "get_weather",
                    "args": {"city": "Paris"},
                },
            }])
        );
        assert_eq!(last["candidates"][0]["finishReason"], "STOP");
        assert_eq!(
            last["usageMetadata"],
            json!({
                "promptTokenCount": 10,
                "cachedContentTokenCount": 4,
                "candidatesTokenCount": 5,
                "thoughtsTokenCount": 2,
                "totalTokenCount": 17,
            })
        );
    }

    #[test]
    fn generate_stream_text_still_ends_with_a_finish_frame() {
        let mut sample = turn(
            Some(text("hello")),
            Some(Finish::Length),
            Usage {
                input: 3,
                output: 2,
                ..Usage::default()
            },
        );
        sample.chunks = 3;
        let events: Vec<Value> = as_sse(generate_content(&sample, &IDS, true))
            .iter()
            .map(parse)
            .collect();
        let texts: String = events
            .iter()
            .take(events.len() - 1)
            .filter_map(|event| event["candidates"][0]["content"]["parts"][0]["text"].as_str())
            .collect();
        assert_eq!(texts, "hello");
        let last = events.last().expect("final");
        assert_eq!(last["candidates"][0]["content"]["parts"], json!([]));
        assert_eq!(last["candidates"][0]["finishReason"], "MAX_TOKENS");
        assert_eq!(last["usageMetadata"]["candidatesTokenCount"], 2);
        assert_eq!(last["usageMetadata"]["totalTokenCount"], 5);
    }

    #[test]
    fn embed_content_float_vectors() {
        let mut sample = turn(None, None, Usage::default());
        sample.api = Api::GeminiEmbedContent;
        sample.dims = Some(2);
        let single = as_json(embed_content(&sample, 2, false));
        assert!(single.get("embeddings").is_none());
        assert_eq!(single["embedding"]["values"][0], json!(-0.5));
        let parsed = single["embedding"]["values"][1].as_f64().expect("number") as f32;
        assert_eq!(parsed, embedding_value(0, 1));

        let batch = as_json(embed_content(&sample, 2, true));
        assert!(batch.get("embedding").is_none());
        assert_eq!(batch["embeddings"].as_array().expect("list").len(), 2);
        let second = batch["embeddings"][1]["values"][0]
            .as_f64()
            .expect("number") as f32;
        assert_eq!(second, embedding_value(1, 0));
        assert_eq!(
            batch["embeddings"][0]["values"],
            single["embedding"]["values"]
        );
    }
}
