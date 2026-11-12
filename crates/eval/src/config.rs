//! Evaluator configuration, parsed from the JSON shapes in spec 06.
//!
//! [`EvaluatorConfig::from_parts`] reads the `evaluators.config` document for a
//! known kind and checks that document's own rules. [`Evaluator::validate`]
//! adds the evaluator name and the agreement between the config and
//! `output_score`.

use std::collections::BTreeMap;

use llmobs_core::enums::ScoreDataType;
use serde::Deserialize;
use serde_json::Value;
use uuid::Uuid;

use crate::error::ConfigError;

/// Largest prompt, in UTF-8 bytes. Spec 06 and D163's template limit.
const PROMPT_MAX_BYTES: usize = 64 * 1024;
const NAME_MAX_CHARS: usize = 128;
const MAX_OUTPUT_TOKENS_DEFAULT: u32 = 1024;
const MAX_OUTPUT_TOKENS_MAX: u32 = 65_536;
const JUDGE_TIMEOUT_DEFAULT_MS: u32 = 60_000;
const JUDGE_TIMEOUT_MIN_MS: u32 = 1_000;
const JUDGE_TIMEOUT_MAX_MS: u32 = 300_000;
const WEBHOOK_TIMEOUT_DEFAULT_MS: u32 = 10_000;
const WEBHOOK_TIMEOUT_MIN_MS: u32 = 1_000;
const WEBHOOK_TIMEOUT_MAX_MS: u32 = 60_000;
const LABEL_MIN: usize = 2;
const LABEL_MAX: usize = 20;
const LABEL_CHARS_MAX: usize = 64;
const HEADER_MAX: usize = 20;

const RESERVED_HEADERS: [&str; 4] = [
    "content-type",
    "content-length",
    "host",
    "x-llmobs-signature",
];

const fn default_true() -> bool {
    true
}

fn default_max_output_tokens() -> u32 {
    MAX_OUTPUT_TOKENS_DEFAULT
}

fn default_judge_timeout() -> u32 {
    JUDGE_TIMEOUT_DEFAULT_MS
}

fn default_webhook_timeout() -> u32 {
    WEBHOOK_TIMEOUT_DEFAULT_MS
}

/// How a `contains` check combines its needles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainsMode {
    /// At least one needle occurs.
    #[default]
    Any,
    /// Every needle occurs.
    All,
}

/// One evaluator: its kind-specific config and the score it writes.
#[derive(Debug, Clone, PartialEq)]
pub struct Evaluator {
    /// `evaluators.id`.
    pub id: Uuid,
    /// `evaluators.name`. One to 128 Unicode scalar values.
    pub name: String,
    /// Parsed `evaluators.config`.
    pub config: EvaluatorConfig,
    /// Parsed `evaluators.output_score`.
    pub output: OutputScore,
}

impl Evaluator {
    /// Checks the config, the name, and that `output` agrees with the config.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError`] when a field breaks a rule or the output data
    /// type does not match the config.
    pub fn validate(&self) -> Result<(), ConfigError> {
        self.config.validate_parts()?;
        validate_name(&self.name)?;
        self.validate_output()
    }

    fn validate_output(&self) -> Result<(), ConfigError> {
        match &self.config {
            EvaluatorConfig::LlmJudge(config) => match &config.output {
                JudgeOutput::Numeric { .. } => {
                    expect_type(ScoreDataType::Numeric, self.output.data_type)
                }
                JudgeOutput::Boolean => expect_type(ScoreDataType::Boolean, self.output.data_type),
                JudgeOutput::Categorical { labels, .. } => {
                    expect_type(ScoreDataType::Categorical, self.output.data_type)?;
                    match &self.output.categories {
                        Some(categories) if categories == labels => Ok(()),
                        _ => Err(ConfigError::Invalid {
                            field: "categories",
                            reason: "must equal labels",
                        }),
                    }
                }
            },
            EvaluatorConfig::Check(check) => {
                let expected = if matches!(check, Check::Levenshtein { .. }) {
                    ScoreDataType::Numeric
                } else {
                    ScoreDataType::Boolean
                };
                expect_type(expected, self.output.data_type)
            }
            EvaluatorConfig::Webhook(_) => match self.output.data_type {
                ScoreDataType::Numeric | ScoreDataType::Boolean | ScoreDataType::Categorical => {
                    Ok(())
                }
                ScoreDataType::Text => Err(ConfigError::Invalid {
                    field: "data_type",
                    reason: "must be numeric, boolean, or categorical",
                }),
            },
        }
    }
}

fn expect_type(expected: ScoreDataType, found: ScoreDataType) -> Result<(), ConfigError> {
    if expected == found {
        Ok(())
    } else {
        Err(ConfigError::OutputMismatch { expected, found })
    }
}

fn validate_name(name: &str) -> Result<(), ConfigError> {
    let chars = name.chars().count();
    if (1..=NAME_MAX_CHARS).contains(&chars) {
        Ok(())
    } else {
        Err(ConfigError::Invalid {
            field: "name",
            reason: "must be 1 to 128 characters",
        })
    }
}

/// Config for one `evaluators.kind`. Not itself a serde enum: the kind is a column.
#[derive(Debug, Clone, PartialEq)]
pub enum EvaluatorConfig {
    /// `kind = "llm_judge"`.
    LlmJudge(JudgeConfig),
    /// `kind = "check"`.
    Check(Check),
    /// `kind = "webhook"`.
    Webhook(WebhookConfig),
}

