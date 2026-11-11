//! Merge, re-mint, shift, and prune one capture into a stable export.
//!
//! The steps run in that order. Two captures of the same scenario differ in
//! their original IDs, their absolute clock, and how the SDK batched the
//! exports; this function gives them one JSON shape.

use std::collections::HashMap;

use chrono::DateTime;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value;
use opentelemetry_proto::tonic::common::v1::{AnyValue, InstrumentationScope, KeyValue};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};
use sha2::{Digest, Sha256};

use crate::error::CaptureError;
use crate::scenario::{CasePath, TimestampAttribute, TimestampFormat};

const TEN_SECONDS_NS: i64 = 10_000_000_000;
const NS_PER_MS: i64 = 1_000_000;

/// The four determinism steps, in order.
pub fn deterministic(
    exports: Vec<ExportTraceServiceRequest>,
    case: &CasePath,
    ingest_time: i64,
    timestamps: &[TimestampAttribute],
) -> Result<ExportTraceServiceRequest, CaptureError> {
    let mut groups = merge(exports);
    remint(&mut groups, &case.to_string());
    shift(&mut groups, ingest_time, timestamps)?;
    prune(&mut groups);
    Ok(to_request(groups))
}

struct Carried {
    resource: Resource,
    resource_schema: String,
    scope: InstrumentationScope,
    scope_schema: String,
    span: Span,
}

struct ScopeGroup {
    scope: InstrumentationScope,
    schema_url: String,
    spans: Vec<Span>,
}

struct ResourceGroup {
    resource: Resource,
    schema_url: String,
    scopes: Vec<ScopeGroup>,
}

fn merge(exports: Vec<ExportTraceServiceRequest>) -> Vec<ResourceGroup> {
    let mut carried = Vec::new();
    for export in exports {
        for resource_spans in export.resource_spans {
            let resource = resource_spans.resource.unwrap_or_default();
            let resource_schema = resource_spans.schema_url;
            for scope_spans in resource_spans.scope_spans {
                let scope = scope_spans.scope.unwrap_or_default();
                let scope_schema = scope_spans.schema_url;
                for span in scope_spans.spans {
                    carried.push(Carried {
                        resource: resource.clone(),
                        resource_schema: resource_schema.clone(),
                        scope: scope.clone(),
                        scope_schema: scope_schema.clone(),
                        span,
                    });
                }
            }
        }
    }
    // Stable: equal (start, end, name) keys keep the order the exports arrived in.
    carried.sort_by(|left, right| {
        (
            &left.span.start_time_unix_nano,
            &left.span.end_time_unix_nano,
            &left.span.name,
        )
            .cmp(&(
                &right.span.start_time_unix_nano,
                &right.span.end_time_unix_nano,
                &right.span.name,
            ))
    });

    let mut groups: Vec<ResourceGroup> = Vec::new();
    for item in carried {
        let resource_index = match groups.iter().position(|group| {
            group.resource.attributes == item.resource.attributes
                && group.schema_url == item.resource_schema
        }) {
            Some(index) => index,
            None => {
                groups.push(ResourceGroup {
                    resource: item.resource.clone(),
                    schema_url: item.resource_schema.clone(),
                    scopes: Vec::new(),
                });
                groups.len() - 1
            }
        };
        let scope_index = match groups[resource_index].scopes.iter().position(|group| {
            group.scope.name == item.scope.name
                && group.scope.version == item.scope.version
                && group.scope.attributes == item.scope.attributes
                && group.schema_url == item.scope_schema
        }) {
            Some(index) => index,
            None => {
                groups[resource_index].scopes.push(ScopeGroup {
                    scope: item.scope.clone(),
                    schema_url: item.scope_schema.clone(),
                    spans: Vec::new(),
                });
                groups[resource_index].scopes.len() - 1
            }
        };
        groups[resource_index].scopes[scope_index]
            .spans
            .push(item.span);
    }
    groups
}

fn remint(groups: &mut [ResourceGroup], case: &str) {
    let mut traces: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
    let mut spans: HashMap<Vec<u8>, Vec<u8>> = HashMap::new();
    let mut trace_k = 0_u64;
    let mut span_k = 0_u64;
    for group in groups.iter_mut() {
        for scope in &mut group.scopes {
            for span in &mut scope.spans {
                // The span's own IDs are assigned before its parent, then its links.
                span.trace_id = assign(&mut traces, &mut trace_k, &span.trace_id, case, "trace");
                span.span_id = assign(&mut spans, &mut span_k, &span.span_id, case, "span");
                span.parent_span_id =
                    assign(&mut spans, &mut span_k, &span.parent_span_id, case, "span");
                for link in &mut span.links {
                    link.trace_id =
                        assign(&mut traces, &mut trace_k, &link.trace_id, case, "trace");
                    link.span_id = assign(&mut spans, &mut span_k, &link.span_id, case, "span");
                }
            }
        }
    }
    let patterns = replacement_patterns(&traces, &spans);
    if patterns.is_empty() {
        return;
    }
    for group in groups.iter_mut() {
        rewrite_attributes(&mut group.resource.attributes, &patterns);
        for scope in &mut group.scopes {
            rewrite_attributes(&mut scope.scope.attributes, &patterns);
            for span in &mut scope.spans {
                rewrite_attributes(&mut span.attributes, &patterns);
                for event in &mut span.events {
                    rewrite_attributes(&mut event.attributes, &patterns);
                }
                for link in &mut span.links {
                    rewrite_attributes(&mut link.attributes, &patterns);
                }
            }
        }
    }
}

