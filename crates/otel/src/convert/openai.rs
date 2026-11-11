//! OpenAI Chat Completions messages, and one `choices[].message`.
//!
//! Spec 01 § Canonical message format. Responses API `output` items are not
//! converted here: the N13 and N18 fixtures use Chat Completions shapes.

use llmobs_core::message::{Message, Modality, Part, Role};
use serde_json::{Map, Value as Json};

use crate::payload::parse_tool_arguments;

/// Why an OpenAI message value could not be converted.
///
/// The display text is an index or a fixed sentence. It never includes payload
/// content. N13 and N18 call [`openai_messages`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum ConvertError {
    /// The value is neither a message array nor one message object.
    #[error("messages are not an array")]
    NotAnArray,
    /// `index` is not a JSON object with a string `role`.
    #[error("message {index} is not an object with a string role")]
    NotAMessage {
        /// Index in the array. A bare message object is index 0.
        index: usize,
    },
}

/// Convert a Chat Completions `messages` array, or one message object.
///
/// # Errors
///
/// [`ConvertError::NotAnArray`] when the value is neither an array nor an
/// object. [`ConvertError::NotAMessage`] when an element has no string `role`.
#[allow(dead_code)] // N13 and N18 call this.
pub(crate) fn openai_messages(value: &Json) -> Result<Vec<Message>, ConvertError> {
    match value {
        Json::Array(items) => items
            .iter()
            .enumerate()
            .map(|(index, item)| message(item, index))
            .collect(),
        Json::Object(_) => message(value, 0).map(|item| vec![item]),
        _ => Err(ConvertError::NotAnArray),
    }
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
    let name = take_string(&mut extra, "name");
    let mut parts = Vec::new();
    if role == "tool"
        && let Some(id) = take_string(&mut extra, "tool_call_id")
    {
        let response = extra
            .shift_remove("content")
            .map(parse_tool_arguments)
            .unwrap_or(Json::Null);
        parts.push(Part::ToolCallResponse {
            id: Some(id),
            response,
            extra: Map::new(),
        });
    } else if let Some(content) = extra.shift_remove("content") {
        match content_parts(content) {
            Ok(converted) => parts.extend(converted),
            Err(content) => {
                extra.insert("content".to_owned(), content);
            }
        }
    }
    if let Some(calls) = extra.shift_remove("tool_calls") {
        parts.extend(tool_calls(calls));
    }
    if let Some(reasoning) = take_string(&mut extra, "reasoning_content") {
        parts.push(Part::Reasoning {
            content: reasoning,
            extra: Map::new(),
        });
    }
    Ok(Message {
        role: Role::from(role.as_str()),
        name,
        parts,
        finish_reason: None,
        extra,
    })
}

fn content_parts(content: Json) -> Result<Vec<Part>, Json> {
    match content {
        Json::Null => Ok(Vec::new()),
        Json::String(text) => Ok(vec![Part::Text {
            content: text,
            extra: Map::new(),
        }]),
        Json::Array(items) => Ok(items.into_iter().map(content_part).collect()),
        other => Err(other),
    }
}

fn content_part(value: Json) -> Part {
    let Json::Object(map) = value else {
        return Part::Unknown(value);
    };
    let Some(Json::String(kind)) = map.get("type").cloned() else {
        return Part::Unknown(Json::Object(map));
    };
    match kind.as_str() {
        "text" => text_part(map, "text"),
        "refusal" => text_part(map, "refusal"),
        "image_url" => image_part(map),
        "input_audio" => audio_part(map),
        // `file`, and any type this converter does not model, stays verbatim.
        _ => Part::Unknown(Json::Object(map)),
    }
}

fn text_part(map: Map<String, Json>, field: &str) -> Part {
    let Some(Json::String(content)) = map.get(field).cloned() else {
        return Part::Unknown(Json::Object(map));
    };
    let mut extra = map;
    extra.shift_remove("type");
    extra.shift_remove(field);
    Part::Text { content, extra }
}

