//! Opt-in calls to real providers. `just check` does not run them (D167).
//!
//! `just eval-live` runs this file with `--ignored`. A provider whose
//! `LLMOBS_LIVE_<PROVIDER>_API_KEY` is unset prints `skipped: <provider> (no key)`
//! and passes. `LLMOBS_LIVE_<PROVIDER>_MODEL` overrides the default model.
//!
//! This file is an integration test, so clippy's test allow does not apply.
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use llmobs_core::enums::{PayloadFormat, ScoreDataType, SpanKind, SpanStatus};
use llmobs_core::ids::{ProjectId, SpanId, TraceId};
use llmobs_core::span::Span;
use llmobs_eval::{
    Adapter, EgressPolicy, EvalError, EvalOrigin, EvalRunner, EvalTarget, Evaluator,
    EvaluatorConfig, ItemData, JudgeConfig, JudgeConnection, JudgeError, JudgeOutput,
    JudgeTemplate, OutputScore, RunInput, Secret, StructuredOutput, TraceData, VariableMapping,
    Variables, builtin_templates, resolve,
};
use llmobs_otel::{IngestContext, IngestLimits, PriceTable, builtin_price_rows};
use serde_json::{Map, Value};
use uuid::Uuid;

/// Current cost-sensitive OpenAI model. `gpt-5-nano` is deprecated; its model
/// page points new cost-sensitive work here. Chat Completions and structured
/// outputs are supported. https://developers.openai.com/api/docs/models/gpt-5.6-luna
const DEFAULT_OPENAI_MODEL: &str = "gpt-5.6-luna";

/// Current small Anthropic model, Claude API id.
/// https://platform.claude.com/docs/en/models/haiku-5-5/overview
const DEFAULT_ANTHROPIC_MODEL: &str = "claude-haiku-5-5";

/// Current small Gemini model for new projects. `gemini-3.1-flash-lite` is
/// replaced by this id, and 2.5 models are limited to existing users.
/// https://ai.google.dev/gemini-api/docs/deprecations
const DEFAULT_GEMINI_MODEL: &str = "gemini-3.5-flash-lite";

const PROJECT: &str = "01900000-0000-7000-8000-000000000001";
const EVALUATOR: &str = "01900000-0000-7000-8000-000000000010";
const CONNECTION: &str = "01900000-0000-7000-8000-000000000011";
const TARGET_TRACE: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const JUDGE_TRACE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

const MODES: [StructuredOutput; 3] = [
    StructuredOutput::Native,
    StructuredOutput::Tool,
    StructuredOutput::Prompt,
];

#[tokio::test]
#[ignore = "needs provider API keys; run with `just eval-live`"]
async fn openai() {
    provider(
        "openai",
        Adapter::OpenAi,
        "LLMOBS_LIVE_OPENAI_API_KEY",
        "LLMOBS_LIVE_OPENAI_MODEL",
        DEFAULT_OPENAI_MODEL,
    )
    .await;
}

#[tokio::test]
#[ignore = "needs provider API keys; run with `just eval-live`"]
async fn anthropic() {
    provider(
        "anthropic",
        Adapter::Anthropic,
        "LLMOBS_LIVE_ANTHROPIC_API_KEY",
        "LLMOBS_LIVE_ANTHROPIC_MODEL",
        DEFAULT_ANTHROPIC_MODEL,
    )
    .await;
}

#[tokio::test]
#[ignore = "needs provider API keys; run with `just eval-live`"]
async fn gemini() {
    provider(
        "gemini",
        Adapter::Gemini,
        "LLMOBS_LIVE_GEMINI_API_KEY",
        "LLMOBS_LIVE_GEMINI_MODEL",
        DEFAULT_GEMINI_MODEL,
    )
    .await;
}

