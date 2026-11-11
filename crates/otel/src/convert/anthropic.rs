//! Anthropic Messages API content blocks.
//!
//! Spec 01 § Canonical message format. The top-level `system` value is returned
//! separately and is not inserted into the message list (spec 02 rule 5).

use llmobs_core::message::{Message, Modality, Part, Role};
use serde_json::{Map, Value as Json};

use super::ConvertError;
use crate::payload::parse_tool_arguments;

/// Convert a Messages API `messages` array and an optional top-level `system`.
///
/// A string or array `system` becomes a JSON array of canonical parts. Any
/// other JSON value is returned unchanged. `None` and JSON null become `None`.
///
/// # Errors
///
/// [`ConvertError::NotAnArray`] when `value` is not an array.
/// [`ConvertError::NotAMessage`] when an element has no string `role`.
#[allow(dead_code)] // A later convention phase calls this.
pub(crate) fn anthropic_messages(
    value: &Json,
    system: Option<&Json>,
) -> Result<(Vec<Message>, Option<Json>), ConvertError> {
    let Json::Array(items) = value else {
        return Err(ConvertError::NotAnArray);
    };
    let messages = items
        .iter()
        .enumerate()
        .map(|(index, item)| message(item, index))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((messages, system_instructions(system)))
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
    if let Some(content) = extra.shift_remove("content") {
        match content_parts(content) {
            Ok(converted) => parts.extend(converted),
            Err(content) => {
                extra.insert("content".to_owned(), content);
            }
        }
    }
    Ok(Message {
        role: Role::from(role.as_str()),
        name: None,
        parts,
        finish_reason: None,
        extra,
    })
}

fn system_instructions(system: Option<&Json>) -> Option<Json> {
    match system? {
        Json::Null => None,
        Json::String(text) => Some(parts_json(&[text_part_value(text.clone(), Map::new())])),
        Json::Array(items) => {
            let parts: Vec<Part> = items.iter().cloned().map(block).collect();
            Some(parts_json(&parts))
        }
        other => Some(other.clone()),
    }
}

fn content_parts(content: Json) -> Result<Vec<Part>, Json> {
    match content {
        Json::Null => Ok(Vec::new()),
        Json::String(text) => Ok(vec![text_part_value(text, Map::new())]),
        Json::Array(items) => Ok(items.into_iter().map(block).collect()),
        other => Err(other),
    }
}

fn block(value: Json) -> Part {
    let Json::Object(map) = value else {
        return Part::Unknown(value);
    };
    let Some(kind) = map.get("type").and_then(Json::as_str).map(str::to_owned) else {
        return Part::Unknown(Json::Object(map));
    };
    match kind.as_str() {
        "text" => text_block(map),
        "thinking" => thinking_block(map),
        "redacted_thinking" => Part::Unknown(Json::Object(map)),
        "tool_use" => tool_use(map),
        "tool_result" => tool_result(map),
        "image" => media_block(map, Modality::Image),
        "document" => media_block(map, Modality::Other("document".to_owned())),
        _ => Part::Unknown(Json::Object(map)),
    }
}

fn text_block(map: Map<String, Json>) -> Part {
    let Some(content) = map.get("text").and_then(Json::as_str).map(str::to_owned) else {
        return Part::Unknown(Json::Object(map));
    };
    let mut extra = map;
    extra.shift_remove("type");
    extra.shift_remove("text");
    text_part_value(content, extra)
}

fn text_part_value(content: String, extra: Map<String, Json>) -> Part {
    Part::Text { content, extra }
}

fn thinking_block(map: Map<String, Json>) -> Part {
    let Some(content) = map
        .get("thinking")
        .and_then(Json::as_str)
        .map(str::to_owned)
    else {
        return Part::Unknown(Json::Object(map));
    };
    let mut extra = map;
    extra.shift_remove("type");
    extra.shift_remove("thinking");
    Part::Reasoning { content, extra }
}

fn tool_use(map: Map<String, Json>) -> Part {
    let Some(name) = map.get("name").and_then(Json::as_str).map(str::to_owned) else {
        return Part::Unknown(Json::Object(map));
    };
    let id = map.get("id").and_then(Json::as_str).map(str::to_owned);
    let arguments = map.get("input").cloned().map(parse_tool_arguments);
    let mut extra = map;
    extra.shift_remove("type");
    extra.shift_remove("name");
    if id.is_some() {
        extra.shift_remove("id");
    }
    extra.shift_remove("input");
    Part::ToolCall {
        id,
        name,
        arguments,
        extra,
    }
}

