//! End-to-end evaluator runs against the in-process mock.
//!
//! This file is an integration test, so clippy's test allow does not apply.
#![allow(clippy::expect_used)]

mod support {
    pub(crate) mod mock;
}

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use llmobs_core::enums::{PayloadFormat, ScoreDataType, SpanKind, SpanStatus};
use llmobs_core::ids::{ProjectId, SpanId, TraceId};
use llmobs_core::names::{environment, metadata_key, score_metadata_key};
use llmobs_core::score::ScoreValue;
use llmobs_core::span::Span;
use llmobs_eval::{
    Adapter, Check, ErrorClass, EvalOrigin, EvalOutcome, EvalRun, EvalRunner, EvalTarget,
    Evaluator, EvaluatorConfig, JudgeConfig, JudgeConnection, JudgeOutput, OutputScore, RunInput,
    Secret, StructuredOutput, VariableMapping, Variables, WebhookConfig,
};
use llmobs_otel::{IngestContext, IngestLimits, PriceTable, builtin_price_rows};
use serde_json::json;
use support::mock::{Mock, MockReply};
use uuid::Uuid;

const PROJECT: &str = "01900000-0000-7000-8000-000000000001";
const EVALUATOR: &str = "01900000-0000-7000-8000-000000000010";
const RULE: &str = "01900000-0000-7000-8000-000000000020";
const EXPERIMENT: &str = "01900000-0000-7000-8000-000000000030";
const DATASET_ITEM: &str = "01900000-0000-7000-8000-000000000031";
const TARGET_TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const JUDGE_TRACE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const KEY: &str = "sk-test-SECRET-PAYLOAD";

fn prices() -> &'static PriceTable {
    static PRICES: OnceLock<PriceTable> = OnceLock::new();
    PRICES.get_or_init(|| PriceTable::new(builtin_price_rows()).expect("built-in prices"))
}

fn now_ns() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos(),
    )
    .unwrap_or(i64::MAX)
}

fn ingest() -> IngestContext {
    IngestContext {
        project_id: PROJECT.parse::<ProjectId>().expect("project"),
        ingest_time: now_ns(),
        retention_days: Some(7),
        limits: IngestLimits::default(),
    }
}

fn runner() -> EvalRunner {
    EvalRunner::new(llmobs_eval::EgressPolicy {
        allow_private_network: true,
    })
    .expect("runner")
}

fn evaluator(config: EvaluatorConfig, data_type: ScoreDataType) -> Evaluator {
    Evaluator {
        id: Uuid::parse_str(EVALUATOR).expect("evaluator"),
        name: "grader".to_owned(),
        config,
        output: OutputScore {
            name: "grade".to_owned(),
            data_type,
            categories: None,
        },
    }
}

fn target() -> EvalTarget {
    EvalTarget {
        trace_id: Some(TARGET_TRACE.parse().expect("trace")),
        span_id: Some(SpanId::from_bytes([1, 0, 0, 0, 0, 0, 0, 1]).expect("span")),
    }
}

fn judge_trace() -> TraceId {
    JUDGE_TRACE.parse().expect("judge trace")
}

fn variables_from(text: &str) -> Variables {
    let span = text_span(text);
    let mapping = VariableMapping::from_json(&json!({"output": "span.output"})).expect("mapping");
    let trace = llmobs_eval::TraceData::new(std::slice::from_ref(&span), Some(span.span_id));
    llmobs_eval::resolve(&mapping, Some(&trace), None).expect("resolve")
}

fn text_span(text: &str) -> Span {
    let mut span = Span {
        project_id: PROJECT.parse().expect("project"),
        trace_id: TARGET_TRACE.parse().expect("trace"),
        span_id: SpanId::from_bytes([1, 0, 0, 0, 0, 0, 0, 1]).expect("span"),
        parent_span_id: None,
        name: "answer".to_owned(),
        span_kind: SpanKind::Llm,
        start_time: 1_700_000_000_000_000_000,
        end_time: 1_700_000_000_000_000_000,
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
    };
    span.output = Some(text.to_owned());
    span.output_format = Some(PayloadFormat::Text);
    span
}

