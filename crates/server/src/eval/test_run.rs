//! One synchronous evaluator test run (D-I-21, spec 06 § Evaluators). It writes the judge trace
//! and the judge usage, and no score. The body rules and the answer are § Test run contract.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use llmobs_core::enums::SpanKind;
use llmobs_core::ids::{SpanId, TraceId};
use llmobs_eval::{
    Check, ErrorClass, EvalOrigin, EvalRun, EvalTarget, EvaluatorConfig, OutputScore,
    PromptTemplate, RunInput, Secret, TraceData, VariableMapping, Variables,
};
use serde_json::Value;
use uuid::Uuid;

use crate::api::common::{FieldError, TimeParam};
use crate::api::connections::TestErrorClass;
use crate::api::evaluators::{
    EvaluatorKind, EvaluatorTestError, EvaluatorTestRequest, EvaluatorTestResult,
    EvaluatorTestUsage, OutputScoreBody, TestEvaluatorBody,
};
use crate::eval::evaluator::{self, EvaluatorError, Prepared};
use crate::eval::trace_load::{self, LoadError};
use crate::http::AppState;
use crate::http::error::ApiError;
use crate::ingest::{CommitTarget, commit_in_task};
use crate::meta::connections;
use crate::meta::evaluators::{self as store, EvaluatorRow};
use crate::meta::projects::ProjectRow;

/// Longest rendered variable in an answer, in `char`s, before the cut (D-I-21).
const ANSWER_CHARS: usize = 2_000;
/// Most literal variables (§ Test run contract).
const MAX_VARIABLES: usize = 32;
/// Most bytes the literal variables may take in all.
const MAX_VARIABLE_BYTES: usize = 256 * 1024;
/// Longest `output` and `expected` of a levenshtein check, in bytes (the distance is quadratic).
const MAX_LEVENSHTEIN_BYTES: usize = 64 * 1024;

/// Where a run's variables come from, as the body asks. Built only when the body is valid.
enum Source {
    /// A stored trace, read with a mapping.
    Trace {
        trace_id: TraceId,
        span_id: Option<SpanId>,
        around: Option<i64>,
        mapping: VariableMapping,
    },
    /// Literal variables.
    Literal(BTreeMap<String, Value>),
}

