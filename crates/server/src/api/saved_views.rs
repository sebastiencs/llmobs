//! Saved list views and SQL queries.
//!
//! Spec 04 `saved_views` and spec 09 § Observability data.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::common::Limit;

/// What a saved view opens.
///
/// Spec 04 `saved_views.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SavedViewKind {
    /// A trace list.
    Traces,
    /// A span list.
    Spans,
    /// A session list.
    Sessions,
    /// A user list.
    Users,
    /// A SQL query.
    Sql,
}

/// A saved list view or SQL query.
///
/// Spec 04 `saved_views` and spec 09 § Observability data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SavedView {
    /// Saved view id.
    pub id: Uuid,
    /// Which page the view opens.
    pub kind: SavedViewKind,
    /// Display name.
    pub name: String,
    /// Opaque UI state: list-page URL params, or `{query, params}` for SQL.
    #[schema(value_type = Object)]
    pub state: Map<String, Value>,
    /// Owner. Omitted when the view has no owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_user_id: Option<Uuid>,
    /// Visible to every member of the project.
    pub shared: bool,
    /// Default view for this kind when the caller has no view of their own.
    pub is_default: bool,
    /// Creation time, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: i64,
    /// Last update, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub updated_at: i64,
}

/// Body of `POST /api/v1/projects/{project}/saved-views`.
///
/// Spec 09 § Observability data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateSavedView {
    /// Which page the view opens.
    pub kind: SavedViewKind,
    /// Display name.
    pub name: String,
    /// Opaque UI state: list-page URL params, or `{query, params}` for SQL.
    #[schema(value_type = Object)]
    pub state: Map<String, Value>,
    /// Visible to every member of the project. Default false.
    #[serde(default)]
    pub shared: bool,
    /// Default view for this kind. Default false.
    #[serde(default)]
    pub is_default: bool,
}

/// Body of `PATCH /api/v1/projects/{project}/saved-views/{id}`.
///
/// Omitted fields are left unchanged. Spec 09 § Observability data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateSavedView {
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Opaque UI state: list-page URL params, or `{query, params}` for SQL.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub state: Option<Map<String, Value>>,
    /// Visible to every member of the project.
    #[serde(default)]
    pub shared: Option<bool>,
    /// Default view for this kind.
    #[serde(default)]
    pub is_default: Option<bool>,
}

/// Query parameters of `GET /api/v1/projects/{project}/saved-views`.
///
/// Spec 09 § Observability data.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct SavedViewListParams {
    /// Return only this kind.
    #[serde(default)]
    pub kind: Option<SavedViewKind>,
    /// Maximum number of items to return (1–1000, default 50).
    #[serde(default)]
    #[param(minimum = 1, maximum = 1000, default = 50)]
    pub limit: Limit,
    /// Opaque cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use uuid::Uuid;

    use super::{CreateSavedView, SavedView, SavedViewKind};

    const AT: i64 = 1_767_261_600_000_000_000;

    #[test]
    fn create_saved_view_rejects_non_object_state_and_unknown_fields() {
        let array = r#"{"kind":"traces","name":"Errors","state":[]}"#;
        assert!(serde_json::from_str::<CreateSavedView>(array).is_err());
        let extra = r#"{"kind":"traces","name":"Errors","state":{},"extra":1}"#;
        assert!(serde_json::from_str::<CreateSavedView>(extra).is_err());

        let created: CreateSavedView = serde_json::from_str(
            r#"{"kind":"traces","name":"Errors","state":{"filter":"status = 'error'"}}"#,
        )
        .unwrap();
        assert!(!created.shared);
        assert!(!created.is_default);
        assert_eq!(
            created.state.get("filter").and_then(Value::as_str),
            Some("status = 'error'")
        );
    }

    #[test]
    fn saved_view_snapshot() {
        let view = SavedView {
            id: Uuid::parse_str("01900000-0000-7000-8000-000000000401").unwrap(),
            kind: SavedViewKind::Traces,
            name: "Errors".to_owned(),
            state: serde_json::from_value(json!({
                "filter": "status = 'error'",
                "time_range": "last_24h"
            }))
            .unwrap(),
            owner_user_id: None,
            shared: true,
            is_default: true,
            created_at: AT,
            updated_at: AT,
        };
        let mut raw = serde_json::to_value(&view).unwrap();
        raw["state"] = json!([]);
        assert!(serde_json::from_value::<SavedView>(raw).is_err());

        insta::with_settings!({
            snapshot_path => "../../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!(
                "saved_view",
                serde_json::to_string_pretty(&view).unwrap()
            );
        });
    }
}
