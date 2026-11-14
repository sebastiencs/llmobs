//! Local admin operations on an existing `meta.db` (spec 09 § CLI).
//!
//! These open the database without migrating and without the data-dir lock, so
//! they work while `llmobs serve` is running (D136). `rotate_encryption_key`
//! is the exception: it takes the lock and needs the server stopped (D-I-3).

use std::path::Path;

use uuid::Uuid;

use crate::auth::AuthError;
use crate::auth::password::{Hasher, check_policy};
use crate::auth::reset::{ResetError, mint_reset, reset_link};
use crate::auth::secret::constant_time_eq;
use crate::clock::Clock;
use crate::config::ServerConfig;
use crate::crypto::{self, Aad, KeySource, SecretBox};
use crate::meta::deletions::{
    DeletionState, all_deletions, insert_copy, scope_projects, scope_sessions, scope_traces,
};
use crate::meta::instance_settings::{self, ENCRYPTION_CHECK};
use crate::meta::password_resets::insert_reset;
use crate::meta::users::{self, NewUser, normalize_email};
use crate::meta::{MetaDb, MetaError};

/// A user created by [`create_user`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedUser {
    /// `users.id`.
    pub id: Uuid,
    /// Present when no password was supplied. The URL carries the reset token.
    pub reset_url: Option<String>,
}

/// One row of [`list_users`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserSummary {
    /// `users.id`.
    pub id: Uuid,
    /// Normalized email.
    pub email: String,
    /// Display name.
    pub name: Option<String>,
    /// Instance admin.
    pub instance_admin: bool,
    /// `disabled_at` is set.
    pub disabled: bool,
    /// Creation time, nanoseconds since the Unix epoch.
    pub created_at: i64,
}

/// Why an admin command failed. The text never includes an email, a password, or a token.
#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    /// `meta.db` does not exist yet.
    #[error("run `llmobs serve` once to create the database")]
    NotInitialized,
    /// The file's schema is not this binary's.
    #[error("this binary expects schema {expected}, found {found}")]
    Schema {
        /// Migration version embedded in this binary.
        expected: i64,
        /// Latest applied migration, or 0 when the table is empty.
        found: i64,
    },
    /// The email failed the shape check.
    #[error("invalid email")]
    Email,
    /// The name is empty or longer than 100 characters after trimming.
    #[error("name must be 1 to 100 characters")]
    Name,
    /// The password failed the policy. The text is the policy's static reason.
    #[error("{0}")]
    Password(String),
    /// No user has that email.
    #[error("no such user")]
    NotFound,
    /// The email is already registered.
    #[error("{0}")]
    Conflict(&'static str),
    /// argon2 could not hash the password.
    #[error("password hash failed")]
    Hash,
    /// The OS random source failed.
    #[error("random number generator failed")]
    Rng,
    /// SQLite failed. The text is [`MetaError`]'s display, which names no row values.
    #[error("{0}")]
    Database(String),
    /// Another process holds `<data_dir>/LOCK`.
    #[error("the server is running: stop it before rotating the key")]
    Locked,
    /// The key could not be loaded, does not match, or a value did not open.
    #[error("{0}")]
    Crypto(String),
    /// The new key is the current key.
    #[error("the new key is the current key")]
    SameKey,
    /// `--from` holds no `meta.db`.
    #[error("no meta.db in the --from directory")]
    SourceNotInitialized,
    /// The `--from` database is at another schema version.
    #[error(
        "the --from meta.db has schema {found}, this binary expects {expected}; run `llmobs serve` on a copy of it first"
    )]
    SourceSchema {
        /// Migration version embedded in this binary.
        expected: i64,
        /// Latest applied migration of the source.
        found: i64,
    },
}

/// Creates a verified user. Without `password`, the result carries a one-hour reset URL.
///
/// The email is normalized. A present name is trimmed and must be 1–100 characters.
/// The password is checked and hashed before `meta.db` is opened. The user row and
/// the reset token, when there is one, are one write.
///
/// # Errors
///
/// Returns [`AdminError`] for a bad email, name, or password, a duplicate email,
/// or a database that is missing or unreadable.
pub async fn create_user(
    config: &ServerConfig,
    email: &str,
    name: Option<&str>,
    password: Option<&str>,
    instance_admin: bool,
) -> Result<CreatedUser, AdminError> {
    let email = normalize_email(email).map_err(|_| AdminError::Email)?;
    let name = checked_name(name)?;
    let password_hash = match password {
        Some(password) => Some(hash_password(password, &email).await?),
        None => None,
    };
    let now = Clock::System.now_ns();
    let minted = match &password_hash {
        Some(_) => None,
        None => Some(mint_reset(&config.public_url, now).map_err(|error| map_auth(&error))?),
    };
    let reset_url = minted.as_ref().map(|minted| minted.url.clone());
    let meta = open_meta(config).await?;
    let inserted = meta
        .write(move |conn| {
            Box::pin(async move {
                let user = users::insert_user(
                    conn,
                    NewUser {
                        email,
                        name,
                        password_hash,
                        email_verified_at: Some(now),
                        is_instance_admin: instance_admin,
                    },
                    now,
                )
                .await?;
                if let Some(minted) = minted {
                    insert_reset(conn, &minted.hash, user.id, now, minted.expires_at).await?;
                }
                Ok(user.id)
            })
        })
        .await;
    meta.close().await;
    Ok(CreatedUser {
        id: inserted.map_err(map_meta)?,
        reset_url,
    })
}

/// Stores a one-hour reset token and returns its URL.
///
/// An unknown email is [`AdminError::NotFound`]. The message does not repeat the email.
///
/// # Errors
///
/// Returns [`AdminError`] when the email is invalid, the user does not exist, or
/// the database cannot be written.
pub async fn reset_password(config: &ServerConfig, email: &str) -> Result<String, AdminError> {
    let email = normalize_email(email).map_err(|_| AdminError::Email)?;
    let now = Clock::System.now_ns();
    let meta = open_meta(config).await?;
    let outcome = match users::user_by_email(meta.reader(), &email).await {
        Ok(Some(user)) => reset_link(&meta, &config.public_url, user.id, now)
            .await
            .map_err(map_reset),
        Ok(None) => Err(AdminError::NotFound),
        Err(error) => Err(map_meta(error)),
    };
    meta.close().await;
    outcome
}

/// Every user, ordered by email.
///
/// # Errors
///
/// Returns [`AdminError`] when the database is missing or a row does not decode.
pub async fn list_users(config: &ServerConfig) -> Result<Vec<UserSummary>, AdminError> {
    let meta = open_meta(config).await?;
    let outcome = list_all(&meta).await;
    meta.close().await;
    outcome
}

/// What [`reapply_deletions`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReapplyReport {
    /// Rows copied as open rows, to be applied again.
    pub reopened: u64,
    /// `failed` rows copied for the audit trail.
    pub failed_copied: u64,
    /// Rows the current `meta.db` already had.
    pub present: u64,
}

/// Copies the deletions of `<from>/meta.db` that `config`'s `meta.db` lacks (D-M-9).
///
/// Each copied row keeps its id, scope and cut-off and is opened again: the running
/// (or next) `llmobs serve` registers or drops it through its worker. Each row is one
/// transaction, so an interrupted run leaves whole rows, and a second run copies nothing
/// new. `from` is opened read-write (WAL), so copy a read-only backup somewhere writable
/// first. Pointing `from` at the current data directory copies nothing.
///
/// # Errors
///
/// [`AdminError::NotInitialized`] / [`AdminError::Schema`] for the target,
/// [`AdminError::SourceNotInitialized`] / [`AdminError::SourceSchema`] for `from`,
/// [`AdminError::Database`] for a query failure.
pub async fn reapply_deletions(
    config: &ServerConfig,
    from: &Path,
) -> Result<ReapplyReport, AdminError> {
    let target = open_meta(config).await?;
    let source = match MetaDb::open_existing(from).await {
        Ok(db) => db,
        Err(error) => {
            target.close().await;
            return Err(map_source(error));
        }
    };
    let outcome = copy_all(&target, &source).await;
    source.close().await;
    target.close().await;
    outcome.map_err(map_meta)
}