impl EvaluatorConfig {
    /// Parses `config` for `kind` and applies that kind's field rules.
    ///
    /// A `contains` check that sets `value` is stored with that string appended
    /// to `values` and `value` cleared, so later code reads `values` only.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::UnknownKind`] for an unknown kind,
    /// [`ConfigError::Json`] when the document does not match the struct, and
    /// [`ConfigError::Invalid`] when a field rule fails.
    pub fn from_parts(kind: &str, config: &Value) -> Result<Self, ConfigError> {
        match kind {
            "llm_judge" => {
                let config = parse::<JudgeConfig>("llm_judge", config)?;
                config.validate_parts()?;
                Ok(Self::LlmJudge(config))
            }
            "check" => {
                let config = parse::<Check>("check", config)?;
                Ok(Self::Check(config.into_stored()?))
            }
            "webhook" => {
                let config = parse::<WebhookConfig>("webhook", config)?;
                config.validate_parts()?;
                Ok(Self::Webhook(config))
            }
            _ => Err(ConfigError::UnknownKind {
                kind: kind.to_owned(),
            }),
        }
    }

    /// The `evaluators.kind` column for this config.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::LlmJudge(_) => "llm_judge",
            Self::Check(_) => "check",
            Self::Webhook(_) => "webhook",
        }
    }

    fn validate_parts(&self) -> Result<(), ConfigError> {
        match self {
            Self::LlmJudge(config) => config.validate_parts(),
            Self::Check(config) => config.validate_parts(),
            Self::Webhook(config) => config.validate_parts(),
        }
    }
}

fn parse<T: for<'de> Deserialize<'de>>(
    kind: &'static str,
    config: &Value,
) -> Result<T, ConfigError> {
    T::deserialize(config).map_err(|err| ConfigError::from_json(kind, &err))
}

/// LLM-judge configuration. Unknown JSON fields are rejected.
#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeConfig {
    /// Connection row that supplies the adapter and the decrypted API key.
    pub connection_id: Uuid,
    /// Provider model id. Non-empty.
    pub model: String,
    /// Prompt template. Non-empty and at most 64 KiB of UTF-8.
    ///
    /// Template syntax is checked when the template is compiled.
    pub prompt: String,
    /// Score the model must produce.
    pub output: JudgeOutput,
    /// Ask the model for an explanation. Defaults to `true`.
    #[serde(default = "default_true")]
    pub include_reason: bool,
    /// Sampling temperature in `0..=2`. Omitted means unset.
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Completion cap. Defaults to 1024, allowed 1..=65_536.
    #[serde(default = "default_max_output_tokens")]
    pub max_output_tokens: u32,
    /// Whole-call timeout in milliseconds. Defaults to 60_000, allowed 1_000..=300_000.
    #[serde(default = "default_judge_timeout")]
    pub timeout_ms: u32,
}

impl JudgeConfig {
    fn validate_parts(&self) -> Result<(), ConfigError> {
        if self.model.is_empty() {
            return Err(ConfigError::Invalid {
                field: "model",
                reason: "must be non-empty",
            });
        }
        if self.prompt.is_empty() {
            return Err(ConfigError::Invalid {
                field: "prompt",
                reason: "must be non-empty",
            });
        }
        if self.prompt.len() > PROMPT_MAX_BYTES {
            return Err(ConfigError::Invalid {
                field: "prompt",
                reason: "must be at most 64 KiB",
            });
        }
        if let Some(temperature) = self.temperature
            && (!temperature.is_finite() || !(0.0..=2.0).contains(&temperature))
        {
            return Err(ConfigError::Invalid {
                field: "temperature",
                reason: "must be between 0 and 2",
            });
        }
        if !(1..=MAX_OUTPUT_TOKENS_MAX).contains(&self.max_output_tokens) {
            return Err(ConfigError::Invalid {
                field: "max_output_tokens",
                reason: "must be between 1 and 65536",
            });
        }
        if !(JUDGE_TIMEOUT_MIN_MS..=JUDGE_TIMEOUT_MAX_MS).contains(&self.timeout_ms) {
            return Err(ConfigError::Invalid {
                field: "timeout_ms",
                reason: "must be between 1000 and 300000",
            });
        }
        self.output.validate_parts()
    }
}

/// What the judge model returns. Tagged by `type`.
#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum JudgeOutput {
    /// A finite number with `min < max`.
    Numeric {
        /// Inclusive lower bound. Finite.
        min: f64,
        /// Inclusive upper bound. Finite and greater than `min`.
        max: f64,
    },
    /// True or false.
    Boolean,
    /// One label from a closed list.
    Categorical {
        /// Two to twenty labels, each 1..=64 characters, unique after trim and case-folding.
        labels: Vec<String>,
        /// Optional numeric value per label. Keys must be labels as stored. Defaults to empty.
        #[serde(default)]
        label_values: BTreeMap<String, f64>,
    },
}

impl JudgeOutput {
    fn validate_parts(&self) -> Result<(), ConfigError> {
        match self {
            Self::Numeric { min, max } => {
                if !min.is_finite() || !max.is_finite() {
                    return Err(ConfigError::Invalid {
                        field: "output",
                        reason: "bounds must be finite",
                    });
                }
                if min >= max {
                    return Err(ConfigError::Invalid {
                        field: "output",
                        reason: "min must be less than max",
                    });
                }
                Ok(())
            }
            Self::Boolean => Ok(()),
            Self::Categorical {
                labels,
                label_values,
            } => validate_labels(labels, label_values),
        }
    }
}

