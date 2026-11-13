//! OpenInference flattened messages, rebuilt as OpenAI chat messages.
//!
//! Spec 02 § Input and output. `AttrBag::indexed` groups each numbered level.
//! The N10 converter turns the rebuilt array into canonical messages.

use serde_json::{Map, Value as Json};

use crate::bag::AttrBag;
use crate::conventions::openinference::captured_text;
use crate::value::as_str;

/// OpenAI-shaped messages from `prefix` (`llm.input_messages.` or `llm.output_messages.`).
///
/// `None` when that prefix has no decimal index. A role-`tool` message keeps
/// `tool_call_id` so N10 stores a `tool_call_response`.
pub(crate) fn flattened_messages(bag: &AttrBag<'_>, prefix: &str) -> Option<Vec<Json>> {
    let indexes: Vec<usize> = bag.indexed(prefix).into_keys().collect();
    if indexes.is_empty() {
        return None;
    }
    Some(
        indexes
            .into_iter()
            .map(|index| message(bag, prefix, index))
            .collect(),
    )
}

fn message(bag: &AttrBag<'_>, prefix: &str, index: usize) -> Json {
    let base = format!("{prefix}{index}.message.");
    let mut message = Map::new();
    // Gemini's `model` role reaches OpenInference unchanged; it is the assistant.
    let role = string_at(bag, &format!("{base}role")).map(|role| {
        if role == "model" {
            "assistant".to_owned()
        } else {
            role
        }
    });
    insert_string(&mut message, "role", role);
    insert_string(&mut message, "name", string_at(bag, &format!("{base}name")));
    insert_string(
        &mut message,
        "tool_call_id",
        string_at(bag, &format!("{base}tool_call_id")),
    );
    insert_string(
        &mut message,
        "finish_reason",
        string_at(bag, &format!("{base}finish_reason")),
    );
    let calls_prefix = format!("{base}tool_calls.");
    let mut calls: Vec<Json> = bag
        .indexed(&calls_prefix)
        .into_keys()
        .map(|call| tool_call(bag, &format!("{calls_prefix}{call}.tool_call.")))
        .collect();
    let contents_prefix = format!("{base}contents.");
    let content_indexes: Vec<usize> = bag.indexed(&contents_prefix).into_keys().collect();
    if content_indexes.is_empty() {
        insert_string(
            &mut message,
            "content",
            string_at(bag, &format!("{base}content")),
        );
    } else {
        let mut parts = Vec::new();
        for index in content_indexes {
            // The Anthropic instrumentation also lists each tool call as a
            // content (`contents.N.tool_call.*`), usually repeating `tool_calls`.
            let call_base = format!("{contents_prefix}{index}.tool_call.");
            if bag.keys_with_prefix(&call_base).next().is_some() {
                let call = tool_call(bag, &call_base);
                if !calls.iter().any(|known| same_call(known, &call)) {
                    calls.push(call);
                }
            } else if let Some(part) = content_part(bag, &contents_prefix, index) {
                parts.push(part);
            }
        }
        message.insert("content".to_owned(), Json::Array(parts));
    }
    if !calls.is_empty() {
        message.insert("tool_calls".to_owned(), Json::Array(calls));
    }
    Json::Object(message)
}

/// Two tool calls with the same id are one call; without ids, equal values are.
fn same_call(known: &Json, call: &Json) -> bool {
    match (known.get("id"), call.get("id")) {
        (Some(left), Some(right)) => left == right,
        _ => known == call,
    }
}

/// `None` for a text part with no text (empty or redacted).
fn content_part(bag: &AttrBag<'_>, prefix: &str, index: usize) -> Option<Json> {
    let base = format!("{prefix}{index}.message_content.");
    let kind = string_at(bag, &format!("{base}type"));
    let text = string_at(bag, &format!("{base}text"));
    let url = string_at(bag, &format!("{base}image.image.url"));
    if let Some(url) = url
        && kind.as_deref() != Some("text")
    {
        return Some(Json::Object(Map::from_iter([
            ("type".to_owned(), Json::String("image_url".to_owned())),
            (
                "image_url".to_owned(),
                Json::Object(Map::from_iter([("url".to_owned(), Json::String(url))])),
            ),
        ])));
    }
    let kind = kind.unwrap_or_else(|| "text".to_owned());
    if kind == "text" && text.is_none() {
        return None;
    }
    let mut part = Map::new();
    part.insert("type".to_owned(), Json::String(kind));
    insert_string(&mut part, "text", text);
    Some(Json::Object(part))
}

