//! Text extracted from a check variable.
//!
//! Spec 06 § `check` defaults (D164). A string is kept. A messages array yields
//! the text parts of its last assistant message. Anything else is compact JSON.

use llmobs_core::message::{Message, Part, Role};
use serde_json::Value;

/// Parses `value` as a messages array.
///
/// Returns [`None`] when any element is not a [`Message`]. Later rendering uses
/// the same parse.
pub(crate) fn is_messages(value: &Value) -> Option<Vec<Message>> {
    serde_json::from_value(value.clone()).ok()
}

/// The text a check compares.
///
/// A JSON string is returned as itself, not as a quoted JSON string. A messages
/// array yields the text parts of the last `assistant` message, joined with
/// newlines, or an empty string when that message has no text part. Every other
/// value is compact JSON.
pub(crate) fn extract_text(value: &Value) -> String {
    if let Value::String(text) = value {
        return text.clone();
    }
    if let Some(messages) = is_messages(value) {
        return last_assistant_text(&messages);
    }
    // `Value` contains only JSON, so compact serialization cannot fail.
    serde_json::to_string(value).unwrap_or_default()
}

fn last_assistant_text(messages: &[Message]) -> String {
    let Some(message) = messages
        .iter()
        .rev()
        .find(|message| message.role == Role::Assistant)
    else {
        return String::new();
    };
    message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { content, .. } => Some(content.as_str()),
            Part::Reasoning { .. }
            | Part::ToolCall { .. }
            | Part::ToolCallResponse { .. }
            | Part::Uri { .. }
            | Part::Blob { .. }
            | Part::Unknown(_) => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::extract_text;

    #[test]
    fn extracts_text_by_shape() {
        assert_eq!(extract_text(&json!("hello")), "hello");
        assert_eq!(
            extract_text(&json!(
                r#"[{"role":"assistant","parts":[{"type":"text","content":"hidden"}]}]"#
            )),
            r#"[{"role":"assistant","parts":[{"type":"text","content":"hidden"}]}]"#
        );
        assert_eq!(extract_text(&json!(12)), "12");
        assert_eq!(extract_text(&json!({"b": 1, "a": 2})), r#"{"b":1,"a":2}"#);
        assert_eq!(extract_text(&json!([1, 2])), "[1,2]");

        let two_assistants = json!([
            {"role": "assistant", "parts": [{"type": "text", "content": "first"}]},
            {"role": "user", "parts": [{"type": "text", "content": "q"}]},
            {
                "role": "assistant",
                "parts": [
                    {"type": "text", "content": "second"},
                    {"type": "reasoning", "content": "scratch"},
                    {"type": "text", "content": "more"}
                ]
            },
            {"role": "user", "parts": [{"type": "text", "content": "tail"}]}
        ]);
        assert_eq!(extract_text(&two_assistants), "second\nmore");

        let no_assistant = json!([
            {"role": "user", "parts": [{"type": "text", "content": "hi"}]}
        ]);
        assert_eq!(extract_text(&no_assistant), "");

        let tool_only = json!([
            {"role": "assistant", "parts": [{"type": "tool_call", "name": "lookup"}]}
        ]);
        assert_eq!(extract_text(&tool_only), "");
    }
}
