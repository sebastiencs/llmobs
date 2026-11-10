//! Owned span record written by normalization and read by storage.
//!
//! Field names match the `spans` columns in spec 01. `duration_ns` and `model` are computed
//! when the record is converted to Arrow; they are not stored on the struct (D60).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::enums::{Convention, CostSource, PayloadFormat, SpanKind, SpanStatus};
use crate::ids::{ProjectId, SpanId, TraceId};

/// One normalized span.
///
/// JSON omits empty `Option`, `Vec`, and `BTreeMap` fields. Instants are RFC 3339 UTC
/// strings (D64). Spec 01 § Span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Span {
    /// Partition key. Spec 01 `project_id`.
    pub project_id: ProjectId,
    /// OTel trace id. Spec 01 `trace_id`.
    pub trace_id: TraceId,
    /// OTel span id. Spec 01 `span_id`.
    pub span_id: SpanId,
    /// Parent span. Absent when this span is a root of the received trace. Spec 01 `parent_span_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<SpanId>,
    /// Span name. Spec 01 `name`.
    pub name: String,
    /// Kind of work. Spec 01 `span_kind`.
    pub span_kind: SpanKind,
    /// Start, nanoseconds since the Unix epoch, UTC. Spec 01 `start_time`.
    #[serde(with = "crate::time_serde")]
    pub start_time: i64,
    /// End, nanoseconds since the Unix epoch. The producer guarantees `end_time >= start_time`. Spec 01 `end_time`.
    #[serde(with = "crate::time_serde")]
    pub end_time: i64,
    /// OTel status. Spec 01 `status`.
    pub status: SpanStatus,
    /// Status description. Spec 01 `status_message`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    /// Resource `service.name`. Spec 01 `service_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// Session id. Spec 01 `session_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// End-user id. GDPR key. Spec 01 `user_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Deployment environment. Spec 01 `environment`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// Release, git SHA, or version. Spec 01 `release`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// Tags. Spec 01 `tags`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// User metadata. Values are JSON-encoded scalars or strings. Spec 01 `metadata`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
    /// Normalized provider name. Spec 01 `provider`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model named in the request. Spec 01 `request_model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_model: Option<String>,
    /// Model named in the response. Spec 01 `response_model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    /// GenAI operation name. Spec 01 `operation`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// Input tokens, including cached tokens. Spec 01 `input_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    /// Output tokens, including reasoning tokens. Spec 01 `output_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    /// Cache-read tokens. Spec 01 `cache_read_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<i64>,
    /// Cache-write tokens. Spec 01 `cache_write_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<i64>,
    /// Reasoning tokens. Spec 01 `reasoning_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<i64>,
    /// Total tokens. Spec 01 `total_tokens`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
    /// Input cost, USD. Spec 01 `cost_input`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_input: Option<f64>,
    /// Output cost, USD. Spec 01 `cost_output`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_output: Option<f64>,
    /// Total cost, USD. Spec 01 `cost_total`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_total: Option<f64>,
    /// How cost was obtained. Spec 01 `cost_source`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_source: Option<CostSource>,
    /// Time to first token, nanoseconds. A duration, so JSON keeps it as an integer. Spec 01 `ttft_ns`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ns: Option<i64>,
    /// Request parameters as a JSON object string. Spec 01 `request_params`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_params: Option<String>,
    /// Payload. Present exactly when [`Self::input_format`] is present. Spec 01 § Payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// Encoding of [`Self::input`]. Spec 01 `input_format`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_format: Option<PayloadFormat>,
    /// Payload. Present exactly when [`Self::output_format`] is present. Spec 01 § Payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// Encoding of [`Self::output`]. Spec 01 `output_format`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_format: Option<PayloadFormat>,
    /// First 512 characters of the plain-text rendering. Spec 01 `input_preview`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_preview: Option<String>,
    /// First 512 characters of the plain-text rendering. Spec 01 `output_preview`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<String>,
    /// System instructions as JSON text. Spec 01 `system_instructions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_instructions: Option<String>,
    /// Tool definitions as JSON text. Spec 01 `tool_definitions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_definitions: Option<String>,
    /// Span attributes not consumed by normalization. Values are JSON-encoded. Spec 01 `attributes`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
    /// Resource attributes except `service.name`. Values are JSON-encoded. Spec 01 `resource_attributes`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resource_attributes: BTreeMap<String, String>,
    /// Instrumentation scope name. Spec 01 `scope_name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_name: Option<String>,
    /// Instrumentation scope version. Spec 01 `scope_version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_version: Option<String>,
    /// Span events. GenAI message events are consumed before this is stored. Spec 01 `events`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<SpanEvent>,
    /// Span links. Spec 01 `links`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<SpanLink>,
    /// SHA-256 hex of extracted media blobs. Spec 01 `media_refs`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub media_refs: Vec<String>,
    /// Names of attributes truncated at ingest. Spec 01 `truncated`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub truncated: Vec<String>,
    /// Conventions detected on the span. Spec 01 `conventions`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conventions: Vec<Convention>,
    /// Server receive time, nanoseconds since the Unix epoch. Spec 01 `ingest_time`.
    #[serde(with = "crate::time_serde")]
    pub ingest_time: i64,
    /// Normalization version of this row. Spec 01 `schema_version`.
    pub schema_version: u16,
}

