//! Score definition request and response types.
//!
//! Spec 09 § Evaluation & alerts and spec 04 § Scores definitions. A definition names the
//! shape of a score; score writes and evaluators must fit the live definition of their name.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use llmobs_core::enums::ScoreDataType;

use super::common::Limit;

/// One label of a categorical score definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ScoreDefinitionCategory {
    /// The label, 1 to 255 bytes.
    pub label: String,
    /// Numeric value of the label. Omitted when the label has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
}

/// A named score shape. Archived definitions are kept but not checked against.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ScoreDefinition {
    /// Definition id.
    pub id: Uuid,
    /// Score name, unique among live definitions of the project.
    pub name: String,
    /// Score data type. Immutable.
    pub data_type: ScoreDataType,
    /// Labels of a categorical score. Empty otherwise.
    pub categories: Vec<ScoreDefinitionCategory>,
    /// Lowest allowed value of a numeric score. Omitted when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_value: Option<f64>,
    /// Highest allowed value of a numeric score. Omitted when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_value: Option<f64>,
    /// What the score means. Omitted when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// When the definition was archived. Omitted while it is live. JSON is RFC 3339.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "crate::api::keys::optional_time"
    )]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub archived_at: Option<i64>,
    /// Creation time. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: i64,
    /// Last change time. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: i64,
}

/// `POST /api/v1/projects/{project}/score-definitions` body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateScoreDefinitionRequest {
    /// Score name, 1 to 255 bytes after trimming.
    pub name: String,
    /// Score data type. Immutable once created.
    pub data_type: ScoreDataType,
    /// Labels of a categorical score: 1 to 100, unique. Only for categorical scores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<ScoreDefinitionCategory>>,
    /// Lowest allowed value. Only for numeric scores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_value: Option<f64>,
    /// Highest allowed value. Only for numeric scores, above `min_value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_value: Option<f64>,
    /// What the score means, at most 1 000 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `PATCH /api/v1/projects/{project}/score-definitions/{id}` body. Absent means unchanged.
///
/// `min_value` and `max_value` cannot be cleared once set. Archive the definition instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateScoreDefinitionRequest {
    /// Score name, 1 to 255 bytes after trimming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Labels of a categorical score: 1 to 100, unique.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<ScoreDefinitionCategory>>,
    /// Lowest allowed value. Only for numeric scores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_value: Option<f64>,
    /// Highest allowed value. Only for numeric scores, above `min_value`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_value: Option<f64>,
    /// What the score means. An empty string clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `true` archives the definition; `false` restores it, unless another live one has its name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
}

/// Query parameters of `GET /api/v1/projects/{project}/score-definitions`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct ScoreDefinitionListParams {
    /// Include archived definitions. Default false.
    #[serde(default)]
    pub include_archived: bool,
    /// Maximum number of items to return (1–1000, default 50).
    #[serde(default)]
    #[param(minimum = 1, maximum = 1000, default = 50)]
    pub limit: Limit,
    /// Opaque cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}