async fn copy_all(target: &MetaDb, source: &MetaDb) -> Result<ReapplyReport, MetaError> {
    let rows = all_deletions(source.reader()).await?;
    let mut report = ReapplyReport::default();
    for row in rows {
        let traces = scope_traces(source.reader(), row.id).await?;
        let sessions = scope_sessions(source.reader(), row.id).await?;
        let projects = scope_projects(source.reader(), row.id).await?;
        let failed = row.state == DeletionState::Failed;
        let copied = target
            .write(move |conn| {
                Box::pin(
                    async move { insert_copy(conn, &row, &traces, &sessions, &projects).await },
                )
            })
            .await?;
        if !copied {
            report.present += 1;
        } else if failed {
            report.failed_copied += 1;
        } else {
            report.reopened += 1;
        }
    }
    Ok(report)
}

const PAGE: u32 = 500;

async fn list_all(meta: &MetaDb) -> Result<Vec<UserSummary>, AdminError> {
    let mut users = Vec::new();
    let mut after = None;
    loop {
        let page = users::list_users(meta.reader(), after.as_deref(), PAGE)
            .await
            .map_err(map_meta)?;
        let short = match u32::try_from(page.len()) {
            Ok(len) => len < PAGE,
            Err(_) => false,
        };
        if let Some(last) = page.last() {
            after = Some(last.email.clone());
        }
        users.extend(page.into_iter().map(|row| UserSummary {
            id: row.id,
            email: row.email,
            name: row.name,
            instance_admin: row.is_instance_admin,
            disabled: row.disabled_at.is_some(),
            created_at: row.created_at,
        }));
        if short {
            break;
        }
    }
    Ok(users)
}

/// What [`rotate_encryption_key`] did.
#[derive(Debug)]
pub struct Rotated {
    /// Sealed values re-encrypted.
    pub values: u64,
}

/// Re-encrypts every sealed column under a new key (D-I-3). The server must be stopped.
/// `new_key` is base64 text (from stdin); `None` generates a key, unless the current
/// key comes from `LLMOBS_ENCRYPTION_KEY`: then the new key must be given, because
/// a generated one would exist only in memory after the commit.
///
/// # Errors
///
/// Returns [`AdminError`] when the server is running ([`AdminError::Locked`]),
/// `meta.db` is missing, the current key is missing or does not match this
/// data directory, no new key was given for an environment key, or a stored
/// value does not open. A failure before the commit leaves the database and
/// `secret.key` unchanged and removes `secret.key.new`; a failure after the
/// commit (the rename) leaves the new key in `secret.key.new`, which the
/// next start renames into place.
pub async fn rotate_encryption_key(
    config: &ServerConfig,
    new_key: Option<&str>,
) -> Result<Rotated, AdminError> {
    if !config.data_dir.join("meta.db").is_file() {
        return Err(AdminError::NotInitialized);
    }
    let lock_path = config.data_dir.join("LOCK");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path);
    let lock = match lock {
        Ok(file) => file,
        Err(_error) => return Err(lock_error(&lock_path)),
    };
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Err(AdminError::Locked),
        Err(std::fs::TryLockError::Error(_error)) => return Err(lock_error(&lock_path)),
    }
    // The lock is held until this function returns.
    let meta = MetaDb::open_existing(&config.data_dir)
        .await
        .map_err(map_meta)?;
    let rotated = rotate_locked(&meta, config, new_key).await;
    meta.close().await;
    rotated
}

fn lock_error(path: &Path) -> AdminError {
    AdminError::Crypto(format!("could not lock {}", path.display()))
}

/// The rotation itself, on a database whose data directory this call has locked.
async fn rotate_locked(
    meta: &MetaDb,
    config: &ServerConfig,
    new_key: Option<&str>,
) -> Result<Rotated, AdminError> {
    let env = config
        .encryption_key
        .as_ref()
        .map(|key| key.expose().as_str());
    let check = instance_settings::get_setting(meta.reader(), ENCRYPTION_CHECK)
        .await
        .map_err(map_meta)?;
    if env.is_none() {
        crypto::finish_rotation(&config.data_dir, check.as_deref())
            .map_err(|error| AdminError::Crypto(error.to_string()))?;
    }
    let old = match crypto::read_key(env, &config.data_dir) {
        Ok(Some(loaded)) => loaded,
        Ok(None) => {
            return Err(AdminError::Crypto(
                "no encryption key: start the server once first".into(),
            ));
        }
        Err(error) => return Err(AdminError::Crypto(error.to_string())),
    };
    let old_box = SecretBox::new(&old.key);
    if !check
        .as_deref()
        .is_some_and(|stored| constant_time_eq(stored, old_box.check_value()))
    {
        return Err(AdminError::Crypto(
            "the current key does not match this data directory".into(),
        ));
    }
    let new = match new_key {
        Some(text) => crypto::decode_key(text.trim(), "--new-from-stdin")
            .map_err(|error| AdminError::Crypto(error.to_string()))?,
        None if old.source == KeySource::Env => {
            // A generated key would exist only in memory after the commit,
            // and the next start would still use the env value.
            return Err(AdminError::Crypto(
                "the key comes from LLMOBS_ENCRYPTION_KEY: pass the new key with --new-from-stdin"
                    .into(),
            ));
        }
        None => crypto::random_key().map_err(|error| AdminError::Crypto(error.to_string()))?,
    };
    if new == old.key {
        return Err(AdminError::SameKey);
    }
    let new_path = config.data_dir.join("secret.key.new");
    let file_source = old.source == KeySource::File;
    if file_source {
        let _ = std::fs::remove_file(&new_path);
        if let Err(error) = crypto::write_key_file(&new_path, &new) {
            // The failed write can leave `secret.key.new.tmp` behind; a missing
            // file is fine.
            let _ = std::fs::remove_file(config.data_dir.join("secret.key.new.tmp"));
            return Err(AdminError::Crypto(error.to_string()));
        }
    }
    let old_key = old.key;
    let written = meta
        .write(move |conn| {
            Box::pin(async move {
                let old_box = SecretBox::new(&old_key);
                let new_box = SecretBox::new(&new);
                let mut values = 0u64;
                for (table, column) in crypto::ENCRYPTED_COLUMNS {
                    // table and column come from ENCRYPTED_COLUMNS, never from input,
                    // so the formatted SQL is audited with `AssertSqlSafe`.
                    let select =
                        format!("SELECT id, {column} FROM {table} WHERE {column} IS NOT NULL");
                    let rows: Vec<(String, Vec<u8>)> = sqlx::query_as(sqlx::AssertSqlSafe(select))
                        .fetch_all(&mut *conn)
                        .await?;
                    for (row_id_text, sealed) in rows {
                        let row_id = Uuid::parse_str(&row_id_text)
                            .map_err(|_| MetaError::Corrupt { table, column })?;
                        let aad = Aad {
                            table,
                            column,
                            row_id: &row_id,
                        };
                        let plaintext = old_box
                            .open(&sealed, aad)
                            .map_err(|_| MetaError::Corrupt { table, column })?;
                        let resealed = new_box
                            .seal(&plaintext, aad)
                            .map_err(|_| MetaError::Corrupt { table, column })?;
                        let update = format!("UPDATE {table} SET {column} = ? WHERE id = ?");
                        let updated = sqlx::query(sqlx::AssertSqlSafe(update))
                            .bind(&resealed)
                            .bind(&row_id_text)
                            .execute(&mut *conn)
                            .await?;
                        values += updated.rows_affected();
                    }
                }
                let check = new_box.check_value().to_owned();
                instance_settings::put_setting(conn, ENCRYPTION_CHECK, &check).await?;
                Ok(values)
            })
        })
        .await;
    let values = match written {
        Ok(values) => values,
        Err(error) => {
            if file_source {
                let _ = std::fs::remove_file(&new_path);
            }
            return Err(AdminError::Crypto(error.to_string()));
        }
    };
    if file_source {
        let key_file = crypto::key_path(&config.data_dir);
        if let Err(_error) = std::fs::rename(&new_path, &key_file) {
            return Err(AdminError::Crypto(format!(
                "the database uses the new key, which is in {}; rename it to secret.key",
                new_path.display()
            )));
        }
        // A rename lost to a crash is finished by the next start's
        // `finish_rotation`, so a directory sync failure needs no error here.
        let _ = std::fs::File::open(&config.data_dir).and_then(|dir| dir.sync_all());
    }
    Ok(Rotated { values })
}

