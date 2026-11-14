//! LLM connection request and response types.
//!
//! Spec 09 § Orgs, projects, members, keys and D-I-4. The API key and the
//! extra header values are write-only: responses carry `api_key_hint` and
//! `extra_header_names` only.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// The provider API a connection talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LlmAdapter {
    /// OpenAI Chat Completions, including compatible servers.
    Openai,
    /// Anthropic Messages.
    Anthropic,
    /// Gemini `generateContent`.
    Gemini,
}

/// How the provider is asked to fill the answer schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StructuredOutputMode {
    /// Provider-native structured output.
    Native,
    /// A forced tool call.
    Tool,
    /// The schema is appended to the prompt.
    Prompt,
}

/// Whether the connection's last test reached the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionStatus {
    /// The last test succeeded.
    Ok,
    /// A pause-class error marked the connection failing.
    Failing,
}

/// What the caller should do with a failed connection test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TestErrorClass {
    /// Try again later.
    Retryable,
    /// Stop using this connection until it is fixed.
    Pause,
    /// The request can never succeed.
    Permanent,
}

impl LlmAdapter {
    /// The stored connection value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Gemini => "gemini",
        }
    }

    /// Parses the stored connection value. Any other text is `None`.
    pub fn from_name(text: &str) -> Option<Self> {
        match text {
            "openai" => Some(Self::Openai),
            "anthropic" => Some(Self::Anthropic),
            "gemini" => Some(Self::Gemini),
            _ => None,
        }
    }
}

impl StructuredOutputMode {
    /// The stored connection value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Tool => "tool",
            Self::Prompt => "prompt",
        }
    }

    /// Parses the stored connection value. Any other text is `None`.
    pub fn from_name(text: &str) -> Option<Self> {
        match text {
            "native" => Some(Self::Native),
            "tool" => Some(Self::Tool),
            "prompt" => Some(Self::Prompt),
            _ => None,
        }
    }
}

impl ConnectionStatus {
    /// The stored connection value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failing => "failing",
        }
    }

    /// Parses the stored connection value. Any other text is `None`.
    pub fn from_name(text: &str) -> Option<Self> {
        match text {
            "ok" => Some(Self::Ok),
            "failing" => Some(Self::Failing),
            _ => None,
        }
    }
}

/// An LLM provider connection an evaluator calls. The API key and the header
/// values are not fields (D-I-4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LlmConnection {
    /// Connection id.
    pub id: Uuid,
    /// Display name, unique in the organization.
    pub name: String,
    /// The provider API the connection talks to.
    pub adapter: LlmAdapter,
    /// Provider endpoint. Omitted when the provider's default is used. May
    /// carry only an `api-version` query parameter (Azure).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Model used when a caller names none. Omitted when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// How the provider is asked to fill the answer schema.
    pub structured_output: StructuredOutputMode,
    /// Provider calls the connection runs at once, 1 to 64.
    pub max_concurrency: u32,
    /// Provider calls per minute, 1 to 10000.
    pub requests_per_minute: u32,
    /// Last 4 characters of the API key, or an empty string when the key is
    /// shorter than 16 characters.
    pub api_key_hint: String,
    /// Extra header names, lowercase, sorted. The values are not included.
    pub extra_header_names: Vec<String>,
    /// Whether the connection's last test reached the provider.
    pub status: ConnectionStatus,
    /// Last pause-class error. Omitted when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// When `last_error` was recorded. Omitted when unset.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::api::keys::optional_time"
    )]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_error_at: Option<i64>,
    /// User who created the connection. Omitted when that user is unknown.
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

/// `POST /api/v1/orgs/{org}/llm-connections` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateLlmConnectionRequest {
    /// Display name, 1 to 100 characters after trimming.
    pub name: String,
    /// The provider API the connection talks to.
    pub adapter: LlmAdapter,
    /// Provider API key, 1 to 4096 bytes. Write-only.
    pub api_key: String,
    /// Provider endpoint overriding the provider's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Model used when a caller names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// How the provider is asked to fill the answer schema. Default `native`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<StructuredOutputMode>,
    /// Provider calls the connection runs at once, 1 to 64. Default 4.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
    /// Provider calls per minute, 1 to 10000. Default 300.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_per_minute: Option<u32>,
    /// Extra headers sent with every provider call, at most 20. Values are
    /// write-only.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra_headers: BTreeMap<String, String>,
}

/// `PATCH /api/v1/orgs/{org}/llm-connections/{id}` body. Absent means
/// unchanged; an empty body is valid and changes only `updated_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateLlmConnectionRequest {
    /// Display name, 1 to 100 characters after trimming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The provider API the connection talks to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<LlmAdapter>,
    /// Replaces the API key. Write-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Provider endpoint. An empty string clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Model used when a caller names none. An empty string clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// How the provider is asked to fill the answer schema.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<StructuredOutputMode>,
    /// Provider calls the connection runs at once, 1 to 64.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
    /// Provider calls per minute, 1 to 10000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requests_per_minute: Option<u32>,
    /// Replaces the whole header map. An empty object clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_headers: Option<BTreeMap<String, String>>,
}

/// `POST /api/v1/orgs/{org}/llm-connections/{id}/test` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TestLlmConnectionRequest {
    /// Model to call. Default: the connection's `default_model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Why a connection test failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LlmConnectionTestError {
    /// What the caller should do with the failure.
    pub class: TestErrorClass,
    /// Provider or transport error text, without secrets.
    pub message: String,
}

/// `POST /api/v1/orgs/{org}/llm-connections/{id}/test` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LlmConnectionTestResult {
    /// Whether one boolean judge call succeeded.
    pub ok: bool,
    /// The model the test called.
    pub model: String,
    /// Round-trip time of the call.
    pub latency_ms: u64,
    /// The failure. Omitted when `ok` is true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<LlmConnectionTestError>,
}

#[cfg(test)]
mod tests {
    use super::{ConnectionStatus, LlmAdapter, StructuredOutputMode};

    #[test]
    fn stored_names_round_trip() {
        for adapter in [
            LlmAdapter::Openai,
            LlmAdapter::Anthropic,
            LlmAdapter::Gemini,
        ] {
            assert_eq!(LlmAdapter::from_name(adapter.as_str()), Some(adapter));
        }
        for mode in [
            StructuredOutputMode::Native,
            StructuredOutputMode::Tool,
            StructuredOutputMode::Prompt,
        ] {
            assert_eq!(StructuredOutputMode::from_name(mode.as_str()), Some(mode));
        }
        for status in [ConnectionStatus::Ok, ConnectionStatus::Failing] {
            assert_eq!(ConnectionStatus::from_name(status.as_str()), Some(status));
        }
        assert_eq!(LlmAdapter::from_name("OPENAI"), None);
        assert_eq!(LlmAdapter::from_name(""), None);
        assert_eq!(StructuredOutputMode::from_name("nativ"), None);
        assert_eq!(ConnectionStatus::from_name("paused"), None);
    }
}
