//! Payload values, formats, previews, and the convention driver.
//!
//! Spec 02 § Input and output, rules 2, 6, 7, 9 and 10, and § Message conversion.
//! Convention readers register on [`READERS`] from N13 on. Until then the driver
//! keeps every payload field empty and drops nothing.

use std::collections::BTreeSet;

use llmobs_core::enums::{Convention, PayloadFormat, SpanKind};
use llmobs_core::message::{Message, Part, Role, messages_to_json};
use llmobs_core::span::Span;
use opentelemetry_proto::tonic::common::v1::AnyValue;
use opentelemetry_proto::tonic::common::v1::any_value::Value;
use opentelemetry_proto::tonic::trace::v1::span::Event;
use serde_json::Value as Json;

use crate::bag::{AttrBag, is_decimal_index};
use crate::value::{as_str, to_json};
use crate::warn::Warnings;

/// Unicode scalar values kept in a preview. Spec 02 rule 9.
const PREVIEW_SCALARS: usize = 512;

/// What one convention recognized on a span.
///
/// `events` are the span-event indices that convention used as a payload source.
/// `errors` are `messages:<convention>:<field>` codes. The driver joins them.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Io {
    pub input: Option<Payload>,
    pub output: Option<Payload>,
    /// Stored as compact JSON text.
    pub system_instructions: Option<Json>,
    /// One JSON value per tool, in index order. An empty list does not yield.
    pub tool_definitions: Option<Vec<Json>>,
    pub events: Vec<usize>,
    pub errors: Vec<&'static str>,
}

/// A normalized input or output value.
///
/// Convention readers construct the variants from N13 on. `write` already
/// matches them.
#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub(crate) enum Payload {
    Messages(Vec<Message>),
    Json(Json),
    Text(String),
}

/// Which payload column a [`KeyPattern`] belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Field {
    Input,
    Output,
    SystemInstructions,
    ToolDefinitions,
}

