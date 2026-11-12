//! One evaluator run: a check, a judge call, or a webhook, plus its judge trace.
//!
//! [`now_ns`] is the only clock in the crate. A run never returns an error by
//! itself: the score or the failure is [`EvalRun::outcome`], and the trace is
//! always built.

use std::collections::BTreeMap;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use llmobs_core::ids::TraceId;
use llmobs_core::score::ScoreValue;
use llmobs_otel::{IngestContext, Normalized, PriceTable};

use crate::check::{CheckInput, run_check};
use crate::config::{Check, Evaluator, EvaluatorConfig, JudgeConfig, WebhookConfig};
use crate::egress::{EgressClient, EgressPolicy};
use crate::error::{ConfigError, EvalError, JudgeError, WebhookError};
use crate::judge::{Adapter, JudgeCall, JudgeConnection, JudgeUsage, call, parse_answer};
use crate::mapping::resolve::Variables;
use crate::secret::Secret;
use crate::template::PromptTemplate;
use crate::trace::{ChildSpan, EvalOrigin, EvalTarget, TraceInput, build, judge_metadata};
use crate::webhook::{self, WebhookCall};

/// Runs checks locally and sends judge and webhook calls through one client.
#[derive(Debug)]
pub struct EvalRunner {
    client: EgressClient,
}

/// Where the run sits, and the material the server already decrypted.
#[derive(Debug)]
pub struct RunInput<'a> {
    /// The evaluator to run.
    pub evaluator: &'a Evaluator,
    /// Variables for the check, the template, or the webhook body.
    pub variables: &'a Variables,
    /// Decrypted provider connection. Required for a judge.
    pub connection: Option<&'a JudgeConnection>,
    /// Decrypted signing secret. Required for a webhook.
    pub webhook_secret: Option<&'a Secret>,
    /// Trace and span the score is about.
    pub target: EvalTarget,
    /// Rule, experiment, or the test panel.
    pub origin: EvalOrigin,
    /// Id of the judge trace. A retry reuses the job's id.
    pub judge_trace_id: TraceId,
    /// Project, clock, and limits for normalization. The project is `project_id`.
    pub ingest: &'a IngestContext,
    /// Prices for the judge's `llm` span.
    pub prices: &'a PriceTable,
}

/// The score or the failure, and the normalized judge trace either way.
///
/// [`Debug`](fmt::Debug) counts the judge trace's spans instead of printing
/// their payloads.
#[derive(Clone, PartialEq)]
pub struct EvalRun {
    /// `Ok` when a score was produced.
    pub outcome: Result<EvalOutcome, EvalError>,
    /// Spans for this run, including a failed one.
    pub judge_trace: Normalized,
}

/// A score and the judge metadata copied off the normalized trace (D161).
///
/// [`Debug`](fmt::Debug) prints the reason's length, not its text.
#[derive(Clone, PartialEq)]
pub struct EvalOutcome {
    /// The score.
    pub value: ScoreValue,
    /// Explanation or check reason. Absent when the run produced none.
    pub reason: Option<String>,
    /// `judge.*` keys read from the normalized trace.
    pub metadata: BTreeMap<String, String>,
    /// `cost_total` of the `llm` span.
    pub cost_usd: Option<f64>,
    /// The `llm` span had tokens and no price.
    pub unpriced_model: bool,
}

impl fmt::Debug for EvalRun {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvalRun")
            .field("outcome", &self.outcome)
            .field("judge_spans", &self.judge_trace.spans.len())
            .field("judge_media", &self.judge_trace.media.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for EvalOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvalOutcome")
            .field("value", &self.value)
            .field(
                "reason_chars",
                &self.reason.as_ref().map(|reason| reason.chars().count()),
            )
            .field("metadata", &self.metadata)
            .field("cost_usd", &self.cost_usd)
            .field("unpriced_model", &self.unpriced_model)
            .finish()
    }
}

struct Finished {
    outcome: Result<EvalOutcome, EvalError>,
    child: Option<OwnedChild>,
}

