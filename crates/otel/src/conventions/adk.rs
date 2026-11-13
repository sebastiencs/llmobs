//! Google ADK request, response, and tool payloads.
//!
//! Spec 02 § Input and output. `gcp.vertex.agent.llm_request` is a Gemini
//! `GenerateContentRequest`: `contents` become messages, `config.system_instruction`
//! becomes system instructions, and `config.tools` become tool definitions.
//! `gcp.vertex.agent.llm_response` is a `GenerateContentResponse`: `content` is
//! one message, and `finish_reason` is copied onto it. On a tool span,
//! `tool_call_args` and `tool_response` are JSON.

use llmobs_core::enums::SpanKind;
use llmobs_core::message::Message;
use serde_json::{Map, Value as Json};

use crate::convert::{gemini_contents, gemini_system_instruction};
use crate::payload::{
    Field, Io, KeyPattern, Payload, ReadContext, decode_json_text, non_message_payload,
    parse_tool_arguments,
};
use crate::value::as_str;

const REQUEST: &str = "gcp.vertex.agent.llm_request";
const RESPONSE: &str = "gcp.vertex.agent.llm_response";
const TOOL_ARGS: &str = "gcp.vertex.agent.tool_call_args";
const TOOL_RESPONSE: &str = "gcp.vertex.agent.tool_response";

pub(crate) const PAYLOAD_KEYS: &[(Field, KeyPattern)] = &[
    (Field::Input, KeyPattern::Exact(REQUEST)),
    (Field::Input, KeyPattern::Exact(TOOL_ARGS)),
    (Field::Output, KeyPattern::Exact(RESPONSE)),
    (Field::Output, KeyPattern::Exact(TOOL_RESPONSE)),
    (Field::SystemInstructions, KeyPattern::Exact(REQUEST)),
    (Field::ToolDefinitions, KeyPattern::Exact(REQUEST)),
];

pub(crate) fn read(ctx: &mut ReadContext<'_, '_>) -> Io {
    let mut system_instructions = None;
    let mut tool_definitions = None;
    let mut errors = Vec::new();
    let tool = ctx.kind == SpanKind::Tool;
    let input = if tool {
        tool_payload(ctx, TOOL_ARGS).or_else(|| {
            request_input(
                ctx,
                &mut system_instructions,
                &mut tool_definitions,
                &mut errors,
            )
        })
    } else {
        request_input(
            ctx,
            &mut system_instructions,
            &mut tool_definitions,
            &mut errors,
        )
        .or_else(|| tool_payload(ctx, TOOL_ARGS))
    };
    let output = if tool {
        tool_payload(ctx, TOOL_RESPONSE).or_else(|| response_output(ctx, &mut errors))
    } else {
        response_output(ctx, &mut errors).or_else(|| tool_payload(ctx, TOOL_RESPONSE))
    };
    Io {
        input,
        output,
        system_instructions,
        tool_definitions,
        errors,
        ..Io::default()
    }
}

/// Messages from `contents`. An object without `contents` is left unread.
///
/// A tool span's `{}` placeholder therefore does not hide `tool_call_args`.
fn request_input(
    ctx: &mut ReadContext<'_, '_>,
    system_instructions: &mut Option<Json>,
    tool_definitions: &mut Option<Vec<Json>>,
    errors: &mut Vec<&'static str>,
) -> Option<Payload> {
    let json = object_attribute(ctx, REQUEST)?;
    let contents = json.get("contents")?;
    let payload = match gemini_contents(contents) {
        Ok(messages) => Payload::Messages(messages),
        Err(_) => {
            errors.push("messages:google_adk:input");
            Payload::Json(json.clone())
        }
    };
    *system_instructions = json
        .pointer("/config/system_instruction")
        .or_else(|| json.pointer("/config/systemInstruction"))
        .map(gemini_system_instruction);
    *tool_definitions = match json.get("config").and_then(|config| config.get("tools")) {
        Some(value) => match tool_list(value) {
            Ok(items) => items,
            Err(()) => {
                errors.push("messages:google_adk:tool_definitions");
                Some(vec![value.clone()])
            }
        },
        None => None,
    };
    ctx.bag.take_payload(REQUEST);
    Some(payload)
}

fn response_output(
    ctx: &mut ReadContext<'_, '_>,
    errors: &mut Vec<&'static str>,
) -> Option<Payload> {
    let json = object_attribute(ctx, RESPONSE)?;
    let content = json.get("content")?;
    let wrapped = Json::Array(vec![content.clone()]);
    let payload = match gemini_contents(&wrapped) {
        Ok(mut messages) => {
            if let Some(reason) = json
                .get("finish_reason")
                .or_else(|| json.get("finishReason"))
                .and_then(Json::as_str)
                .filter(|text| !text.is_empty())
            {
                apply_finish_reason(&mut messages, reason);
            }
            Payload::Messages(messages)
        }
        Err(_) => {
            errors.push("messages:google_adk:output");
            Payload::Json(json)
        }
    };
    ctx.bag.take_payload(RESPONSE);
    Some(payload)
}