/// A payload attribute or event a convention may own.
///
/// N13 and later convention modules construct these. The driver already matches
/// every variant so a new convention is a list entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub(crate) enum KeyPattern {
    Exact(&'static str),
    /// Includes the trailing dot, for example `llm.input_messages.`.
    IndexedPrefix(&'static str),
    Event(&'static str),
}

/// Payload keys one convention removes when a higher-precedence source wins.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(crate) struct PayloadKeys {
    pub convention: Convention,
    pub keys: &'static [(Field, KeyPattern)],
}

/// One convention's payload reader. The list is precedence order.
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub(crate) struct ConventionPayload {
    pub convention: Convention,
    /// Lifetimes stay independent so the driver can reborrow the bag per reader.
    pub read: for<'a, 'b> fn(&'a mut ReadContext<'a, 'b>) -> Io,
}

/// What a convention reader may look at.
pub(crate) struct ReadContext<'a, 'b> {
    /// Readers use the bag from N13 on.
    #[allow(dead_code)]
    pub bag: &'a mut AttrBag<'b>,
    /// Readers use events from N13 on.
    #[allow(dead_code)]
    pub events: &'a [Event],
    /// Tool payloads depend on kind. Readers use it from N13 on.
    #[allow(dead_code)]
    pub kind: SpanKind,
}

/// Conventions registered so far. N13 appends llmobs and GenAI.
pub(crate) const READERS: &[ConventionPayload] = &[];

/// Payload keys registered so far. Each convention module adds its own slice.
pub(crate) const REGISTRIES: &[PayloadKeys] = &[];

const FIELDS: [Field; 4] = [
    Field::Input,
    Field::Output,
    Field::SystemInstructions,
    Field::ToolDefinitions,
];

/// The driver's result: columns to store, and events to omit.
#[derive(Debug, PartialEq)]
pub(crate) struct Assembled {
    pub io: Io,
    pub dropped_events: Vec<usize>,
}

/// Parse a JSON-bearing attribute. Spec 02 § Payload encodings.
///
/// A string is parsed as JSON. When that parse is itself a string whose text
/// parses as an object or array, that second parse is used. Parsing stops
/// there. `arrayValue` and `kvlistValue` use rule 1. Anything else is `None`.
#[allow(dead_code)] // N13 readers call this.
pub(crate) fn decode_json_text(value: &AnyValue, warnings: &mut Warnings) -> Option<Json> {
    if let Some(text) = as_str(value) {
        return decode_string(text);
    }
    match value.value.as_ref() {
        Some(Value::ArrayValue(_) | Value::KvlistValue(_)) => Some(to_json(value, warnings)),
        _ => None,
    }
}

/// Rule 10. An object or array is `json`. Any other string is `text`.
/// Every remaining AnyValue is `json` of its rule 1 encoding.
#[allow(dead_code)] // N13 readers call this.
pub(crate) fn non_message_payload(value: &AnyValue, warnings: &mut Warnings) -> Payload {
    if let Some(parsed) = decode_json_text(value, warnings)
        && (parsed.is_object() || parsed.is_array())
    {
        return Payload::Json(parsed);
    }
    if let Some(text) = as_str(value) {
        return Payload::Text(text.to_owned());
    }
    Payload::Json(to_json(value, warnings))
}

/// Rule 6. A string that is valid JSON becomes that value. Anything else is unchanged.
pub(crate) fn parse_tool_arguments(value: Json) -> Json {
    let Json::String(text) = &value else {
        return value;
    };
    serde_json::from_str(text).unwrap_or(value)
}

/// Rule 7. Each JSON-string item is parsed. Order is kept.
#[allow(dead_code)] // N13 readers call this.
pub(crate) fn tool_definitions(items: Vec<Json>) -> Vec<Json> {
    items.into_iter().map(parse_tool_arguments).collect()
}

/// Rule 9 for `input`.
pub(crate) fn preview_input(payload: &Payload) -> Option<String> {
    match payload {
        Payload::Messages(messages) => message_preview(input_message(messages)?),
        Payload::Json(value) => Some(cut_preview(&compact(value))),
        Payload::Text(text) => Some(cut_preview(text)),
    }
}

/// Rule 9 for `output`.
pub(crate) fn preview_output(payload: &Payload) -> Option<String> {
    match payload {
        Payload::Messages(messages) => message_preview(messages.last()?),
        Payload::Json(value) => Some(cut_preview(&compact(value))),
        Payload::Text(text) => Some(cut_preview(text)),
    }
}

/// Copy `io` onto the span columns. Messages use [`messages_to_json`].
pub(crate) fn write(span: &mut Span, io: Io) {
    if let Some(payload) = io.input {
        span.input_preview = preview_input(&payload);
        let (text, format) = stored(&payload);
        span.input = Some(text);
        span.input_format = Some(format);
    }
    if let Some(payload) = io.output {
        span.output_preview = preview_output(&payload);
        let (text, format) = stored(&payload);
        span.output = Some(text);
        span.output_format = Some(format);
    }
    if let Some(value) = io.system_instructions {
        span.system_instructions = Some(compact(&value));
    }
    if let Some(items) = io.tool_definitions.filter(|items| !items.is_empty()) {
        span.tool_definitions = Some(compact(&Json::Array(items)));
    }
}

/// `llmobs.normalize_error` text. `None` when nothing failed.
pub(crate) fn error_attribute(errors: &[&str]) -> Option<String> {
    if errors.is_empty() {
        None
    } else {
        Some(errors.join(","))
    }
}

/// First value per field, in the readers' order. Then drop lower-precedence
/// payload keys for each field that has a winner (rule 2).
pub(crate) fn assemble(
    bag: &mut AttrBag<'_>,
    events: &[Event],
    kind: SpanKind,
    readers: &[ConventionPayload],
    registries: &[PayloadKeys],
) -> Assembled {
    let mut io = Io::default();
    let mut winners = [None; FIELDS.len()];
    let mut dropped = BTreeSet::new();
    for reader in readers {
        let got = (reader.read)(&mut ReadContext { bag, events, kind });
        keep(&mut io.input, &mut winners[0], got.input, reader.convention);
        keep(
            &mut io.output,
            &mut winners[1],
            got.output,
            reader.convention,
        );
        keep(
            &mut io.system_instructions,
            &mut winners[2],
            got.system_instructions,
            reader.convention,
        );
        let tools = got.tool_definitions.filter(|items| !items.is_empty());
        keep(
            &mut io.tool_definitions,
            &mut winners[3],
            tools,
            reader.convention,
        );
        dropped.extend(got.events);
        io.errors.extend(got.errors);
    }
    for (index, field) in FIELDS.iter().enumerate() {
        dropped.extend(drop_lower_payloads(
            bag,
            events,
            *field,
            winners[index],
            registries,
        ));
    }
    Assembled {
        io,
        dropped_events: dropped.into_iter().collect(),
    }
}

/// Mark payload keys of conventions ranked below `winner`, and return their event indices.
pub(crate) fn drop_lower_payloads(
    bag: &mut AttrBag<'_>,
    events: &[Event],
    field: Field,
    winner: Option<Convention>,
    registries: &[PayloadKeys],
) -> Vec<usize> {
    let Some(winner) = winner else {
        return Vec::new();
    };
    let winner_rank = rank(winner);
    let mut dropped = Vec::new();
    for registry in registries {
        if rank(registry.convention) <= winner_rank {
            continue;
        }
        for (key_field, pattern) in registry.keys {
            if *key_field != field {
                continue;
            }
            match *pattern {
                KeyPattern::Exact(key) => bag.drop_payload(key),
                KeyPattern::IndexedPrefix(prefix) => drop_indexed(bag, prefix),
                KeyPattern::Event(name) => {
                    for (index, event) in events.iter().enumerate() {
                        if event.name == name {
                            dropped.push(index);
                        }
                    }
                }
            }
        }
    }
    dropped
}

fn decode_string(text: &str) -> Option<Json> {
    let parsed = serde_json::from_str::<Json>(text).ok()?;
    match parsed {
        Json::String(inner) => match serde_json::from_str::<Json>(&inner) {
            Ok(nested) if nested.is_object() || nested.is_array() => Some(nested),
            _ => Some(Json::String(inner)),
        },
        other => Some(other),
    }
}

fn input_message(messages: &[Message]) -> Option<&Message> {
    messages
        .iter()
        .rev()
        .find(|message| message.role == Role::User)
        .or_else(|| messages.last())
}

fn message_preview(message: &Message) -> Option<String> {
    let texts: Vec<&str> = message
        .parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { content, .. } => Some(content.as_str()),
            _ => None,
        })
        .collect();
    if texts.is_empty() {
        None
    } else {
        Some(cut_preview(&texts.join("\n")))
    }
}