/// Runs one test (D-I-21). `user` is the caller, for the rate limit.
///
/// # Errors
///
/// Returns [`ApiError::RateLimited`] past 10 tests a minute, [`ApiError::BadRequest`] with every
/// field error of an invalid body, 404 for an unknown evaluator or trace, and 503 when storage is
/// busy. A failed evaluator is an answer with `ok: false`, not an error.
pub(crate) async fn run(
    state: &AppState,
    project: &ProjectRow,
    user: Uuid,
    body: EvaluatorTestRequest,
) -> Result<EvaluatorTestResult, ApiError> {
    state
        .limits
        .connection_tests
        .check(&user, state.clock.now_ns())
        .map_err(|retry| ApiError::RateLimited {
            retry_after_s: retry.0.as_secs().max(1),
        })?;
    let EvaluatorTestRequest {
        evaluator_id,
        evaluator,
        trace_id,
        span_id,
        trace_start,
        variable_mapping,
        variables,
    } = body;
    let mut errors = Vec::new();
    let source = parse_source(
        trace_id.as_deref(),
        span_id.as_deref(),
        trace_start.map(TimeParam::ns),
        variable_mapping,
        variables,
        &mut errors,
    );
    let saved = match evaluator_id {
        Some(id) => Some(
            store::get_evaluator(state.meta.reader(), project.id, id)
                .await?
                .ok_or(ApiError::NotFound)?,
        ),
        None => None,
    };
    let prepared = evaluator_to_run(state, saved.as_ref(), evaluator, &mut errors)?;
    let (Some(source), Some(prepared)) = (source, prepared) else {
        return Err(bad_request(errors));
    };
    let connection = match prepared.connection_id {
        Some(connection) => {
            let row = connections::get_connection(state.meta.reader(), project.org_id, connection)
                .await?;
            match row {
                Some(row) => Some(
                    state
                        .connections
                        .get(&state.secrets, &row)
                        .map_err(internal)?,
                ),
                None => {
                    return Err(bad_request(vec![field_error(
                        "evaluator.config.connection_id",
                        "connection not found in this organization",
                    )]));
                }
            }
        }
        None => None,
    };

    let (variables, target, truncated) = match source {
        Source::Literal(map) => (
            Variables::from_map(map),
            EvalTarget {
                trace_id: None,
                span_id: None,
            },
            false,
        ),
        Source::Trace {
            trace_id,
            span_id,
            around,
            mapping,
        } => {
            let loaded =
                match trace_load::load_trace(state, project.id, trace_id, span_id, around).await {
                    Ok(loaded) => loaded,
                    Err(LoadError::TooLarge) => {
                        return Ok(refused(TestErrorClass::Permanent, "trace too large", true));
                    }
                    Err(LoadError::NotFound | LoadError::NoTargetSpan) => {
                        return Err(ApiError::NotFound);
                    }
                    Err(LoadError::EvaluationTrace) => {
                        return Err(ApiError::BadRequest {
                            detail: Some("target is an evaluation trace".to_owned()),
                            errors: Vec::new(),
                        });
                    }
                    Err(LoadError::Busy) => {
                        return Err(ApiError::Unavailable {
                            detail: "storage is busy",
                        });
                    }
                    Err(error @ (LoadError::Failed(_) | LoadError::Convert(_))) => {
                        return Err(internal(error));
                    }
                };
            let data = TraceData::new(&loaded.spans, span_id);
            match llmobs_eval::resolve(&mapping, Some(&data), None) {
                Ok(variables) => (
                    variables,
                    EvalTarget {
                        trace_id: Some(trace_id),
                        span_id,
                    },
                    loaded.truncated,
                ),
                // A mapping error is refused before the evaluator runs: no judge trace.
                Err(error) => {
                    return Ok(refused(
                        TestErrorClass::Permanent,
                        &error.to_string(),
                        false,
                    ));
                }
            }
        }
    };
    let is_levenshtein = matches!(
        prepared.evaluator.config,
        EvaluatorConfig::Check(Check::Levenshtein { .. })
    );
    if is_levenshtein
        && variables.iter().any(|(name, value)| {
            matches!(name, "output" | "expected") && render(value).len() > MAX_LEVENSHTEIN_BYTES
        })
    {
        return Err(bad_request(vec![field_error(
            "evaluator.variables",
            "levenshtein inputs must be at most 64 KiB each",
        )]));
    }
    let rendered = rendered_variables(&variables);
    let prompt = match &prepared.evaluator.config {
        EvaluatorConfig::LlmJudge(judge) => PromptTemplate::compile(&judge.prompt)
            .and_then(|template| template.render(&variables))
            .ok(),
        EvaluatorConfig::Check(_) | EvaluatorConfig::Webhook(_) => None,
    };

    let judge_trace_id = judge_trace_id()?;
    let now = state.clock.now_ns();
    let ingest = state.ingest.context(project, now);
    let prices = state.ingest.prices();
    let signing_secret = prepared
        .secrets
        .as_ref()
        .map(|values| Secret::new(values.signing_secret.clone()));
    let evaluator_id = prepared.evaluator.id;
    let started = Instant::now();
    let run = if matches!(prepared.evaluator.config, EvaluatorConfig::Check(_)) {
        // Checks are CPU work: they run off the async workers (D-I-23).
        let runner = Arc::clone(&state.eval_runner);
        let handle = tokio::runtime::Handle::current();
        let evaluator = prepared.evaluator;
        tokio::task::spawn_blocking(move || {
            handle.block_on(async move {
                runner
                    .run(RunInput {
                        evaluator: &evaluator,
                        variables: &variables,
                        connection: None,
                        webhook_secret: None,
                        target,
                        origin: EvalOrigin::Test,
                        judge_trace_id,
                        ingest: &ingest,
                        prices: &prices,
                    })
                    .await
            })
        })
        .await
        .map_err(internal)?
    } else {
        state
            .eval_runner
            .run(RunInput {
                evaluator: &prepared.evaluator,
                variables: &variables,
                connection: connection.as_deref(),
                webhook_secret: signing_secret.as_ref(),
                target,
                origin: EvalOrigin::Test,
                judge_trace_id,
                ingest: &ingest,
                prices: &prices,
            })
            .await
    };
    let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let EvalRun {
        outcome,
        judge_trace,
    } = run;
    let llm = judge_trace
        .spans
        .iter()
        .find(|span| span.span_kind == SpanKind::Llm);
    let usage = llm.map(|span| {
        let input_tokens = span.input_tokens.unwrap_or(0);
        let output_tokens = span.output_tokens.unwrap_or(0);
        state.usage.add_judge(
            project.org_id,
            project.id,
            now,
            u64::try_from(input_tokens).unwrap_or(0),
            u64::try_from(output_tokens).unwrap_or(0),
            span.cost_total.unwrap_or(0.0),
        );
        EvaluatorTestUsage {
            input_tokens: span
                .input_tokens
                .and_then(|count| u64::try_from(count).ok()),
            output_tokens: span
                .output_tokens
                .and_then(|count| u64::try_from(count).ok()),
            cost_usd: span.cost_total,
        }
    });
    // The judge trace is committed on its own (D-I-16); its spans are not billable (D-I-31).
    let reservation = state.ingest.reserve_exact(0)?;
    commit_in_task(
        state,
        CommitTarget {
            org_id: project.org_id,
            project_id: project.id,
            request_bytes: 0,
            now_ns: now,
        },
        judge_trace,
        reservation,
    )
    .await?;

    let mut result = EvaluatorTestResult {
        ok: false,
        value: None,
        reason: None,
        error: None,
        prompt,
        variables: rendered,
        usage,
        latency_ms,
        trace_truncated: truncated,
        judge_trace_id: Some(judge_trace_id),
    };
    match outcome {
        Ok(outcome) => {
            result.ok = true;
            result.value = Some(outcome.value);
            result.reason = outcome.reason;
        }
        Err(error) => {
            result.error = Some(EvaluatorTestError {
                class: test_class(error.class()),
                // The display quotes no payload (D160).
                message: error.to_string(),
            });
        }
    }
    tracing::info!(
        target: "audit",
        evaluator_id = %evaluator_id,
        project_id = %project.id,
        user_id = %user,
        ok = result.ok,
        latency_ms,
        "evaluator tested"
    );
    Ok(result)
}

