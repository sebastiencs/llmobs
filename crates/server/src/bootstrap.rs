//! First user, org, and project (spec 08 § Bootstrapping, D144).
//!
//! Headless `[init]` runs in one transaction when `users` is empty. Otherwise
//! a setup token is kept in memory and logged once. That log line is the only
//! secret this process writes.

use std::sync::Mutex;

use crate::api::account::OrgRole;
use crate::auth::AuthError;
use crate::auth::password::{Hasher, PolicyError, check_policy};
use crate::auth::roles::KeyScope;
use crate::auth::secret::{
    KeyKind, display_prefix, hash_token, new_key, new_opaque_token, parse_key,
};
use crate::config::InitConfig;
use crate::http::AppState;
use crate::meta::MetaError;
use crate::meta::api_keys::{ApiKeyRow, NewApiKey, insert_api_key};
use crate::meta::members::add_org_member;
use crate::meta::orgs::{OrgRow, insert_org_with_derived_slug};
use crate::meta::projects::{ProjectRow, insert_project_with_derived_slug};
use crate::meta::users::{
    NewUser, UserRow, count_users, count_users_tx, insert_user, normalize_email,
};

const FULL_SCOPES: [KeyScope; 3] = [KeyScope::Ingest, KeyScope::Read, KeyScope::Write];

/// The setup token, when first-run setup is still open.
pub(crate) struct SetupState {
    token: Mutex<Option<String>>,
}

impl SetupState {
    /// No token. [`prepare`] fills it when the database has no user and no `[init]`.
    pub(crate) fn new() -> Self {
        Self {
            token: Mutex::new(None),
        }
    }

    /// The current token. `None` means setup is closed.
    ///
    /// # Errors
    ///
    /// Returns [`BootstrapError::Lock`] when a holder panicked.
    pub(crate) fn token(&self) -> Result<Option<String>, BootstrapError> {
        Ok(self.token.lock().map_err(|_| BootstrapError::Lock)?.clone())
    }

    /// Forgets the token after a successful setup.
    ///
    /// # Errors
    ///
    /// Returns [`BootstrapError::Lock`] when a holder panicked.
    pub(crate) fn clear(&self) -> Result<(), BootstrapError> {
        *self.token.lock().map_err(|_| BootstrapError::Lock)? = None;
        Ok(())
    }

    fn set(&self, token: String) -> Result<(), BootstrapError> {
        *self.token.lock().map_err(|_| BootstrapError::Lock)? = Some(token);
        Ok(())
    }
}

/// Why first-run setup or headless init failed.
///
/// `Init` names the rule or the variable. It does not include the password,
/// the key, the email, or the org name.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BootstrapError {
    /// `[init]` failed a check. Startup stops.
    #[error("init failed: {reason}")]
    Init {
        /// The rule or the variable name.
        reason: String,
    },
    /// `meta.db` rejected the write.
    #[error(transparent)]
    Meta(#[from] MetaError),
    /// The random source or argon2 failed.
    #[error(transparent)]
    Auth(#[from] AuthError),
    /// A setup-token lock holder panicked.
    #[error("setup token lock poisoned")]
    Lock,
}

/// What one transaction created.
pub(crate) struct Provisioned {
    /// The instance admin.
    pub(crate) user: UserRow,
    /// The org.
    pub(crate) org: OrgRow,
    /// The project.
    pub(crate) project: ProjectRow,
    /// The secret key, when one was requested.
    pub(crate) key: Option<ApiKeyRow>,
}

/// Values for [`provision`]. The password is already a PHC string.
struct Provision {
    email: String,
    name: Option<String>,
    password_hash: String,
    org_name: String,
    project_name: String,
    key: Option<KeyMaterial>,
}

struct KeyMaterial {
    name: String,
    prefix: String,
    hash: String,
}

/// Users exist, `[init]` is applied, or a setup token is logged once.
///
/// # Errors
///
/// Returns [`BootstrapError::Init`] when `[init]` fails a check. The database
/// is unchanged. A database or hashing failure is [`BootstrapError::Meta`] or
/// [`BootstrapError::Auth`].
pub(crate) async fn prepare(state: &AppState) -> Result<(), BootstrapError> {
    if count_users(state.meta.reader()).await? > 0 {
        return Ok(());
    }
    if init_complete(&state.config.init) {
        apply_init(state).await?;
        return Ok(());
    }
    let token = new_opaque_token()?;
    let url = format!(
        "{}?token={}",
        state.config.public_url.join("/setup"),
        token.plaintext
    );
    tracing::warn!(target: "setup", "setup required: open {url}");
    state.setup.set(token.plaintext)?;
    Ok(())
}

/// Creates the first admin from `[init]` when `users` is still empty.
///
/// Policy and the secret-key shape are checked before the transaction, so
/// argon2 does not hold the SQLite write lock. The inserts and the empty-table
/// re-check are one write. A crash before commit leaves an empty database.
///
/// # Errors
///
/// Returns [`BootstrapError::Init`] for a bad email, name, password, or secret
/// key. The reason names the rule or `LLMOBS_INIT_SECRET_KEY`, not the value.
pub(crate) async fn apply_init(state: &AppState) -> Result<(), BootstrapError> {
    let init = state.config.init.clone();
    let email = init
        .admin_email
        .as_deref()
        .ok_or_else(|| init_error("init.admin_email is unset"))?;
    let email = normalize_email(email).map_err(|_| init_error("init.admin_email is invalid"))?;
    let password = init
        .admin_password
        .as_ref()
        .map(|secret| secret.expose().clone())
        .ok_or_else(|| init_error("init.admin_password is unset"))?;
    let org_name = required_name(init.org.as_deref(), "init.org")?;
    let project_name = required_name(init.project.as_deref(), "init.project")?;
    check_policy(&password, &email).map_err(|error| init_error(policy_reason(error)))?;
    let key = match init.secret_key.as_ref() {
        None => None,
        Some(secret) => Some(provided_key(secret.expose())?),
    };
    let hasher = Hasher::new().await?;
    let password_hash = hasher.hash(&password).await?;
    match provision(
        state,
        Provision {
            email,
            name: None,
            password_hash,
            org_name,
            project_name,
            key,
        },
    )
    .await?
    {
        None => {
            tracing::info!("init skipped: users exist");
            Ok(())
        }
        Some(created) => {
            audit("init applied", &created);
            Ok(())
        }
    }
}

/// Inserts the first admin, org, project, and optional secret key.
///
/// `None` means a user appeared before this transaction. The caller answers
/// 409 or logs that init was skipped.
///
/// # Errors
///
/// Returns [`BootstrapError`] when the write fails.
async fn provision(
    state: &AppState,
    input: Provision,
) -> Result<Option<Provisioned>, BootstrapError> {
    let now = state.clock.now_ns();
    let created = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                if count_users_tx(conn).await? != 0 {
                    return Ok(None);
                }
                Ok(Some(insert_first(conn, &input, now).await?))
            })
        })
        .await?;
    Ok(created)
}

