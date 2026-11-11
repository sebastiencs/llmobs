//! Span skeleton: ids, times, resource, attributes, events, and links.
//!
//! Spec 02 rules 1, 2, 13, 15, 17, 18 and 19. Scalar columns stay empty until
//! the field phases.

use std::collections::BTreeMap;

use llmobs_core::enums::{SpanKind, SpanStatus};
use llmobs_core::ids::{ProjectId, SpanId, TraceId};
use llmobs_core::media::MediaBlob;
use llmobs_core::names::metadata_key;
use llmobs_core::span::{Span, SpanEvent, SpanLink};
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::KeyValue;
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::Status;
use opentelemetry_proto::tonic::trace::v1::span::Event;
use opentelemetry_proto::tonic::trace::v1::span::Link;
use serde_json::{Map, Value as Json};

use crate::bag::AttrBag;
use crate::detect::detect;
use crate::fields::ResourceView;
use crate::reject::{RejectReason, Rejections};
use crate::value::{as_str, to_json};
use crate::warn::{Warning, Warnings};

/// `2000-01-01T00:00:00Z` in nanoseconds. Spec 02 rule 15.
const YEAR_2000_NS: i64 = 946_684_800_i64 * 1_000_000_000;
/// One day in nanoseconds. The future bound is one of these past `ingest_time`.
const NANOS_PER_DAY: i64 = 86_400_i64 * 1_000_000_000;

/// Core stores this on every span and does not expose it as a constant.
const SCHEMA_VERSION: u16 = 1;

/// Clock, project, and limits for one ingest request.
///
/// Spec 02 § Limits and rule 15. `retention_days` is `None` when the project
/// keeps spans forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestContext {
    /// Partition key written onto every accepted span.
    pub project_id: ProjectId,
    /// Server receive time, nanoseconds since the Unix epoch.
    pub ingest_time: i64,
    /// How many days of history this project keeps. `None` means no cutoff.
    pub retention_days: Option<u32>,
    /// Size limits. Truncation and media read them from N14 on.
    pub limits: IngestLimits,
}

/// Byte limits from spec 02 § Limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestLimits {
    /// `ingest.max_attribute_bytes`. Default 8 MiB.
    pub max_attribute_bytes: usize,
    /// `media.min_bytes`. Default 4 KiB.
    pub media_min_bytes: usize,
    /// `media.max_bytes`. Default 32 MiB.
    pub media_max_bytes: usize,
}

impl Default for IngestLimits {
    fn default() -> Self {
        Self {
            max_attribute_bytes: 8 * 1024 * 1024,
            media_min_bytes: 4 * 1024,
            media_max_bytes: 32 * 1024 * 1024,
        }
    }
}

/// Accepted spans, extracted blobs, and rejection counts.
///
/// `media` stays empty until N14. `normalize` does not fail: a bad span is counted
/// in `rejections` and the rest of the request is kept.
#[derive(Debug, Clone, PartialEq)]
pub struct Normalized {
    /// Spans that passed the id and time checks, in request order.
    pub spans: Vec<Span>,
    /// Blobs to store before the WAL append.
    pub media: Vec<MediaBlob>,
    /// Why the other spans were dropped.
    pub rejections: Rejections,
}

/// Normalize one export request into span skeletons.
///
/// `PriceTable` joins this signature in N16. Until then cost is unset.
pub fn normalize(request: &ExportTraceServiceRequest, ctx: &IngestContext) -> Normalized {
    let mut spans = Vec::new();
    let mut rejections = Rejections::default();
    for resource_spans in &request.resource_spans {
        let resource = encode_resource(resource_spans.resource.as_ref());
        for scope_spans in &resource_spans.scope_spans {
            let (scope_name, scope_version) = scope_identity(scope_spans.scope.as_ref());
            for span in &scope_spans.spans {
                if let Some(built) = accept_span(
                    span,
                    ctx,
                    &resource,
                    scope_name.clone(),
                    scope_version.clone(),
                    &mut rejections,
                ) {
                    spans.push(built);
                }
            }
        }
    }
    Normalized {
        spans,
        media: Vec::new(),
        rejections,
    }
}

