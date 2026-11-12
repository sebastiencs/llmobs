//! Resolve a [`VariableMapping`] against one trace and, when present, one dataset item.
//!
//! Variables are resolved in name order. The first error stops the rest.
//! `NoTrace`, `NoItem`, and `NoTargetSpan` are context errors: an optional
//! marker does not turn them into null. An optional marker only turns an
//! absent value into null.

use std::collections::{BTreeMap, HashSet};

use llmobs_core::enums::{PayloadFormat, SpanKind};
use llmobs_core::ids::SpanId;
use llmobs_core::span::Span;
use serde_json::{Map, Value};

use super::{
    Expr, ItemField, Source, SpanField, SpanSelector, TraceField, VariableMapping, decode_payload,
    transcript,
};
use crate::error::MappingError;

/// One trace's spans, with heavy columns loaded, in any order.
#[derive(Debug, Clone, Copy)]
pub struct TraceData<'a> {
    spans: &'a [Span],
    target_span: Option<SpanId>,
}

/// One dataset item.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemData {
    /// Item input. Always present for an item.
    pub input: Value,
    /// Reference output. Absent when the item has none.
    pub expected: Option<Value>,
    /// Item metadata. Values are already JSON, unlike span metadata.
    pub metadata: Map<String, Value>,
}

/// The variables passed to a check, a judge, or a webhook.
#[derive(Debug, Clone, PartialEq)]
pub struct Variables(BTreeMap<String, Value>);

impl<'a> TraceData<'a> {
    /// Borrows `spans` and records the span a bare `span.*` expression reads.
    #[must_use]
    pub fn new(spans: &'a [Span], target_span: Option<SpanId>) -> Self {
        Self { spans, target_span }
    }
}

impl Variables {
    /// Wraps variables supplied by the evaluator test endpoint.
    #[must_use]
    pub fn from_map(variables: BTreeMap<String, Value>) -> Self {
        Self(variables)
    }
}

/// Resolves every expression in `mapping`.
///
/// # Errors
///
/// Returns [`MappingError::NoTrace`] when a trace expression has no trace,
/// [`MappingError::NoItem`] when an item expression has no item, and
/// [`MappingError::NoTargetSpan`] when `span.*` has no target in the trace.
/// A required expression that resolves to nothing is
/// [`MappingError::MissingVariable`]. The error names the variable and the
/// expression, and it does not include the payload.
pub fn resolve(
    mapping: &VariableMapping,
    trace: Option<&TraceData<'_>>,
    item: Option<&ItemData>,
) -> Result<Variables, MappingError> {
    let mut variables = BTreeMap::new();
    for (name, expr) in &mapping.0 {
        let value = resolve_expr(expr, trace, item)?;
        variables.insert(name.clone(), materialize(name, expr, value)?);
    }
    Ok(Variables(variables))
}

fn materialize(name: &str, expr: &Expr, value: Option<Value>) -> Result<Value, MappingError> {
    match value {
        Some(value) => Ok(value),
        None if expr.optional => Ok(Value::Null),
        None => Err(MappingError::MissingVariable {
            name: name.to_owned(),
            expr: expr.text.clone(),
        }),
    }
}

fn resolve_expr(
    expr: &Expr,
    trace: Option<&TraceData<'_>>,
    item: Option<&ItemData>,
) -> Result<Option<Value>, MappingError> {
    let value = match &expr.source {
        Source::Trace(field) => resolve_trace(field, trace)?,
        Source::Span(selector, field) => resolve_span(selector, field, trace)?,
        Source::Expected => item.ok_or(MappingError::NoItem)?.expected.clone(),
        Source::Item(ItemField::Input) => Some(item.ok_or(MappingError::NoItem)?.input.clone()),
        Source::Item(ItemField::Metadata(key)) => {
            item.ok_or(MappingError::NoItem)?.metadata.get(key).cloned()
        }
    };
    Ok(match (&expr.path, value) {
        (_, None) => None,
        (None, Some(value)) => Some(value),
        (Some(path), Some(value)) => apply_path(path, &value),
    })
}

