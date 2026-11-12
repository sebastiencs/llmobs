//! `identities` rows (spec 04, D148).
//!
//! One row is a `(provider, subject)` pair. The email is the normalized
//! address from the login that created the row.

use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::{MetaError, unique_or};

/// The user for `(provider, subject)`, when that identity exists.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or the stored id is corrupt.
pub(crate) async fn identity_user(
    pool: &SqlitePool,
    provider: &str,
    subject: &str,
) -> Result<Option<Uuid>, MetaError> {
    let id: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM identities WHERE provider = ? AND subject = ?")
            .bind(provider)
            .bind(subject)
            .fetch_optional(pool)
            .await?;
    id.map(|id| parse_user(&id)).transpose()
}

/// The same lookup on the writer, so a login transaction sees its own inserts.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or the stored id is corrupt.
pub(crate) async fn identity_user_tx(
    conn: &mut SqliteConnection,
    provider: &str,
    subject: &str,
) -> Result<Option<Uuid>, MetaError> {
    let id: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM identities WHERE provider = ? AND subject = ?")
            .bind(provider)
            .bind(subject)
            .fetch_optional(&mut *conn)
            .await?;
    id.map(|id| parse_user(&id)).transpose()
}

/// Inserts one identity. `email` is already normalized.
///
/// # Errors
///
/// Returns [`MetaError::Conflict`] when `(provider, subject)` already exists.
pub(crate) async fn insert_identity(
    conn: &mut SqliteConnection,
    user_id: Uuid,
    provider: &str,
    subject: &str,
    email: &str,
    now: i64,
) -> Result<(), MetaError> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO identities (id, user_id, provider, subject, email, created_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(user_id.to_string())
    .bind(provider)
    .bind(subject)
    .bind(email)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, "identity already registered"))?;
    Ok(())
}

/// Provider ids linked to one user, in alphabetical order.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails.
pub(crate) async fn user_identities(
    pool: &SqlitePool,
    user_id: Uuid,
) -> Result<Vec<String>, MetaError> {
    sqlx::query_scalar("SELECT provider FROM identities WHERE user_id = ? ORDER BY provider")
        .bind(user_id.to_string())
        .fetch_all(pool)
        .await
        .map_err(MetaError::from)
}

fn parse_user(value: &str) -> Result<Uuid, MetaError> {
    Uuid::parse_str(value).map_err(|_| MetaError::Corrupt {
        table: "identities",
        column: "user_id",
    })
}