/// The body rules that need no database (§ Test run contract). Each failure is pushed to
/// `errors`; the source is returned only when none was pushed.
fn parse_source(
    trace_id: Option<&str>,
    span_id: Option<&str>,
    trace_start: Option<i64>,
    mapping: Option<BTreeMap<String, String>>,
    variables: Option<BTreeMap<String, Value>>,
    errors: &mut Vec<FieldError>,
) -> Option<Source> {
    let before = errors.len();
    let has_trace = trace_id.is_some();
    if has_trace == variables.is_some() {
        errors.push(field_error(
            "variables",
            "give trace_id with variable_mapping, or variables",
        ));
    }
    if has_trace && mapping.is_none() {
        errors.push(field_error(
            "variable_mapping",
            "variable_mapping is required with trace_id",
        ));
    }
    if !has_trace && mapping.is_some() {
        errors.push(field_error(
            "trace_id",
            "trace_id is required with variable_mapping",
        ));
    }
    if !has_trace && (span_id.is_some() || trace_start.is_some()) {
        errors.push(field_error(
            "trace_id",
            "trace_id is required with span_id and trace_start",
        ));
    }
    let trace = match trace_id.map(str::parse::<TraceId>) {
        Some(Ok(id)) => Some(id),
        Some(Err(_)) => {
            errors.push(field_error("trace_id", "trace_id must be 32 hex digits"));
            None
        }
        None => None,
    };
    let span = match span_id.map(str::parse::<SpanId>) {
        Some(Ok(id)) => Some(id),
        Some(Err(_)) => {
            errors.push(field_error("span_id", "span_id must be 16 hex digits"));
            None
        }
        None => None,
    };
    let mapping = mapping.and_then(|map| {
        let value = Value::Object(
            map.into_iter()
                .map(|(name, expression)| (name, Value::String(expression)))
                .collect(),
        );
        match VariableMapping::from_json(&value) {
            Ok(mapping) if mapping.needs_item() => {
                errors.push(field_error(
                    "variable_mapping",
                    "expected and item.* are for experiments only",
                ));
                None
            }
            Ok(mapping) => Some(mapping),
            Err(error) => {
                errors.push(field_error("variable_mapping", &error.to_string()));
                None
            }
        }
    });
    if let Some(map) = &variables {
        if map.len() > MAX_VARIABLES {
            errors.push(field_error("variables", "at most 32 variables"));
        }
        if !map.keys().all(|name| valid_name(name)) {
            errors.push(field_error("variables", "invalid variable name"));
        }
        let total = map
            .values()
            .map(|value| serde_json::to_string(value).map_or(0, |text| text.len()))
            .fold(0_usize, usize::saturating_add);
        if total > MAX_VARIABLE_BYTES {
            errors.push(field_error(
                "variables",
                "variables must be at most 256 KiB in total",
            ));
        }
    }
    if errors.len() > before {
        return None;
    }
    match variables {
        Some(map) if !has_trace => Some(Source::Literal(map)),
        _ => Some(Source::Trace {
            trace_id: trace?,
            span_id: span,
            around: trace_start,
            mapping: mapping?,
        }),
    }
}

