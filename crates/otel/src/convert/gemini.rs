//! Gemini `contents` and system instructions.
//!
//! Spec 01 § Canonical message format. Part keys are accepted in both camelCase
//! and snake_case. `role: "model"` becomes `assistant`.

use llmobs_core::message::{Message, Modality, Part, Role};
use serde_json::{Map, Value as Json};

use super::ConvertError;
use crate::payload::parse_tool_arguments;

/// Convert a `contents` array of `{role, parts}` objects.
///
/// # Errors
///
/// [`ConvertError::NotAnArray`] when `value` is not an array.
/// [`ConvertError::NotAMessage`] when an element has no string `role`.
#[allow(dead_code)] // A later convention phase calls this.
pub(crate) fn gemini_contents(value: &Json) -> Result<Vec<Message>, ConvertError> {
    let Json::Array(items) = value else {
        return Err(ConvertError::NotAnArray);
    };
    items
        .iter()
        .enumerate()
        .map(|(index, item)| message(item, index))
        .collect()
}

/// Instruction parts as JSON.
///
/// A string, a part array, a part object, or a content object `{parts}` becomes
/// a JSON array of canonical parts. Any other value is returned unchanged.
#[allow(dead_code)] // A later convention phase calls this.
pub(crate) fn gemini_system_instruction(value: &Json) -> Json {
    match value {
        Json::String(text) => parts_json(&[plain_text(text.clone())]),
        Json::Array(items) => {
            let parts: Vec<Part> = items.iter().cloned().map(part).collect();
            parts_json(&parts)
        }
        Json::Object(map) => {
            if let Some(Json::Array(items)) = map.get("parts") {
                let parts: Vec<Part> = items.iter().cloned().map(part).collect();
                return parts_json(&parts);
            }
            let converted = part(value.clone());
            if matches!(converted, Part::Unknown(_)) {
                value.clone()
            } else {
                parts_json(&[converted])
            }
        }
        other => other.clone(),
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
    let mut parts = Vec::new();
    if let Some(raw) = extra.shift_remove("parts") {
        match raw {
            Json::Null => {}
            Json::Array(items) => parts.extend(items.into_iter().map(part)),
            other => {
                extra.insert("parts".to_owned(), other);
            }
        }
    }
    Ok(Message {
        role: gemini_role(role),
        name: None,
        parts,
        finish_reason: None,
        extra,
    })
}

fn gemini_role(role: &str) -> Role {
    if role == "model" {
        Role::Assistant
    } else {
        Role::from(role)
    }
}

fn part(value: Json) -> Part {
    let Json::Object(map) = value else {
        return Part::Unknown(value);
    };
    if key(&map, "functionCall", "function_call").is_some() {
        return function_call(map);
    }
    if key(&map, "functionResponse", "function_response").is_some() {
        return function_response(map);
    }
    if key(&map, "inlineData", "inline_data").is_some() {
        return inline_data(map);
    }
    if key(&map, "fileData", "file_data").is_some() {
        return file_data(map);
    }
    if map.contains_key("text") {
        return text_part(map);
    }
    Part::Unknown(Json::Object(map))
}

fn text_part(map: Map<String, Json>) -> Part {
    let thought = map.get("thought") == Some(&Json::Bool(true));
    let Some(content) = map.get("text").and_then(Json::as_str).map(str::to_owned) else {
        return Part::Unknown(Json::Object(map));
    };
    let mut extra = map;
    extra.shift_remove("text");
    extra.shift_remove("thought");
    if thought {
        Part::Reasoning { content, extra }
    } else {
        Part::Text { content, extra }
    }
}

fn plain_text(content: String) -> Part {
    Part::Text {
        content,
        extra: Map::new(),
    }
}

fn function_call(mut map: Map<String, Json>) -> Part {
    let Some(raw_key) = key(&map, "functionCall", "function_call") else {
        return Part::Unknown(Json::Object(map));
    };
    let key = raw_key.to_owned();
    let Some(object) = map.get(&key).and_then(Json::as_object) else {
        return Part::Unknown(Json::Object(map));
    };
    let Some(name) = object.get("name").and_then(Json::as_str).map(str::to_owned) else {
        return Part::Unknown(Json::Object(map));
    };
    let id = object.get("id").and_then(Json::as_str).map(str::to_owned);
    let arguments = object.get("args").cloned().map(parse_tool_arguments);
    let mut nested = object.clone();
    nested.shift_remove("name");
    nested.shift_remove("args");
    if id.is_some() {
        nested.shift_remove("id");
    }
    map.shift_remove(&key);
    map.shift_remove(other_key(&key));
    for (field, value) in nested {
        map.entry(field).or_insert(value);
    }
    Part::ToolCall {
        id,
        name,
        arguments,
        extra: map,
    }
}

fn function_response(mut map: Map<String, Json>) -> Part {
    let Some(raw_key) = key(&map, "functionResponse", "function_response") else {
        return Part::Unknown(Json::Object(map));
    };
    let key = raw_key.to_owned();
    let Some(object) = map.get(&key).and_then(Json::as_object) else {
        return Part::Unknown(Json::Object(map));
    };
    let id = object.get("id").and_then(Json::as_str).map(str::to_owned);
    let response = object
        .get("response")
        .cloned()
        .map(parse_tool_arguments)
        .unwrap_or(Json::Null);
    let mut nested = object.clone();
    nested.shift_remove("response");
    if id.is_some() {
        nested.shift_remove("id");
    }
    map.shift_remove(&key);
    map.shift_remove(other_key(&key));
    for (field, value) in nested {
        map.entry(field).or_insert(value);
    }
    Part::ToolCallResponse {
        id,
        response,
        extra: map,
    }
}

fn inline_data(map: Map<String, Json>) -> Part {
    media_part(map, "inlineData", "inline_data", "data", MediaKind::Blob)
}

fn file_data(map: Map<String, Json>) -> Part {
    media_part(map, "fileData", "file_data", "fileUri", MediaKind::Uri)
}

fn media_part(
    mut map: Map<String, Json>,
    camel: &str,
    snake: &str,
    uri_or_data_camel: &str,
    kind: MediaKind,
) -> Part {
    let Some(raw_key) = key(&map, camel, snake) else {
        return Part::Unknown(Json::Object(map));
    };
    let key = raw_key.to_owned();
    let Some(object) = map.get(&key).and_then(Json::as_object) else {
        return Part::Unknown(Json::Object(map));
    };
    let payload_key = if object.contains_key(uri_or_data_camel) {
        uri_or_data_camel
    } else if uri_or_data_camel == "fileUri" && object.contains_key("file_uri") {
        "file_uri"
    } else {
        return Part::Unknown(Json::Object(map));
    };
    let Some(payload) = object
        .get(payload_key)
        .and_then(Json::as_str)
        .map(str::to_owned)
    else {
        return Part::Unknown(Json::Object(map));
    };
    let mime_type = mime_type(object);
    let modality = modality_of(mime_type.as_deref());
    let mut nested = object.clone();
    nested.shift_remove(payload_key);
    nested.shift_remove("mimeType");
    nested.shift_remove("mime_type");
    map.shift_remove(&key);
    map.shift_remove(other_key(&key));
    for (field, value) in nested {
        map.entry(field).or_insert(value);
    }
    match kind {
        MediaKind::Blob => Part::Blob {
            modality,
            mime_type,
            content: payload,
            extra: map,
        },
        MediaKind::Uri => Part::Uri {
            modality,
            mime_type,
            uri: payload,
            extra: map,
        },
    }
}

fn mime_type(object: &Map<String, Json>) -> Option<String> {
    object
        .get("mimeType")
        .or_else(|| object.get("mime_type"))
        .and_then(Json::as_str)
        .map(str::to_owned)
}

fn modality_of(mime: Option<&str>) -> Option<Modality> {
    let (top, _) = mime?.split_once('/')?;
    match top {
        "image" => Some(Modality::Image),
        "audio" => Some(Modality::Audio),
        "video" => Some(Modality::Video),
        _ => None,
    }
}

fn key<'a>(map: &Map<String, Json>, camel: &'a str, snake: &'a str) -> Option<&'a str> {
    if map.contains_key(camel) {
        Some(camel)
    } else if map.contains_key(snake) {
        Some(snake)
    } else {
        None
    }
}

fn other_key(found: &str) -> &'static str {
    match found {
        "functionCall" => "function_call",
        "function_call" => "functionCall",
        "functionResponse" => "function_response",
        "function_response" => "functionResponse",
        "inlineData" => "inline_data",
        "inline_data" => "inlineData",
        "fileData" => "file_data",
        "file_data" => "fileData",
        _ => "",
    }
}

