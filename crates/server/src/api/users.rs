//! End-user response types.
//!
//! Spec 09 § Observability data and § Response shapes.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::common::{Limit, SortOrder, TimeParam};
use super::sessions::SessionSummary;

/// Most sessions a user detail response includes. Spec 09 § Response shapes.
pub const MAX_USER_SESSIONS: usize = 100;

/// One row of the end-user list.
///
/// `first_seen` is the minimum trace `start_time` and `last_seen` the maximum `end_time`.
/// `error_count` is the sum of the traces' `error_count`. Spec 09 § Observability data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EndUserSummary {
    /// End-user id.
    pub user_id: String,
    /// Earliest trace start, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub first_seen: i64,
    /// Latest trace end, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub last_seen: i64,
    /// Number of traces.
    pub trace_count: i64,
    /// Number of sessions.
    pub session_count: i64,
    /// Sum of the traces' `error_count`.
    pub error_count: i64,
    /// Sum of trace token totals. Omitted when every trace omitted the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
    /// Sum of trace costs, USD. Omitted when every trace omitted the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_total: Option<f64>,
}

/// User summary plus the most recent sessions.
///
/// Spec 09 § Response shapes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EndUserDetail {
    /// User row.
    pub user: EndUserSummary,
    /// The user's most recent sessions. At most [`MAX_USER_SESSIONS`].
    pub sessions: Vec<SessionSummary>,
}

/// Sort key for user lists.
///
/// Spec 09 § Observability data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UserSort {
    /// `last_seen`.
    #[default]
    LastSeen,
    /// `trace_count`.
    TraceCount,
    /// `cost_total`.
    CostTotal,
}

/// Query parameters of `GET /api/v1/projects/{project}/users`.
///
/// The filter runs on the `traces` view before traces are grouped by user. Spec 09 § Observability data.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, IntoParams, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UserListParams {
    /// Filter DSL expression over the `traces` view, applied before grouping (spec 05).
    #[serde(default)]
    pub filter: Option<String>,
    /// Inclusive range start. RFC 3339 or epoch milliseconds.
    #[serde(default)]
    pub from: Option<TimeParam>,
    /// Exclusive range end. RFC 3339 or epoch milliseconds.
    #[serde(default)]
    pub to: Option<TimeParam>,
    /// Sort key. Defaults to last seen.
    #[serde(default)]
    pub sort: UserSort,
    /// Sort direction. Defaults to descending.
    #[serde(default)]
    pub order: SortOrder,
    /// Maximum number of items to return (1–1000, default 50).
    #[serde(default)]
    #[param(minimum = 1, maximum = 1000, default = 50)]
    pub limit: Limit,
    /// Opaque cursor from a previous page.
    #[serde(default)]
    pub cursor: Option<String>,
    /// Include traces from the evaluation environment. Default false. Spec 05 § Default exclusions.
    #[serde(default)]
    pub include_eval: bool,
    /// Include experiment traces. Default false. Spec 05 § Default exclusions.
    #[serde(default)]
    pub include_experiments: bool,
}

#[cfg(test)]
mod tests {
    use llmobs_core::enums::ScoreSource;
    use llmobs_core::score::ScoreValue;
    use uuid::Uuid;

    use super::{EndUserDetail, EndUserSummary};
    use crate::api::sessions::SessionSummary;
    use crate::api::traces::ScoreBrief;

    const AT: i64 = 1_767_261_600_000_000_000;

    #[test]
    fn end_user_detail_snapshot() {
        let detail = EndUserDetail {
            user: EndUserSummary {
                user_id: "user-42".to_owned(),
                first_seen: AT,
                last_seen: AT + 3_000_000_000,
                trace_count: 2,
                session_count: 1,
                error_count: 0,
                total_tokens: Some(2600),
                cost_total: Some(0.00306),
            },
            sessions: vec![SessionSummary {
                session_id: "sess-001".to_owned(),
                start_time: AT,
                end_time: AT + 4_200_000_000,
                duration_ns: 4_200_000_000,
                trace_count: 2,
                error_count: 0,
                total_tokens: Some(2600),
                cost_total: Some(0.00306),
                user_id: Some("user-42".to_owned()),
                tags: vec!["support".to_owned()],
                scores: vec![ScoreBrief {
                    score_id: Uuid::parse_str("01900000-0000-7000-8000-000000000305").unwrap(),
                    name: "resolved".to_owned(),
                    value: ScoreValue::Boolean(true),
                    source: ScoreSource::Human,
                }],
            }],
        };

        insta::with_settings!({
            snapshot_path => "../../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!(
                "end_user_detail",
                serde_json::to_string_pretty(&detail).unwrap()
            );
        });
    }
}
