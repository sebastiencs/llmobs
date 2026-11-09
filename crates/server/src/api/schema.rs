//! Filter and SQL schema catalog.
//!
//! Spec 05 § Logical views and § Filter DSL. The UI picks operators from [`ColumnType`];
//! it does not need Arrow types. Binary id columns are [`ColumnType::String`] because the
//! views expose hex.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Logical views a filter or SQL query can name.
///
/// Spec 05 § Logical views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ViewName {
    /// Span rows, including heavy payload columns.
    Spans,
    /// One row per trace.
    Traces,
    /// One row per session.
    Sessions,
    /// Score rows.
    Scores,
    /// Experiment item rows.
    ExperimentItems,
    /// Dataset item rows.
    DatasetItems,
}

/// Column type the filter UI understands.
///
/// Spec 05 § Logical views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColumnType {
    /// Text, including hex ids.
    String,
    /// Integer, including durations in nanoseconds and token counts.
    Integer,
    /// Floating point, including USD costs.
    Float,
    /// Boolean.
    Boolean,
    /// Instant. JSON values are RFC 3339.
    Timestamp,
    /// List of strings.
    StringList,
    /// String map.
    StringMap,
    /// String column whose text is JSON.
    Json,
}

/// One column of a logical view.
///
/// Spec 05 § Logical views.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ColumnSchema {
    /// Column name.
    pub name: String,
    /// Type the filter UI uses to pick operators.
    pub column_type: ColumnType,
    /// Whether the column may be null.
    pub nullable: bool,
    /// What the column means.
    pub description: String,
}

/// One logical view.
///
/// Spec 05 § Logical views.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ViewSchema {
    /// View name.
    pub name: ViewName,
    /// What the view contains.
    pub description: String,
    /// Columns, in spec order.
    pub columns: Vec<ColumnSchema>,
}

/// One function the filter DSL or SQL console can call.
///
/// Spec 05 § Filter DSL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FunctionSchema {
    /// Function name.
    pub name: String,
    /// Argument and return types, for example `score(name: string) -> float`.
    pub signature: String,
    /// What the function does.
    pub description: String,
    /// Whether the filter DSL allows the function. SQL allows every function.
    pub in_filter: bool,
}

/// Views and functions the filter builder and SQL console show.
///
/// `GET /api/projects/{project_id}/schema`. Spec 05 and spec 09 § Schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SchemaResponse {
    /// Logical views.
    pub views: Vec<ViewSchema>,
    /// Functions.
    pub functions: Vec<FunctionSchema>,
}

#[cfg(test)]
mod tests {
    use super::{ColumnSchema, ColumnType, FunctionSchema, SchemaResponse, ViewName, ViewSchema};

    #[test]
    fn schema_response_snapshot() {
        let response = SchemaResponse {
            views: vec![ViewSchema {
                name: ViewName::Traces,
                description: "One row per trace.".to_owned(),
                columns: vec![ColumnSchema {
                    name: "trace_id".to_owned(),
                    column_type: ColumnType::String,
                    nullable: false,
                    description: "Trace id, 16 bytes as lowercase hex.".to_owned(),
                }],
            }],
            functions: vec![FunctionSchema {
                name: "score".to_owned(),
                signature: "score(name: string) -> float".to_owned(),
                description: "Latest numeric value of the named score.".to_owned(),
                in_filter: true,
            }],
        };

        insta::with_settings!({
            snapshot_path => "../../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!(
                "schema_response",
                serde_json::to_string_pretty(&response).unwrap()
            );
        });
    }
}
