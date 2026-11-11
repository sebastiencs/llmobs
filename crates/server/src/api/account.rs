//! Account, organization, and project types.
//!
//! Spec 08 § Sign-up policy and § Authorization, spec 09 § Endpoints.

use llmobs_core::ids::{OrgId, ProjectId};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// Login methods the UI should offer.
///
/// `GET /api/auth/config`. Spec 08 § Sign-up policy and spec 09 § Auth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuthConfig {
    /// No user exists yet, so the UI shows the setup page.
    pub setup_required: bool,
    /// Password login is enabled. False when `LLMOBS_AUTH_DISABLE_PASSWORD` is set.
    pub password_enabled: bool,
    /// Who may create an account after bootstrap.
    pub signup: SignupPolicy,
    /// Configured OAuth and OIDC providers.
    pub providers: Vec<AuthProvider>,
    /// SMTP is configured, so password reset and verification email can be sent.
    pub smtp_enabled: bool,
    /// New accounts must verify their email before signing in.
    pub email_verification_required: bool,
}

/// Who may register after the first user exists.
///
/// Spec 08 `LLMOBS_AUTH_SIGNUP`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SignupPolicy {
    /// Anyone may register.
    Open,
    /// Only invited emails may register.
    InviteOnly,
    /// Registration is closed.
    Disabled,
}

/// One OAuth or OIDC provider the UI can start a login with.
///
/// Spec 08 § OAuth / OIDC. `id` matches `identities.provider`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuthProvider {
    /// Provider id: `github`, `google`, or `oidc:<name>`.
    pub id: String,
    /// Name shown on the login button.
    pub display_name: String,
}

/// The signed-in user and the orgs they belong to.
///
/// `GET /api/auth/me`. Spec 09 § Auth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Me {
    /// The signed-in user.
    pub user: User,
    /// Orgs the user belongs to, with their org role.
    pub orgs: Vec<OrgMembership>,
}

/// A user account.
///
/// Spec 04 `users` and spec 08.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct User {
    /// User id.
    pub id: Uuid,
    /// Email address.
    pub email: String,
    /// Display name. Omitted when the user has not set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Instance administrator, allowed to manage instance settings.
    pub is_instance_admin: bool,
}

/// One org membership of the signed-in user.
///
/// Spec 04 `org_members`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrgMembership {
    /// Org id.
    pub id: OrgId,
    /// URL slug.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// Org role. A project override of `none` hides that project and is never returned.
    pub role: OrgRole,
}

/// Role on an org, and the caller's effective role on a project.
///
/// Spec 04 `org_members.role` and spec 08 § Authorization. `none` is not a response value:
/// a project override of `none` hides the project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum OrgRole {
    /// Org owner. At project level this is the effective role when no override is set.
    Owner,
    /// Org or project admin.
    Admin,
    /// Member.
    Member,
    /// Read-only.
    Viewer,
}

/// A project the caller can see.
///
/// Spec 04 `projects` and spec 09 § Orgs, projects, members, keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Project {
    /// Project id.
    pub id: ProjectId,
    /// Owning org.
    pub org_id: OrgId,
    /// URL slug, unique within the org.
    pub slug: String,
    /// Display name.
    pub name: String,
    /// Retention in days. Omitted when the project uses the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<u32>,
    /// The caller's effective role on this project.
    pub role: OrgRole,
    /// Creation time, nanoseconds since the Unix epoch. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: i64,
}

/// `POST /api/auth/setup`. The token is the one logged at startup (D144).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SetupRequest {
    /// The setup token from the startup log.
    pub setup_token: String,
    /// Email address of the first admin.
    pub email: String,
    /// Password of the first admin.
    pub password: String,
    /// Display name. Absent when the admin does not set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Name of the first organization.
    pub org_name: String,
    /// Name of the first project.
    pub project_name: String,
}

