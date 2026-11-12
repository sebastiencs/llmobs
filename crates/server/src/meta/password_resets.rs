//! `password_resets` rows (spec 04, spec 08).
//!
//! The table stores the SHA-256 hex of the reset token. The plaintext is only
//! in the reset URL returned to the operator or the mailer.
#![cfg_attr(not(test), allow(dead_code))]

use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::MetaError;

/// Inserts one unused reset token.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the insert fails.
pub(crate) async fn insert_reset(
    conn: &mut SqliteConnection,
    hash: &str,
    user_id: Uuid,
    now: i64,
    expires_at: i64,
) -> Result<(), MetaError> {
    sqlx::query(
        "INSERT INTO password_resets (token_hash, user_id, created_at, expires_at) \
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

/// The user id of an unused, unexpired token.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or the stored id does not parse.
pub(crate) async fn valid_reset(
    pool: &SqlitePool,
    hash: &str,
    now: i64,
) -> Result<Option<Uuid>, MetaError> {
    let id: Option<String> = sqlx::query_scalar(
        "SELECT user_id FROM password_resets \
         WHERE token_hash = ? AND used_at IS NULL AND expires_at > ?",
    )
    .bind(hash)
    .bind(now)
    .fetch_optional(pool)
    .await?;
    id.map(|id| parse_user_id(&id)).transpose()
}

/// Marks one unused, unexpired token used and returns its user id.
///
/// A missing, used, or expired token is `Ok(None)` and changes no row.
///
/// # Errors
///
/// Returns [`MetaError`] when the update fails or the stored id does not parse.
pub(crate) async fn use_reset(
    conn: &mut SqliteConnection,
    hash: &str,
    now: i64,
) -> Result<Option<Uuid>, MetaError> {
    let id: Option<String> = sqlx::query_scalar(
        "UPDATE password_resets SET used_at = ? \
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

/// Deletes every reset token for `user_id`.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the delete fails.
pub(crate) async fn delete_user_resets(
    conn: &mut SqliteConnection,
    user_id: Uuid,
) -> Result<(), MetaError> {
    sqlx::query("DELETE FROM password_resets WHERE user_id = ?")
        .bind(user_id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

fn parse_user_id(id: &str) -> Result<Uuid, MetaError> {
    Uuid::parse_str(id).map_err(|_| MetaError::Corrupt {
        table: "password_resets",
        column: "user_id",
    })
}