#[derive(Clone, Copy)]
enum MediaKind {
    Blob,
    Uri,
}

fn parts_json(parts: &[Part]) -> Json {
    serde_json::to_value(parts).unwrap_or_else(|_| Json::Array(Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::{gemini_contents, gemini_system_instruction};
    use crate::convert::ConvertError;
    use llmobs_core::message::messages_to_json;
    use serde_json::json;

    fn contents(value: &serde_json::Value) -> String {
        let messages = gemini_contents(value).expect("contents");
        messages_to_json(&messages)
    }

    #[test]
    fn model_role_is_assistant_and_text_is_a_text_part() {
        assert_eq!(
            contents(&json!([
                {"role": "user", "parts": [{"text": "hi"}]},
                {"role": "model", "parts": [{"text": "ok", "thoughtSignature": "sig"}]}
            ])),
            concat!(
                r#"[{"role":"user","parts":[{"type":"text","content":"hi"}]},"#,
                r#"{"role":"assistant","parts":[{"type":"text","content":"ok","thoughtSignature":"sig"}]}]"#
            )
        );
    }

    #[test]
    fn thought_text_is_reasoning() {
        assert_eq!(
            contents(&json!([{
                "role": "model",
                "parts": [{"text": "because", "thought": true}, {"text": "answer", "thought": false}]
            }])),
            concat!(
                r#"[{"role":"assistant","parts":["#,
                r#"{"type":"reasoning","content":"because"},"#,
                r#"{"type":"text","content":"answer"}]}]"#
            )
        );
    }

    #[test]
    fn inline_data_is_a_blob_in_either_spelling() {
        assert_eq!(
            contents(&json!([{
                "role": "user",
                "parts": [
                    {"inlineData": {"mimeType": "image/png", "data": "AAAA"}},
                    {"inline_data": {"mime_type": "audio/wav", "data": "QQQ="}},
                    {"inlineData": {"mimeType": "application/pdf", "data": "JVBE"}}
                ]
            }])),
            concat!(
                r#"[{"role":"user","parts":["#,
                r#"{"type":"blob","modality":"image","mime_type":"image/png","content":"AAAA"},"#,
                r#"{"type":"blob","modality":"audio","mime_type":"audio/wav","content":"QQQ="},"#,
                r#"{"type":"blob","mime_type":"application/pdf","content":"JVBE"}"#,
                r#"]}]"#
            )
        );
    }

    #[test]
    fn file_data_is_a_uri_in_either_spelling() {
        assert_eq!(
            contents(&json!([{
                "role": "user",
                "parts": [
                    {"fileData": {"mimeType": "video/mp4", "fileUri": "https://example.com/a.mp4"}},
                    {"file_data": {"mime_type": "application/pdf", "file_uri": "https://example.com/a.pdf"}}
                ]
            }])),
            concat!(
                r#"[{"role":"user","parts":["#,
                r#"{"type":"uri","modality":"video","mime_type":"video/mp4","uri":"https://example.com/a.mp4"},"#,
                r#"{"type":"uri","mime_type":"application/pdf","uri":"https://example.com/a.pdf"}"#,
                r#"]}]"#
            )
        );
    }

    #[test]
    fn function_calls_parse_args_in_either_spelling() {
        assert_eq!(
            contents(&json!([{
                "role": "model",
                "parts": [
                    {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}, "id": "c1"}},
                    {"function_call": {"name": "lookup", "args": "{\"q\":1}"}},
                    {"functionCall": {"name": "bare"}, "text": "also"}
                ]
            }])),
            concat!(
                r#"[{"role":"assistant","parts":["#,
                r#"{"type":"tool_call","id":"c1","name":"get_weather","arguments":{"city":"Paris"}},"#,
                r#"{"type":"tool_call","name":"lookup","arguments":{"q":1}},"#,
                r#"{"type":"tool_call","name":"bare","text":"also"}"#,
                r#"]}]"#
            )
        );
    }

    #[test]
    fn function_responses_keep_the_name_in_either_spelling() {
        assert_eq!(
            contents(&json!([{
                "role": "user",
                "parts": [
                    {"functionResponse": {"name": "get_weather", "response": {"temp": 18}, "id": "c1"}},
                    {"function_response": {"name": "lookup", "response": "{\"ok\":true}"}}
                ]
            }])),
            concat!(
                r#"[{"role":"user","parts":["#,
                r#"{"type":"tool_call_response","id":"c1","response":{"temp":18},"name":"get_weather"},"#,
                r#"{"type":"tool_call_response","response":{"ok":true},"name":"lookup"}"#,
                r#"]}]"#
            )
        );
    }

    #[test]
    fn system_instruction_becomes_canonical_parts() {
        assert_eq!(
            gemini_system_instruction(&json!({"parts": [{"text": "be brief"}]})),
            json!([{"type": "text", "content": "be brief"}])
        );
        assert_eq!(
            gemini_system_instruction(&json!("be brief")),
            json!([{"type": "text", "content": "be brief"}])
        );
        assert_eq!(
            gemini_system_instruction(&json!([
                {"inline_data": {"mime_type": "image/png", "data": "AAAA"}}
            ])),
            json!([{
                "type": "blob",
                "modality": "image",
                "mime_type": "image/png",
                "content": "AAAA"
            }])
        );
        assert_eq!(
            gemini_system_instruction(&json!({"text": "direct"})),
            json!([{"type": "text", "content": "direct"}])
        );
        assert_eq!(
            gemini_system_instruction(&json!({"role": "user"})),
            json!({"role": "user"})
        );
        assert_eq!(
            gemini_system_instruction(&serde_json::Value::Null),
            serde_json::Value::Null
        );
    }

    #[test]
    fn an_unknown_part_stays_on_the_message() {
        assert_eq!(
            contents(&json!([{
                "role": "model",
                "parts": [{"executableCode": {"language": "PYTHON", "code": "print(1)"}}],
                "citation": 1
            }])),
            concat!(
                r#"[{"role":"assistant","parts":[{"executableCode":{"language":"PYTHON","code":"print(1)"}}],"#,
                r#""citation":1}]"#
            )
        );
    }

    #[test]
    fn malformed_input_is_an_error_without_the_payload() {
        let secret = "do-not-echo";
        let not_array = gemini_contents(&json!(secret)).expect_err("not an array");
        assert_eq!(not_array, ConvertError::NotAnArray);
        assert_eq!(not_array.to_string(), "messages are not an array");
        assert!(!not_array.to_string().contains(secret));

        let bare = gemini_contents(&json!({"role": "model", "parts": [{"text": secret}]}))
            .expect_err("bare object");
        assert_eq!(bare, ConvertError::NotAnArray);

        let missing = gemini_contents(&json!([{"parts": [{"text": secret}]}])).expect_err("role");
        assert_eq!(missing, ConvertError::NotAMessage { index: 0 });
        assert_eq!(
            missing.to_string(),
            "message 0 is not an object with a string role"
        );
        assert!(!missing.to_string().contains(secret));
    }
}
