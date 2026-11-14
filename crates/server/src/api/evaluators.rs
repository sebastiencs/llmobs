//! Evaluator request and response types, built-in templates, and test runs.
//!
//! Spec 09 § Evaluation & alerts and spec 06 § Evaluators. The wire contract is
//! in `crates/server/openapi.json`.

use std::collections::BTreeMap;
use std::fmt;

use llmobs_core::enums::ScoreDataType;
use llmobs_core::ids::TraceId;
use llmobs_core::score::ScoreValue;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use super::common::TimeParam;
use super::connections::TestErrorClass;

/// What an evaluator does. Stored as `evaluators.kind`; `kind` cannot change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvaluatorKind {
    /// An LLM judge that answers a question about the trace.
    LlmJudge,
    /// A deterministic check over the trace.
    Check,
    /// An HTTP webhook that returns the score.
    Webhook,
}

impl EvaluatorKind {
    /// The stored evaluator kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LlmJudge => "llm_judge",
            Self::Check => "check",
            Self::Webhook => "webhook",
        }
    }

    /// Parses the stored evaluator kind. Any other text is `None`.
    pub fn from_name(text: &str) -> Option<Self> {
        match text {
            "llm_judge" => Some(Self::LlmJudge),
            "check" => Some(Self::Check),
            "webhook" => Some(Self::Webhook),
            _ => None,
        }
    }
}

/// The score an evaluator writes. The JSON of `llmobs_eval::OutputScore`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OutputScoreBody {
    /// Score name, 1 to 255 bytes.
    pub name: String,
    /// Score data type.
    pub data_type: ScoreDataType,
    /// Category labels of a categorical score. Omitted otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<String>>,
}

/// An evaluator. Sealed values (a webhook's signing secret and header values) are not included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Evaluator {
    /// Evaluator id.
    pub id: Uuid,
    /// Display name, 1 to 128 characters.
    pub name: String,
    /// What the evaluator does.
    pub kind: EvaluatorKind,
    /// The kind-specific configuration as stored. A webhook keeps only its URL host, timeout and header names.
    #[schema(value_type = Object)]
    pub config: Value,
    /// The score the evaluator writes.
    pub output_score: OutputScoreBody,
    /// Whether a signing secret is stored. True for a webhook.
    pub has_signing_secret: bool,
    /// Starts at 1 and increases on every change to the config, the output or the sealed values.
    pub version: i64,
    /// User who created the evaluator. Omitted when that user is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Uuid>,
    /// Creation time. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: i64,
    /// Last change time. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: i64,
}

/// `POST /api/v1/projects/{project}/evaluators` body.
///
/// Its `Debug` prints flags, not the config or the secret.
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateEvaluatorRequest {
    /// Display name, 1 to 128 characters.
    pub name: String,
    /// What the evaluator does. Immutable once created.
    pub kind: EvaluatorKind,
    /// The kind-specific configuration. For a webhook, the full `{url, timeout_ms?, headers?}` document.
    #[schema(value_type = Object)]
    pub config: Value,
    /// The score the evaluator writes.
    pub output_score: OutputScoreBody,
    /// Signing secret of a webhook, 16 to 256 characters. Write-only, and required for a webhook.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<String>,
}

impl fmt::Debug for CreateEvaluatorRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateEvaluatorRequest")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("has_config", &!self.config.is_null())
            .field("has_signing_secret", &self.webhook_secret.is_some())
            .finish()
    }
}

/// `PATCH /api/v1/projects/{project}/evaluators/{id}` body. Absent means unchanged.
///
/// Its `Debug` prints flags, not the config or the secret.
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateEvaluatorRequest {
    /// Display name, 1 to 128 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The kind-specific configuration. For a webhook, replaces the URL, the headers and the timeout together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub config: Option<Value>,
    /// The score the evaluator writes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_score: Option<OutputScoreBody>,
    /// Replaces the signing secret of a webhook. An empty string is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<String>,
}

impl fmt::Debug for UpdateEvaluatorRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateEvaluatorRequest")
            .field("name", &self.name)
            .field("has_config", &self.config.is_some())
            .field("has_output_score", &self.output_score.is_some())
            .field("has_signing_secret", &self.webhook_secret.is_some())
            .finish()
    }
}

/// One mapping of a template: the prompt variable and the expression that fills it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TemplateMapping {
    /// Prompt variable name.
    pub variable: String,
    /// Mapping expression, such as `trace.input`.
    pub expression: String,
}