/// One span event.
///
/// Spec 01 `events`. `attributes` is a JSON object string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanEvent {
    /// Event time, nanoseconds since the Unix epoch. Spec 01 `events.time`.
    #[serde(with = "crate::time_serde")]
    pub time: i64,
    /// Event name. Spec 01 `events.name`.
    pub name: String,
    /// JSON object text. Spec 01 `events.attributes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<String>,
}

/// One span link.
///
/// Spec 01 `links`. `attributes` is a JSON object string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanLink {
    /// Linked trace id. Spec 01 `links.trace_id`.
    pub trace_id: TraceId,
    /// Linked span id. Spec 01 `links.span_id`.
    pub span_id: SpanId,
    /// JSON object text. Spec 01 `links.attributes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<String>,
}

/// A span with only the required fields set. Other crates build their own literals.
#[cfg(test)]
pub(crate) fn minimal_span() -> Span {
    Span {
        project_id: project_id(),
        trace_id: trace_id(),
        span_id: span_id(),
        parent_span_id: None,
        name: "root".to_owned(),
        span_kind: SpanKind::Span,
        start_time: 0,
        end_time: 0,
        status: SpanStatus::Unset,
        status_message: None,
        service_name: None,
        session_id: None,
        user_id: None,
        environment: None,
        release: None,
        tags: Vec::new(),
        metadata: BTreeMap::new(),
        provider: None,
        request_model: None,
        response_model: None,
        operation: None,
        input_tokens: None,
        output_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
        total_tokens: None,
        cost_input: None,
        cost_output: None,
        cost_total: None,
        cost_source: None,
        ttft_ns: None,
        request_params: None,
        input: None,
        input_format: None,
        output: None,
        output_format: None,
        input_preview: None,
        output_preview: None,
        system_instructions: None,
        tool_definitions: None,
        attributes: BTreeMap::new(),
        resource_attributes: BTreeMap::new(),
        scope_name: None,
        scope_version: None,
        events: Vec::new(),
        links: Vec::new(),
        media_refs: Vec::new(),
        truncated: Vec::new(),
        conventions: Vec::new(),
        ingest_time: 0,
        schema_version: 1,
    }
}

