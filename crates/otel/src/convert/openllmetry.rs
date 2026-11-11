//! OpenLLMetry flattened prompts, rebuilt as OpenAI chat messages.
//!
//! Spec 02 § Input and output. `gen_ai.prompt.N.*` and `gen_ai.completion.N.*`
//! become one object per index. The N10 converter turns that array into
//! canonical messages.

use serde_json::{Map, Value as Json};

use crate::bag::AttrBag;
use crate::value::as_str;

/// OpenAI-shaped messages from `prefix` (`gen_ai.prompt.` or `gen_ai.completion.`).
///
/// `None` when that prefix has no decimal index. `content` that starts with `[`
/// and parses as a JSON array is stored as that array so N10 maps content parts.
/// Anything else stays a string. Tool-call `arguments` stay a string for N10.
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
    let base = format!("{prefix}{index}.");
    let mut message = Map::new();
    insert_string(&mut message, "role", string_at(bag, &format!("{base}role")));
    if let Some(content) = string_at(bag, &format!("{base}content")) {
        message.insert("content".to_owned(), content_value(&content));
    }
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
    if let Some(calls) = tool_calls(bag, &format!("{base}tool_calls.")) {
        message.insert("tool_calls".to_owned(), Json::Array(calls));
    }
    Json::Object(message)
}

fn content_value(text: &str) -> Json {
    if text.starts_with('[')
        && let Ok(Json::Array(parts)) = serde_json::from_str(text)
    {
        return Json::Array(parts);
    }
    Json::String(text.to_owned())
}

fn tool_calls(bag: &AttrBag<'_>, prefix: &str) -> Option<Vec<Json>> {
    let indexes: Vec<usize> = bag.indexed(prefix).into_keys().collect();
    let calls: Vec<Json> = indexes
        .into_iter()
        .filter_map(|index| tool_call(bag, prefix, index))
        .collect();
    if calls.is_empty() { None } else { Some(calls) }
}

fn tool_call(bag: &AttrBag<'_>, prefix: &str, index: usize) -> Option<Json> {
    let base = format!("{prefix}{index}.");
    let id = string_at(bag, &format!("{base}id"));
    let name = string_at(bag, &format!("{base}name"));
    let arguments = string_at(bag, &format!("{base}arguments"));
    if id.is_none() && name.is_none() && arguments.is_none() {
        return None;
    }
    let mut function = Map::new();
    insert_string(&mut function, "name", name);
    insert_string(&mut function, "arguments", arguments);
    let mut call = Map::new();
    insert_string(&mut call, "id", id);
    call.insert("type".to_owned(), Json::String("function".to_owned()));
    call.insert("function".to_owned(), Json::Object(function));
    Some(Json::Object(call))
}

fn insert_string(map: &mut Map<String, Json>, key: &str, value: Option<String>) {
    if let Some(value) = value {
        map.insert(key.to_owned(), Json::String(value));
    }
}

fn string_at(bag: &AttrBag<'_>, key: &str) -> Option<String> {
    let text = as_str(bag.get(key)?)?;
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::flattened_messages;
    use crate::bag::AttrBag;
    use crate::convert::openai_messages;
    use crate::warn::Warnings;
    use llmobs_core::message::messages_to_json;
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue};

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
    fn content_parts_tool_calls_and_tool_results_follow_index_order() {
        let attrs = [
            string_attr("gen_ai.prompt.1.role", "user"),
            string_attr(
                "gen_ai.prompt.1.content",
                r#"[{"type":"text","text":"What is the capital?"}]"#,
            ),
            string_attr("gen_ai.prompt.0.role", "system"),
            string_attr("gen_ai.prompt.0.content", "You are concise."),
            string_attr("gen_ai.prompt.2.role", "assistant"),
            string_attr("gen_ai.prompt.2.tool_calls.0.id", "call_1"),
            string_attr("gen_ai.prompt.2.tool_calls.0.name", "lookup"),
            string_attr(
                "gen_ai.prompt.2.tool_calls.0.arguments",
                r#"{"city":"Paris"}"#,
            ),
            string_attr("gen_ai.prompt.3.role", "tool"),
            string_attr("gen_ai.prompt.3.tool_call_id", "call_1"),
            string_attr("gen_ai.prompt.3.content", "sunny"),
            string_attr("gen_ai.prompt.name", "skip"),
        ];
        let mut warnings = Warnings::default();
        let bag = AttrBag::new(&attrs, &mut warnings);
        let raw = flattened_messages(&bag, "gen_ai.prompt.").expect("messages");
        let messages = openai_messages(&serde_json::Value::Array(raw)).expect("converted");
        assert_eq!(
            messages_to_json(&messages),
            concat!(
                r#"[{"role":"system","parts":[{"type":"text","content":"You are concise."}]},"#,
                r#"{"role":"user","parts":[{"type":"text","content":"What is the capital?"}]},"#,
                r#"{"role":"assistant","parts":[{"type":"tool_call","id":"call_1","name":"lookup","arguments":{"city":"Paris"}}]},"#,
                r#"{"role":"tool","parts":[{"type":"tool_call_response","id":"call_1","response":"sunny"}]}]"#
            )
        );
    }

    #[test]
    fn a_bracket_that_is_not_json_stays_text() {
        let attrs = [
            string_attr("gen_ai.prompt.0.role", "user"),
            string_attr("gen_ai.prompt.0.content", "[not json"),
        ];
        let mut warnings = Warnings::default();
        let bag = AttrBag::new(&attrs, &mut warnings);
        let raw = flattened_messages(&bag, "gen_ai.prompt.").expect("messages");
        let messages = openai_messages(&serde_json::Value::Array(raw)).expect("converted");
        assert_eq!(
            messages_to_json(&messages),
            r#"[{"role":"user","parts":[{"type":"text","content":"[not json"}]}]"#
        );
    }
}
