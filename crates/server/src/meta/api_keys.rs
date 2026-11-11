//! `api_keys` rows (spec 04).
#![cfg_attr(not(test), allow(dead_code))]

use llmobs_core::ids::ProjectId;
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::{MetaError, unique_or};
use crate::auth::roles::KeyScope;
use crate::auth::secret::KeyKind;

/// A stored project API key. The plaintext is not a column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApiKeyRow {
    /// UUIDv7.
    pub(crate) id: Uuid,
    /// Owning project.
    pub(crate) project_id: ProjectId,
    /// `secret` or `public`.
    pub(crate) kind: String,
    /// Scopes, in the order they were stored.
    pub(crate) scopes: Vec<KeyScope>,
    /// Display name. Absent when the key was created without one.
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
    /// Last successful authentication. Absent until a flush.
    pub(crate) last_used_at: Option<i64>,
    /// When the key was revoked. Absent while it is live.
    pub(crate) revoked_at: Option<i64>,
}

/// A key loaded for authentication, including whether its project and org are active.
pub(crate) struct LoadedApiKey {
    /// The key row, including a revoked one.
    pub(crate) key: ApiKeyRow,
    /// The project and its org are both `active`.
    pub(crate) active: bool,
}

/// Columns for [`insert_api_key`].
pub(crate) struct NewApiKey {
    /// Owning project.
    pub(crate) project_id: ProjectId,
    /// `secret` or `public`.
    pub(crate) kind: &'static str,
    /// Scopes, stored as a JSON array of lowercase names.
    pub(crate) scopes: Vec<KeyScope>,
    /// Display name. `None` is stored as NULL.
    pub(crate) name: Option<String>,
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
        scopes: key.scopes,
        name: key.name,
        prefix: key.prefix,
        hash: key.hash,
        created_by: Some(key.created_by),
        created_at: now,
        expires_at: key.expires_at,
        last_used_at: None,
        revoked_at: None,
    })
}

/// Live keys of `project_id`, newest first.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn list_api_keys(
    pool: &SqlitePool,
    project_id: ProjectId,
) -> Result<Vec<ApiKeyRow>, MetaError> {
    let records: Vec<KeyRecord> = sqlx::query_as(
        "SELECT id, project_id, kind, scopes, name, prefix, key_hash, created_by, created_at, \
         expires_at, last_used_at, revoked_at \
         FROM api_keys WHERE project_id = ? AND revoked_at IS NULL \
         ORDER BY created_at DESC, id DESC",
    )
    .bind(project_id.to_string())
    .fetch_all(pool)
    .await?;
    records.into_iter().map(row_from).collect()
}

/// The key with this hash, including a revoked key, joined to project and org state.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn api_key_by_hash(
    pool: &SqlitePool,
    hash: &str,
) -> Result<Option<LoadedApiKey>, MetaError> {
    let record: Option<AuthRecord> = sqlx::query_as(
        "SELECT k.id, k.project_id, k.kind, k.scopes, k.name, k.prefix, k.key_hash, k.created_by, \
         k.created_at, k.expires_at, k.last_used_at, k.revoked_at, p.state AS project_state, \
         o.state AS org_state \
         FROM api_keys k \
         JOIN projects p ON p.id = k.project_id \
         JOIN orgs o ON o.id = p.org_id \
         WHERE k.key_hash = ?",
    )
    .bind(hash)
    .fetch_optional(pool)
    .await?;
    record.map(loaded_from).transpose()
}

/// The live key `id` in `project_id`.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn api_key_by_id(
    pool: &SqlitePool,
    project_id: ProjectId,
    id: Uuid,
) -> Result<Option<ApiKeyRow>, MetaError> {
    let record: Option<KeyRecord> = sqlx::query_as(
        "SELECT id, project_id, kind, scopes, name, prefix, key_hash, created_by, created_at, \
         expires_at, last_used_at, revoked_at \
         FROM api_keys WHERE project_id = ? AND id = ? AND revoked_at IS NULL",
    )
    .bind(project_id.to_string())
    .bind(id.to_string())
    .fetch_optional(pool)
    .await?;
    record.map(row_from).transpose()
}