fn image_part(map: Map<String, Json>) -> Part {
    let Some((url, nested)) = image_url(map.get("image_url")) else {
        return Part::Unknown(Json::Object(map));
    };
    let mut extra = map;
    extra.shift_remove("type");
    extra.shift_remove("image_url");
    for (key, value) in nested {
        extra.entry(key).or_insert(value);
    }
    if let Some((mime_type, content)) = data_uri(&url) {
        Part::Blob {
            modality: Some(Modality::Image),
            mime_type,
            content,
            extra,
        }
    } else {
        Part::Uri {
            modality: Some(Modality::Image),
            mime_type: None,
            uri: url,
            extra,
        }
    }
}

fn image_url(value: Option<&Json>) -> Option<(String, Map<String, Json>)> {
    match value {
        Some(Json::String(url)) => Some((url.clone(), Map::new())),
        Some(Json::Object(object)) => {
            let url = object.get("url")?.as_str()?.to_owned();
            let mut nested = object.clone();
            nested.shift_remove("url");
            Some((url, nested))
        }
        _ => None,
    }
}

fn audio_part(map: Map<String, Json>) -> Part {
    let Some((content, mime_type, nested)) = audio_body(map.get("input_audio")) else {
        return Part::Unknown(Json::Object(map));
    };
    let mut extra = map;
    extra.shift_remove("type");
    extra.shift_remove("input_audio");
    for (key, value) in nested {
        extra.entry(key).or_insert(value);
    }
    Part::Blob {
        modality: Some(Modality::Audio),
        mime_type,
        content,
        extra,
    }
}

fn audio_body(value: Option<&Json>) -> Option<(String, Option<String>, Map<String, Json>)> {
    let object = value?.as_object()?;
    let content = object.get("data")?.as_str()?.to_owned();
    let mut nested = object.clone();
    nested.shift_remove("data");
    let mime_type = match nested.shift_remove("format") {
        Some(Json::String(format)) if !format.is_empty() => Some(audio_mime(&format)),
        Some(format) => {
            nested.insert("format".to_owned(), format);
            None
        }
        None => None,
    };
    Some((content, mime_type, nested))
}

fn audio_mime(format: &str) -> String {
    if format.contains('/') {
        format.to_owned()
    } else {
        format!("audio/{format}")
    }
}

/// A `data:` URL splits into a MIME type and the payload after the first comma.
///
/// The payload is kept as written. N14 reads it as base64.
fn data_uri(url: &str) -> Option<(Option<String>, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let mime_type = meta
        .split(';')
        .next()
        .filter(|mime| !mime.is_empty())
        .map(str::to_owned);
    Some((mime_type, data.to_owned()))
}

fn tool_calls(value: Json) -> Vec<Part> {
    let Json::Array(items) = value else {
        return vec![Part::Unknown(value)];
    };
    items.into_iter().map(tool_call).collect()
}

fn tool_call(value: Json) -> Part {
    let Json::Object(map) = value else {
        return Part::Unknown(value);
    };
    let Some(name) = map
        .get("function")
        .and_then(Json::as_object)
        .and_then(|function| function.get("name"))
        .and_then(Json::as_str)
        .map(str::to_owned)
    else {
        return Part::Unknown(Json::Object(map));
    };
    let mut map = map;
    let mut function = match map.shift_remove("function") {
        Some(Json::Object(function)) => function,
        other => {
            if let Some(other) = other {
                map.insert("function".to_owned(), other);
            }
            return Part::Unknown(Json::Object(map));
        }
    };
    function.shift_remove("name");
    let arguments = function.shift_remove("arguments").map(parse_tool_arguments);
    let id = match map.get("id") {
        Some(Json::String(id)) => Some(id.clone()),
        _ => None,
    };
    if id.is_some() {
        map.shift_remove("id");
    }
    map.shift_remove("type");
    for (key, value) in function {
        map.entry(key).or_insert(value);
    }
    Part::ToolCall {
        id,
        name,
        arguments,
        extra: map,
    }
}