fn judge_config(prompt: &str) -> JudgeConfig {
    JudgeConfig {
        connection_id: Uuid::parse_str(EVALUATOR).expect("connection"),
        model: "gpt-4o".to_owned(),
        prompt: prompt.to_owned(),
        output: JudgeOutput::Numeric { min: 0.0, max: 1.0 },
        include_reason: true,
        temperature: None,
        max_output_tokens: 1024,
        timeout_ms: 60_000,
    }
}

fn connection(base: reqwest::Url) -> JudgeConnection {
    JudgeConnection {
        adapter: Adapter::OpenAi,
        base_url: Some(base),
        api_key: Secret::new(KEY.to_owned()),
        extra_headers: Vec::new(),
        structured_output: StructuredOutput::Native,
    }
}

async fn execute(
    evaluator: &Evaluator,
    variables: &Variables,
    connection: Option<&JudgeConnection>,
    secret: Option<&Secret>,
    origin: EvalOrigin,
) -> EvalRun {
    let ingest = ingest();
    runner()
        .run(RunInput {
            evaluator,
            variables,
            connection,
            webhook_secret: secret,
            target: target(),
            origin,
            judge_trace_id: judge_trace(),
            ingest: &ingest,
            prices: prices(),
        })
        .await
}

fn assert_trace(run: &EvalRun, children: usize) {
    assert_eq!(run.judge_trace.rejections.total(), 0);
    assert_eq!(run.judge_trace.spans.len(), 1 + children);
    for span in &run.judge_trace.spans {
        assert_eq!(span.environment.as_deref(), Some(environment::EVAL));
    }
    assert_eq!(run.judge_trace.spans[0].name, "grader");
    assert_eq!(run.judge_trace.spans[0].span_kind, SpanKind::Evaluator);
    let rendered = serde_json::to_string(&run.judge_trace.spans).expect("spans");
    assert!(!rendered.contains(KEY), "{rendered}");
    assert!(!rendered.contains("SECRET-PAYLOAD"), "{rendered}");
}

fn success(run: EvalRun) -> EvalOutcome {
    run.outcome.expect("score")
}

#[tokio::test]
async fn check_resolves_a_span_and_scores_it() {
    let variables = variables_from("{}");
    let evaluator = evaluator(
        EvaluatorConfig::Check(Check::JsonValid),
        ScoreDataType::Boolean,
    );
    let origin = EvalOrigin::Rule(Uuid::parse_str(RULE).expect("rule"));
    let run = execute(&evaluator, &variables, None, None, origin).await;
    assert_trace(&run, 0);
    let root = &run.judge_trace.spans[0];
    assert_eq!(
        root.metadata
            .get(metadata_key::EVAL_RULE_ID)
            .map(String::as_str),
        Some(RULE)
    );
    assert_eq!(
        root.metadata
            .get(metadata_key::EVAL_TARGET_TRACE_ID)
            .map(String::as_str),
        Some(TARGET_TRACE)
    );
    let outcome = success(run);
    assert_eq!(outcome.value, ScoreValue::Boolean(true));
    assert_eq!(outcome.reason, None);
    assert_eq!(outcome.cost_usd, None);
    assert!(!outcome.unpriced_model);
    assert_eq!(
        outcome
            .metadata
            .get(score_metadata_key::JUDGE_TRACE_ID)
            .map(String::as_str),
        Some(JUDGE_TRACE)
    );
    assert!(
        outcome
            .metadata
            .contains_key(score_metadata_key::JUDGE_LATENCY_MS)
    );
    assert!(
        !outcome
            .metadata
            .contains_key(score_metadata_key::JUDGE_MODEL)
    );
}

