//! Judge traces built as OTLP and passed through the normalizer (D161).
//!
//! Span ids are the first 8 bytes of SHA-256(trace id ‖ span index). The root
//! is index 0. A judge or webhook child is index 1.

use std::collections::BTreeMap;

use llmobs_core::enums::SpanKind;
use llmobs_core::ids::{SpanId, TraceId};
use llmobs_core::names::{attr, environment, metadata_key, score_metadata_key};
use llmobs_core::score::ScoreValue;
use llmobs_otel::{IngestContext, Normalized, PriceTable, RejectReason, normalize};
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use opentelemetry_proto::tonic::common::v1::any_value::Value as AnyValueKind;
use opentelemetry_proto::tonic::common::v1::{AnyValue, InstrumentationScope, KeyValue};
use opentelemetry_proto::tonic::resource::v1::Resource;
use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span, Status};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::Evaluator;
use crate::error::{EvalError, JudgeError, WebhookError};
use crate::judge::{Adapter, JudgeUsage};
use crate::mapping::resolve::Variables;

/// Where a run was started. `Test` writes neither a rule id nor an experiment id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalOrigin {
    /// An online rule.
    Rule(Uuid),
    /// One dataset item inside an experiment.
    Experiment {
        /// Experiment id.
        experiment_id: Uuid,
        /// Dataset item id. Kept for the runner; the trace does not copy it.
        dataset_item_id: Uuid,
    },
    /// The evaluator test panel.
    Test,
}

/// The trace and span the score is about. Either may be absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalTarget {
    /// Target trace. Omitted from metadata when absent.
    pub trace_id: Option<TraceId>,
    /// Target span. Omitted from metadata when absent.
    pub span_id: Option<SpanId>,
}

/// One judge trace before normalization.
pub(crate) struct TraceInput<'a> {
    pub(crate) trace_id: TraceId,
    pub(crate) evaluator: &'a Evaluator,
    pub(crate) target: &'a EvalTarget,
    pub(crate) origin: &'a EvalOrigin,
    pub(crate) started_ns: i64,
    pub(crate) ended_ns: i64,
    pub(crate) variables: &'a Variables,
    pub(crate) result: Result<(&'a ScoreValue, Option<&'a str>), &'a EvalError>,
    pub(crate) child: Option<ChildSpan<'a>>,
}

/// The single child of a judge or webhook run. A check has none.
pub(crate) enum ChildSpan<'a> {
    /// The model call.
    Llm {
        /// Child start, nanoseconds since the Unix epoch.
        started_ns: i64,
        /// Child end, nanoseconds since the Unix epoch.
        ended_ns: i64,
        /// Which provider built the request.
        adapter: Adapter,
        /// Model named on the request.
        request_model: &'a str,
        /// Model named on the response, when the provider sent one.
        response_model: Option<&'a str>,
        /// Rendered judge prompt.
        prompt: &'a str,
        /// Assistant text, when the call returned one.
        output_text: Option<&'a str>,
        /// Token counts, when the provider sent them.
        usage: Option<&'a JudgeUsage>,
        /// Provider or parse error. The message is the error's `Display`.
        error: Option<&'a JudgeError>,
    },
    /// The webhook POST.
    Webhook {
        /// Child start, nanoseconds since the Unix epoch.
        started_ns: i64,
        /// Child end, nanoseconds since the Unix epoch.
        ended_ns: i64,
        /// URL host, without the path or query.
        host: &'a str,
        /// HTTP status, when a response arrived.
        status: Option<u16>,
        /// Exact request body bytes.
        request_body: &'a [u8],
        /// Parsed response object, when the call returned one.
        response_body: Option<&'a Value>,
        /// Status, parse, or transport error. The message is the error's `Display`.
        error: Option<&'a WebhookError>,
    },
}

/// Builds the OTLP request, normalizes it, and returns the accepted spans.
///
/// A rejection is a bug in this builder. The evaluator id and the rejection
/// counts are logged, and the spans that were accepted are still returned.
pub(crate) fn build(
    input: &TraceInput<'_>,
    ingest: &IngestContext,
    prices: &PriceTable,
) -> Normalized {
    let request = export_request(input);
    let normalized = normalize(&request, ingest, prices);
    if normalized.rejections.total() > 0 {
        tracing::warn!(
            evaluator_id = %input.evaluator.id,
            invalid_trace_id = normalized.rejections.count(RejectReason::InvalidTraceId),
            invalid_span_id = normalized.rejections.count(RejectReason::InvalidSpanId),
            timestamp_overflow = normalized.rejections.count(RejectReason::TimestampOverflow),
            start_time_out_of_range = normalized
                .rejections
                .count(RejectReason::StartTimeOutOfRange),
            "judge trace dropped spans"
        );
    }
    normalized
}