fn take_string(map: &mut Map<String, Json>, key: &str) -> Option<String> {
    match map.get(key) {
        Some(Json::String(_)) => match map.shift_remove(key) {
            Some(Json::String(text)) => Some(text),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{ConvertError, openai_messages};
    use llmobs_core::message::messages_to_json;
    use serde_json::json;

    fn converted(value: &serde_json::Value) -> String {
        let messages = openai_messages(value).expect("messages");
        messages_to_json(&messages)
    }

    #[test]
    fn role_and_name_are_kept() {
        assert_eq!(
            converted(&json!([
                {"role": "system", "name": "guide", "content": "be brief"},
                {"role": "developer", "content": "rules"}
            ])),
            concat!(
                r#"[{"role":"system","name":"guide","parts":[{"type":"text","content":"be brief"}]},"#,
                r#"{"role":"developer","parts":[{"type":"text","content":"rules"}]}]"#
            )
        );
    }

    #[test]
    fn a_string_content_is_one_text_part() {
        assert_eq!(
            converted(&json!([{"role": "user", "content": "hi"}])),
            r#"[{"role":"user","parts":[{"type":"text","content":"hi"}]}]"#
        );
    }

    #[test]
    fn content_parts_keep_their_order_and_extra_fields() {
        assert_eq!(
            converted(&json!([{
                "role": "user",
                "content": [
                    {"type": "text", "text": "see", "annotations": [1]},
                    {"type": "refusal", "refusal": "no"},
                    {"type": "image_url", "image_url": {"url": "https://cdn.example/a.png", "detail": "high"}},
                    {"type": "image_url", "image_url": "https://cdn.example/b.png"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
                    {"type": "image_url", "image_url": {"url": "data:image/svg+xml;charset=utf-8,PHN2Zw=="}},
                    {"type": "input_audio", "input_audio": {"data": "QQQ=", "format": "wav"}},
                    {"type": "input_audio", "input_audio": {"data": "UUU=", "format": "audio/mpeg"}},
                    {"type": "file", "file": {"file_id": "file_1"}},
                    "not-a-part"
                ]
            }])),
            concat!(
                r#"[{"role":"user","parts":["#,
                r#"{"type":"text","content":"see","annotations":[1]},"#,
                r#"{"type":"text","content":"no"},"#,
                r#"{"type":"uri","modality":"image","uri":"https://cdn.example/a.png","detail":"high"},"#,
                r#"{"type":"uri","modality":"image","uri":"https://cdn.example/b.png"},"#,
                r#"{"type":"blob","modality":"image","mime_type":"image/png","content":"AAAA"},"#,
                r#"{"type":"blob","modality":"image","mime_type":"image/svg+xml","content":"PHN2Zw=="},"#,
                r#"{"type":"blob","modality":"audio","mime_type":"audio/wav","content":"QQQ="},"#,
                r#"{"type":"blob","modality":"audio","mime_type":"audio/mpeg","content":"UUU="},"#,
                r#"{"type":"file","file":{"file_id":"file_1"}},"#,
                r#""not-a-part"]}]"#
            )
        );
    }

    #[test]
    fn tool_calls_parse_arguments_and_keep_unknown_fields() {
        assert_eq!(
            converted(&json!([{
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {
                        "id": "call_1",
                        "type": "function",
                        "index": 0,
                        "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}", "description": "d"}
                    },
                    {"id": "call_2", "type": "function", "function": {"name": "ping", "arguments": "not json"}},
                    {"id": "call_3", "type": "function", "function": {"name": "bare"}},
                    {"type": "function", "function": {"name": "anon", "arguments": {"ok": true}}},
                    "not-a-call"
                ]
            }])),
            concat!(
                r#"[{"role":"assistant","parts":["#,
                r#"{"type":"tool_call","id":"call_1","name":"get_weather","arguments":{"city":"Paris"},"index":0,"description":"d"},"#,
                r#"{"type":"tool_call","id":"call_2","name":"ping","arguments":"not json"},"#,
                r#"{"type":"tool_call","id":"call_3","name":"bare"},"#,
                r#"{"type":"tool_call","name":"anon","arguments":{"ok":true}},"#,
                r#""not-a-call"]}]"#
            )
        );
    }

    #[test]
    fn a_tool_message_is_a_tool_call_response() {
        assert_eq!(
            converted(&json!([
                {"role": "tool", "name": "get_weather", "tool_call_id": "call_1", "content": "{\"temp\":18}"},
                {"role": "tool", "tool_call_id": "call_2", "content": "72F"}
            ])),
            concat!(
                r#"[{"role":"tool","name":"get_weather","parts":[{"type":"tool_call_response","id":"call_1","response":{"temp":18}}]},"#,
                r#"{"role":"tool","parts":[{"type":"tool_call_response","id":"call_2","response":"72F"}]}]"#
            )
        );
    }

    #[test]
    fn reasoning_content_is_a_reasoning_part_after_the_reply() {
        assert_eq!(
            converted(&json!([{
                "role": "assistant",
                "content": "yes",
                "tool_calls": [{"id": "call_1", "type": "function", "function": {"name": "ping", "arguments": "{}"}}],
                "reasoning_content": "because"
            }])),
            concat!(
                r#"[{"role":"assistant","parts":["#,
                r#"{"type":"text","content":"yes"},"#,
                r#"{"type":"tool_call","id":"call_1","name":"ping","arguments":{}},"#,
                r#"{"type":"reasoning","content":"because"}]}]"#
            )
        );
    }

    #[test]
    fn unknown_fields_stay_on_the_message() {
        assert_eq!(
            converted(
                &json!([{"role": "user", "content": "hi", "custom": 1, "finish_reason": "stop"}])
            ),
            r#"[{"role":"user","parts":[{"type":"text","content":"hi"}],"custom":1,"finish_reason":"stop"}]"#
        );
    }

    #[test]
    fn a_single_message_object_is_accepted() {
        assert_eq!(
            converted(&json!({"role": "assistant", "content": "done"})),
            r#"[{"role":"assistant","parts":[{"type":"text","content":"done"}]}]"#
        );
    }

    #[test]
    fn a_part_that_cannot_be_modelled_stays_on_the_message() {
        assert_eq!(
            converted(&json!([{
                "role": "user",
                "content": [{"type": "text"}, {"type": "image_url", "image_url": {}}],
                "tool_calls": [{"id": "call_1", "function": {"arguments": "{}"}}]
            }])),
            concat!(
                r#"[{"role":"user","parts":["#,
                r#"{"type":"text"},"#,
                r#"{"type":"image_url","image_url":{}},"#,
                r#"{"id":"call_1","function":{"arguments":"{}"}}]}]"#
            )
        );
    }

    #[test]
    fn malformed_input_is_an_error_without_the_payload() {
        let secret = "do-not-echo";
        let not_array = openai_messages(&json!(secret)).expect_err("not an array");
        assert_eq!(not_array, ConvertError::NotAnArray);
        assert_eq!(not_array.to_string(), "messages are not an array");
        assert!(!not_array.to_string().contains(secret));

        let missing_role =
            openai_messages(&json!([{"content": secret}, {"role": 1}])).expect_err("not a message");
        assert_eq!(missing_role, ConvertError::NotAMessage { index: 0 });
        assert_eq!(
            missing_role.to_string(),
            "message 0 is not an object with a string role"
        );
        assert!(!missing_role.to_string().contains(secret));

        let later = openai_messages(&json!([
            {"role": "user", "content": "ok"},
            {"content": "no role"}
        ]))
        .expect_err("second item");
        assert_eq!(later, ConvertError::NotAMessage { index: 1 });

        let bare = openai_messages(&json!({"content": "no role"})).expect_err("bare object");
        assert_eq!(bare, ConvertError::NotAMessage { index: 0 });
        assert_eq!(
            openai_messages(&json!(null)).expect_err("null"),
            ConvertError::NotAnArray
        );
    }

    #[test]
    fn an_empty_array_has_no_messages() {
        assert_eq!(converted(&json!([])), "[]");
    }
}