struct EncodedResource<'a> {
    service_name: Option<String>,
    attributes: BTreeMap<String, String>,
    warnings: Warnings,
    truncated: Vec<String>,
    /// Resource attributes for the field resolver. Read from N7 on.
    #[allow(dead_code)]
    view: ResourceView<'a>,
}

fn encode_resource(resource: Option<&Resource>) -> EncodedResource<'_> {
    let attrs = resource
        .map(|item| item.attributes.as_slice())
        .unwrap_or(&[]);
    let view = ResourceView::new(attrs);
    let mut warnings = Warnings::default();
    let mut bag = AttrBag::new(attrs, &mut warnings);
    let service_name = bag
        .get("service.name")
        .and_then(as_str)
        .map(ToOwned::to_owned);
    bag.drop_payload("service.name");
    let mut truncated = Vec::new();
    let attributes = bag.into_attributes(&mut warnings, &mut truncated);
    EncodedResource {
        service_name,
        attributes,
        warnings,
        truncated,
        view,
    }
}

fn scope_identity(
    scope: Option<&opentelemetry_proto::tonic::common::v1::InstrumentationScope>,
) -> (Option<String>, Option<String>) {
    let Some(scope) = scope else {
        return (None, None);
    };
    (non_empty(&scope.name), non_empty(&scope.version))
}