/// Revokes the live key. `None` means it is missing, already revoked, or in another project.
///
/// The returned hash is the cache key. The caller evicts it after the commit.
///
/// # Errors
///
/// Returns [`MetaError`] when the update fails.
pub(crate) async fn revoke_api_key(
    conn: &mut SqliteConnection,
    project_id: ProjectId,
    id: Uuid,
    now: i64,
) -> Result<Option<String>, MetaError> {
    let row: Option<HashRecord> = sqlx::query_as(
        "UPDATE api_keys SET revoked_at = ? \
         WHERE project_id = ? AND id = ? AND revoked_at IS NULL \
         RETURNING key_hash",
    )
    .bind(now)
    .bind(project_id.to_string())
    .bind(id.to_string())
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|row| row.key_hash))
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
            "UPDATE api_keys SET last_used_at = ? \
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

/// The key kind stored in `kind`.
///
/// # Errors
///
/// Returns [`MetaError::Corrupt`] when `kind` is not `secret` or `public`.
pub(crate) fn stored_kind(kind: &str) -> Result<KeyKind, MetaError> {
    match kind {
        "secret" => Ok(KeyKind::Secret),
        "public" => Ok(KeyKind::Public),
        _ => Err(MetaError::Corrupt {
            table: "api_keys",
            column: "kind",
        }),
    }
}

#[derive(Debug, sqlx::FromRow)]
struct KeyRecord {
    id: String,
    project_id: String,
    kind: String,
    scopes: String,
    name: Option<String>,
    prefix: String,
    key_hash: String,
    created_by: Option<String>,
    created_at: i64,
    expires_at: Option<i64>,
    last_used_at: Option<i64>,
    revoked_at: Option<i64>,
}

#[derive(Debug, sqlx::FromRow)]
struct AuthRecord {
    id: String,
    project_id: String,
    kind: String,
    scopes: String,
    name: Option<String>,
    prefix: String,
    key_hash: String,
    created_by: Option<String>,
    created_at: i64,
    expires_at: Option<i64>,
    last_used_at: Option<i64>,
    revoked_at: Option<i64>,
    project_state: String,
    org_state: String,
}

#[derive(Debug, sqlx::FromRow)]
struct HashRecord {
    key_hash: String,
}

fn loaded_from(record: AuthRecord) -> Result<LoadedApiKey, MetaError> {
    let active = record.project_state == "active" && record.org_state == "active";
    Ok(LoadedApiKey {
        key: row_from(KeyRecord {
            id: record.id,
            project_id: record.project_id,
            kind: record.kind,
            scopes: record.scopes,
            name: record.name,
            prefix: record.prefix,
            key_hash: record.key_hash,
            created_by: record.created_by,
            created_at: record.created_at,
            expires_at: record.expires_at,
            last_used_at: record.last_used_at,
            revoked_at: record.revoked_at,
        })?,
        active,
    })
}

fn row_from(record: KeyRecord) -> Result<ApiKeyRow, MetaError> {
    Ok(ApiKeyRow {
        id: parse_uuid(&record.id, "id")?,
        project_id: record.project_id.parse().map_err(|_| MetaError::Corrupt {
            table: "api_keys",
            column: "project_id",
        })?,
        kind: record.kind,
        scopes: parse_scopes(&record.scopes)?,
        name: record.name,
        prefix: record.prefix,
        hash: record.key_hash,
        created_by: match record.created_by {
            Some(value) => Some(parse_uuid(&value, "created_by")?),
            None => None,
        },
        created_at: record.created_at,
        expires_at: record.expires_at,
        last_used_at: record.last_used_at,
        revoked_at: record.revoked_at,
    })
}

fn parse_uuid(value: &str, column: &'static str) -> Result<Uuid, MetaError> {
    value.parse().map_err(|_| MetaError::Corrupt {
        table: "api_keys",
        column,
    })
}

fn parse_scopes(text: &str) -> Result<Vec<KeyScope>, MetaError> {
    let names: Vec<String> = serde_json::from_str(text).map_err(|_| MetaError::Corrupt {
        table: "api_keys",
        column: "scopes",
    })?;
    names
        .iter()
        .map(|name| match name.as_str() {
            "ingest" => Ok(KeyScope::Ingest),
            "read" => Ok(KeyScope::Read),
            "write" => Ok(KeyScope::Write),
            "feedback" => Ok(KeyScope::Feedback),
            _ => Err(MetaError::Corrupt {
                table: "api_keys",
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
        KeyScope::Ingest => "ingest",
        KeyScope::Read => "read",
        KeyScope::Write => "write",
        KeyScope::Feedback => "feedback",
    }
}