/// A span with every field set, for serde and Arrow tests.
#[cfg(test)]
pub(crate) fn full_span() -> Span {
    let start = crate::time_serde::parse_rfc3339("2026-11-09T10:12:03.123456789Z").unwrap();
    let mut metadata = BTreeMap::new();
    metadata.insert("plan".to_owned(), "\"pro\"".to_owned());
    let mut attributes = BTreeMap::new();
    attributes.insert("http.method".to_owned(), "\"GET\"".to_owned());
    let mut resource = BTreeMap::new();
    resource.insert("host.name".to_owned(), "\"api\"".to_owned());
    Span {
        project_id: project_id(),
        trace_id: trace_id(),
        span_id: span_id(),
        parent_span_id: Some(parent_span_id()),
        name: "chat".to_owned(),
        span_kind: SpanKind::Llm,
        start_time: start,
        end_time: start + 1_500_000_000,
        status: SpanStatus::Error,
        status_message: Some("rate limited".to_owned()),
        service_name: Some("api".to_owned()),
        session_id: Some("chat-42".to_owned()),
        user_id: Some("u-17".to_owned()),
        environment: Some("production".to_owned()),
        release: Some("abc123".to_owned()),
        tags: vec!["beta".to_owned()],
        metadata,
        provider: Some("openai".to_owned()),
        request_model: Some("gpt-5-mini".to_owned()),
        response_model: Some("gpt-5-mini-2026-11".to_owned()),
        operation: Some("chat".to_owned()),
        input_tokens: Some(10),
        output_tokens: Some(4),
        cache_read_tokens: Some(2),
        cache_write_tokens: Some(1),
        reasoning_tokens: Some(3),
        total_tokens: Some(14),
        cost_input: Some(0.001),
        cost_output: Some(0.002),
        cost_total: Some(0.003),
        cost_source: Some(CostSource::Computed),
        ttft_ns: Some(42_000_000),
        request_params: Some(r#"{"temperature":0.2}"#.to_owned()),
        input: Some(r#"[{"role":"user","parts":[{"type":"text","content":"hi"}]}]"#.to_owned()),
        input_format: Some(PayloadFormat::Messages),
        output: Some("hello".to_owned()),
        output_format: Some(PayloadFormat::Text),
        input_preview: Some("hi".to_owned()),
        output_preview: Some("hello".to_owned()),
        system_instructions: Some(r#"[{"type":"text","content":"be brief"}]"#.to_owned()),
        tool_definitions: Some("[]".to_owned()),
        attributes,
        resource_attributes: resource,
        scope_name: Some("llmobs".to_owned()),
        scope_version: Some("0.0.0".to_owned()),
        events: vec![SpanEvent {
            time: start,
            name: "exception".to_owned(),
            attributes: Some(r#"{"exception.type":"RateLimit"}"#.to_owned()),
        }],
        links: vec![SpanLink {
            trace_id: trace_id(),
            span_id: parent_span_id(),
            attributes: Some(r#"{"link.kind":"child"}"#.to_owned()),
        }],
        media_refs: vec!["ab".repeat(32)],
        truncated: vec!["llmobs.input".to_owned()],
        conventions: vec![Convention::Llmobs, Convention::Genai],
        ingest_time: start + 2_000_000_000,
        schema_version: 1,
    }
}

#[cfg(test)]
fn project_id() -> ProjectId {
    "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
}

#[cfg(test)]
fn trace_id() -> TraceId {
    "4bf92f3577b34da6a3ce929d0e0e4736".parse().unwrap()
}

#[cfg(test)]
fn span_id() -> SpanId {
    "00f067aa0ba902b7".parse().unwrap()
}

#[cfg(test)]
fn parent_span_id() -> SpanId {
    "00f067aa0ba902b8".parse().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUIRED: &[&str] = &[
        "project_id",
        "trace_id",
        "span_id",
        "name",
        "span_kind",
        "start_time",
        "end_time",
        "status",
        "ingest_time",
        "schema_version",
    ];

    #[test]
    fn minimal_span_json_has_only_required_fields() {
        let json = serde_json::to_value(minimal_span()).unwrap();
        let object = json.as_object().unwrap();
        let keys: Vec<_> = object.keys().cloned().collect();
        assert_eq!(keys, REQUIRED);
        assert_eq!(object["start_time"], "1970-01-01T00:00:00.000000000Z");
        assert_eq!(object["status"], "unset");
        assert!(object.get("duration_ns").is_none());
        assert!(object.get("model").is_none());
        assert!(object.get("ttft_ns").is_none());
    }

    #[test]
    fn span_json_round_trip() {
        for span in [minimal_span(), full_span()] {
            let json = serde_json::to_string(&span).unwrap();
            assert_eq!(serde_json::from_str::<Span>(&json).unwrap(), span);
        }
        let full = serde_json::to_value(full_span()).unwrap();
        assert_eq!(full["ttft_ns"], 42_000_000);
        assert!(full["start_time"].is_string());
        assert!(full.get("duration_ns").is_none());
    }

    #[test]
    fn span_deserializes_from_a_json_value() {
        let span = full_span();
        let value = serde_json::to_value(&span).unwrap();
        assert_eq!(serde_json::from_value::<Span>(value).unwrap(), span);
    }

    #[test]
    fn span_json_snapshots() {
        insta::with_settings!({
            snapshot_path => "../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!("span_minimal", serde_json::to_string_pretty(&minimal_span()).unwrap());
            insta::assert_snapshot!("span_full", serde_json::to_string_pretty(&full_span()).unwrap());
        });
    }
}