/// An all-zero or empty ID is absent. A root parent is empty, so it is not re-minted.
fn present(id: &[u8]) -> bool {
    id.iter().any(|byte| *byte != 0)
}

fn assign(
    map: &mut HashMap<Vec<u8>, Vec<u8>>,
    counter: &mut u64,
    id: &[u8],
    case: &str,
    kind: &str,
) -> Vec<u8> {
    if !present(id) {
        return Vec::new();
    }
    if let Some(minted) = map.get(id) {
        return minted.clone();
    }
    let minted = mint(case, kind, *counter, if kind == "trace" { 16 } else { 8 });
    *counter += 1;
    map.insert(id.to_vec(), minted.clone());
    minted
}

fn mint(case: &str, kind: &str, k: u64, len: usize) -> Vec<u8> {
    let material = format!("llmobs-capture\0{case}\0{kind}\0{k}");
    let digest = Sha256::digest(material.as_bytes());
    let mut id = digest[..len].to_vec();
    if id.iter().all(|byte| *byte == 0) {
        id.fill(0);
        let value = k.saturating_add(1_000_000).to_be_bytes();
        let start = len - value.len();
        id[start..].copy_from_slice(&value);
    }
    id
}

fn replacement_patterns(
    traces: &HashMap<Vec<u8>, Vec<u8>>,
    spans: &HashMap<Vec<u8>, Vec<u8>>,
) -> Vec<(String, String)> {
    let mut patterns: Vec<(String, String)> = traces
        .iter()
        .chain(spans.iter())
        .map(|(original, minted)| (hex::encode(original), hex::encode(minted)))
        .collect();
    // A span id is 16 hex chars and a trace id is 32. Longer originals win so a
    // span id that happens to sit inside a trace id hex does not split it.
    patterns.sort_by(|left, right| {
        right
            .0
            .len()
            .cmp(&left.0.len())
            .then_with(|| left.0.cmp(&right.0))
    });
    patterns
}

fn rewrite_attributes(attributes: &mut [KeyValue], patterns: &[(String, String)]) {
    for attribute in attributes {
        if let Some(value) = attribute.value.as_mut() {
            rewrite_strings(value, patterns);
        }
    }
}

fn rewrite_strings(value: &mut AnyValue, patterns: &[(String, String)]) {
    match &mut value.value {
        Some(Value::StringValue(text)) => *text = replace_ids(text, patterns),
        Some(Value::ArrayValue(array)) => {
            for item in &mut array.values {
                rewrite_strings(item, patterns);
            }
        }
        Some(Value::KvlistValue(list)) => rewrite_attributes(&mut list.values, patterns),
        _ => {}
    }
}

fn replace_ids(input: &str, patterns: &[(String, String)]) -> String {
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        let matched = patterns
            .iter()
            .find(|(original, _)| lower.as_bytes()[index..].starts_with(original.as_bytes()));
        if let Some((original, minted)) = matched {
            out.push_str(minted);
            index += original.len();
            continue;
        }
        let Some(ch) = input[index..].chars().next() else {
            break;
        };
        out.push(ch);
        index += ch.len_utf8();
    }
    out
}

