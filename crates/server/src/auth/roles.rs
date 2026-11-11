//! Stored role strings. P10 adds the ceiling and override rules.
#![cfg_attr(not(test), allow(dead_code))]

pub(crate) use crate::api::account::OrgRole;

/// A per-project role override. `None` hides the project from that member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectOverride {
    /// Project admin.
    Admin,
    /// Project member.
    Member,
    /// Read-only on the project.
    Viewer,
    /// The project is hidden from this member.
    None,
}

/// The `org_members.role` text.
pub(crate) fn org_role_str(role: OrgRole) -> &'static str {
    match role {
        OrgRole::Owner => "owner",
        OrgRole::Admin => "admin",
        OrgRole::Member => "member",
        OrgRole::Viewer => "viewer",
    }
}

/// Parses `org_members.role`. An unknown string is `None` so the caller can
/// return [`crate::meta::MetaError::Corrupt`] without echoing the value.
pub(crate) fn parse_org_role(value: &str) -> Option<OrgRole> {
    match value {
        "owner" => Some(OrgRole::Owner),
        "admin" => Some(OrgRole::Admin),
        "member" => Some(OrgRole::Member),
        "viewer" => Some(OrgRole::Viewer),
        _ => None,
    }
}

/// The `project_members.role` text.
pub(crate) fn project_override_str(role: ProjectOverride) -> &'static str {
    match role {
        ProjectOverride::Admin => "admin",
        ProjectOverride::Member => "member",
        ProjectOverride::Viewer => "viewer",
        ProjectOverride::None => "none",
    }
}

/// Parses `project_members.role`. Unknown strings are `None`.
pub(crate) fn parse_project_override(value: &str) -> Option<ProjectOverride> {
    match value {
        "admin" => Some(ProjectOverride::Admin),
        "member" => Some(ProjectOverride::Member),
        "viewer" => Some(ProjectOverride::Viewer),
        "none" => Some(ProjectOverride::None),
        _ => None,
    }
}
