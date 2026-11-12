//! Configuration, check, and mapping errors.
//!
//! Messages name the evaluator kind, the field, the variable, the expression, the
//! byte offset, or serde's error category with its line and column. They never
//! quote prompt text, check inputs, patterns, header values, URLs, or other payload.

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

/// Fails the test when `message` contains `payload`.
#[cfg(test)]
pub(crate) fn assert_no_payload(message: &str, payload: &str) {
    assert!(
        !message.contains(payload),
        "error text contains the planted payload"
    );
}
