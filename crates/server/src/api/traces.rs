//! Trace and span response types.
//!
//! Spec 09 § Observability data and § Response shapes. `duration_ns` and `model` are computed here;
//! [`llmobs_core::span::Span`] does not store them (D60).

use std::collections::{BTreeMap, HashMap, HashSet};

use llmobs_core::enums::{
    Convention, CostSource, PayloadFormat, ScoreSource, SpanKind, SpanStatus,
};
use llmobs_core::ids::{SpanId, TraceId};
use llmobs_core::score::ScoreValue;
use llmobs_core::span::{Span, SpanEvent, SpanLink};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::common::{Limit, SortOrder, TimeParam};
use super::payload::Payload;

/// Most spans a trace detail response includes. Spec 09 § Response shapes.
pub const MAX_TRACE_SPANS: usize = 20_000;

/// The latest score of one name on a trace or session row.
///
/// Spec 09 § Response shapes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ScoreBrief {
    /// Score id.
    pub score_id: Uuid,
    /// Score name.
    pub name: String,
    /// Score value.
    pub value: ScoreValue,
    /// Who wrote the score.
    pub source: ScoreSource,
}

/// One row of the trace list.
///
/// Spec 01 § Trace and spec 09 § Observability data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TraceSummary {
    /// Trace id.
    pub trace_id: TraceId,
    /// Span id of the root. Omitted when the trace has no root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_span_id: Option<SpanId>,
    /// Whether a span with no parent is present.
    pub has_root: bool,
    /// Root span name, or the earliest span's name when there is no root.
    pub name: String,
    /// Earliest span start, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub start_time: i64,
    /// Latest span end, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub end_time: i64,
    /// `end_time - start_time`, nanoseconds.
    pub duration_ns: i64,
    /// `error` when any span errored, otherwise the root's status, otherwise `unset`.
    pub status: SpanStatus,
    /// Session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// End-user id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Deployment environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// Release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// Sorted distinct tags of every span.
    pub tags: Vec<String>,
    /// Root metadata, filled with keys from later spans when absent.
    pub metadata: BTreeMap<String, String>,
    /// Root input preview, or the first `llm` span's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_preview: Option<String>,
    /// Root output preview, or the first `llm` span's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<String>,
    /// Number of spans.
    pub span_count: i64,
    /// Number of spans with status `error`.
    pub error_count: i64,
    /// Number of `llm` spans.
    pub llm_count: i64,
    /// Sum of span input tokens. Omitted when every span omitted the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    /// Sum of span output tokens. Omitted when every span omitted the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    /// Sum of cache-read tokens. Omitted when every span omitted the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<i64>,
    /// Sum of cache-write tokens. Omitted when every span omitted the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<i64>,
    /// Sum of reasoning tokens. Omitted when every span omitted the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<i64>,
    /// Sum of total tokens. Omitted when every span omitted the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
    /// Sum of span costs, USD. Omitted when every span omitted the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_total: Option<f64>,
    /// Sorted distinct model names.
    pub models: Vec<String>,
    /// Latest score of each name attached to this trace.
    pub scores: Vec<ScoreBrief>,
}

/// Light columns of one span, for lists, trees, and the span stream.
///
/// Spec 09 § Response shapes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SpanSummary {
    /// Trace id.
    pub trace_id: TraceId,
    /// Span id.
    pub span_id: SpanId,
    /// Parent span. Omitted when this span is a root of the received trace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<SpanId>,
    /// Span name.
    pub name: String,
    /// Kind of work.
    pub span_kind: SpanKind,
    /// Start, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub start_time: i64,
    /// End, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub end_time: i64,
    /// `end_time - start_time`, nanoseconds. Saturates on overflow.
    pub duration_ns: i64,
    /// OTel status.
    pub status: SpanStatus,
    /// Status description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    /// Resource `service.name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// Session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// End-user id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Deployment environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// Release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// Tags.
    pub tags: Vec<String>,
    /// User metadata.
    pub metadata: BTreeMap<String, String>,
    /// Normalized provider name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Response model, or the request model when the response names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Input tokens, including cached tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    /// Output tokens, including reasoning tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    /// Cache-read tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<i64>,
    /// Cache-write tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<i64>,
    /// Reasoning tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<i64>,
    /// Total tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
    /// Total cost, USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_total: Option<f64>,
    /// Time to first token, nanoseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ns: Option<i64>,
    /// First 512 characters of the plain-text input rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_preview: Option<String>,
    /// First 512 characters of the plain-text output rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<String>,
}