fn tool_result(map: Map<String, Json>) -> Part {
    let id = map
        .get("tool_use_id")
        .and_then(Json::as_str)
        .map(str::to_owned);
    let response = match map.get("content").cloned() {
        None | Some(Json::Null) => Json::Null,
        Some(Json::String(text)) => parse_tool_arguments(Json::String(text)),
        Some(Json::Array(items)) => {
            let parts: Vec<Part> = items.into_iter().map(block).collect();
            parts_json(&parts)
        }
        Some(other) => other,
    };
    let mut extra = map;
    extra.shift_remove("type");
    extra.shift_remove("content");
    if id.is_some() {
        extra.shift_remove("tool_use_id");
    }
    Part::ToolCallResponse {
        id,
        response,
        extra,
    }
}

fn media_block(map: Map<String, Json>, modality: Modality) -> Part {
    let Some(located) = media_source(map.get("source")) else {
        return Part::Unknown(Json::Object(map));
    };
    let mut extra = map;
    extra.shift_remove("type");
    if let Some(Json::Object(mut source)) = extra.shift_remove("source") {
        source.shift_remove("type");
        source.shift_remove("media_type");
        source.shift_remove("data");
        source.shift_remove("url");
        for (key, value) in source {
            extra.entry(key).or_insert(value);
        }
    }
    match located {
        Located::Blob { content, mime_type } => Part::Blob {
            modality: Some(modality),
            mime_type,
            content,
            extra,
        },
        Located::Uri { uri, mime_type } => Part::Uri {
            modality: Some(modality),
            mime_type,
            uri,
            extra,
        },
    }
}

fn media_source(value: Option<&Json>) -> Option<Located> {
    let source = value?.as_object()?;
    let mime_type = source
        .get("media_type")
        .and_then(Json::as_str)
        .map(str::to_owned);
    match source.get("type").and_then(Json::as_str) {
        Some("base64") => Some(Located::Blob {
            content: source.get("data")?.as_str()?.to_owned(),
            mime_type,
        }),
        Some("url") => Some(Located::Uri {
            uri: source.get("url")?.as_str()?.to_owned(),
            mime_type,
        }),
        _ => None,
    }
}

enum Located {
    Blob {
        content: String,
        mime_type: Option<String>,
    },
    Uri {
        uri: String,
        mime_type: Option<String>,
    },
}

fn parts_json(parts: &[Part]) -> Json {
    serde_json::to_value(parts).unwrap_or_else(|_| Json::Array(Vec::new()))
}

#[cfg(test)]
mod tests {
    use super::anthropic_messages;
    use crate::convert::ConvertError;
    use llmobs_core::message::messages_to_json;
    use serde_json::{Value as Json, json};

    fn messages(value: &Json) -> String {
        let (converted, _) = anthropic_messages(value, None).expect("messages");
        messages_to_json(&converted)
    }

    #[test]
    fn a_string_content_is_one_text_part() {
        assert_eq!(
            messages(&json!([{"role": "user", "content": "hi"}])),
            r#"[{"role":"user","parts":[{"type":"text","content":"hi"}]}]"#
        );
    }

    #[test]
    fn a_text_block_keeps_its_extra_fields() {
        assert_eq!(
            messages(&json!([{
                "role": "user",
                "content": [{"type": "text", "text": "see", "cache_control": {"type": "ephemeral"}}]
            }])),
            concat!(
                r#"[{"role":"user","parts":[{"type":"text","content":"see","#,
                r#""cache_control":{"type":"ephemeral"}}]}]"#
            )
        );
    }