async fn provider(
    name: &str,
    adapter: Adapter,
    key_env: &str,
    model_env: &str,
    default_model: &str,
) {
    let Some(key) = std::env::var(key_env)
        .ok()
        .filter(|value| !value.is_empty())
    else {
        skip(name);
        return;
    };
    let model = std::env::var(model_env)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_model.to_owned());
    let runner = EvalRunner::new(EgressPolicy {
        allow_private_network: false,
    })
    .expect("runner");
    let connection = JudgeConnection {
        adapter,
        base_url: None,
        api_key: Secret::new(key),
        extra_headers: Vec::new(),
        structured_output: StructuredOutput::Native,
    };
    let templates = builtin_templates();
    for mode in MODES {
        let mut connection = connection.clone();
        connection.structured_output = mode;
        for template_id in ["correctness", "toxicity"] {
            let template = templates
                .iter()
                .find(|template| template.id == template_id)
                .expect(template_id);
            let evaluator = evaluator(template, &model);
            evaluator.validate().expect(template_id);
            let variables = variables(template);
            let run = runner
                .run(RunInput {
                    evaluator: &evaluator,
                    variables: &variables,
                    connection: Some(&connection),
                    webhook_secret: None,
                    target: target(),
                    origin: EvalOrigin::Test,
                    judge_trace_id: judge_trace(),
                    ingest: &ingest(),
                    prices: prices(),
                })
                .await;
            if let Err(error) = &run.outcome {
                panic!(
                    "{}",
                    failure(name, mode.as_str(), template_id, &model, error)
                );
            }
            let llm = run
                .judge_trace
                .spans
                .iter()
                .find(|span| span.span_kind == SpanKind::Llm)
                .unwrap_or_else(|| {
                    panic!(
                        "{name} {mode} {template_id} model {model}: judge trace has no llm span",
                        mode = mode.as_str()
                    )
                });
            let usage = llm
                .input_tokens
                .unwrap_or(0)
                .saturating_add(llm.output_tokens.unwrap_or(0));
            assert!(
                usage > 0,
                "{name} {mode} {template_id} model {model}: usage is zero",
                mode = mode.as_str()
            );
        }
    }
}

fn failure(provider: &str, mode: &str, template: &str, model: &str, error: &EvalError) -> String {
    match error {
        EvalError::Judge(JudgeError::Provider {
            status,
            class,
            provider_type,
            provider_code,
            provider_message,
            hint,
        }) => format!(
            "{provider} {mode} {template} model {model}: provider status {status} class {class:?} type {provider_type:?} code {provider_code:?} hint {hint:?} message {provider_message:?}"
        ),
        other => format!("{provider} {mode} {template} model {model}: {other}"),
    }
}

fn variables(template: &JudgeTemplate) -> Variables {
    let (input, output, expected) = match template.id {
        "correctness" => (Some("Name the capital of France."), "Paris.", Some("Paris")),
        "toxicity" => (None, "Hello.", None),
        other => panic!("unexpected template {other}"),
    };
    let span = text_span(input, output);
    let mapping = mapping_of(template);
    let trace = TraceData::new(std::slice::from_ref(&span), Some(span.span_id));
    let item = expected.map(|text| ItemData {
        input: Value::String(input.unwrap_or("").to_owned()),
        expected: Some(Value::String(text.to_owned())),
        metadata: Map::new(),
    });
    resolve(&mapping, Some(&trace), item.as_ref()).expect("resolve")
}

fn mapping_of(template: &JudgeTemplate) -> VariableMapping {
    let mut object = Map::new();
    for (name, expr) in template.default_mapping {
        object.insert((*name).to_owned(), Value::String((*expr).to_owned()));
    }
    VariableMapping::from_json(&Value::Object(object)).expect("mapping")
}

fn evaluator(template: &JudgeTemplate, model: &str) -> Evaluator {
    let (data_type, categories) = match &template.output {
        JudgeOutput::Numeric { .. } => (ScoreDataType::Numeric, None),
        JudgeOutput::Boolean => (ScoreDataType::Boolean, None),
        JudgeOutput::Categorical { labels, .. } => {
            (ScoreDataType::Categorical, Some(labels.clone()))
        }
    };
    Evaluator {
        id: Uuid::parse_str(EVALUATOR).expect("evaluator"),
        name: template.name.to_owned(),
        config: EvaluatorConfig::LlmJudge(JudgeConfig {
            connection_id: Uuid::parse_str(CONNECTION).expect("connection"),
            model: model.to_owned(),
            prompt: template.prompt.to_owned(),
            output: template.output.clone(),
            include_reason: true,
            temperature: None,
            max_output_tokens: 1024,
            timeout_ms: 60_000,
        }),
        output: OutputScore {
            name: template.output_score_name.to_owned(),
            data_type,
            categories,
        },
    }
}

fn text_span(input: Option<&str>, output: &str) -> Span {
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
    if let Some(input) = input {
        span.input = Some(input.to_owned());
        span.input_format = Some(PayloadFormat::Text);
    }
    span.output = Some(output.to_owned());
    span.output_format = Some(PayloadFormat::Text);
    span
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

fn ingest() -> IngestContext {
    IngestContext {
        project_id: PROJECT.parse::<ProjectId>().expect("project"),
        ingest_time: i64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
        )
        .unwrap_or(i64::MAX),
        retention_days: Some(7),
        limits: IngestLimits::default(),
    }
}

fn prices() -> &'static PriceTable {
    static PRICES: OnceLock<PriceTable> = OnceLock::new();
    PRICES.get_or_init(|| PriceTable::new(builtin_price_rows()).expect("built-in prices"))
}

#[allow(clippy::print_stdout)]
fn skip(name: &str) {
    println!("skipped: {name} (no key)");
}