/// One span in a flat pre-order trace tree.
///
/// Spec 09 § Response shapes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TreeSpan {
    /// Distance from the root. Roots are 0.
    pub depth: u32,
    /// The parent is missing, or this span sits in a parent cycle.
    pub detached: bool,
    /// Light columns of the span.
    pub span: SpanSummary,
}

/// Trace summary plus its span tree.
///
/// Spec 09 § Response shapes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TraceDetail {
    /// Trace row.
    pub trace: TraceSummary,
    /// Spans in pre-order. At most [`MAX_TRACE_SPANS`].
    pub spans: Vec<TreeSpan>,
    /// True when spans were omitted because the trace is larger than [`MAX_TRACE_SPANS`].
    pub truncated: bool,
}

/// One span event as returned by the API.
///
/// Absent stored attributes become `{}`. Spec 01 `events`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SpanEventView {
    /// Event time, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub time: i64,
    /// Event name.
    pub name: String,
    /// Event attributes. `{}` when the stored span had none.
    pub attributes: Value,
}

/// One span link as returned by the API.
///
/// Absent stored attributes become `{}`. Spec 01 `links`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SpanLinkView {
    /// Linked trace id.
    pub trace_id: TraceId,
    /// Linked span id.
    pub span_id: SpanId,
    /// Link attributes. `{}` when the stored span had none.
    pub attributes: Value,
}

/// Every stored span column except `project_id`, plus computed `duration_ns` and `model`.
///
/// `input_format` and `output_format` are the `format` tag of [`Payload`]. Spec 01 § Span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SpanDetail {
    /// Trace id.
    pub trace_id: TraceId,
    /// Span id.
    pub span_id: SpanId,
    /// Parent span. Omitted when this span is a root of the received trace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<SpanId>,
    /// Span name.
    pub name: String,
    /// Kind of work.
    pub span_kind: SpanKind,
    /// Start, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub start_time: i64,
    /// End, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub end_time: i64,
    /// `end_time - start_time`, nanoseconds. Saturates on overflow.
    pub duration_ns: i64,
    /// OTel status.
    pub status: SpanStatus,
    /// Status description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
    /// Resource `service.name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
    /// Session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// End-user id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Deployment environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    /// Release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<String>,
    /// Tags.
    pub tags: Vec<String>,
    /// User metadata.
    pub metadata: BTreeMap<String, String>,
    /// Normalized provider name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// Model named in the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_model: Option<String>,
    /// Model named in the response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    /// Response model, or the request model when the response names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// GenAI operation name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// Input tokens, including cached tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    /// Output tokens, including reasoning tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    /// Cache-read tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<i64>,
    /// Cache-write tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<i64>,
    /// Reasoning tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<i64>,
    /// Total tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
    /// Input cost, USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_input: Option<f64>,
    /// Output cost, USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_output: Option<f64>,
    /// Total cost, USD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_total: Option<f64>,
    /// How cost was obtained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_source: Option<CostSource>,
    /// Time to first token, nanoseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ns: Option<i64>,
    /// Request parameters. A JSON string when the stored text is not JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_params: Option<Value>,
    /// Parsed input. Omitted when the stored span has no input text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Payload>,
    /// Parsed output. Omitted when the stored span has no output text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Payload>,
    /// First 512 characters of the plain-text input rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_preview: Option<String>,
    /// First 512 characters of the plain-text output rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_preview: Option<String>,
    /// System instructions. A JSON string when the stored text is not JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_instructions: Option<Value>,
    /// Tool definitions. A JSON string when the stored text is not JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_definitions: Option<Value>,
    /// Span attributes not consumed by normalization.
    pub attributes: BTreeMap<String, String>,
    /// Resource attributes except `service.name`.
    pub resource_attributes: BTreeMap<String, String>,
    /// Instrumentation scope name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_name: Option<String>,
    /// Instrumentation scope version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_version: Option<String>,
    /// Span events.
    pub events: Vec<SpanEventView>,
    /// Span links.
    pub links: Vec<SpanLinkView>,
    /// SHA-256 hex of extracted media blobs.
    pub media_refs: Vec<String>,
    /// Names of attributes truncated at ingest.
    pub truncated: Vec<String>,
    /// Conventions detected on the span.
    pub conventions: Vec<Convention>,
    /// Server receive time, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub ingest_time: i64,
    /// Normalization version of this row.
    pub schema_version: u16,
}

