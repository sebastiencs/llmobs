//! Payload readers in convention precedence order.
//!
//! Spec 02 § Input and output. llmobs and GenAI are N13. OpenInference is N18.
//! Vercel is N19. OpenLLMetry is N20. Langfuse and Laminar are N21.
//! Google ADK is N22.

mod adk;
mod genai;
mod laminar;
mod langfuse;
mod llmobs;
mod openinference;
mod openllmetry;
mod vercel;

use llmobs_core::enums::Convention;
use llmobs_core::message::{Message, Part};
use opentelemetry_proto::tonic::common::v1::AnyValue;
use serde_json::Value as Json;

pub(crate) use langfuse::apply_status;

use crate::payload::{
    ConventionPayload, Payload, PayloadKeys, decode_json_text, parse_tool_arguments,
};
use crate::value::{as_str, to_json};
use crate::warn::Warnings;

/// llmobs, then GenAI, then OpenInference, then Vercel, then OpenLLMetry,
/// then Langfuse, then Laminar, then Google ADK.
pub(crate) const READERS: &[ConventionPayload] = &[
    ConventionPayload {
        convention: Convention::Llmobs,
        read: llmobs::read,
    },
    ConventionPayload {
        convention: Convention::Genai,
        read: genai::read,
    },
    ConventionPayload {
        convention: Convention::Openinference,
        read: openinference::read,
    },
    ConventionPayload {
        convention: Convention::Vercel,
        read: vercel::read,
    },
    ConventionPayload {
        convention: Convention::Openllmetry,
        read: openllmetry::read,
    },
    ConventionPayload {
        convention: Convention::Langfuse,
        read: langfuse::read,
    },
    ConventionPayload {
        convention: Convention::Laminar,
        read: laminar::read,
    },
    ConventionPayload {
        convention: Convention::GoogleAdk,
        read: adk::read,
    },
];

/// Payload keys removed when a higher-precedence convention wins the field.
pub(crate) const REGISTRIES: &[PayloadKeys] = &[
    PayloadKeys {
        convention: Convention::Llmobs,
        keys: llmobs::PAYLOAD_KEYS,
    },
    PayloadKeys {
        convention: Convention::Genai,
        keys: genai::PAYLOAD_KEYS,
    },
    PayloadKeys {
        convention: Convention::Openinference,
        keys: openinference::PAYLOAD_KEYS,
    },
    PayloadKeys {
        convention: Convention::Vercel,
        keys: vercel::PAYLOAD_KEYS,
    },
    PayloadKeys {
        convention: Convention::Openllmetry,
        keys: openllmetry::PAYLOAD_KEYS,
    },
    PayloadKeys {
        convention: Convention::Langfuse,
        keys: langfuse::PAYLOAD_KEYS,
    },
    PayloadKeys {
        convention: Convention::Laminar,
        keys: laminar::PAYLOAD_KEYS,
    },
    PayloadKeys {
        convention: Convention::GoogleAdk,
        keys: adk::PAYLOAD_KEYS,
    },
];

/// Canonical messages, or the raw JSON when parsing fails.
///
/// A non-JSON string is text. `failed` means the source was recognized as
/// messages and could not be converted.
pub(super) fn messages_from_value(value: &AnyValue, warnings: &mut Warnings) -> (Payload, bool) {
    if let Some(json) = decode_json_text(value, warnings) {
        let text = serde_json::to_string(&json).unwrap_or_else(|_| "null".to_owned());
        return match llmobs_core::message::parse_messages(&text) {
            Ok(mut messages) => {
                parse_tool_values(&mut messages);
                (Payload::Messages(messages), false)
            }
            Err(_) => (Payload::Json(json), true),
        };
    }
    if let Some(text) = as_str(value) {
        return (Payload::Text(text.to_owned()), true);
    }
    (Payload::Json(to_json(value, warnings)), true)
}

/// Rule 6 on canonical messages: a JSON string in a tool call's `arguments` or
/// a tool response's `response` becomes that JSON value.
fn parse_tool_values(messages: &mut [Message]) {
    for part in messages
        .iter_mut()
        .flat_map(|message| message.parts.iter_mut())
    {
        match part {
            Part::ToolCall {
                arguments: Some(arguments),
                ..
            } => *arguments = parse_tool_arguments(std::mem::take(arguments)),
            Part::ToolCallResponse { response, .. } => {
                *response = parse_tool_arguments(std::mem::take(response));
            }
            _ => {}
        }
    }
}

/// JSON from an attribute. An empty string is absent.
pub(super) fn present_json(value: &AnyValue, warnings: &mut Warnings) -> Option<Json> {
    if as_str(value) == Some("") {
        return None;
    }
    if let Some(json) = decode_json_text(value, warnings) {
        return Some(json);
    }
    if let Some(text) = as_str(value) {
        return Some(Json::String(text.to_owned()));
    }
    Some(to_json(value, warnings))
}

#[cfg(test)]
mod tests {
    use llmobs_core::enums::SpanKind;
    use llmobs_core::message::messages_to_json;
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue};
    use serde_json::json;

    use super::{READERS, REGISTRIES};
    use crate::bag::{AttrBag, Use};
    use crate::payload::assemble;
    use crate::warn::Warnings;

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
    fn llmobs_input_wins_and_genai_messages_are_not_merged() {
        let attrs = vec![
            string_attr(
                "llmobs.input",
                r#"[{"role":"user","parts":[{"type":"text","content":"ours"}]}]"#,
            ),
            string_attr("llmobs.input.format", "messages"),
            string_attr(
                "gen_ai.input.messages",
                r#"[{"role":"user","parts":[{"type":"text","content":"theirs"}]}]"#,
            ),
        ];
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        let assembled = assemble(
            &mut bag,
            &[],
            SpanKind::Llm,
            &mut warnings,
            READERS,
            REGISTRIES,
        );
        let messages = match assembled.io.input {
            Some(crate::payload::Payload::Messages(messages)) => messages,
            other => panic!("expected messages, got {other:?}"),
        };
        assert_eq!(
            messages_to_json(&messages),
            r#"[{"role":"user","parts":[{"type":"text","content":"ours"}]}]"#
        );
        assert!(assembled.io.errors.is_empty());
        assert_eq!(bag.state("llmobs.input"), Some(Use::Payload));
        assert_eq!(bag.state("llmobs.input.format"), Some(Use::Payload));
        assert_eq!(bag.state("gen_ai.input.messages"), Some(Use::Payload));
        assert_eq!(json!(messages.len()), json!(1));
    }
}