enum OwnedChild {
    Llm {
        started_ns: i64,
        ended_ns: i64,
        adapter: Adapter,
        request_model: String,
        response_model: Option<String>,
        prompt: String,
        output_text: Option<String>,
        usage: Option<JudgeUsage>,
        error: Option<JudgeError>,
    },
    Webhook {
        started_ns: i64,
        ended_ns: i64,
        host: String,
        status: Option<u16>,
        request_body: Vec<u8>,
        response_body: Option<serde_json::Value>,
        error: Option<WebhookError>,
    },
}

impl EvalRunner {
    /// Builds the shared client.
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::Egress`] when the TLS client cannot be created.
    pub fn new(policy: EgressPolicy) -> Result<Self, EvalError> {
        Ok(Self {
            client: EgressClient::new(policy)?,
        })
    }

    /// Runs `input` once. A failure is [`EvalRun::outcome`]; the trace is always set.
    pub async fn run(&self, input: RunInput<'_>) -> EvalRun {
        let started = now_ns();
        let finished = match &input.evaluator.config {
            EvaluatorConfig::LlmJudge(config) => self.judge(&input, config).await,
            EvaluatorConfig::Check(check) => run_one_check(&input, check),
            EvaluatorConfig::Webhook(config) => self.webhook(&input, config).await,
        };
        let ended = now_ns().max(started);
        tracing::debug!(
            evaluator_id = %input.evaluator.id,
            kind = input.evaluator.config.kind(),
            duration_ns = ended.saturating_sub(started),
            error_class = class_label(&finished.outcome),
            "evaluator run"
        );
        let judge_trace = trace_of(&input, started, ended, &finished);
        let outcome = match finished.outcome {
            Ok(mut outcome) => {
                let (metadata, cost, unpriced) = judge_metadata(&judge_trace, input.judge_trace_id);
                outcome.metadata = metadata;
                outcome.cost_usd = cost;
                outcome.unpriced_model = unpriced;
                Ok(outcome)
            }
            Err(error) => Err(error),
        };
        EvalRun {
            outcome,
            judge_trace,
        }
    }

    async fn judge(&self, input: &RunInput<'_>, config: &JudgeConfig) -> Finished {
        let Some(connection) = input.connection else {
            return Finished::failed(ConfigError::Invalid {
                field: "connection",
                reason: "required",
            });
        };
        let template = match PromptTemplate::compile(&config.prompt) {
            Ok(template) => template,
            Err(error) => return Finished::failed(error),
        };
        let prompt = match template.render(input.variables) {
            Ok(prompt) => prompt,
            Err(error) => return Finished::failed(error),
        };
        let request = JudgeCall {
            model: &config.model,
            prompt: &prompt,
            output: &config.output,
            include_reason: config.include_reason,
            temperature: config.temperature,
            max_output_tokens: config.max_output_tokens,
            timeout: std::time::Duration::from_millis(u64::from(config.timeout_ms)),
        };
        let started = now_ns();
        let reply = call(&self.client, connection, &request).await;
        let ended = now_ns().max(started);
        match reply {
            Ok(reply) => {
                let parsed = parse_answer(reply.answer, &config.output, config.include_reason);
                let error = parsed.as_ref().err().cloned();
                let child = OwnedChild::Llm {
                    started_ns: started,
                    ended_ns: ended,
                    adapter: connection.adapter,
                    request_model: config.model.clone(),
                    response_model: reply.response_model,
                    prompt,
                    output_text: Some(reply.output_text),
                    usage: Some(reply.usage),
                    error,
                };
                match parsed {
                    Ok(answer) => Finished {
                        outcome: Ok(outcome(answer.value, answer.explanation)),
                        child: Some(child),
                    },
                    Err(error) => Finished {
                        outcome: Err(error.into()),
                        child: Some(child),
                    },
                }
            }
            Err(error) => Finished {
                outcome: Err(EvalError::from(error.clone())),
                child: Some(OwnedChild::Llm {
                    started_ns: started,
                    ended_ns: ended,
                    adapter: connection.adapter,
                    request_model: config.model.clone(),
                    response_model: None,
                    prompt,
                    output_text: None,
                    usage: None,
                    error: Some(error),
                }),
            },
        }
    }

    async fn webhook(&self, input: &RunInput<'_>, config: &WebhookConfig) -> Finished {
        let Some(secret) = input.webhook_secret else {
            return Finished::failed(ConfigError::Invalid {
                field: "secret",
                reason: "required",
            });
        };
        let started = now_ns();
        let invocation = WebhookCall {
            config,
            secret,
            evaluator_id: input.evaluator.id,
            evaluator_name: &input.evaluator.name,
            project_id: input.ingest.project_id,
            trace_id: input.target.trace_id,
            span_id: input.target.span_id,
            variables: input.variables,
            output: &input.evaluator.output,
            now_unix_s: started.div_euclid(1_000_000_000),
        };
        let sent = webhook::exchange(&self.client, &invocation).await;
        let ended = now_ns().max(started);
        let webhook::WebhookExchange {
            host,
            status,
            request_body,
            response_body,
            result,
        } = sent;
        let (outcome, error) = match result {
            Ok(reply) => (Ok(outcome(reply.value, reply.reason)), None),
            Err(error) => (Err(EvalError::from(error.clone())), Some(error)),
        };
        let child = host.map(|host| OwnedChild::Webhook {
            started_ns: started,
            ended_ns: ended,
            host,
            status,
            request_body,
            response_body,
            error,
        });
        Finished { outcome, child }
    }
}

impl Finished {
    fn failed(error: impl Into<EvalError>) -> Self {
        Self {
            outcome: Err(error.into()),
            child: None,
        }
    }
}

impl OwnedChild {
    fn view(&self) -> ChildSpan<'_> {
        match self {
            Self::Llm {
                started_ns,
                ended_ns,
                adapter,
                request_model,
                response_model,
                prompt,
                output_text,
                usage,
                error,
            } => ChildSpan::Llm {
                started_ns: *started_ns,
                ended_ns: *ended_ns,
                adapter: *adapter,
                request_model,
                response_model: response_model.as_deref(),
                prompt,
                output_text: output_text.as_deref(),
                usage: usage.as_ref(),
                error: error.as_ref(),
            },
            Self::Webhook {
                started_ns,
                ended_ns,
                host,
                status,
                request_body,
                response_body,
                error,
            } => ChildSpan::Webhook {
                started_ns: *started_ns,
                ended_ns: *ended_ns,
                host,
                status: *status,
                request_body,
                response_body: response_body.as_ref(),
                error: error.as_ref(),
            },
        }
    }
}

fn run_one_check(input: &RunInput<'_>, check: &Check) -> Finished {
    let check_input = CheckInput {
        output: input.variables.get("output"),
        expected: input.variables.get("expected"),
    };
    match run_check(check, &check_input) {
        Ok(result) => Finished {
            outcome: Ok(outcome(result.value, result.reason)),
            child: None,
        },
        Err(error) => Finished::failed(error),
    }
}

fn outcome(value: ScoreValue, reason: Option<String>) -> EvalOutcome {
    EvalOutcome {
        value,
        reason,
        metadata: BTreeMap::new(),
        cost_usd: None,
        unpriced_model: false,
    }
}

fn trace_of(input: &RunInput<'_>, started: i64, ended: i64, finished: &Finished) -> Normalized {
    let result = match &finished.outcome {
        Ok(outcome) => Ok((&outcome.value, outcome.reason.as_deref())),
        Err(error) => Err(error),
    };
    build(
        &TraceInput {
            trace_id: input.judge_trace_id,
            evaluator: input.evaluator,
            target: &input.target,
            origin: &input.origin,
            started_ns: started,
            ended_ns: ended,
            variables: input.variables,
            result,
            child: finished.child.as_ref().map(OwnedChild::view),
        },
        input.ingest,
        input.prices,
    )
}

fn class_label(outcome: &Result<EvalOutcome, EvalError>) -> &'static str {
    match outcome {
        Ok(_) => "none",
        Err(error) => error.class().as_str(),
    }
}

/// Nanoseconds since the Unix epoch. A clock before the epoch is negative.
fn now_ns() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX),
        Err(earlier) => -i64::try_from(earlier.duration().as_nanos()).unwrap_or(i64::MAX),
    }
}
