//! Session response types.
//!
//! Spec 09 § Observability data and § Response shapes.

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::common::{Limit, SortOrder, TimeParam};
use super::payload::Payload;
use super::traces::{ScoreBrief, TraceSummary};

/// Most traces a session detail response includes. Spec 09 § Response shapes.
pub const MAX_SESSION_TRACES: usize = 1_000;

/// One row of the session list.
///
/// Spec 01 § Session and spec 09 § Observability data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SessionSummary {
    /// Session id.
    pub session_id: String,
    /// Earliest trace start, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub start_time: i64,
    /// Latest trace end, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub end_time: i64,
    /// `end_time - start_time`, nanoseconds.
    pub duration_ns: i64,
    /// Number of traces in the session.
    pub trace_count: i64,
    /// Sum of the traces' `error_count`.
    pub error_count: i64,
    /// Sum of trace token totals. Omitted when every trace omitted the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
    /// Sum of trace costs, USD. Omitted when every trace omitted the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_total: Option<f64>,
    /// End-user id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Sorted distinct union of the session's trace tags.
    pub tags: Vec<String>,
    /// Latest score of each name attached to this session.
    pub scores: Vec<ScoreBrief>,
}

/// One trace in a session, oldest first, with the root span's payloads.
///
/// `input` and `output` are omitted when the trace has no root. Spec 09 § Response shapes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SessionTurn {
    /// Trace row.
    pub trace: TraceSummary,
    /// Root span input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Payload>,
    /// Root span output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Payload>,
}

/// Session summary plus its turns.
///
/// Spec 09 § Response shapes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SessionDetail {
    /// Session row.
    pub session: SessionSummary,
    /// Traces by `start_time`. At most [`MAX_SESSION_TRACES`].
    pub turns: Vec<SessionTurn>,
    /// True when traces were omitted because the session is larger than [`MAX_SESSION_TRACES`].
    pub truncated: bool,
}

/// Sort key for session lists.
///
/// Spec 09 § Observability data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionSort {
    /// `start_time`.
    #[default]
    StartTime,
    /// `end_time`.
    EndTime,
    /// `trace_count`.
    TraceCount,
    /// `cost_total`.
    CostTotal,
}

/// Query parameters of `GET /api/v1/projects/{project}/sessions`.
///
/// Spec 09 § Observability data.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct SessionListParams {
    /// Filter DSL expression over the `sessions` view (spec 05).
    #[serde(default)]
    pub filter: Option<String>,
    /// Inclusive range start. RFC 3339 or epoch milliseconds.
    #[serde(default)]
    pub from: Option<TimeParam>,
    /// Exclusive range end. RFC 3339 or epoch milliseconds.
    #[serde(default)]
    pub to: Option<TimeParam>,
    /// Sort key. Defaults to start time.
    #[serde(default)]
    pub sort: SessionSort,
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
    use std::collections::BTreeMap;

    use llmobs_core::enums::{ScoreSource, SpanStatus};
    use llmobs_core::ids::TraceId;
    use llmobs_core::score::ScoreValue;
    use serde::Serialize;
    use uuid::Uuid;

    use super::{SessionDetail, SessionSummary, SessionTurn};
    use crate::api::payload::Payload;
    use crate::api::traces::{ScoreBrief, TraceSummary};

    const AT: i64 = 1_767_261_600_000_000_000;

    fn snap(value: &impl Serialize) -> String {
        serde_json::to_string_pretty(value).unwrap()
    }

    #[test]
    fn session_detail_snapshot() {
        let detail = SessionDetail {
            session: SessionSummary {
                session_id: "sess-001".to_owned(),
                start_time: AT,
                end_time: AT + 4_200_000_000,
                duration_ns: 4_200_000_000,
                trace_count: 1,
                error_count: 0,
                total_tokens: Some(1240),
                cost_total: Some(0.0015),
                user_id: Some("user-42".to_owned()),
                tags: vec!["support".to_owned()],
                scores: vec![ScoreBrief {
                    score_id: Uuid::parse_str("01900000-0000-7000-8000-000000000305").unwrap(),
                    name: "resolved".to_owned(),
                    value: ScoreValue::Boolean(true),
                    source: ScoreSource::Human,
                }],
            },
            turns: vec![SessionTurn {
                trace: TraceSummary {
                    trace_id: TraceId::from_bytes([
                        0xa1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
                    ])
                    .unwrap(),
                    root_span_id: None,
                    has_root: true,
                    name: "support-agent".to_owned(),
                    start_time: AT,
                    end_time: AT + 4_200_000_000,
                    duration_ns: 4_200_000_000,
                    status: SpanStatus::Ok,
                    session_id: Some("sess-001".to_owned()),
                    user_id: Some("user-42".to_owned()),
                    environment: Some("production".to_owned()),
                    release: Some("v1.4.0".to_owned()),
                    tags: vec!["support".to_owned()],
                    metadata: BTreeMap::new(),
                    input_preview: Some("My order 1234 hasn't arrived.".to_owned()),
                    output_preview: Some(
                        "Your order 1234 shipped and arrives tomorrow.".to_owned(),
                    ),
                    span_count: 4,
                    error_count: 0,
                    llm_count: 2,
                    input_tokens: Some(2500),
                    output_tokens: Some(100),
                    cache_read_tokens: Some(2200),
                    cache_write_tokens: None,
                    reasoning_tokens: None,
                    total_tokens: Some(2600),
                    cost_total: Some(0.00306),
                    models: vec!["claude-sonnet-4-5".to_owned()],
                    scores: Vec::new(),
                },
                input: Some(Payload::Text("My order 1234 hasn't arrived.".to_owned())),
                output: Some(Payload::Text(
                    "Your order 1234 shipped and arrives tomorrow.".to_owned(),
                )),
            }],
            truncated: false,
        };

        insta::with_settings!({
            snapshot_path => "../../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!("session_detail", snap(&detail));
        });
    }
}
