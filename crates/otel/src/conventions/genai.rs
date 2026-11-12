//! GenAI messages, system instructions, tool definitions, and legacy events.
//!
//! Spec 02 § Input and output, the GenAI and GenAI legacy events rows, and
//! rule 16. Tool-call arguments and results are read only on tool spans.

use llmobs_core::enums::SpanKind;
use llmobs_core::message::Message;
use opentelemetry_proto::tonic::common::v1::AnyValue;
use opentelemetry_proto::tonic::trace::v1::span::Event;
use serde_json::{Map, Value as Json};

use super::{messages_from_value, present_json};
use crate::convert::openai_messages;
use crate::payload::{
    Field, Io, KeyPattern, Payload, ReadContext, decode_json_text, non_message_payload,
    tool_definitions,
};
use crate::value::as_str;
use crate::warn::Warnings;

const INPUT_MESSAGES: &str = "gen_ai.input.messages";
const OUTPUT_MESSAGES: &str = "gen_ai.output.messages";
const SYSTEM: &str = "gen_ai.system_instructions";
const TOOLS: &str = "gen_ai.tool.definitions";
const TOOL_ARGUMENTS: &str = "gen_ai.tool.call.arguments";
const TOOL_RESULT: &str = "gen_ai.tool.call.result";
const FINISH_REASONS: &str = "gen_ai.response.finish_reasons";
const DETAILS: &str = "gen_ai.client.inference.operation.details";

const INPUT_EVENTS: &[(&str, &str)] = &[
    ("gen_ai.system.message", "system"),
    ("gen_ai.user.message", "user"),
    ("gen_ai.assistant.message", "assistant"),
    ("gen_ai.tool.message", "tool"),
];

pub(crate) const PAYLOAD_KEYS: &[(Field, KeyPattern)] = &[
    (Field::Input, KeyPattern::Exact(INPUT_MESSAGES)),
    (Field::Input, KeyPattern::Exact(TOOL_ARGUMENTS)),
    (Field::Input, KeyPattern::Event(DETAILS)),
    (Field::Input, KeyPattern::Event("gen_ai.system.message")),
    (Field::Input, KeyPattern::Event("gen_ai.user.message")),
    (Field::Input, KeyPattern::Event("gen_ai.assistant.message")),
    (Field::Input, KeyPattern::Event("gen_ai.tool.message")),
    (Field::Output, KeyPattern::Exact(OUTPUT_MESSAGES)),
    (Field::Output, KeyPattern::Exact(TOOL_RESULT)),
    (Field::Output, KeyPattern::Event(DETAILS)),
    (Field::Output, KeyPattern::Event("gen_ai.choice")),
    (Field::SystemInstructions, KeyPattern::Exact(SYSTEM)),
    (Field::SystemInstructions, KeyPattern::Event(DETAILS)),
    (Field::ToolDefinitions, KeyPattern::Exact(TOOLS)),
];

pub(crate) fn read(ctx: &mut ReadContext<'_, '_>) -> Io {
    let mut io = Io::default();
    read_messages(ctx, &mut io, INPUT_MESSAGES, "messages:genai:input", true);
    read_messages(
        ctx,
        &mut io,
        OUTPUT_MESSAGES,
        "messages:genai:output",
        false,
    );
    if io.system_instructions.is_none() {
        io.system_instructions = instruction(ctx, SYSTEM);
    }
    io.tool_definitions = definitions(ctx);
    fill_finish_reasons(ctx, &mut io);
    io.events = payload_events(ctx.events);
    io
}

fn read_messages(
    ctx: &mut ReadContext<'_, '_>,
    io: &mut Io,
    key: &str,
    error: &'static str,
    input: bool,
) {
    let found = if let Some(found) = span_messages(ctx, key) {
        Some(found)
    } else if let Some(found) = details_messages(ctx, key) {
        Some(found)
    } else if let Some(found) = legacy_messages(ctx.events, ctx.warnings, input) {
        Some(found)
    } else if ctx.kind == SpanKind::Tool {
        tool_payload(ctx, input)
    } else {
        None
    };
    let Some((payload, failed)) = found else {
        return;
    };
    if input {
        io.input = Some(payload);
    } else {
        io.output = Some(payload);
    }
    if failed {
        io.errors.push(error);
    }
}

fn span_messages(ctx: &mut ReadContext<'_, '_>, key: &str) -> Option<(Payload, bool)> {
    if as_str(ctx.bag.get(key)?) == Some("") {
        return None;
    }
    let value = ctx.bag.take_payload(key)?;
    Some(genai_messages(value, ctx.warnings))
}