fn resolve_trace(
    field: &TraceField,
    trace: Option<&TraceData<'_>>,
) -> Result<Option<Value>, MappingError> {
    let trace = trace.ok_or(MappingError::NoTrace)?;
    if let TraceField::Transcript = field {
        return Ok(Some(Value::String(transcript::render(trace.spans))));
    }
    let Some(root) = root_span(trace.spans) else {
        return Ok(None);
    };
    Ok(match field {
        TraceField::Input => decode_payload(root.input.as_deref(), root.input_format),
        TraceField::Output => decode_payload(root.output.as_deref(), root.output_format),
        TraceField::Metadata(key) => stored_json(root.metadata.get(key)),
        // Returned above, before the root is chosen. A transcript does not need a root.
        TraceField::Transcript => None,
    })
}

fn resolve_span(
    selector: &SpanSelector,
    field: &SpanField,
    trace: Option<&TraceData<'_>>,
) -> Result<Option<Value>, MappingError> {
    let trace = trace.ok_or(MappingError::NoTrace)?;
    let span = match selector {
        SpanSelector::Target => {
            let id = trace.target_span.ok_or(MappingError::NoTargetSpan)?;
            earliest(trace.spans.iter().filter(|span| span.span_id == id))
                .ok_or(MappingError::NoTargetSpan)?
        }
        SpanSelector::Name(name) => {
            match earliest(trace.spans.iter().filter(|span| span.name == *name)) {
                Some(span) => span,
                None => return Ok(None),
            }
        }
        SpanSelector::Kind(kind) => {
            match earliest(trace.spans.iter().filter(|span| span.span_kind == *kind)) {
                Some(span) => span,
                None => return Ok(None),
            }
        }
    };
    Ok(match field {
        SpanField::Input => decode_payload(span.input.as_deref(), span.input_format),
        SpanField::Output => decode_payload(span.output.as_deref(), span.output_format),
        SpanField::Metadata(key) => stored_json(span.metadata.get(key)),
        SpanField::Attributes(key) => stored_json(span.attributes.get(key)),
    })
}

/// A root has no parent, or a parent that is not in the slice. The earliest
/// start time wins, then the lower span id. When every parent is in the slice,
/// the same order picks the first `llm` span.
fn root_span(spans: &[Span]) -> Option<&Span> {
    let ids = spans
        .iter()
        .map(|span| span.span_id)
        .collect::<HashSet<_>>();
    let candidates = spans.iter().filter(|span| match span.parent_span_id {
        None => true,
        Some(parent) => !ids.contains(&parent),
    });
    if let Some(root) = earliest(candidates) {
        return Some(root);
    }
    earliest(spans.iter().filter(|span| span.span_kind == SpanKind::Llm))
}

fn earliest<'a>(spans: impl Iterator<Item = &'a Span>) -> Option<&'a Span> {
    spans.min_by(|left, right| {
        left.start_time
            .cmp(&right.start_time)
            .then_with(|| left.span_id.cmp(&right.span_id))
    })
}

/// Span metadata and attributes are JSON text. A value that is not JSON is
/// kept as a string; storage writes JSON, so this is defensive.
fn stored_json(raw: Option<&String>) -> Option<Value> {
    raw.map(|raw| {
        decode_payload(Some(raw), Some(PayloadFormat::Json))
            .unwrap_or_else(|| Value::String(raw.clone()))
    })
}

