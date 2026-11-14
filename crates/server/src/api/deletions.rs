//! Deletion request and response types (spec 09 § Pricing & GDPR, D-M-2).

use llmobs_core::ids::{OrgId, ProjectId};
use llmobs_core::time_serde;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::common::TimeParam;
use super::keys::optional_time;

/// What a deletion covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeletionKind {
    /// Listed traces.
    TraceIds,
    /// Traces matching a filter, search and window.
    Filter,
    /// One end user's data.
    User,
    /// A whole project.
    Project,
    /// A whole organization.
    Org,
}

/// Where a deletion is (D-M-3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeletionStatus {
    /// Finding the traces it covers. The data is still visible.
    Resolving,
    /// Hidden from every read; bytes are erased within 24 h.
    Pending,
    /// Erased.
    Applied,
    /// Nothing was deleted; see `error`.
    Failed,
}

/// Why a deletion failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeletionFailure {
    /// The filter matched more than 100,000 traces when it was resolved.
    TooManyTraces,
    /// Finding the traces failed.
    ResolutionFailed,
}

/// Who requested a deletion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeletionActorKind {
    /// A user, through a session or a personal access token.
    User,
    /// A project API key.
    ApiKey,
}

/// The requester.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeletionActor {
    /// `user` or `api_key`.
    pub kind: DeletionActorKind,
    /// The user id or the API key id.
    pub id: Uuid,
}

/// The selection of a `filter` deletion, as requested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeletionFilter {
    /// Filter DSL text. Omitted when none was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Full-text search. Omitted when none was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<String>,
    /// Window start, inclusive.
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub from: i64,
    /// Window end, exclusive.
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub to: i64,
    /// Evaluation traces were included.
    pub include_eval: bool,
    /// Experiment traces were included.
    pub include_experiments: bool,
}

/// A deletion request and its progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Deletion {
    /// Deletion id.
    pub id: Uuid,
    pub kind: DeletionKind,
    pub status: DeletionStatus,
    /// The organization.
    pub org_id: OrgId,
    /// The project. Omitted for `org`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    /// The end user (`user` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// Distinct trace ids in the request (`trace_ids` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_traces: Option<u32>,
    /// The selection (`filter` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<DeletionFilter>,
    /// Name of the dropped project or organization (`project`, `org`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Slug of the dropped project or organization (`project`, `org`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slug: Option<String>,
    /// Traces covered, including evaluation traces. Omitted until resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_count: Option<u64>,
    /// Sessions covered (`user`). Omitted until resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_count: Option<u64>,
    pub requested_by: DeletionActor,
    /// When it was requested; rows ingested later are not deleted. JSON is RFC 3339.
    #[serde(with = "time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: i64,
    /// When storage began hiding the rows. Omitted before.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_time"
    )]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub handed_over_at: Option<i64>,
    /// When it was applied. Omitted before.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_time"
    )]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub applied_at: Option<i64>,
    /// Why it failed (`failed` only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<DeletionFailure>,
}

/// `POST /api/v1/projects/{project}/deletions`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CreateDeletion {
    /// 1 to 1,000 trace ids, 32 hex digits each, either case.
    TraceIds { trace_ids: Vec<String> },
    /// Traces the trace list shows for this filter, search and window (at most 100,000).
    Filter {
        #[serde(default)]
        filter: Option<String>,
        #[serde(default)]
        search: Option<String>,
        from: TimeParam,
        to: TimeParam,
        #[serde(default)]
        include_eval: bool,
        #[serde(default)]
        include_experiments: bool,
    },
    /// Every span and score of one end user, with their traces and sessions.
    User {
        /// 1 to 255 bytes, matched exactly.
        user_id: String,
    },
}

/// `POST /api/v1/projects/{project}/deletions/preview`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DeletionPreviewRequest {
    #[serde(default)]
    pub filter: Option<String>,
    #[serde(default)]
    pub search: Option<String>,
    pub from: TimeParam,
    pub to: TimeParam,
    #[serde(default)]
    pub include_eval: bool,
    #[serde(default)]
    pub include_experiments: bool,
}

/// How many traces a filter deletion would cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DeletionPreview {
    /// Matching traces, at most 100,000.
    pub matched_traces: u64,
    /// More than 100,000 traces match: the deletion would be refused.
    pub capped: bool,
}
