//! `users` rows (spec 04).
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashMap;

use sqlx::{SqliteConnection, SqlitePool};
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use super::{MetaError, unique_or};

/// Why an email was rejected. The message does not contain the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum EmailError {
    /// The address failed the shape check.
    #[error("invalid email")]
    Invalid,
}

/// A `users` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UserRow {
    /// UUIDv7.
    pub(crate) id: Uuid,
    /// Already normalized.
    pub(crate) email: String,
    /// Display name. Absent for a nameless account.
    pub(crate) name: Option<String>,
    /// argon2id PHC string. Absent for an SSO-only user.
    pub(crate) password_hash: Option<String>,
    /// When the address was verified, nanoseconds.
    pub(crate) email_verified_at: Option<i64>,
    /// Instance admin.
    pub(crate) is_instance_admin: bool,
    /// When the account was disabled. Absent means active.
    pub(crate) disabled_at: Option<i64>,
    /// Creation time, nanoseconds.
    pub(crate) created_at: i64,
    /// Last change to this row, nanoseconds.
    pub(crate) updated_at: i64,
}

/// Columns for [`insert_user`]. `email` is already normalized.
#[derive(Debug, Clone)]
pub(crate) struct NewUser {
    /// Normalized email.
    pub(crate) email: String,
    /// Display name.
    pub(crate) name: Option<String>,
    /// Password hash.
    pub(crate) password_hash: Option<String>,
    /// Verification time, if the address is already verified.
    pub(crate) email_verified_at: Option<i64>,
    /// Instance admin.
    pub(crate) is_instance_admin: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct UserRecord {
    id: String,
    email: String,
    name: Option<String>,
    password_hash: Option<String>,
    email_verified_at: Option<i64>,
    is_instance_admin: i64,
    disabled_at: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

/// Trim, NFC, lowercase, then the shape check.
///
/// # Errors
///
/// Returns [`EmailError::Invalid`] when the result is not one local part and
/// one dotted domain inside the size limits, or when it contains whitespace
/// or a control character.
pub(crate) fn normalize_email(raw: &str) -> Result<String, EmailError> {
    let composed: String = raw.trim().nfc().collect();
    let email = composed.to_lowercase();
    if email.len() > 254
        || email
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err(EmailError::Invalid);
    }
    let Some((local, domain)) = email.split_once('@') else {
        return Err(EmailError::Invalid);
    };
    if domain.contains('@')
        || local.is_empty()
        || local.len() > 64
        || domain.is_empty()
        || !domain.contains('.')
        || domain.split('.').any(|label| label.is_empty())
    {
        return Err(EmailError::Invalid);
    }
    Ok(email)
}

/// Inserts a user. `email` must already be [`normalize_email`]'s output.
///
/// # Errors
///
/// A duplicate email is [`MetaError::Conflict`] `"email already registered"`.
pub(crate) async fn insert_user(
    conn: &mut SqliteConnection,
    user: NewUser,
    now: i64,
) -> Result<UserRow, MetaError> {
    debug_assert!(
        normalize_email(&user.email).ok().as_deref() == Some(user.email.as_str()),
        "email is not normalized"
    );
    let id = Uuid::now_v7();
    let id_text = id.to_string();
    let admin = i64::from(user.is_instance_admin);
    sqlx::query(
        "INSERT INTO users \
         (id, email, name, password_hash, email_verified_at, is_instance_admin, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id_text)
    .bind(&user.email)
    .bind(&user.name)
    .bind(&user.password_hash)
    .bind(user.email_verified_at)
    .bind(admin)
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, "email already registered"))?;
    Ok(UserRow {
        id,
        email: user.email,
        name: user.name,
        password_hash: user.password_hash,
        email_verified_at: user.email_verified_at,
        is_instance_admin: user.is_instance_admin,
        disabled_at: None,
        created_at: now,
        updated_at: now,
    })
}