fn details_messages(ctx: &mut ReadContext<'_, '_>, key: &str) -> Option<(Payload, bool)> {
    let value = details_attr(ctx.events, key)?.clone();
    Some(genai_messages(&value, ctx.warnings))
}

/// Canonical messages, or OpenAI chat messages when no element has `parts`.
///
/// Laminar writes OpenAI request messages and response `choices` under the
/// GenAI keys (D-B1-3). A choice is `{message, finish_reason}`.
fn genai_messages(value: &AnyValue, warnings: &mut Warnings) -> (Payload, bool) {
    if let Some(json) = decode_json_text(value, warnings)
        && let Some(items) = openai_shaped(&json)
    {
        let mut reasons = Vec::with_capacity(items.len());
        let mut raw = Vec::with_capacity(items.len());
        for item in items {
            match item.get("message") {
                Some(Json::Object(message)) => {
                    // A response message lists every optional field, as null.
                    let present = message
                        .iter()
                        .filter(|(_, value)| !value.is_null())
                        .map(|(key, value)| (key.clone(), value.clone()));
                    raw.push(Json::Object(present.collect()));
                    reasons.push(item.get("finish_reason").and_then(Json::as_str));
                }
                _ => {
                    raw.push(item.clone());
                    reasons.push(None);
                }
            }
        }
        if let Ok(mut messages) = openai_messages(&Json::Array(raw)) {
            for (message, reason) in messages.iter_mut().zip(reasons) {
                if let Some(reason) = reason {
                    message.finish_reason = Some(reason.to_owned());
                }
            }
            return (Payload::Messages(messages), false);
        }
    }
    messages_from_value(value, warnings)
}

/// The elements of a non-empty array of objects where none has `parts` and
/// each has a string `role` or an object `message`.
fn openai_shaped(json: &Json) -> Option<&Vec<Json>> {
    let items = json.as_array()?;
    let shaped = !items.is_empty()
        && items.iter().all(|item| {
            item.as_object().is_some_and(|object| {
                !object.contains_key("parts")
                    && (object.get("role").is_some_and(Json::is_string)
                        || object.get("message").is_some_and(Json::is_object))
            })
        });
    shaped.then_some(items)
}

fn instruction(ctx: &mut ReadContext<'_, '_>, key: &str) -> Option<Json> {
    if let Some(value) = ctx.bag.get(key)
        && as_str(value) != Some("")
    {
        let value = value.clone();
        ctx.bag.take_payload(key);
        return present_json(&value, ctx.warnings);
    }
    let value = details_attr(ctx.events, key)?.clone();
    present_json(&value, ctx.warnings)
}

fn definitions(ctx: &mut ReadContext<'_, '_>) -> Option<Vec<Json>> {
    let value = ctx.bag.get(TOOLS)?.clone();
    let Json::Array(items) = present_json(&value, ctx.warnings)? else {
        return None;
    };
    let items = tool_definitions(items);
    if items.is_empty() {
        None
    } else {
        ctx.bag.take_payload(TOOLS);
        Some(items)
    }
}

fn tool_payload(ctx: &mut ReadContext<'_, '_>, input: bool) -> Option<(Payload, bool)> {
    let key = if input { TOOL_ARGUMENTS } else { TOOL_RESULT };
    if as_str(ctx.bag.get(key)?) == Some("") {
        return None;
    }
    let value = ctx.bag.take_payload(key)?;
    Some((non_message_payload(value, ctx.warnings), false))
}

fn legacy_messages(
    events: &[Event],
    warnings: &mut Warnings,
    input: bool,
) -> Option<(Payload, bool)> {
    let mut messages = Vec::new();
    for event in events {
        let Some(role) = legacy_role(event.name.as_str(), input) else {
            continue;
        };
        messages.push(event_message(event, role, warnings));
    }
    if messages.is_empty() {
        return None;
    }
    let raw = Json::Array(messages);
    match openai_messages(&raw) {
        Ok(converted) => Some((Payload::Messages(converted), false)),
        Err(_) => Some((Payload::Json(raw), true)),
    }
}

fn legacy_role(name: &str, input: bool) -> Option<&'static str> {
    if input {
        event_role(name)
    } else if name == "gen_ai.choice" {
        Some("assistant")
    } else {
        None
    }
}

fn event_role(name: &str) -> Option<&'static str> {
    INPUT_EVENTS
        .iter()
        .find(|(event, _)| *event == name)
        .map(|(_, role)| *role)
}

