//! Instance administration.
//!
//! Spec 09 § Endpoints and D149. Settings are the effective config. Nothing
//! here carries an SMTP URL, a client secret, or a password.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::account::{AuthProvider, SignupPolicy};
use super::entitlements::Feature;
use super::keys::optional_time;

/// One account, as an instance admin sees it.
///
/// `GET /api/v1/admin/users` and the user field of the create and update responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminUser {
    /// User id.
    pub id: Uuid,
    /// Email address.
    pub email: String,
    /// Display name. Omitted when the user has not set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Instance administrator.
    pub is_instance_admin: bool,
    /// The account cannot sign in.
    pub disabled: bool,
    /// The address has been verified.
    pub email_verified: bool,
    /// A password is set. SSO-only accounts are false.
    pub has_password: bool,
    /// Linked provider ids, such as `github` or `oidc:okta`.
    pub providers: Vec<String>,
    /// When the account was created. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: i64,
}

/// `POST /api/v1/admin/users`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAdminUserRequest {
    /// Email address.
    pub email: String,
    /// Display name. Absent when the admin does not set one.
    #[serde(default)]
    pub name: Option<String>,
    /// Grant instance admin. Absent means no.
    #[serde(default)]
    pub is_instance_admin: Option<bool>,
}

/// `POST /api/v1/admin/users` returns 201.
///
/// The user has no password. `reset_url` is the one-hour link that sets one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreatedAdminUser {
    /// The new account. The address is already verified.
    pub user: AdminUser,
    /// `<public_url>/reset?token=…`.
    pub reset_url: String,
}

/// `PATCH /api/v1/admin/users/{id}`.
///
/// Absent fields stay as they are.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateAdminUserRequest {
    /// Disable or re-enable the account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
    /// Grant or remove instance admin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_instance_admin: Option<bool>,
}

/// `POST /api/v1/admin/users/{id}/reset-link` returns 200.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminResetLink {
    /// `<public_url>/reset?token=…`. Valid for one hour.
    pub url: String,
}

/// Effective instance settings. Read-only: they come from config.
///
/// `GET /api/v1/admin/settings`. No SMTP URL and no client secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminSettings {
    /// Origin used in links.
    pub public_url: String,
    /// Who may create an account.
    pub signup: SignupPolicy,
    /// Email domains allowed to sign up. Empty means no domain restriction.
    pub signup_domains: Vec<String>,
    /// Password login is enabled.
    pub password_enabled: bool,
    /// New accounts must verify their email before signing in.
    pub email_verification_required: bool,
    /// Domains that must use SSO. Empty means SSO is not required.
    pub enforce_sso_domains: Vec<String>,
    /// Whether `enforce_sso_domains` is applied: it needs a licence listing `sso_enforcement`.
    pub enforce_sso_applied: bool,
    /// SMTP is configured.
    pub smtp_enabled: bool,
    /// `From` mailbox. Omitted when SMTP is not configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email_from: Option<String>,
    /// Configured OAuth and OIDC providers. No client secrets.
    pub providers: Vec<AuthProvider>,
    /// Proxy CIDRs whose `X-Forwarded-For` is trusted.
    pub trusted_proxies: Vec<String>,
}

/// Where the instance licence stands (D-P-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LicenseStatus {
    /// No key is configured.
    None,
    /// A key is configured and was refused.
    Invalid,
    /// Before `expires_at`.
    Active,
    /// Expired, before `grace_ends_at`: features still on.
    Grace,
    /// After `grace_ends_at`: enterprise features are locked.
    Expired,
}

/// `GET /api/v1/admin/license`. Never contains the key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminLicense {
    /// The status now.
    pub status: LicenseStatus,
    /// Why the key was refused. Only with `invalid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Licence id. Omitted with `none` and `invalid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    /// Customer name. Omitted with `none` and `invalid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer: Option<String>,
    /// Features the licence lists, known to this build, expired or not. Empty with `none` and `invalid`.
    pub features: Vec<Feature>,
    /// Omitted with `none` and `invalid`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_time"
    )]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub expires_at: Option<i64>,
    /// Omitted with `none` and `invalid`.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "optional_time"
    )]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub grace_ends_at: Option<i64>,
}
