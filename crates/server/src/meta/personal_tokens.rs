//! `personal_tokens` rows (spec 04).
#![cfg_attr(not(test), allow(dead_code))]

use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::{MetaError, unique_or};
use crate::auth::roles::KeyScope;

/// A stored personal access token. The plaintext is not a column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TokenRow {
    /// UUIDv7.
    pub(crate) id: Uuid,
    /// Owner.
    pub(crate) user_id: Uuid,
    /// Display name.
    pub(crate) name: String,
    /// Type prefix plus the first 6 tail characters.
    pub(crate) prefix: String,
    /// SHA-256 of the plaintext, lowercase hex.
    pub(crate) hash: String,
    /// `read`, or `read` then `write`.
    pub(crate) scopes: Vec<KeyScope>,
    /// Creation time, nanoseconds.
    pub(crate) created_at: i64,
    /// When the token stops working. Absent means it does not expire.
    pub(crate) expires_at: Option<i64>,
    /// Last successful authentication. Absent until a flush.
    pub(crate) last_used_at: Option<i64>,
    /// When the token was revoked. Absent while it is live.
    pub(crate) revoked_at: Option<i64>,
}

/// Columns for [`insert_token`].
pub(crate) struct NewToken {
    /// Owner.
    pub(crate) user_id: Uuid,
    /// Display name, already checked.
    pub(crate) name: String,
    /// Type prefix plus the first 6 tail characters.
    pub(crate) prefix: String,
    /// SHA-256 of the plaintext, lowercase hex.
    pub(crate) hash: String,
    /// `read`, or `read` then `write`.
    pub(crate) scopes: Vec<KeyScope>,
    /// When the token stops working.
    pub(crate) expires_at: Option<i64>,
}

/// Inserts a token. A duplicate hash is [`MetaError::Conflict`] `"duplicate token"`.
///
/// # Errors
///
/// Returns [`MetaError`] when the insert fails.
pub(crate) async fn insert_token(
    conn: &mut SqliteConnection,
    token: NewToken,
    now: i64,
) -> Result<TokenRow, MetaError> {
    let id = Uuid::now_v7();
    let scopes = scopes_json(&token.scopes);
    sqlx::query(
        "INSERT INTO personal_tokens \
         (id, user_id, name, prefix, token_hash, scopes, created_at, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(token.user_id.to_string())
    .bind(&token.name)
    .bind(&token.prefix)
    .bind(&token.hash)
    .bind(&scopes)
    .bind(now)
    .bind(token.expires_at)
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, "duplicate token"))?;
    Ok(TokenRow {
        id,
        user_id: token.user_id,
        name: token.name,
        prefix: token.prefix,
        hash: token.hash,
        scopes: token.scopes,
        created_at: now,
        expires_at: token.expires_at,
        last_used_at: None,
        revoked_at: None,
    })
}

/// Live tokens of `user_id`, newest first.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn list_user_tokens(
    pool: &SqlitePool,
    user_id: Uuid,
) -> Result<Vec<TokenRow>, MetaError> {
    let records: Vec<TokenRecord> = sqlx::query_as(
        "SELECT id, user_id, name, prefix, token_hash, scopes, created_at, expires_at, \
         last_used_at, revoked_at \
         FROM personal_tokens WHERE user_id = ? AND revoked_at IS NULL \
         ORDER BY created_at DESC, id DESC",
    )
    .bind(user_id.to_string())
    .fetch_all(pool)
    .await?;
    records.into_iter().map(row_from).collect()
}

/// The token with this hash, including a revoked one.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn token_by_hash(
    pool: &SqlitePool,
    hash: &str,
) -> Result<Option<TokenRow>, MetaError> {
    let record: Option<TokenRecord> = sqlx::query_as(
        "SELECT id, user_id, name, prefix, token_hash, scopes, created_at, expires_at, \
         last_used_at, revoked_at \
         FROM personal_tokens WHERE token_hash = ?",
    )
    .bind(hash)
    .fetch_optional(pool)
    .await?;
    record.map(row_from).transpose()
}

