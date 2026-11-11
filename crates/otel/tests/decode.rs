//! OTLP decode. Inline bodies only; fixtures start at N4.
//!
//! Each JSON quirk is pinned to what `opentelemetry-proto` 0.33.1 `with-serde`
//! actually does. Spec 02 § Endpoints.

#![allow(clippy::expect_used)]

use llmobs_otel::{
    DecodeError, ExportTraceServiceRequest, JsonErrorCategory, decode_json, decode_protobuf,
};
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use prost::Message;

const TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const SPAN_ID: &str = "00f067aa0ba902b7";

#[test]
fn protobuf_round_trip() {
    let request = ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: None,
            scope_spans: vec![ScopeSpans {
                scope: None,
                spans: vec![Span {
                    trace_id: vec![0x11; 16],
                    span_id: vec![0x22; 8],
                    name: "round-trip".to_owned(),
                    ..Span::default()
                }],
                ..ScopeSpans::default()
            }],
            ..ResourceSpans::default()
        }],
    };
    let bytes = request.encode_to_vec();
    let decoded = decode_protobuf(&bytes).expect("encoded request decodes");
    assert_eq!(decoded, request);
}

#[test]
fn protobuf_garbage_is_a_protobuf_error() {
    // A truncated length-delimited field. Not a valid message.
    let err = decode_protobuf(&[0x0a]).expect_err("truncated protobuf");
    assert!(matches!(err, DecodeError::Protobuf(_)));
    let text = err.to_string();
    assert!(text.starts_with("invalid OTLP protobuf:"));
}

#[test]
fn json_hex_ids_decode() {
    // OTLP/JSON ids are lowercase hex, not base64.
    let span = only_span(&format!(
        r#"{{"resourceSpans":[{{"scopeSpans":[{{"spans":[{{
            "traceId":"{TRACE_ID}","spanId":"{SPAN_ID}","name":"hex"
        }}]}}]}}]}}"#
    ));
    assert_eq!(span.trace_id, hex_bytes(TRACE_ID));
    assert_eq!(span.span_id, hex_bytes(SPAN_ID));
    assert_eq!(span.name, "hex");
}

#[test]
fn json_int64_accepts_a_decimal_string_and_a_number() {
    // 64-bit fields accept both forms. The string form is what exporters send.
    let as_string = only_span(&format!(
        r#"{{"resourceSpans":[{{"scopeSpans":[{{"spans":[{{
            "traceId":"{TRACE_ID}","spanId":"{SPAN_ID}",
            "startTimeUnixNano":"1000","endTimeUnixNano":"2500"
        }}]}}]}}]}}"#
    ));
    assert_eq!(as_string.start_time_unix_nano, 1000);
    assert_eq!(as_string.end_time_unix_nano, 2500);

    let as_number = only_span(&format!(
        r#"{{"resourceSpans":[{{"scopeSpans":[{{"spans":[{{
            "traceId":"{TRACE_ID}","spanId":"{SPAN_ID}",
            "startTimeUnixNano":1000,"endTimeUnixNano":2500
        }}]}}]}}]}}"#
    ));
    assert_eq!(as_number.start_time_unix_nano, 1000);
    assert_eq!(as_number.end_time_unix_nano, 2500);
}

#[test]
fn json_enum_name_is_rejected() {
    // `kind` is an integer. The proto enum name is not accepted.
    let err = json_err(&format!(
        r#"{{"resourceSpans":[{{"scopeSpans":[{{"spans":[{{
            "traceId":"{TRACE_ID}","spanId":"{SPAN_ID}","kind":"SPAN_KIND_CLIENT"
        }}]}}]}}]}}"#
    ));
    assert!(matches!(
        err,
        DecodeError::Json {
            category: JsonErrorCategory::Data,
            ..
        }
    ));
}

#[test]
fn json_base64_trace_id_is_rejected() {
    // 16 zero bytes in standard base64 are 24 characters and end in `=`.
    // Hex decoding rejects `=`, so this is an error rather than a short id.
    // N4's length check is not what catches it.
    let err = json_err(
        r#"{"resourceSpans":[{"scopeSpans":[{"spans":[{
            "traceId":"AAAAAAAAAAAAAAAAAAAAAA==","spanId":"00f067aa0ba902b7"
        }]}]}]}"#,
    );
    assert!(matches!(
        err,
        DecodeError::Json {
            category: JsonErrorCategory::Data,
            ..
        }
    ));
    assert!(
        !err.to_string().contains("AAAAAAAAAAAAAAAA"),
        "the error must not echo the rejected id"
    );
}

#[test]
fn json_short_hex_trace_id_decodes() {
    // `with-serde` checks hex, not length. Three bytes stay three bytes.
    // N4 rejects the span (rule 15).
    let span = only_span(
        r#"{"resourceSpans":[{"scopeSpans":[{"spans":[{
            "traceId":"010203","spanId":"00f067aa0ba902b7","name":"short"
        }]}]}]}"#,
    );
    assert_eq!(span.trace_id, vec![0x01, 0x02, 0x03]);
    assert_eq!(span.name, "short");
}

#[test]
fn json_unknown_field_is_ignored() {
    // Generated messages do not set `deny_unknown_fields`. Spec 02 § Endpoints.
    let span = only_span(&format!(
        r#"{{"resourceSpans":[{{"scopeSpans":[{{"spans":[{{
            "traceId":"{TRACE_ID}","spanId":"{SPAN_ID}",
            "name":"known","notAField":"ignored"
        }}]}}],"alsoUnknown":1}}],"requestUnknown":true}}"#
    ));
    assert_eq!(span.name, "known");
}

#[test]
fn json_empty_object_has_no_resource_spans() {
    let request = decode_json(b"{}").expect("empty object is an empty export");
    assert!(request.resource_spans.is_empty());
}

#[test]
fn json_any_value_without_a_value_key_is_empty() {
    // `{}` selects no AnyValue alternative. This version accepts it as an
    // empty value (OpenObserve #15999: some versions reject the whole batch).
    let span = only_span(&format!(
        r#"{{"resourceSpans":[{{"scopeSpans":[{{"spans":[{{
            "traceId":"{TRACE_ID}","spanId":"{SPAN_ID}",
            "attributes":[{{"key":"empty","value":{{}}}}]
        }}]}}]}}]}}"#
    ));
    let value = span.attributes[0]
        .value
        .as_ref()
        .expect("the attribute value object is present");
    assert!(value.value.is_none(), "no value alternative was selected");
}

#[test]
fn json_error_does_not_echo_input() {
    // serde's message would include the bad enum string and can include the
    // span name. Keep line, column, and category only.
    let err = json_err(
        r#"{"resourceSpans":[{"scopeSpans":[{"spans":[{"name":"SECRET-123","kind":"SECRET-456"}]}]}]}"#,
    );
    let text = err.to_string();
    assert!(!text.contains("SECRET"), "{text}");
    assert!(matches!(
        err,
        DecodeError::Json {
            category: JsonErrorCategory::Data,
            line: 1,
            ..
        }
    ));
}

fn only_span(body: &str) -> Span {
    let mut request = decode_json(body.as_bytes()).expect("OTLP/JSON decodes");
    let mut resource = request.resource_spans.swap_remove(0);
    let mut scope = resource.scope_spans.swap_remove(0);
    scope.spans.swap_remove(0)
}

fn json_err(body: &str) -> DecodeError {
    decode_json(body.as_bytes()).expect_err("OTLP/JSON is rejected")
}

fn hex_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("hex digit"))
        .collect()
}