/// First 8 bytes of SHA-256(trace id ‖ index). An all-zero prefix becomes `…01`.
pub(crate) fn span_id(trace_id: TraceId, index: u8) -> SpanId {
    let mut hasher = Sha256::new();
    hasher.update(trace_id.as_bytes());
    hasher.update([index]);
    let digest = hasher.finalize();
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    if bytes == [0; 8] {
        bytes[7] = 1;
    }
    // Eight bytes, and not all zeros: `from_bytes` only rejects the all-zero id.
    #[allow(clippy::expect_used)]
    {
        SpanId::from_bytes(bytes).expect("span id is eight non-zero bytes")
    }
}

/// Score metadata for one normalized judge trace.
///
/// Returns the metadata map, the llm span's `cost_total`, and whether that span
/// carried tokens without a price. A check or webhook sets `judge.trace_id` and
/// `judge.latency_ms` from the root span only.
pub(crate) fn judge_metadata(
    trace: &Normalized,
    judge_trace_id: TraceId,
) -> (BTreeMap<String, String>, Option<f64>, bool) {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        score_metadata_key::JUDGE_TRACE_ID.to_owned(),
        judge_trace_id.to_hex(),
    );
    let Some(llm) = trace
        .spans
        .iter()
        .find(|span| span.span_kind == SpanKind::Llm)
    else {
        if let Some(root) = trace
            .spans
            .iter()
            .find(|span| span.parent_span_id.is_none())
        {
            metadata.insert(
                score_metadata_key::JUDGE_LATENCY_MS.to_owned(),
                latency_ms(root.start_time, root.end_time),
            );
        }
        return (metadata, None, false);
    };
    if let Some(model) = llm
        .response_model
        .as_deref()
        .or(llm.request_model.as_deref())
    {
        metadata.insert(score_metadata_key::JUDGE_MODEL.to_owned(), model.to_owned());
    }
    if let Some(tokens) = llm.input_tokens {
        metadata.insert(
            score_metadata_key::JUDGE_INPUT_TOKENS.to_owned(),
            tokens.to_string(),
        );
    }
    if let Some(tokens) = llm.output_tokens {
        metadata.insert(
            score_metadata_key::JUDGE_OUTPUT_TOKENS.to_owned(),
            tokens.to_string(),
        );
    }
    if let Some(cost) = llm.cost_total {
        metadata.insert(
            score_metadata_key::JUDGE_COST_USD.to_owned(),
            format_cost(cost),
        );
    }
    metadata.insert(
        score_metadata_key::JUDGE_LATENCY_MS.to_owned(),
        latency_ms(llm.start_time, llm.end_time),
    );
    let has_tokens = llm.input_tokens.is_some() || llm.output_tokens.is_some();
    let unpriced = has_tokens && llm.cost_total.is_none();
    (metadata, llm.cost_total, unpriced)
}

fn export_request(input: &TraceInput<'_>) -> ExportTraceServiceRequest {
    let root_id = span_id(input.trace_id, 0);
    let mut spans = vec![root_span(input, &root_id)];
    if let Some(child) = &input.child {
        spans.push(child_span(input, &root_id, child));
    }
    ExportTraceServiceRequest {
        resource_spans: vec![ResourceSpans {
            resource: Some(Resource {
                attributes: vec![string_attr("service.name", environment::EVAL)],
                ..Resource::default()
            }),
            scope_spans: vec![ScopeSpans {
                scope: Some(InstrumentationScope {
                    name: env!("CARGO_PKG_NAME").to_owned(),
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                    ..InstrumentationScope::default()
                }),
                spans,
                ..ScopeSpans::default()
            }],
            ..ResourceSpans::default()
        }],
    }
}