impl From<&Span> for SpanSummary {
    fn from(span: &Span) -> Self {
        Self {
            trace_id: span.trace_id,
            span_id: span.span_id,
            parent_span_id: span.parent_span_id,
            name: span.name.clone(),
            span_kind: span.span_kind,
            start_time: span.start_time,
            end_time: span.end_time,
            duration_ns: span.end_time.saturating_sub(span.start_time),
            status: span.status,
            status_message: span.status_message.clone(),
            service_name: span.service_name.clone(),
            session_id: span.session_id.clone(),
            user_id: span.user_id.clone(),
            environment: span.environment.clone(),
            release: span.release.clone(),
            tags: span.tags.clone(),
            metadata: span.metadata.clone(),
            provider: span.provider.clone(),
            model: span.response_model.clone().or(span.request_model.clone()),
            input_tokens: span.input_tokens,
            output_tokens: span.output_tokens,
            cache_read_tokens: span.cache_read_tokens,
            cache_write_tokens: span.cache_write_tokens,
            reasoning_tokens: span.reasoning_tokens,
            total_tokens: span.total_tokens,
            cost_total: span.cost_total,
            ttft_ns: span.ttft_ns,
            input_preview: span.input_preview.clone(),
            output_preview: span.output_preview.clone(),
        }
    }
}

impl From<Span> for SpanDetail {
    fn from(span: Span) -> Self {
        let duration_ns = span.end_time.saturating_sub(span.start_time);
        let model = span.response_model.clone().or(span.request_model.clone());
        Self {
            trace_id: span.trace_id,
            span_id: span.span_id,
            parent_span_id: span.parent_span_id,
            name: span.name,
            span_kind: span.span_kind,
            start_time: span.start_time,
            end_time: span.end_time,
            duration_ns,
            status: span.status,
            status_message: span.status_message,
            service_name: span.service_name,
            session_id: span.session_id,
            user_id: span.user_id,
            environment: span.environment,
            release: span.release,
            tags: span.tags,
            metadata: span.metadata,
            provider: span.provider,
            request_model: span.request_model,
            response_model: span.response_model,
            model,
            operation: span.operation,
            input_tokens: span.input_tokens,
            output_tokens: span.output_tokens,
            cache_read_tokens: span.cache_read_tokens,
            cache_write_tokens: span.cache_write_tokens,
            reasoning_tokens: span.reasoning_tokens,
            total_tokens: span.total_tokens,
            cost_input: span.cost_input,
            cost_output: span.cost_output,
            cost_total: span.cost_total,
            cost_source: span.cost_source,
            ttft_ns: span.ttft_ns,
            request_params: span.request_params.map(parse_json_or_string),
            input: stored_payload(span.input_format, span.input),
            output: stored_payload(span.output_format, span.output),
            input_preview: span.input_preview,
            output_preview: span.output_preview,
            system_instructions: span.system_instructions.map(parse_json_or_string),
            tool_definitions: span.tool_definitions.map(parse_json_or_string),
            attributes: span.attributes,
            resource_attributes: span.resource_attributes,
            scope_name: span.scope_name,
            scope_version: span.scope_version,
            events: span.events.into_iter().map(event_view).collect(),
            links: span.links.into_iter().map(link_view).collect(),
            media_refs: span.media_refs,
            truncated: span.truncated,
            conventions: span.conventions,
            ingest_time: span.ingest_time,
            schema_version: span.schema_version,
        }
    }
}

/// Parses JSON text. Invalid text is returned as a JSON string, so the field still renders.
fn parse_json_or_string(text: String) -> Value {
    serde_json::from_str(&text).unwrap_or(Value::String(text))
}