fn validate_labels(
    labels: &[String],
    label_values: &BTreeMap<String, f64>,
) -> Result<(), ConfigError> {
    if !(LABEL_MIN..=LABEL_MAX).contains(&labels.len()) {
        return Err(ConfigError::Invalid {
            field: "labels",
            reason: "must have between 2 and 20 labels",
        });
    }
    let mut seen = BTreeMap::<String, ()>::new();
    for label in labels {
        let chars = label.chars().count();
        if !(1..=LABEL_CHARS_MAX).contains(&chars) {
            return Err(ConfigError::Invalid {
                field: "labels",
                reason: "must be 1 to 64 characters",
            });
        }
        let folded = fold_label(label);
        if folded.is_empty() {
            return Err(ConfigError::Invalid {
                field: "labels",
                reason: "must not be blank",
            });
        }
        if seen.insert(folded, ()).is_some() {
            return Err(ConfigError::Invalid {
                field: "labels",
                reason: "must be unique",
            });
        }
    }
    for (key, value) in label_values {
        if !value.is_finite() {
            return Err(ConfigError::Invalid {
                field: "label_values",
                reason: "must be finite",
            });
        }
        if !labels.iter().any(|label| label == key) {
            return Err(ConfigError::Invalid {
                field: "label_values",
                reason: "key is not a label",
            });
        }
    }
    Ok(())
}

fn fold_label(label: &str) -> String {
    label.trim().to_lowercase()
}

/// Built-in check configuration. Tagged by `check`. Unknown JSON fields are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(tag = "check", rename_all = "snake_case", deny_unknown_fields)]
pub enum Check {
    /// Compare the output text with the expected text.
    ExactMatch {
        /// Defaults to `true`.
        #[serde(default = "default_true")]
        case_sensitive: bool,
        /// Trim both sides before comparing. Defaults to `true`.
        #[serde(default = "default_true")]
        trim: bool,
    },
    /// Look for needles in the output text.
    ///
    /// Set `value` or a non-empty `values`, not both. `from_parts` appends
    /// `value` to `values` and clears `value`. When both are absent, the check
    /// compares the output with `expected`.
    Contains {
        /// Single needle. Folded into `values` by [`EvaluatorConfig::from_parts`].
        #[serde(default)]
        value: Option<String>,
        /// Needles. Empty, with `value` absent, means compare with `expected`.
        #[serde(default)]
        values: Vec<String>,
        /// Defaults to [`ContainsMode::Any`].
        #[serde(default)]
        mode: ContainsMode,
        /// Defaults to `false`.
        #[serde(default)]
        case_sensitive: bool,
    },
    /// Rust regex searched anywhere in the output.
    ///
    /// [`crate::check::compile_regex`] compiles it here and again when the check runs.
    Regex {
        /// Pattern stored as written.
        pattern: String,
    },
    /// The output text parses as JSON.
    JsonValid,
    /// The output validates against a JSON Schema.
    ///
    /// [`crate::check::compile_schema`] compiles it here and again when the check runs.
    /// External `$ref`s are rejected; the compiler does not fetch them.
    JsonSchema {
        /// Schema document.
        schema: Value,
    },
    /// Normalized Levenshtein similarity.
    Levenshtein {
        /// Trim, case-fold, and collapse whitespace before the distance. Defaults to `true`.
        #[serde(default = "default_true")]
        normalize: bool,
    },
}

impl Check {
    fn into_stored(mut self) -> Result<Self, ConfigError> {
        self.validate_parts()?;
        if let Self::Contains { value, values, .. } = &mut self
            && let Some(needle) = value.take()
        {
            values.push(needle);
        }
        Ok(self)
    }

    fn validate_parts(&self) -> Result<(), ConfigError> {
        match self {
            Self::Contains { value, values, .. } => {
                if value.is_some() && !values.is_empty() {
                    return Err(ConfigError::Invalid {
                        field: "values",
                        reason: "set value or values, not both",
                    });
                }
                let empty_value = value.as_ref().is_some_and(String::is_empty);
                if empty_value || values.iter().any(String::is_empty) {
                    return Err(ConfigError::Invalid {
                        field: "values",
                        reason: "must not be empty",
                    });
                }
                Ok(())
            }
            Self::Regex { pattern } => {
                if crate::check::compile_regex(pattern).is_err() {
                    return Err(ConfigError::Invalid {
                        field: "pattern",
                        reason: "invalid regular expression",
                    });
                }
                Ok(())
            }
            Self::JsonSchema { schema } => {
                if crate::check::compile_schema(schema).is_err() {
                    return Err(ConfigError::Invalid {
                        field: "schema",
                        reason: "invalid JSON schema",
                    });
                }
                Ok(())
            }
            Self::ExactMatch { .. } | Self::JsonValid | Self::Levenshtein { .. } => Ok(()),
        }
    }
}

/// Webhook configuration.
///
/// Unknown JSON fields are ignored so the server can keep bookkeeping keys in
/// the same document. Judge and check configs reject unknown fields.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
pub struct WebhookConfig {
    /// Absolute `http` or `https` URL with a host and no userinfo.
    pub url: String,
    /// Timeout in milliseconds. Defaults to 10_000, allowed 1_000..=60_000.
    #[serde(default = "default_webhook_timeout")]
    pub timeout_ms: u32,
    /// Extra request headers. At most 20. Names are HTTP tokens and must not
    /// be `content-type`, `content-length`, `host`, or `x-llmobs-signature`.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Opaque ciphertext stored by the server.
    ///
    /// This crate does not decrypt or read the value. The server supplies the
    /// decrypted signing secret separately when the webhook runs.
    #[serde(default)]
    pub secret_enc: Option<String>,
}