fn apply_path(path: &serde_json_path::JsonPath, value: &Value) -> Option<Value> {
    let nodes = path.query(value).all();
    match nodes.as_slice() {
        [] => None,
        [one] => Some((*one).clone()),
        many => Some(Value::Array(many.iter().copied().cloned().collect())),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use llmobs_core::enums::{PayloadFormat, SpanKind, SpanStatus};
    use llmobs_core::ids::{ProjectId, SpanId, TraceId};
    use llmobs_core::span::Span;
    use serde_json::{Map, Value, json};

    use super::{ItemData, TraceData, Variables, resolve};
    use crate::error::{MappingError, assert_no_payload};
    use crate::mapping::VariableMapping;

    const PLANTED: &str = "SECRET-PAYLOAD";

    fn span_id(byte: u8) -> SpanId {
        SpanId::from_bytes([byte, 0, 0, 0, 0, 0, 0, 1]).unwrap()
    }

    fn bare(id: u8, parent: Option<u8>, name: &str, kind: SpanKind, start: i64) -> Span {
        Span {
            project_id: "018f6b4e-7c1a-7c1a-8c1a-000000000001"
                .parse::<ProjectId>()
                .unwrap(),
            trace_id: "4bf92f3577b34da6a3ce929d0e0e4736"
                .parse::<TraceId>()
                .unwrap(),
            span_id: span_id(id),
            parent_span_id: parent.map(span_id),
            name: name.to_owned(),
            span_kind: kind,
            start_time: start,
            end_time: start,
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

    fn text_io(mut span: Span, input: Option<&str>, output: Option<&str>) -> Span {
        set_payload(&mut span, true, input, PayloadFormat::Text);
        set_payload(&mut span, false, output, PayloadFormat::Text);
        span
    }

    fn set_payload(span: &mut Span, input: bool, raw: Option<&str>, format: PayloadFormat) {
        let Some(raw) = raw else {
            return;
        };
        if input {
            span.input = Some(raw.to_owned());
            span.input_format = Some(format);
        } else {
            span.output = Some(raw.to_owned());
            span.output_format = Some(format);
        }
    }

    fn mapping(value: &Value) -> VariableMapping {
        VariableMapping::from_json(value).unwrap()
    }

    fn value<'a>(variables: &'a Variables, name: &str) -> &'a Value {
        variables
            .0
            .get(name)
            .unwrap_or_else(|| panic!("missing variable {name}"))
    }

    #[test]
    fn chooses_the_root() {
        let only = text_io(bare(1, None, "only", SpanKind::Chain, 0), Some("one"), None);
        let trace = TraceData::new(std::slice::from_ref(&only), None);
        let vars = resolve(
            &mapping(&json!({"input": "trace.input"})),
            Some(&trace),
            None,
        )
        .unwrap();
        assert_eq!(value(&vars, "input"), &json!("one"));

        let later = text_io(
            bare(1, None, "later", SpanKind::Chain, 20),
            Some("later"),
            None,
        );
        let earlier = text_io(
            bare(2, None, "earlier", SpanKind::Agent, 10),
            Some("earlier"),
            None,
        );
        let spans = [later, earlier];
        let trace = TraceData::new(&spans, None);
        let vars = resolve(
            &mapping(&json!({"input": "trace.input"})),
            Some(&trace),
            None,
        )
        .unwrap();
        assert_eq!(value(&vars, "input"), &json!("earlier"));

        let high = text_io(
            bare(2, None, "high", SpanKind::Chain, 10),
            Some("high"),
            None,
        );
        let low = text_io(bare(1, None, "low", SpanKind::Chain, 10), Some("low"), None);
        let spans = [high, low];
        let trace = TraceData::new(&spans, None);
        let vars = resolve(
            &mapping(&json!({"input": "trace.input"})),
            Some(&trace),
            None,
        )
        .unwrap();
        assert_eq!(value(&vars, "input"), &json!("low"));

        let chain = text_io(
            bare(1, Some(2), "chain", SpanKind::Chain, 0),
            Some("chain"),
            None,
        );
        let llm_late = text_io(
            bare(2, Some(1), "late", SpanKind::Llm, 8),
            Some("late"),
            None,
        );
        let llm_early = text_io(
            bare(3, Some(1), "early", SpanKind::Llm, 4),
            Some("early"),
            None,
        );
        let spans = [chain.clone(), llm_late, llm_early];
        let trace = TraceData::new(&spans, None);
        let vars = resolve(
            &mapping(&json!({"input": "trace.input"})),
            Some(&trace),
            None,
        )
        .unwrap();
        assert_eq!(value(&vars, "input"), &json!("early"));

        let same_time_high = text_io(
            bare(3, Some(1), "high", SpanKind::Llm, 4),
            Some("high"),
            None,
        );
        let same_time_low = text_io(bare(2, Some(1), "low", SpanKind::Llm, 4), Some("low"), None);
        let spans = [chain, same_time_high, same_time_low];
        let trace = TraceData::new(&spans, None);
        let vars = resolve(
            &mapping(&json!({"input": "trace.input"})),
            Some(&trace),
            None,
        )
        .unwrap();
        assert_eq!(value(&vars, "input"), &json!("low"));

        let orphan = text_io(
            bare(1, Some(9), "orphan", SpanKind::Chain, 10),
            Some("orphan"),
            None,
        );
        let child = text_io(
            bare(2, Some(1), "child", SpanKind::Llm, 1),
            Some("child"),
            None,
        );
        let spans = [child, orphan];
        let trace = TraceData::new(&spans, None);
        let vars = resolve(
            &mapping(&json!({"input": "trace.input"})),
            Some(&trace),
            None,
        )
        .unwrap();
        assert_eq!(value(&vars, "input"), &json!("orphan"));
    }

    #[test]
    fn resolves_each_source() {
        let messages = r#"[{"role":"user","parts":[{"type":"text","content":"hi"}]}]"#;
        let mut root = bare(1, None, "root", SpanKind::Agent, 0);
        set_payload(&mut root, true, Some(messages), PayloadFormat::Messages);
        set_payload(&mut root, false, Some("done"), PayloadFormat::Text);
        root.metadata
            .insert("llmobs.eval.rule_id".to_owned(), "\"rule-7\"".to_owned());

        let mut target = bare(2, Some(1), "chat", SpanKind::Llm, 1);
        set_payload(&mut target, true, Some("prompt"), PayloadFormat::Text);
        set_payload(
            &mut target,
            false,
            Some(r#"{"ok":true}"#),
            PayloadFormat::Json,
        );
        target
            .metadata
            .insert("k".to_owned(), "\"from-span\"".to_owned());
        target
            .attributes
            .insert("llmobs.eval.rule_id".to_owned(), "{\"n\":1}".to_owned());

        let retrieve = text_io(
            bare(3, Some(1), "retrieve", SpanKind::Retriever, 4),
            None,
            Some("found"),
        );
        let spans = [root, target, retrieve];
        let trace = TraceData::new(&spans, Some(span_id(2)));
        let mut metadata = Map::new();
        metadata.insert("topic".to_owned(), json!("travel"));
        let item = ItemData {
            input: json!({"q": "weather"}),
            expected: Some(json!("sunny")),
            metadata,
        };
        let vars = resolve(
            &mapping(&json!({
                "trace_in": "trace.input",
                "trace_out": "trace.output",
                "trace_meta": "trace.metadata.llmobs.eval.rule_id",
                "transcript": "trace.transcript",
                "span_in": "span.input",
                "span_out": "span.output",
                "span_meta": "span.metadata.k",
                "span_attr": "span.attributes.llmobs.eval.rule_id",
                "by_name": "span[name=\"retrieve\"].output",
                "by_kind": "span[kind=\"retriever\"].output",
                "expected": "expected",
                "item_in": "item.input",
                "item_meta": "item.metadata.topic",
            })),
            Some(&trace),
            Some(&item),
        )
        .unwrap();

        assert_eq!(
            value(&vars, "trace_in"),
            &json!([{"role": "user", "parts": [{"type": "text", "content": "hi"}]}])
        );
        assert_eq!(value(&vars, "trace_out"), &json!("done"));
        assert_eq!(value(&vars, "trace_meta"), &json!("rule-7"));
        assert_eq!(
            value(&vars, "transcript"),
            &json!(
                "agent root: user: hi → done\n  llm chat: prompt → {\"ok\":true}\n  retriever retrieve: - → found"
            )
        );
        assert_eq!(value(&vars, "span_in"), &json!("prompt"));
        assert_eq!(value(&vars, "span_out"), &json!({"ok": true}));
        assert_eq!(value(&vars, "span_meta"), &json!("from-span"));
        assert_eq!(value(&vars, "span_attr"), &json!({"n": 1}));
        assert_eq!(value(&vars, "by_name"), &json!("found"));
        assert_eq!(value(&vars, "by_kind"), &json!("found"));
        assert_eq!(value(&vars, "expected"), &json!("sunny"));
        assert_eq!(value(&vars, "item_in"), &json!({"q": "weather"}));
        assert_eq!(value(&vars, "item_meta"), &json!("travel"));
    }

    #[test]
    fn selectors_pick_the_earliest_span() {
        let late = text_io(
            bare(2, None, "retrieve", SpanKind::Retriever, 20),
            None,
            Some("late"),
        );
        let early = text_io(
            bare(3, None, "retrieve", SpanKind::Retriever, 10),
            None,
            Some("early"),
        );
        let tie_high = text_io(bare(5, None, "tie", SpanKind::Tool, 10), None, Some("high"));
        let tie_low = text_io(bare(4, None, "tie", SpanKind::Tool, 10), None, Some("low"));
        let spans = [late, tie_high, early, tie_low];
        let trace = TraceData::new(&spans, None);
        let vars = resolve(
            &mapping(&json!({
                "named": "span[name=\"retrieve\"].output",
                "kind": "span[kind=\"tool\"].output",
            })),
            Some(&trace),
            None,
        )
        .unwrap();
        assert_eq!(value(&vars, "named"), &json!("early"));
        assert_eq!(value(&vars, "kind"), &json!("low"));
    }

    #[test]
    fn target_span_must_be_present() {
        let span = text_io(
            bare(1, None, "root", SpanKind::Llm, 0),
            Some(PLANTED),
            Some("out"),
        );
        let document = mapping(&json!({"output": "span.output"}));
        let missing = TraceData::new(std::slice::from_ref(&span), None);
        let err = resolve(&document, Some(&missing), None).unwrap_err();
        assert_eq!(err, MappingError::NoTargetSpan);
        assert_no_payload(&err.to_string(), PLANTED);

        let optional = mapping(&json!({"output": "span.output?"}));
        assert_eq!(
            resolve(&optional, Some(&missing), None).unwrap_err(),
            MappingError::NoTargetSpan
        );

        let unknown = TraceData::new(std::slice::from_ref(&span), Some(span_id(9)));
        assert_eq!(
            resolve(&document, Some(&unknown), None).unwrap_err(),
            MappingError::NoTargetSpan
        );

        let found = TraceData::new(std::slice::from_ref(&span), Some(span_id(1)));
        let vars = resolve(&document, Some(&found), None).unwrap();
        assert_eq!(value(&vars, "output"), &json!("out"));
    }

    #[test]
    fn decodes_payloads_metadata_and_attributes() {
        let mut span = bare(1, None, "root", SpanKind::Span, 0);
        set_payload(&mut span, true, Some("{not json"), PayloadFormat::Messages);
        set_payload(&mut span, false, Some("{\"a\":1}"), PayloadFormat::Text);
        span.metadata
            .insert("llmobs.eval.rule_id".to_owned(), "\"dotted\"".to_owned());
        span.attributes.insert("count".to_owned(), "12".to_owned());
        span.attributes
            .insert("raw".to_owned(), "{not json".to_owned());
        let trace = TraceData::new(std::slice::from_ref(&span), Some(span_id(1)));
        let vars = resolve(
            &mapping(&json!({
                "broken": "trace.input",
                "text": "trace.output",
                "meta": "trace.metadata.llmobs.eval.rule_id",
                "count": "span.attributes.count",
                "raw": "span.attributes.raw",
            })),
            Some(&trace),
            None,
        )
        .unwrap();
        assert_eq!(value(&vars, "broken"), &json!("{not json"));
        assert_eq!(value(&vars, "text"), &json!("{\"a\":1}"));
        assert_eq!(value(&vars, "meta"), &json!("dotted"));
        assert_eq!(value(&vars, "count"), &json!(12));
        assert_eq!(value(&vars, "raw"), &json!("{not json"));
    }

    #[test]
    fn json_path_returns_zero_one_or_several_matches() {
        let mut span = bare(1, None, "root", SpanKind::Span, 0);
        set_payload(
            &mut span,
            false,
            Some(r#"{"items":[null, 2],"secret":"SECRET-PAYLOAD"}"#),
            PayloadFormat::Json,
        );
        let trace = TraceData::new(std::slice::from_ref(&span), None);
        let vars = resolve(
            &mapping(&json!({
                "one": "trace.output$.items[1]",
                "several": "trace.output$.items[*]",
                "null_match": "trace.output$.items[0]",
                "optional_miss": "trace.output$.missing?",
            })),
            Some(&trace),
            None,
        )
        .unwrap();
        assert_eq!(value(&vars, "one"), &json!(2));
        assert_eq!(value(&vars, "several"), &json!([null, 2]));
        assert_eq!(value(&vars, "null_match"), &Value::Null);
        assert_eq!(value(&vars, "optional_miss"), &Value::Null);

        let err = resolve(
            &mapping(&json!({"answer": "trace.output$.missing"})),
            Some(&trace),
            None,
        )
        .unwrap_err();
        assert_eq!(
            err,
            MappingError::MissingVariable {
                name: "answer".to_owned(),
                expr: "trace.output$.missing".to_owned(),
            }
        );
        assert_no_payload(&err.to_string(), PLANTED);
        assert!(err.to_string().contains("answer"));
        assert!(err.to_string().contains("trace.output$.missing"));
    }

    #[test]
    fn item_sources_require_an_item_and_expected_may_be_absent() {
        let item = ItemData {
            input: json!("question"),
            expected: None,
            metadata: Map::new(),
        };
        let vars = resolve(
            &mapping(&json!({"input": "item.input", "gold": "expected?"})),
            None,
            Some(&item),
        )
        .unwrap();
        assert_eq!(value(&vars, "input"), &json!("question"));
        assert_eq!(value(&vars, "gold"), &Value::Null);

        let err = resolve(&mapping(&json!({"gold": "expected"})), None, Some(&item)).unwrap_err();
        assert_eq!(
            err,
            MappingError::MissingVariable {
                name: "gold".to_owned(),
                expr: "expected".to_owned(),
            }
        );

        let span = text_io(
            bare(1, None, "root", SpanKind::Span, 0),
            Some(PLANTED),
            None,
        );
        let trace = TraceData::new(std::slice::from_ref(&span), None);
        let err = resolve(&mapping(&json!({"gold": "expected"})), Some(&trace), None).unwrap_err();
        assert_eq!(err, MappingError::NoItem);
        assert_no_payload(&err.to_string(), PLANTED);

        let err = resolve(
            &mapping(&json!({"input": "item.input"})),
            Some(&trace),
            None,
        )
        .unwrap_err();
        assert_eq!(err, MappingError::NoItem);

        let err = resolve(
            &mapping(&json!({"input": "trace.input"})),
            None,
            Some(&item),
        )
        .unwrap_err();
        assert_eq!(err, MappingError::NoTrace);

        let err = resolve(
            &mapping(&json!({"named": "span[name=\"root\"].output"})),
            None,
            None,
        )
        .unwrap_err();
        assert_eq!(err, MappingError::NoTrace);
    }

    #[test]
    fn from_map_keeps_the_supplied_variables() {
        let mut variables = BTreeMap::new();
        variables.insert("output".to_owned(), json!("hi"));
        let wrapped = Variables::from_map(variables);
        assert_eq!(wrapped.0.get("output"), Some(&json!("hi")));

        let empty = resolve(&mapping(&json!({})), None, None).unwrap();
        assert!(empty.0.is_empty());
    }
}