/// One OpenAI tool call from the keys under `base` (`….tool_call.`).
fn tool_call(bag: &AttrBag<'_>, base: &str) -> Json {
    let mut function = Map::new();
    insert_string(
        &mut function,
        "name",
        string_at(bag, &format!("{base}function.name")),
    );
    insert_string(
        &mut function,
        "arguments",
        string_at(bag, &format!("{base}function.arguments")),
    );
    let mut call = Map::new();
    call.insert("type".to_owned(), Json::String("function".to_owned()));
    insert_string(&mut call, "id", string_at(bag, &format!("{base}id")));
    if !function.is_empty() {
        call.insert("function".to_owned(), Json::Object(function));
    }
    Json::Object(call)
}

fn insert_string(object: &mut Map<String, Json>, key: &str, value: Option<String>) {
    if let Some(value) = value {
        object.insert(key.to_owned(), Json::String(value));
    }
}

/// Redacted content (`__REDACTED__`) reads as absent, like an empty string.
fn string_at(bag: &AttrBag<'_>, key: &str) -> Option<String> {
    let text = as_str(bag.get(key)?)?;
    captured_text(text).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue};

    use super::flattened_messages;
    use crate::bag::AttrBag;
    use crate::convert::openai_messages;
    use crate::warn::Warnings;
    use llmobs_core::message::messages_to_json;

    fn string_attr(key: &str, text: &str) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(AnyValue {
                value: Some(Value::StringValue(text.to_owned())),
            }),
            key_strindex: 0,
        }
    }

    #[test]
    fn a_tool_role_becomes_a_tool_call_response() {
        let attrs = [
            string_attr("llm.input_messages.0.message.role", "tool"),
            string_attr("llm.input_messages.0.message.tool_call_id", "call_1"),
            string_attr("llm.input_messages.0.message.content", "{\"temp\":18}"),
            string_attr("llm.input_messages.name.content", "skip"),
        ];
        let mut warnings = Warnings::default();
        let bag = AttrBag::new(&attrs, &mut warnings);
        let raw = flattened_messages(&bag, "llm.input_messages.").expect("messages");
        let messages = openai_messages(&serde_json::Value::Array(raw)).expect("converted");
        assert_eq!(
            messages_to_json(&messages),
            r#"[{"role":"tool","parts":[{"type":"tool_call_response","id":"call_1","response":{"temp":18}}]}]"#
        );
    }

    #[test]
    fn redacted_text_is_not_content() {
        let attrs = [
            string_attr("llm.input_messages.0.message.role", "system"),
            string_attr("llm.input_messages.0.message.content", "__REDACTED__"),
            string_attr("llm.input_messages.1.message.role", "user"),
            string_attr(
                "llm.input_messages.1.message.contents.0.message_content.type",
                "text",
            ),
            string_attr(
                "llm.input_messages.1.message.contents.0.message_content.text",
                "__REDACTED__",
            ),
            string_attr(
                "llm.input_messages.1.message.contents.1.message_content.text",
                "kept",
            ),
        ];
        let mut warnings = Warnings::default();
        let bag = AttrBag::new(&attrs, &mut warnings);
        let raw = flattened_messages(&bag, "llm.input_messages.").expect("messages");
        let messages = openai_messages(&serde_json::Value::Array(raw)).expect("converted");
        assert_eq!(
            messages_to_json(&messages),
            r#"[{"role":"system","parts":[]},{"role":"user","parts":[{"type":"text","content":"kept"}]}]"#
        );
    }

    #[test]
    fn contents_and_tool_calls_keep_index_order() {
        let attrs = [
            string_attr("llm.output_messages.0.message.role", "assistant"),
            string_attr("llm.output_messages.0.message.finish_reason", "stop"),
            string_attr(
                "llm.output_messages.0.message.contents.1.message_content.text",
                "second",
            ),
            string_attr(
                "llm.output_messages.0.message.contents.1.message_content.type",
                "text",
            ),
            string_attr(
                "llm.output_messages.0.message.contents.0.message_content.type",
                "image",
            ),
            string_attr(
                "llm.output_messages.0.message.contents.0.message_content.image.image.url",
                "https://example.com/cat.png",
            ),
            string_attr(
                "llm.output_messages.0.message.tool_calls.0.tool_call.function.name",
                "get_weather",
            ),
            string_attr(
                "llm.output_messages.0.message.tool_calls.0.tool_call.function.arguments",
                "{\"city\":\"Paris\"}",
            ),
            string_attr(
                "llm.output_messages.0.message.tool_calls.0.tool_call.id",
                "call_9",
            ),
        ];
        let mut warnings = Warnings::default();
        let bag = AttrBag::new(&attrs, &mut warnings);
        let raw = flattened_messages(&bag, "llm.output_messages.").expect("messages");
        let value = serde_json::Value::Array(raw);
        let mut messages = openai_messages(&value).expect("converted");
        let reason = value
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.get("finish_reason"))
            .and_then(|reason| reason.as_str())
            .map(str::to_owned);
        messages[0].finish_reason = reason;
        messages[0].extra.shift_remove("finish_reason");
        assert_eq!(
            messages_to_json(&messages),
            concat!(
                r#"[{"role":"assistant","parts":["#,
                r#"{"type":"uri","modality":"image","uri":"https://example.com/cat.png"},"#,
                r#"{"type":"text","content":"second"},"#,
                r#"{"type":"tool_call","id":"call_9","name":"get_weather","arguments":{"city":"Paris"}}"#,
                r#"],"finish_reason":"stop"}]"#
            )
        );
    }
    #[test]
    fn a_content_tool_call_is_kept_once_and_model_is_the_assistant() {
        let attrs = [
            string_attr("llm.output_messages.0.message.role", "model"),
            string_attr(
                "llm.output_messages.0.message.contents.0.message_content.type",
                "reasoning",
            ),
            string_attr(
                "llm.output_messages.0.message.contents.0.message_content.text",
                "think",
            ),
            string_attr(
                "llm.output_messages.0.message.contents.1.message_content.type",
                "tool_use",
            ),
            string_attr(
                "llm.output_messages.0.message.contents.1.tool_call.id",
                "call_1",
            ),
            string_attr(
                "llm.output_messages.0.message.contents.1.tool_call.function.name",
                "get_weather",
            ),
            string_attr(
                "llm.output_messages.0.message.contents.2.message_content.type",
                "tool_use",
            ),
            string_attr(
                "llm.output_messages.0.message.contents.2.tool_call.id",
                "call_2",
            ),
            string_attr(
                "llm.output_messages.0.message.contents.2.tool_call.function.name",
                "get_time",
            ),
            string_attr(
                "llm.output_messages.0.message.tool_calls.0.tool_call.id",
                "call_1",
            ),
            string_attr(
                "llm.output_messages.0.message.tool_calls.0.tool_call.function.name",
                "get_weather",
            ),
        ];
        let mut warnings = Warnings::default();
        let bag = AttrBag::new(&attrs, &mut warnings);
        let raw = flattened_messages(&bag, "llm.output_messages.").expect("messages");
        let messages = openai_messages(&serde_json::Value::Array(raw)).expect("converted");
        assert_eq!(
            messages_to_json(&messages),
            concat!(
                r#"[{"role":"assistant","parts":["#,
                r#"{"type":"reasoning","content":"think"},"#,
                r#"{"type":"tool_call","id":"call_1","name":"get_weather"},"#,
                r#"{"type":"tool_call","id":"call_2","name":"get_time"}"#,
                r#"]}]"#
            )
        );
    }
}
