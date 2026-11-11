//! Laminar span payloads.
//!
//! Spec 02 § Input and output. `lmnr.span.input` and `lmnr.span.output` become
//! messages when they are canonical (`parts`) or OpenAI-style. Anything else
//! follows rule 10. `lmnr.span.path` is not a payload key.

use llmobs_core::message::{Message, parse_messages};
use opentelemetry_proto::tonic::common::v1::AnyValue;
use serde_json::Value as Json;

use crate::convert::openai_messages;
use crate::payload::{
    Field, Io, KeyPattern, Payload, ReadContext, decode_json_text, non_message_payload,
};
use crate::value::as_str;
use crate::warn::Warnings;

const INPUT: &str = "lmnr.span.input";
const OUTPUT: &str = "lmnr.span.output";

pub(crate) const PAYLOAD_KEYS: &[(Field, KeyPattern)] = &[
    (Field::Input, KeyPattern::Exact(INPUT)),
    (Field::Output, KeyPattern::Exact(OUTPUT)),
];

pub(crate) fn read(ctx: &mut ReadContext<'_, '_>) -> Io {
    let input = take_value(ctx, INPUT);
    let output = take_value(ctx, OUTPUT);
    Io {
        input,
        output,
        ..Io::default()
    }
}

fn take_value(ctx: &mut ReadContext<'_, '_>, key: &str) -> Option<Payload> {
    let payload = {
        let value = ctx.bag.get(key)?;
        if as_str(value) == Some("") {
            return None;
        }
        messages_or_rule10(value, ctx.warnings)
    };
    ctx.bag.take_payload(key);
    Some(payload)
}

fn messages_or_rule10(value: &AnyValue, warnings: &mut Warnings) -> Payload {
    if let Some(json) = decode_json_text(value, warnings)
        && (json.is_array() || json.is_object())
    {
        if has_parts(&json) {
            if let Some(messages) = canonical_messages(&json) {
                return Payload::Messages(messages);
            }
            return Payload::Json(json);
        }
        if let Ok(messages) = openai_messages(&json) {
            return Payload::Messages(messages);
        }
        return Payload::Json(json);
    }
    non_message_payload(value, warnings)
}

fn has_parts(json: &Json) -> bool {
    match json {
        Json::Array(items) => items.iter().any(|item| item.get("parts").is_some()),
        Json::Object(map) => map.contains_key("parts"),
        _ => false,
    }
}

fn canonical_messages(json: &Json) -> Option<Vec<Message>> {
    let text = serde_json::to_string(json).ok()?;
    if let Ok(messages) = parse_messages(&text)
        && !messages.is_empty()
    {
        return Some(messages);
    }
    if json.is_object() {
        let wrapped = Json::Array(vec![json.clone()]);
        let text = serde_json::to_string(&wrapped).ok()?;
        return parse_messages(&text)
            .ok()
            .filter(|messages| !messages.is_empty());
    }
    None
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

    fn read_span(attrs: &[KeyValue]) -> crate::payload::Io {
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(attrs, &mut warnings);
        read(&mut ReadContext {
            bag: &mut bag,
            events: &[],
            kind: SpanKind::Llm,
            warnings: &mut warnings,
        })
    }

    #[test]
    fn canonical_parts_and_openai_messages_both_convert() {
        let canonical = read_span(&[string_attr(
            "lmnr.span.input",
            r#"[{"role":"user","parts":[{"type":"text","content":"from parts"}]}]"#,
        )]);
        let Payload::Messages(messages) = canonical.input.expect("input") else {
            panic!("canonical input is messages");
        };
        assert_eq!(
            messages_to_json(&messages),
            r#"[{"role":"user","parts":[{"type":"text","content":"from parts"}]}]"#
        );

        let openai = read_span(&[string_attr(
            "lmnr.span.output",
            r#"{"role":"assistant","content":"done"}"#,
        )]);
        let Payload::Messages(messages) = openai.output.expect("output") else {
            panic!("openai output is messages");
        };
        assert_eq!(
            messages_to_json(&messages),
            r#"[{"role":"assistant","parts":[{"type":"text","content":"done"}]}]"#
        );
    }
}