impl WebhookConfig {
    fn validate_parts(&self) -> Result<(), ConfigError> {
        validate_webhook_url(&self.url)?;
        if !(WEBHOOK_TIMEOUT_MIN_MS..=WEBHOOK_TIMEOUT_MAX_MS).contains(&self.timeout_ms) {
            return Err(ConfigError::Invalid {
                field: "timeout_ms",
                reason: "must be between 1000 and 60000",
            });
        }
        validate_headers(&self.headers)
    }
}

fn validate_webhook_url(url: &str) -> Result<(), ConfigError> {
    if url.is_empty() || url.chars().any(char::is_whitespace) {
        return Err(invalid_url());
    }
    let Some((scheme, rest)) = url.split_once("://") else {
        return Err(invalid_url());
    };
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return Err(invalid_url());
    }
    // `split` yields at least one piece, so an empty remainder is `Some("")`.
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() {
        return Err(missing_host());
    }
    if authority.contains('@') {
        return Err(ConfigError::Invalid {
            field: "url",
            reason: "must not include userinfo",
        });
    }
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        let Some((host, after)) = bracketed.split_once(']') else {
            return Err(missing_host());
        };
        if !after.is_empty() && !after.starts_with(':') {
            return Err(missing_host());
        }
        host
    } else {
        authority
            .split_once(':')
            .map_or(authority, |(host, _)| host)
    };
    if host.is_empty() {
        return Err(missing_host());
    }
    Ok(())
}

fn invalid_url() -> ConfigError {
    ConfigError::Invalid {
        field: "url",
        reason: "must be an absolute http or https URL",
    }
}

fn missing_host() -> ConfigError {
    ConfigError::Invalid {
        field: "url",
        reason: "must include a host",
    }
}

fn validate_headers(headers: &BTreeMap<String, String>) -> Result<(), ConfigError> {
    if headers.len() > HEADER_MAX {
        return Err(ConfigError::Invalid {
            field: "headers",
            reason: "at most 20 headers",
        });
    }
    for name in headers.keys() {
        if !is_http_token(name) {
            return Err(ConfigError::Invalid {
                field: "headers",
                reason: "name must be an HTTP token",
            });
        }
        if RESERVED_HEADERS
            .iter()
            .any(|reserved| name.eq_ignore_ascii_case(reserved))
        {
            return Err(ConfigError::Invalid {
                field: "headers",
                reason: "name is reserved",
            });
        }
    }
    Ok(())
}

/// `tchar` from RFC 9110.
fn is_http_token(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(|byte| {
            matches!(
                byte,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'*'
                    | b'+'
                    | b'-'
                    | b'.'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'|'
                    | b'~'
                    | b'0'..=b'9'
                    | b'A'..=b'Z'
                    | b'a'..=b'z'
            )
        })
}

