//! LLM judge calls.
//!
//! E11 is the OpenAI-compatible adapter, E12 is Anthropic, and E13 is Gemini.

mod answer;
mod anthropic;
mod gemini;
mod http;
mod openai;

use std::time::Duration;

#[cfg(test)]
pub(crate) use answer::Answer;
pub(crate) use answer::{RawAnswer, parse_answer};
pub(crate) use answer::{cut_chars, normalize_label};
use reqwest::Url;

use crate::config::JudgeOutput;
use crate::egress::EgressClient;
use crate::error::JudgeError;
use crate::secret::Secret;

/// Which provider API a connection calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adapter {
    /// OpenAI Chat Completions, including compatible servers.
    OpenAi,
    /// Anthropic Messages.
    Anthropic,
    /// Gemini `generateContent`.
    Gemini,
}

impl Adapter {
    /// The stored connection value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
        }
    }

    /// Parses the stored connection value.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_name(text: &str) -> Result<Self, UnknownName> {
        match text {
            "openai" => Ok(Self::OpenAi),
            "anthropic" => Ok(Self::Anthropic),
            "gemini" => Ok(Self::Gemini),
            _ => Err(UnknownName),
        }
    }
}

/// How the provider is asked to fill the answer schema.
///
/// The default is native JSON schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StructuredOutput {
    /// Provider-native structured output.
    #[default]
    Native,
    /// A forced `submit_judgement` tool call.
    Tool,
    /// The schema is appended to the prompt.
    Prompt,
}

/// Name of the tool forced in [`StructuredOutput::Tool`].
pub(crate) const JUDGEMENT_TOOL: &str = "submit_judgement";

/// Description sent with [`JUDGEMENT_TOOL`].
pub(crate) const JUDGEMENT_DESCRIPTION: &str = "Submit your judgement.";

/// Instruction appended in [`StructuredOutput::Prompt`], before the schema.
pub(crate) const PROMPT_INSTRUCTION: &str =
    "Reply with one JSON object and nothing else. It must match this JSON Schema:";

impl StructuredOutput {
    /// The stored connection value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Tool => "tool",
            Self::Prompt => "prompt",
        }
    }

    /// Parses the stored connection value.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn from_name(text: &str) -> Result<Self, UnknownName> {
        match text {
            "native" => Ok(Self::Native),
            "tool" => Ok(Self::Tool),
            "prompt" => Ok(Self::Prompt),
            _ => Err(UnknownName),
        }
    }
}

/// [`Adapter::from_name`] or [`StructuredOutput::from_name`] rejected the text.
///
/// The rejected text is omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct UnknownName;

impl std::fmt::Display for UnknownName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("unknown name")
    }
}

impl std::error::Error for UnknownName {}

/// A decrypted provider connection. The server loads and decrypts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeConnection {
    /// Provider API.
    pub adapter: Adapter,
    /// Override for the provider base. The OpenAI default is `https://api.openai.com/v1`.
    pub base_url: Option<Url>,
    /// Provider credential. [`Debug`](std::fmt::Debug) redacts it.
    pub api_key: Secret,
    /// Extra request headers, applied after the adapter's own headers.
    ///
    /// The same header name replaces the earlier value. Values are redacted in
    /// [`Debug`](std::fmt::Debug).
    pub extra_headers: Vec<(String, Secret)>,
    /// Structured-output mode.
    pub structured_output: StructuredOutput,
}

/// One judge invocation. The caller owns the model, prompt, and output definition.
pub(crate) struct JudgeCall<'a> {
    /// Provider model id.
    pub(crate) model: &'a str,
    /// Rendered prompt. Sent as one user message.
    pub(crate) prompt: &'a str,
    /// Score the model must produce.
    pub(crate) output: &'a JudgeOutput,
    /// Include `explanation` in the schema.
    pub(crate) include_reason: bool,
    /// Sampling temperature. Omitted from the request when unset.
    pub(crate) temperature: Option<f64>,
    /// Completion cap, including reasoning tokens.
    pub(crate) max_output_tokens: u32,
    /// Whole-call timeout. The client also has a 10 s connect timeout.
    pub(crate) timeout: Duration,
}