#[tokio::test]
async fn judge_resolves_a_span_and_prices_the_model() {
    let mock = Mock::start().await;
    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "model": "gpt-4o",
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": "{\"value\":0.8,\"explanation\":\"clear\"}"}
            }],
            "usage": {"prompt_tokens": 1000, "completion_tokens": 100}
        }),
        delay: std::time::Duration::ZERO,
    });
    let variables = variables_from("hello");
    let evaluator = evaluator(
        EvaluatorConfig::LlmJudge(judge_config("Grade {{ output }}")),
        ScoreDataType::Numeric,
    );
    let connection = connection(mock.url());
    let run = execute(
        &evaluator,
        &variables,
        Some(&connection),
        None,
        EvalOrigin::Rule(Uuid::parse_str(RULE).expect("rule")),
    )
    .await;
    assert_trace(&run, 1);
    let llm = &run.judge_trace.spans[1];
    assert_eq!(llm.span_kind, SpanKind::Llm);
    assert_eq!(llm.request_model.as_deref(), Some("gpt-4o"));
    assert_eq!(llm.response_model.as_deref(), Some("gpt-4o"));
    let cost = llm.cost_total;
    assert!(cost.is_some());
    let seen = mock.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/v1/chat/completions");
    let body = seen[0].body.to_string();
    assert!(body.contains("Grade hello"), "{body}");
    let outcome = success(run);
    assert_eq!(outcome.value, ScoreValue::Numeric(0.8));
    assert_eq!(outcome.reason.as_deref(), Some("clear"));
    assert_eq!(outcome.cost_usd, cost);
    assert!(!outcome.unpriced_model);
    assert_eq!(
        outcome
            .metadata
            .get(score_metadata_key::JUDGE_MODEL)
            .map(String::as_str),
        Some("gpt-4o")
    );
    assert_eq!(
        outcome
            .metadata
            .get(score_metadata_key::JUDGE_INPUT_TOKENS)
            .map(String::as_str),
        Some("1000")
    );
    assert!(outcome.cost_usd.is_some());
}

#[tokio::test]
async fn webhook_resolves_a_span_and_posts_it() {
    let mock = Mock::start().await;
    mock.enqueue(MockReply {
        status: 200,
        body: json!({"value": true}),
        delay: std::time::Duration::ZERO,
    });
    let mut url = mock.origin();
    url.set_path("/webhook");
    let variables = variables_from("hello");
    let evaluator = evaluator(
        EvaluatorConfig::Webhook(WebhookConfig {
            url: url.to_string(),
            timeout_ms: 10_000,
            headers: BTreeMap::new(),
            secret_enc: None,
        }),
        ScoreDataType::Boolean,
    );
    let secret = Secret::new("whsec_test".to_owned());
    let run = execute(
        &evaluator,
        &variables,
        None,
        Some(&secret),
        EvalOrigin::Rule(Uuid::parse_str(RULE).expect("rule")),
    )
    .await;
    assert_trace(&run, 1);
    let webhook = &run.judge_trace.spans[1];
    assert_eq!(webhook.name, "webhook");
    assert_eq!(webhook.span_kind, SpanKind::Span);
    assert_eq!(
        webhook
            .attributes
            .get("http.response.status_code")
            .map(String::as_str),
        Some("200")
    );
    let seen = mock.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/webhook");
    assert!(seen[0].body.to_string().contains("hello"));
    assert!(seen[0].header("x-llmobs-signature").is_some());
    let outcome = success(run);
    assert_eq!(outcome.value, ScoreValue::Boolean(true));
    assert_eq!(outcome.cost_usd, None);
    assert!(!outcome.unpriced_model);
}

#[tokio::test]
async fn a_judge_without_a_connection_is_permanent() {
    let evaluator = evaluator(
        EvaluatorConfig::LlmJudge(judge_config("Grade.")),
        ScoreDataType::Numeric,
    );
    let variables = Variables::from_map(BTreeMap::new());
    let run = execute(&evaluator, &variables, None, None, EvalOrigin::Test).await;
    assert_trace(&run, 0);
    let error = run.outcome.expect_err("missing connection");
    assert_eq!(error.class(), ErrorClass::Permanent);
    assert_eq!(error.to_string(), "invalid connection: required");
    assert_eq!(run.judge_trace.spans[0].status, SpanStatus::Error);
}

