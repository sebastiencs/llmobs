//! Score list parameters and the score write contract.
//!
//! The list response is [`llmobs_core::score::Score`]. Spec 09 § Scores & annotations.
//! Spec 06 § Scores and § End-user feedback; D-G-26 to D-G-28.

use std::collections::BTreeMap;

use llmobs_core::enums::{ScoreSource, ScoreTargetKind};
use llmobs_core::ids::{SpanId, TraceId};
use llmobs_core::score::{ScoreTarget, ScoreValue};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use super::common::{Limit, TimeParam};

/// One score to write. Spec 06 § Scores, D-F-1, D-G-28.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScoreCreate {
    /// Client id; a UUIDv7 gives `created_at` from its time (D-G-26). Derived when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_id: Option<Uuid>,
    /// Span, trace or session the score is attached to.
    pub target: ScoreTarget,
    /// Session of a span or trace target. Ignored for a session target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// End user the score is about, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Score name.
    pub name: String,
    /// The score value.
    pub value: ScoreValue,
    /// Free-text reason for the value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Who wrote the score. Defaults by caller (D-G-27).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ScoreSource>,
    /// String metadata stored with the score.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

/// `POST …/scores` and `POST /api/v1/scores` body.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScoreBatch {
    /// Between 1 and 1000 scores.
    pub scores: Vec<ScoreCreate>,
}

/// Answer of a score write: one id per input item, in input order.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
pub struct ScoreWriteResult {
    /// One id per input item, in input order.
    pub score_ids: Vec<Uuid>,
}

/// `POST /api/public/feedback` body (spec 06 § End-user feedback).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FeedbackRequest {
    /// Trace the feedback is about.
    pub trace_id: TraceId,
    /// Span of the trace, when the feedback is about one span.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<SpanId>,
    /// Default `user_feedback`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The feedback value.
    pub value: FeedbackValue,
    /// Free-text comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// A number, a boolean or a string.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, ToSchema)]
#[serde(untagged)]
pub enum FeedbackValue {
    /// True or false.
    Boolean(bool),
    /// A finite number.
    Number(f64),
    /// A label or text.
    Text(String),
}

/// Answer of `POST /api/public/feedback`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
pub struct FeedbackResult {
    /// The id of the stored score.
    pub score_id: Uuid,
}

/// Query parameters of `GET /api/v1/projects/{project}/scores`.
///
/// `from` and `to` bound `created_at`. Spec 09 § Scores & annotations.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
pub struct ScoreListParams {
    /// Kind of target to return.
    #[serde(default)]
    pub target: Option<ScoreTargetKind>,
    /// Scores attached to this trace.
    #[serde(default)]
    pub trace_id: Option<TraceId>,
    /// Scores attached to this span.
    #[serde(default)]
    pub span_id: Option<SpanId>,
    /// Scores attached to this session.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Score name.
    #[serde(default)]
    pub name: Option<String>,
    /// Who wrote the score.
    #[serde(default)]
    pub source: Option<ScoreSource>,
    /// Inclusive `created_at` start. RFC 3339 or epoch milliseconds.
    #[serde(default)]
    pub from: Option<TimeParam>,
    /// Exclusive `created_at` end. RFC 3339 or epoch milliseconds.
    #[serde(default)]
    pub to: Option<TimeParam>,
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
    use llmobs_core::enums::{ScoreSource, ScoreTargetKind};

    use super::ScoreListParams;

    #[test]
    fn score_list_params_parse_ids_and_enums() {
        let params: ScoreListParams = serde_urlencoded::from_str(
            "trace_id=a1000000000000000000000000000001&span_id=b100000000000012&target=span&source=llm_judge",
        )
        .unwrap();
        assert_eq!(params.target, Some(ScoreTargetKind::Span));
        assert_eq!(params.source, Some(ScoreSource::LlmJudge));
        assert_eq!(
            params.trace_id.unwrap().to_string(),
            "a1000000000000000000000000000001"
        );
        assert_eq!(params.span_id.unwrap().to_string(), "b100000000000012");
    }
}
