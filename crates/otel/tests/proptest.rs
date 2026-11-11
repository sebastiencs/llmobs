//! Property tests for decoding and normalization.
//!
//! Spec 02 and N23. Each property runs 256 cases unless `LLMOBS_PROPTEST_CASES`
//! sets another count. `proptest` is built with the `std` feature only.

#![allow(clippy::expect_used)]

#[allow(dead_code, unreachable_pub)]
mod support;

use std::sync::OnceLock;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use llmobs_core::enums::{Convention, PayloadFormat, SpanKind};
use llmobs_core::message::parse_messages;
use llmobs_core::names::marker::TRUNCATED_STRING_SUFFIX_PREFIX;
use llmobs_core::span::Span;
use llmobs_otel::{
    IngestContext, IngestLimits, PriceTable, decode_json, decode_protobuf, normalize,
};
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value;
use opentelemetry_proto::tonic::common::v1::{
    AnyValue, ArrayValue, InstrumentationScope, KeyValue, KeyValueList,
};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::Span as OtlpSpan;
use opentelemetry_proto::tonic::trace::v1::span::{Event, Link};
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Status};
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;
use prost::Message;
use support::load_prices;

/// Planted in names, keys, and values. Rejection text must not contain it.
const SENTINEL: &str = "SENTINEL-7f3a";
/// `2026-01-01T00:00:00Z`. Most generated starts fall in the ingest window.
const INGEST_NS: i64 = 1_767_225_600_000_000_000;
/// Attribute budget for these cases, small enough that truncation runs.
const LIMIT: usize = 128;
/// Digits in a `usize` byte count, plus the closing ` bytes]`.
const MARKER_SLACK: usize = TRUNCATED_STRING_SUFFIX_PREFIX.len() + 20 + " bytes]".len();

const PREFIXES: &[&str] = &[
    "gen_ai.",
    "gen_ai.prompt.",
    "gen_ai.completion.",
    "llm.input_messages.",
    "llm.output_messages.",
    "ai.",
    "traceloop.",
    "langfuse.",
    "lmnr.",
    "gcp.vertex.agent.",
    "llmobs.",
    "embedding.",
    "retrieval.",
    "tool.",
    "llm.request.functions.",
];

const PROMOTED: &[&str] = &[
    "service.name",
    "session.id",
    "user.id",
    "gen_ai.request.model",
    "gen_ai.response.model",
    "gen_ai.operation.name",
    "gen_ai.conversation.id",
    "tag.tags",
];

const GENAI_EVENTS: &[&str] = &[
    "gen_ai.system.message",
    "gen_ai.user.message",
    "gen_ai.assistant.message",
    "gen_ai.tool.message",
    "gen_ai.choice",
    "gen_ai.client.inference.operation.details",
];

const PRECEDENCE: [Convention; 8] = [
    Convention::Llmobs,
    Convention::Genai,
    Convention::Openinference,
    Convention::Vercel,
    Convention::Openllmetry,
    Convention::Langfuse,
    Convention::Laminar,
    Convention::GoogleAdk,
];

fn config() -> ProptestConfig {
    let cases = std::env::var("LLMOBS_PROPTEST_CASES")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(256);
    ProptestConfig {
        cases,
        failure_persistence: Some(Box::new(FileFailurePersistence::Off)),
        ..ProptestConfig::default()
    }
}

fn prices() -> &'static PriceTable {
    static TABLE: OnceLock<PriceTable> = OnceLock::new();
    TABLE.get_or_init(|| PriceTable::new(load_prices()).expect("fixtures/otlp/prices.json"))
}

fn context() -> IngestContext {
    IngestContext {
        project_id: "01900000-0000-7000-8000-000000000001"
            .parse()
            .expect("project id"),
        ingest_time: INGEST_NS,
        retention_days: None,
        limits: IngestLimits {
            max_attribute_bytes: LIMIT,
            media_min_bytes: 8,
            media_max_bytes: 4_096,
        },
    }
}

fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        12 => "[ -~]{0,48}",
        4 => prop::collection::vec(prop::char::any(), 0..24).prop_map(|chars| chars.into_iter().collect()),
        3 => Just(SENTINEL.to_owned()),
        3 => json_text(),
        2 => data_uri(),
        1 => (1usize..=65_536).prop_map(|len| "x".repeat(len)),
    ]
}

fn json_text() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(format!(r#"{{"role":"user","content":"{SENTINEL}"}}"#)),
        Just(r#"["a",1,true,null]"#.to_owned()),
        Just(serde_json::to_string(&serde_json::json!({"n": 1})).expect("json")),
        Just(serde_json::to_string(&SENTINEL).expect("json string")),
    ]
}

fn data_uri() -> impl Strategy<Value = String> {
    prop_oneof![
        Just({
            let encoded = STANDARD.encode([0x89, b'P', b'N', b'G', 1, 2, 3, 4, 5, 6, 7, 8]);
            format!("data:image/png;base64,{encoded}")
        }),
        Just(format!("data:text/plain;base64,{SENTINEL}")),
    ]
}

fn key() -> impl Strategy<Value = String> {
    let index = 0u32..=40;
    prop_oneof![
        6 => (prop::sample::select(PREFIXES), index, "[a-z]{0,12}").prop_map(|(prefix, index, suffix)| format!("{prefix}{index}.{suffix}")),
        4 => (prop::sample::select(PREFIXES), "[a-z]{0,12}").prop_map(|(prefix, suffix)| format!("{prefix}{suffix}")),
        2 => Just("input.value".to_owned()),
        2 => Just("output.value".to_owned()),
        2 => Just(SENTINEL.to_owned()),
        2 => "[a-zA-Z0-9._]{1,24}",
        4 => prop::sample::select(PROMOTED).prop_map(str::to_owned),
    ]
}

fn leaf_value() -> impl Strategy<Value = AnyValue> {
    prop_oneof![
        text().prop_map(string_value),
        any::<bool>().prop_map(|bit| AnyValue {
            value: Some(Value::BoolValue(bit))
        }),
        any::<i64>().prop_map(|number| AnyValue {
            value: Some(Value::IntValue(number))
        }),
        any::<f64>().prop_map(|number| AnyValue {
            value: Some(Value::DoubleValue(number))
        }),
        prop::collection::vec(any::<u8>(), 0..8).prop_map(|bytes| AnyValue {
            value: Some(Value::BytesValue(bytes)),
        }),
        Just(AnyValue { value: None }),
    ]
}

fn any_value() -> impl Strategy<Value = AnyValue> {
    leaf_value().prop_recursive(8, 16, 3, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..3).prop_map(|values| AnyValue {
                value: Some(Value::ArrayValue(ArrayValue { values })),
            }),
            prop::collection::vec(kv(inner), 0..3).prop_map(|values| AnyValue {
                value: Some(Value::KvlistValue(KeyValueList { values })),
            }),
        ]
    })
}

fn generated_value() -> impl Strategy<Value = AnyValue> {
    prop_oneof![
        9 => any_value().boxed(),
        1 => Just(deep_array(40)),
    ]
}

fn deep_array(depth: usize) -> AnyValue {
    let mut value = string_value(SENTINEL.to_owned());
    for _ in 0..depth {
        value = AnyValue {
            value: Some(Value::ArrayValue(ArrayValue {
                values: vec![value],
            })),
        };
    }
    value
}

fn string_value(text: String) -> AnyValue {
    AnyValue {
        value: Some(Value::StringValue(text)),
    }
}

fn kv(value: impl Strategy<Value = AnyValue> + 'static) -> impl Strategy<Value = KeyValue> {
    (key(), value).prop_map(|(key, value)| KeyValue {
        key,
        value: Some(value),
        key_strindex: 0,
    })
}

fn attributes() -> impl Strategy<Value = Vec<KeyValue>> {
    prop::collection::vec(kv(generated_value().boxed()), 0..6).prop_map(|mut items| {
        if let Some(first) = items.first().cloned() {
            items.push(first);
        }
        items
    })
}

fn id_bytes(len: usize) -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        6 => prop::collection::vec(1u8..=255, len..=len),
        1 => Just(vec![0; len]),
        1 => Just(Vec::new()),
        2 => prop::collection::vec(any::<u8>(), 0..=20),
    ]
}

