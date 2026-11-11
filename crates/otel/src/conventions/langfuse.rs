//! Langfuse observation and trace payloads, and the observation level.
//!
//! Spec 02 § Input and output. `langfuse.observation.input` / `.output` win
//! over `langfuse.trace.input` / `.output`. An OpenAI message array, or one
//! message object, becomes messages. Anything else follows rule 10.

use llmobs_core::enums::SpanStatus;
use opentelemetry_proto::tonic::common::v1::AnyValue;

use crate::bag::AttrBag;
use crate::convert::openai_messages;
use crate::payload::{
    Field, Io, KeyPattern, Payload, ReadContext, decode_json_text, non_message_payload,
};
use crate::value::as_str;
use crate::warn::Warnings;

const OBSERVATION_INPUT: &str = "langfuse.observation.input";
const OBSERVATION_OUTPUT: &str = "langfuse.observation.output";
const TRACE_INPUT: &str = "langfuse.trace.input";
const TRACE_OUTPUT: &str = "langfuse.trace.output";
const LEVEL: &str = "langfuse.observation.level";
const STATUS_MESSAGE: &str = "langfuse.observation.status_message";

pub(crate) const PAYLOAD_KEYS: &[(Field, KeyPattern)] = &[
    (Field::Input, KeyPattern::Exact(OBSERVATION_INPUT)),
    (Field::Input, KeyPattern::Exact(TRACE_INPUT)),
    (Field::Output, KeyPattern::Exact(OBSERVATION_OUTPUT)),
    (Field::Output, KeyPattern::Exact(TRACE_OUTPUT)),
];

pub(crate) fn read(ctx: &mut ReadContext<'_, '_>) -> Io {
    let input = side(ctx, OBSERVATION_INPUT, TRACE_INPUT);
    let output = side(ctx, OBSERVATION_OUTPUT, TRACE_OUTPUT);
    Io {
        input,
        output,
        ..Io::default()
    }
}

/// `level = ERROR` sets `status` to error when the OTLP status is unset.
///
/// `status_message` fills the column when the OTLP message is empty. The
/// attribute is consumed only when it fills a column.
pub(crate) fn apply_status(
    bag: &mut AttrBag<'_>,
    status: &mut SpanStatus,
    message: &mut Option<String>,
) {
    if *status == SpanStatus::Unset && bag.get(LEVEL).and_then(as_str) == Some("ERROR") {
        *status = SpanStatus::Error;
        bag.take(LEVEL);
    }
    if message.is_some() {
        return;
    }
    let Some(text) = bag.get(STATUS_MESSAGE).and_then(as_str) else {
        return;
    };
    if text.is_empty() {
        return;
    }
    *message = Some(text.to_owned());
    bag.take(STATUS_MESSAGE);
}

fn side(ctx: &mut ReadContext<'_, '_>, observation: &str, trace: &str) -> Option<Payload> {
    if let Some(payload) = take_value(ctx, observation) {
        ctx.bag.drop_payload(trace);
        return Some(payload);
    }
    take_value(ctx, trace)
}

fn take_value(ctx: &mut ReadContext<'_, '_>, key: &str) -> Option<Payload> {
    let payload = {
        let value = ctx.bag.get(key)?;
        if as_str(value) == Some("") {
            return None;
        }
        openai_or_rule10(value, ctx.warnings)
    };
    ctx.bag.take_payload(key);
    Some(payload)
}

fn openai_or_rule10(value: &AnyValue, warnings: &mut Warnings) -> Payload {
    if let Some(json) = decode_json_text(value, warnings)
        && (json.is_array() || json.is_object())
    {
        if let Ok(messages) = openai_messages(&json) {
            return Payload::Messages(messages);
        }
        return Payload::Json(json);
    }
    non_message_payload(value, warnings)
}

#[cfg(test)]
mod tests {
    use super::{apply_status, read};
    use crate::bag::{AttrBag, Use};
    use crate::payload::{Payload, ReadContext};
    use crate::warn::Warnings;
    use llmobs_core::enums::{SpanKind, SpanStatus};
    use llmobs_core::message::messages_to_json;
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue};
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

    fn read_span(attrs: &[KeyValue]) -> (crate::payload::Io, AttrBag<'_>) {
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(attrs, &mut warnings);
        let io = read(&mut ReadContext {
            bag: &mut bag,
            events: &[],
            kind: SpanKind::Llm,
            warnings: &mut warnings,
        });
        (io, bag)
    }

    #[test]
    fn observation_messages_win_and_the_trace_value_is_dropped() {
        let attrs = [
            string_attr(
                "langfuse.observation.input",
                r#"[{"role":"user","content":"hello"}]"#,
            ),
            string_attr(
                "langfuse.trace.input",
                r#"[{"role":"user","content":"trace"}]"#,
            ),
            string_attr(
                "langfuse.observation.output",
                r#"{"role":"assistant","content":"hi"}"#,
            ),
        ];
        let (io, bag) = read_span(&attrs);
        let Payload::Messages(messages) = io.input.expect("input") else {
            panic!("input is messages");
        };
        assert_eq!(
            messages_to_json(&messages),
            r#"[{"role":"user","parts":[{"type":"text","content":"hello"}]}]"#
        );
        assert_eq!(bag.state("langfuse.trace.input"), Some(Use::Payload));
        let Payload::Messages(messages) = io.output.expect("output") else {
            panic!("output is messages");
        };
        assert_eq!(
            messages_to_json(&messages),
            r#"[{"role":"assistant","parts":[{"type":"text","content":"hi"}]}]"#
        );
    }

    #[test]
    fn trace_input_is_used_when_the_observation_key_is_absent() {
        let attrs = [string_attr(
            "langfuse.trace.output",
            r#"{"not":"messages"}"#,
        )];
        let (io, _) = read_span(&attrs);
        assert_eq!(io.output, Some(Payload::Json(json!({"not": "messages"}))));
    }

    #[test]
    fn error_level_sets_status_only_when_otlp_status_is_unset() {
        let attrs = [
            string_attr("langfuse.observation.level", "ERROR"),
            string_attr("langfuse.observation.status_message", "rate limited"),
        ];
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        let mut status = SpanStatus::Unset;
        let mut message = None;
        apply_status(&mut bag, &mut status, &mut message);
        assert_eq!(status, SpanStatus::Error);
        assert_eq!(message.as_deref(), Some("rate limited"));
        assert_eq!(bag.state("langfuse.observation.level"), Some(Use::Consumed));
        assert_eq!(
            bag.state("langfuse.observation.status_message"),
            Some(Use::Consumed)
        );

        let mut bag = AttrBag::new(&attrs, &mut warnings);
        let mut status = SpanStatus::Ok;
        let mut message = Some("from otlp".to_owned());
        apply_status(&mut bag, &mut status, &mut message);
        assert_eq!(status, SpanStatus::Ok);
        assert_eq!(message.as_deref(), Some("from otlp"));
        assert_eq!(bag.state("langfuse.observation.level"), Some(Use::Unread));
        assert_eq!(
            bag.state("langfuse.observation.status_message"),
            Some(Use::Unread)
        );
    }
}