fn cut_preview(text: &str) -> String {
    text.chars().take(PREVIEW_SCALARS).collect()
}

fn stored(payload: &Payload) -> (String, PayloadFormat) {
    match payload {
        Payload::Messages(messages) => (messages_to_json(messages), PayloadFormat::Messages),
        Payload::Json(value) => (compact(value), PayloadFormat::Json),
        Payload::Text(text) => (text.clone(), PayloadFormat::Text),
    }
}

fn compact(value: &Json) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned())
}

fn keep<T>(
    slot: &mut Option<T>,
    winner: &mut Option<Convention>,
    value: Option<T>,
    convention: Convention,
) {
    if slot.is_none()
        && let Some(value) = value
    {
        *slot = Some(value);
        *winner = Some(convention);
    }
}

fn drop_indexed(bag: &mut AttrBag<'_>, prefix: &str) {
    let matched: Vec<String> = bag
        .keys()
        .filter(|key| matches_indexed(key, prefix))
        .map(str::to_owned)
        .collect();
    for key in matched {
        bag.drop_payload(&key);
    }
}

fn matches_indexed(key: &str, prefix: &str) -> bool {
    let Some(rest) = key.strip_prefix(prefix) else {
        return false;
    };
    let Some((index, _)) = rest.split_once('.') else {
        return false;
    };
    is_decimal_index(index)
}