/// The evaluator to run (§ Test run contract). `None` when the body is invalid; the errors are in
/// `errors`.
fn evaluator_to_run(
    state: &AppState,
    saved: Option<&EvaluatorRow>,
    body: Option<TestEvaluatorBody>,
    errors: &mut Vec<FieldError>,
) -> Result<Option<Prepared>, ApiError> {
    match (saved, body) {
        (None, None) => {
            errors.push(field_error(
                "evaluator",
                "give evaluator_id, evaluator, or both",
            ));
            Ok(None)
        }
        (Some(row), None) => {
            let (current, opened) = evaluator::load(&state.secrets, row).map_err(load_error)?;
            let config =
                evaluator::config_document(&current, opened.as_ref()).map_err(load_error)?;
            let secret = opened.map(|values| values.signing_secret);
            Ok(keep(
                evaluator::prepare(
                    current.id,
                    &current.name,
                    kind_of(&current.config),
                    &config,
                    &current.output,
                    secret.as_deref(),
                    "evaluator.",
                ),
                errors,
            ))
        }
        (Some(row), Some(body)) => {
            let (current, opened) = evaluator::load(&state.secrets, row).map_err(load_error)?;
            let kind = kind_of(&current.config);
            if body.kind != kind {
                errors.push(field_error(
                    "evaluator.kind",
                    "kind must match the saved evaluator",
                ));
                return Ok(None);
            }
            if kind == EvaluatorKind::Webhook
                && body.config.is_some()
                && body.webhook_secret.is_none()
            {
                errors.push(field_error(
                    "evaluator.webhook_secret",
                    evaluator::CONFIG_NEEDS_SECRET,
                ));
                return Ok(None);
            }
            let name = body.name.unwrap_or_else(|| current.name.clone());
            let config = match body.config {
                Some(config) => config,
                None => {
                    evaluator::config_document(&current, opened.as_ref()).map_err(load_error)?
                }
            };
            let secret = body
                .webhook_secret
                .or_else(|| opened.map(|values| values.signing_secret));
            Ok(keep(
                evaluator::prepare(
                    current.id,
                    &name,
                    kind,
                    &config,
                    &output_of(&body.output_score),
                    secret.as_deref(),
                    "evaluator.",
                ),
                errors,
            ))
        }
        (None, Some(body)) => {
            let name = body.name.unwrap_or_else(|| "Test".to_owned());
            match body.config {
                Some(config) => Ok(keep(
                    evaluator::prepare(
                        Uuid::nil(),
                        &name,
                        body.kind,
                        &config,
                        &output_of(&body.output_score),
                        body.webhook_secret.as_deref(),
                        "evaluator.",
                    ),
                    errors,
                )),
                None => {
                    errors.push(field_error(
                        "evaluator.config",
                        "config is required without evaluator_id",
                    ));
                    if body.kind == EvaluatorKind::Webhook && body.webhook_secret.is_none() {
                        errors.push(field_error(
                            "evaluator.webhook_secret",
                            "webhook_secret is required",
                        ));
                    }
                    Ok(None)
                }
            }
        }
    }
}