fn shift(
    groups: &mut [ResourceGroup],
    ingest_time: i64,
    timestamps: &[TimestampAttribute],
) -> Result<(), CaptureError> {
    let Some(min_start) = earliest_start(groups)? else {
        return Ok(());
    };
    let base = ingest_time
        .checked_sub(TEN_SECONDS_NS)
        .ok_or(CaptureError::WindowExceeded { span_ms: i64::MAX })?;
    let mut furthest_ms = 0_i64;
    for group in groups.iter() {
        visit_group_timestamps(
            &group.resource.attributes,
            timestamps,
            min_start,
            base,
            &mut furthest_ms,
        )?;
        for scope in &group.scopes {
            visit_group_timestamps(
                &scope.scope.attributes,
                timestamps,
                min_start,
                base,
                &mut furthest_ms,
            )?;
            for span in &scope.spans {
                record(
                    as_ns(span.start_time_unix_nano)?,
                    min_start,
                    base,
                    &mut furthest_ms,
                    false,
                )?;
                record(
                    as_ns(span.end_time_unix_nano)?,
                    min_start,
                    base,
                    &mut furthest_ms,
                    false,
                )?;
                for event in &span.events {
                    record(
                        as_ns(event.time_unix_nano)?,
                        min_start,
                        base,
                        &mut furthest_ms,
                        true,
                    )?;
                }
                visit_group_timestamps(
                    &span.attributes,
                    timestamps,
                    min_start,
                    base,
                    &mut furthest_ms,
                )?;
                for event in &span.events {
                    visit_group_timestamps(
                        &event.attributes,
                        timestamps,
                        min_start,
                        base,
                        &mut furthest_ms,
                    )?;
                }
                for link in &span.links {
                    visit_group_timestamps(
                        &link.attributes,
                        timestamps,
                        min_start,
                        base,
                        &mut furthest_ms,
                    )?;
                }
            }
        }
    }
    if furthest_ms >= 10_000 {
        return Err(CaptureError::WindowExceeded {
            span_ms: furthest_ms,
        });
    }
    for group in groups.iter_mut() {
        apply_timestamps(&mut group.resource.attributes, timestamps, min_start, base)?;
        for scope in &mut group.scopes {
            apply_timestamps(&mut scope.scope.attributes, timestamps, min_start, base)?;
            for span in &mut scope.spans {
                span.start_time_unix_nano =
                    shifted_u64(span.start_time_unix_nano, min_start, base)?;
                span.end_time_unix_nano = shifted_u64(span.end_time_unix_nano, min_start, base)?;
                for event in &mut span.events {
                    event.time_unix_nano = shifted_u64(event.time_unix_nano, min_start, base)?;
                }
                apply_timestamps(&mut span.attributes, timestamps, min_start, base)?;
                for event in &mut span.events {
                    apply_timestamps(&mut event.attributes, timestamps, min_start, base)?;
                }
                for link in &mut span.links {
                    apply_timestamps(&mut link.attributes, timestamps, min_start, base)?;
                }
            }
        }
    }
    Ok(())
}

fn earliest_start(groups: &[ResourceGroup]) -> Result<Option<i64>, CaptureError> {
    let Some(min) = groups
        .iter()
        .flat_map(|group| group.scopes.iter())
        .flat_map(|scope| scope.spans.iter())
        .map(|span| span.start_time_unix_nano)
        .min()
    else {
        return Ok(None);
    };
    Ok(Some(as_ns(min)?))
}

fn as_ns(time: u64) -> Result<i64, CaptureError> {
    i64::try_from(time).map_err(|_| CaptureError::WindowExceeded { span_ms: i64::MAX })
}

/// `guard_before` rejects an event or timestamp attribute earlier than the first span.
fn record(
    time: i64,
    min_start: i64,
    base: i64,
    furthest_ms: &mut i64,
    guard_before: bool,
) -> Result<(), CaptureError> {
    if guard_before && time < min_start {
        return Err(CaptureError::InstantBeforeStart);
    }
    let delta = time
        .checked_sub(min_start)
        .ok_or(CaptureError::WindowExceeded { span_ms: i64::MAX })?;
    let rounded =
        round_half_away(delta).ok_or(CaptureError::WindowExceeded { span_ms: i64::MAX })?;
    let at = base
        .checked_add(rounded)
        .ok_or(CaptureError::WindowExceeded { span_ms: i64::MAX })?;
    let _ = at;
    let ms = rounded / NS_PER_MS;
    if ms > *furthest_ms {
        *furthest_ms = ms;
    }
    Ok(())
}

/// Round to a whole millisecond, half away from zero. The result is nanoseconds.
fn round_half_away(delta_ns: i64) -> Option<i64> {
    let negative = delta_ns < 0;
    let abs = delta_ns.unsigned_abs();
    let whole = abs / u64::try_from(NS_PER_MS).ok()?;
    let remainder = abs % u64::try_from(NS_PER_MS).ok()?;
    let rounded = if remainder >= 500_000 {
        whole + 1
    } else {
        whole
    };
    let mut nanos = i64::try_from(rounded).ok()?.checked_mul(NS_PER_MS)?;
    if negative {
        nanos = nanos.checked_neg()?;
    }
    Some(nanos)
}

fn shifted_u64(time: u64, min_start: i64, base: i64) -> Result<u64, CaptureError> {
    let shifted = shifted_i64(as_ns(time)?, min_start, base)?;
    u64::try_from(shifted).map_err(|_| CaptureError::InstantBeforeStart)
}

fn shifted_i64(time: i64, min_start: i64, base: i64) -> Result<i64, CaptureError> {
    let delta = time
        .checked_sub(min_start)
        .ok_or(CaptureError::WindowExceeded { span_ms: i64::MAX })?;
    let rounded =
        round_half_away(delta).ok_or(CaptureError::WindowExceeded { span_ms: i64::MAX })?;
    base.checked_add(rounded)
        .ok_or(CaptureError::WindowExceeded { span_ms: i64::MAX })
}

fn visit_group_timestamps(
    attributes: &[KeyValue],
    timestamps: &[TimestampAttribute],
    min_start: i64,
    base: i64,
    furthest_ms: &mut i64,
) -> Result<(), CaptureError> {
    visit_timestamp_keys(attributes, timestamps, &mut |key, value| {
        let time = read_timestamp(key, value, spec_format(timestamps, key)?)?;
        record(time, min_start, base, furthest_ms, true)
    })
}