fn root_span(input: &TraceInput<'_>, span_id: &SpanId) -> Span {
    let mut attributes = vec![
        string_attr(attr::SPAN_KIND, SpanKind::Evaluator.as_str()),
        string_attr(attr::ENVIRONMENT, environment::EVAL),
        string_attr(attr::INPUT, &variables_json(input.variables)),
        string_attr(attr::INPUT_FORMAT, "json"),
        metadata_attr(
            metadata_key::EVAL_EVALUATOR_ID,
            &input.evaluator.id.to_string(),
        ),
    ];
    if let Some(trace_id) = input.target.trace_id {
        attributes.push(metadata_attr(
            metadata_key::EVAL_TARGET_TRACE_ID,
            &trace_id.to_hex(),
        ));
    }
    if let Some(span_id) = input.target.span_id {
        attributes.push(metadata_attr(
            metadata_key::EVAL_TARGET_SPAN_ID,
            &span_id.to_hex(),
        ));
    }
    match input.origin {
        EvalOrigin::Rule(id) => {
            attributes.push(metadata_attr(metadata_key::EVAL_RULE_ID, &id.to_string()));
        }
        EvalOrigin::Experiment { experiment_id, .. } => {
            attributes.push(metadata_attr(
                metadata_key::EVAL_EXPERIMENT_ID,
                &experiment_id.to_string(),
            ));
        }
        EvalOrigin::Test => {}
    }
    let mut status = None;
    match input.result {
        Ok((value, reason)) => {
            attributes.push(string_attr(attr::OUTPUT, &score_json(value, reason)));
            attributes.push(string_attr(attr::OUTPUT_FORMAT, "json"));
        }
        Err(error) => status = Some(error_status(&error.to_string())),
    }
    otel_span(
        input.trace_id,
        span_id,
        None,
        &input.evaluator.name,
        (input.started_ns, input.ended_ns),
        attributes,
        status,
    )
}

fn child_span(input: &TraceInput<'_>, parent: &SpanId, child: &ChildSpan<'_>) -> Span {
    match child {
        ChildSpan::Llm {
            started_ns,
            ended_ns,
            adapter,
            request_model,
            response_model,
            prompt,
            output_text,
            usage,
            error,
        } => {
            let mut attributes = vec![
                string_attr(attr::SPAN_KIND, SpanKind::Llm.as_str()),
                string_attr(attr::ENVIRONMENT, environment::EVAL),
                string_attr("gen_ai.operation.name", "chat"),
                string_attr("gen_ai.provider.name", provider_name(*adapter)),
                string_attr("gen_ai.request.model", request_model),
                string_attr(attr::INPUT, &user_message(prompt)),
                string_attr(attr::INPUT_FORMAT, "messages"),
            ];
            if let Some(model) = response_model {
                attributes.push(string_attr("gen_ai.response.model", model));
            }
            if let Some(text) = output_text {
                attributes.push(string_attr(attr::OUTPUT, &assistant_message(text)));
                attributes.push(string_attr(attr::OUTPUT_FORMAT, "messages"));
            }
            if let Some(usage) = usage {
                attributes.extend(usage_attrs(usage));
            }
            let status = error.map(|error| error_status(&error.to_string()));
            otel_span(
                input.trace_id,
                &span_id(input.trace_id, 1),
                Some(parent),
                &format!("chat {request_model}"),
                (*started_ns, *ended_ns),
                attributes,
                status,
            )
        }
        ChildSpan::Webhook {
            started_ns,
            ended_ns,
            host,
            status,
            request_body,
            response_body,
            error,
        } => {
            // `server.address`, `http.request.method`, and `http.response.status_code`
            // are not read by a convention. They stay in `span.attributes`.
            let mut attributes = vec![
                string_attr(attr::SPAN_KIND, SpanKind::Span.as_str()),
                string_attr(attr::ENVIRONMENT, environment::EVAL),
                string_attr("server.address", host),
                string_attr("http.request.method", "POST"),
                string_attr(attr::INPUT, &bytes_text(request_body)),
                string_attr(attr::INPUT_FORMAT, "json"),
            ];
            if let Some(status) = status {
                attributes.push(int_attr("http.response.status_code", i64::from(*status)));
            }
            if let Some(body) = response_body {
                attributes.push(string_attr(
                    attr::OUTPUT,
                    &serde_json::to_string(body).unwrap_or_else(|_| "null".to_owned()),
                ));
                attributes.push(string_attr(attr::OUTPUT_FORMAT, "json"));
            }
            let otel_status = error.map(|error| error_status(&error.to_string()));
            otel_span(
                input.trace_id,
                &span_id(input.trace_id, 1),
                Some(parent),
                "webhook",
                (*started_ns, *ended_ns),
                attributes,
                otel_status,
            )
        }
    }
}

fn usage_attrs(usage: &JudgeUsage) -> Vec<KeyValue> {
    vec![
        int_attr("gen_ai.usage.input_tokens", token_count(usage.input)),
        int_attr("gen_ai.usage.output_tokens", token_count(usage.output)),
        int_attr(
            "gen_ai.usage.cache_read.input_tokens",
            token_count(usage.cache_read),
        ),
        int_attr(
            "gen_ai.usage.cache_write.input_tokens",
            token_count(usage.cache_write),
        ),
        int_attr(
            "gen_ai.usage.reasoning.output_tokens",
            token_count(usage.reasoning),
        ),
    ]
}