impl std::fmt::Debug for JudgeCall<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JudgeCall")
            .field("model", &self.model)
            .field("prompt_bytes", &self.prompt.len())
            .field("output", &self.output)
            .field("include_reason", &self.include_reason)
            .field("temperature", &self.temperature)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// The user message for this mode.
///
/// Prompt mode appends [`PROMPT_INSTRUCTION`] after a blank line, then the
/// pretty-printed answer schema. Native and tool modes send the prompt as is.
pub(crate) fn user_message(
    mode: StructuredOutput,
    call: &JudgeCall<'_>,
) -> Result<String, JudgeError> {
    match mode {
        StructuredOutput::Native | StructuredOutput::Tool => Ok(call.prompt.to_owned()),
        StructuredOutput::Prompt => {
            let schema = serde_json::to_string_pretty(&answer::answer_schema(
                call.output,
                call.include_reason,
            ))
            .map_err(|_| JudgeError::InvalidRequest)?;
            Ok(format!(
                "{prompt}\n\n{PROMPT_INSTRUCTION}\n{schema}",
                prompt = call.prompt
            ))
        }
    }
}

/// What the provider returned, before the answer parser runs.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct JudgeReply {
    /// Tool-call arguments, or the message text.
    pub(crate) answer: RawAnswer,
    /// Text the model returned. Tool-call arguments are compact JSON.
    pub(crate) output_text: String,
    /// Token counts in OTel semantics.
    pub(crate) usage: JudgeUsage,
    /// Model the provider says it ran.
    pub(crate) response_model: Option<String>,
}

/// Token counts. Input includes cached prompt tokens. Output includes reasoning tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JudgeUsage {
    /// Prompt tokens, including cached tokens.
    pub input: u64,
    /// Completion tokens, including reasoning tokens.
    pub output: u64,
    /// Cached prompt tokens.
    pub cache_read: u64,
    /// Prompt tokens written to the cache.
    pub cache_write: u64,
    /// Reasoning tokens. They are also included in `output`.
    pub reasoning: u64,
}

/// Sends one judge call and returns the model's answer without parsing it.
///
/// # Errors
///
/// Returns [`JudgeError::InvalidModel`] for a model name that cannot be sent,
/// [`JudgeError::InvalidHeader`] for an extra header that is not valid HTTP, and
/// [`JudgeError::Transport`] when the client fails. A non-200 status becomes
/// [`JudgeError::Provider`]. Refusal, truncation, and content filtering are
/// their own variants and do not include the provider's text.
pub(crate) async fn call(
    client: &EgressClient,
    conn: &JudgeConnection,
    call: &JudgeCall<'_>,
) -> Result<JudgeReply, JudgeError> {
    match conn.adapter {
        Adapter::OpenAi => openai::complete(client, conn, call).await,
        Adapter::Anthropic => anthropic::complete(client, conn, call).await,
        Adapter::Gemini => gemini::complete(client, conn, call).await,
    }
}

/// Largest provider response body, in bytes (D159).
pub(crate) const RESPONSE_MAX_BYTES: usize = 4 * 1024 * 1024;

const MODEL_MAX_BYTES: usize = 256;

/// Rejects a model name that would be placed in a URL or a JSON body.
pub(crate) fn validate_model(model: &str) -> Result<(), JudgeError> {
    if model.is_empty() || model.len() > MODEL_MAX_BYTES || model.chars().any(forbidden_in_model) {
        return Err(JudgeError::InvalidModel);
    }
    Ok(())
}

fn forbidden_in_model(ch: char) -> bool {
    ch.is_whitespace() || ch.is_control() || ch == '/' || ch == '?' || ch == '#'
}