fn checked_name(name: Option<&str>) -> Result<Option<String>, AdminError> {
    let Some(name) = name else {
        return Ok(None);
    };
    let name = name.trim();
    let length = name.chars().count();
    if !(1..=100).contains(&length) {
        return Err(AdminError::Name);
    }
    Ok(Some(name.to_owned()))
}

async fn hash_password(password: &str, email: &str) -> Result<String, AdminError> {
    check_policy(password, email).map_err(|error| AdminError::Password(error.to_string()))?;
    let hasher = Hasher::new().await.map_err(|error| map_auth(&error))?;
    hasher
        .hash(password)
        .await
        .map_err(|error| map_auth(&error))
}

async fn open_meta(config: &ServerConfig) -> Result<MetaDb, AdminError> {
    MetaDb::open_existing(&config.data_dir)
        .await
        .map_err(map_meta)
}

fn map_auth(error: &AuthError) -> AdminError {
    match error {
        AuthError::Rng => AdminError::Rng,
        AuthError::PasswordHash => AdminError::Hash,
    }
}

fn map_reset(error: ResetError) -> AdminError {
    match error {
        ResetError::Auth(error) => map_auth(&error),
        ResetError::Meta(error) => map_meta(error),
    }
}

/// The `--from` database's errors: the same as the target's, with the source variants.
fn map_source(error: MetaError) -> AdminError {
    match error {
        MetaError::NotInitialized => AdminError::SourceNotInitialized,
        MetaError::SchemaMismatch { expected, found } => {
            AdminError::SourceSchema { expected, found }
        }
        other => map_meta(other),
    }
}

