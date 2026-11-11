//! `personal_tokens` rows (spec 04). P17 adds creation and lookup.
#![cfg_attr(not(test), allow(dead_code))]

use sqlx::SqliteConnection;
use uuid::Uuid;

use super::MetaError;

/// Sets `revoked_at` on every live personal access token of `user_id`.
///
/// A token that is already revoked keeps its original `revoked_at`.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn revoke_user_tokens(
    conn: &mut SqliteConnection,
    user_id: Uuid,
    now: i64,
) -> Result<(), MetaError> {
    sqlx::query(
        "UPDATE personal_tokens SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL",
    )
    .bind(now)
    .bind(user_id.to_string())
    .execute(&mut *conn)
    .await?;
    Ok(())
}