fn provider_name(adapter: Adapter) -> &'static str {
    match adapter {
        Adapter::OpenAi => "openai",
        Adapter::Anthropic => "anthropic",
        Adapter::Gemini => "gcp.gemini",
    }
}

fn otel_span(
    trace_id: TraceId,
    span_id: &SpanId,
    parent: Option<&SpanId>,
    name: &str,
    bounds: (i64, i64),
    attributes: Vec<KeyValue>,
    status: Option<Status>,
) -> Span {
    Span {
        trace_id: trace_id.as_bytes().to_vec(),
        span_id: span_id.as_bytes().to_vec(),
        parent_span_id: parent.map(|id| id.as_bytes().to_vec()).unwrap_or_default(),
        name: name.to_owned(),
        start_time_unix_nano: u64::try_from(bounds.0).unwrap_or(0),
        end_time_unix_nano: u64::try_from(bounds.1).unwrap_or(0),
        attributes,
        status,
        ..Span::default()
    }
}

fn error_status(message: &str) -> Status {
    Status {
        message: message.to_owned(),
        code: 2,
    }
}

fn metadata_attr(key: &str, value: &str) -> KeyValue {
    string_attr(&format!("{}{key}", attr::METADATA_PREFIX), value)
}

fn string_attr(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: Some(AnyValue {
            value: Some(AnyValueKind::StringValue(value.to_owned())),
        }),
        ..KeyValue::default()
    }
}

fn int_attr(key: &str, value: i64) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: Some(AnyValue {
            value: Some(AnyValueKind::IntValue(value)),
        }),
        ..KeyValue::default()
    }
}

fn variables_json(variables: &Variables) -> String {
    let mut object = Map::new();
    for (name, value) in variables.iter() {
        object.insert(name.to_owned(), value.clone());
    }
    serde_json::to_string(&object).unwrap_or_else(|_| "{}".to_owned())
}

fn score_json(value: &ScoreValue, reason: Option<&str>) -> String {
    let mut object = Map::new();
    object.insert("value".to_owned(), score_value_json(value));
    if let Some(reason) = reason {
        object.insert("reason".to_owned(), Value::String(reason.to_owned()));
    }
    serde_json::to_string(&object).unwrap_or_else(|_| "{}".to_owned())
}

fn score_value_json(value: &ScoreValue) -> Value {
    match value {
        ScoreValue::Numeric(number) => serde_json::Number::from_f64(*number)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ScoreValue::Boolean(value) => Value::Bool(*value),
        ScoreValue::Categorical { label, .. } => Value::String(label.clone()),
        ScoreValue::Text(text) => Value::String(text.clone()),
    }
}

fn user_message(prompt: &str) -> String {
    message_json("user", prompt)
}

fn assistant_message(text: &str) -> String {
    message_json("assistant", text)
}

fn message_json(role: &str, text: &str) -> String {
    serde_json::json!([{
        "role": role,
        "parts": [{"type": "text", "content": text}]
    }])
    .to_string()
}

