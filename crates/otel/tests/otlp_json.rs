//! Confirms `opentelemetry-proto`'s `with-serde` speaks standard OTLP/JSON.
//!
//! Hex ids, string-encoded 64-bit integers, integer enums, and lowerCamelCase
//! field names. If this fails, stop: do not add a custom OTLP/JSON decoder here.

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value;

const REQUEST: &str = r#"{
  "resourceSpans": [
    {
      "scopeSpans": [
        {
          "spans": [
            {
              "traceId": "4bf92f3577b34da6a3ce929d0e0e4736",
              "spanId": "00f067aa0ba902b7",
              "parentSpanId": "00f067aa0ba902b8",
              "name": "minimal",
              "kind": 3,
              "startTimeUnixNano": "1000",
              "endTimeUnixNano": "2500",
              "status": { "code": 1 },
              "attributes": [
                { "key": "count", "value": { "intValue": "7" } },
                { "key": "ratio", "value": { "doubleValue": 0.5 } },
                { "key": "flag", "value": { "boolValue": true } },
                {
                  "key": "reasons",
                  "value": { "arrayValue": { "values": [{ "stringValue": "stop" }] } }
                }
              ]
            }
          ]
        }
      ]
    }
  ]
}"#;

#[test]
fn otlp_json_uses_hex_ids_string_ints_and_integer_enums() {
    let request: ExportTraceServiceRequest =
        serde_json::from_str(REQUEST).expect("standard OTLP/JSON deserializes");
    let span = &request.resource_spans[0].scope_spans[0].spans[0];
    assert_eq!(
        hex_encode(&span.trace_id),
        "4bf92f3577b34da6a3ce929d0e0e4736"
    );
    assert_eq!(hex_encode(&span.span_id), "00f067aa0ba902b7");
    assert_eq!(hex_encode(&span.parent_span_id), "00f067aa0ba902b8");
    assert_eq!(span.kind, 3, "span kind is the integer enum, not a name");
    assert_eq!(span.start_time_unix_nano, 1000);
    assert_eq!(span.end_time_unix_nano, 2500);
    let status = span.status.as_ref().expect("status object");
    assert_eq!(status.code, 1);

    assert_eq!(attribute(span, "count"), &Value::IntValue(7));
    assert_eq!(attribute(span, "ratio"), &Value::DoubleValue(0.5));
    assert_eq!(attribute(span, "flag"), &Value::BoolValue(true));
    let Value::ArrayValue(reasons) = attribute(span, "reasons") else {
        panic!("reasons is an array");
    };
    let reason = reasons.values[0].value.as_ref().expect("array element");
    assert_eq!(reason, &Value::StringValue("stop".to_owned()));
}

fn attribute<'a>(span: &'a opentelemetry_proto::tonic::trace::v1::Span, key: &str) -> &'a Value {
    let item = span
        .attributes
        .iter()
        .find(|item| item.key == key)
        .unwrap_or_else(|| panic!("missing attribute {key}"));
    item.value
        .as_ref()
        .unwrap_or_else(|| panic!("{key} has no value"))
        .value
        .as_ref()
        .unwrap_or_else(|| panic!("{key} has an empty AnyValue"))
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}
