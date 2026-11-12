//! Org members and per-project role overrides.
//!
//! Spec 08 § Authorization and spec 09 § Orgs, projects, members, keys.
//! `MemberUser` does not include instance-admin status.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::account::OrgRole;

/// A user as seen by the other members of an org.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct MemberUser {
    /// User id.
    pub id: Uuid,
    /// Email address.
    pub email: String,
    /// Display name. Omitted when the user has not set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One org membership.
///
/// `GET /api/v1/orgs/{org}/members`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrgMember {
    /// The member.
    pub user: MemberUser,
    /// Org role.
    pub role: OrgRole,
    /// When the user joined the org, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub joined_at: i64,
}

/// `PATCH /api/v1/orgs/{org}/members/{user}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateOrgMemberRequest {
    /// The new org role.
    pub role: OrgRole,
}

/// A stored project override. `none` hides the project.
///
/// Spec 04 `project_members.role`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProjectRoleOverride {
    /// Project admin.
    Admin,
    /// Member.
    Member,
    /// Read-only.
    Viewer,
    /// The project is hidden from this member.
    None,
}

/// One org member's place on a project.
///
/// `effective_role` is omitted when an override of `none` hides the project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProjectMember {
    /// The member.
    pub user: MemberUser,
    /// Org role, before the override.
    pub org_role: OrgRole,
    /// Stored override. Omitted when the member has none.
    #[serde(rename = "override", default, skip_serializing_if = "Option::is_none")]
    pub role_override: Option<ProjectRoleOverride>,
    /// Effective project role. Omitted when the project is hidden from this member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_role: Option<OrgRole>,
}

/// `PUT /api/v1/projects/{project}/members/{user}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetProjectOverrideRequest {
    /// The override to store.
    pub role: ProjectRoleOverride,
}
