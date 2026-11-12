//! Configuration, check, mapping, template, judge, webhook, egress, and run errors.
//!
//! Messages name the evaluator kind, the field, the variable, the expression, the
//! byte offset, the host, the provider status, or serde's error category with its
//! line and column. They never quote prompt text, check inputs, patterns, header
//! values, URL paths or queries, judge answers, provider messages, or other payload.

use llmobs_core::enums::ScoreDataType;
use serde_json::error::Category;

/// A configuration document or an assembled [`crate::Evaluator`] broke a rule.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// `evaluators.kind` is not `llm_judge`, `check`, or `webhook`.
    #[error("unknown evaluator kind `{kind}`")]
    UnknownKind {
        /// The rejected kind string.
        kind: String,
    },
    /// The JSON for a known kind does not match that kind's struct.
    ///
    /// `message` is serde's category plus the line and column. It is not
    /// serde's `Display`, which can quote the input.
    #[error("invalid {kind} configuration: {message}")]
    Json {
        /// `llm_judge`, `check`, or `webhook`.
        kind: &'static str,
        /// Category, line, and column only.
        message: String,
    },
    /// A field broke a range, shape, or uniqueness rule.
    #[error("invalid {field}: {reason}")]
    Invalid {
        /// Struct field the rule applies to.
        field: &'static str,
        /// Static description. Never a value from the document.
        reason: &'static str,
    },
    /// [`crate::OutputScore::data_type`] does not match the evaluator config.
    #[error("output data type is {found}, expected {expected}")]
    OutputMismatch {
        /// Data type the config requires.
        expected: ScoreDataType,
        /// Data type on the output score.
        found: ScoreDataType,
    },
}

impl ConfigError {
    pub(crate) fn from_json(kind: &'static str, err: &serde_json::Error) -> Self {
        let category = match err.classify() {
            Category::Syntax => "syntax",
            Category::Data => "data",
            Category::Eof => "eof",
            // Deserializing a `Value` does no I/O.
            Category::Io => "io",
        };
        let line = err.line();
        let column = err.column();
        Self::Json {
            kind,
            message: format!("{category} at line {line} column {column}"),
        }
    }
}

/// A check could not produce a score.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckError {
    /// `output` or `expected` was absent and this check reads it.
    #[error("missing variable {name}")]
    MissingVariable {
        /// `output` or `expected`.
        name: &'static str,
    },
    /// The pattern does not compile, or its compiled form exceeds 1 MiB.
    ///
    /// The pattern is omitted: it can be attacker-controlled text.
    #[error("invalid regular expression")]
    InvalidPattern,
    /// The JSON Schema does not compile.
    ///
    /// The schema is omitted: a `$ref` URL or a schema string can carry payload text.
    #[error("invalid JSON schema")]
    InvalidSchema,
}

/// A mapping document or one of its expressions broke a rule.
///
/// [`Syntax`](MappingError::Syntax) names a byte offset and a static reason.
/// [`MissingVariable`](MappingError::MissingVariable) names the variable and the
/// expression text. No variant quotes a payload value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MappingError {
    /// The expression does not match the mapping grammar.
    #[error("invalid expression at {expr_offset}: {reason}")]
    Syntax {
        /// Byte offset into the expression.
        expr_offset: usize,
        /// Static description. Never a slice of the expression.
        reason: &'static str,
    },
    /// A variable name or its JSON value is not usable.
    #[error("invalid variable `{name}`: {reason}")]
    Variable {
        /// The object key from the mapping document.
        name: String,
        /// Static description.
        reason: &'static str,
    },
    /// One variable's expression failed to parse.
    #[error("variable `{name}`: {source}")]
    InVariable {
        /// The object key from the mapping document.
        name: String,
        /// The expression error.
        source: Box<MappingError>,
    },
    /// The mapping JSON is not an object.
    #[error("mapping must be a JSON object")]
    NotAnObject,
    /// The mapping has more than 32 variables.
    #[error("mapping has more than 32 variables")]
    TooMany,
    /// A required expression resolved to nothing.
    #[error("missing variable {name} ({expr})")]
    MissingVariable {
        /// Mapping variable name.
        name: String,
        /// Original expression text.
        expr: String,
    },
    /// `span.*` was used and the target span was absent or not in the trace.
    #[error("no target span")]
    NoTargetSpan,
    /// A trace expression was used without trace data.
    #[error("no trace")]
    NoTrace,
    /// An item expression was used without an item.
    #[error("no item")]
    NoItem,
}

