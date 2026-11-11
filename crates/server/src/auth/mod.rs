//! Authentication building blocks: roles, tokens, passwords, rate limits, and web sessions.

mod limits;
mod password;
mod roles;
mod secret;
mod session;

pub(crate) use limits::Limits;

/// A failure while minting a token or hashing a password.
///
/// The text is static. It does not include the password, the token, or the salt.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AuthError {
    /// The OS random source failed.
    #[error("random number generator failed")]
    Rng,
    /// argon2 rejected the parameters or the hash operation failed.
    #[error("password hash failed")]
    PasswordHash,
}

pub(crate) use roles::{
    OrgRole, ProjectOverride, org_role_str, parse_org_role, parse_project_override,
    project_override_str,
};