/// `evaluators.output_score`: the score this evaluator writes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct OutputScore {
    /// Score name.
    pub name: String,
    /// Score data type.
    pub data_type: ScoreDataType,
    /// Category labels. Required for a categorical judge, where they must equal
    /// [`JudgeOutput::Categorical`]'s `labels` in the same order.
    #[serde(default)]
    pub categories: Option<Vec<String>>,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{
        Check, ConfigError, ContainsMode, Evaluator, EvaluatorConfig, JudgeConfig, JudgeOutput,
        OutputScore, PROMPT_MAX_BYTES,
    };
    use crate::error::assert_no_payload;
    use llmobs_core::enums::ScoreDataType;
    use serde_json::{Value, json};
    use uuid::Uuid;

    const ID: &str = "00000000-0000-4000-8000-000000000001";

    fn connection() -> Uuid {
        Uuid::parse_str(ID).expect("uuid")
    }

    fn boolean_judge() -> Value {
        json!({
            "connection_id": ID,
            "model": "m",
            "prompt": "p",
            "output": {"type": "boolean"}
        })
    }

    fn assert_invalid(kind: &str, value: &Value, field: &'static str, reason: &'static str) {
        let err = EvaluatorConfig::from_parts(kind, value).expect_err("config is invalid");
        assert_eq!(err, ConfigError::Invalid { field, reason });
    }

    fn evaluator(config: EvaluatorConfig, output: OutputScore) -> Evaluator {
        Evaluator {
            id: Uuid::nil(),
            name: "grader".to_owned(),
            config,
            output,
        }
    }

    fn score(data_type: ScoreDataType, categories: Option<Vec<&str>>) -> OutputScore {
        OutputScore {
            name: "score".to_owned(),
            data_type,
            categories: categories.map(|labels| labels.into_iter().map(str::to_owned).collect()),
        }
    }

    #[test]
    fn parses_the_spec_judge_examples() {
        let numeric = json!({
            "connection_id": ID,
            "model": "gpt-5-mini",
            "prompt": "You are grading … Question: {{ input }}\nAnswer: {{ output }}\n…",
            "output": {"type": "numeric", "min": 0, "max": 1},
            "include_reason": true,
            "max_output_tokens": 1024,
            "timeout_ms": 60000
        });
        let config = EvaluatorConfig::from_parts("llm_judge", &numeric).expect("numeric judge");
        assert_eq!(config.kind(), "llm_judge");
        let EvaluatorConfig::LlmJudge(judge) = config else {
            panic!("llm judge");
        };
        assert_eq!(judge.connection_id, connection());
        assert_eq!(judge.model, "gpt-5-mini");
        assert!(judge.include_reason);
        assert_eq!(judge.temperature, None);
        assert_eq!(judge.max_output_tokens, 1024);
        assert_eq!(judge.timeout_ms, 60_000);
        assert_eq!(judge.output, JudgeOutput::Numeric { min: 0.0, max: 1.0 });

        let boolean = json!({
            "connection_id": ID,
            "model": "gpt-5-mini",
            "prompt": "grade",
            "output": {"type": "boolean"}
        });
        let EvaluatorConfig::LlmJudge(judge) =
            EvaluatorConfig::from_parts("llm_judge", &boolean).expect("boolean judge")
        else {
            panic!("llm judge");
        };
        assert_eq!(judge.output, JudgeOutput::Boolean);

        let categorical = json!({
            "connection_id": ID,
            "model": "gpt-5-mini",
            "prompt": "grade",
            "output": {
                "type": "categorical",
                "labels": ["correct", "partially_correct", "incorrect"],
                "label_values": {"correct": 1, "partially_correct": 0.5, "incorrect": 0}
            }
        });
        let EvaluatorConfig::LlmJudge(judge) =
            EvaluatorConfig::from_parts("llm_judge", &categorical).expect("categorical judge")
        else {
            panic!("llm judge");
        };
        let JudgeOutput::Categorical {
            labels,
            label_values,
        } = judge.output
        else {
            panic!("categorical");
        };
        assert_eq!(labels, ["correct", "partially_correct", "incorrect"]);
        assert_eq!(label_values["correct"], 1.0);
        assert_eq!(label_values["partially_correct"], 0.5);
        assert_eq!(label_values["incorrect"], 0.0);
    }

    #[test]
    fn parses_each_check_and_the_webhook_example() {
        let cases = [
            json!({"check": "exact_match"}),
            json!({"check": "contains", "values": ["yes"], "mode": "all", "case_sensitive": true}),
            json!({"check": "regex", "pattern": "a+"}),
            json!({"check": "json_valid"}),
            json!({"check": "json_schema", "schema": {"type": "object"}}),
            json!({"check": "levenshtein"}),
        ];
        for value in cases {
            let config = EvaluatorConfig::from_parts("check", &value).expect("check");
            assert_eq!(config.kind(), "check");
        }

        let webhook = json!({
            "url": "https://example.com/hook",
            "secret_enc": "opaque-ciphertext",
            "timeout_ms": 10000,
            "headers": {"X-Custom": "1"}
        });
        let config = EvaluatorConfig::from_parts("webhook", &webhook).expect("webhook");
        assert_eq!(config.kind(), "webhook");
        let EvaluatorConfig::Webhook(config) = config else {
            panic!("webhook");
        };
        assert_eq!(config.url, "https://example.com/hook");
        assert_eq!(config.secret_enc.as_deref(), Some("opaque-ciphertext"));
        assert_eq!(config.timeout_ms, 10_000);
        assert_eq!(config.headers["X-Custom"], "1");
    }

    #[test]
    fn defaults_match_the_spec() {
        let EvaluatorConfig::LlmJudge(judge) =
            EvaluatorConfig::from_parts("llm_judge", &boolean_judge()).expect("judge")
        else {
            panic!("judge");
        };
        assert!(judge.include_reason);
        assert_eq!(judge.temperature, None);
        assert_eq!(judge.max_output_tokens, 1024);
        assert_eq!(judge.timeout_ms, 60_000);

        let temperature_null = json!({
            "connection_id": ID,
            "model": "m",
            "prompt": "p",
            "output": {"type": "boolean"},
            "temperature": null
        });
        let EvaluatorConfig::LlmJudge(judge) =
            EvaluatorConfig::from_parts("llm_judge", &temperature_null).expect("null temperature")
        else {
            panic!("judge");
        };
        assert_eq!(judge.temperature, None);

        let EvaluatorConfig::Check(Check::ExactMatch {
            case_sensitive,
            trim,
        }) = EvaluatorConfig::from_parts("check", &json!({"check": "exact_match"})).expect("exact")
        else {
            panic!("exact");
        };
        assert!(case_sensitive);
        assert!(trim);

        let EvaluatorConfig::Check(Check::Contains {
            value,
            values,
            mode,
            case_sensitive,
        }) = EvaluatorConfig::from_parts("check", &json!({"check": "contains"})).expect("contains")
        else {
            panic!("contains");
        };
        assert_eq!(value, None);
        assert!(values.is_empty());
        assert_eq!(mode, ContainsMode::Any);
        assert!(!case_sensitive);

        let EvaluatorConfig::Check(Check::Levenshtein { normalize }) =
            EvaluatorConfig::from_parts("check", &json!({"check": "levenshtein"})).expect("lev")
        else {
            panic!("levenshtein");
        };
        assert!(normalize);

        let EvaluatorConfig::Webhook(config) =
            EvaluatorConfig::from_parts("webhook", &json!({"url": "https://example.com/hook"}))
                .expect("webhook")
        else {
            panic!("webhook");
        };
        assert_eq!(config.timeout_ms, 10_000);
        assert!(config.headers.is_empty());
        assert_eq!(config.secret_enc, None);
    }

    #[test]
    fn folds_contains_value_into_values() {
        let EvaluatorConfig::Check(Check::Contains { value, values, .. }) =
            EvaluatorConfig::from_parts("check", &json!({"check": "contains", "value": "hello"}))
                .expect("contains")
        else {
            panic!("contains");
        };
        assert_eq!(value, None);
        assert_eq!(values, ["hello"]);
    }

    #[test]
    fn rejects_unknown_kind_and_unknown_fields() {
        let err = EvaluatorConfig::from_parts("nope", &json!({})).expect_err("kind");
        assert_eq!(
            err,
            ConfigError::UnknownKind {
                kind: "nope".to_owned()
            }
        );

        let mut judge = boolean_judge();
        judge["prompt"] = json!("SECRET-PAYLOAD");
        judge["extra"] = json!(true);
        let err = EvaluatorConfig::from_parts("llm_judge", &judge).expect_err("judge field");
        assert!(matches!(
            err,
            ConfigError::Json {
                kind: "llm_judge",
                ..
            }
        ));
        assert_no_payload(&err.to_string(), "SECRET-PAYLOAD");

        let err = EvaluatorConfig::from_parts(
            "check",
            &json!({"check": "exact_match", "extra": "SECRET-PAYLOAD"}),
        )
        .expect_err("check field");
        assert!(matches!(err, ConfigError::Json { kind: "check", .. }));
        assert_no_payload(&err.to_string(), "SECRET-PAYLOAD");

        let webhook = json!({
            "url": "https://example.com/hook",
            "bookkeeping": "SECRET-PAYLOAD"
        });
        let EvaluatorConfig::Webhook(config) =
            EvaluatorConfig::from_parts("webhook", &webhook).expect("bookkeeping key")
        else {
            panic!("webhook");
        };
        assert_eq!(config.url, "https://example.com/hook");
    }

    #[test]
    fn json_type_error_does_not_quote_the_prompt() {
        let value = json!({
            "connection_id": ID,
            "model": "m",
            "prompt": "SECRET-PAYLOAD",
            "output": {"type": "boolean"},
            "max_output_tokens": "SECRET-PAYLOAD"
        });
        let err = EvaluatorConfig::from_parts("llm_judge", &value).expect_err("type");
        let ConfigError::Json { kind, message } = &err else {
            panic!("json error");
        };
        assert_eq!(*kind, "llm_judge");
        assert!(message.starts_with("data at line "), "{message}");
        assert_no_payload(message, "SECRET-PAYLOAD");
        assert_no_payload(&err.to_string(), "SECRET-PAYLOAD");
    }

    #[test]
    fn rejects_invalid_judge_rules() {
        let mut empty_model = boolean_judge();
        empty_model["model"] = json!("");
        assert_invalid("llm_judge", &empty_model, "model", "must be non-empty");

        let mut empty_prompt = boolean_judge();
        empty_prompt["prompt"] = json!("");
        assert_invalid("llm_judge", &empty_prompt, "prompt", "must be non-empty");

        let mut big = boolean_judge();
        big["prompt"] = Value::String("a".repeat(PROMPT_MAX_BYTES + 1));
        assert_invalid("llm_judge", &big, "prompt", "must be at most 64 KiB");

        let mut multibyte = boolean_judge();
        multibyte["prompt"] = Value::String("é".repeat(40_000));
        assert_invalid("llm_judge", &multibyte, "prompt", "must be at most 64 KiB");

        let mut at_limit = boolean_judge();
        at_limit["prompt"] = Value::String("a".repeat(PROMPT_MAX_BYTES));
        EvaluatorConfig::from_parts("llm_judge", &at_limit).expect("64 KiB prompt");

        for (temperature, ok) in [(-0.1, false), (0.0, true), (2.0, true), (2.1, false)] {
            let mut value = boolean_judge();
            value["temperature"] = json!(temperature);
            if ok {
                EvaluatorConfig::from_parts("llm_judge", &value).expect("temperature");
            } else {
                assert_invalid(
                    "llm_judge",
                    &value,
                    "temperature",
                    "must be between 0 and 2",
                );
            }
        }

        for (tokens, ok) in [(0, false), (1, true), (65_536, true), (65_537, false)] {
            let mut value = boolean_judge();
            value["max_output_tokens"] = json!(tokens);
            if ok {
                EvaluatorConfig::from_parts("llm_judge", &value).expect("tokens");
            } else {
                assert_invalid(
                    "llm_judge",
                    &value,
                    "max_output_tokens",
                    "must be between 1 and 65536",
                );
            }
        }

        for (timeout, ok) in [
            (999, false),
            (1_000, true),
            (300_000, true),
            (300_001, false),
        ] {
            let mut value = boolean_judge();
            value["timeout_ms"] = json!(timeout);
            if ok {
                EvaluatorConfig::from_parts("llm_judge", &value).expect("timeout");
            } else {
                assert_invalid(
                    "llm_judge",
                    &value,
                    "timeout_ms",
                    "must be between 1000 and 300000",
                );
            }
        }

        for (min, max) in [(1.0, 1.0), (2.0, 1.0)] {
            let value = json!({
                "connection_id": ID,
                "model": "m",
                "prompt": "p",
                "output": {"type": "numeric", "min": min, "max": max}
            });
            assert_invalid("llm_judge", &value, "output", "min must be less than max");
        }

        let one_label = json!({
            "connection_id": ID,
            "model": "m",
            "prompt": "p",
            "output": {"type": "categorical", "labels": ["only"]}
        });
        assert_invalid(
            "llm_judge",
            &one_label,
            "labels",
            "must have between 2 and 20 labels",
        );

        let mut labels: Vec<String> = (0..21).map(|n| n.to_string()).collect();
        let too_many = json!({
            "connection_id": ID,
            "model": "m",
            "prompt": "p",
            "output": {"type": "categorical", "labels": labels}
        });
        assert_invalid(
            "llm_judge",
            &too_many,
            "labels",
            "must have between 2 and 20 labels",
        );
        labels.pop();
        let twenty = json!({
            "connection_id": ID,
            "model": "m",
            "prompt": "p",
            "output": {"type": "categorical", "labels": labels}
        });
        EvaluatorConfig::from_parts("llm_judge", &twenty).expect("20 labels");

        let long_label = json!({
            "connection_id": ID,
            "model": "m",
            "prompt": "p",
            "output": {"type": "categorical", "labels": ["a".repeat(65), "b"]}
        });
        assert_invalid(
            "llm_judge",
            &long_label,
            "labels",
            "must be 1 to 64 characters",
        );

        let blank = json!({
            "connection_id": ID,
            "model": "m",
            "prompt": "p",
            "output": {"type": "categorical", "labels": ["  ", "b"]}
        });
        assert_invalid("llm_judge", &blank, "labels", "must not be blank");

        let folded = json!({
            "connection_id": ID,
            "model": "m",
            "prompt": "p",
            "output": {"type": "categorical", "labels": ["OK", " ok "]}
        });
        assert_invalid("llm_judge", &folded, "labels", "must be unique");

        let unknown_key = json!({
            "connection_id": ID,
            "model": "m",
            "prompt": "p",
            "output": {
                "type": "categorical",
                "labels": ["a", "b"],
                "label_values": {"c": 1}
            }
        });
        assert_invalid(
            "llm_judge",
            &unknown_key,
            "label_values",
            "key is not a label",
        );
    }

    #[test]
    fn rejects_non_finite_numbers_on_a_built_config() {
        let mut judge = JudgeConfig {
            connection_id: Uuid::nil(),
            model: "m".to_owned(),
            prompt: "p".to_owned(),
            output: JudgeOutput::Boolean,
            include_reason: true,
            temperature: Some(f64::NAN),
            max_output_tokens: 1024,
            timeout_ms: 60_000,
        };
        let err = evaluator(
            EvaluatorConfig::LlmJudge(judge.clone()),
            score(ScoreDataType::Boolean, None),
        )
        .validate()
        .expect_err("nan temperature");
        assert_eq!(
            err,
            ConfigError::Invalid {
                field: "temperature",
                reason: "must be between 0 and 2",
            }
        );

        judge.temperature = None;
        judge.output = JudgeOutput::Numeric {
            min: f64::INFINITY,
            max: 1.0,
        };
        let err = evaluator(
            EvaluatorConfig::LlmJudge(judge.clone()),
            score(ScoreDataType::Numeric, None),
        )
        .validate()
        .expect_err("infinite bound");
        assert_eq!(
            err,
            ConfigError::Invalid {
                field: "output",
                reason: "bounds must be finite",
            }
        );

        let mut values = BTreeMap::new();
        values.insert("a".to_owned(), f64::NAN);
        judge.output = JudgeOutput::Categorical {
            labels: vec!["a".to_owned(), "b".to_owned()],
            label_values: values,
        };
        let err = evaluator(
            EvaluatorConfig::LlmJudge(judge),
            score(ScoreDataType::Categorical, Some(vec!["a", "b"])),
        )
        .validate()
        .expect_err("nan label value");
        assert_eq!(
            err,
            ConfigError::Invalid {
                field: "label_values",
                reason: "must be finite",
            }
        );
    }

    #[test]
    fn rejects_invalid_checks() {
        assert_invalid(
            "check",
            &json!({"check": "contains", "value": "a", "values": ["b"]}),
            "values",
            "set value or values, not both",
        );
        assert_invalid(
            "check",
            &json!({"check": "contains", "value": ""}),
            "values",
            "must not be empty",
        );
        assert_invalid(
            "check",
            &json!({"check": "contains", "values": ["ok", ""]}),
            "values",
            "must not be empty",
        );

        let explicit = json!({
            "check": "exact_match",
            "case_sensitive": false,
            "trim": false
        });
        let EvaluatorConfig::Check(Check::ExactMatch {
            case_sensitive,
            trim,
        }) = EvaluatorConfig::from_parts("check", &explicit).expect("flags")
        else {
            panic!("exact");
        };
        assert!(!case_sensitive);
        assert!(!trim);

        let EvaluatorConfig::Check(Check::Levenshtein { normalize }) = EvaluatorConfig::from_parts(
            "check",
            &json!({"check": "levenshtein", "normalize": false}),
        )
        .expect("normalize") else {
            panic!("levenshtein");
        };
        assert!(!normalize);
    }

    #[test]
    fn rejects_invalid_webhooks_without_quoting_the_url() {
        assert_invalid(
            "webhook",
            &json!({"url": "ftp://example.com/hook?token=SECRET-PAYLOAD"}),
            "url",
            "must be an absolute http or https URL",
        );
        let err = EvaluatorConfig::from_parts(
            "webhook",
            &json!({"url": "ftp://example.com/hook?token=SECRET-PAYLOAD"}),
        )
        .expect_err("scheme");
        assert_no_payload(&err.to_string(), "SECRET-PAYLOAD");

        assert_invalid(
            "webhook",
            &json!({"url": "https://user:pass@example.com/hook"}),
            "url",
            "must not include userinfo",
        );
        assert_invalid(
            "webhook",
            &json!({"url": "https://"}),
            "url",
            "must include a host",
        );
        assert_invalid(
            "webhook",
            &json!({"url": "https://example.com/a b"}),
            "url",
            "must be an absolute http or https URL",
        );
        assert_invalid(
            "webhook",
            &json!({"url": "https://:443/"}),
            "url",
            "must include a host",
        );

        for url in [
            "https://example.com/hook",
            "HTTP://example.com/hook",
            "http://[::1]/hook",
            "https://example.com:8443/hook",
        ] {
            EvaluatorConfig::from_parts("webhook", &json!({"url": url})).expect(url);
        }

        for (timeout, ok) in [(999, false), (1_000, true), (60_000, true), (60_001, false)] {
            let value = json!({"url": "https://example.com/hook", "timeout_ms": timeout});
            if ok {
                EvaluatorConfig::from_parts("webhook", &value).expect("timeout");
            } else {
                assert_invalid(
                    "webhook",
                    &value,
                    "timeout_ms",
                    "must be between 1000 and 60000",
                );
            }
        }

        let mut headers = BTreeMap::new();
        for index in 0..21 {
            headers.insert(format!("X-H{index}"), "1".to_owned());
        }
        assert_invalid(
            "webhook",
            &json!({"url": "https://example.com/hook", "headers": headers}),
            "headers",
            "at most 20 headers",
        );
        headers.pop_last();
        EvaluatorConfig::from_parts(
            "webhook",
            &json!({"url": "https://example.com/hook", "headers": headers}),
        )
        .expect("20 headers");

        assert_invalid(
            "webhook",
            &json!({"url": "https://example.com/hook", "headers": {"bad name": "SECRET-PAYLOAD"}}),
            "headers",
            "name must be an HTTP token",
        );
        let err = EvaluatorConfig::from_parts(
            "webhook",
            &json!({"url": "https://example.com/hook", "headers": {"bad name": "SECRET-PAYLOAD"}}),
        )
        .expect_err("header");
        assert_no_payload(&err.to_string(), "SECRET-PAYLOAD");

        for name in [
            "Content-Type",
            "content-length",
            "Host",
            "X-Llmobs-Signature",
        ] {
            assert_invalid(
                "webhook",
                &json!({"url": "https://example.com/hook", "headers": {name: "1"}}),
                "headers",
                "name is reserved",
            );
        }
    }

    #[test]
    fn output_must_agree_with_the_config_and_the_name_has_a_length() {
        let boolean = EvaluatorConfig::from_parts("llm_judge", &boolean_judge()).expect("judge");
        let numeric = EvaluatorConfig::from_parts(
            "llm_judge",
            &json!({
                "connection_id": ID,
                "model": "m",
                "prompt": "p",
                "output": {"type": "numeric", "min": 0, "max": 1}
            }),
        )
        .expect("numeric");
        let labels = ["correct", "partially_correct", "incorrect"];
        let categorical = EvaluatorConfig::from_parts(
            "llm_judge",
            &json!({
                "connection_id": ID,
                "model": "m",
                "prompt": "p",
                "output": {"type": "categorical", "labels": labels, "label_values": {"correct": 1}}
            }),
        )
        .expect("categorical");
        let exact =
            EvaluatorConfig::from_parts("check", &json!({"check": "exact_match"})).expect("exact");
        let levenshtein = EvaluatorConfig::from_parts("check", &json!({"check": "levenshtein"}))
            .expect("levenshtein");
        let webhook =
            EvaluatorConfig::from_parts("webhook", &json!({"url": "https://example.com/hook"}))
                .expect("webhook");

        let mismatches = [
            (
                boolean.clone(),
                score(ScoreDataType::Numeric, None),
                ConfigError::OutputMismatch {
                    expected: ScoreDataType::Boolean,
                    found: ScoreDataType::Numeric,
                },
            ),
            (
                numeric,
                score(ScoreDataType::Boolean, None),
                ConfigError::OutputMismatch {
                    expected: ScoreDataType::Numeric,
                    found: ScoreDataType::Boolean,
                },
            ),
            (
                categorical.clone(),
                score(ScoreDataType::Boolean, None),
                ConfigError::OutputMismatch {
                    expected: ScoreDataType::Categorical,
                    found: ScoreDataType::Boolean,
                },
            ),
            (
                exact,
                score(ScoreDataType::Numeric, None),
                ConfigError::OutputMismatch {
                    expected: ScoreDataType::Boolean,
                    found: ScoreDataType::Numeric,
                },
            ),
            (
                levenshtein,
                score(ScoreDataType::Boolean, None),
                ConfigError::OutputMismatch {
                    expected: ScoreDataType::Numeric,
                    found: ScoreDataType::Boolean,
                },
            ),
        ];
        for (config, output, expected) in mismatches {
            let err = evaluator(config, output).validate().expect_err("mismatch");
            assert_eq!(err, expected);
        }

        let err = evaluator(
            categorical.clone(),
            score(
                ScoreDataType::Categorical,
                Some(vec!["incorrect", "correct"]),
            ),
        )
        .validate()
        .expect_err("order");
        assert_eq!(
            err,
            ConfigError::Invalid {
                field: "categories",
                reason: "must equal labels",
            }
        );
        evaluator(
            categorical,
            score(ScoreDataType::Categorical, Some(labels.to_vec())),
        )
        .validate()
        .expect("categories match");

        evaluator(boolean, score(ScoreDataType::Boolean, None))
            .validate()
            .expect("boolean judge");
        evaluator(webhook.clone(), score(ScoreDataType::Categorical, None))
            .validate()
            .expect("webhook categorical");
        let err = evaluator(webhook, score(ScoreDataType::Text, None))
            .validate()
            .expect_err("webhook text");
        assert_eq!(
            err,
            ConfigError::Invalid {
                field: "data_type",
                reason: "must be numeric, boolean, or categorical",
            }
        );

        let mut named = evaluator(
            EvaluatorConfig::from_parts("check", &json!({"check": "json_valid"})).expect("json"),
            score(ScoreDataType::Boolean, None),
        );
        named.name = "é".repeat(128);
        named.validate().expect("128 chars");
        named.name = "é".repeat(129);
        assert_eq!(
            named.validate().expect_err("129 chars"),
            ConfigError::Invalid {
                field: "name",
                reason: "must be 1 to 128 characters",
            }
        );
        named.name.clear();
        assert_eq!(
            named.validate().expect_err("empty name"),
            ConfigError::Invalid {
                field: "name",
                reason: "must be 1 to 128 characters",
            }
        );
    }
}