/// Spec 02 rule 13. A lower rank is higher precedence.
fn rank(convention: Convention) -> usize {
    match convention {
        Convention::Llmobs => 0,
        Convention::Genai => 1,
        Convention::Openinference => 2,
        Convention::Vercel => 3,
        Convention::Openllmetry => 4,
        Convention::Langfuse => 5,
        Convention::Laminar => 6,
        Convention::GoogleAdk => 7,
    }
}

#[cfg(test)]
mod tests {
    use llmobs_core::enums::PayloadFormat;
    use llmobs_core::message::{Message, Part, Role, messages_to_json};
    use llmobs_core::span::Span;
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, ArrayValue, KeyValue, KeyValueList};
    use opentelemetry_proto::tonic::trace::v1::span::Event;
    use serde_json::{Map, Value as Json, json};

    use super::{
        Convention, Field, Io, KeyPattern, Payload, PayloadKeys, ReadContext, SpanKind, assemble,
        decode_json_text, drop_lower_payloads, error_attribute, non_message_payload,
        parse_tool_arguments, preview_input, preview_output, tool_definitions, write,
    };
    use crate::bag::{AttrBag, Use};
    use crate::warn::Warnings;

    fn any(value: Value) -> AnyValue {
        AnyValue { value: Some(value) }
    }

    fn string(text: &str) -> AnyValue {
        any(Value::StringValue(text.to_owned()))
    }

    fn integer(number: i64) -> AnyValue {
        any(Value::IntValue(number))
    }

    fn array(values: Vec<AnyValue>) -> AnyValue {
        any(Value::ArrayValue(ArrayValue { values }))
    }

    fn kv(key: &str, value: AnyValue) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(value),
            key_strindex: 0,
        }
    }

    fn kvlist(pairs: Vec<KeyValue>) -> AnyValue {
        any(Value::KvlistValue(KeyValueList { values: pairs }))
    }

    fn named(name: &str) -> Event {
        Event {
            name: name.to_owned(),
            ..Event::default()
        }
    }

    fn text_part(content: &str) -> Part {
        Part::Text {
            content: content.to_owned(),
            extra: Map::new(),
        }
    }

    fn message(role: &str, parts: Vec<Part>) -> Message {
        Message {
            role: Role::from(role),
            name: None,
            parts,
            finish_reason: None,
            extra: Map::new(),
        }
    }

    fn blank_span() -> Span {
        serde_json::from_str(
            r#"{
                "project_id": "01900000-0000-7000-8000-000000000001",
                "trace_id": "f80a0000000000000000000000000001",
                "span_id": "f80a000000000001",
                "name": "span",
                "span_kind": "span",
                "start_time": "2026-01-01T00:00:00.000000000Z",
                "end_time": "2026-01-01T00:00:00.000000000Z",
                "status": "unset",
                "ingest_time": "2026-01-01T00:00:00.000000000Z",
                "schema_version": 1
            }"#,
        )
        .unwrap_or_else(|_| panic!("blank span json"))
    }

    #[test]
    fn decodes_a_json_string_once_and_a_wrapped_object_twice() {
        let mut warnings = Warnings::default();
        assert_eq!(
            decode_json_text(&string(r#"{"a":1}"#), &mut warnings),
            Some(json!({"a": 1}))
        );
        let wrapped = serde_json::to_string(&Json::String(r#"{"a":1}"#.to_owned()))
            .unwrap_or_else(|_| panic!("wrap"));
        assert_eq!(
            decode_json_text(&string(&wrapped), &mut warnings),
            Some(json!({"a": 1}))
        );
    }

    #[test]
    fn a_third_json_string_is_not_parsed() {
        let mut warnings = Warnings::default();
        let inner = serde_json::to_string(&Json::String("hello".to_owned()))
            .unwrap_or_else(|_| panic!("inner"));
        let outer =
            serde_json::to_string(&Json::String(inner.clone())).unwrap_or_else(|_| panic!("outer"));
        assert_eq!(
            decode_json_text(&string(&outer), &mut warnings),
            Some(Json::String(inner))
        );
    }

    #[test]
    fn structured_values_decode_and_scalars_do_not() {
        let mut warnings = Warnings::default();
        assert_eq!(
            decode_json_text(&array(vec![string("stop")]), &mut warnings),
            Some(json!(["stop"]))
        );
        assert_eq!(
            decode_json_text(&kvlist(vec![kv("k", integer(2))]), &mut warnings),
            Some(json!({"k": 2}))
        );
        assert_eq!(decode_json_text(&integer(3), &mut warnings), None);
        assert_eq!(decode_json_text(&string("hello"), &mut warnings), None);
        assert_eq!(
            decode_json_text(&AnyValue { value: None }, &mut warnings),
            None
        );
    }

    #[test]
    fn non_message_values_follow_rule_10() {
        let mut warnings = Warnings::default();
        assert_eq!(
            non_message_payload(&string(r#"{"a":1}"#), &mut warnings),
            Payload::Json(json!({"a": 1}))
        );
        assert_eq!(
            non_message_payload(&string("[1]"), &mut warnings),
            Payload::Json(json!([1]))
        );
        assert_eq!(
            non_message_payload(&string("hello"), &mut warnings),
            Payload::Text("hello".to_owned())
        );
        assert_eq!(
            non_message_payload(&string("true"), &mut warnings),
            Payload::Text("true".to_owned())
        );
        let wrapped = serde_json::to_string(&Json::String(r#"{"a":1}"#.to_owned()))
            .unwrap_or_else(|_| panic!("wrap"));
        assert_eq!(
            non_message_payload(&string(&wrapped), &mut warnings),
            Payload::Json(json!({"a": 1}))
        );
        assert_eq!(
            non_message_payload(&integer(3), &mut warnings),
            Payload::Json(json!(3))
        );
        assert_eq!(
            non_message_payload(&any(Value::BoolValue(true)), &mut warnings),
            Payload::Json(json!(true))
        );
    }

    #[test]
    fn tool_arguments_parse_only_when_the_string_is_json() {
        assert_eq!(parse_tool_arguments(json!(r#"{"x":1}"#)), json!({"x": 1}));
        assert_eq!(parse_tool_arguments(json!("not json")), json!("not json"));
        assert_eq!(parse_tool_arguments(json!(1)), json!(1));
        assert_eq!(parse_tool_arguments(json!("12")), json!(12));
        assert_eq!(
            tool_definitions(vec![
                json!(r#"{"name":"lookup"}"#),
                json!({"name": "search"}),
                json!("nope")
            ]),
            vec![
                json!({"name": "lookup"}),
                json!({"name": "search"}),
                json!("nope")
            ]
        );
    }

    #[test]
    fn message_previews_use_text_parts_only() {
        let messages = vec![
            message("system", vec![text_part("sys")]),
            message("user", vec![text_part("first")]),
            message("assistant", vec![text_part("mid")]),
            message(
                "user",
                vec![
                    text_part("last-a"),
                    Part::Reasoning {
                        content: "hidden".to_owned(),
                        extra: Map::new(),
                    },
                    text_part("last-b"),
                ],
            ),
            message("assistant", vec![text_part("tail")]),
        ];
        let payload = Payload::Messages(messages);
        assert_eq!(preview_input(&payload).as_deref(), Some("last-a\nlast-b"));
        assert_eq!(preview_output(&payload).as_deref(), Some("tail"));
    }

    #[test]
    fn input_preview_falls_back_to_the_last_message() {
        let payload = Payload::Messages(vec![
            message("system", vec![text_part("sys")]),
            message("assistant", vec![text_part("tail")]),
        ]);
        assert_eq!(preview_input(&payload).as_deref(), Some("tail"));
        let reasoning_only = Payload::Messages(vec![message(
            "assistant",
            vec![Part::Reasoning {
                content: "hidden".to_owned(),
                extra: Map::new(),
            }],
        )]);
        assert_eq!(preview_input(&reasoning_only), None);
        assert_eq!(preview_output(&reasoning_only), None);
        assert_eq!(preview_input(&Payload::Messages(Vec::new())), None);
    }

    #[test]
    fn previews_cut_at_512_scalars_without_splitting_a_character() {
        let exactly = "é".repeat(512);
        assert_eq!(
            preview_input(&Payload::Text(exactly.clone())).as_deref(),
            Some(exactly.as_str())
        );
        let mut longer = "é".repeat(512);
        longer.push('é');
        let cut = preview_output(&Payload::Text(longer)).expect("preview");
        assert_eq!(cut.chars().count(), 512);
        assert!(cut.ends_with('é'));

        let scalar = "𝄞";
        assert_eq!(scalar.len(), 4);
        let mut wide = scalar.repeat(511);
        wide.push('z');
        wide.push('𝄞');
        let preview = preview_input(&Payload::Text(wide)).expect("preview");
        assert_eq!(preview.chars().count(), 512);
        assert!(preview.ends_with('z'));
        assert_eq!(preview.len(), 511 * 4 + 1);
    }

    #[test]
    fn json_and_text_previews_are_the_stored_text() {
        assert_eq!(
            preview_input(&Payload::Json(json!({"a": 1}))).as_deref(),
            Some(r#"{"a":1}"#)
        );
        assert_eq!(
            preview_output(&Payload::Text("plain".to_owned())).as_deref(),
            Some("plain")
        );
    }

    #[test]
    fn write_sets_formats_previews_and_compact_json() {
        let mut span = blank_span();
        write(
            &mut span,
            Io {
                input: Some(Payload::Messages(vec![message(
                    "user",
                    vec![text_part("hi")],
                )])),
                output: Some(Payload::Text("done".to_owned())),
                system_instructions: Some(json!("be brief")),
                tool_definitions: Some(vec![json!({"name": "lookup"})]),
                ..Io::default()
            },
        );
        assert_eq!(span.input_format, Some(PayloadFormat::Messages));
        assert_eq!(span.input_preview.as_deref(), Some("hi"));
        assert!(
            span.input
                .as_deref()
                .is_some_and(|text| text.contains("hi"))
        );
        assert_eq!(span.output.as_deref(), Some("done"));
        assert_eq!(span.output_format, Some(PayloadFormat::Text));
        assert_eq!(span.output_preview.as_deref(), Some("done"));
        assert_eq!(span.system_instructions.as_deref(), Some(r#""be brief""#));
        assert_eq!(
            span.tool_definitions.as_deref(),
            Some(r#"[{"name":"lookup"}]"#)
        );
    }

    #[test]
    fn write_omits_an_empty_tool_list_and_a_messageless_preview() {
        let mut span = blank_span();
        let messages = vec![message(
            "assistant",
            vec![Part::Reasoning {
                content: "hidden".to_owned(),
                extra: Map::new(),
            }],
        )];
        write(
            &mut span,
            Io {
                input: Some(Payload::Messages(messages.clone())),
                tool_definitions: Some(Vec::new()),
                ..Io::default()
            },
        );
        assert_eq!(span.input_format, Some(PayloadFormat::Messages));
        assert_eq!(
            span.input.as_deref(),
            Some(messages_to_json(&messages).as_str())
        );
        assert_eq!(span.input_preview, None);
        assert_eq!(span.tool_definitions, None);
        assert_eq!(span.output, None);
    }

    #[test]
    fn error_codes_join_with_a_comma_and_no_spaces() {
        assert_eq!(error_attribute(&[]), None);
        assert_eq!(
            error_attribute(&["messages:llmobs:input", "messages:genai:output"]).as_deref(),
            Some("messages:llmobs:input,messages:genai:output")
        );
    }

    fn ours(ctx: &mut ReadContext<'_, '_>) -> Io {
        let _ = ctx;
        Io {
            input: Some(Payload::Text("ours".to_owned())),
            events: vec![0],
            errors: vec!["messages:llmobs:input"],
            ..Io::default()
        }
    }

    fn theirs(ctx: &mut ReadContext<'_, '_>) -> Io {
        let _ = ctx;
        Io {
            input: Some(Payload::Text("should-not-merge".to_owned())),
            output: Some(Payload::Text("from-genai".to_owned())),
            system_instructions: Some(json!("be brief")),
            events: vec![1],
            errors: vec!["messages:genai:output"],
            ..Io::default()
        }
    }

    #[test]
    fn the_first_value_wins_and_lower_payload_keys_are_dropped() {
        const LLMOBS_KEYS: &[(Field, KeyPattern)] =
            &[(Field::Input, KeyPattern::Exact("llmobs.input"))];
        const GENAI_KEYS: &[(Field, KeyPattern)] = &[
            (Field::Input, KeyPattern::Exact("gen_ai.input.messages")),
            (Field::Output, KeyPattern::Exact("gen_ai.output.messages")),
            (Field::Output, KeyPattern::Event("gen_ai.choice")),
        ];
        const OPENINFERENCE_KEYS: &[(Field, KeyPattern)] = &[
            (
                Field::Input,
                KeyPattern::IndexedPrefix("llm.input_messages."),
            ),
            (Field::Input, KeyPattern::Event("lower.payload")),
        ];
        const LAMINAR_KEYS: &[(Field, KeyPattern)] =
            &[(Field::Output, KeyPattern::Exact("lmnr.span.output"))];

        let attrs = vec![
            kv("llmobs.input", string("keep")),
            kv("gen_ai.input.messages", string("[]")),
            kv("llm.input_messages.0.message.content", string("flat")),
            kv("llm.input_messages.2.message.role", string("user")),
            kv("llm.input_messages.name.content", string("skip")),
            kv("gen_ai.output.messages", string("[]")),
            kv("lmnr.span.output", string("lower-out")),
        ];
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        let events = vec![
            named("winner.event"),
            named("gen_ai.choice"),
            named("lower.payload"),
            named("kept"),
        ];
        let readers = [
            super::ConventionPayload {
                convention: Convention::Llmobs,
                read: ours,
            },
            super::ConventionPayload {
                convention: Convention::Genai,
                read: theirs,
            },
        ];
        let registries = [
            PayloadKeys {
                convention: Convention::Llmobs,
                keys: LLMOBS_KEYS,
            },
            PayloadKeys {
                convention: Convention::Genai,
                keys: GENAI_KEYS,
            },
            PayloadKeys {
                convention: Convention::Openinference,
                keys: OPENINFERENCE_KEYS,
            },
            PayloadKeys {
                convention: Convention::Laminar,
                keys: LAMINAR_KEYS,
            },
        ];
        let assembled = assemble(&mut bag, &events, SpanKind::Llm, &readers, &registries);
        assert_eq!(assembled.io.input, Some(Payload::Text("ours".to_owned())));
        assert_eq!(
            assembled.io.output,
            Some(Payload::Text("from-genai".to_owned()))
        );
        assert_eq!(assembled.io.system_instructions, Some(json!("be brief")));
        assert_eq!(
            error_attribute(&assembled.io.errors).as_deref(),
            Some("messages:llmobs:input,messages:genai:output")
        );
        assert_eq!(assembled.dropped_events, vec![0, 1, 2]);
        assert_eq!(bag.state("llmobs.input"), Some(Use::Unread));
        assert_eq!(bag.state("gen_ai.input.messages"), Some(Use::Payload));
        assert_eq!(
            bag.state("llm.input_messages.0.message.content"),
            Some(Use::Payload)
        );
        assert_eq!(
            bag.state("llm.input_messages.2.message.role"),
            Some(Use::Payload)
        );
        assert_eq!(
            bag.state("llm.input_messages.name.content"),
            Some(Use::Unread)
        );
        assert_eq!(bag.state("gen_ai.output.messages"), Some(Use::Unread));
        assert_eq!(bag.state("lmnr.span.output"), Some(Use::Payload));
    }

    fn empty_tools(ctx: &mut ReadContext<'_, '_>) -> Io {
        let _ = ctx;
        Io {
            tool_definitions: Some(Vec::new()),
            ..Io::default()
        }
    }

    fn genai_tools(ctx: &mut ReadContext<'_, '_>) -> Io {
        let _ = ctx;
        Io {
            tool_definitions: Some(vec![json!({"name": "lookup"})]),
            ..Io::default()
        }
    }

    fn laminar_tools(ctx: &mut ReadContext<'_, '_>) -> Io {
        let _ = ctx;
        Io {
            tool_definitions: Some(vec![json!({"name": "other"})]),
            ..Io::default()
        }
    }

    #[test]
    fn an_empty_tool_list_does_not_win() {
        const LLMOBS_KEYS: &[(Field, KeyPattern)] =
            &[(Field::ToolDefinitions, KeyPattern::Exact("llmobs.tools"))];
        const GENAI_KEYS: &[(Field, KeyPattern)] = &[(
            Field::ToolDefinitions,
            KeyPattern::Exact("gen_ai.tool.definitions"),
        )];
        const LAMINAR_KEYS: &[(Field, KeyPattern)] =
            &[(Field::ToolDefinitions, KeyPattern::Exact("lmnr.tools"))];
        let attrs = vec![
            kv("llmobs.tools", string("[]")),
            kv("gen_ai.tool.definitions", string("[]")),
            kv("lmnr.tools", string("[]")),
        ];
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        let readers = [
            super::ConventionPayload {
                convention: Convention::Llmobs,
                read: empty_tools,
            },
            super::ConventionPayload {
                convention: Convention::Genai,
                read: genai_tools,
            },
            super::ConventionPayload {
                convention: Convention::Laminar,
                read: laminar_tools,
            },
        ];
        let registries = [
            PayloadKeys {
                convention: Convention::Llmobs,
                keys: LLMOBS_KEYS,
            },
            PayloadKeys {
                convention: Convention::Genai,
                keys: GENAI_KEYS,
            },
            PayloadKeys {
                convention: Convention::Laminar,
                keys: LAMINAR_KEYS,
            },
        ];
        let assembled = assemble(&mut bag, &[], SpanKind::Llm, &readers, &registries);
        assert_eq!(
            assembled.io.tool_definitions,
            Some(vec![json!({"name": "lookup"})])
        );
        assert_eq!(bag.state("llmobs.tools"), Some(Use::Unread));
        assert_eq!(bag.state("gen_ai.tool.definitions"), Some(Use::Unread));
        assert_eq!(bag.state("lmnr.tools"), Some(Use::Payload));
    }

    #[test]
    fn no_winner_drops_nothing() {
        let attrs = vec![kv("gen_ai.input.messages", string("[]"))];
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        let events = vec![named("gen_ai.choice")];
        let dropped = drop_lower_payloads(
            &mut bag,
            &events,
            Field::Input,
            None,
            &[PayloadKeys {
                convention: Convention::Genai,
                keys: &[(Field::Input, KeyPattern::Event("gen_ai.choice"))],
            }],
        );
        assert!(dropped.is_empty());
        assert_eq!(bag.state("gen_ai.input.messages"), Some(Use::Unread));
    }

    #[test]
    fn no_convention_is_registered_yet() {
        assert!(super::READERS.is_empty());
        assert!(super::REGISTRIES.is_empty());
    }
}