fn bytes_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn token_count(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn latency_ms(start_ns: i64, end_ns: i64) -> String {
    let nanos = end_ns.saturating_sub(start_ns).max(0);
    (nanos / 1_000_000).to_string()
}

fn format_cost(cost: f64) -> String {
    serde_json::to_string(&cost).unwrap_or_else(|_| "null".to_owned())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::OnceLock;

    use llmobs_core::enums::{ScoreDataType, SpanKind, SpanStatus};
    use llmobs_core::ids::{ProjectId, SpanId, TraceId};
    use llmobs_core::names::{environment, metadata_key, score_metadata_key};
    use llmobs_core::score::ScoreValue;
    use llmobs_otel::{IngestContext, IngestLimits, PriceTable, builtin_price_rows};
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::{ChildSpan, EvalOrigin, EvalTarget, TraceInput, build, judge_metadata, span_id};
    use crate::config::{Check, Evaluator, EvaluatorConfig, OutputScore};
    use crate::error::{ErrorClass, EvalError, JudgeError, WebhookError};
    use crate::judge::{Adapter, JudgeUsage};
    use crate::mapping::resolve::Variables;

    const INGEST: i64 = 1_767_225_600_000_000_000;
    const START: i64 = INGEST - 1_500_000_000;
    const END: i64 = INGEST;
    const TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
    const TARGET_SPAN: &str = "00f067aa0ba902b7";
    const EVALUATOR_ID: &str = "01900000-0000-7000-8000-000000000010";
    const RULE_ID: &str = "01900000-0000-7000-8000-000000000020";
    const EXPERIMENT_ID: &str = "01900000-0000-7000-8000-000000000030";
    const DATASET_ITEM_ID: &str = "01900000-0000-7000-8000-000000000031";

    fn prices() -> &'static PriceTable {
        static PRICES: OnceLock<PriceTable> = OnceLock::new();
        PRICES.get_or_init(|| PriceTable::new(builtin_price_rows()).expect("built-in prices"))
    }

    fn ingest() -> IngestContext {
        IngestContext {
            project_id: "01900000-0000-7000-8000-000000000001"
                .parse::<ProjectId>()
                .expect("project id"),
            ingest_time: INGEST,
            retention_days: Some(7),
            limits: IngestLimits::default(),
        }
    }

    fn trace_id() -> TraceId {
        TRACE.parse().expect("trace id")
    }

    fn evaluator() -> Evaluator {
        Evaluator {
            id: Uuid::parse_str(EVALUATOR_ID).expect("evaluator id"),
            name: "grader".to_owned(),
            config: EvaluatorConfig::Check(Check::JsonValid),
            output: OutputScore {
                name: "grade".to_owned(),
                data_type: ScoreDataType::Boolean,
                categories: None,
            },
        }
    }

    fn variables() -> Variables {
        let mut map = BTreeMap::new();
        map.insert("output".to_owned(), Value::String("hello".to_owned()));
        Variables::from_map(map)
    }

    fn target() -> EvalTarget {
        EvalTarget {
            trace_id: Some(trace_id()),
            span_id: Some(TARGET_SPAN.parse::<SpanId>().expect("span id")),
        }
    }

    fn rule() -> EvalOrigin {
        EvalOrigin::Rule(Uuid::parse_str(RULE_ID).expect("rule id"))
    }

    fn usage() -> JudgeUsage {
        JudgeUsage {
            input: 1_000,
            output: 100,
            cache_read: 200,
            cache_write: 0,
            reasoning: 0,
        }
    }

    fn assert_common(normalized: &llmobs_otel::Normalized, indexes: &[u8]) {
        assert_eq!(normalized.rejections.total(), 0);
        assert_eq!(normalized.spans.len(), indexes.len());
        let trace = trace_id();
        for (span, index) in normalized.spans.iter().zip(indexes) {
            assert_eq!(span.environment.as_deref(), Some(environment::EVAL));
            assert_eq!(span.span_id, span_id(trace, *index));
            assert_eq!(span.service_name.as_deref(), Some(environment::EVAL));
            assert_eq!(span.scope_name.as_deref(), Some("llmobs-eval"));
            assert_eq!(span.scope_version.as_deref(), Some("0.0.0"));
        }
        let ids: Vec<_> = normalized.spans.iter().map(|span| span.span_id).collect();
        let mut unique = ids.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), ids.len());
        if indexes.len() == 2 {
            assert_eq!(
                normalized.spans[1].parent_span_id,
                Some(normalized.spans[0].span_id)
            );
            assert!(normalized.spans[0].parent_span_id.is_none());
        }
    }

    fn snapshot(name: &str, normalized: &llmobs_otel::Normalized) {
        let text = serde_json::to_string(&normalized.spans).expect("spans");
        assert!(
            !text.contains("SECRET-PAYLOAD"),
            "span json contains a planted payload"
        );
        insta::with_settings!({
            snapshot_path => "snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_json_snapshot!(name, &normalized.spans);
        });
    }

    #[test]
    fn span_ids_are_stable_and_distinct() {
        let trace = trace_id();
        assert_eq!(span_id(trace, 0), span_id(trace, 0));
        assert_ne!(span_id(trace, 0), span_id(trace, 1));
        assert_ne!(span_id(trace, 1), span_id(trace, 255));
    }

    #[test]
    fn judge_success_prices_a_known_model() {
        let evaluator = evaluator();
        let variables = variables();
        let target = target();
        let origin = rule();
        let score = ScoreValue::Numeric(0.8);
        let reason = "clear";
        let usage = usage();
        let response_model = "gpt-4o";
        let prompt = "Grade the answer.";
        let output_text = "0.8";
        let child = ChildSpan::Llm {
            started_ns: START,
            ended_ns: END,
            adapter: Adapter::OpenAi,
            request_model: "gpt-4o",
            response_model: Some(response_model),
            prompt,
            output_text: Some(output_text),
            usage: Some(&usage),
            error: None,
        };
        let input = TraceInput {
            trace_id: trace_id(),
            evaluator: &evaluator,
            target: &target,
            origin: &origin,
            started_ns: START,
            ended_ns: END,
            variables: &variables,
            result: Ok((&score, Some(reason))),
            child: Some(child),
        };
        let normalized = build(&input, &ingest(), prices());
        assert_common(&normalized, &[0, 1]);
        let root = &normalized.spans[0];
        assert_eq!(root.span_kind, SpanKind::Evaluator);
        assert_eq!(root.status, SpanStatus::Unset);
        assert_eq!(
            root.metadata
                .get(metadata_key::EVAL_EVALUATOR_ID)
                .map(String::as_str),
            Some(EVALUATOR_ID)
        );
        assert_eq!(
            root.metadata
                .get(metadata_key::EVAL_TARGET_TRACE_ID)
                .map(String::as_str),
            Some(TRACE)
        );
        assert_eq!(
            root.metadata
                .get(metadata_key::EVAL_TARGET_SPAN_ID)
                .map(String::as_str),
            Some(TARGET_SPAN)
        );
        assert_eq!(
            root.metadata
                .get(metadata_key::EVAL_RULE_ID)
                .map(String::as_str),
            Some(RULE_ID)
        );
        assert!(!root.metadata.contains_key(metadata_key::EVAL_EXPERIMENT_ID));
        let llm = &normalized.spans[1];
        assert_eq!(llm.span_kind, SpanKind::Llm);
        assert_eq!(llm.input_tokens, Some(1_000));
        assert_eq!(llm.output_tokens, Some(100));
        assert!(llm.cost_total.is_some());
        let (metadata, cost, unpriced) = judge_metadata(&normalized, trace_id());
        assert!(!unpriced);
        assert_eq!(cost, llm.cost_total);
        assert_eq!(
            metadata
                .get(score_metadata_key::JUDGE_TRACE_ID)
                .map(String::as_str),
            Some(TRACE)
        );
        assert_eq!(
            metadata
                .get(score_metadata_key::JUDGE_MODEL)
                .map(String::as_str),
            Some("gpt-4o")
        );
        assert_eq!(
            metadata
                .get(score_metadata_key::JUDGE_INPUT_TOKENS)
                .map(String::as_str),
            Some("1000")
        );
        assert_eq!(
            metadata
                .get(score_metadata_key::JUDGE_OUTPUT_TOKENS)
                .map(String::as_str),
            Some("100")
        );
        assert_eq!(
            metadata
                .get(score_metadata_key::JUDGE_LATENCY_MS)
                .map(String::as_str),
            Some("1500")
        );
        assert!(metadata.contains_key(score_metadata_key::JUDGE_COST_USD));
        snapshot("judge_trace_success", &normalized);
    }

    #[test]
    fn judge_provider_error_drops_the_message() {
        let evaluator = evaluator();
        let variables = variables();
        let target = target();
        let origin = rule();
        let judge_error = JudgeError::Provider {
            status: 429,
            class: ErrorClass::Retryable,
            provider_type: Some("rate_limit".to_owned()),
            provider_code: None,
            provider_message: Some("SECRET-PAYLOAD".to_owned()),
            hint: None,
        };
        let root_error = EvalError::from(judge_error.clone());
        let usage = usage();
        let child = ChildSpan::Llm {
            started_ns: START,
            ended_ns: END,
            adapter: Adapter::OpenAi,
            request_model: "gpt-4o",
            response_model: None,
            prompt: "Grade the answer.",
            output_text: None,
            usage: Some(&usage),
            error: Some(&judge_error),
        };
        let input = TraceInput {
            trace_id: trace_id(),
            evaluator: &evaluator,
            target: &target,
            origin: &origin,
            started_ns: START,
            ended_ns: END,
            variables: &variables,
            result: Err(&root_error),
            child: Some(child),
        };
        let normalized = build(&input, &ingest(), prices());
        assert_common(&normalized, &[0, 1]);
        for span in &normalized.spans {
            assert_eq!(span.status, SpanStatus::Error);
            assert_eq!(span.status_message.as_deref(), Some("provider status 429"));
        }
        snapshot("judge_trace_provider_error", &normalized);
    }

    #[test]
    fn unknown_model_is_unpriced() {
        let evaluator = evaluator();
        let variables = variables();
        let target = target();
        let origin = rule();
        let score = ScoreValue::Boolean(true);
        let usage = JudgeUsage {
            input: 10,
            output: 2,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        };
        let child = ChildSpan::Llm {
            started_ns: START,
            ended_ns: END,
            adapter: Adapter::OpenAi,
            request_model: "missing-model",
            response_model: Some("missing-model"),
            prompt: "Grade the answer.",
            output_text: Some("true"),
            usage: Some(&usage),
            error: None,
        };
        let input = TraceInput {
            trace_id: trace_id(),
            evaluator: &evaluator,
            target: &target,
            origin: &origin,
            started_ns: START,
            ended_ns: END,
            variables: &variables,
            result: Ok((&score, None)),
            child: Some(child),
        };
        let normalized = build(&input, &ingest(), prices());
        assert_eq!(normalized.rejections.total(), 0);
        let llm = &normalized.spans[1];
        assert_eq!(llm.input_tokens, Some(10));
        assert!(llm.cost_total.is_none());
        let (metadata, cost, unpriced) = judge_metadata(&normalized, trace_id());
        assert!(unpriced);
        assert_eq!(cost, None);
        assert!(!metadata.contains_key(score_metadata_key::JUDGE_COST_USD));
        assert_eq!(
            metadata
                .get(score_metadata_key::JUDGE_MODEL)
                .map(String::as_str),
            Some("missing-model")
        );
    }

    #[test]
    fn check_success_has_no_child() {
        let evaluator = evaluator();
        let variables = variables();
        let target = target();
        let origin = rule();
        let score = ScoreValue::Boolean(true);
        let input = TraceInput {
            trace_id: trace_id(),
            evaluator: &evaluator,
            target: &target,
            origin: &origin,
            started_ns: START,
            ended_ns: END,
            variables: &variables,
            result: Ok((&score, None)),
            child: None,
        };
        let normalized = build(&input, &ingest(), prices());
        assert_common(&normalized, &[0]);
        assert_eq!(normalized.spans[0].span_kind, SpanKind::Evaluator);
        let (metadata, cost, unpriced) = judge_metadata(&normalized, trace_id());
        assert!(!unpriced);
        assert_eq!(cost, None);
        assert_eq!(
            metadata
                .get(score_metadata_key::JUDGE_LATENCY_MS)
                .map(String::as_str),
            Some("1500")
        );
        assert!(!metadata.contains_key(score_metadata_key::JUDGE_MODEL));
        assert!(!metadata.contains_key(score_metadata_key::JUDGE_COST_USD));
        snapshot("judge_trace_check_success", &normalized);
    }

    #[test]
    fn webhook_success_keeps_unread_http_attributes() {
        let evaluator = evaluator();
        let variables = variables();
        let target = target();
        let origin = rule();
        let score = ScoreValue::Boolean(true);
        let body = br#"{"value":true}"#;
        let response = json!({"value": true});
        let child = ChildSpan::Webhook {
            started_ns: START,
            ended_ns: END,
            host: "hooks.example",
            status: Some(200),
            request_body: body,
            response_body: Some(&response),
            error: None,
        };
        let input = TraceInput {
            trace_id: trace_id(),
            evaluator: &evaluator,
            target: &target,
            origin: &origin,
            started_ns: START,
            ended_ns: END,
            variables: &variables,
            result: Ok((&score, None)),
            child: Some(child),
        };
        let normalized = build(&input, &ingest(), prices());
        assert_common(&normalized, &[0, 1]);
        let webhook = &normalized.spans[1];
        assert_eq!(webhook.span_kind, SpanKind::Span);
        assert_eq!(webhook.name, "webhook");
        assert_eq!(
            webhook.attributes.get("server.address").map(String::as_str),
            Some("hooks.example")
        );
        assert_eq!(
            webhook
                .attributes
                .get("http.request.method")
                .map(String::as_str),
            Some("POST")
        );
        assert_eq!(
            webhook
                .attributes
                .get("http.response.status_code")
                .map(String::as_str),
            Some("200")
        );
        let (metadata, cost, unpriced) = judge_metadata(&normalized, trace_id());
        assert!(!unpriced);
        assert_eq!(cost, None);
        assert!(!metadata.contains_key(score_metadata_key::JUDGE_MODEL));
        snapshot("judge_trace_webhook_success", &normalized);
    }

    #[test]
    fn webhook_failure_sets_error_status() {
        let evaluator = evaluator();
        let variables = variables();
        let target = target();
        let origin = rule();
        let webhook_error = WebhookError::Status {
            status: 500,
            class: ErrorClass::Retryable,
        };
        let root_error = EvalError::from(webhook_error.clone());
        let body = br#"{"value":true}"#;
        let child = ChildSpan::Webhook {
            started_ns: START,
            ended_ns: END,
            host: "hooks.example",
            status: Some(500),
            request_body: body,
            response_body: None,
            error: Some(&webhook_error),
        };
        let input = TraceInput {
            trace_id: trace_id(),
            evaluator: &evaluator,
            target: &target,
            origin: &origin,
            started_ns: START,
            ended_ns: END,
            variables: &variables,
            result: Err(&root_error),
            child: Some(child),
        };
        let normalized = build(&input, &ingest(), prices());
        assert_common(&normalized, &[0, 1]);
        for span in &normalized.spans {
            assert_eq!(span.status, SpanStatus::Error);
            assert_eq!(span.status_message.as_deref(), Some("webhook status 500"));
        }
        snapshot("judge_trace_webhook_failure", &normalized);
    }

    #[test]
    fn experiment_origin_omits_the_dataset_item() {
        let evaluator = evaluator();
        let variables = variables();
        let target = EvalTarget {
            trace_id: None,
            span_id: None,
        };
        let origin = EvalOrigin::Experiment {
            experiment_id: Uuid::parse_str(EXPERIMENT_ID).expect("experiment id"),
            dataset_item_id: Uuid::parse_str(DATASET_ITEM_ID).expect("dataset item id"),
        };
        let score = ScoreValue::Categorical {
            label: "yes".to_owned(),
            value: Some(1.0),
        };
        let input = TraceInput {
            trace_id: trace_id(),
            evaluator: &evaluator,
            target: &target,
            origin: &origin,
            started_ns: START,
            ended_ns: END,
            variables: &variables,
            result: Ok((&score, None)),
            child: None,
        };
        let normalized = build(&input, &ingest(), prices());
        assert_eq!(normalized.rejections.total(), 0);
        let metadata = &normalized.spans[0].metadata;
        assert_eq!(
            metadata
                .get(metadata_key::EVAL_EXPERIMENT_ID)
                .map(String::as_str),
            Some(EXPERIMENT_ID)
        );
        assert!(!metadata.values().any(|value| value == DATASET_ITEM_ID));
        assert!(!metadata.contains_key(metadata_key::EVAL_RULE_ID));
        assert!(!metadata.contains_key(metadata_key::EVAL_TARGET_TRACE_ID));
        assert!(!metadata.contains_key(metadata_key::EVAL_TARGET_SPAN_ID));
        let output = normalized.spans[0].output.as_deref().expect("output");
        assert_eq!(output, r#"{"value":"yes"}"#);
    }

    #[test]
    fn test_origin_writes_neither_rule_nor_experiment() {
        let evaluator = evaluator();
        let variables = variables();
        let target = target();
        let origin = EvalOrigin::Test;
        let score = ScoreValue::Boolean(false);
        let input = TraceInput {
            trace_id: trace_id(),
            evaluator: &evaluator,
            target: &target,
            origin: &origin,
            started_ns: START,
            ended_ns: END,
            variables: &variables,
            result: Ok((&score, None)),
            child: None,
        };
        let normalized = build(&input, &ingest(), prices());
        let metadata = &normalized.spans[0].metadata;
        assert!(!metadata.contains_key(metadata_key::EVAL_RULE_ID));
        assert!(!metadata.contains_key(metadata_key::EVAL_EXPERIMENT_ID));
        assert_eq!(normalized.rejections.total(), 0);
    }

    #[test]
    fn a_non_utf8_webhook_body_is_lossy() {
        let evaluator = evaluator();
        let variables = variables();
        let target = target();
        let origin = rule();
        let score = ScoreValue::Boolean(true);
        let child = ChildSpan::Webhook {
            started_ns: START,
            ended_ns: END,
            host: "hooks.example",
            status: Some(200),
            request_body: &[0xff],
            response_body: None,
            error: None,
        };
        let input = TraceInput {
            trace_id: trace_id(),
            evaluator: &evaluator,
            target: &target,
            origin: &origin,
            started_ns: START,
            ended_ns: END,
            variables: &variables,
            result: Ok((&score, None)),
            child: Some(child),
        };
        let normalized = build(&input, &ingest(), prices());
        assert_eq!(normalized.rejections.total(), 0);
        let input = normalized.spans[1].input.as_deref().expect("input");
        assert!(input.contains('\u{FFFD}'));
    }
}
