//! Password reset links (spec 08).
//!
//! The URL carries the plaintext token. The database stores its SHA-256 hex.
#![cfg_attr(not(test), allow(dead_code))]

use std::time::Duration;

use uuid::Uuid;

use super::AuthError;
use super::secret::new_opaque_token;
use crate::config::PublicUrl;
use crate::meta::password_resets::insert_reset;
use crate::meta::{MetaDb, MetaError};

/// How long a reset link stays valid.
pub(crate) const LIFETIME: Duration = Duration::from_secs(60 * 60);

/// A token that has not been stored yet.
pub(crate) struct MintedReset {
    /// `<public_url>/reset?token=…`.
    pub(crate) url: String,
    /// SHA-256 hex of the token.
    pub(crate) hash: String,
    /// When the link stops working, nanoseconds.
    pub(crate) expires_at: i64,
}

/// Why a reset link could not be stored.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ResetError {
    /// The random source failed.
    #[error(transparent)]
    Auth(#[from] AuthError),
    /// `meta.db` rejected the insert.
    #[error(transparent)]
    Meta(#[from] MetaError),
}

/// Builds a reset URL. The caller stores [`MintedReset::hash`].
///
/// # Errors
///
/// Returns [`AuthError::Rng`] when the random source fails.
pub(crate) fn mint_reset(public_url: &PublicUrl, now: i64) -> Result<MintedReset, AuthError> {
    let token = new_opaque_token()?;
    let expires_at = now.saturating_add(duration_ns(LIFETIME));
    let url = format!("{}?token={}", public_url.join("/reset"), token.plaintext);
    Ok(MintedReset {
        url,
        hash: token.hash,
        expires_at,
    })
}

/// Stores a new reset token and returns its URL.
///
/// # Errors
///
/// Returns [`ResetError`] when the token or the insert fails.
pub(crate) async fn reset_link(
    meta: &MetaDb,
    public_url: &PublicUrl,
    user_id: Uuid,
    now: i64,
) -> Result<String, ResetError> {
    let minted = mint_reset(public_url, now)?;
    let hash = minted.hash.clone();
    let expires_at = minted.expires_at;
    meta.write(move |conn| {
        Box::pin(async move { insert_reset(conn, &hash, user_id, now, expires_at).await })
    })
    .await?;
    Ok(minted.url)
}

fn duration_ns(duration: Duration) -> i64 {
    i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX)
}