fn apply_finish_reason(messages: &mut [Message], reason: &str) {
    for message in messages {
        if message.finish_reason.is_none() {
            message.finish_reason = Some(reason.to_owned());
        }
    }
}

fn object_attribute(ctx: &mut ReadContext<'_, '_>, key: &str) -> Option<Json> {
    let value = ctx.bag.get(key)?;
    if as_str(value) == Some("") {
        return None;
    }
    decode_json_text(value, ctx.warnings).filter(|json| json.is_object())
}

fn tool_payload(ctx: &mut ReadContext<'_, '_>, key: &str) -> Option<Payload> {
    let payload = {
        let value = ctx.bag.get(key)?;
        if as_str(value) == Some("") {
            return None;
        }
        non_message_payload(value, ctx.warnings)
    };
    ctx.bag.take_payload(key);
    Some(payload)
}

fn tool_list(value: &Json) -> Result<Option<Vec<Json>>, ()> {
    let Json::Array(items) = value else {
        return Err(());
    };
    if items.is_empty() {
        return Ok(None);
    }
    let mut tools = Vec::new();
    for item in items {
        if let Some(declarations) = item
            .get("function_declarations")
            .or_else(|| item.get("functionDeclarations"))
            .and_then(Json::as_array)
        {
            for declaration in declarations {
                tools.push(function_tool(declaration));
            }
        } else {
            tools.push(item.clone());
        }
    }
    if tools.is_empty() {
        Ok(None)
    } else {
        Ok(Some(tools))
    }
}

fn function_tool(value: &Json) -> Json {
    let Some(map) = value.as_object() else {
        return value.clone();
    };
    let Some(name) = map.get("name").and_then(Json::as_str) else {
        return value.clone();
    };
    let mut tool = Map::new();
    tool.insert("type".to_owned(), Json::String("function".to_owned()));
    tool.insert("name".to_owned(), Json::String(name.to_owned()));
    if let Some(description) = map.get("description").and_then(Json::as_str) {
        tool.insert(
            "description".to_owned(),
            Json::String(description.to_owned()),
        );
    }
    // google-genai ≥ 1.x writes the JSON Schema form as `parameters_json_schema`.
    if let Some(parameters) = map
        .get("parameters")
        .or_else(|| map.get("parameters_json_schema"))
    {
        tool.insert(
            "parameters".to_owned(),
            parse_tool_arguments(parameters.clone()),
        );
    }
    Json::Object(tool)
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

    fn string_attr(key: &str, text: &str) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(AnyValue {
                value: Some(Value::StringValue(text.to_owned())),
            }),
            key_strindex: 0,
        }
    }

    fn read_span(attrs: &[KeyValue], kind: SpanKind) -> crate::payload::Io {
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(attrs, &mut warnings);
        read(&mut ReadContext {
            bag: &mut bag,
            events: &[],
            kind,
            warnings: &mut warnings,
        })
    }

    #[test]
    fn a_request_and_response_become_messages_with_tools_and_a_finish_reason() {
        let request = r#"{"contents":[{"role":"user","parts":[{"text":"Hello"}]}],"config":{"system_instruction":"Be brief.","tools":[{"function_declarations":[{"name":"lookup","description":"Look up a city.","parameters":{"type":"OBJECT"}}]}]}}"#;
        let response =
            r#"{"content":{"role":"model","parts":[{"text":"Hi"}]},"finish_reason":"STOP"}"#;
        let io = read_span(
            &[
                string_attr("gcp.vertex.agent.llm_request", request),
                string_attr("gcp.vertex.agent.llm_response", response),
            ],
            SpanKind::Llm,
        );
        let Payload::Messages(messages) = io.input.expect("input") else {
            panic!("input is messages");
        };
        assert_eq!(
            messages_to_json(&messages),
            r#"[{"role":"user","parts":[{"type":"text","content":"Hello"}]}]"#
        );
        let Payload::Messages(messages) = io.output.expect("output") else {
            panic!("output is messages");
        };
        assert_eq!(
            messages_to_json(&messages),
            r#"[{"role":"assistant","parts":[{"type":"text","content":"Hi"}],"finish_reason":"STOP"}]"#
        );
        assert!(io.system_instructions.is_some());
        assert_eq!(io.tool_definitions.as_ref().map(Vec::len), Some(1));
        assert!(io.errors.is_empty());
    }

    #[test]
    fn a_tool_span_keeps_json_args_and_leaves_the_empty_request() {
        let io = read_span(
            &[
                string_attr("gcp.vertex.agent.llm_request", "{}"),
                string_attr("gcp.vertex.agent.llm_response", "{}"),
                string_attr("gcp.vertex.agent.tool_call_args", r#"{"city":"Paris"}"#),
                string_attr("gcp.vertex.agent.tool_response", r#"{"ok":true}"#),
            ],
            SpanKind::Tool,
        );
        assert_eq!(
            io.input,
            Some(Payload::Json(serde_json::json!({"city": "Paris"})))
        );
        assert_eq!(
            io.output,
            Some(Payload::Json(serde_json::json!({"ok": true})))
        );
    }
}
