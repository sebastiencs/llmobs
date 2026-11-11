//! Vercel AI SDK `CoreMessage` / `ModelMessage` values.
//!
//! Spec 02 § Input and output. v4 tool calls use `args` and tool results use
//! `result`. v5+ uses `input` and `output: {type, value}`. `image` and `file`
//! parts carry raw base64, or a URL.

use llmobs_core::message::{Message, Modality, Part, Role};
use serde_json::{Map, Value as Json};

use crate::convert::ConvertError;
use crate::payload::parse_tool_arguments;

/// Convert a Vercel message array.
///
/// # Errors
///
/// [`ConvertError::NotAnArray`] when the value is not an array.
/// [`ConvertError::NotAMessage`] when an element has no string `role`.
pub(crate) fn vercel_messages(value: &Json) -> Result<Vec<Message>, ConvertError> {
    let Json::Array(items) = value else {
        return Err(ConvertError::NotAnArray);
    };
    items
        .iter()
        .enumerate()
        .map(|(index, item)| message(item, index))
        .collect()
}

/// One assistant message from `ai.response.text` and `ai.response.toolCalls`.
///
/// # Errors
///
/// [`ConvertError::NotAnArray`] when `tool_calls` is present and not an array.
pub(crate) fn assistant_message(
    text: Option<&str>,
    tool_calls: Option<&Json>,
    finish_reason: Option<String>,
) -> Result<Message, ConvertError> {
    let mut parts = Vec::new();
    if let Some(text) = text {
        parts.push(Part::Text {
            content: text.to_owned(),
            extra: Map::new(),
        });
    }
    if let Some(tool_calls) = tool_calls {
        let Json::Array(items) = tool_calls else {
            return Err(ConvertError::NotAnArray);
        };
        parts.extend(items.iter().cloned().map(response_tool_call));
    }
    Ok(Message {
        role: Role::Assistant,
        name: None,
        parts,
        finish_reason,
        extra: Map::new(),
    })
}

fn message(value: &Json, index: usize) -> Result<Message, ConvertError> {
    let Json::Object(map) = value else {
        return Err(ConvertError::NotAMessage { index });
    };
    let Some(Json::String(role)) = map.get("role") else {
        return Err(ConvertError::NotAMessage { index });
    };
    let mut extra = map.clone();
    extra.shift_remove("role");
    let parts = match extra.shift_remove("content") {
        Some(Json::String(text)) => vec![Part::Text {
            content: text,
            extra: Map::new(),
        }],
        Some(Json::Array(items)) => items.into_iter().map(content_part).collect(),
        Some(other) => vec![Part::Unknown(other)],
        None => Vec::new(),
    };
    Ok(Message {
        role: Role::from(role.as_str()),
        name: None,
        parts,
        finish_reason: None,
        extra,
    })
}

fn content_part(value: Json) -> Part {
    let Json::Object(map) = value else {
        return Part::Unknown(value);
    };
    let Some(kind) = map.get("type").and_then(Json::as_str).map(str::to_owned) else {
        return Part::Unknown(Json::Object(map));
    };
    match kind.as_str() {
        "text" => text_part(map, "text", false),
        "reasoning" => text_part(map, "text", true),
        "tool-call" => tool_call(map),
        "tool-result" => tool_result(map),
        "image" => media_part(map, "image", true),
        "file" => media_part(map, "data", false),
        _ => Part::Unknown(Json::Object(map)),
    }
}

fn text_part(map: Map<String, Json>, field: &str, reasoning: bool) -> Part {
    let Some(content) = map.get(field).and_then(Json::as_str).map(str::to_owned) else {
        return Part::Unknown(Json::Object(map));
    };
    let mut extra = map;
    extra.shift_remove("type");
    extra.shift_remove(field);
    if reasoning {
        Part::Reasoning { content, extra }
    } else {
        Part::Text { content, extra }
    }
}

fn tool_call(mut map: Map<String, Json>) -> Part {
    let Some(name) = take_string(&mut map, "toolName") else {
        return Part::Unknown(Json::Object(map));
    };
    map.shift_remove("type");
    let id = take_string(&mut map, "toolCallId");
    let arguments = if map.contains_key("input") {
        map.shift_remove("input")
    } else {
        map.shift_remove("args")
    }
    .map(parse_tool_arguments);
    Part::ToolCall {
        id,
        name,
        arguments,
        extra: map,
    }
}

fn response_tool_call(value: Json) -> Part {
    let Json::Object(map) = value else {
        return Part::Unknown(value);
    };
    tool_call(map)
}

fn tool_result(mut map: Map<String, Json>) -> Part {
    let id = take_string(&mut map, "toolCallId");
    map.shift_remove("type");
    let response = if let Some(output) = map.shift_remove("output") {
        output_value(output)
    } else {
        map.shift_remove("result").unwrap_or(Json::Null)
    };
    Part::ToolCallResponse {
        id,
        response: parse_tool_arguments(response),
        extra: map,
    }
}

fn output_value(output: Json) -> Json {
    let Json::Object(mut object) = output else {
        return output;
    };
    if object.contains_key("type")
        && let Some(value) = object.shift_remove("value")
    {
        return value;
    }
    Json::Object(object)
}