fn non_empty(text: &str) -> Option<String> {
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

fn accept_span(
    span: &opentelemetry_proto::tonic::trace::v1::Span,
    ctx: &IngestContext,
    resource: &EncodedResource<'_>,
    scope_name: Option<String>,
    scope_version: Option<String>,
    rejections: &mut Rejections,
) -> Option<Span> {
    let trace_id = match TraceId::from_slice(&span.trace_id) {
        Ok(id) => id,
        Err(_) => {
            rejections.record(RejectReason::InvalidTraceId);
            return None;
        }
    };
    let span_id = match SpanId::from_slice(&span.span_id) {
        Ok(id) => id,
        Err(_) => {
            rejections.record(RejectReason::InvalidSpanId);
            return None;
        }
    };
    let Ok(start_time) = i64::try_from(span.start_time_unix_nano) else {
        rejections.record(RejectReason::TimestampOverflow);
        return None;
    };
    let Ok(end_time) = i64::try_from(span.end_time_unix_nano) else {
        rejections.record(RejectReason::TimestampOverflow);
        return None;
    };
    if start_out_of_range(start_time, ctx) {
        rejections.record(RejectReason::StartTimeOutOfRange);
        return None;
    }

    let mut warnings = resource.warnings.clone();
    let end_time = clamp_end(start_time, end_time, span.end_time_unix_nano, &mut warnings);
    let (status, status_message) = map_status(span.status.as_ref());

    let mut truncated = resource.truncated.clone();
    let bag = AttrBag::new(&span.attributes, &mut warnings);
    let conventions = detect(&bag, &span.events);
    let mut attributes = bag.into_attributes(&mut warnings, &mut truncated);
    let events = encode_events(&span.events, start_time, &mut warnings, &mut truncated);
    let links = encode_links(&span.links, &mut warnings, &mut truncated);
    if let Some(text) = warnings.to_attribute() {
        attributes.insert(metadata_key::NORMALIZE_WARNING.to_owned(), text);
    }

    Some(Span {
        project_id: ctx.project_id,
        trace_id,
        span_id,
        parent_span_id: parent_id(&span.parent_span_id),
        name: span.name.clone(),
        span_kind: SpanKind::Span,
        start_time,
        end_time,
        status,
        status_message,
        service_name: resource.service_name.clone(),
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
        attributes,
        resource_attributes: resource.attributes.clone(),
        scope_name,
        scope_version,
        events,
        links,
        media_refs: Vec::new(),
        truncated,
        conventions,
        ingest_time: ctx.ingest_time,
        schema_version: SCHEMA_VERSION,
    })
}

fn start_out_of_range(start_time: i64, ctx: &IngestContext) -> bool {
    if start_time < YEAR_2000_NS {
        return true;
    }
    if let Some(days) = ctx.retention_days {
        let window = i64::from(days).saturating_mul(NANOS_PER_DAY);
        if start_time < ctx.ingest_time.saturating_sub(window) {
            return true;
        }
    }
    start_time > ctx.ingest_time.saturating_add(NANOS_PER_DAY)
}

fn clamp_end(start: i64, end: i64, end_raw: u64, warnings: &mut Warnings) -> i64 {
    if end < start || end_raw == 0 {
        warnings.add(Warning::EndBeforeStart);
        start
    } else {
        end
    }
}

fn map_status(status: Option<&Status>) -> (SpanStatus, Option<String>) {
    let Some(status) = status else {
        return (SpanStatus::Unset, None);
    };
    let code = match status.code {
        1 => SpanStatus::Ok,
        2 => SpanStatus::Error,
        _ => SpanStatus::Unset,
    };
    let message = non_empty(&status.message);
    (code, message)
}

/// Empty or all-zero is absent. Any other length is absent too. Spec 02 rule 15.
fn parent_id(bytes: &[u8]) -> Option<SpanId> {
    if bytes.is_empty() || bytes.iter().all(|byte| *byte == 0) {
        return None;
    }
    SpanId::from_slice(bytes).ok()
}

fn encode_events(
    events: &[Event],
    start_time: i64,
    warnings: &mut Warnings,
    truncated: &mut Vec<String>,
) -> Vec<SpanEvent> {
    events
        .iter()
        .map(|event| {
            let time = match i64::try_from(event.time_unix_nano) {
                Ok(time) => time,
                Err(_) => {
                    warnings.add(Warning::InvalidEventTime);
                    start_time
                }
            };
            SpanEvent {
                time,
                name: event.name.clone(),
                attributes: encode_attribute_object(&event.attributes, warnings, truncated),
            }
        })
        .collect()
}

fn encode_links(
    links: &[Link],
    warnings: &mut Warnings,
    truncated: &mut Vec<String>,
) -> Vec<SpanLink> {
    let mut out = Vec::new();
    for link in links {
        let Ok(trace_id) = TraceId::from_slice(&link.trace_id) else {
            continue;
        };
        let Ok(span_id) = SpanId::from_slice(&link.span_id) else {
            continue;
        };
        out.push(SpanLink {
            trace_id,
            span_id,
            attributes: encode_attribute_object(&link.attributes, warnings, truncated),
        });
    }
    out
}

/// Event and link attributes are one JSON object. Values use rule 1's JSON
/// form, not map strings. An empty list is omitted.
fn encode_attribute_object(
    attrs: &[KeyValue],
    warnings: &mut Warnings,
    truncated: &mut Vec<String>,
) -> Option<String> {
    if attrs.is_empty() {
        return None;
    }
    let mut object = Map::new();
    for attr in attrs {
        if object.contains_key(&attr.key) {
            warnings.add(Warning::DuplicateKey);
        }
        let mut local = Warnings::default();
        let json = match &attr.value {
            Some(value) => to_json(value, &mut local),
            None => Json::Null,
        };
        if local.contains(Warning::MaxDepth) {
            truncated.push(attr.key.clone());
        }
        warnings.append(local);
        object.insert(attr.key.clone(), json);
    }
    // The object is finite JSON: non-finite doubles are already null.
    Some(serde_json::to_string(&Json::Object(object)).unwrap_or_else(|_| "{}".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use llmobs_core::enums::Convention;
    use opentelemetry_proto::tonic::common::v1::AnyValue;
    use opentelemetry_proto::tonic::common::v1::InstrumentationScope;
    use opentelemetry_proto::tonic::trace::v1::ResourceSpans;
    use opentelemetry_proto::tonic::trace::v1::ScopeSpans;
    use opentelemetry_proto::tonic::trace::v1::Span as OtlpSpan;

    fn context() -> IngestContext {
        IngestContext {
            project_id: "01900000-0000-7000-8000-000000000001"
                .parse()
                .expect("project id"),
            ingest_time: 1_767_225_600_000_000_000,
            retention_days: Some(7),
            limits: IngestLimits::default(),
        }
    }

    fn ids() -> (Vec<u8>, Vec<u8>) {
        (vec![0x11; 16], vec![0x22; 8])
    }

    fn span_at(start: u64, end: u64) -> OtlpSpan {
        let (trace_id, span_id) = ids();
        OtlpSpan {
            trace_id,
            span_id,
            name: "skeleton".to_owned(),
            start_time_unix_nano: start,
            end_time_unix_nano: end,
            ..OtlpSpan::default()
        }
    }

    fn request_of(spans: Vec<OtlpSpan>) -> ExportTraceServiceRequest {
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                scope_spans: vec![ScopeSpans {
                    spans,
                    ..ScopeSpans::default()
                }],
                ..ResourceSpans::default()
            }],
        }
    }

    #[test]
    fn an_empty_request_rejects_nothing() {
        let normalized = normalize(&ExportTraceServiceRequest::default(), &context());
        assert!(normalized.spans.is_empty());
        assert!(normalized.media.is_empty());
        assert_eq!(normalized.rejections.total(), 0);
        assert_eq!(normalized.rejections.message(), None);

        let with_scope = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![KeyValue {
                        key: "service.name".to_owned(),
                        value: Some(AnyValue {
                            value: Some(
                                opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(
                                    "unused".to_owned(),
                                ),
                            ),
                        }),
                        ..KeyValue::default()
                    }],
                    ..Resource::default()
                }),
                scope_spans: vec![ScopeSpans::default()],
                ..ResourceSpans::default()
            }],
        };
        let normalized = normalize(&with_scope, &context());
        assert!(normalized.spans.is_empty());
        assert_eq!(normalized.rejections.total(), 0);
    }

    #[test]
    fn one_resource_is_shared_by_two_scopes() {
        let attributes = vec![
            KeyValue {
                key: "region".to_owned(),
                value: Some(AnyValue {
                    value: Some(
                        opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(
                            "eu".to_owned(),
                        ),
                    ),
                }),
                ..KeyValue::default()
            },
            KeyValue {
                key: "region".to_owned(),
                value: Some(AnyValue {
                    value: Some(
                        opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(
                            "us".to_owned(),
                        ),
                    ),
                }),
                ..KeyValue::default()
            },
            KeyValue {
                key: "service.name".to_owned(),
                value: Some(AnyValue {
                    value: Some(
                        opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(
                            "api".to_owned(),
                        ),
                    ),
                }),
                ..KeyValue::default()
            },
        ];
        let first = span_at(1_767_225_590_000_000_000, 1_767_225_591_000_000_000);
        let mut second = span_at(1_767_225_592_000_000_000, 1_767_225_593_000_000_000);
        second.span_id = vec![0x33; 8];
        let request = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes,
                    ..Resource::default()
                }),
                scope_spans: vec![
                    ScopeSpans {
                        scope: Some(InstrumentationScope {
                            name: "one".to_owned(),
                            version: "1".to_owned(),
                            ..InstrumentationScope::default()
                        }),
                        spans: vec![first],
                        ..ScopeSpans::default()
                    },
                    ScopeSpans {
                        scope: Some(InstrumentationScope {
                            name: String::new(),
                            version: String::new(),
                            ..InstrumentationScope::default()
                        }),
                        spans: vec![second],
                        ..ScopeSpans::default()
                    },
                ],
                ..ResourceSpans::default()
            }],
        };
        let normalized = normalize(&request, &context());
        assert_eq!(normalized.spans.len(), 2);
        assert_eq!(
            normalized.spans[0].resource_attributes,
            normalized.spans[1].resource_attributes
        );
        assert_eq!(
            normalized.spans[0]
                .resource_attributes
                .get("region")
                .map(String::as_str),
            Some("us")
        );
        assert!(
            !normalized.spans[0]
                .resource_attributes
                .contains_key("service.name")
        );
        assert_eq!(normalized.spans[0].service_name.as_deref(), Some("api"));
        assert_eq!(normalized.spans[1].service_name.as_deref(), Some("api"));
        assert_eq!(normalized.spans[0].scope_name.as_deref(), Some("one"));
        assert_eq!(normalized.spans[0].scope_version.as_deref(), Some("1"));
        assert_eq!(normalized.spans[1].scope_name, None);
        assert_eq!(normalized.spans[1].scope_version, None);
        let warning = normalized.spans[0]
            .attributes
            .get(metadata_key::NORMALIZE_WARNING)
            .map(String::as_str);
        assert_eq!(warning, Some("[\"duplicate_key\"]"));
        assert_eq!(
            normalized.spans[1]
                .attributes
                .get(metadata_key::NORMALIZE_WARNING),
            normalized.spans[0]
                .attributes
                .get(metadata_key::NORMALIZE_WARNING)
        );
        assert_eq!(normalized.spans[0].schema_version, 1);
        assert_eq!(normalized.spans[0].span_kind, SpanKind::Span);
        assert!(normalized.spans[0].conventions.is_empty());
    }

    #[test]
    fn a_span_with_two_bad_ids_counts_the_trace_id_only() {
        let mut span = span_at(1_767_225_590_000_000_000, 1_767_225_591_000_000_000);
        span.trace_id = vec![1, 2, 3];
        span.span_id = vec![0; 8];
        let normalized = normalize(&request_of(vec![span]), &context());
        assert!(normalized.spans.is_empty());
        assert_eq!(normalized.rejections.total(), 1);
        assert_eq!(normalized.rejections.count(RejectReason::InvalidTraceId), 1);
        assert_eq!(normalized.rejections.count(RejectReason::InvalidSpanId), 0);
    }

    #[test]
    fn an_event_time_past_i64_is_clamped_to_the_start() {
        let start = 1_767_225_590_000_000_000;
        let mut span = span_at(start, start + 1_000_000_000);
        span.events.push(Event {
            time_unix_nano: u64::MAX,
            name: "late".to_owned(),
            ..Event::default()
        });
        span.status = Some(Status {
            message: String::new(),
            code: 9,
        });
        let normalized = normalize(&request_of(vec![span]), &context());
        let span = &normalized.spans[0];
        assert_eq!(span.events.len(), 1);
        assert_eq!(span.events[0].time, start as i64);
        assert_eq!(span.events[0].name, "late");
        assert_eq!(span.events[0].attributes, None);
        assert_eq!(span.status, SpanStatus::Unset);
        assert_eq!(
            span.attributes
                .get(metadata_key::NORMALIZE_WARNING)
                .map(String::as_str),
            Some("[\"invalid_event_time\"]")
        );
    }

    #[test]
    fn a_wrong_length_parent_is_absent_and_a_bad_link_is_dropped() {
        let start = 1_767_225_590_000_000_000;
        let mut span = span_at(start, start + 1_000_000_000);
        span.parent_span_id = vec![1, 2, 3, 4];
        span.links.push(Link {
            trace_id: vec![1, 2],
            span_id: vec![0x44; 8],
            ..Link::default()
        });
        span.links.push(Link {
            trace_id: vec![0x55; 16],
            span_id: vec![0x66; 8],
            ..Link::default()
        });
        let normalized = normalize(&request_of(vec![span]), &context());
        let span = &normalized.spans[0];
        assert_eq!(span.parent_span_id, None);
        assert_eq!(span.links.len(), 1);
        assert_eq!(span.links[0].trace_id.as_bytes(), &[0x55; 16]);
        assert!(normalized.rejections.message().is_none());
    }

    #[test]
    fn conventions_follow_detection_order() {
        let mut span = span_at(1_767_225_590_000_000_000, 1_767_225_591_000_000_000);
        span.attributes = vec![
            string_attribute("ai.model.id", "gpt"),
            string_attribute("gen_ai.request.model", "gpt"),
        ];
        let normalized = normalize(&request_of(vec![span]), &context());
        assert_eq!(
            normalized.spans[0].conventions,
            vec![Convention::Genai, Convention::Vercel]
        );
    }

    fn string_attribute(key: &str, text: &str) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(AnyValue {
                value: Some(
                    opentelemetry_proto::tonic::common::v1::any_value::Value::StringValue(
                        text.to_owned(),
                    ),
                ),
            }),
            ..KeyValue::default()
        }
    }
}
