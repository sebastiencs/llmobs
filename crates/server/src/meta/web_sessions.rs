//! `web_sessions` rows (spec 04, spec 08).
//!
//! `id` is the SHA-256 hex of the cookie value. The plaintext never lands here.
#![cfg_attr(not(test), allow(dead_code))]

use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::MetaError;
use super::users::UserRow;

/// Columns for [`insert_session`]. `hash` is the cookie's SHA-256 hex.
#[derive(Debug)]
pub(crate) struct NewSession {
    /// SHA-256 hex of the cookie value.
    pub(crate) hash: String,
    /// The signed-in user.
    pub(crate) user_id: Uuid,
    /// `User-Agent`, already clipped.
    pub(crate) user_agent: Option<String>,
    /// Client IP of D142, as text.
    pub(crate) ip: Option<String>,
}

/// A session that is still valid, with its user.
///
/// `Debug` omits the hash and the user's email.
pub(crate) struct SessionRow {
    /// SHA-256 hex of the cookie value.
    pub(crate) hash: String,
    /// The user. `disabled_at` is unset: a disabled user is not returned.
    pub(crate) user: UserRow,
    /// Last slide, nanoseconds.
    pub(crate) last_seen_at: i64,
    /// When the session stops resolving, nanoseconds. `expires_at <= now` is gone.
    pub(crate) expires_at: i64,
}

impl std::fmt::Debug for SessionRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionRow")
            .field("user_id", &self.user.id)
            .field("last_seen_at", &self.last_seen_at)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, sqlx::FromRow)]