fn map_meta(error: MetaError) -> AdminError {
    match error {
        MetaError::NotInitialized => AdminError::NotInitialized,
        MetaError::SchemaMismatch { expected, found } => AdminError::Schema { expected, found },
        MetaError::Conflict(detail) => AdminError::Conflict(detail),
        other => AdminError::Database(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::secret::hash_token;
    use crate::config::Secret;
    use crate::meta::deletions::{
        ActorKind, DeletionKind, DeletionRow, FailReason, NewDeletion, RequestDetails,
        deletion_by_id, insert_deletion, insert_projects, insert_sessions, insert_traces,
        mark_applied, mark_dropped, mark_failed, mark_handed_over, scope_projects, scope_sessions,
        scope_traces, store_counts,
    };
    use crate::meta::orgs::insert_org;
    use crate::meta::password_resets::valid_reset;
    use crate::meta::projects::insert_project;
    use crate::meta::users::{set_disabled, user_by_email};
    use crate::test_support::{TestApp, dataset_span};
    use axum::http::{Method, StatusCode};
    use llmobs_core::ids::{OrgId, ProjectId, SpanId, TraceId};
    use std::time::Duration;

    const PASSWORD: &str = "llmobs-ok1";

    /// The org every seeded connection belongs to.
    const ORG: &str = "01900000-0000-7000-8000-00000000000a";

    /// The plaintext sealed into `extra_headers_enc` by `seed_connection`.
    const HEADERS: &[u8] = br#"{"x-tenant":"acme"}"#;

    async fn ready_config() -> (tempfile::TempDir, ServerConfig) {
        let app = TestApp::new().await;
        let dir = tempfile::tempdir().unwrap();
        let db = MetaDb::open(dir.path()).await.unwrap();
        db.close().await;
        let mut config = app.state.config.clone();
        config.data_dir = dir.path().to_path_buf();
        (dir, config)
    }

    #[tokio::test]
    async fn missing_database_is_not_initialized() {
        let app = TestApp::new().await;
        let empty = tempfile::tempdir().unwrap();
        let mut config = app.state.config.clone();
        config.data_dir = empty.path().to_path_buf();
        let error = list_users(&config).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            "run `llmobs serve` once to create the database"
        );
        let rejected = create_user(
            &config,
            "not-an-email",
            Some("  "),
            Some("tiny-pass"),
            false,
        )
        .await
        .unwrap_err();
        assert_eq!(rejected.to_string(), "invalid email");
        assert!(!rejected.to_string().contains("not-an-email"));
        let named = create_user(&config, "ada@example.com", Some("  "), None, false)
            .await
            .unwrap_err();
        assert_eq!(named.to_string(), "name must be 1 to 100 characters");
        let weak = create_user(&config, "ada@example.com", None, Some("tiny-pass"), false)
            .await
            .unwrap_err();
        assert_eq!(weak.to_string(), "password is too short");
        assert!(!weak.to_string().contains("tiny-pass"));
    }

    #[tokio::test]
    async fn schema_mismatch_names_the_versions() {
        let (_dir, config) = ready_config().await;
        let db = MetaDb::open(&config.data_dir).await.unwrap();
        let expected = crate::meta::expected_version();
        let future = expected + 1;
        db.write(move |connection| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO _sqlx_migrations \
                     (version, description, success, checksum, execution_time) \
                     VALUES (?, 'future', 1, ?, 0)",
                )
                .bind(future)
                .bind(Vec::<u8>::new())
                .execute(&mut *connection)
                .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        db.close().await;
        let error = list_users(&config).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("this binary expects schema {expected}, found {future}")
        );
    }

    #[tokio::test]
    async fn create_user_with_and_without_a_password() {
        let (_dir, config) = ready_config().await;
        let created = create_user(&config, "Ada@Example.com", Some("  Ada  "), None, true)
            .await
            .unwrap();
        let url = created.reset_url.expect("reset url");
        assert!(url.contains("/reset?token="), "{url}");
        let db = MetaDb::open_existing(&config.data_dir).await.unwrap();
        let now = Clock::System.now_ns();
        let owner = valid_reset(db.reader(), &reset_hash(&url), now)
            .await
            .unwrap();
        assert_eq!(owner, Some(created.id));
        let row = user_by_email(db.reader(), "ada@example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.name.as_deref(), Some("Ada"));
        assert!(row.password_hash.is_none());
        assert!(row.email_verified_at.is_some());
        assert!(row.is_instance_admin);
        db.close().await;

        let with_password = create_user(&config, "bea@example.com", None, Some(PASSWORD), false)
            .await
            .unwrap();
        assert!(with_password.reset_url.is_none());
        let db = MetaDb::open_existing(&config.data_dir).await.unwrap();
        let row = user_by_email(db.reader(), "bea@example.com")
            .await
            .unwrap()
            .unwrap();
        db.close().await;
        let hasher = Hasher::new().await.unwrap();
        let verified = hasher
            .verify(PASSWORD, row.password_hash.as_deref())
            .await
            .unwrap();
        assert!(verified.ok);
        assert!(!row.is_instance_admin);
        assert!(row.email_verified_at.is_some());

        let duplicate = create_user(&config, "ada@example.com", None, None, false)
            .await
            .unwrap_err();
        assert_eq!(duplicate.to_string(), "email already registered");
        assert!(!duplicate.to_string().contains("ada@example.com"));

        let missing = reset_password(&config, "missing@example.com")
            .await
            .unwrap_err();
        assert_eq!(missing.to_string(), "no such user");
        assert!(!missing.to_string().contains("missing@example.com"));
    }

    #[tokio::test]
    async fn list_users_is_ordered_by_email() {
        let (_dir, config) = ready_config().await;
        let bea = create_user(&config, "bea@example.com", Some("Bea"), None, false)
            .await
            .unwrap();
        let ada = create_user(&config, "ada@example.com", None, None, true)
            .await
            .unwrap();
        let db = MetaDb::open_existing(&config.data_dir).await.unwrap();
        let now = Clock::System.now_ns();
        db.write(move |conn| {
            Box::pin(async move { set_disabled(conn, bea.id, Some(now), now).await })
        })
        .await
        .unwrap();
        db.close().await;
        let listed = list_users(&config).await.unwrap();
        assert_eq!(
            listed
                .iter()
                .map(|user| user.email.as_str())
                .collect::<Vec<_>>(),
            vec!["ada@example.com", "bea@example.com"]
        );
        assert!(listed[0].instance_admin);
        assert!(!listed[0].disabled);
        assert!(listed[0].name.is_none());
        assert_eq!(listed[0].id, ada.id);
        assert!(!listed[1].instance_admin);
        assert!(listed[1].disabled);
        assert_eq!(listed[1].name.as_deref(), Some("Bea"));
    }

    #[tokio::test]
    async fn create_user_works_while_the_server_holds_the_directory() {
        let app = TestApp::new().await;
        let created = create_user(
            &app.state.config,
            "ada@example.com",
            None,
            Some(PASSWORD),
            false,
        )
        .await
        .unwrap();
        let row = user_by_email(app.state.meta.reader(), "ada@example.com")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.id, created.id);
        assert!(created.reset_url.is_none());
    }

    async fn seed_org(db: &MetaDb) {
        db.write(|conn| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO orgs (id, name, slug, created_at, updated_at) \
                     VALUES (?, 'Acme', 'acme', 0, 0)",
                )
                .bind(ORG)
                .execute(&mut *conn)
                .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
    }

    /// Inserts one connection with the given sealed columns.
    async fn insert_connection(
        db: &MetaDb,
        id: &Uuid,
        secret_enc: Vec<u8>,
        extra_headers_enc: Option<Vec<u8>>,
    ) {
        let id = id.to_string();
        // `name` is unique per org; the row id doubles as the seed name.
        let name = id.clone();
        db.write(move |conn| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO llm_connections \
                     (id, org_id, name, adapter, secret_enc, api_key_hint, extra_headers_enc, \
                      created_at, updated_at) \
                     VALUES (?, ?, ?, 'openai', ?, 'TEST', ?, 0, 0)",
                )
                .bind(id)
                .bind(ORG)
                .bind(name)
                .bind(&secret_enc)
                .bind(extra_headers_enc)
                .execute(&mut *conn)
                .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
    }

    /// Inserts one connection whose sealed columns were written by `secrets`.
    async fn seed_connection(db: &MetaDb, id: &Uuid, secrets: &SecretBox, with_headers: bool) {
        let secret_enc = secrets
            .seal(
                b"sk-test-key",
                Aad {
                    table: "llm_connections",
                    column: "secret_enc",
                    row_id: id,
                },
            )
            .unwrap();
        let extra_headers_enc = with_headers.then(|| {
            secrets
                .seal(
                    HEADERS,
                    Aad {
                        table: "llm_connections",
                        column: "extra_headers_enc",
                        row_id: id,
                    },
                )
                .unwrap()
        });
        insert_connection(db, id, secret_enc, extra_headers_enc).await;
    }

    /// Records the check value of `key`, as a started server would have.
    async fn seed_check(config: &ServerConfig, key: [u8; 32]) {
        let db = MetaDb::open(&config.data_dir).await.unwrap();
        crypto::verify_or_record(
            &db,
            &SecretBox::new(&key),
            &crypto::key_path(&config.data_dir),
        )
        .await
        .unwrap();
        db.close().await;
    }

    async fn stored_column(db: &MetaDb, id: &Uuid, column: &str) -> Option<Vec<u8>> {
        // `column` is a test literal, so the formatted SQL is audited with `AssertSqlSafe`.
        let select = format!("SELECT {column} FROM llm_connections WHERE id = ?");
        sqlx::query_scalar(sqlx::AssertSqlSafe(select))
            .bind(id.to_string())
            .fetch_one(db.reader())
            .await
            .unwrap()
    }

    async fn stored_check(db: &MetaDb) -> String {
        instance_settings::get_setting(db.reader(), ENCRYPTION_CHECK)
            .await
            .unwrap()
            .unwrap()
    }

    fn connection_aad<'a>(column: &'static str, row_id: &'a Uuid) -> Aad<'a> {
        Aad {
            table: "llm_connections",
            column,
            row_id,
        }
    }

    #[tokio::test]
    async fn rotation_reencrypts_and_replaces_the_file() {
        let (dir, config) = ready_config().await;
        let old = crypto::load_key(None, dir.path(), false).unwrap();
        assert_eq!(old.source, KeySource::Generated);
        let old_box = SecretBox::new(&old.key);
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        {
            let db = MetaDb::open(&config.data_dir).await.unwrap();
            crypto::verify_or_record(&db, &old_box, &crypto::key_path(dir.path()))
                .await
                .unwrap();
            seed_org(&db).await;
            seed_connection(&db, &first, &old_box, true).await;
            seed_connection(&db, &second, &old_box, false).await;
            db.close().await;
        }

        let rotated = rotate_encryption_key(&config, None).await.unwrap();
        assert_eq!(rotated.values, 3);
        assert!(!dir.path().join("secret.key.new").exists());

        let new_text = std::fs::read_to_string(crypto::key_path(dir.path())).unwrap();
        let new_key = crypto::decode_key(new_text.trim(), "test").unwrap();
        assert_ne!(new_key, old.key);
        let new_box = SecretBox::new(&new_key);
        let db = MetaDb::open_existing(&config.data_dir).await.unwrap();
        for (id, column, expected) in [
            (&first, "secret_enc", &b"sk-test-key"[..]),
            (&first, "extra_headers_enc", HEADERS),
            (&second, "secret_enc", &b"sk-test-key"[..]),
        ] {
            let sealed = stored_column(&db, id, column).await.unwrap();
            let opened = new_box.open(&sealed, connection_aad(column, id)).unwrap();
            assert_eq!(opened, expected);
        }
        let check = stored_check(&db).await;
        db.close().await;
        assert_eq!(check, new_box.check_value());
    }

    #[tokio::test]
    async fn rotation_with_an_env_key_reencrypts_under_a_given_key() {
        let (dir, mut config) = ready_config().await;
        let old = [1; 32];
        let new = [2; 32];
        let id = Uuid::now_v7();
        {
            let db = MetaDb::open(&config.data_dir).await.unwrap();
            crypto::verify_or_record(&db, &SecretBox::new(&old), &crypto::key_path(dir.path()))
                .await
                .unwrap();
            seed_org(&db).await;
            seed_connection(&db, &id, &SecretBox::new(&old), false).await;
            db.close().await;
        }
        config.encryption_key = Some(Secret::new(crypto::encode_key(&old)));
        // Surrounding whitespace is trimmed where the key is decoded.
        let given = format!("  {}\r\n", crypto::encode_key(&new));
        let rotated = rotate_encryption_key(&config, Some(&given)).await.unwrap();
        assert_eq!(rotated.values, 1);
        assert!(!crypto::key_path(dir.path()).exists());
        assert!(!dir.path().join("secret.key.new").exists());
        let db = MetaDb::open_existing(&config.data_dir).await.unwrap();
        let sealed = stored_column(&db, &id, "secret_enc").await.unwrap();
        let check = stored_check(&db).await;
        db.close().await;
        let new_box = SecretBox::new(&new);
        assert_eq!(
            new_box
                .open(&sealed, connection_aad("secret_enc", &id))
                .unwrap(),
            b"sk-test-key"
        );
        assert_eq!(check, new_box.check_value());
    }

    #[tokio::test]
    async fn rotation_with_an_env_key_leaves_secret_key_alone() {
        let (dir, mut config) = ready_config().await;
        let old = [1; 32];
        let new = [2; 32];
        let id = Uuid::now_v7();
        crypto::write_key_file(&crypto::key_path(dir.path()), &[5; 32]).unwrap();
        let before = std::fs::read(crypto::key_path(dir.path())).unwrap();
        {
            let db = MetaDb::open(&config.data_dir).await.unwrap();
            crypto::verify_or_record(&db, &SecretBox::new(&old), &crypto::key_path(dir.path()))
                .await
                .unwrap();
            seed_org(&db).await;
            seed_connection(&db, &id, &SecretBox::new(&old), false).await;
            db.close().await;
        }
        config.encryption_key = Some(Secret::new(crypto::encode_key(&old)));
        let rotated = rotate_encryption_key(&config, Some(&crypto::encode_key(&new)))
            .await
            .unwrap();
        assert_eq!(rotated.values, 1);
        assert_eq!(std::fs::read(crypto::key_path(dir.path())).unwrap(), before);
        assert!(!dir.path().join("secret.key.new").exists());
    }

    #[tokio::test]
    async fn rotation_with_an_env_key_requires_a_new_key() {
        let (dir, mut config) = ready_config().await;
        let old = [1; 32];
        let id = Uuid::now_v7();
        {
            let db = MetaDb::open(&config.data_dir).await.unwrap();
            crypto::verify_or_record(&db, &SecretBox::new(&old), &crypto::key_path(dir.path()))
                .await
                .unwrap();
            seed_org(&db).await;
            seed_connection(&db, &id, &SecretBox::new(&old), false).await;
            db.close().await;
        }
        config.encryption_key = Some(Secret::new(crypto::encode_key(&old)));
        let error = rotate_encryption_key(&config, None).await.unwrap_err();
        assert!(matches!(error, AdminError::Crypto(_)), "{error}");
        assert_eq!(
            error.to_string(),
            "the key comes from LLMOBS_ENCRYPTION_KEY: pass the new key with --new-from-stdin"
        );
        assert!(!crypto::key_path(dir.path()).exists());
        assert!(!dir.path().join("secret.key.new").exists());
        let db = MetaDb::open_existing(&config.data_dir).await.unwrap();
        let sealed = stored_column(&db, &id, "secret_enc").await.unwrap();
        let check = stored_check(&db).await;
        db.close().await;
        assert_eq!(
            SecretBox::new(&old)
                .open(&sealed, connection_aad("secret_enc", &id))
                .unwrap(),
            b"sk-test-key"
        );
        assert_eq!(check, SecretBox::new(&old).check_value());
    }

    #[tokio::test]
    async fn rotation_refuses_while_locked() {
        let (dir, mut config) = ready_config().await;
        let old = [1; 32];
        seed_check(&config, old).await;
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.path().join("LOCK"))
            .unwrap();
        lock.try_lock().unwrap();
        config.encryption_key = Some(Secret::new(crypto::encode_key(&old)));
        let error = rotate_encryption_key(&config, None).await.unwrap_err();
        assert!(matches!(error, AdminError::Locked), "{error}");
        let db = MetaDb::open_existing(&config.data_dir).await.unwrap();
        let check = stored_check(&db).await;
        db.close().await;
        assert_eq!(check, SecretBox::new(&old).check_value());
    }

    #[tokio::test]
    async fn rotation_refuses_a_wrong_current_key() {
        let (_dir, mut config) = ready_config().await;
        let stored = [1; 32];
        let offered = [2; 32];
        let id = Uuid::now_v7();
        {
            let db = MetaDb::open(&config.data_dir).await.unwrap();
            crypto::verify_or_record(
                &db,
                &SecretBox::new(&stored),
                &crypto::key_path(&config.data_dir),
            )
            .await
            .unwrap();
            seed_org(&db).await;
            seed_connection(&db, &id, &SecretBox::new(&stored), false).await;
            db.close().await;
        }
        config.encryption_key = Some(Secret::new(crypto::encode_key(&offered)));
        let error = rotate_encryption_key(&config, None).await.unwrap_err();
        assert!(matches!(error, AdminError::Crypto(_)), "{error}");
        let db = MetaDb::open_existing(&config.data_dir).await.unwrap();
        let sealed = stored_column(&db, &id, "secret_enc").await.unwrap();
        db.close().await;
        let opened = SecretBox::new(&stored)
            .open(&sealed, connection_aad("secret_enc", &id))
            .unwrap();
        assert_eq!(opened, b"sk-test-key");
    }

    #[tokio::test]
    async fn rotation_refuses_the_same_key() {
        let (_dir, mut config) = ready_config().await;
        let old = [1; 32];
        seed_check(&config, old).await;
        config.encryption_key = Some(Secret::new(crypto::encode_key(&old)));
        let error = rotate_encryption_key(&config, Some(&crypto::encode_key(&old)))
            .await
            .unwrap_err();
        assert!(matches!(error, AdminError::SameKey), "{error}");
    }

    #[tokio::test]
    async fn rotation_without_any_key_fails_and_writes_nothing() {
        let (dir, config) = ready_config().await;
        seed_check(&config, [1; 32]).await;
        let error = rotate_encryption_key(&config, None).await.unwrap_err();
        assert!(matches!(error, AdminError::Crypto(_)), "{error}");
        assert_eq!(
            error.to_string(),
            "no encryption key: start the server once first"
        );
        assert!(!crypto::key_path(dir.path()).exists());
        assert!(!dir.path().join("secret.key.new").exists());
    }

    #[tokio::test]
    async fn rotation_rolls_back_on_a_corrupt_value() {
        let (dir, config) = ready_config().await;
        let old = crypto::load_key(None, dir.path(), false).unwrap();
        let old_box = SecretBox::new(&old.key);
        // The bad row is seeded first and its `secret_enc` is valid, so the pass
        // over `secret_enc` re-encrypts both rows before the pass over
        // `extra_headers_enc` fails: a partial update would be detected.
        let bad = Uuid::now_v7();
        let good = Uuid::now_v7();
        let bad_secret_enc;
        let bad_headers_enc;
        {
            let db = MetaDb::open(&config.data_dir).await.unwrap();
            crypto::verify_or_record(&db, &old_box, &crypto::key_path(dir.path()))
                .await
                .unwrap();
            seed_org(&db).await;
            let secret_enc = old_box
                .seal(b"sk-test-key", connection_aad("secret_enc", &bad))
                .unwrap();
            let headers_enc = SecretBox::new(&[9; 32])
                .seal(HEADERS, connection_aad("extra_headers_enc", &bad))
                .unwrap();
            insert_connection(&db, &bad, secret_enc, Some(headers_enc)).await;
            seed_connection(&db, &good, &old_box, true).await;
            bad_secret_enc = stored_column(&db, &bad, "secret_enc").await.unwrap();
            bad_headers_enc = stored_column(&db, &bad, "extra_headers_enc").await.unwrap();
            db.close().await;
        }

        let before = std::fs::read(crypto::key_path(dir.path())).unwrap();
        let error = rotate_encryption_key(&config, None).await.unwrap_err();
        assert!(matches!(error, AdminError::Crypto(_)), "{error}");
        let after = std::fs::read(crypto::key_path(dir.path())).unwrap();
        assert_eq!(before, after);
        assert!(!dir.path().join("secret.key.new").exists());

        let db = MetaDb::open_existing(&config.data_dir).await.unwrap();
        assert_eq!(
            stored_column(&db, &bad, "secret_enc").await.unwrap(),
            bad_secret_enc
        );
        assert_eq!(
            stored_column(&db, &bad, "extra_headers_enc").await.unwrap(),
            bad_headers_enc
        );
        let sealed = stored_column(&db, &good, "secret_enc").await.unwrap();
        let headers = stored_column(&db, &good, "extra_headers_enc")
            .await
            .unwrap();
        let check = stored_check(&db).await;
        db.close().await;
        assert_eq!(
            old_box
                .open(&sealed, connection_aad("secret_enc", &good))
                .unwrap(),
            b"sk-test-key"
        );
        assert_eq!(
            old_box
                .open(&headers, connection_aad("extra_headers_enc", &good))
                .unwrap(),
            HEADERS
        );
        assert_eq!(check, old_box.check_value());
    }

    #[tokio::test]
    async fn rotation_finishes_an_interrupted_rotation_first() {
        let (dir, config) = ready_config().await;
        let old = [1; 32];
        let pending = [2; 32];
        let new = [3; 32];
        let id = Uuid::now_v7();
        // A previous rotation committed the check and `secret.key.new`, then
        // crashed before the rename.
        crypto::write_key_file(&crypto::key_path(dir.path()), &old).unwrap();
        crypto::write_key_file(&dir.path().join("secret.key.new"), &pending).unwrap();
        {
            let db = MetaDb::open(&config.data_dir).await.unwrap();
            crypto::verify_or_record(
                &db,
                &SecretBox::new(&pending),
                &crypto::key_path(dir.path()),
            )
            .await
            .unwrap();
            seed_org(&db).await;
            seed_connection(&db, &id, &SecretBox::new(&pending), false).await;
            db.close().await;
        }

        let rotated = rotate_encryption_key(&config, Some(&crypto::encode_key(&new)))
            .await
            .unwrap();
        assert_eq!(rotated.values, 1);
        assert!(!dir.path().join("secret.key.new").exists());
        let text = std::fs::read_to_string(crypto::key_path(dir.path())).unwrap();
        assert_eq!(crypto::decode_key(text.trim(), "test").unwrap(), new);
        let db = MetaDb::open_existing(&config.data_dir).await.unwrap();
        let sealed = stored_column(&db, &id, "secret_enc").await.unwrap();
        let check = stored_check(&db).await;
        db.close().await;
        let new_box = SecretBox::new(&new);
        assert_eq!(
            new_box
                .open(&sealed, connection_aad("secret_enc", &id))
                .unwrap(),
            b"sk-test-key"
        );
        assert_eq!(check, new_box.check_value());
    }

    #[tokio::test]
    async fn rotation_deletes_a_stale_new_key() {
        let (dir, config) = ready_config().await;
        let old = [1; 32];
        let stale = [7; 32];
        let new = [3; 32];
        let id = Uuid::now_v7();
        crypto::write_key_file(&crypto::key_path(dir.path()), &old).unwrap();
        crypto::write_key_file(&dir.path().join("secret.key.new"), &stale).unwrap();
        {
            let db = MetaDb::open(&config.data_dir).await.unwrap();
            crypto::verify_or_record(&db, &SecretBox::new(&old), &crypto::key_path(dir.path()))
                .await
                .unwrap();
            seed_org(&db).await;
            seed_connection(&db, &id, &SecretBox::new(&old), false).await;
            db.close().await;
        }

        let rotated = rotate_encryption_key(&config, Some(&crypto::encode_key(&new)))
            .await
            .unwrap();
        assert_eq!(rotated.values, 1);
        assert!(!dir.path().join("secret.key.new").exists());
        let text = std::fs::read_to_string(crypto::key_path(dir.path())).unwrap();
        assert_eq!(crypto::decode_key(text.trim(), "test").unwrap(), new);
        let db = MetaDb::open_existing(&config.data_dir).await.unwrap();
        let sealed = stored_column(&db, &id, "secret_enc").await.unwrap();
        let check = stored_check(&db).await;
        db.close().await;
        let new_box = SecretBox::new(&new);
        assert_eq!(
            new_box
                .open(&sealed, connection_aad("secret_enc", &id))
                .unwrap(),
            b"sk-test-key"
        );
        assert_eq!(check, new_box.check_value());
    }

    #[tokio::test]
    async fn rotation_removes_the_tmp_file_when_the_write_fails() {
        let (dir, config) = ready_config().await;
        let old = crypto::load_key(None, dir.path(), false).unwrap();
        let old_box = SecretBox::new(&old.key);
        let id = Uuid::now_v7();
        {
            let db = MetaDb::open(&config.data_dir).await.unwrap();
            crypto::verify_or_record(&db, &old_box, &crypto::key_path(dir.path()))
                .await
                .unwrap();
            seed_org(&db).await;
            seed_connection(&db, &id, &old_box, false).await;
            db.close().await;
        }
        let before = std::fs::read(crypto::key_path(dir.path())).unwrap();
        // A directory at `secret.key.new` makes the rename inside `write_key_file` fail.
        std::fs::create_dir(dir.path().join("secret.key.new")).unwrap();
        let error = rotate_encryption_key(&config, Some(&crypto::encode_key(&[3; 32])))
            .await
            .unwrap_err();
        assert!(matches!(error, AdminError::Crypto(_)), "{error}");
        assert!(!dir.path().join("secret.key.new.tmp").exists());
        assert_eq!(std::fs::read(crypto::key_path(dir.path())).unwrap(), before);
        let db = MetaDb::open_existing(&config.data_dir).await.unwrap();
        let sealed = stored_column(&db, &id, "secret_enc").await.unwrap();
        let check = stored_check(&db).await;
        db.close().await;
        assert_eq!(
            old_box
                .open(&sealed, connection_aad("secret_enc", &id))
                .unwrap(),
            b"sk-test-key"
        );
        assert_eq!(check, old_box.check_value());
    }

    #[tokio::test]
    async fn rotation_on_a_missing_database_names_serve() {
        let app = TestApp::new().await;
        let empty = tempfile::tempdir().unwrap();
        let mut config = app.state.config.clone();
        config.data_dir = empty.path().to_path_buf();
        let error = rotate_encryption_key(&config, None).await.unwrap_err();
        assert!(matches!(error, AdminError::NotInitialized), "{error}");
        assert!(!empty.path().join("LOCK").exists());
    }

    #[test]
    fn rotated_debug_prints_only_the_values() {
        let rotated = Rotated { values: 3 };
        assert_eq!(format!("{rotated:?}"), "Rotated { values: 3 }");
    }

    fn reset_hash(url: &str) -> String {
        let token = url.split("token=").nth(1).expect("reset url");
        hash_token(token)
    }

    /// A trace id for test `n`. Never all zero.
    fn trace(n: u128) -> TraceId {
        TraceId::from_bytes((n + 1).to_be_bytes()).unwrap()
    }

    fn did(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    /// The org and projects the source rows refer to, as ids in the target.
    struct Ids {
        org: OrgId,
        p1: ProjectId,
        p2: ProjectId,
        org2: OrgId,
        p0: ProjectId,
    }

    fn fresh_ids() -> Ids {
        Ids {
            org: OrgId::from_uuid(Uuid::now_v7()),
            p1: ProjectId::from_uuid(Uuid::now_v7()),
            p2: ProjectId::from_uuid(Uuid::now_v7()),
            org2: OrgId::from_uuid(Uuid::now_v7()),
            p0: ProjectId::from_uuid(Uuid::now_v7()),
        }
    }

    /// Row `n` of the source: created at `n * 10`, actor `100 + n`.
    fn new_row(
        n: u128,
        kind: DeletionKind,
        org: OrgId,
        project: Option<ProjectId>,
        state: DeletionState,
    ) -> NewDeletion {
        NewDeletion {
            id: did(n),
            org_id: org,
            project_id: project,
            kind,
            user_id: (kind == DeletionKind::User).then(|| "u1".to_owned()),
            request: RequestDetails::default(),
            actor_kind: ActorKind::User,
            actor_id: Uuid::from_u128(100 + n),
            state,
            created_at: (n as i64) * 10,
        }
    }

    async fn put(
        db: &MetaDb,
        new: NewDeletion,
        traces: Vec<TraceId>,
        sessions: Vec<String>,
        projects: Vec<ProjectId>,
    ) {
        let id = new.id;
        db.write(move |conn| {
            Box::pin(async move {
                insert_deletion(conn, &new).await?;
                insert_traces(conn, id, &traces).await?;
                insert_sessions(conn, id, &sessions).await?;
                insert_projects(conn, id, &projects).await
            })
        })
        .await
        .unwrap();
    }

    async fn set_counts(db: &MetaDb, id: Uuid, traces: u64, sessions: Option<u64>) {
        db.write(move |conn| {
            Box::pin(async move { store_counts(conn, id, traces, sessions).await })
        })
        .await
        .unwrap();
    }

    async fn set_handed_over(db: &MetaDb, id: Uuid) {
        db.write(move |conn| Box::pin(async move { mark_handed_over(conn, id, 11).await }))
            .await
            .unwrap();
    }

    async fn set_applied(db: &MetaDb, id: Uuid) {
        db.write(move |conn| Box::pin(async move { mark_applied(conn, id, 12).await }))
            .await
            .unwrap();
    }

    async fn set_dropped(db: &MetaDb, id: Uuid) {
        db.write(move |conn| Box::pin(async move { mark_dropped(conn, id, 13).await }))
            .await
            .unwrap();
    }

    async fn set_failed(db: &MetaDb, id: Uuid, reason: FailReason) {
        db.write(move |conn| Box::pin(async move { mark_failed(conn, id, reason).await }))
            .await
            .unwrap();
    }

    /// A target with the org and projects of `Ids`, under the ids the database gives them.
    async fn target_with_projects(config: &ServerConfig) -> Ids {
        let db = MetaDb::open_existing(&config.data_dir).await.unwrap();
        let ids = db
            .write(|conn| {
                Box::pin(async move {
                    let org = insert_org(conn, "O", "o", 1).await?;
                    let p1 = insert_project(conn, org.id, "p1", "p1", None, 1).await?;
                    let p2 = insert_project(conn, org.id, "p2", "p2", None, 1).await?;
                    let org2 = insert_org(conn, "O2", "o2", 1).await?;
                    let p0 = insert_project(conn, org2.id, "p0", "p0", None, 1).await?;
                    Ok(Ids {
                        org: org.id,
                        p1: p1.id,
                        p2: p2.id,
                        org2: org2.id,
                        p0: p0.id,
                    })
                })
            })
            .await
            .unwrap();
        db.close().await;
        ids
    }

    /// A source `meta.db` in a new directory with the six rows of the first test: each case
    /// of the plan's copy list, with the transitions that leave it in that state.
    async fn build_source(ids: &Ids) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let db = MetaDb::open(dir.path()).await.unwrap();
        // (a) resolved trace_ids request, handed over and applied.
        put(
            &db,
            new_row(
                1,
                DeletionKind::TraceIds,
                ids.org,
                Some(ids.p1),
                DeletionState::Resolving,
            ),
            vec![trace(1), trace(2)],
            vec![],
            vec![],
        )
        .await;
        set_counts(&db, did(1), 2, None).await;
        set_handed_over(&db, did(1)).await;
        set_applied(&db, did(1)).await;
        // (b) user request, resolved, pending.
        put(
            &db,
            new_row(
                2,
                DeletionKind::User,
                ids.org,
                Some(ids.p1),
                DeletionState::Resolving,
            ),
            vec![trace(3)],
            vec!["s1".to_owned()],
            vec![],
        )
        .await;
        set_counts(&db, did(2), 1, Some(1)).await;
        set_handed_over(&db, did(2)).await;
        // (c) filter request, still resolving, with its request text.
        let mut filter = new_row(
            3,
            DeletionKind::Filter,
            ids.org,
            Some(ids.p1),
            DeletionState::Resolving,
        );
        filter.request = RequestDetails {
            filter: Some("model = 'gpt'".to_owned()),
            ..RequestDetails::default()
        };
        put(&db, filter, vec![], vec![], vec![]).await;
        // (d) project drop of P0, applied.
        put(
            &db,
            new_row(
                4,
                DeletionKind::Project,
                ids.org2,
                Some(ids.p0),
                DeletionState::Pending,
            ),
            vec![],
            vec![],
            vec![],
        )
        .await;
        set_dropped(&db, did(4)).await;
        // (e) org drop of O, applied, with its projects.
        put(
            &db,
            new_row(5, DeletionKind::Org, ids.org, None, DeletionState::Pending),
            vec![],
            vec![],
            vec![ids.p1, ids.p2],
        )
        .await;
        set_dropped(&db, did(5)).await;
        // (f) trace_ids request that failed.
        put(
            &db,
            new_row(
                6,
                DeletionKind::TraceIds,
                ids.org,
                Some(ids.p1),
                DeletionState::Resolving,
            ),
            vec![trace(5)],
            vec![],
            vec![],
        )
        .await;
        set_failed(&db, did(6), FailReason::ResolutionFailed).await;
        db.close().await;
        dir
    }

    async fn row_of(db: &MetaDb, n: u128) -> DeletionRow {
        deletion_by_id(db.reader(), did(n)).await.unwrap().unwrap()
    }

    async fn project_state(db: &MetaDb, id: ProjectId) -> String {
        sqlx::query_scalar::<_, String>("SELECT state FROM projects WHERE id = ?")
            .bind(id.to_string())
            .fetch_one(db.reader())
            .await
            .unwrap()
    }

    async fn org_state(db: &MetaDb, id: OrgId) -> String {
        sqlx::query_scalar::<_, String>("SELECT state FROM orgs WHERE id = ?")
            .bind(id.to_string())
            .fetch_one(db.reader())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn copies_missing_rows_as_open_rows() {
        let (_dir, config) = ready_config().await;
        let ids = target_with_projects(&config).await;
        let source = build_source(&ids).await;
        let report = reapply_deletions(&config, source.path()).await.unwrap();
        assert_eq!(
            report,
            ReapplyReport {
                reopened: 5,
                failed_copied: 1,
                present: 0
            }
        );

        let db = MetaDb::open_existing(&config.data_dir).await.unwrap();
        for (n, kind) in [
            (1, DeletionKind::TraceIds),
            (2, DeletionKind::User),
            (3, DeletionKind::Filter),
            (4, DeletionKind::Project),
            (5, DeletionKind::Org),
            (6, DeletionKind::TraceIds),
        ] {
            let row = row_of(&db, n).await;
            assert_eq!(row.kind, kind);
            assert_eq!(row.created_at, (n as i64) * 10, "row {n}");
            assert_eq!(row.actor_id, Uuid::from_u128(100 + n), "row {n}");
        }

        // (a) resolved scope, open again.
        let a = row_of(&db, 1).await;
        assert_eq!(a.state, DeletionState::Resolving);
        assert_eq!(a.trace_count, Some(2));
        assert_eq!(a.handed_over_at, None);
        assert_eq!(a.applied_at, None);
        assert_eq!(
            scope_traces(db.reader(), did(1)).await.unwrap(),
            vec![trace(1), trace(2)]
        );

        // (b) user request keeps its counts, user and sessions.
        let b = row_of(&db, 2).await;
        assert_eq!(b.state, DeletionState::Resolving);
        assert_eq!(b.trace_count, Some(1));
        assert_eq!(b.session_count, Some(1));
        assert_eq!(b.user_id.as_deref(), Some("u1"));
        assert_eq!(b.handed_over_at, None);
        assert_eq!(
            scope_traces(db.reader(), did(2)).await.unwrap(),
            vec![trace(3)]
        );
        assert_eq!(
            scope_sessions(db.reader(), did(2)).await.unwrap(),
            vec!["s1".to_owned()]
        );

        // (c) unresolved filter request keeps its text.
        let c = row_of(&db, 3).await;
        assert_eq!(c.state, DeletionState::Resolving);
        assert_eq!(c.trace_count, None);
        assert_eq!(c.request.filter.as_deref(), Some("model = 'gpt'"));

        // (d) project drop: pending, and P0 hidden at once.
        let d = row_of(&db, 4).await;
        assert_eq!(d.state, DeletionState::Pending);
        assert_eq!(d.handed_over_at, None);
        assert_eq!(d.applied_at, None);
        assert_eq!(project_state(&db, ids.p0).await, "deleting");

        // (e) org drop: pending, with its projects, all hidden at once.
        let e = row_of(&db, 5).await;
        assert_eq!(e.state, DeletionState::Pending);
        assert_eq!(e.handed_over_at, None);
        assert_eq!(scope_projects(db.reader(), did(5)).await.unwrap(), {
            let mut expected = vec![ids.p1, ids.p2];
            expected.sort();
            expected
        });
        assert_eq!(org_state(&db, ids.org).await, "deleting");
        assert_eq!(project_state(&db, ids.p1).await, "deleting");
        assert_eq!(project_state(&db, ids.p2).await, "deleting");
        assert_eq!(org_state(&db, ids.org2).await, "active");

        // (f) failed stays failed, with its error.
        let f = row_of(&db, 6).await;
        assert_eq!(f.state, DeletionState::Failed);
        assert_eq!(f.error, Some(FailReason::ResolutionFailed));
        db.close().await;
    }

    #[tokio::test]
    async fn second_run_copies_nothing() {
        let (_dir, config) = ready_config().await;
        let ids = target_with_projects(&config).await;
        let source = build_source(&ids).await;
        reapply_deletions(&config, source.path()).await.unwrap();
        let again = reapply_deletions(&config, source.path()).await.unwrap();
        assert_eq!(
            again,
            ReapplyReport {
                reopened: 0,
                failed_copied: 0,
                present: 6
            }
        );
    }

    #[tokio::test]
    async fn rows_already_present_are_left_alone() {
        let (_dir, config) = ready_config().await;
        let ids = fresh_ids();
        let source = build_source(&ids).await;
        let target = MetaDb::open_existing(&config.data_dir).await.unwrap();
        put(
            &target,
            new_row(
                1,
                DeletionKind::TraceIds,
                ids.org,
                Some(ids.p1),
                DeletionState::Resolving,
            ),
            vec![],
            vec![],
            vec![],
        )
        .await;
        set_applied(&target, did(1)).await;
        target.close().await;

        let report = reapply_deletions(&config, source.path()).await.unwrap();
        assert_eq!(
            report,
            ReapplyReport {
                reopened: 4,
                failed_copied: 1,
                present: 1
            }
        );
        let db = MetaDb::open_existing(&config.data_dir).await.unwrap();
        assert_eq!(row_of(&db, 1).await.state, DeletionState::Applied);
        db.close().await;
    }

    #[tokio::test]
    async fn missing_source_is_refused() {
        let (_dir, config) = ready_config().await;
        let empty = tempfile::tempdir().unwrap();
        let error = reapply_deletions(&config, empty.path()).await.unwrap_err();
        assert!(matches!(error, AdminError::SourceNotInitialized), "{error}");
    }

    #[tokio::test]
    async fn source_at_another_schema_is_refused() {
        let (_dir, config) = ready_config().await;
        let source = tempfile::tempdir().unwrap();
        let db = MetaDb::open(source.path()).await.unwrap();
        let expected = crate::meta::expected_version();
        let future = expected + 1;
        db.write(move |connection| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO _sqlx_migrations \
                     (version, description, success, checksum, execution_time) \
                     VALUES (?, 'future', 1, ?, 0)",
                )
                .bind(future)
                .bind(Vec::<u8>::new())
                .execute(&mut *connection)
                .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        db.close().await;
        let error = reapply_deletions(&config, source.path()).await.unwrap_err();
        assert!(
            matches!(
                error,
                AdminError::SourceSchema { expected: e, found } if e == expected && found == future
            ),
            "{error}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn copied_rows_are_applied_by_the_worker() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let project = &seeded.project;

        // Trace 1 exists before the deletion's cut-off, so the deletion covers it.
        let t0 = app.state.clock.now_ns();
        let mut span = dataset_span(0);
        span.trace_id = trace(1);
        span.span_id = SpanId::from_bytes([1, 0, 0, 0, 0, 0, 0, 1]).unwrap();
        span.parent_span_id = None;
        span.user_id = None;
        span.session_id = None;
        span.environment = None;
        span.metadata.clear();
        span.ingest_time = t0;
        app.commit_spans(project, vec![span]).await;
        app.state.clock.advance(Duration::from_secs(1));

        // The deletion made after the backup, in the source directory.
        let source = tempfile::tempdir().unwrap();
        let db = MetaDb::open(source.path()).await.unwrap();
        let mut new = new_row(
            7,
            DeletionKind::TraceIds,
            seeded.org_id,
            Some(project.id),
            DeletionState::Resolving,
        );
        new.created_at = app.state.clock.now_ns();
        put(&db, new, vec![trace(1)], vec![], vec![]).await;
        set_counts(&db, did(7), 1, None).await;
        set_applied(&db, did(7)).await;
        db.close().await;

        let report = reapply_deletions(&app.state.config, source.path())
            .await
            .unwrap();
        assert_eq!(report.reopened, 1);

        crate::deletion::run_once(&app.state).await.unwrap();
        let row = deletion_by_id(app.state.meta.reader(), did(7))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, DeletionState::Pending);
        let status = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}/traces/{}", project.id, trace(1)),
            )
            .header("cookie", &seeded.cookie)
            .send()
            .await
            .status;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