/// A judge prompt failed to compile or render.
///
/// `kind` is the engine's error category, not its detail text. The detail can
/// quote the template or a value, so it is never copied here.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    /// The template is longer than 64 KiB or the engine rejected it.
    #[error("invalid template at line {line}: {kind}")]
    Syntax {
        /// Line reported by the engine. `0` when the source was rejected first.
        line: usize,
        /// Engine category, such as `syntax error`.
        kind: String,
    },
    /// A top-level name used by the template was not supplied.
    #[error("missing variable {name}")]
    MissingVariable {
        /// The first missing name in sorted order.
        name: String,
    },
    /// A value or attribute was undefined while rendering.
    #[error("undefined value at line {line}")]
    Undefined {
        /// Line of the expression that read the undefined value.
        line: usize,
    },
    /// The engine's fuel budget was exhausted.
    #[error("template is too complex")]
    TooComplex,
    /// The rendered prompt is larger than 2 MiB.
    #[error("prompt is {bytes} bytes")]
    PromptTooLarge {
        /// UTF-8 length of the rendered prompt.
        bytes: usize,
    },
}

/// How the eval queue should treat one failure (D160).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Timeouts, connection failures, 408, 425, 429, and 5xx.
    Retryable,
    /// Authentication, quota, and a 403 that is not a content filter.
    Pause,
    /// Other 4xx, bad answers, refusals, and content filters.
    Permanent,
}

/// A judge answer could not be turned into a score, or the provider call failed.
///
/// No variant stores the prompt or the answer text. [`Self::Provider`] keeps the
/// provider message for the UI and leaves it out of [`Display`](std::fmt::Display).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum JudgeError {
    /// The model returned only whitespace.
    #[error("empty answer")]
    EmptyAnswer,
    /// The text is not JSON, has no fence containing JSON, and has no object.
    #[error("answer is not JSON")]
    NotJson,
    /// JSON was found, and it is not an object.
    #[error("answer is not a JSON object")]
    NotAnObject,
    /// The score field is absent. `field` is `value` or `label`.
    #[error("missing field {field}")]
    MissingField {
        /// `value` or `label`.
        field: &'static str,
    },
    /// The score field is present but has a type this output cannot use.
    #[error("field {field} has the wrong type")]
    WrongType {
        /// `value` or `label`.
        field: &'static str,
    },
    /// A numeric answer is not finite or lies outside the configured bounds.
    #[error("value is outside {min}..={max}")]
    OutOfRange {
        /// Inclusive lower bound from the evaluator.
        min: f64,
        /// Inclusive upper bound from the evaluator.
        max: f64,
    },
    /// A categorical answer matches none of the configured labels.
    #[error("unknown label")]
    UnknownLabel,
    /// The model name cannot be placed in a URL or a request body.
    #[error("invalid model name")]
    InvalidModel,
    /// An extra header name or value is not a valid HTTP header.
    ///
    /// The name and value are omitted.
    #[error("invalid header")]
    InvalidHeader,
    /// The request body could not be encoded.
    #[error("invalid request")]
    InvalidRequest,
    /// The model refused the prompt. The refusal text is discarded.
    #[error("model refused")]
    Refused,
    /// The provider omitted the answer because of a content filter.
    #[error("content filtered")]
    ContentFiltered,
    /// The provider stopped because the completion cap was reached.
    #[error("response truncated")]
    Truncated,
    /// The provider returned a non-success status.
    ///
    /// `provider_message` is kept for the UI and is not part of [`Display`](std::fmt::Display).
    #[error("provider status {status}")]
    Provider {
        /// HTTP status code.
        status: u16,
        /// Class derived from the status and the provider's type or code.
        class: ErrorClass,
        /// Provider error type, when it is a short token.
        provider_type: Option<String>,
        /// Provider error code, when it is a short token.
        provider_code: Option<String>,
        /// Provider error message, cut to 500 characters. Never logged.
        provider_message: Option<String>,
        /// Optional hint for the UI. Set by later modes.
        hint: Option<&'static str>,
    },
    /// The HTTP client failed before a provider status was available.
    #[error(transparent)]
    Transport(#[from] EgressError),
}

impl EgressError {
    /// Class for the eval queue. Timeouts and connection failures are retryable.
    #[must_use]
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Timeout | Self::Connect => ErrorClass::Retryable,
            Self::Scheme | Self::Denied { .. } | Self::Request | Self::TooLarge { .. } => {
                ErrorClass::Permanent
            }
        }
    }
}

/// A webhook call could not be turned into a score, or the HTTP call failed.
///
/// No variant stores the URL path or query, header values, the signing secret,
/// or either body. [`Self::BadResponse`] uses a fixed reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebhookError {
    /// A configured header name or value is not valid HTTP.
    ///
    /// The name and value are omitted.
    #[error("invalid header")]
    InvalidHeader,
    /// The URL could not be parsed or the body could not be signed.
    #[error("invalid request")]
    InvalidRequest,
    /// The webhook returned a status other than 200.
    #[error("webhook status {status}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Class derived from the status.
        class: ErrorClass,
    },
    /// A 200 body is not a score for this output definition.
    ///
    /// `reason` is a fixed phrase such as `not a JSON object`.
    #[error("{reason}")]
    BadResponse {
        /// Why the body was rejected.
        reason: &'static str,
    },
    /// The HTTP client failed before a status was available.
    #[error(transparent)]
    Transport(#[from] EgressError),
}