/// Keeps a prepared evaluator, or moves its field errors into `errors`.
fn keep(
    result: Result<Prepared, Vec<FieldError>>,
    errors: &mut Vec<FieldError>,
) -> Option<Prepared> {
    match result {
        Ok(prepared) => Some(prepared),
        Err(found) => {
            errors.extend(found);
            None
        }
    }
}

fn kind_of(config: &EvaluatorConfig) -> EvaluatorKind {
    match config {
        EvaluatorConfig::LlmJudge(_) => EvaluatorKind::LlmJudge,
        EvaluatorConfig::Check(_) => EvaluatorKind::Check,
        EvaluatorConfig::Webhook(_) => EvaluatorKind::Webhook,
    }
}

fn output_of(body: &OutputScoreBody) -> OutputScore {
    OutputScore {
        name: body.name.clone(),
        data_type: body.data_type,
        categories: body.categories.clone(),
    }
}

/// A variable name: `^[A-Za-z_][A-Za-z0-9_]{0,63}$`.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    name.is_ascii()
        && name.len() <= 64
        && chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|next| next.is_ascii_alphanumeric() || next == '_')
}

/// A variable as the answer shows it: a string as itself, anything else as compact JSON.
fn render(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// Every variable rendered and cut to [`ANSWER_CHARS`] characters, with a final `…` when cut.
fn rendered_variables(variables: &Variables) -> BTreeMap<String, String> {
    variables
        .iter()
        .map(|(name, value)| {
            let text = render(value);
            let text = if text.chars().count() > ANSWER_CHARS {
                let mut cut: String = text.chars().take(ANSWER_CHARS).collect();
                cut.push('…');
                cut
            } else {
                text
            };
            (name.to_owned(), text)
        })
        .collect()
}

/// The judge trace id: 16 random bytes (as M1's connection test).
fn judge_trace_id() -> Result<TraceId, ApiError> {
    let bytes = crate::auth::secret::random_bytes(16).map_err(internal)?;
    let bytes = <[u8; 16]>::try_from(bytes.as_slice()).map_err(internal)?;
    // Refuses all-zero bytes only.
    TraceId::from_bytes(bytes).map_err(internal)
}

fn test_class(class: ErrorClass) -> TestErrorClass {
    match class {
        ErrorClass::Retryable => TestErrorClass::Retryable,
        ErrorClass::Pause => TestErrorClass::Pause,
        ErrorClass::Permanent => TestErrorClass::Permanent,
    }
}

/// A failed answer that wrote no judge trace and ran nothing.
fn refused(class: TestErrorClass, message: &str, truncated: bool) -> EvaluatorTestResult {
    EvaluatorTestResult {
        ok: false,
        value: None,
        reason: None,
        error: Some(EvaluatorTestError {
            class,
            message: message.to_owned(),
        }),
        prompt: None,
        variables: BTreeMap::new(),
        usage: None,
        latency_ms: 0,
        trace_truncated: truncated,
        judge_trace_id: None,
    }
}

fn field_error(field: &str, message: &str) -> FieldError {
    FieldError {
        field: field.to_owned(),
        message: message.to_owned(),
        position: None,
    }
}

fn bad_request(errors: Vec<FieldError>) -> ApiError {
    ApiError::BadRequest {
        detail: None,
        errors,
    }
}

fn load_error(error: EvaluatorError) -> ApiError {
    ApiError::Internal(Box::new(error))
}

fn internal(error: impl std::error::Error + Send + Sync + 'static) -> ApiError {
    ApiError::Internal(Box::new(error))
}