pub(crate) fn stored_payload(
    format: Option<PayloadFormat>,
    text: Option<String>,
) -> Option<Payload> {
    match (format, text) {
        (Some(format), Some(text)) => Some(Payload::from_stored(format, text)),
        (None, Some(text)) => Some(Payload::Text(text)),
        (Some(_) | None, None) => None,
    }
}

fn event_view(event: SpanEvent) -> SpanEventView {
    SpanEventView {
        time: event.time,
        name: event.name,
        attributes: json_attributes(event.attributes),
    }
}

fn link_view(link: SpanLink) -> SpanLinkView {
    SpanLinkView {
        trace_id: link.trace_id,
        span_id: link.span_id,
        attributes: json_attributes(link.attributes),
    }
}

fn json_attributes(text: Option<String>) -> Value {
    text.map_or_else(|| Value::Object(Map::new()), parse_json_or_string)
}

/// Flat pre-order span tree.
///
/// Duplicate span ids keep the first row. A span whose parent is missing, or that sits in a
/// parent cycle, is a root with `detached: true`. Children of a cycle member stay under that
/// member. Spec 09 § Response shapes.
pub fn build_tree(spans: Vec<SpanSummary>) -> Vec<TreeSpan> {
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for span in spans {
        if seen.insert(span.span_id) {
            unique.push(span);
        }
    }

    let mut index_of = HashMap::with_capacity(unique.len());
    for (index, span) in unique.iter().enumerate() {
        index_of.insert(span.span_id, index);
    }
    let parent_index: Vec<Option<usize>> = unique
        .iter()
        .map(|span| {
            span.parent_span_id
                .and_then(|parent| index_of.get(&parent).copied())
        })
        .collect();
    let in_cycle = cycle_members(&parent_index);

    let mut children: HashMap<usize, Vec<usize>> = HashMap::new();
    let mut roots = Vec::new();
    for index in 0..unique.len() {
        if in_cycle[index] {
            continue;
        }
        match parent_index[index] {
            Some(parent) => children.entry(parent).or_default().push(index),
            None => {
                let detached = unique[index].parent_span_id.is_some();
                roots.push((index, detached));
            }
        }
    }

    let keys: Vec<(i64, SpanId)> = unique
        .iter()
        .map(|span| (span.start_time, span.span_id))
        .collect();
    for kids in children.values_mut() {
        kids.sort_by_key(|&index| keys[index]);
    }
    roots.sort_by_key(|&(index, _)| keys[index]);

    let mut slots: Vec<Option<SpanSummary>> = unique.into_iter().map(Some).collect();
    let mut emitted = vec![false; slots.len()];
    let mut out = Vec::with_capacity(slots.len());
    let mut stack = Vec::new();

    for (index, detached) in roots.into_iter().rev() {
        stack.push((index, 0, detached));
    }
    walk(&mut stack, &mut slots, &mut emitted, &children, &mut out);

    // What is left sits on a cycle or below one. Start from the cycle members only, so a
    // descendant of a cycle member stays under it even when it starts earlier. The final
    // `!in_cycle` group is empty by construction; it keeps the "every span once" guarantee.
    let mut rest: Vec<usize> = (0..slots.len()).filter(|&index| !emitted[index]).collect();
    rest.sort_by_key(|&index| (!in_cycle[index], keys[index]));
    for index in rest {
        if emitted[index] {
            continue;
        }
        stack.push((index, 0, true));
        walk(&mut stack, &mut slots, &mut emitted, &children, &mut out);
    }
    out
}

/// True for each span that sits on a parent cycle, including a self-parent.
fn cycle_members(parent_index: &[Option<usize>]) -> Vec<bool> {
    let mut color = vec![0_u8; parent_index.len()];
    let mut in_cycle = vec![false; parent_index.len()];
    for start in 0..parent_index.len() {
        if color[start] != 0 {
            continue;
        }
        let mut path = Vec::new();
        let mut current = start;
        loop {
            if color[current] == 2 {
                break;
            }
            if color[current] == 1 {
                let mut marking = false;
                for &node in &path {
                    if node == current {
                        marking = true;
                    }
                    if marking {
                        in_cycle[node] = true;
                    }
                }
                break;
            }
            color[current] = 1;
            path.push(current);
            match parent_index[current] {
                Some(parent) => current = parent,
                None => break,
            }
        }
        for node in path {
            if color[node] == 1 {
                color[node] = 2;
            }
        }
    }
    in_cycle
}

