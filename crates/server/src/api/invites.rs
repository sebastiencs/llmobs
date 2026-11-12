//! Org invitations.
//!
//! Spec 08 § Email and spec 09 § Orgs. The accept token is not part of these
//! types. The create response carries it once, in `accept_url`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use super::account::OrgRole;
use super::members::MemberUser;

/// One pending invitation.
///
/// `GET /api/v1/orgs/{org}/invites` and the `invite` field of the create response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Invite {
    /// Invitation id.
    pub id: Uuid,
    /// Address the invitation was sent to.
    pub email: String,
    /// Role granted on accept.
    pub role: OrgRole,
    /// Who sent it. Omitted when that user no longer exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invited_by: Option<MemberUser>,
    /// When the invitation was created. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub created_at: i64,
    /// When the invitation stops working. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: i64,
}

/// `POST /api/v1/orgs/{org}/invites`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateInviteRequest {
    /// Address to invite.
    pub email: String,
    /// Role granted on accept. Not above the caller's role.
    pub role: OrgRole,
}

/// `POST /api/v1/orgs/{org}/invites` returns 201.
///
/// `accept_url` is the only response that contains the token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct CreatedInvite {
    /// The pending invitation.
    pub invite: Invite,
    /// `<public_url>/invite/<token>`.
    pub accept_url: String,
}

/// Org named in an invitation preview. The id is not included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InviteOrg {
    /// Display name.
    pub name: String,
    /// URL slug.
    pub slug: String,
}

/// `GET /api/auth/invites/{token}`.
///
/// Only the token holder sees this. `account_exists` tells the UI whether to
/// show login or sign-up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InvitePreview {
    /// The org the invitation joins.
    pub org: InviteOrg,
    /// Address the invitation was sent to.
    pub email: String,
    /// Role granted on accept.
    pub role: OrgRole,
    /// When the invitation stops working. JSON is RFC 3339.
    #[serde(with = "llmobs_core::time_serde")]
    #[schema(value_type = String, format = DateTime)]
    pub expires_at: i64,
    /// Whether a user with this email already exists.
    pub account_exists: bool,
}
