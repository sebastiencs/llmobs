//! llmobs `input` and `output`.
//!
//! Spec 02 § Input and output. `llmobs.input.format` / `llmobs.output.format`
//! select `messages`, `json`, or `text`. When the format is absent, rule 10
//! decides.

use llmobs_core::names::attr;
use opentelemetry_proto::tonic::common::v1::AnyValue;
use serde_json::Value as Json;

use super::{messages_from_value, present_json};
use crate::payload::{
    Field, Io, KeyPattern, Payload, ReadContext, decode_json_text, non_message_payload,
};
use crate::value::{as_str, to_json};
use crate::warn::Warnings;

pub(crate) const PAYLOAD_KEYS: &[(Field, KeyPattern)] = &[
    (Field::Input, KeyPattern::Exact(attr::INPUT)),
    (Field::Input, KeyPattern::Exact(attr::INPUT_FORMAT)),
    (Field::Output, KeyPattern::Exact(attr::OUTPUT)),
    (Field::Output, KeyPattern::Exact(attr::OUTPUT_FORMAT)),
];

pub(crate) fn read(ctx: &mut ReadContext<'_, '_>) -> Io {
    let mut io = Io::default();
    if let Some((payload, failed)) = field(ctx, attr::INPUT, attr::INPUT_FORMAT) {
        io.input = Some(payload);
        if failed {
            io.errors.push("messages:llmobs:input");
        }
    }
    if let Some((payload, failed)) = field(ctx, attr::OUTPUT, attr::OUTPUT_FORMAT) {
        io.output = Some(payload);
        if failed {
            io.errors.push("messages:llmobs:output");
        }
    }
    io
}

fn field(
    ctx: &mut ReadContext<'_, '_>,
    value_key: &str,
    format_key: &str,
) -> Option<(Payload, bool)> {
    if as_str(ctx.bag.get(value_key)?) == Some("") {
        return None;
    }
    let format = ctx
        .bag
        .get(format_key)
        .and_then(as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    let value = ctx.bag.take_payload(value_key)?;
    let outcome = match format.as_deref() {
        Some("messages") => messages_from_value(value, ctx.warnings),
        Some("json") => (json_payload(value, ctx.warnings), false),
        Some("text") => (text_payload(value, ctx.warnings), false),
        _ => (non_message_payload(value, ctx.warnings), false),
    };
    if matches!(format.as_deref(), Some("messages" | "json" | "text")) {
        ctx.bag.take_payload(format_key);
    }
    Some(outcome)
}

fn json_payload(value: &AnyValue, warnings: &mut Warnings) -> Payload {
    if let Some(json) = present_json(value, warnings) {
        return Payload::Json(json);
    }
    Payload::Json(Json::Null)
}

fn text_payload(value: &AnyValue, warnings: &mut Warnings) -> Payload {
    if let Some(text) = as_str(value) {
        return Payload::Text(text.to_owned());
    }
    if let Some(json) = decode_json_text(value, warnings) {
        let text = serde_json::to_string(&json).unwrap_or_else(|_| "null".to_owned());
        return Payload::Text(text);
    }
    Payload::Text(to_json(value, warnings).to_string())
}

#[cfg(test)]
mod tests {
    use super::read;
    use crate::bag::AttrBag;
    use crate::payload::{Payload, ReadContext};
    use crate::warn::Warnings;
    use llmobs_core::enums::SpanKind;
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

    fn read_attrs(attrs: &[KeyValue]) -> crate::payload::Io {
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(attrs, &mut warnings);
        read(&mut ReadContext {
            bag: &mut bag,
            events: &[],
            kind: SpanKind::Span,
            warnings: &mut warnings,
        })
    }

    #[test]
    fn an_explicit_text_format_is_not_parsed_as_json() {
        let io = read_attrs(&[
            string_attr("llmobs.input", r#"{"query":"hi"}"#),
            string_attr("llmobs.input.format", "text"),
        ]);
        assert_eq!(
            io.input,
            Some(Payload::Text(r#"{"query":"hi"}"#.to_owned()))
        );
        assert!(io.errors.is_empty());
    }

    #[test]
    fn a_messages_value_that_is_not_an_array_keeps_the_raw_json() {
        let io = read_attrs(&[
            string_attr("llmobs.output", r#"{"not":"messages"}"#),
            string_attr("llmobs.output.format", "messages"),
        ]);
        assert_eq!(
            io.output,
            Some(Payload::Json(serde_json::json!({"not": "messages"})))
        );
        assert_eq!(io.errors, ["messages:llmobs:output"]);
    }

    #[test]
    fn absent_format_follows_rule_10() {
        let io = read_attrs(&[string_attr("llmobs.input", "plain")]);
        assert_eq!(io.input, Some(Payload::Text("plain".to_owned())));
        let io = read_attrs(&[string_attr("llmobs.input", r#"{"a":1}"#)]);
        let Payload::Json(value) = io.input.expect("json") else {
            panic!("expected json");
        };
        assert_eq!(value, serde_json::json!({"a": 1}));
    }
}