fn walk(
    stack: &mut Vec<(usize, u32, bool)>,
    slots: &mut [Option<SpanSummary>],
    emitted: &mut [bool],
    children: &HashMap<usize, Vec<usize>>,
    out: &mut Vec<TreeSpan>,
) {
    while let Some((index, depth, detached)) = stack.pop() {
        if emitted[index] {
            continue;
        }
        let Some(span) = slots[index].take() else {
            continue;
        };
        emitted[index] = true;
        if let Some(kids) = children.get(&index) {
            for &child in kids.iter().rev() {
                stack.push((child, depth.saturating_add(1), false));
            }
        }
        out.push(TreeSpan {
            depth,
            detached,
            span,
        });
    }
}

/// Sort key for trace lists.
///
/// Spec 09 § Observability data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TraceSort {
    /// `start_time`.
    #[default]
    StartTime,
    /// `duration_ns`.
    Duration,
    /// `cost_total`.
    CostTotal,
    /// `total_tokens`.
    TotalTokens,
}

/// Sort key for span lists.
///
/// Spec 09 § Observability data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SpanSort {
    /// `start_time`.
    #[default]
    StartTime,
    /// `duration_ns`.
    Duration,
    /// `cost_total`.
    CostTotal,
    /// `total_tokens`.
    TotalTokens,
}

/// Query parameters of `GET /api/v1/projects/{project}/traces`.
///
/// Spec 09 § Observability data.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct TraceListParams {
    /// Filter DSL expression over the `traces` view (spec 05).
    #[serde(default)]
    pub filter: Option<String>,
    /// Full-text search (spec 05 § Full-text search).
    #[serde(default)]
    pub search: Option<String>,
    /// Inclusive range start. RFC 3339 or epoch milliseconds.
    #[serde(default)]
    pub from: Option<TimeParam>,
    /// Exclusive range end. RFC 3339 or epoch milliseconds.
    #[serde(default)]
    pub to: Option<TimeParam>,
    /// Sort key. Defaults to start time.
    #[serde(default)]
    pub sort: TraceSort,
    /// Sort direction. Defaults to descending.
    #[serde(default)]
    pub order: SortOrder,
    /// Maximum number of items to return (1–1000, default 50).
    #[serde(default)]
    #[param(minimum = 1, maximum = 1000, default = 50)]
    pub limit: Limit,
    /// Opaque cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Include traces from the evaluation environment. Default false. Spec 05 § Default exclusions.
    #[serde(default)]
    pub include_eval: bool,
    /// Include experiment traces. Default false. Spec 05 § Default exclusions.
    #[serde(default)]
    pub include_experiments: bool,
}

/// Query parameters of `GET /api/v1/projects/{project}/traces/stream`.
///
/// Spec 09 § Observability data.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct TraceStreamParams {
    /// Filter DSL expression over the light columns of the `traces` view (spec 05).
    #[serde(default)]
    pub filter: Option<String>,
    /// Full-text search (spec 05 § Full-text search). A trace is sent once any of its spans
    /// matches, then on every later update while the stream stays open.
    #[serde(default)]
    pub search: Option<String>,
    /// Include traces from the evaluation environment. Default false. Spec 05 § Default exclusions.
    #[serde(default)]
    pub include_eval: bool,
    /// Include experiment traces. Default false. Spec 05 § Default exclusions.
    #[serde(default)]
    pub include_experiments: bool,
}

/// Query parameters of `GET /api/v1/projects/{project}/traces/{trace_id}`.
///
/// Spec 09 § Observability data.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct TraceDetailParams {
    /// Instant the client is looking at. RFC 3339 or epoch milliseconds.
    #[serde(default)]
    pub around: Option<TimeParam>,
}

