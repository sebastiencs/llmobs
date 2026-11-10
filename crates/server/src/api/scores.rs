//! Score list parameters.
//!
//! The response is [`llmobs_core::score::Score`]. Spec 09 § Scores & annotations.

use llmobs_core::enums::{ScoreSource, ScoreTargetKind};
use llmobs_core::ids::{SpanId, TraceId};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};

use super::common::{Limit, TimeParam};

/// Query parameters of `GET /api/v1/projects/{project}/scores`.
///
/// `from` and `to` bound `created_at`. Spec 09 § Scores & annotations.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, IntoParams, ToSchema)]
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
