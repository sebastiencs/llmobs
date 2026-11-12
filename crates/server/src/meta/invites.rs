//! `invites` rows (spec 04, spec 08, D147).
//!
//! The table stores the SHA-256 hex of the accept token. The plaintext is only
//! in the accept URL returned to the caller and, when mail is configured, in
//! the message.

use llmobs_core::ids::OrgId;
use sqlx::{SqliteConnection, SqliteExecutor, SqlitePool};
use uuid::Uuid;

use super::orgs::OrgRow;
use super::{MetaError, unique_or};
use crate::auth::{OrgRole, org_role_str, parse_org_role};

/// How long an invitation stays pending. Seven days, in nanoseconds.
pub(crate) const INVITE_TTL_NS: i64 = 7 * 24 * 60 * 60 * 1_000_000_000i64;

/// An `invites` row. `token_hash` is not loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InviteRow {
    /// UUIDv7.
    pub(crate) id: Uuid,
    /// The org.
    pub(crate) org_id: OrgId,
    /// Normalized email.
    pub(crate) email: String,
    /// Role granted on accept.
    pub(crate) role: OrgRole,
    /// Creation time, nanoseconds.
    pub(crate) created_at: i64,
    /// Expiry time, nanoseconds. Equal to `now` is already expired.
    pub(crate) expires_at: i64,
    /// Accept time. Absent while the invitation is pending.
    pub(crate) accepted_at: Option<i64>,
}

/// Who sent an invitation, when that user still exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Inviter {
    /// `users.id`.
    pub(crate) id: Uuid,
    /// The inviter's email.
    pub(crate) email: String,
    /// Display name. Absent when the user has not set one.
    pub(crate) name: Option<String>,
}

/// A pending invitation and its inviter, for the org list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingInvite {
    /// The invitation.
    pub(crate) invite: InviteRow,
    /// The inviter. Absent when the user row is gone.
    pub(crate) invited_by: Option<Inviter>,
}

const JOIN_PENDING: &str = "\
SELECT invites.id, invites.org_id, invites.email, invites.role, invites.created_at, \
       invites.expires_at, invites.accepted_at, \
       orgs.name AS org_name, orgs.slug AS org_slug, orgs.state AS org_state, \
       orgs.created_at AS org_created_at, orgs.updated_at AS org_updated_at \
FROM invites JOIN orgs ON orgs.id = invites.org_id \
WHERE invites.token_hash = ? AND invites.accepted_at IS NULL AND invites.expires_at > ?";

const JOIN_UNEXPIRED: &str = "\
SELECT invites.id, invites.org_id, invites.email, invites.role, invites.created_at, \
       invites.expires_at, invites.accepted_at, \
       orgs.name AS org_name, orgs.slug AS org_slug, orgs.state AS org_state, \
       orgs.created_at AS org_created_at, orgs.updated_at AS org_updated_at \
FROM invites JOIN orgs ON orgs.id = invites.org_id \
WHERE invites.token_hash = ? AND invites.expires_at > ?";

/// Deletes the pending invitation for `(org, email)`, then inserts one.
///
/// The caller runs this inside a transaction. `email` is already normalized.
///
/// # Errors
///
/// Returns [`MetaError`] when the write fails.
#[allow(clippy::too_many_arguments)] // the columns of one insert
pub(crate) async fn replace_invite(
    conn: &mut SqliteConnection,
    org_id: OrgId,
    email: &str,
    role: OrgRole,
    hash: &str,
    invited_by: Uuid,
    now: i64,
    expires_at: i64,
) -> Result<InviteRow, MetaError> {
    debug_assert!(
        crate::meta::users::normalize_email(email).ok().as_deref() == Some(email),
        "email is not normalized"
    );
    sqlx::query("DELETE FROM invites WHERE org_id = ? AND email = ? AND accepted_at IS NULL")
        .bind(org_id.to_string())
        .bind(email)
        .execute(&mut *conn)
        .await?;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO invites \
         (id, org_id, email, role, token_hash, invited_by, created_at, expires_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(org_id.to_string())
    .bind(email)
    .bind(org_role_str(role))
    .bind(hash)
    .bind(invited_by.to_string())
    .bind(now)
    .bind(expires_at)
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, "invite already exists"))?;
    Ok(InviteRow {
        id,
        org_id,
        email: email.to_owned(),
        role,
        created_at: now,
        expires_at,
        accepted_at: None,
    })
}

/// Pending, unexpired invitations for one org, ordered by email then id.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored id or role is corrupt.
pub(crate) async fn pending_invites(
    pool: &SqlitePool,
    org_id: OrgId,
    now: i64,
) -> Result<Vec<PendingInvite>, MetaError> {
    let records: Vec<PendingRecord> = sqlx::query_as(
        "SELECT invites.id, invites.org_id, invites.email, invites.role, invites.created_at, \
                invites.expires_at, users.id AS inviter_id, users.email AS inviter_email, \
                users.name AS inviter_name \
         FROM invites LEFT JOIN users ON users.id = invites.invited_by \
         WHERE invites.org_id = ? AND invites.accepted_at IS NULL AND invites.expires_at > ? \
         ORDER BY invites.email, invites.id",
    )
    .bind(org_id.to_string())
    .bind(now)
    .fetch_all(pool)
    .await?;
    records.into_iter().map(pending_invite).collect()
}

