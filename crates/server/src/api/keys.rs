//! API key request and response types.
//!
//! Spec 08 § API keys and spec 09 § Conventions.

use llmobs_core::time_serde;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::common::TimeParam;

/// `secret` or `public`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyKind {
    /// `sk-llmobs-` and the ingest, read, and write scopes.
    Secret,
    /// `pk-llmobs-` and the feedback scope.
    Public,
}

/// One scope name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApiKeyScope {
    /// OTLP and score writes.
    Ingest,
    /// Safe methods of project routes.
    Read,
    /// Datasets, experiments, scores, and trace deletion.
    Write,
    /// Public feedback.
    Feedback,
}

/// A project API key. The secret is not a field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ApiKey {
    /// Key id.
    pub id: Uuid,
    /// `secret` or `public`.
    pub kind: ApiKeyKind,
    /// Display name. Omitted when the key was created without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Type prefix plus the first 6 random characters.
    pub prefix: String,
    /// Scopes, in a stable order.
    pub scopes: Vec<ApiKeyScope>,
    /// User who created the key. Omitted when that user is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<Uuid>,
    /// Creation time. JSON is RFC 3339.
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: i64,
    /// When the key stops working. Omitted when the key does not expire.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_time"
    )]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub expires_at: Option<i64>,
    /// Last successful authentication. Omitted until a flush has written it.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_time"
    )]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub last_used_at: Option<i64>,
}

/// `POST /api/v1/projects/{project}/api-keys`. The response is the only place the secret appears.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreatedApiKey {
    /// The stored key, without the secret.
    pub key: ApiKey,
    /// The bearer value. Shown only in this response.
    pub secret: String,
}

/// `POST /api/v1/projects/{project}/api-keys` body.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateApiKeyRequest {
    /// `secret` or `public`.
    pub kind: ApiKeyKind,
    /// Display name, at most 100 characters. Absent when the key has none.
    #[serde(default)]
    pub name: Option<String>,
    /// Secret keys default to ingest, read, and write. Public keys default to feedback.
    #[serde(default)]
    pub scopes: Option<Vec<ApiKeyScope>>,
    /// When the key stops working. Absent when the key does not expire.
    #[serde(default)]
    pub expires_at: Option<TimeParam>,
}

mod optional_time {
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serializer};

    use super::time_serde;

    pub(super) fn serialize<S: Serializer>(
        value: &Option<i64>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(ns) => time_serde::serialize(ns, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<i64>, D::Error> {
        match Option::<String>::deserialize(deserializer)? {
            None => Ok(None),
            Some(text) => time_serde::parse_rfc3339(&text)
                .map(Some)
                .map_err(Error::custom),
        }
    }
}