async fn insert_first(
    conn: &mut sqlx::SqliteConnection,
    input: &Provision,
    now: i64,
) -> Result<Provisioned, MetaError> {
    let user = insert_user(
        conn,
        NewUser {
            email: input.email.clone(),
            name: input.name.clone(),
            password_hash: Some(input.password_hash.clone()),
            email_verified_at: Some(now),
            is_instance_admin: true,
        },
        now,
    )
    .await?;
    let org = insert_org_with_derived_slug(conn, &input.org_name, now).await?;
    add_org_member(conn, org.id, user.id, OrgRole::Owner, now).await?;
    let project =
        insert_project_with_derived_slug(conn, org.id, &input.project_name, None, now).await?;
    let key = match &input.key {
        None => None,
        Some(key) => Some(
            insert_api_key(
                conn,
                NewApiKey {
                    project_id: project.id,
                    kind: "secret",
                    scopes: FULL_SCOPES.to_vec(),
                    name: Some(key.name.clone()),
                    prefix: key.prefix.clone(),
                    hash: key.hash.clone(),
                    created_by: user.id,
                    expires_at: None,
                },
                now,
            )
            .await?,
        ),
    };
    Ok(Provisioned {
        user,
        org,
        project,
        key,
    })
}

fn init_complete(init: &InitConfig) -> bool {
    init.admin_email.is_some()
}

fn required_name(value: Option<&str>, key: &'static str) -> Result<String, BootstrapError> {
    let Some(value) = value else {
        return Err(init_error(match key {
            "init.org" => "init.org is unset",
            _ => "init.project is unset",
        }));
    };
    let name = value.trim();
    let length = name.chars().count();
    if !(1..=100).contains(&length) {
        return Err(init_error(match key {
            "init.org" => "init.org must be 1 to 100 characters",
            _ => "init.project must be 1 to 100 characters",
        }));
    }
    Ok(name.to_owned())
}

fn provided_key(plaintext: &str) -> Result<KeyMaterial, BootstrapError> {
    if parse_key(plaintext) != Some(KeyKind::Secret) {
        return Err(init_error("LLMOBS_INIT_SECRET_KEY is not a secret key"));
    }
    Ok(KeyMaterial {
        name: "init".to_owned(),
        prefix: display_prefix(plaintext),
        hash: hash_token(plaintext),
    })
}

fn policy_reason(error: PolicyError) -> &'static str {
    match error {
        PolicyError::TooShort => "password is too short",
        PolicyError::TooLong => "password is too long",
        PolicyError::EqualsEmail => "password matches the email",
        PolicyError::TooCommon => "password is too common",
    }
}

fn init_error(reason: &str) -> BootstrapError {
    BootstrapError::Init {
        reason: reason.to_owned(),
    }
}

fn audit(message: &'static str, created: &Provisioned) {
    let user_id = created.user.id;
    let org_id = created.org.id;
    let project_id = created.project.id;
    if let Some(key) = &created.key {
        let key_id = key.id;
        tracing::info!(target: "audit", %user_id, %org_id, %project_id, %key_id, "{message}");
    } else {
        tracing::info!(target: "audit", %user_id, %org_id, %project_id, "{message}");
    }
}