fn timestamp() -> impl Strategy<Value = u64> {
    let window = 0u64..86_400_000_000_000;
    prop_oneof![
        6 => window.prop_map(|delta| (INGEST_NS as u64).saturating_sub(1_000_000_000 + delta)),
        1 => Just(0u64),
        1 => Just(u64::MAX),
        2 => any::<u64>(),
    ]
}

fn event() -> impl Strategy<Value = Event> {
    (
        prop::sample::select(GENAI_EVENTS),
        timestamp(),
        attributes(),
    )
        .prop_map(|(name, time, attributes)| Event {
            time_unix_nano: time,
            name: name.to_owned(),
            attributes,
            dropped_attributes_count: 0,
        })
}

fn link() -> impl Strategy<Value = Link> {
    (id_bytes(16), id_bytes(8), attributes()).prop_map(|(trace_id, span_id, attributes)| Link {
        trace_id,
        span_id,
        trace_state: String::new(),
        attributes,
        dropped_attributes_count: 0,
        flags: 0,
    })
}

fn otlp_span() -> impl Strategy<Value = OtlpSpan> {
    (
        id_bytes(16),
        id_bytes(8),
        id_bytes(8),
        text(),
        timestamp(),
        timestamp(),
        attributes(),
        prop::collection::vec(event(), 0..3),
        prop::collection::vec(link(), 0..2),
    )
        .prop_map(
            |(trace_id, span_id, parent, name, start, end, attributes, events, links)| OtlpSpan {
                trace_id,
                span_id,
                parent_span_id: parent,
                name: format!("{SENTINEL} {name}"),
                start_time_unix_nano: start,
                end_time_unix_nano: end,
                attributes,
                events,
                links,
                status: Some(Status {
                    message: SENTINEL.to_owned(),
                    code: 0,
                }),
                ..OtlpSpan::default()
            },
        )
}

fn request() -> impl Strategy<Value = ExportTraceServiceRequest> {
    prop::collection::vec(
        (
            attributes(),
            text(),
            text(),
            prop::collection::vec(otlp_span(), 0..=2),
        ),
        0..=2,
    )
    .prop_map(|resources| ExportTraceServiceRequest {
        resource_spans: resources
            .into_iter()
            .map(
                |(attributes, scope_name, scope_version, spans)| ResourceSpans {
                    resource: Some(Resource {
                        attributes,
                        dropped_attributes_count: 0,
                        entity_refs: Vec::new(),
                    }),
                    scope_spans: vec![ScopeSpans {
                        scope: Some(InstrumentationScope {
                            name: format!("{SENTINEL}-{scope_name}"),
                            version: scope_version,
                            attributes: Vec::new(),
                            dropped_attributes_count: 0,
                        }),
                        spans,
                        schema_url: String::new(),
                    }],
                    schema_url: String::new(),
                },
            )
            .collect(),
    })
}

fn input_span_count(request: &ExportTraceServiceRequest) -> u64 {
    request
        .resource_spans
        .iter()
        .flat_map(|resource| resource.scope_spans.iter())
        .map(|scope| scope.spans.len() as u64)
        .sum()
}

