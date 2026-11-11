//! Authentication building blocks. P6 is the role enums; later phases add passwords,
//! sessions, and the permission rules.

mod roles;

pub(crate) use roles::{
    OrgRole, ProjectOverride, org_role_str, parse_org_role, parse_project_override,
    project_override_str,
};