fn event_message(event: &Event, role: &str, warnings: &mut Warnings) -> Json {
    let mut object = Map::new();
    for attribute in &event.attributes {
        let Some(value) = attribute.value.as_ref() else {
            continue;
        };
        if let Some(json) = present_json(value, warnings) {
            object.insert(attribute.key.clone(), json);
        }
    }
    if object.get("role").and_then(Json::as_str).is_none() {
        object.insert("role".to_owned(), Json::String(role.to_owned()));
    }
    Json::Object(object)
}

fn fill_finish_reasons(ctx: &mut ReadContext<'_, '_>, io: &mut Io) {
    if !matches!(io.output, Some(Payload::Messages(_))) {
        return;
    }
    let Some(value) = ctx.bag.get(FINISH_REASONS).cloned() else {
        return;
    };
    let Some(reasons) = reason_list(&value, ctx.warnings) else {
        return;
    };
    ctx.bag.take(FINISH_REASONS);
    let Some(Payload::Messages(messages)) = io.output.as_mut() else {
        return;
    };
    apply_reasons(messages, &reasons);
}

fn apply_reasons(messages: &mut [Message], reasons: &[Option<String>]) {
    for (message, reason) in messages.iter_mut().zip(reasons) {
        if message.finish_reason.is_none() {
            message.finish_reason.clone_from(reason);
        }
    }
}

fn reason_list(value: &AnyValue, warnings: &mut Warnings) -> Option<Vec<Option<String>>> {
    let Json::Array(items) = present_json(value, warnings)? else {
        return None;
    };
    Some(
        items
            .into_iter()
            .map(|item| match item {
                Json::String(text) if !text.is_empty() => Some(text),
                _ => None,
            })
            .collect(),
    )
}

fn payload_events(events: &[Event]) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, event)| {
            event.name == DETAILS
                || event.name == "gen_ai.choice"
                || event_role(event.name.as_str()).is_some()
        })
        .map(|(index, _)| index)
        .collect()
}

fn details_attr<'a>(events: &'a [Event], key: &str) -> Option<&'a AnyValue> {
    let event = events.iter().find(|event| event.name == DETAILS)?;
    event
        .attributes
        .iter()
        .find(|attribute| attribute.key == key)?
        .value
        .as_ref()
}

#[cfg(test)]
mod tests {
    use super::read;
    use crate::bag::AttrBag;
    use crate::payload::{Payload, ReadContext};
    use crate::warn::Warnings;
    use llmobs_core::enums::SpanKind;
    use llmobs_core::message::messages_to_json;
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue};
    use opentelemetry_proto::tonic::trace::v1::span::Event;
    use serde_json::json;

    fn string_attr(key: &str, text: &str) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(AnyValue {
                value: Some(Value::StringValue(text.to_owned())),
            }),
            key_strindex: 0,
        }
    }

    fn read_span(attrs: &[KeyValue], events: &[Event], kind: SpanKind) -> crate::payload::Io {
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(attrs, &mut warnings);
        read(&mut ReadContext {
            bag: &mut bag,
            events,
            kind,
            warnings: &mut warnings,
        })
    }

    #[test]
    fn finish_reasons_fill_only_missing_ones() {
        let messages = r#"[{"role":"assistant","parts":[{"type":"text","content":"one"}],"finish_reason":"stop"},{"role":"assistant","parts":[{"type":"text","content":"two"}]}]"#;
        let io = read_span(
            &[
                string_attr("gen_ai.output.messages", messages),
                string_attr(
                    "gen_ai.response.finish_reasons",
                    r#"["length","tool_calls"]"#,
                ),
            ],
            &[],
            SpanKind::Llm,
        );
        let Payload::Messages(messages) = io.output.expect("output") else {
            panic!("expected messages");
        };
        assert_eq!(
            messages_to_json(&messages),
            concat!(
                r#"[{"role":"assistant","parts":[{"type":"text","content":"one"}],"finish_reason":"stop"},"#,
                r#"{"role":"assistant","parts":[{"type":"text","content":"two"}],"finish_reason":"tool_calls"}]"#
            )
        );
    }

    #[test]
    fn tool_definitions_parse_json_strings() {
        let io = read_span(
            &[string_attr(
                "gen_ai.tool.definitions",
                r#"["{\"name\":\"lookup\"}",{"name":"search"}]"#,
            )],
            &[],
            SpanKind::Llm,
        );
        assert_eq!(
            io.tool_definitions,
            Some(vec![json!({"name": "lookup"}), json!({"name": "search"})])
        );
    }
}
