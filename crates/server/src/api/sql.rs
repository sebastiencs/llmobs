//! SQL console request and JSON result (spec 05 § SQL console, spec 09, D98).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use utoipa::ToSchema;

/// Body of `POST /api/v1/projects/{project}/sql`.
///
/// Spec 09 § Observability data, D-H-8.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SqlQueryRequest {
    /// One statement, with `{name:Type}` placeholders (spec 05 § Parameters).
    pub query: String,
    /// Values for the placeholders, by name. Default empty.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub params: Map<String, Value>,
    /// Start of the time window, RFC 3339, inclusive. Give both `from` and `to`, or neither.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub from: Option<String>,
    /// End of the time window, RFC 3339, exclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub to: Option<String>,
    /// Most rows to return as JSON, 1–10,000. Default 10,000. Checked on every request; exports do not use it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 1, maximum = 10000)]
    pub limit: Option<i64>,
    /// Time limit in milliseconds, 1,000–60,000. Default 60,000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(minimum = 1000, maximum = 60000)]
    pub timeout_ms: Option<i64>,
}

/// One output column.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SqlColumnInfo {
    /// Output column name.
    pub name: String,
    /// Arrow type as DataFusion prints it, for example `Int64` or `Utf8`.
    #[serde(rename = "type")]
    pub data_type: String,
}

/// JSON answer of `POST …/sql` (D98).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SqlQueryResult {
    /// Output columns, in order.
    pub columns: Vec<SqlColumnInfo>,
    /// One object per row, keyed by column name. Timestamps are RFC 3339 with 9 fractional
    /// digits, binary is lowercase hex, maps are objects, NaN and infinities are null.
    #[schema(value_type = Vec<Object>)]
    pub rows: Vec<Map<String, Value>>,
    /// More rows matched than `limit`; `rows` holds the first `limit`.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_rejects_unknown_fields_and_defaults_params() {
        assert!(
            serde_json::from_str::<SqlQueryRequest>(r#"{"query":"SELECT 1","foo":1}"#).is_err()
        );
        let request: SqlQueryRequest = serde_json::from_str(r#"{"query":"SELECT 1"}"#).unwrap();
        assert!(request.params.is_empty());
        assert_eq!(request.from, None);
        assert_eq!(request.limit, None);
    }
}