fn check_span(span: &Span) {
    assert!(span.end_time >= span.start_time);
    let text = serde_json::to_string(span).expect("span json");
    let parsed: Span = serde_json::from_str(&text).expect("span round trip");
    assert_eq!(&parsed, span);
    paired(span.input.as_ref(), span.input_format.as_ref());
    paired(span.output.as_ref(), span.output_format.as_ref());
    if span.input_format == Some(PayloadFormat::Messages) {
        parse_messages(span.input.as_deref().unwrap_or_default()).expect("input messages");
    }
    if span.output_format == Some(PayloadFormat::Messages) {
        parse_messages(span.output.as_deref().unwrap_or_default()).expect("output messages");
    }
    preview_bound(span.input_preview.as_deref());
    preview_bound(span.output_preview.as_deref());
    bounded(&span.name);
    if let Some(message) = &span.status_message {
        bounded(message);
    }
    if let Some(value) = &span.input {
        bounded(value);
    }
    if let Some(value) = &span.output {
        bounded(value);
    }
    if let Some(value) = &span.system_instructions {
        bounded(value);
    }
    if let Some(value) = &span.tool_definitions {
        bounded(value);
    }
    for value in span.attributes.values() {
        bounded(value);
    }
    for value in span.resource_attributes.values() {
        bounded(value);
    }
    for event in &span.events {
        if let Some(attributes) = &event.attributes {
            bounded(attributes);
        }
    }
    let mut truncated = span.truncated.clone();
    truncated.sort();
    truncated.dedup();
    assert_eq!(span.truncated, truncated);
    assert!(conventions_in_order(&span.conventions));
    if !matches!(span.span_kind, SpanKind::Llm | SpanKind::Embedding) {
        assert!(span.input_tokens.is_none());
        assert!(span.output_tokens.is_none());
        assert!(span.cache_read_tokens.is_none());
        assert!(span.cache_write_tokens.is_none());
        assert!(span.reasoning_tokens.is_none());
        assert!(span.total_tokens.is_none());
        assert!(span.cost_input.is_none());
        assert!(span.cost_output.is_none());
        assert!(span.cost_total.is_none());
        assert!(span.cost_source.is_none());
    }
}

fn paired(value: Option<&String>, format: Option<&PayloadFormat>) {
    assert_eq!(value.is_some(), format.is_some());
}

fn preview_bound(preview: Option<&str>) {
    if let Some(text) = preview {
        assert!(text.chars().count() <= 512);
    }
}

fn bounded(text: &str) {
    assert!(
        text.len() <= LIMIT + MARKER_SLACK,
        "stored string is {} bytes",
        text.len()
    );
}

fn conventions_in_order(conventions: &[Convention]) -> bool {
    let mut previous = None;
    let mut seen = [false; PRECEDENCE.len()];
    for convention in conventions {
        let Some(slot) = PRECEDENCE.iter().position(|item| item == convention) else {
            return false;
        };
        if seen[slot] {
            return false;
        }
        if previous.is_some_and(|index| slot < index) {
            return false;
        }
        seen[slot] = true;
        previous = Some(slot);
    }
    true
}

fn check_media(span: &Span, hashes: &[String]) {
    for hash in &span.media_refs {
        assert!(hashes.iter().any(|item| item == hash));
    }
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn normalize_holds_the_span_invariants(request in request()) {
        let ctx = context();
        let first = normalize(&request, &ctx, prices());
        let second = normalize(&request, &ctx, prices());
        prop_assert_eq!(&first, &second);
        prop_assert_eq!(
            first.spans.len() as u64 + first.rejections.total(),
            input_span_count(&request)
        );
        let hashes: Vec<String> = first.media.iter().map(|blob| blob.sha256.clone()).collect();
        for span in &first.spans {
            check_span(span);
            check_media(span, &hashes);
        }
        if let Some(message) = first.rejections.message() {
            prop_assert!(!message.contains(SENTINEL));
        }
    }

    #[test]
    fn decode_protobuf_does_not_panic(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
        let _ = decode_protobuf(&bytes);
    }

    #[test]
    fn decode_protobuf_accepts_an_encoded_request(request in request()) {
        let decoded = decode_protobuf(&request.encode_to_vec()).expect("encoded request");
        prop_assert_eq!(decoded, request);
    }

    #[test]
    fn decode_json_does_not_panic(text in prop_oneof![
        8 => prop::collection::vec(any::<u8>(), 0..2048).prop_map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
        1 => Just(format!("{SENTINEL}{{{{")),
        1 => Just("[".repeat(200) + &"]".repeat(200)),
    ]) {
        let _ = decode_json(text.as_bytes());
    }
}

#[test]
fn deeply_nested_json_is_rejected() {
    let mut text = "[".repeat(10_000);
    text.push_str(&"]".repeat(10_000));
    let error = decode_json(text.as_bytes()).expect_err("serde recursion limit");
    let rendered = error.to_string();
    assert!(!rendered.contains(SENTINEL));
    assert!(rendered.contains("invalid OTLP JSON"));
}