fn media_part(mut map: Map<String, Json>, field: &str, image: bool) -> Part {
    let Some(payload) = take_string(&mut map, field) else {
        return Part::Unknown(Json::Object(map));
    };
    map.shift_remove("type");
    let mime_type =
        take_string(&mut map, "mediaType").or_else(|| take_string(&mut map, "mimeType"));
    let modality = modality_of(image, mime_type.as_deref());
    if is_url(&payload) {
        Part::Uri {
            modality,
            mime_type,
            uri: payload,
            extra: map,
        }
    } else {
        Part::Blob {
            modality,
            mime_type,
            content: payload,
            extra: map,
        }
    }
}

fn modality_of(image: bool, mime_type: Option<&str>) -> Option<Modality> {
    if image {
        return Some(Modality::Image);
    }
    match mime_type.and_then(|mime| mime.split('/').next()) {
        Some("image") => Some(Modality::Image),
        Some("audio") => Some(Modality::Audio),
        Some("video") => Some(Modality::Video),
        _ => None,
    }
}

fn is_url(text: &str) -> bool {
    text.starts_with("data:") || text.contains("://")
}

fn take_string(map: &mut Map<String, Json>, key: &str) -> Option<String> {
    match map.shift_remove(key)? {
        Json::String(text) if !text.is_empty() => Some(text),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{assistant_message, vercel_messages};
    use llmobs_core::message::messages_to_json;
    use serde_json::json;

    #[test]
    fn v4_args_and_result_and_v5_input_and_output() {
        let value = json!([
            {
                "role": "assistant",
                "content": [{
                    "type": "tool-call",
                    "toolCallId": "call_4",
                    "toolName": "get_weather",
                    "args": {"city": "Paris"}
                }]
            },
            {
                "role": "tool",
                "content": [{
                    "type": "tool-result",
                    "toolCallId": "call_4",
                    "toolName": "get_weather",
                    "result": "{\"temp\":18}"
                }]
            },
            {
                "role": "assistant",
                "content": [{
                    "type": "tool-call",
                    "toolCallId": "call_5",
                    "toolName": "get_weather",
                    "input": {"city": "Lyon"}
                }]
            },
            {
                "role": "tool",
                "content": [{
                    "type": "tool-result",
                    "toolCallId": "call_5",
                    "toolName": "get_weather",
                    "output": {"type": "json", "value": {"temp": 20}}
                }]
            }
        ]);
        let messages = vercel_messages(&value).expect("messages");
        assert_eq!(
            messages_to_json(&messages),
            concat!(
                r#"[{"role":"assistant","parts":[{"type":"tool_call","id":"call_4","name":"get_weather","arguments":{"city":"Paris"}}]},"#,
                r#"{"role":"tool","parts":[{"type":"tool_call_response","id":"call_4","response":{"temp":18},"toolName":"get_weather"}]},"#,
                r#"{"role":"assistant","parts":[{"type":"tool_call","id":"call_5","name":"get_weather","arguments":{"city":"Lyon"}}]},"#,
                r#"{"role":"tool","parts":[{"type":"tool_call_response","id":"call_5","response":{"temp":20},"toolName":"get_weather"}]}]"#
            )
        );
    }

    #[test]
    fn image_base64_is_a_blob_and_a_url_is_a_uri() {
        let value = json!([{
            "role": "user",
            "content": [
                {"type": "text", "text": "Look"},
                {"type": "reasoning", "text": "checking"},
                {"type": "image", "image": "aaaa", "mimeType": "image/png"},
                {"type": "image", "image": "https://example.com/cat.png", "mediaType": "image/png"},
                {"type": "file", "data": "bbbb", "mediaType": "application/pdf"}
            ]
        }]);
        let messages = vercel_messages(&value).expect("messages");
        assert_eq!(
            messages_to_json(&messages),
            concat!(
                r#"[{"role":"user","parts":["#,
                r#"{"type":"text","content":"Look"},"#,
                r#"{"type":"reasoning","content":"checking"},"#,
                r#"{"type":"blob","modality":"image","mime_type":"image/png","content":"aaaa"},"#,
                r#"{"type":"uri","modality":"image","mime_type":"image/png","uri":"https://example.com/cat.png"},"#,
                r#"{"type":"blob","mime_type":"application/pdf","content":"bbbb"}"#,
                r#"]}]"#
            )
        );
    }

    #[test]
    fn response_tool_calls_omit_type_and_keep_input() {
        let message = assistant_message(
            Some("Hi"),
            Some(&json!([{"toolCallId": "call_1", "toolName": "get_weather", "input": {"city": "Paris"}}])),
            Some("tool-calls".to_owned()),
        )
        .expect("assistant");
        assert_eq!(
            messages_to_json(&[message]),
            concat!(
                r#"[{"role":"assistant","parts":["#,
                r#"{"type":"text","content":"Hi"},"#,
                r#"{"type":"tool_call","id":"call_1","name":"get_weather","arguments":{"city":"Paris"}}"#,
                r#"],"finish_reason":"tool-calls"}]"#
            )
        );
    }
}