/// The user with this id.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored flag is not 0 or 1.
pub(crate) async fn user_by_id(pool: &SqlitePool, id: Uuid) -> Result<Option<UserRow>, MetaError> {
    fetch_user(
        sqlx::query_as(
            "SELECT id, email, name, password_hash, email_verified_at, is_instance_admin, \
             disabled_at, created_at, updated_at FROM users WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(pool)
        .await?,
    )
}

/// The user with this normalized email.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored flag is not 0 or 1.
pub(crate) async fn user_by_email(
    pool: &SqlitePool,
    email: &str,
) -> Result<Option<UserRow>, MetaError> {
    fetch_user(
        sqlx::query_as(
            "SELECT id, email, name, password_hash, email_verified_at, is_instance_admin, \
             disabled_at, created_at, updated_at FROM users WHERE email = ?",
        )
        .bind(email)
        .fetch_optional(pool)
        .await?,
    )
}

/// How many users exist.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the query fails.
pub(crate) async fn count_users(pool: &SqlitePool) -> Result<i64, MetaError> {
    sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(pool)
        .await
        .map_err(MetaError::from)
}

/// [`count_users`] on the writer connection.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the query fails.
pub(crate) async fn count_users_tx(conn: &mut SqliteConnection) -> Result<i64, MetaError> {
    sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&mut *conn)
        .await
        .map_err(MetaError::from)
}

