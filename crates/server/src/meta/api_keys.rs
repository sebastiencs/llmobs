//! `api_keys` rows (spec 04). P16 adds lookup and revocation.
#![cfg_attr(not(test), allow(dead_code))]

use llmobs_core::ids::ProjectId;
use sqlx::SqliteConnection;
use uuid::Uuid;

use super::{MetaError, unique_or};
use crate::auth::roles::KeyScope;

/// A newly stored project API key. The plaintext is not a column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiKeyRow {
    /// UUIDv7.
    pub(crate) id: Uuid,
    /// Owning project.
    pub(crate) project_id: ProjectId,
    /// `secret` or `public`.
    pub(crate) kind: String,
    /// JSON array of scope names.
    pub(crate) scopes: String,
    /// Display name.
    pub(crate) name: Option<String>,
    /// Type prefix plus the first 6 tail characters.
    pub(crate) prefix: String,
    /// SHA-256 of the plaintext, lowercase hex.
    pub(crate) hash: String,
    /// User who created the key.
    pub(crate) created_by: Option<Uuid>,
    /// Creation time, nanoseconds.
    pub(crate) created_at: i64,
    /// When the key stops working. Absent means it does not expire.
    pub(crate) expires_at: Option<i64>,
}

/// Columns for [`insert_api_key`].
pub(crate) struct NewApiKey {
    /// Owning project.
    pub(crate) project_id: ProjectId,
    /// `secret` or `public`.
    pub(crate) kind: &'static str,
    /// Scopes, stored as a JSON array of lowercase names.
    pub(crate) scopes: Vec<KeyScope>,
    /// Display name.
    pub(crate) name: String,
    /// Type prefix plus the first 6 tail characters.
    pub(crate) prefix: String,
    /// SHA-256 of the plaintext, lowercase hex.
    pub(crate) hash: String,
    /// User who created the key.
    pub(crate) created_by: Uuid,
    /// When the key stops working.
    pub(crate) expires_at: Option<i64>,
}

/// Inserts a key. A duplicate hash is [`MetaError::Conflict`] `"duplicate key"`.
///
/// # Errors
///
/// Returns [`MetaError`] when the insert fails.
pub(crate) async fn insert_api_key(
    conn: &mut SqliteConnection,
    key: NewApiKey,
    now: i64,
) -> Result<ApiKeyRow, MetaError> {
    let id = Uuid::now_v7();
    let scopes = scopes_json(&key.scopes);
    sqlx::query(
        "INSERT INTO api_keys \
         (id, project_id, kind, scopes, name, prefix, key_hash, created_by, created_at, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(key.project_id.to_string())
    .bind(key.kind)
    .bind(&scopes)
    .bind(&key.name)
    .bind(&key.prefix)
    .bind(&key.hash)
    .bind(key.created_by.to_string())
    .bind(now)
    .bind(key.expires_at)
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, "duplicate key"))?;
    Ok(ApiKeyRow {
        id,
        project_id: key.project_id,
        kind: key.kind.to_owned(),
        scopes,
        name: Some(key.name),
        prefix: key.prefix,
        hash: key.hash,
        created_by: Some(key.created_by),
        created_at: now,
        expires_at: key.expires_at,
    })
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
        KeyScope::Ingest => "ingest",
        KeyScope::Read => "read",
        KeyScope::Write => "write",
        KeyScope::Feedback => "feedback",
    }
}