/// Query parameters of `GET /api/v1/projects/{project}/spans`.
///
/// Spec 09 § Observability data.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct SpanListParams {
    /// Filter DSL expression over the `spans` view (spec 05).
    #[serde(default)]
    pub filter: Option<String>,
    /// Full-text search (spec 05 § Full-text search).
    #[serde(default)]
    pub search: Option<String>,
    /// Inclusive range start. RFC 3339 or epoch milliseconds.
    #[serde(default)]
    pub from: Option<TimeParam>,
    /// Exclusive range end. RFC 3339 or epoch milliseconds.
    #[serde(default)]
    pub to: Option<TimeParam>,
    /// Sort key. Defaults to start time.
    #[serde(default)]
    pub sort: SpanSort,
    /// Sort direction. Defaults to descending.
    #[serde(default)]
    pub order: SortOrder,
    /// Maximum number of items to return (1–1000, default 50).
    #[serde(default)]
    #[param(minimum = 1, maximum = 1000, default = 50)]
    pub limit: Limit,
    /// Opaque cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Include spans from the evaluation environment. Default false. Spec 05 § Default exclusions.
    #[serde(default)]
    pub include_eval: bool,
    /// Include experiment spans. Default false. Spec 05 § Default exclusions.
    #[serde(default)]
    pub include_experiments: bool,
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap, HashSet};

    use llmobs_core::enums::{PayloadFormat, SpanKind, SpanStatus};
    use llmobs_core::ids::{SpanId, TraceId};
    use llmobs_core::span::{Span, SpanEvent, SpanLink};
    use serde::Serialize;
    use serde_json::Value;

    use super::{
        SpanDetail, SpanListParams, SpanSort, SpanSummary, TraceListParams, TraceSort, build_tree,
        parse_json_or_string,
    };
    use crate::api::common::{Limit, SortOrder};
    use crate::api::payload::Payload;

    fn snap(value: &impl Serialize) -> String {
        serde_json::to_string_pretty(value).unwrap()
    }

    fn span_id(n: u64) -> SpanId {
        SpanId::from_bytes(n.to_be_bytes()).unwrap()
    }

    fn trace_id() -> TraceId {
        TraceId::from_bytes([0xa1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]).unwrap()
    }

    fn summary(id: u64, parent: Option<u64>, start: i64) -> SpanSummary {
        SpanSummary {
            trace_id: trace_id(),
            span_id: span_id(id),
            parent_span_id: parent.map(span_id),
            name: format!("s{id}"),
            span_kind: SpanKind::Span,
            start_time: start,
            end_time: start,
            duration_ns: 0,
            status: SpanStatus::Ok,
            status_message: None,
            service_name: None,
            session_id: None,
            user_id: None,
            environment: None,
            release: None,
            tags: Vec::new(),
            metadata: BTreeMap::new(),
            provider: None,
            model: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            cost_total: None,
            ttft_ns: None,
            input_preview: None,
            output_preview: None,
        }
    }

    fn minimal_span() -> Span {
        serde_json::from_str(
            r#"{
                "project_id": "01900000-0000-7000-8000-000000000001",
                "trace_id": "a1000000000000000000000000000001",
                "span_id": "b100000000000011",
                "name": "root",
                "span_kind": "span",
                "start_time": "2026-01-01T10:00:00.000000000Z",
                "end_time": "2026-01-01T10:00:00.000000000Z",
                "status": "unset",
                "ingest_time": "2026-01-01T10:00:00.000000000Z",
                "schema_version": 1
            }"#,
        )
        .unwrap()
    }

    #[test]
    fn span_conversion_snapshots() {
        let mut full: Span = {
            #[derive(serde::Deserialize)]
            struct Fixture {
                spans: Vec<Span>,
            }
            let fixture: Fixture = serde_json::from_str(include_str!(
                "../../../../fixtures/otlp/genai/chat-cache-tokens/expected.json"
            ))
            .unwrap();
            fixture.spans.into_iter().next().unwrap()
        };
        full.parent_span_id = Some("b1000000000000ff".parse().unwrap());
        full.status_message = Some("ok".to_owned());
        full.tags = vec!["support".to_owned()];
        full.metadata.insert("priority".to_owned(), "1".to_owned());
        full.reasoning_tokens = Some(12);
        full.ttft_ns = Some(600_000_000);
        full.tool_definitions = Some(r#"[{"name":"lookup"}]"#.to_owned());
        full.events = vec![SpanEvent {
            time: full.start_time,
            name: "gen_ai.choice".to_owned(),
            attributes: Some(r#"{"index":0}"#.to_owned()),
        }];
        full.links = vec![SpanLink {
            trace_id: full.trace_id,
            span_id: full.span_id,
            attributes: None,
        }];
        full.media_refs =
            vec!["497790947d4666760ce38f3c00e852c71fdb66cae849bae8e9ede352719e1581".to_owned()];
        full.truncated = vec!["gen_ai.input.messages".to_owned()];

        let detail = SpanDetail::from(full.clone());
        let summary = SpanSummary::from(&full);
        assert_eq!(detail.model.as_deref(), Some("claude-sonnet-4-5-20250929"));
        assert_eq!(summary.model.as_deref(), Some("claude-sonnet-4-5-20250929"));
        assert_eq!(
            detail.duration_ns,
            full.end_time.saturating_sub(full.start_time)
        );
        assert_eq!(detail.links[0].attributes, serde_json::json!({}));

        let minimal_detail = SpanDetail::from(minimal_span());
        let minimal_summary = SpanSummary::from(&minimal_span());
        assert!(minimal_detail.status_message.is_none());
        assert!(minimal_detail.tags.is_empty());
        assert!(minimal_detail.metadata.is_empty());
        assert!(minimal_detail.events.is_empty());
        assert!(minimal_summary.tags.is_empty());
        let minimal_json = serde_json::to_value(&minimal_detail).unwrap();
        assert!(minimal_json.get("status_message").is_none());
        assert_eq!(minimal_json["tags"], serde_json::json!([]));
        assert_eq!(minimal_json["metadata"], serde_json::json!({}));
        assert_eq!(minimal_json["events"], serde_json::json!([]));

        insta::with_settings!({
            snapshot_path => "../../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!("span_detail_full", snap(&detail));
            insta::assert_snapshot!("span_summary_full", snap(&summary));
            insta::assert_snapshot!("span_detail_minimal", snap(&minimal_detail));
            insta::assert_snapshot!("span_summary_minimal", snap(&minimal_summary));
        });
    }

    #[test]
    fn payload_and_model_conversion() {
        let mut text_only = minimal_span();
        text_only.input = Some("hello".to_owned());
        text_only.input_format = None;
        assert_eq!(
            SpanDetail::from(text_only).input,
            Some(Payload::Text("hello".to_owned()))
        );

        let mut format_only = minimal_span();
        format_only.output = None;
        format_only.output_format = Some(PayloadFormat::Messages);
        assert!(SpanDetail::from(format_only).output.is_none());

        let mut requested = minimal_span();
        requested.request_model = Some("requested".to_owned());
        requested.start_time = 10;
        requested.end_time = 3;
        let summary = SpanSummary::from(&requested);
        assert_eq!(summary.model.as_deref(), Some("requested"));
        assert_eq!(summary.duration_ns, 3_i64.saturating_sub(10));
        requested.response_model = Some("responded".to_owned());
        assert_eq!(
            SpanDetail::from(requested).model.as_deref(),
            Some("responded")
        );
    }

    #[test]
    fn parse_json_or_string_keeps_invalid_text() {
        assert_eq!(
            parse_json_or_string(r#"{"a":1}"#.to_owned()),
            serde_json::json!({"a": 1})
        );
        assert_eq!(
            parse_json_or_string("nope".to_owned()),
            Value::String("nope".to_owned())
        );
    }

    fn assert_tree(input: Vec<SpanSummary>, check: impl FnOnce(&[super::TreeSpan])) {
        let distinct = input
            .iter()
            .map(|span| span.span_id)
            .collect::<HashSet<_>>()
            .len();
        let output = build_tree(input);
        assert_eq!(output.len(), distinct);
        let position: HashMap<SpanId, usize> = output
            .iter()
            .enumerate()
            .map(|(index, node)| (node.span.span_id, index))
            .collect();
        for (index, node) in output.iter().enumerate() {
            if node.depth == 0 || node.detached {
                continue;
            }
            let parent = node.span.parent_span_id.unwrap();
            let parent_at = position[&parent];
            assert!(parent_at < index);
            assert_eq!(output[parent_at].depth + 1, node.depth);
        }
        check(&output);
    }

    fn shape(nodes: &[super::TreeSpan]) -> Vec<(u64, u32, bool)> {
        nodes
            .iter()
            .map(|node| {
                let id = u64::from_be_bytes(*node.span.span_id.as_bytes());
                (id, node.depth, node.detached)
            })
            .collect()
    }

    #[test]
    fn build_tree_orders_a_deep_tree() {
        let input = vec![
            summary(3, Some(1), 20),
            summary(5, Some(4), 16),
            summary(1, None, 0),
            summary(4, Some(2), 15),
            summary(2, Some(1), 10),
        ];
        assert_tree(input, |nodes| {
            assert_eq!(
                shape(nodes),
                vec![
                    (1, 0, false),
                    (2, 1, false),
                    (4, 2, false),
                    (5, 3, false),
                    (3, 1, false),
                ]
            );
        });
    }

    #[test]
    fn build_tree_detaches_orphans_and_orders_roots() {
        assert_tree(
            vec![summary(1, Some(99), 0), summary(2, Some(1), 1)],
            |nodes| {
                assert_eq!(shape(nodes), vec![(1, 0, true), (2, 1, false)]);
            },
        );
        assert_tree(
            vec![
                summary(2, None, 5),
                summary(1, None, 5),
                summary(3, None, 4),
            ],
            |nodes| {
                assert_eq!(
                    shape(nodes),
                    vec![(3, 0, false), (1, 0, false), (2, 0, false)]
                );
            },
        );
    }

    #[test]
    fn build_tree_keeps_the_first_duplicate() {
        let mut second = summary(1, None, 0);
        second.name = "second".to_owned();
        let mut first = summary(1, None, 0);
        first.name = "first".to_owned();
        assert_tree(vec![first, second], |nodes| {
            assert_eq!(nodes.len(), 1);
            assert_eq!(nodes[0].span.name, "first");
        });
    }

    #[test]
    fn build_tree_emits_cycles_as_detached_roots() {
        // 1 ↔ 2 is a cycle. 3 is a self-parent. 4 hangs off 3 and stays in its subtree.
        let input = vec![
            summary(1, Some(2), 0),
            summary(2, Some(1), 1),
            summary(3, Some(3), 2),
            summary(4, Some(3), 3),
        ];
        assert_tree(input, |nodes| {
            assert_eq!(
                shape(nodes),
                vec![(1, 0, true), (2, 0, true), (3, 0, true), (4, 1, false)]
            );
        });
    }

    #[test]
    fn build_tree_keeps_an_early_child_under_its_cycle_member() {
        // 1 ↔ 2 is a cycle. 3 is 1's child and starts before both.
        let input = vec![
            summary(1, Some(2), 1),
            summary(2, Some(1), 2),
            summary(3, Some(1), 0),
        ];
        assert_tree(input, |nodes| {
            assert_eq!(
                shape(nodes),
                vec![(1, 0, true), (3, 1, false), (2, 0, true)]
            );
        });
    }

    #[test]
    fn build_tree_walks_a_twenty_thousand_chain() {
        let input: Vec<_> = (1..=20_000)
            .map(|id| {
                summary(
                    id,
                    if id == 1 { None } else { Some(id - 1) },
                    i64::try_from(id).unwrap(),
                )
            })
            .collect();
        assert_tree(input, |nodes| {
            assert_eq!(nodes.len(), 20_000);
            assert_eq!(nodes.last().unwrap().depth, 19_999);
            assert!(!nodes.last().unwrap().detached);
        });
    }

    #[test]
    fn build_tree_empty() {
        assert!(build_tree(Vec::new()).is_empty());
    }

    #[test]
    fn list_params_default_and_reject_unknown_fields() {
        let params: TraceListParams = serde_urlencoded::from_str("").unwrap();
        assert_eq!(params.sort, TraceSort::StartTime);
        assert_eq!(params.order, SortOrder::Desc);
        assert_eq!(params.limit, Limit::default());
        assert!(!params.include_eval);
        assert!(!params.include_experiments);
        assert!(serde_urlencoded::from_str::<TraceListParams>("nope=1").is_err());

        let spans: SpanListParams =
            serde_urlencoded::from_str("sort=cost_total&include_eval=true").unwrap();
        assert_eq!(spans.sort, SpanSort::CostTotal);
        assert!(spans.include_eval);
    }
}