impl WebhookError {
    /// Class for the eval queue.
    ///
    /// A bad body, an invalid header, and an invalid request are permanent.
    /// [`Self::Status`] returns the class stored when the status was read.
    #[must_use]
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Status { class, .. } => *class,
            Self::Transport(error) => error.class(),
            Self::InvalidHeader | Self::InvalidRequest | Self::BadResponse { .. } => {
                ErrorClass::Permanent
            }
        }
    }
}

/// One evaluator run failed before a score was produced.
///
/// [`Display`](std::fmt::Display) is the inner error's text, which names an id,
/// a status, or a fixed reason and does not quote a payload.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EvalError {
    /// The evaluator or its connection is not usable.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A check could not produce a score.
    #[error(transparent)]
    Check(#[from] CheckError),
    /// A variable could not be resolved.
    #[error(transparent)]
    Mapping(#[from] MappingError),
    /// The judge prompt could not be rendered.
    #[error(transparent)]
    Template(#[from] TemplateError),
    /// The judge call failed or its answer was rejected.
    #[error(transparent)]
    Judge(#[from] JudgeError),
    /// The webhook call failed or its body was rejected.
    #[error(transparent)]
    Webhook(#[from] WebhookError),
    /// The HTTP client failed before a status was available.
    #[error(transparent)]
    Egress(#[from] EgressError),
}

impl JudgeError {
    /// Class for the eval queue (D160).
    ///
    /// Parse failures, refusals, content filters, and truncated answers are permanent.
    /// [`Self::Provider`] returns the class stored when the status was read.
    #[must_use]
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Provider { class, .. } => *class,
            Self::Transport(error) => error.class(),
            Self::EmptyAnswer
            | Self::NotJson
            | Self::NotAnObject
            | Self::MissingField { .. }
            | Self::WrongType { .. }
            | Self::OutOfRange { .. }
            | Self::UnknownLabel
            | Self::InvalidModel
            | Self::InvalidHeader
            | Self::InvalidRequest
            | Self::Refused
            | Self::ContentFiltered
            | Self::Truncated => ErrorClass::Permanent,
        }
    }
}

/// An outbound judge or webhook call was refused or failed.
///
/// [`Self::Denied`] names the host. No variant includes a path, query, or body.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EgressError {
    /// The URL scheme is not `http` or `https`.
    #[error("unsupported URL scheme")]
    Scheme,
    /// Every resolved address, or the literal address, is in a denied range.
    #[error("denied host {host}")]
    Denied {
        /// Hostname or literal address. No port, path, or query.
        host: String,
    },
    /// The call exceeded its timeout.
    #[error("connection timed out")]
    Timeout,
    /// The connection could not be established.
    #[error("connection failed")]
    Connect,
    /// The client could not build or send the request.
    #[error("request failed")]
    Request,
    /// The response body grew past the configured limit.
    #[error("response exceeds {limit} bytes")]
    TooLarge {
        /// The limit that was exceeded, in bytes.
        limit: usize,
    },
}

/// Fails the test when `message` contains `payload`.
#[cfg(test)]
pub(crate) fn assert_no_payload(message: &str, payload: &str) {
    assert!(
        !message.contains(payload),
        "error text contains the planted payload"
    );
}

#[cfg(test)]
mod class_tests {
    use super::{EgressError, ErrorClass, JudgeError};

    #[test]
    fn classes_match_d160() {
        assert_eq!(EgressError::Timeout.class(), ErrorClass::Retryable);
        assert_eq!(EgressError::Connect.class(), ErrorClass::Retryable);
        assert_eq!(EgressError::Scheme.class(), ErrorClass::Permanent);
        assert_eq!(EgressError::Request.class(), ErrorClass::Permanent);
        assert_eq!(
            EgressError::TooLarge { limit: 4 }.class(),
            ErrorClass::Permanent
        );
        assert_eq!(
            EgressError::Denied {
                host: "127.0.0.1".to_owned()
            }
            .class(),
            ErrorClass::Permanent
        );
        assert_eq!(
            JudgeError::Transport(EgressError::Connect).class(),
            ErrorClass::Retryable
        );
        assert_eq!(JudgeError::Refused.class(), ErrorClass::Permanent);
        assert_eq!(JudgeError::ContentFiltered.class(), ErrorClass::Permanent);
        assert_eq!(JudgeError::Truncated.class(), ErrorClass::Permanent);
        assert_eq!(JudgeError::InvalidModel.class(), ErrorClass::Permanent);
        assert_eq!(JudgeError::EmptyAnswer.class(), ErrorClass::Permanent);
        assert_eq!(
            JudgeError::OutOfRange { min: 0.0, max: 1.0 }.class(),
            ErrorClass::Permanent
        );
    }
}