/// Replaces the password hash. `None` clears it.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn set_password_hash(
    conn: &mut SqliteConnection,
    id: Uuid,
    hash: Option<&str>,
    now: i64,
) -> Result<(), MetaError> {
    sqlx::query("UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?")
        .bind(hash)
        .bind(now)
        .bind(id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Marks the email verified at `now`.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn set_email_verified(
    conn: &mut SqliteConnection,
    id: Uuid,
    now: i64,
) -> Result<(), MetaError> {
    sqlx::query("UPDATE users SET email_verified_at = ?, updated_at = ? WHERE id = ?")
        .bind(now)
        .bind(now)
        .bind(id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Replaces the display name. `None` clears it.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn set_name(
    conn: &mut SqliteConnection,
    id: Uuid,
    name: Option<&str>,
    now: i64,
) -> Result<(), MetaError> {
    sqlx::query("UPDATE users SET name = ?, updated_at = ? WHERE id = ?")
        .bind(name)
        .bind(now)
        .bind(id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Sets or clears `disabled_at`. `now` is `updated_at`.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn set_disabled(
    conn: &mut SqliteConnection,
    id: Uuid,
    disabled_at: Option<i64>,
    now: i64,
) -> Result<(), MetaError> {
    sqlx::query("UPDATE users SET disabled_at = ?, updated_at = ? WHERE id = ?")
        .bind(disabled_at)
        .bind(now)
        .bind(id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Sets the instance-admin flag.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn set_instance_admin(
    conn: &mut SqliteConnection,
    id: Uuid,
    admin: bool,
    now: i64,
) -> Result<(), MetaError> {
    sqlx::query("UPDATE users SET is_instance_admin = ?, updated_at = ? WHERE id = ?")
        .bind(i64::from(admin))
        .bind(now)
        .bind(id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Changing this user would leave no enabled instance admin.
pub(crate) const NO_ACTIVE_ADMIN: &str = "the instance would have no active administrator";

/// This user is the only enabled owner of an org.
pub(crate) const LAST_ORG_OWNER: &str = "the user is the last owner of an org";

/// Enabled instance admins.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the query fails.
pub(crate) async fn count_active_instance_admins(
    conn: &mut SqliteConnection,
) -> Result<i64, MetaError> {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM users WHERE is_instance_admin = 1 AND disabled_at IS NULL",
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(MetaError::from)
}

/// [`MetaError::Conflict`] [`NO_ACTIVE_ADMIN`] when the change would leave none.
///
/// `disabled` and `instance_admin` are the requested values. `None` keeps the
/// current column. The row is not modified.
///
/// # Errors
///
/// Returns [`MetaError::Conflict`] when the user is the last enabled instance
/// admin and the change would disable them or clear the flag. Returns
/// [`MetaError::Db`] when the count fails.
pub(crate) async fn ensure_active_admin(
    conn: &mut SqliteConnection,
    user: &UserRow,
    disabled: Option<bool>,
    instance_admin: Option<bool>,
) -> Result<(), MetaError> {
    let enabled_admin = user.is_instance_admin && user.disabled_at.is_none();
    let admin_after = instance_admin.unwrap_or(user.is_instance_admin);
    let disabled_after = disabled.unwrap_or(user.disabled_at.is_some());
    let enabled_after = admin_after && !disabled_after;
    if enabled_admin == enabled_after {
        return Ok(());
    }
    let count = count_active_instance_admins(conn).await?;
    let next = if enabled_after { count + 1 } else { count - 1 };
    if next <= 0 {
        return Err(MetaError::Conflict(NO_ACTIVE_ADMIN));
    }
    Ok(())
}

/// Whether `user_id` is the only enabled owner of any org.
///
/// A disabled user is not an active owner, so this is false for them.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the query fails.
pub(crate) async fn is_last_active_owner(
    conn: &mut SqliteConnection,
    user_id: Uuid,
) -> Result<bool, MetaError> {
    let found: i64 = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
             FROM org_members AS mine
             JOIN users AS me ON me.id = mine.user_id
             WHERE mine.user_id = ?
               AND mine.role = 'owner'
               AND me.disabled_at IS NULL
               AND (
                   SELECT COUNT(*)
                   FROM org_members AS owners
                   JOIN users AS account ON account.id = owners.user_id
                   WHERE owners.org_id = mine.org_id
                     AND owners.role = 'owner'
                     AND account.disabled_at IS NULL
               ) = 1
         )",
    )
    .bind(user_id.to_string())
    .fetch_one(&mut *conn)
    .await?;
    Ok(found == 1)
}

/// Provider ids linked to these users, ordered by provider.
///
/// Users with no identity are absent from the map.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored user id does not parse.
pub(crate) async fn providers_by_user(
    pool: &SqlitePool,
    user_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<String>>, MetaError> {
    let mut providers: HashMap<Uuid, Vec<String>> = HashMap::new();
    if user_ids.is_empty() {
        return Ok(providers);
    }
    let mut builder = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT user_id, provider FROM identities WHERE user_id IN (",
    );
    let mut separated = builder.separated(", ");
    for id in user_ids {
        separated.push_bind(id.to_string());
    }
    separated.push_unseparated(") ORDER BY user_id, provider");
    let rows: Vec<(String, String)> = builder
        .build_query_as()
        .fetch_all(pool)
        .await
        .map_err(MetaError::from)?;
    for (user_id, provider) in rows {
        let id = Uuid::parse_str(&user_id).map_err(|_| MetaError::Corrupt {
            table: "identities",
            column: "user_id",
        })?;
        providers.entry(id).or_default().push(provider);
    }
    Ok(providers)
}

/// The user with this id, read on the writer connection.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored flag is not 0 or 1.
pub(crate) async fn user_by_id_tx(
    conn: &mut SqliteConnection,
    id: Uuid,
) -> Result<Option<UserRow>, MetaError> {
    fetch_user(
        sqlx::query_as(
            "SELECT id, email, name, password_hash, email_verified_at, is_instance_admin, \
             disabled_at, created_at, updated_at FROM users WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&mut *conn)
        .await?,
    )
}

/// Users ordered by email, after `after_email` (exclusive), at most `limit`.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored flag is not 0 or 1.
pub(crate) async fn list_users(
    pool: &SqlitePool,
    after_email: Option<&str>,
    limit: u32,
) -> Result<Vec<UserRow>, MetaError> {
    let records: Vec<UserRecord> = match after_email {
        Some(email) => {
            sqlx::query_as(
                "SELECT id, email, name, password_hash, email_verified_at, is_instance_admin, \
                 disabled_at, created_at, updated_at FROM users WHERE email > ? \
                 ORDER BY email LIMIT ?",
            )
            .bind(email)
            .bind(i64::from(limit))
            .fetch_all(pool)
            .await?
        }
        None => {
            sqlx::query_as(
                "SELECT id, email, name, password_hash, email_verified_at, is_instance_admin, \
                 disabled_at, created_at, updated_at FROM users ORDER BY email LIMIT ?",
            )
            .bind(i64::from(limit))
            .fetch_all(pool)
            .await?
        }
    };
    records.into_iter().map(user_row).collect()
}

fn fetch_user(record: Option<UserRecord>) -> Result<Option<UserRow>, MetaError> {
    record.map(user_row).transpose()
}

fn user_row(record: UserRecord) -> Result<UserRow, MetaError> {
    let id = Uuid::parse_str(&record.id).map_err(|_| MetaError::Corrupt {
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
    Ok(UserRow {
        id,
        email: record.email,
        name: record.name,
        password_hash: record.password_hash,
        email_verified_at: record.email_verified_at,
        is_instance_admin,
        disabled_at: record.disabled_at,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::test_db;
    use uuid::Uuid;

    #[test]
    fn normalize_email_trims_and_rejects_a_bad_shape() {
        assert_eq!(
            normalize_email("  Ada@Example.COM ").unwrap(),
            "ada@example.com"
        );
        assert_eq!(
            normalize_email("e\u{301}@example.com").unwrap(),
            normalize_email("é@example.com").unwrap()
        );

        let too_long = "a".repeat(64) + "@" + &"b".repeat(186) + ".com";
        let fits = "a".repeat(64) + "@" + &"b".repeat(185) + ".com";
        for raw in [
            "secret-value",
            "a@b@example.com",
            "@example.com",
            "a@b",
            "a@.com",
            "a b@example.com",
            too_long.as_str(),
        ] {
            let error = normalize_email(raw).unwrap_err();
            assert_eq!(error, EmailError::Invalid);
            assert!(!error.to_string().contains("secret"), "{error}");
            assert!(!error.to_string().contains(raw), "{raw}");
        }
        assert!(normalize_email(&fits).is_ok());
    }

    #[tokio::test]
    async fn case_variants_are_one_email_and_columns_round_trip() {
        let (_dir, db) = test_db().await;
        let first = insert(&db, "Ada@Example.COM", false).await;
        let error = db
            .write({
                let email = normalize_email("ADA@example.com").unwrap();
                move |conn| {
                    Box::pin(async move {
                        insert_user(conn, new_user(email, false), 1).await?;
                        Ok(())
                    })
                }
            })
            .await
            .unwrap_err();
        assert!(
            matches!(error, MetaError::Conflict("email already registered")),
            "{error}"
        );

        let loaded = user_by_email(db.reader(), &first.email)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded, first);
        assert_eq!(
            user_by_id(db.reader(), first.id).await.unwrap().unwrap().id,
            first.id
        );

        db.write({
            let id = first.id;
            move |conn| {
                Box::pin(async move {
                    set_name(conn, id, Some("Ada"), 2).await?;
                    set_password_hash(conn, id, Some("hash"), 3).await?;
                    set_email_verified(conn, id, 4).await?;
                    set_instance_admin(conn, id, true, 5).await?;
                    set_disabled(conn, id, Some(6), 7).await?;
                    assert_eq!(count_active_instance_admins(conn).await?, 0);
                    set_disabled(conn, id, None, 8).await?;
                    assert_eq!(count_active_instance_admins(conn).await?, 1);
                    assert_eq!(count_users_tx(conn).await?, 1);
                    Ok(())
                })
            }
        })
        .await
        .unwrap();

        let updated = user_by_id(db.reader(), first.id).await.unwrap().unwrap();
        assert_eq!(updated.name.as_deref(), Some("Ada"));
        assert_eq!(updated.password_hash.as_deref(), Some("hash"));
        assert_eq!(updated.email_verified_at, Some(4));
        assert!(updated.is_instance_admin);
        assert_eq!(updated.disabled_at, None);
        assert_eq!(updated.updated_at, 8);
        assert_eq!(count_users(db.reader()).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn list_users_pages_by_email() {
        let (_dir, db) = test_db().await;
        for email in ["c@example.com", "a@example.com", "b@example.com"] {
            insert(&db, email, false).await;
        }
        let page = list_users(db.reader(), None, 2).await.unwrap();
        assert_eq!(emails(&page), ["a@example.com", "b@example.com"]);
        let next = list_users(db.reader(), Some(&page[1].email), 2)
            .await
            .unwrap();
        assert_eq!(emails(&next), ["c@example.com"]);
    }

    async fn insert(db: &crate::meta::MetaDb, email: &str, admin: bool) -> UserRow {
        let email = normalize_email(email).unwrap();
        db.write(move |conn| {
            Box::pin(async move { insert_user(conn, new_user(email, admin), 1).await })
        })
        .await
        .unwrap()
    }

    fn new_user(email: String, admin: bool) -> NewUser {
        NewUser {
            email,
            name: None,
            password_hash: None,
            email_verified_at: None,
            is_instance_admin: admin,
        }
    }

    fn emails(users: &[UserRow]) -> Vec<&str> {
        users.iter().map(|user| user.email.as_str()).collect()
    }

    #[tokio::test]
    async fn the_last_active_admin_cannot_be_removed() {
        let (_dir, db) = test_db().await;
        let only = insert(&db, "ada@example.com", true).await;
        let error = db
            .write({
                let only = only.clone();
                move |conn| {
                    Box::pin(async move {
                        ensure_active_admin(conn, &only, Some(true), None).await?;
                        Ok(())
                    })
                }
            })
            .await
            .unwrap_err();
        assert!(
            matches!(error, MetaError::Conflict(NO_ACTIVE_ADMIN)),
            "{error}"
        );
        let demote = db
            .write({
                let only = only.clone();
                move |conn| {
                    Box::pin(async move {
                        ensure_active_admin(conn, &only, None, Some(false)).await?;
                        Ok(())
                    })
                }
            })
            .await
            .unwrap_err();
        assert!(matches!(demote, MetaError::Conflict(NO_ACTIVE_ADMIN)));

        let second = insert(&db, "bob@example.com", true).await;
        db.write(move |conn| {
            Box::pin(async move {
                ensure_active_admin(conn, &second, Some(true), None).await?;
                ensure_active_admin(conn, &only, None, Some(false)).await?;
                Ok(())
            })
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn the_last_active_owner_of_an_org_is_reported() {
        let (_dir, db) = test_db().await;
        let owner = insert(&db, "ada@example.com", false).await;
        let other = insert(&db, "bob@example.com", false).await;
        db.write({
            let owner_id = owner.id;
            let other_id = other.id;
            move |conn| {
                Box::pin(async move {
                    assert!(!is_last_active_owner(conn, owner_id).await?);
                    let org = crate::meta::orgs::insert_org(conn, "Acme", "acme", 1).await?;
                    crate::meta::members::add_org_member(
                        conn,
                        org.id,
                        owner_id,
                        crate::auth::OrgRole::Owner,
                        1,
                    )
                    .await?;
                    assert!(is_last_active_owner(conn, owner_id).await?);
                    crate::meta::members::add_org_member(
                        conn,
                        org.id,
                        other_id,
                        crate::auth::OrgRole::Owner,
                        1,
                    )
                    .await?;
                    assert!(!is_last_active_owner(conn, owner_id).await?);
                    set_disabled(conn, other_id, Some(2), 2).await?;
                    assert!(is_last_active_owner(conn, owner_id).await?);
                    set_disabled(conn, owner_id, Some(3), 3).await?;
                    assert!(!is_last_active_owner(conn, owner_id).await?);
                    Ok(())
                })
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn providers_are_grouped_and_ordered() {
        let (_dir, db) = test_db().await;
        let user = insert(&db, "ada@example.com", false).await;
        let other = insert(&db, "bob@example.com", false).await;
        db.write({
            let user_id = user.id.to_string();
            let other_id = other.id.to_string();
            move |conn| {
                Box::pin(async move {
                    for (id, user_id, provider, subject) in [
                        (Uuid::now_v7(), &user_id, "oidc:okta", "okta-1"),
                        (Uuid::now_v7(), &user_id, "github", "gh-1"),
                        (Uuid::now_v7(), &other_id, "google", "g-1"),
                    ] {
                        sqlx::query(
                            "INSERT INTO identities \
                             (id, user_id, provider, subject, email, created_at) \
                             VALUES (?, ?, ?, ?, NULL, 1)",
                        )
                        .bind(id.to_string())
                        .bind(user_id)
                        .bind(provider)
                        .bind(subject)
                        .execute(&mut *conn)
                        .await?;
                    }
                    Ok(())
                })
            }
        })
        .await
        .unwrap();
        let found = providers_by_user(db.reader(), &[user.id, other.id])
            .await
            .unwrap();
        let ada = vec!["github".to_owned(), "oidc:okta".to_owned()];
        let bob = vec!["google".to_owned()];
        assert_eq!(found.get(&user.id), Some(&ada));
        assert_eq!(found.get(&other.id), Some(&bob));
        let empty = providers_by_user(db.reader(), &[]).await.unwrap();
        assert!(empty.is_empty());
    }
}