fn apply_timestamps(
    attributes: &mut [KeyValue],
    timestamps: &[TimestampAttribute],
    min_start: i64,
    base: i64,
) -> Result<(), CaptureError> {
    visit_timestamp_keys_mut(attributes, timestamps, &mut |key, value| {
        let format = spec_format(timestamps, key)?;
        let time = read_timestamp(key, value, format)?;
        let shifted = shifted_i64(time, min_start, base)?;
        write_timestamp(key, value, format, shifted)
    })
}

fn spec_format(
    timestamps: &[TimestampAttribute],
    key: &str,
) -> Result<TimestampFormat, CaptureError> {
    timestamps
        .iter()
        .find(|spec| spec.key == key)
        .map(|spec| spec.format)
        .ok_or_else(|| CaptureError::TimestampAttribute {
            key: key.to_owned(),
        })
}

fn visit_timestamp_keys(
    attributes: &[KeyValue],
    timestamps: &[TimestampAttribute],
    visit: &mut impl FnMut(&str, &AnyValue) -> Result<(), CaptureError>,
) -> Result<(), CaptureError> {
    for attribute in attributes {
        if timestamps.iter().any(|spec| spec.key == attribute.key)
            && let Some(value) = attribute.value.as_ref()
        {
            visit(&attribute.key, value)?;
        }
        if let Some(value) = attribute.value.as_ref() {
            descend_timestamp_keys(value, timestamps, visit)?;
        }
    }
    Ok(())
}

fn descend_timestamp_keys(
    value: &AnyValue,
    timestamps: &[TimestampAttribute],
    visit: &mut impl FnMut(&str, &AnyValue) -> Result<(), CaptureError>,
) -> Result<(), CaptureError> {
    match &value.value {
        Some(Value::ArrayValue(array)) => {
            for item in &array.values {
                descend_timestamp_keys(item, timestamps, visit)?;
            }
        }
        Some(Value::KvlistValue(list)) => visit_timestamp_keys(&list.values, timestamps, visit)?,
        _ => {}
    }
    Ok(())
}

fn visit_timestamp_keys_mut(
    attributes: &mut [KeyValue],
    timestamps: &[TimestampAttribute],
    visit: &mut impl FnMut(&str, &mut AnyValue) -> Result<(), CaptureError>,
) -> Result<(), CaptureError> {
    for attribute in attributes.iter_mut() {
        let listed = timestamps.iter().any(|spec| spec.key == attribute.key);
        if let Some(value) = attribute.value.as_mut() {
            if listed {
                visit(&attribute.key, value)?;
            }
            descend_timestamp_keys_mut(value, timestamps, visit)?;
        }
    }
    Ok(())
}

fn descend_timestamp_keys_mut(
    value: &mut AnyValue,
    timestamps: &[TimestampAttribute],
    visit: &mut impl FnMut(&str, &mut AnyValue) -> Result<(), CaptureError>,
) -> Result<(), CaptureError> {
    match &mut value.value {
        Some(Value::ArrayValue(array)) => {
            for item in &mut array.values {
                descend_timestamp_keys_mut(item, timestamps, visit)?;
            }
        }
        Some(Value::KvlistValue(list)) => {
            visit_timestamp_keys_mut(&mut list.values, timestamps, visit)?;
        }
        _ => {}
    }
    Ok(())
}

fn read_timestamp(
    key: &str,
    value: &AnyValue,
    format: TimestampFormat,
) -> Result<i64, CaptureError> {
    let bad = || CaptureError::TimestampAttribute {
        key: key.to_owned(),
    };
    match format {
        TimestampFormat::Rfc3339 => {
            let Some(Value::StringValue(text)) = &value.value else {
                return Err(bad());
            };
            DateTime::parse_from_rfc3339(text)
                .ok()
                .and_then(|parsed| parsed.timestamp_nanos_opt())
                .ok_or_else(bad)
        }
        TimestampFormat::UnixNano => read_integer(value).ok_or_else(bad),
        TimestampFormat::UnixMilli => read_integer(value)
            .and_then(|millis| millis.checked_mul(NS_PER_MS))
            .ok_or_else(bad),
    }
}

fn read_integer(value: &AnyValue) -> Option<i64> {
    match &value.value {
        Some(Value::IntValue(integer)) => Some(*integer),
        Some(Value::StringValue(text)) => text.parse().ok(),
        _ => None,
    }
}

fn write_timestamp(
    key: &str,
    value: &mut AnyValue,
    format: TimestampFormat,
    shifted: i64,
) -> Result<(), CaptureError> {
    let bad = || CaptureError::TimestampAttribute {
        key: key.to_owned(),
    };
    match format {
        TimestampFormat::Rfc3339 => {
            let Some(Value::StringValue(text)) = &value.value else {
                return Err(bad());
            };
            let rewritten = rewrite_rfc3339(text, shifted).map_err(|_| bad())?;
            value.value = Some(Value::StringValue(rewritten));
        }
        TimestampFormat::UnixNano => write_integer(value, shifted),
        TimestampFormat::UnixMilli => {
            write_integer(value, shifted / NS_PER_MS);
        }
    }
    Ok(())
}

