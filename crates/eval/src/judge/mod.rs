//! LLM judge calls.
//!
//! E11 is the OpenAI-compatible adapter. Anthropic and Gemini arrive in E12 and E13.

// `parse_answer` is first called by the runner in E17.
#[cfg_attr(not(test), allow(dead_code))]
mod answer;
mod http;
mod openai;

use std::str::FromStr;
use std::time::Duration;

pub use answer::RawAnswer;
use reqwest::Url;

use crate::config::JudgeOutput;
use crate::egress::EgressClient;
use crate::error::JudgeError;
use crate::secret::Secret;

/// Which provider API a connection calls.
///
/// Anthropic and Gemini are added with their adapters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adapter {
    /// OpenAI Chat Completions, including compatible servers.
    OpenAi,
}

impl Adapter {
    /// The stored connection value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::OpenAi => "openai",
        }
    }
}

impl FromStr for Adapter {
    type Err = UnknownName;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "openai" => Ok(Self::OpenAi),
            _ => Err(UnknownName),
        }
    }
}

/// How the provider is asked to fill the answer schema.
///
/// `tool` and `prompt` arrive in E14. The default is native JSON schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StructuredOutput {
    /// Provider-native structured output.
    #[default]
    Native,
}

impl StructuredOutput {
    /// The stored connection value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
        }
    }
}

impl FromStr for StructuredOutput {
    type Err = UnknownName;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text {
            "native" => Ok(Self::Native),
            _ => Err(UnknownName),
        }
    }
}

/// [`FromStr`](std::str::FromStr) failed for an adapter or a structured-output mode.
///
/// The rejected text is omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownName;

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
    /// Structured-output mode. Only native is implemented.
    pub structured_output: StructuredOutput,
}

/// One judge invocation. The caller owns the model, prompt, and output definition.
pub struct JudgeCall<'a> {
    /// Provider model id.
    pub model: &'a str,
    /// Rendered prompt. Sent as one user message.
    pub prompt: &'a str,
    /// Score the model must produce.
    pub output: &'a JudgeOutput,
    /// Include `explanation` in the schema.
    pub include_reason: bool,
    /// Sampling temperature. Omitted from the request when unset.
    pub temperature: Option<f64>,
    /// Completion cap, including reasoning tokens.
    pub max_output_tokens: u32,
    /// Whole-call timeout. The client also has a 10 s connect timeout.
    pub timeout: Duration,
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

/// What the provider returned, before the answer parser runs.
#[derive(Debug, Clone, PartialEq)]
pub struct JudgeReply {
    /// Tool-call arguments, or the message text.
    pub answer: RawAnswer,
    /// Text the model returned. Tool-call arguments are compact JSON.
    pub output_text: String,
    /// Token counts in OTel semantics.
    pub usage: JudgeUsage,
    /// Model the provider says it ran.
    pub response_model: Option<String>,
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
pub async fn call(
    client: &EgressClient,
    conn: &JudgeConnection,
    call: &JudgeCall<'_>,
) -> Result<JudgeReply, JudgeError> {
    match conn.adapter {
        Adapter::OpenAi => openai::complete(client, conn, call).await,
    }
}

#[cfg(test)]
mod tests {
    use super::{Adapter, StructuredOutput, UnknownName};

    #[test]
    fn parses_adapter_and_mode_names() {
        assert_eq!("openai".parse::<Adapter>(), Ok(Adapter::OpenAi));
        assert_eq!(Adapter::OpenAi.as_str(), "openai");
        let err = "anthropic/SECRET-PAYLOAD"
            .parse::<Adapter>()
            .expect_err("anthropic arrives in E12");
        assert_eq!(err, UnknownName);
        assert_eq!(err.to_string(), "unknown name");
        assert!(!err.to_string().contains("SECRET-PAYLOAD"));
        assert!("OpenAI".parse::<Adapter>().is_err());

        assert_eq!(
            "native".parse::<StructuredOutput>(),
            Ok(StructuredOutput::Native)
        );
        assert_eq!(StructuredOutput::default(), StructuredOutput::Native);
        assert_eq!(StructuredOutput::Native.as_str(), "native");
        assert!("tool".parse::<StructuredOutput>().is_err());
        assert!("prompt".parse::<StructuredOutput>().is_err());
    }
}