/// The first admin, org, project, and the secret key shown once.
///
/// `POST /api/auth/setup` returns 201. `secret_key` is not stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetupResponse {
    /// The instance admin.
    pub user: User,
    /// The org, with the owner role.
    pub org: OrgMembership,
    /// The project, with the owner role.
    pub project: Project,
    /// Secret API key. Shown once.
    pub secret_key: String,
    /// Type prefix plus the first 6 characters of the key tail.
    pub secret_key_prefix: String,
}

/// `POST /api/auth/login`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    /// Email address.
    pub email: String,
    /// Password.
    pub password: String,
}

/// `POST /api/auth/signup`. P20 adds `invite_token`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SignupRequest {
    /// Email address.
    pub email: String,
    /// Password.
    pub password: String,
    /// Display name. Absent when the user does not set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `POST /api/auth/signup` returns 201.
///
/// `me` is present when a session was started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SignupResponse {
    /// The new user and their orgs, when a session was started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub me: Option<Me>,
    /// The account cannot sign in until the email is verified.
    pub verification_required: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREATED_AT: i64 = 1_767_261_600_000_000_000;

    fn org_id() -> OrgId {
        "01900000-0000-7000-8000-000000000010".parse().unwrap()
    }

    fn project_id() -> ProjectId {
        "01900000-0000-7000-8000-000000000001".parse().unwrap()
    }

    fn user() -> User {
        User {
            id: "01900000-0000-7000-8000-000000000100".parse().unwrap(),
            email: "ada@example.com".to_owned(),
            name: Some("Ada Example".to_owned()),
            is_instance_admin: true,
        }
    }

    #[test]
    fn enum_spellings() {
        let policies = [
            (SignupPolicy::Open, "open"),
            (SignupPolicy::InviteOnly, "invite_only"),
            (SignupPolicy::Disabled, "disabled"),
        ];
        for (policy, spelling) in policies {
            let json = serde_json::to_value(policy).unwrap();
            assert_eq!(json, spelling);
            assert_eq!(
                serde_json::from_value::<SignupPolicy>(json).unwrap(),
                policy
            );
        }
        let roles = [
            (OrgRole::Owner, "owner"),
            (OrgRole::Admin, "admin"),
            (OrgRole::Member, "member"),
            (OrgRole::Viewer, "viewer"),
        ];
        for (role, spelling) in roles {
            let json = serde_json::to_value(role).unwrap();
            assert_eq!(json, spelling);
            assert_eq!(serde_json::from_value::<OrgRole>(json).unwrap(), role);
        }
    }

    #[test]
    fn account_types_snapshot() {
        let config = AuthConfig {
            setup_required: false,
            password_enabled: true,
            signup: SignupPolicy::InviteOnly,
            providers: vec![
                AuthProvider {
                    id: "github".to_owned(),
                    display_name: "GitHub".to_owned(),
                },
                AuthProvider {
                    id: "oidc:okta".to_owned(),
                    display_name: "Okta".to_owned(),
                },
            ],
            smtp_enabled: false,
            email_verification_required: false,
        };
        let me = Me {
            user: user(),
            orgs: vec![OrgMembership {
                id: org_id(),
                slug: "acme".to_owned(),
                name: "Acme".to_owned(),
                role: OrgRole::Owner,
            }],
        };
        let project = Project {
            id: project_id(),
            org_id: org_id(),
            slug: "support-bot".to_owned(),
            name: "Support bot".to_owned(),
            retention_days: Some(30),
            role: OrgRole::Owner,
            created_at: CREATED_AT,
        };
        let unnamed = User {
            name: None,
            ..user()
        };

        insta::with_settings!({
            snapshot_path => "../../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!("auth_config", serde_json::to_string_pretty(&config).unwrap());
            insta::assert_snapshot!("me", serde_json::to_string_pretty(&me).unwrap());
            insta::assert_snapshot!("project", serde_json::to_string_pretty(&project).unwrap());
            insta::assert_snapshot!("user_without_name", serde_json::to_string_pretty(&unnamed).unwrap());
        });
    }
}