fn write_integer(value: &mut AnyValue, integer: i64) {
    match &value.value {
        Some(Value::StringValue(_)) => {
            value.value = Some(Value::StringValue(integer.to_string()));
        }
        _ => value.value = Some(Value::IntValue(integer)),
    }
}

fn rewrite_rfc3339(original: &str, shifted_ns: i64) -> Result<String, ()> {
    let parsed = DateTime::parse_from_rfc3339(original).map_err(|_| ())?;
    let shifted = DateTime::from_timestamp_nanos(shifted_ns).with_timezone(parsed.offset());
    let digits = fractional_digits(original);
    let mut text = shifted.format("%Y-%m-%dT%H:%M:%S").to_string();
    if digits > 0 {
        let nanos = shifted.timestamp_subsec_nanos();
        let frac = format!("{nanos:09}");
        let digits = digits.min(9);
        text.push('.');
        text.push_str(&frac[..digits]);
    }
    if original.ends_with('Z') || original.ends_with('z') {
        text.push('Z');
    } else {
        text.push_str(&shifted.format("%:z").to_string());
    }
    Ok(text)
}

fn fractional_digits(text: &str) -> usize {
    let Some(dot) = text.rfind('.') else {
        return 0;
    };
    text[dot + 1..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .count()
}

/// Drops an unset status and an all-zero parent.
///
/// Empty strings, zero counters, and empty `events` or `links` are already the
/// proto3 zero value. The generated serde still writes those fields.
fn prune(groups: &mut [ResourceGroup]) {
    for group in groups.iter_mut() {
        for scope in &mut group.scopes {
            for span in &mut scope.spans {
                if !present(&span.parent_span_id) {
                    span.parent_span_id.clear();
                }
                if span
                    .status
                    .as_ref()
                    .is_some_and(|status| status.code == 0 && status.message.is_empty())
                {
                    span.status = None;
                }
            }
        }
    }
}

fn to_request(groups: Vec<ResourceGroup>) -> ExportTraceServiceRequest {
    ExportTraceServiceRequest {
        resource_spans: groups
            .into_iter()
            .map(|group| ResourceSpans {
                resource: Some(group.resource),
                scope_spans: group
                    .scopes
                    .into_iter()
                    .map(|scope| ScopeSpans {
                        scope: Some(scope.scope),
                        spans: scope.spans,
                        schema_url: scope.schema_url,
                    })
                    .collect(),
                schema_url: group.schema_url,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use opentelemetry_proto::tonic::common::v1::{AnyValue, InstrumentationScope, KeyValue};
    use opentelemetry_proto::tonic::trace::v1::span::Event;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status, span};
    use serde_json::Value as Json;
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::scenario::TimestampFormat;

    const INGEST: i64 = 1_767_225_600_000_000_000;
    const MIN_START: u64 = 1_000_000_000_000_000_000;

    fn case() -> CasePath {
        CasePath {
            group: "genai".to_owned(),
            case: "chat-tool".to_owned(),
        }
    }

    fn string_value(text: &str) -> AnyValue {
        AnyValue {
            value: Some(Value::StringValue(text.to_owned())),
        }
    }

    fn int_value(integer: i64) -> AnyValue {
        AnyValue {
            value: Some(Value::IntValue(integer)),
        }
    }

    fn bool_value(flag: bool) -> AnyValue {
        AnyValue {
            value: Some(Value::BoolValue(flag)),
        }
    }

    fn kv(key: &str, value: AnyValue) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(value),
            ..KeyValue::default()
        }
    }

    fn span(name: &str, start: u64, end: u64) -> Span {
        Span {
            name: name.to_owned(),
            start_time_unix_nano: start,
            end_time_unix_nano: end,
            ..Span::default()
        }
    }

    fn packaged(
        resource_name: &str,
        scope_name: &str,
        spans: Vec<Span>,
    ) -> ExportTraceServiceRequest {
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: Some(Resource {
                    attributes: vec![kv("service.name", string_value(resource_name))],
                    ..Resource::default()
                }),
                scope_spans: vec![ScopeSpans {
                    scope: Some(InstrumentationScope {
                        name: scope_name.to_owned(),
                        ..InstrumentationScope::default()
                    }),
                    spans,
                    ..ScopeSpans::default()
                }],
                ..ResourceSpans::default()
            }],
        }
    }

    fn expected_id(kind: &str, k: u64, len: usize) -> Vec<u8> {
        let material = format!("llmobs-capture\0genai/chat-tool\0{kind}\0{k}");
        Sha256::digest(material.as_bytes())[..len].to_vec()
    }

    #[test]
    fn two_captures_of_the_same_run_serialize_identically() {
        let mut early = span("root", MIN_START, MIN_START + 2_000_000);
        early.trace_id = vec![0x11; 16];
        early.span_id = vec![0x22; 8];
        early.attributes = vec![kv("n", string_value("1"))];
        let mut late = span("child", MIN_START + 5_000_000, MIN_START + 8_000_000);
        late.trace_id = vec![0x11; 16];
        late.span_id = vec![0x33; 8];
        late.parent_span_id = vec![0x22; 8];

        let batched = deterministic(
            vec![
                packaged("api", "genai", vec![late.clone()]),
                packaged("api", "genai", vec![early.clone()]),
            ],
            &case(),
            INGEST,
            &[],
        )
        .unwrap();

        let hours = 5 * 3_600 * 1_000_000_000;
        early.trace_id = vec![0xab; 16];
        early.span_id = vec![0xcd; 8];
        early.start_time_unix_nano += hours;
        early.end_time_unix_nano += hours;
        late.trace_id = vec![0xab; 16];
        late.span_id = vec![0xef; 8];
        late.parent_span_id = vec![0xcd; 8];
        late.start_time_unix_nano += hours;
        late.end_time_unix_nano += hours;
        let together = deterministic(
            vec![packaged("api", "genai", vec![early, late])],
            &case(),
            INGEST,
            &[],
        )
        .unwrap();

        let left = serde_json::to_vec(&batched).unwrap();
        let right = serde_json::to_vec(&together).unwrap();
        assert_eq!(left, right);
        let json: Json = serde_json::from_slice(&left).unwrap();
        let spans = &json["resourceSpans"][0]["scopeSpans"][0]["spans"];
        assert_eq!(spans[0]["name"], "root");
        assert_eq!(spans[1]["name"], "child");
        assert_eq!(json["resourceSpans"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn equal_span_keys_keep_arrival_order_and_resources_follow_the_earliest_span() {
        let mut first = span("work", MIN_START, MIN_START + 1);
        first.attributes = vec![kv("n", string_value("first"))];
        let mut second = span("work", MIN_START, MIN_START + 1);
        second.attributes = vec![kv("n", string_value("second"))];
        let later = span("later", MIN_START + 50, MIN_START + 80);
        let result = deterministic(
            vec![
                packaged("later-resource", "scope", vec![later]),
                packaged("early-resource", "scope", vec![second]),
                packaged("early-resource", "scope", vec![first]),
            ],
            &case(),
            INGEST,
            &[],
        )
        .unwrap();
        assert_eq!(
            result.resource_spans[0]
                .resource
                .as_ref()
                .unwrap()
                .attributes[0]
                .value
                .as_ref()
                .unwrap()
                .value,
            Some(Value::StringValue("early-resource".to_owned()))
        );
        let spans = &result.resource_spans[0].scope_spans[0].spans;
        assert_eq!(
            spans[0].attributes[0].value.as_ref().unwrap().value,
            Some(Value::StringValue("second".to_owned()))
        );
        assert_eq!(
            spans[1].attributes[0].value.as_ref().unwrap().value,
            Some(Value::StringValue("first".to_owned()))
        );
        assert_eq!(
            result.resource_spans[1]
                .resource
                .as_ref()
                .unwrap()
                .attributes[0]
                .value
                .as_ref()
                .unwrap()
                .value,
            Some(Value::StringValue("later-resource".to_owned()))
        );
    }

    #[test]
    fn parent_link_and_attribute_ids_are_reminted() {
        let mut child = span("child", MIN_START, MIN_START + 1_000);
        child.trace_id = vec![0x11; 16];
        child.span_id = vec![0x02; 8];
        child.parent_span_id = vec![0x01; 8];
        let trace_hex = hex::encode(vec![0x11_u8; 16]).to_ascii_uppercase();
        let parent_hex = hex::encode(vec![0x01_u8; 8]).to_ascii_uppercase();
        child.attributes = vec![kv(
            "traceparent",
            string_value(&format!("00-{trace_hex}-{parent_hex}-01")),
        )];

        let mut parent = span("parent", MIN_START + 5_000, MIN_START + 9_000);
        parent.trace_id = vec![0x11; 16];
        parent.span_id = vec![0x01; 8];

        let mut linker = span("linker", MIN_START + 20_000, MIN_START + 30_000);
        linker.trace_id = vec![0x22; 16];
        linker.span_id = vec![0x03; 8];
        linker.links = vec![span::Link {
            trace_id: vec![0x33; 16],
            span_id: vec![0x04; 8],
            ..span::Link::default()
        }];
        linker.attributes = vec![kv(
            "bag",
            AnyValue {
                value: Some(Value::KvlistValue(
                    opentelemetry_proto::tonic::common::v1::KeyValueList {
                        values: vec![kv("seen", string_value(&hex::encode(vec![0x04_u8; 8])))],
                    },
                )),
            },
        )];

        let result = deterministic(
            vec![packaged("api", "genai", vec![child, parent, linker])],
            &case(),
            INGEST,
            &[],
        )
        .unwrap();
        let spans = &result.resource_spans[0].scope_spans[0].spans;
        let child = &spans[0];
        let parent = &spans[1];
        let linker = &spans[2];

        // The child sorts first, so its span id is k = 0 and its parent is k = 1.
        assert_eq!(child.span_id, expected_id("span", 0, 8));
        assert_eq!(child.trace_id, expected_id("trace", 0, 16));
        assert_eq!(child.parent_span_id, expected_id("span", 1, 8));
        assert_eq!(parent.span_id, expected_id("span", 1, 8));
        assert_eq!(parent.trace_id, expected_id("trace", 0, 16));
        assert_eq!(linker.trace_id, expected_id("trace", 1, 16));
        assert_eq!(linker.span_id, expected_id("span", 2, 8));
        assert_eq!(linker.links[0].trace_id, expected_id("trace", 2, 16));
        assert_eq!(linker.links[0].span_id, expected_id("span", 3, 8));

        let traceparent = match &child.attributes[0].value.as_ref().unwrap().value {
            Some(Value::StringValue(text)) => text.clone(),
            _ => panic!("traceparent"),
        };
        let expected_parent = format!(
            "00-{}-{}-01",
            hex::encode(expected_id("trace", 0, 16)),
            hex::encode(expected_id("span", 1, 8))
        );
        assert_eq!(traceparent, expected_parent);

        let nested = match &linker.attributes[0].value.as_ref().unwrap().value {
            Some(Value::KvlistValue(list)) => match &list.values[0].value.as_ref().unwrap().value {
                Some(Value::StringValue(text)) => text.clone(),
                _ => panic!("nested"),
            },
            _ => panic!("bag"),
        };
        assert_eq!(nested, hex::encode(expected_id("span", 3, 8)));
    }

    #[test]
    fn rounding_is_half_away_from_zero_at_half_a_millisecond() {
        let mut sample = span("work", MIN_START, MIN_START - 500_000);
        sample.events = vec![
            Event {
                time_unix_nano: MIN_START + 499_999,
                name: "under".to_owned(),
                ..Event::default()
            },
            Event {
                time_unix_nano: MIN_START + 500_000,
                name: "half".to_owned(),
                ..Event::default()
            },
            Event {
                time_unix_nano: MIN_START + 1_500_000,
                name: "one-and-a-half".to_owned(),
                ..Event::default()
            },
        ];
        let result = deterministic(
            vec![packaged("api", "genai", vec![sample])],
            &case(),
            INGEST,
            &[],
        )
        .unwrap();
        let base = INGEST - TEN_SECONDS_NS;
        let got = &result.resource_spans[0].scope_spans[0].spans[0];
        assert_eq!(got.start_time_unix_nano, u64::try_from(base).unwrap());
        assert_eq!(
            got.end_time_unix_nano,
            u64::try_from(base - 1_000_000).unwrap()
        );
        assert_eq!(got.events[0].time_unix_nano, u64::try_from(base).unwrap());
        assert_eq!(
            got.events[1].time_unix_nano,
            u64::try_from(base + 1_000_000).unwrap()
        );
        assert_eq!(
            got.events[2].time_unix_nano,
            u64::try_from(base + 2_000_000).unwrap()
        );
    }

    #[test]
    fn a_run_of_ten_seconds_is_outside_the_window() {
        let sample = span("work", MIN_START, MIN_START + 10_000_000_000);
        let error = deterministic(
            vec![packaged("api", "genai", vec![sample])],
            &case(),
            INGEST,
            &[],
        )
        .unwrap_err();
        assert!(
            matches!(error, CaptureError::WindowExceeded { span_ms: 10_000 }),
            "{error}"
        );
    }

    #[test]
    fn an_event_before_the_first_span_is_rejected() {
        let mut sample = span("work", MIN_START, MIN_START + 1_000);
        sample.events = vec![Event {
            time_unix_nano: MIN_START - 1,
            name: "early".to_owned(),
            ..Event::default()
        }];
        let error = deterministic(
            vec![packaged("api", "genai", vec![sample])],
            &case(),
            INGEST,
            &[],
        )
        .unwrap_err();
        assert!(matches!(error, CaptureError::InstantBeforeStart), "{error}");
    }

    #[test]
    fn timestamp_attributes_keep_their_encoding() {
        let mut sample = span("work", MIN_START, MIN_START + 1_000);
        let start = DateTime::from_timestamp_nanos(i64::try_from(MIN_START).unwrap());
        let rfc_z = format!(
            "{}.{:03}Z",
            start.format("%Y-%m-%dT%H:%M:%S"),
            start.timestamp_subsec_nanos() / 1_000_000
        );
        let rfc_offset = format!(
            "{}.{:09}+00:00",
            start.format("%Y-%m-%dT%H:%M:%S"),
            start.timestamp_subsec_nanos()
        );
        sample.attributes = vec![
            kv("when.rfc.z", string_value(&rfc_z)),
            kv("when.rfc.offset", string_value(&rfc_offset)),
            kv(
                "when.nano",
                int_value(i64::try_from(MIN_START).unwrap() + 500_000),
            ),
            kv(
                "when.nano.text",
                string_value(&(i64::try_from(MIN_START).unwrap() + 1_500_000).to_string()),
            ),
            kv(
                "when.milli",
                int_value(i64::try_from(MIN_START / 1_000_000).unwrap() + 4),
            ),
        ];
        let specs = [
            TimestampAttribute {
                key: "when.rfc.z".to_owned(),
                format: TimestampFormat::Rfc3339,
            },
            TimestampAttribute {
                key: "when.rfc.offset".to_owned(),
                format: TimestampFormat::Rfc3339,
            },
            TimestampAttribute {
                key: "when.nano".to_owned(),
                format: TimestampFormat::UnixNano,
            },
            TimestampAttribute {
                key: "when.nano.text".to_owned(),
                format: TimestampFormat::UnixNano,
            },
            TimestampAttribute {
                key: "when.milli".to_owned(),
                format: TimestampFormat::UnixMilli,
            },
        ];
        let result = deterministic(
            vec![packaged("api", "genai", vec![sample])],
            &case(),
            INGEST,
            &specs,
        )
        .unwrap();
        let base = INGEST - TEN_SECONDS_NS;
        let shifted = DateTime::from_timestamp_nanos(base);
        let values: Vec<Value> = result.resource_spans[0].scope_spans[0].spans[0]
            .attributes
            .iter()
            .map(|attribute| attribute.value.as_ref().unwrap().value.clone().unwrap())
            .collect();
        assert_eq!(
            values[0],
            Value::StringValue(format!(
                "{}.{:03}Z",
                shifted.format("%Y-%m-%dT%H:%M:%S"),
                shifted.timestamp_subsec_nanos() / 1_000_000
            ))
        );
        assert_eq!(
            values[1],
            Value::StringValue(format!(
                "{}.{:09}+00:00",
                shifted.format("%Y-%m-%dT%H:%M:%S"),
                shifted.timestamp_subsec_nanos()
            ))
        );
        assert_eq!(values[2], Value::IntValue(base + 1_000_000));
        assert_eq!(
            values[3],
            Value::StringValue((base + 2_000_000).to_string())
        );
        assert_eq!(values[4], Value::IntValue(base / NS_PER_MS + 4));
    }

    #[test]
    fn an_unparseable_timestamp_names_the_key_only() {
        let mut sample = span("work", MIN_START, MIN_START + 1_000);
        sample.attributes = vec![kv("when", string_value("SECRET-TIME"))];
        let specs = [TimestampAttribute {
            key: "when".to_owned(),
            format: TimestampFormat::Rfc3339,
        }];
        let error = deterministic(
            vec![packaged("api", "genai", vec![sample])],
            &case(),
            INGEST,
            &specs,
        )
        .unwrap_err();
        let text = error.to_string();
        assert!(
            matches!(error, CaptureError::TimestampAttribute { ref key } if key == "when"),
            "{error}"
        );
        assert!(text.contains("when"), "{text}");
        assert!(!text.contains("SECRET-TIME"), "{text}");
        assert!(!format!("{error:?}").contains("SECRET-TIME"));
    }

    #[test]
    fn default_status_is_cleared_and_real_attribute_values_stay() {
        let mut noisy = span("noisy", MIN_START, MIN_START + 1_000);
        noisy.trace_state.clear();
        noisy.flags = 0;
        noisy.status = Some(Status {
            message: String::new(),
            code: 0,
        });
        noisy.attributes = vec![kv("ok", bool_value(false)), kv("count", int_value(0))];
        let mut kept = span("kept", MIN_START + 2_000, MIN_START + 3_000);
        kept.flags = 1;
        kept.trace_state = "vendor=capture".to_owned();
        kept.dropped_attributes_count = 3;
        kept.status = Some(Status {
            message: "kept".to_owned(),
            code: 2,
        });
        kept.events = vec![Event {
            time_unix_nano: MIN_START + 2_500,
            name: "mark".to_owned(),
            ..Event::default()
        }];
        let result = deterministic(
            vec![packaged("api", "genai", vec![noisy, kept])],
            &case(),
            INGEST,
            &[],
        )
        .unwrap();
        let spans = &result.resource_spans[0].scope_spans[0].spans;
        assert!(spans[0].status.is_none());
        assert!(spans[0].events.is_empty());
        assert!(spans[0].parent_span_id.is_empty());
        assert_eq!(spans[0].flags, 0);
        assert_eq!(
            spans[0].attributes[0].value.as_ref().unwrap().value,
            Some(Value::BoolValue(false))
        );
        assert_eq!(
            spans[0].attributes[1].value.as_ref().unwrap().value,
            Some(Value::IntValue(0))
        );
        assert_eq!(spans[1].flags, 1);
        assert_eq!(spans[1].trace_state, "vendor=capture");
        assert_eq!(spans[1].dropped_attributes_count, 3);
        assert_eq!(spans[1].status.as_ref().unwrap().code, 2);
        assert_eq!(spans[1].status.as_ref().unwrap().message, "kept");
        assert_eq!(spans[1].events.len(), 1);

        let json = serde_json::to_value(&result).unwrap();
        let attrs = &json["resourceSpans"][0]["scopeSpans"][0]["spans"][0]["attributes"];
        assert_eq!(attrs[0]["value"]["boolValue"], false);
        assert_eq!(attrs[1]["value"]["intValue"], "0");
    }
}