/// A built-in judge template. The UI copies it into the editor; the server does not create evaluators from it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EvaluatorTemplate {
    /// Stable id: `hallucination`, `relevance`, `toxicity`, `correctness` or `custom`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// One-line description.
    pub description: String,
    /// The prompt template.
    pub prompt: String,
    /// The score the judge must produce, as the judge output JSON.
    #[schema(value_type = Object)]
    pub output: Value,
    /// Name of the score the template writes.
    pub output_score_name: String,
    /// The variables of the prompt and the expressions that fill them, in display order.
    pub default_mapping: Vec<TemplateMapping>,
}

/// `GET /api/v1/projects/{project}/evaluator-templates` response. The list is fixed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EvaluatorTemplateList {
    /// The built-in templates, in display order.
    pub data: Vec<EvaluatorTemplate>,
}

impl From<&llmobs_eval::JudgeTemplate> for EvaluatorTemplate {
    fn from(template: &llmobs_eval::JudgeTemplate) -> Self {
        Self {
            id: template.id.to_owned(),
            name: template.name.to_owned(),
            description: template.description.to_owned(),
            prompt: template.prompt.to_owned(),
            // A `JudgeOutput` always serializes; the fallback is never reached.
            output: serde_json::to_value(&template.output).unwrap_or(Value::Null),
            output_score_name: template.output_score_name.to_owned(),
            default_mapping: template
                .default_mapping
                .iter()
                .map(|(variable, expression)| TemplateMapping {
                    variable: (*variable).to_owned(),
                    expression: (*expression).to_owned(),
                })
                .collect(),
        }
    }
}

/// The built-in templates, in `llmobs_eval::builtin_templates` order.
pub fn builtin_template_list() -> EvaluatorTemplateList {
    EvaluatorTemplateList {
        data: llmobs_eval::builtin_templates()
            .iter()
            .map(EvaluatorTemplate::from)
            .collect(),
    }
}

/// An evaluator to test, possibly partial. Absent fields come from the saved evaluator.
///
/// Its `Debug` prints flags, not the config or the secret.
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TestEvaluatorBody {
    /// What the evaluator does. Must match the saved evaluator's kind.
    pub kind: EvaluatorKind,
    /// Display name. Absent: the saved name, or `Test`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The kind-specific configuration. Absent: the saved configuration, which needs `evaluator_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<Object>)]
    pub config: Option<Value>,
    /// The score the evaluator writes.
    pub output_score: OutputScoreBody,
    /// Signing secret of a webhook. Absent: the saved secret, which needs `evaluator_id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_secret: Option<String>,
}

impl fmt::Debug for TestEvaluatorBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TestEvaluatorBody")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .field("has_config", &self.config.is_some())
            .field("has_signing_secret", &self.webhook_secret.is_some())
            .finish()
    }
}

/// `POST /api/v1/projects/{project}/evaluators/test` body.
///
/// Deserialize only: `TimeParam` has no `Serialize`. Its `Debug` prints flags and counts, not
/// the variables, the mapping or the evaluator.
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EvaluatorTestRequest {
    /// The saved evaluator to run. Needed unless `evaluator` is given.
    #[serde(default)]
    pub evaluator_id: Option<Uuid>,
    /// The evaluator to run. With `evaluator_id` too, it is merged over the saved one.
    #[serde(default)]
    pub evaluator: Option<TestEvaluatorBody>,
    /// Stored trace to load, 32 hex digits.
    #[serde(default)]
    pub trace_id: Option<String>,
    /// Span of the trace to target, 16 hex digits.
    #[serde(default)]
    pub span_id: Option<String>,
    /// Start of the trace, to narrow the lookup.
    #[serde(default)]
    pub trace_start: Option<TimeParam>,
    /// Prompt variable to mapping expression, for a stored trace.
    #[serde(default)]
    #[schema(value_type = Option<BTreeMap<String, String>>)]
    pub variable_mapping: Option<BTreeMap<String, String>>,
    /// Literal prompt variables, instead of a stored trace. At most 32, 256 KiB in total.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub variables: Option<BTreeMap<String, Value>>,
}

impl fmt::Debug for EvaluatorTestRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluatorTestRequest")
            .field("evaluator_id", &self.evaluator_id)
            .field("has_evaluator", &self.evaluator.is_some())
            .field("has_trace_id", &self.trace_id.is_some())
            .field("has_span_id", &self.span_id.is_some())
            .field("has_trace_start", &self.trace_start.is_some())
            .field(
                "mapping_count",
                &self.variable_mapping.as_ref().map(BTreeMap::len),
            )
            .field(
                "variable_count",
                &self.variables.as_ref().map(BTreeMap::len),
            )
            .finish()
    }
}

