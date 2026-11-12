//! `email_verifications` rows (spec 04, spec 08, D146).
//!
//! The table stores the SHA-256 hex of the verification token. The plaintext is
//! only in the verification URL handed to the mailer.
#![cfg_attr(not(test), allow(dead_code))]

use sqlx::SqliteConnection;
use uuid::Uuid;

use super::MetaError;

/// Deletes this user's unused tokens, then inserts `hash`.
///
/// A used token stays, so presenting it again is the same rejection as an
/// unknown one. The caller runs this inside one `meta.db` transaction.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the delete or the insert fails.
pub(crate) async fn replace_verification(
    conn: &mut SqliteConnection,
    user_id: Uuid,
    hash: &str,
    now: i64,
    expires_at: i64,
) -> Result<(), MetaError> {
    sqlx::query("DELETE FROM email_verifications WHERE user_id = ? AND used_at IS NULL")
        .bind(user_id.to_string())
        .execute(&mut *conn)
        .await?;
    sqlx::query(
        "INSERT INTO email_verifications (token_hash, user_id, created_at, expires_at) \
         VALUES (?, ?, ?, ?)",
    )
    .bind(hash)
    .bind(user_id.to_string())
    .bind(now)
    .bind(expires_at)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Marks an unused, unexpired token used and returns its user id.
///
/// A missing, used, or expired token is `Ok(None)` and changes no row.
///
/// # Errors
///
/// Returns [`MetaError`] when the update fails or the stored id does not parse.
pub(crate) async fn use_verification(
    conn: &mut SqliteConnection,
    hash: &str,
    now: i64,
) -> Result<Option<Uuid>, MetaError> {
    let id: Option<String> = sqlx::query_scalar(
        "UPDATE email_verifications SET used_at = ? \
         WHERE token_hash = ? AND used_at IS NULL AND expires_at > ? \
         RETURNING user_id",
    )
    .bind(now)
    .bind(hash)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await?;
    id.map(|id| parse_user_id(&id)).transpose()
}

fn parse_user_id(id: &str) -> Result<Uuid, MetaError> {
    Uuid::parse_str(id).map_err(|_| MetaError::Corrupt {
        table: "email_verifications",
        column: "user_id",
    })
}