/// Joins `path` onto a provider base, preserving a query string.
///
/// A base without a trailing slash is treated as a directory. `set_path` drops
/// the query, so it is written back for bases such as `?api-version=`.
pub(crate) fn endpoint(
    base: Option<&Url>,
    default_base: &str,
    path: &str,
) -> Result<Url, JudgeError> {
    let parsed;
    let base = match base {
        Some(base) => base,
        None => {
            parsed = Url::parse(default_base).map_err(|_| JudgeError::InvalidRequest)?;
            &parsed
        }
    };
    let query = base.query().map(ToOwned::to_owned);
    let mut directory = base.clone();
    if !directory.path().ends_with('/') {
        let mut with_slash = directory.path().to_owned();
        with_slash.push('/');
        directory.set_path(&with_slash);
    }
    let mut url = directory
        .join(path)
        .map_err(|_| JudgeError::InvalidRequest)?;
    if let Some(query) = query {
        url.set_query(Some(&query));
    }
    Ok(url)
}

#[cfg(test)]
mod tests {
    use super::{Adapter, StructuredOutput, UnknownName};

    #[test]
    fn parses_adapter_and_mode_names() {
        assert_eq!(Adapter::from_name("openai"), Ok(Adapter::OpenAi));
        assert_eq!(Adapter::OpenAi.as_str(), "openai");
        assert_eq!(Adapter::from_name("anthropic"), Ok(Adapter::Anthropic));
        assert_eq!(Adapter::Anthropic.as_str(), "anthropic");
        assert_eq!(Adapter::from_name("gemini"), Ok(Adapter::Gemini));
        assert_eq!(Adapter::Gemini.as_str(), "gemini");
        let err = Adapter::from_name("vertex/SECRET-PAYLOAD").expect_err("unknown adapter");
        assert_eq!(err, UnknownName);
        assert_eq!(err.to_string(), "unknown name");
        assert!(!err.to_string().contains("SECRET-PAYLOAD"));
        assert!(Adapter::from_name("OpenAI").is_err());

        assert_eq!(
            StructuredOutput::from_name("native"),
            Ok(StructuredOutput::Native)
        );
        assert_eq!(StructuredOutput::default(), StructuredOutput::Native);
        assert_eq!(StructuredOutput::Native.as_str(), "native");
        assert_eq!(
            StructuredOutput::from_name("tool"),
            Ok(StructuredOutput::Tool)
        );
        assert_eq!(StructuredOutput::Tool.as_str(), "tool");
        assert_eq!(
            StructuredOutput::from_name("prompt"),
            Ok(StructuredOutput::Prompt)
        );
        assert_eq!(StructuredOutput::Prompt.as_str(), "prompt");
        assert!(StructuredOutput::from_name("Tool").is_err());
    }

    #[test]
    fn prompt_mode_appends_the_schema_after_a_blank_line() {
        let output = crate::config::JudgeOutput::Boolean;
        let call = super::JudgeCall {
            model: "m",
            prompt: "Grade.",
            output: &output,
            include_reason: false,
            temperature: None,
            max_output_tokens: 1,
            timeout: std::time::Duration::from_secs(1),
        };
        let text = super::user_message(StructuredOutput::Prompt, &call).expect("prompt");
        assert_eq!(
            text,
            "Grade.\n\nReply with one JSON object and nothing else. It must match this JSON Schema:\n{\n  \"type\": \"object\",\n  \"properties\": {\n    \"value\": {\n      \"type\": \"boolean\"\n    }\n  },\n  \"required\": [\n    \"value\"\n  ],\n  \"additionalProperties\": false\n}"
        );
        assert_eq!(
            super::user_message(StructuredOutput::Native, &call).expect("native"),
            "Grade."
        );
        assert_eq!(
            super::user_message(StructuredOutput::Tool, &call).expect("tool"),
            "Grade."
        );
    }
}