/// Why a test run did not produce a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EvaluatorTestError {
    /// What the caller should do with the failure.
    pub class: TestErrorClass,
    /// The error text, without payload content or secrets.
    pub message: String,
}

/// Judge usage of one test run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EvaluatorTestUsage {
    /// Input tokens the provider counted. Omitted when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Output tokens the provider counted. Omitted when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Estimated cost in USD. Omitted when no price is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// `POST /api/v1/projects/{project}/evaluators/test` response. Also used when the evaluator failed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EvaluatorTestResult {
    /// Whether the evaluator produced a value.
    pub ok: bool,
    /// The score value. Omitted unless `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<ScoreValue>,
    /// The judge's explanation, when it gave one. Omitted otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The failure. Omitted when `ok` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<EvaluatorTestError>,
    /// The rendered prompt. Omitted for checks, webhooks and failures before rendering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Each prompt variable as rendered, cut to 2 000 characters. Empty when the run failed before rendering.
    pub variables: BTreeMap<String, String>,
    /// Judge usage. Omitted when the run made no LLM call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<EvaluatorTestUsage>,
    /// Time the run took, in milliseconds.
    pub latency_ms: u64,
    /// Whether the target trace was cut to fit the loading limits. Always false for literal variables.
    pub trace_truncated: bool,
    /// The judge trace the run wrote (environment `llmobs-eval`). Omitted when no judge trace was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judge_trace_id: Option<TraceId>,
}

#[cfg(test)]
mod tests {
    use super::{
        CreateEvaluatorRequest, EvaluatorKind, EvaluatorTestRequest, OutputScoreBody,
        builtin_template_list,
    };
    use llmobs_core::enums::ScoreDataType;
    use serde_json::json;

    #[test]
    fn stored_kinds_round_trip() {
        for kind in [
            EvaluatorKind::LlmJudge,
            EvaluatorKind::Check,
            EvaluatorKind::Webhook,
        ] {
            assert_eq!(EvaluatorKind::from_name(kind.as_str()), Some(kind));
        }
        assert_eq!(EvaluatorKind::from_name("LLM_JUDGE"), None);
        assert_eq!(EvaluatorKind::from_name(""), None);
    }

    #[test]
    fn builtin_templates_keep_their_order() {
        let list = builtin_template_list();
        let ids: Vec<&str> = list
            .data
            .iter()
            .map(|template| template.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "hallucination",
                "relevance",
                "toxicity",
                "correctness",
                "custom"
            ]
        );
        assert!(list.data.iter().all(|template| !template.prompt.is_empty()));
    }

    #[test]
    fn create_request_debug_hides_the_config_and_the_secret() {
        let request: CreateEvaluatorRequest = serde_json::from_value(json!({
            "name": "Hooked",
            "kind": "webhook",
            "config": {"url": "https://hooks.example.com/a?token=very-secret-token"},
            "output_score": {"name": "hook", "data_type": "numeric"},
            "webhook_secret": "sixteen-char-secret-value"
        }))
        .unwrap_or_else(|error| panic!("{error}"));
        let rendered = format!("{request:?}");
        assert!(rendered.contains("has_signing_secret: true"));
        assert!(!rendered.contains("very-secret-token"));
        assert!(!rendered.contains("sixteen-char-secret-value"));
        assert!(!rendered.contains("hooks.example.com"));
    }

    #[test]
    fn test_request_refuses_unknown_fields() {
        let extra = r#"{"evaluator_id": "01900000-0000-7000-8000-000000000500", "extra": 1}"#;
        assert!(serde_json::from_str::<EvaluatorTestRequest>(extra).is_err());
        let known = r#"{"variables": {"input": "hi"}}"#;
        assert!(serde_json::from_str::<EvaluatorTestRequest>(known).is_ok());
    }

    #[test]
    fn output_score_refuses_unknown_fields() {
        let extra = r#"{"name": "relevance", "data_type": "numeric", "min": 0}"#;
        assert!(serde_json::from_str::<OutputScoreBody>(extra).is_err());
        let body = OutputScoreBody {
            name: "relevance".to_owned(),
            data_type: ScoreDataType::Numeric,
            categories: None,
        };
        let text = serde_json::to_string(&body).unwrap_or_else(|error| panic!("{error}"));
        assert!(!text.contains("categories"));
    }
}