#[tokio::test]
async fn a_webhook_without_a_secret_is_permanent() {
    let evaluator = evaluator(
        EvaluatorConfig::Webhook(WebhookConfig {
            url: "https://hooks.example/hook".to_owned(),
            timeout_ms: 10_000,
            headers: BTreeMap::new(),
            secret_enc: None,
        }),
        ScoreDataType::Boolean,
    );
    let variables = Variables::from_map(BTreeMap::new());
    let run = execute(&evaluator, &variables, None, None, EvalOrigin::Test).await;
    assert_trace(&run, 0);
    let error = run.outcome.expect_err("missing secret");
    assert_eq!(error.class(), ErrorClass::Permanent);
    assert_eq!(error.to_string(), "invalid secret: required");
}

#[tokio::test]
async fn a_missing_variable_is_permanent_and_still_traced() {
    let evaluator = evaluator(
        EvaluatorConfig::Check(Check::JsonValid),
        ScoreDataType::Boolean,
    );
    let variables = Variables::from_map(BTreeMap::new());
    let run = execute(&evaluator, &variables, None, None, EvalOrigin::Test).await;
    assert_trace(&run, 0);
    let error = run.outcome.expect_err("missing variable");
    assert_eq!(error.class(), ErrorClass::Permanent);
    assert_eq!(error.to_string(), "missing variable output");
    assert_eq!(run.judge_trace.spans[0].status, SpanStatus::Error);
}

#[tokio::test]
async fn experiment_and_test_origins_write_the_matching_metadata() {
    let variables = variables_from("{}");
    let evaluator = evaluator(
        EvaluatorConfig::Check(Check::JsonValid),
        ScoreDataType::Boolean,
    );
    let experiment = execute(
        &evaluator,
        &variables,
        None,
        None,
        EvalOrigin::Experiment {
            experiment_id: Uuid::parse_str(EXPERIMENT).expect("experiment"),
            dataset_item_id: Uuid::parse_str(DATASET_ITEM).expect("item"),
        },
    )
    .await;
    assert_trace(&experiment, 0);
    let metadata = &experiment.judge_trace.spans[0].metadata;
    assert_eq!(
        metadata
            .get(metadata_key::EVAL_EXPERIMENT_ID)
            .map(String::as_str),
        Some(EXPERIMENT)
    );
    assert!(!metadata.values().any(|value| value == DATASET_ITEM));
    assert!(!metadata.contains_key(metadata_key::EVAL_RULE_ID));

    let test_run = execute(&evaluator, &variables, None, None, EvalOrigin::Test).await;
    assert_trace(&test_run, 0);
    let metadata = &test_run.judge_trace.spans[0].metadata;
    assert!(!metadata.contains_key(metadata_key::EVAL_RULE_ID));
    assert!(!metadata.contains_key(metadata_key::EVAL_EXPERIMENT_ID));
}

#[tokio::test]
async fn a_provider_429_is_retryable_and_marks_the_llm_span() {
    let mock = Mock::start().await;
    mock.enqueue(MockReply {
        status: 429,
        body: json!({"error": {"message": "SECRET-PAYLOAD", "type": "rate_limit", "code": "rate_limit_exceeded"}}),
        delay: std::time::Duration::ZERO,
    });
    let evaluator = evaluator(
        EvaluatorConfig::LlmJudge(judge_config("Grade the answer.")),
        ScoreDataType::Numeric,
    );
    let variables = Variables::from_map(BTreeMap::new());
    let connection = connection(mock.url());
    let run = execute(
        &evaluator,
        &variables,
        Some(&connection),
        None,
        EvalOrigin::Rule(Uuid::parse_str(RULE).expect("rule")),
    )
    .await;
    assert_trace(&run, 1);
    let llm = &run.judge_trace.spans[1];
    assert_eq!(llm.span_kind, SpanKind::Llm);
    assert_eq!(llm.status, SpanStatus::Error);
    assert_eq!(llm.status_message.as_deref(), Some("provider status 429"));
    let error = run.outcome.expect_err("rate limit");
    assert_eq!(error.class(), ErrorClass::Retryable);
    assert_eq!(error.to_string(), "provider status 429");
}
