//! Local admin operations on an existing `meta.db` (spec 09 § CLI).
//!
//! These open the database without migrating and without the data-dir lock, so
//! they work while `llmobs serve` is running (D136).

use uuid::Uuid;

use crate::auth::AuthError;
use crate::auth::password::{Hasher, check_policy};
use crate::auth::reset::{ResetError, mint_reset, reset_link};
use crate::clock::Clock;
use crate::config::ServerConfig;
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
    use crate::meta::password_resets::valid_reset;
    use crate::meta::users::{set_disabled, user_by_email};
    use crate::test_support::TestApp;

    const PASSWORD: &str = "llmobs-ok1";

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
        db.write(|connection| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO _sqlx_migrations \
                     (version, description, success, checksum, execution_time) \
                     VALUES (4, 'future', 1, ?, 0)",
                )
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
        assert_eq!(error.to_string(), "this binary expects schema 3, found 4");
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

    fn reset_hash(url: &str) -> String {
        let token = url.split("token=").nth(1).expect("reset url");
        hash_token(token)
    }
}