    #[test]
    fn thinking_is_reasoning_and_redacted_thinking_stays_unknown() {
        assert_eq!(
            messages(&json!([{
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "because", "signature": "sig"},
                    {"type": "redacted_thinking", "data": "abc"}
                ]
            }])),
            concat!(
                r#"[{"role":"assistant","parts":["#,
                r#"{"type":"reasoning","content":"because","signature":"sig"},"#,
                r#"{"type":"redacted_thinking","data":"abc"}]}]"#
            )
        );
    }

    #[test]
    fn tool_use_takes_input_as_arguments() {
        assert_eq!(
            messages(&json!([{
                "role": "assistant",
                "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "get_weather", "input": {"city": "Paris"}},
                    {"type": "tool_use", "id": "toolu_2", "name": "lookup", "input": "{\"q\":1}"},
                    {"type": "tool_use", "id": "toolu_3", "name": "bare"}
                ]
            }])),
            concat!(
                r#"[{"role":"assistant","parts":["#,
                r#"{"type":"tool_call","id":"toolu_1","name":"get_weather","arguments":{"city":"Paris"}},"#,
                r#"{"type":"tool_call","id":"toolu_2","name":"lookup","arguments":{"q":1}},"#,
                r#"{"type":"tool_call","id":"toolu_3","name":"bare"}]}]"#
            )
        );
    }

    #[test]
    fn tool_result_content_is_a_string_or_blocks() {
        assert_eq!(
            messages(&json!([{
                "role": "user",
                "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "72F", "is_error": false},
                    {"type": "tool_result", "tool_use_id": "toolu_2", "content": "{\"temp\":18}"},
                    {
                        "type": "tool_result",
                        "tool_use_id": "toolu_3",
                        "content": [{"type": "text", "text": "done"}]
                    }
                ]
            }])),
            concat!(
                r#"[{"role":"user","parts":["#,
                r#"{"type":"tool_call_response","id":"toolu_1","response":"72F","is_error":false},"#,
                r#"{"type":"tool_call_response","id":"toolu_2","response":{"temp":18}},"#,
                r#"{"type":"tool_call_response","id":"toolu_3","response":[{"type":"text","content":"done"}]}"#,
                r#"]}]"#
            )
        );
    }

    #[test]
    fn image_and_document_sources_become_blobs_or_uris() {
        assert_eq!(
            messages(&json!([{
                "role": "user",
                "content": [
                    {
                        "type": "image",
                        "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"},
                        "cache_control": {"type": "ephemeral"}
                    },
                    {"type": "image", "source": {"type": "url", "url": "https://cdn.example/a.png"}},
                    {
                        "type": "document",
                        "title": "spec",
                        "source": {"type": "base64", "media_type": "application/pdf", "data": "JVBE"}
                    },
                    {"type": "document", "source": {"type": "url", "url": "https://example.com/a.pdf"}},
                    {"type": "image", "source": {"type": "file", "file_id": "file_1"}}
                ]
            }])),
            concat!(
                r#"[{"role":"user","parts":["#,
                r#"{"type":"blob","modality":"image","mime_type":"image/png","content":"AAAA","cache_control":{"type":"ephemeral"}},"#,
                r#"{"type":"uri","modality":"image","uri":"https://cdn.example/a.png"},"#,
                r#"{"type":"blob","modality":"document","mime_type":"application/pdf","content":"JVBE","title":"spec"},"#,
                r#"{"type":"uri","modality":"document","uri":"https://example.com/a.pdf"},"#,
                r#"{"type":"image","source":{"type":"file","file_id":"file_1"}}"#,
                r#"]}]"#
            )
        );
    }

    #[test]
    fn system_is_returned_separately() {
        let (converted, system) = anthropic_messages(
            &json!([{"role": "user", "content": "hi"}]),
            Some(&json!("be brief")),
        )
        .expect("messages");
        assert_eq!(
            messages_to_json(&converted),
            r#"[{"role":"user","parts":[{"type":"text","content":"hi"}]}]"#
        );
        assert_eq!(
            system,
            Some(json!([{"type": "text", "content": "be brief"}]))
        );

        let (_, blocks) = anthropic_messages(
            &json!([]),
            Some(
                &json!([{"type": "text", "text": "rules", "cache_control": {"type": "ephemeral"}}]),
            ),
        )
        .expect("empty messages");
        assert_eq!(
            blocks,
            Some(json!([{
                "type": "text",
                "content": "rules",
                "cache_control": {"type": "ephemeral"}
            }]))
        );

        let (_, absent) = anthropic_messages(&json!([]), None).expect("no system");
        assert_eq!(absent, None);
        let (_, null_system) = anthropic_messages(&json!([]), Some(&Json::Null)).expect("null");
        assert_eq!(null_system, None);
    }

    #[test]
    fn unknown_message_fields_and_unmodelled_blocks_stay() {
        assert_eq!(
            messages(&json!([{
                "role": "assistant",
                "content": [{"type": "text"}, 1],
                "name": "claude"
            }])),
            r#"[{"role":"assistant","parts":[{"type":"text"},1],"name":"claude"}]"#
        );
    }

    #[test]
    fn malformed_input_is_an_error_without_the_payload() {
        let secret = "do-not-echo";
        let not_array = anthropic_messages(&json!(secret), None).expect_err("not an array");
        assert_eq!(not_array, ConvertError::NotAnArray);
        assert_eq!(not_array.to_string(), "messages are not an array");
        assert!(!not_array.to_string().contains(secret));

        let bare = anthropic_messages(&json!({"role": "user", "content": secret}), None)
            .expect_err("bare object");
        assert_eq!(bare, ConvertError::NotAnArray);

        let missing = anthropic_messages(&json!([{"content": secret}]), None).expect_err("role");
        assert_eq!(missing, ConvertError::NotAMessage { index: 0 });
        assert_eq!(
            missing.to_string(),
            "message 0 is not an object with a string role"
        );
        assert!(!missing.to_string().contains(secret));
    }
}