/// A completed `POST /api/auth/setup` write. `secret_key` is shown once.
pub(crate) struct SetupCreated {
    /// The rows that were inserted.
    pub(crate) created: Provisioned,
    /// Secret API key plaintext.
    pub(crate) secret_key: String,
    /// Type prefix plus the first 6 tail characters.
    pub(crate) secret_key_prefix: String,
}

/// Hashes the password, mints a secret key, and inserts the first admin.
///
/// `None` means a user appeared first. The plaintext key is not logged.
///
/// # Errors
///
/// Returns [`BootstrapError`] when hashing or the write fails.
pub(crate) async fn create_setup(
    state: &AppState,
    email: String,
    name: Option<String>,
    password: &str,
    org_name: String,
    project_name: String,
) -> Result<Option<SetupCreated>, BootstrapError> {
    let minted = new_key(KeyKind::Secret)?;
    let secret_key_prefix = minted.prefix.clone();
    let password_hash = Hasher::new().await?.hash(password).await?;
    let created = provision(
        state,
        Provision {
            email,
            name,
            password_hash,
            org_name,
            project_name,
            key: Some(KeyMaterial {
                name: "setup".to_owned(),
                prefix: minted.prefix,
                hash: minted.hash,
            }),
        },
    )
    .await?;
    Ok(created.map(|created| SetupCreated {
        created,
        secret_key: minted.plaintext,
        secret_key_prefix,
    }))
}

#[cfg(test)]
mod tests {
    use super::{apply_init, prepare};
    use crate::auth::secret::{KeyKind, hash_token, new_key};
    use crate::meta::users::count_users;
    use crate::test_support::{TestApp, capture_logs};

    fn init(value: serde_json::Value) -> crate::config::InitConfig {
        serde_json::from_value(value).unwrap()
    }

    #[tokio::test]
    async fn init_creates_the_admin_once() {
        let key = new_key(KeyKind::Secret).unwrap();
        let app = TestApp::with_config(|config| {
            config.init = init(serde_json::json!({
                "admin_email": "Ada@Example.com",
                "admin_password": "llmobs-ok1",
                "org": " Acme ",
                "project": "Support bot",
                "secret_key": key.plaintext.clone(),
            }));
        })
        .await;
        let (prepared, logs) = capture_logs(prepare(&app.state)).await;
        prepared.unwrap();
        assert!(logs.contains("init applied"), "{logs}");
        assert!(!logs.contains("setup required"), "{logs}");
        assert!(!logs.contains(&key.plaintext), "{logs}");
        assert!(app.state.setup.token().unwrap().is_none());
        assert_eq!(count_users(app.state.meta.reader()).await.unwrap(), 1);
        let (hash, name): (String, String) = sqlx::query_as("SELECT key_hash, name FROM api_keys")
            .fetch_one(app.state.meta.reader())
            .await
            .unwrap();
        assert_eq!(hash, hash_token(&key.plaintext));
        assert_eq!(name, "init");
        let (again_result, again) = capture_logs(prepare(&app.state)).await;
        again_result.unwrap();
        assert!(!again.contains("init applied"), "{again}");
        assert!(!again.contains("setup required"), "{again}");
        assert_eq!(count_users(app.state.meta.reader()).await.unwrap(), 1);
        let (skipped_result, skipped) = capture_logs(apply_init(&app.state)).await;
        skipped_result.unwrap();
        assert!(skipped.contains("init skipped: users exist"), "{skipped}");
        let orgs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orgs")
            .fetch_one(app.state.meta.reader())
            .await
            .unwrap();
        assert_eq!(orgs, 1);
    }

    #[tokio::test]
    async fn a_bad_init_secret_or_password_stops_startup() {
        let bad_key = TestApp::with_config(|config| {
            config.init = init(serde_json::json!({
                "admin_email": "ada@example.com",
                "admin_password": "llmobs-ok1",
                "org": "Acme",
                "project": "Support bot",
                "secret_key": "not-a-secret-key",
            }));
        })
        .await;
        let error = prepare(&bad_key.state).await.unwrap_err();
        let text = error.to_string();
        assert!(text.contains("LLMOBS_INIT_SECRET_KEY"), "{text}");
        assert!(!text.contains("not-a-secret-key"), "{text}");
        assert_eq!(count_users(bad_key.state.meta.reader()).await.unwrap(), 0);

        let weak = TestApp::with_config(|config| {
            config.init = init(serde_json::json!({
                "admin_email": "ada@example.com",
                "admin_password": "nope-nope",
                "org": "Acme",
                "project": "Support bot",
            }));
        })
        .await;
        let error = prepare(&weak.state).await.unwrap_err();
        let text = error.to_string();
        assert!(text.contains("password is too short"), "{text}");
        assert!(!text.contains("nope-nope"), "{text}");
        assert_eq!(count_users(weak.state.meta.reader()).await.unwrap(), 0);
    }
}