/// Revokes one live token of `user_id`. `None` means it is missing or already revoked.
///
/// The string is the token hash, so the caller can drop the cache entry.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn revoke_token(
    conn: &mut SqliteConnection,
    user_id: Uuid,
    id: Uuid,
    now: i64,
) -> Result<Option<String>, MetaError> {
    let row: Option<HashRecord> = sqlx::query_as(
        "UPDATE personal_tokens SET revoked_at = ? \
         WHERE user_id = ? AND id = ? AND revoked_at IS NULL \
         RETURNING token_hash",
    )
    .bind(now)
    .bind(user_id.to_string())
    .bind(id.to_string())
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|row| row.token_hash))
}

/// Sets `revoked_at` on every live personal access token of `user_id`.
///
/// A token that is already revoked keeps its original `revoked_at`. The returned
/// hashes are the ones this update revoked.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn revoke_user_tokens(
    conn: &mut SqliteConnection,
    user_id: Uuid,
    now: i64,
) -> Result<Vec<String>, MetaError> {
    let rows: Vec<HashRecord> = sqlx::query_as(
        "UPDATE personal_tokens SET revoked_at = ? \
         WHERE user_id = ? AND revoked_at IS NULL \
         RETURNING token_hash",
    )
    .bind(now)
    .bind(user_id.to_string())
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.into_iter().map(|row| row.token_hash).collect())
}

/// Sets `last_used_at` when the stored value is absent or earlier. One statement per row.
///
/// The caller holds the transaction.
///
/// # Errors
///
/// Returns [`MetaError`] when an update fails.
pub(crate) async fn set_last_used(
    conn: &mut SqliteConnection,
    rows: &[(Uuid, i64)],
) -> Result<(), MetaError> {
    for (id, at) in rows {
        sqlx::query(
            "UPDATE personal_tokens SET last_used_at = ? \
             WHERE id = ? AND (last_used_at IS NULL OR last_used_at < ?)",
        )
        .bind(at)
        .bind(id.to_string())
        .bind(at)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct TokenRecord {
    id: String,
    user_id: String,
    name: Option<String>,
    prefix: String,
    token_hash: String,
    scopes: String,
    created_at: i64,
    expires_at: Option<i64>,
    last_used_at: Option<i64>,
    revoked_at: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
struct HashRecord {
    token_hash: String,
}

fn row_from(record: TokenRecord) -> Result<TokenRow, MetaError> {
    let name = record.name.ok_or(MetaError::Corrupt {
        table: "personal_tokens",
        column: "name",
    })?;
    Ok(TokenRow {
        id: parse_uuid(&record.id, "id")?,
        user_id: parse_uuid(&record.user_id, "user_id")?,
        name,
        prefix: record.prefix,
        hash: record.token_hash,
        scopes: parse_scopes(&record.scopes)?,
        created_at: record.created_at,
        expires_at: record.expires_at,
        last_used_at: record.last_used_at,
        revoked_at: record.revoked_at,
    })
}

fn parse_uuid(value: &str, column: &'static str) -> Result<Uuid, MetaError> {
    value.parse().map_err(|_| MetaError::Corrupt {
        table: "personal_tokens",
        column,
    })
}

fn parse_scopes(text: &str) -> Result<Vec<KeyScope>, MetaError> {
    let names: Vec<String> = serde_json::from_str(text).map_err(|_| MetaError::Corrupt {
        table: "personal_tokens",
        column: "scopes",
    })?;
    names
        .iter()
        .map(|name| match name.as_str() {
            "read" => Ok(KeyScope::Read),
            "write" => Ok(KeyScope::Write),
            _ => Err(MetaError::Corrupt {
                table: "personal_tokens",
                column: "scopes",
            }),
        })
        .collect()
}

fn scopes_json(scopes: &[KeyScope]) -> String {
    let mut out = String::from("[");
    for (index, scope) in scopes.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(scope_name(*scope));
        out.push('"');
    }
    out.push(']');
    out
}

fn scope_name(scope: KeyScope) -> &'static str {
    match scope {
        KeyScope::Read => "read",
        KeyScope::Write => "write",
        KeyScope::Ingest => "ingest",
        KeyScope::Feedback => "feedback",
    }
}