/// The pending, unexpired invitation with this token hash, and its org.
///
/// An accepted or expired invitation is `Ok(None)`.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored id or role is corrupt.
pub(crate) async fn invite_by_hash(
    pool: &SqlitePool,
    hash: &str,
    now: i64,
) -> Result<Option<(InviteRow, OrgRow)>, MetaError> {
    fetch_invite(pool, JOIN_PENDING, hash, now).await
}

/// [`invite_by_hash`] on the writer connection.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored id or role is corrupt.
pub(crate) async fn invite_by_hash_tx(
    conn: &mut SqliteConnection,
    hash: &str,
    now: i64,
) -> Result<Option<(InviteRow, OrgRow)>, MetaError> {
    fetch_invite(&mut *conn, JOIN_PENDING, hash, now).await
}

/// The unexpired invitation with this token hash, accepted or not.
///
/// A second accept of a still-valid invitation finds the row here. An expired
/// invitation is `Ok(None)`.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored id or role is corrupt.
pub(crate) async fn unexpired_invite_tx(
    conn: &mut SqliteConnection,
    hash: &str,
    now: i64,
) -> Result<Option<(InviteRow, OrgRow)>, MetaError> {
    fetch_invite(&mut *conn, JOIN_UNEXPIRED, hash, now).await
}

/// Deletes one invitation in this org. `false` when no row had that id.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the delete fails.
pub(crate) async fn delete_invite(
    conn: &mut SqliteConnection,
    org_id: OrgId,
    id: Uuid,
) -> Result<bool, MetaError> {
    let result = sqlx::query("DELETE FROM invites WHERE org_id = ? AND id = ?")
        .bind(org_id.to_string())
        .bind(id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Sets `accepted_at` when it is still null. A second call changes nothing.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn mark_accepted(
    conn: &mut SqliteConnection,
    id: Uuid,
    now: i64,
) -> Result<(), MetaError> {
    sqlx::query("UPDATE invites SET accepted_at = ? WHERE id = ? AND accepted_at IS NULL")
        .bind(now)
        .bind(id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

async fn fetch_invite<'e, E>(
    executor: E,
    sql: &'static str,
    hash: &str,
    now: i64,
) -> Result<Option<(InviteRow, OrgRow)>, MetaError>
where
    E: SqliteExecutor<'e>,
{
    let record: Option<JoinedRecord> = sqlx::query_as(sql)
        .bind(hash)
        .bind(now)
        .fetch_optional(executor)
        .await?;
    record.map(invite_and_org).transpose()
}

#[derive(Debug, sqlx::FromRow)]
struct JoinedRecord {
    id: String,
    org_id: String,
    email: String,
    role: String,
    created_at: i64,
    expires_at: i64,
    accepted_at: Option<i64>,
    org_name: String,
    org_slug: String,
    org_state: String,
    org_created_at: i64,
    org_updated_at: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct PendingRecord {
    id: String,
    org_id: String,
    email: String,
    role: String,
    created_at: i64,
    expires_at: i64,
    inviter_id: Option<String>,
    inviter_email: Option<String>,
    inviter_name: Option<String>,
}

fn invite_and_org(record: JoinedRecord) -> Result<(InviteRow, OrgRow), MetaError> {
    let org_id = parse_org_id(&record.org_id)?;
    Ok((
        InviteRow {
            id: parse_uuid(&record.id, "invites", "id")?,
            org_id,
            email: record.email,
            role: decode_role(&record.role)?,
            created_at: record.created_at,
            expires_at: record.expires_at,
            accepted_at: record.accepted_at,
        },
        OrgRow {
            id: org_id,
            name: record.org_name,
            slug: record.org_slug,
            state: record.org_state,
            created_at: record.org_created_at,
            updated_at: record.org_updated_at,
        },
    ))
}

fn pending_invite(record: PendingRecord) -> Result<PendingInvite, MetaError> {
    let invited_by = match (record.inviter_id, record.inviter_email) {
        (None, _) => None,
        (Some(id), Some(email)) => Some(Inviter {
            id: parse_uuid(&id, "users", "id")?,
            email,
            name: record.inviter_name,
        }),
        (Some(_), None) => {
            return Err(MetaError::Corrupt {
                table: "users",
                column: "email",
            });
        }
    };
    Ok(PendingInvite {
        invite: InviteRow {
            id: parse_uuid(&record.id, "invites", "id")?,
            org_id: parse_org_id(&record.org_id)?,
            email: record.email,
            role: decode_role(&record.role)?,
            created_at: record.created_at,
            expires_at: record.expires_at,
            accepted_at: None,
        },
        invited_by,
    })
}

fn decode_role(value: &str) -> Result<OrgRole, MetaError> {
    parse_org_role(value).ok_or(MetaError::Corrupt {
        table: "invites",
        column: "role",
    })
}

fn parse_org_id(value: &str) -> Result<OrgId, MetaError> {
    Ok(OrgId::from_uuid(parse_uuid(value, "invites", "org_id")?))
}

fn parse_uuid(value: &str, table: &'static str, column: &'static str) -> Result<Uuid, MetaError> {
    Uuid::parse_str(value).map_err(|_| MetaError::Corrupt { table, column })
}