struct SessionRecord {
    session_id: String,
    last_seen_at: i64,
    expires_at: i64,
    user_id: String,
    email: String,
    name: Option<String>,
    password_hash: Option<String>,
    email_verified_at: Option<i64>,
    is_instance_admin: i64,
    disabled_at: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

/// Inserts a session. `user_agent` is cut to 256 characters.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the insert fails.
pub(crate) async fn insert_session(
    conn: &mut SqliteConnection,
    session: NewSession,
    now: i64,
    expires_at: i64,
) -> Result<(), MetaError> {
    sqlx::query(
        "INSERT INTO web_sessions \
         (id, user_id, created_at, last_seen_at, expires_at, user_agent, ip) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&session.hash)
    .bind(session.user_id.to_string())
    .bind(now)
    .bind(now)
    .bind(expires_at)
    .bind(clip_user_agent(session.user_agent.as_deref()))
    .bind(&session.ip)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The session and its user, when the session is unexpired and the user is enabled.
///
/// `expires_at <= now` and a set `disabled_at` are both `Ok(None)`.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored id or flag does not decode.
pub(crate) async fn session_user(
    pool: &SqlitePool,
    hash: &str,
    now: i64,
) -> Result<Option<SessionRow>, MetaError> {
    let record = sqlx::query_as::<_, SessionRecord>(
        "SELECT web_sessions.id AS session_id, web_sessions.last_seen_at, \
         web_sessions.expires_at, users.id AS user_id, users.email, users.name, \
         users.password_hash, users.email_verified_at, users.is_instance_admin, \
         users.disabled_at, users.created_at, users.updated_at \
         FROM web_sessions \
         JOIN users ON users.id = web_sessions.user_id \
         WHERE web_sessions.id = ? AND web_sessions.expires_at > ? \
         AND users.disabled_at IS NULL",
    )
    .bind(hash)
    .bind(now)
    .fetch_optional(pool)
    .await?;
    record.map(session_row).transpose()
}

/// Slides `last_seen_at` and `expires_at`.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn touch_session(
    conn: &mut SqliteConnection,
    hash: &str,
    now: i64,
    expires_at: i64,
) -> Result<(), MetaError> {
    sqlx::query("UPDATE web_sessions SET last_seen_at = ?, expires_at = ? WHERE id = ?")
        .bind(now)
        .bind(expires_at)
        .bind(hash)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Deletes one session.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the delete fails.
pub(crate) async fn delete_session(
    conn: &mut SqliteConnection,
    hash: &str,
) -> Result<(), MetaError> {
    sqlx::query("DELETE FROM web_sessions WHERE id = ?")
        .bind(hash)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Deletes every session of `user_id`. `keep` is a session hash that stays.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the delete fails.
pub(crate) async fn delete_user_sessions(
    conn: &mut SqliteConnection,
    user_id: Uuid,
    keep: Option<&str>,
) -> Result<(), MetaError> {
    if let Some(keep) = keep {
        sqlx::query("DELETE FROM web_sessions WHERE user_id = ? AND id != ?")
            .bind(user_id.to_string())
            .bind(keep)
            .execute(&mut *conn)
            .await?;
    } else {
        sqlx::query("DELETE FROM web_sessions WHERE user_id = ?")
            .bind(user_id.to_string())
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
}

/// Deletes sessions with `expires_at <= now`. Returns how many rows went.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the delete fails.
pub(crate) async fn purge_expired_sessions(
    conn: &mut SqliteConnection,
    now: i64,
) -> Result<u64, MetaError> {
    let removed = sqlx::query("DELETE FROM web_sessions WHERE expires_at <= ?")
        .bind(now)
        .execute(&mut *conn)
        .await?
        .rows_affected();
    Ok(removed)
}

fn clip_user_agent(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let clipped: String = raw.chars().take(256).collect();
    if clipped.is_empty() {
        None
    } else {
        Some(clipped)
    }
}

fn session_row(record: SessionRecord) -> Result<SessionRow, MetaError> {
    let user_id = Uuid::parse_str(&record.user_id).map_err(|_| MetaError::Corrupt {
        table: "users",
        column: "id",
    })?;
    let is_instance_admin = match record.is_instance_admin {
        0 => false,
        1 => true,
        _ => {
            return Err(MetaError::Corrupt {
                table: "users",
                column: "is_instance_admin",
            });
        }
    };
    Ok(SessionRow {
        hash: record.session_id,
        user: UserRow {
            id: user_id,
            email: record.email,
            name: record.name,
            password_hash: record.password_hash,
            email_verified_at: record.email_verified_at,
            is_instance_admin,
            disabled_at: record.disabled_at,
            created_at: record.created_at,
            updated_at: record.updated_at,
        },
        last_seen_at: record.last_seen_at,
        expires_at: record.expires_at,
    })
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use super::*;
    use crate::meta::test_db;
    use crate::meta::users::{NewUser, insert_user};

    async fn user(db: &crate::meta::MetaDb, email: &str, now: i64) -> Uuid {
        let email = email.to_owned();
        db.write(move |conn| {
            Box::pin(async move {
                let row = insert_user(
                    conn,
                    NewUser {
                        email,
                        name: None,
                        password_hash: None,
                        email_verified_at: None,
                        is_instance_admin: false,
                    },
                    now,
                )
                .await?;
                Ok(row.id)
            })
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn user_agent_is_clipped_and_ip_is_text() {
        let (_dir, db) = test_db().await;
        let now = 10;
        let user_id = user(&db, "ada@example.com", now).await;
        let agent: String = "a".repeat(255) + "é" + "zzz";
        let ip: IpAddr = "192.0.2.10".parse().unwrap();
        db.write(move |conn| {
            Box::pin(async move {
                insert_session(
                    conn,
                    NewSession {
                        hash: "hash-1".to_owned(),
                        user_id,
                        user_agent: Some(agent),
                        ip: Some(ip.to_string()),
                    },
                    now,
                    now + 1,
                )
                .await
            })
        })
        .await
        .unwrap();
        let (stored_agent, stored_ip): (String, String) =
            sqlx::query_as("SELECT user_agent, ip FROM web_sessions")
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(stored_agent.chars().count(), 256);
        assert!(stored_agent.ends_with('é'));
        assert_eq!(stored_ip, "192.0.2.10");
    }

    #[tokio::test]
    async fn keep_one_session_and_purge_counts_expired_rows() {
        let (_dir, db) = test_db().await;
        let now = 100;
        let user_id = user(&db, "ada@example.com", now).await;
        db.write(move |conn| {
            Box::pin(async move {
                for (hash, expires_at) in [("keep", now + 5), ("drop", now - 1), ("other", now)] {
                    insert_session(
                        conn,
                        NewSession {
                            hash: hash.to_owned(),
                            user_id,
                            user_agent: None,
                            ip: None,
                        },
                        now,
                        expires_at,
                    )
                    .await?;
                }
                delete_user_sessions(conn, user_id, Some("keep")).await?;
                Ok(())
            })
        })
        .await
        .unwrap();

        let left: Vec<String> = sqlx::query_scalar("SELECT id FROM web_sessions ORDER BY id")
            .fetch_all(db.reader())
            .await
            .unwrap();
        assert_eq!(left, vec!["keep".to_owned()]);

        let removed = db
            .write(move |conn| Box::pin(async move { purge_expired_sessions(conn, now).await }))
            .await
            .unwrap();
        assert_eq!(removed, 0);

        db.write(move |conn| {
            Box::pin(async move {
                insert_session(
                    conn,
                    NewSession {
                        hash: "stale".to_owned(),
                        user_id,
                        user_agent: None,
                        ip: None,
                    },
                    now,
                    now,
                )
                .await?;
                insert_session(
                    conn,
                    NewSession {
                        hash: "live".to_owned(),
                        user_id,
                        user_agent: None,
                        ip: None,
                    },
                    now,
                    now + 1,
                )
                .await
            })
        })
        .await
        .unwrap();
        let removed = db
            .write(move |conn| Box::pin(async move { purge_expired_sessions(conn, now).await }))
            .await
            .unwrap();
        assert_eq!(removed, 1);
        let found = session_user(db.reader(), "live", now)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(found.user.id, user_id);
        assert!(
            session_user(db.reader(), "stale", now)
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            session_user(db.reader(), "keep", now)
                .await
                .unwrap()
                .is_some()
        );

        db.write(move |conn| Box::pin(async move { delete_session(conn, "live").await }))
            .await
            .unwrap();
        assert!(
            session_user(db.reader(), "live", now)
                .await
                .unwrap()
                .is_none()
        );
    }
}
