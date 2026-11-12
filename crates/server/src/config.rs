//! Process configuration for `llmobs serve` (spec 12).
//!
//! [`ServerConfig::load`] merges serde defaults, `llmobs.toml`, and `LLMOBS_*`
//! environment variables. Later sources win. A secret's [`Debug`] output is
//! `Secret(***)`, and a rejected secret is reported by key only.
//!
//! The `[storage]` table is [`llmobs_storage::config::StorageConfig`]. That
//! type requires `data_dir`, which is not a `[storage]` key here: a user-set
//! `storage.data_dir` is an error, and the top-level `data_dir` is injected
//! before the table is deserialized. [`StorageConfig::validate`] then runs
//! with the same field paths (`storage.wal.sync`, and so on).

use std::collections::BTreeSet;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use figment::providers::{Format, Serialized, Toml};
use figment::value::{Dict, Map, Value};
use figment::{Figment, Metadata, Profile, Provider};
use ipnet::IpNet;
use lettre::message::Mailbox;
use llmobs_storage::config::StorageConfig;
use serde::Deserialize;
use serde::de::{self, Deserializer, SeqAccess, Visitor};

use crate::api::account::SignupPolicy;

/// `admin_email`, `admin_password`, `org`, and `project` are one group.
const INIT_GROUP: &str =
    "set admin_email, admin_password, org, and project together, or leave them all unset";

/// Environment names that map to a different key than `LLMOBS_` + `__` splitting.
///
/// These names are not also read by the split provider. `LLMOBS_AUTH_SIGNUP`
/// would otherwise become a top-level `auth_signup` and fail
/// [`deny_unknown_fields`](https://serde.rs/container-attrs.html#deny_unknown_fields).
const ALIASES: &[(&str, &str)] = &[
    ("LLMOBS_DATA_DIR", "data_dir"),
    ("LLMOBS_PUBLIC_URL", "public_url"),
    ("LLMOBS_HTTP_ADDR", "http_addr"),
    ("LLMOBS_TRUSTED_PROXIES", "trusted_proxies"),
    ("LLMOBS_OBJECT_STORE", "storage.object_store"),
    ("LLMOBS_SMTP_URL", "smtp_url"),
    ("LLMOBS_EMAIL_FROM", "email_from"),
    ("LLMOBS_AUTH_SIGNUP", "auth.signup"),
    ("LLMOBS_AUTH_SIGNUP_DOMAINS", "auth.signup_domains"),
    ("LLMOBS_AUTH_DISABLE_PASSWORD", "auth.disable_password"),
    (
        "LLMOBS_AUTH_REQUIRE_EMAIL_VERIFICATION",
        "auth.require_email_verification",
    ),
    (
        "LLMOBS_AUTH_ENFORCE_SSO_DOMAINS",
        "auth.enforce_sso_domains",
    ),
    ("LLMOBS_INIT_ADMIN_EMAIL", "init.admin_email"),
    ("LLMOBS_INIT_ADMIN_PASSWORD", "init.admin_password"),
    ("LLMOBS_INIT_ORG", "init.org"),
    ("LLMOBS_INIT_PROJECT", "init.project"),
    ("LLMOBS_INIT_SECRET_KEY", "init.secret_key"),
    ("LLMOBS_LOG", "log.level"),
];

/// A value that must not appear in logs or error text.
///
/// [`Debug`] prints `Secret(***)`. There is no [`Display`] impl.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// The value this secret holds.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret(***)")
    }
}

impl<'de> Deserialize<'de> for Secret<String> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(SecretVisitor)
    }
}

macro_rules! reject_secret {
    ($($method:ident($ty:ty)),+ $(,)?) => {
        $(
            fn $method<E: de::Error>(self, _value: $ty) -> Result<Secret<String>, E> {
                Err(E::custom("expected a string"))
            }
        )+
    };
}

struct SecretVisitor;

impl<'de> Visitor<'de> for SecretVisitor {
    type Value = Secret<String>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a string")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Secret<String>, E> {
        Ok(Secret(value.to_owned()))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Secret<String>, E> {
        Err(E::custom("expected a string"))
    }

    fn visit_none<E: de::Error>(self) -> Result<Secret<String>, E> {
        Err(E::custom("expected a string"))
    }

    fn visit_some<D: Deserializer<'de>>(
        self,
        _deserializer: D,
    ) -> Result<Secret<String>, D::Error> {
        Err(de::Error::custom("expected a string"))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, _seq: A) -> Result<Secret<String>, A::Error> {
        Err(de::Error::custom("expected a string"))
    }

    fn visit_map<A: de::MapAccess<'de>>(self, _map: A) -> Result<Secret<String>, A::Error> {
        Err(de::Error::custom("expected a string"))
    }

    reject_secret!(
        visit_bool(bool),
        visit_i8(i8),
        visit_i16(i16),
        visit_i32(i32),
        visit_i64(i64),
        visit_i128(i128),
        visit_u8(u8),
        visit_u16(u16),
        visit_u32(u32),
        visit_u64(u64),
        visit_u128(u128),
        visit_f32(f32),
        visit_f64(f64),
        visit_char(char),
    );
}

/// `http` or `https` origin used in links and cookies.
///
/// The path is empty or `/`. A query, a fragment, or userinfo is rejected so a
/// password cannot sit in the URL.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicUrl {
    origin: String,
    https: bool,
}

impl PublicUrl {
    /// `scheme://host[:port]` with no trailing slash.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Whether the scheme is `https`.
    pub fn is_https(&self) -> bool {
        self.https
    }

    /// `origin` joined with `path`. A path without a leading `/` gets one.
    pub fn join(&self, path: &str) -> String {
        if path.is_empty() {
            return self.origin.clone();
        }
        if path.starts_with('/') {
            format!("{}{path}", self.origin)
        } else {
            format!("{}/{path}", self.origin)
        }
    }
}

impl Default for PublicUrl {
    fn default() -> Self {
        Self {
            origin: "http://localhost:8080".to_owned(),
            https: false,
        }
    }
}

impl fmt::Debug for PublicUrl {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublicUrl")
            .field("origin", &self.origin)
            .finish()
    }
}

impl<'de> Deserialize<'de> for PublicUrl {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        parse_public_url(&raw).map_err(de::Error::custom)
    }
}

/// Who may register, password and SSO flags, and the OAuth providers (spec 08).
///
/// GitHub and Google accept the split env form (`LLMOBS_AUTH__GITHUB__CLIENT_ID`,
/// `LLMOBS_AUTH__GOOGLE__CLIENT_SECRET`, and the other scalar fields).
/// `[[auth.oidc]]` is TOML only. `LLMOBS_AUTH__OIDC__*` is ignored.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthSettings {
    /// Who may create an account after the first user exists.
    #[serde(default = "default_signup")]
    pub signup: SignupPolicy,
    /// Email domains allowed to sign up. Empty means no domain restriction.
    #[serde(default, deserialize_with = "deserialize_string_list")]
    pub signup_domains: Vec<String>,
    /// Refuse password login and password signup.
    #[serde(default)]
    pub disable_password: bool,
    /// New accounts must verify their email before signing in.
    #[serde(default)]
    pub require_email_verification: bool,
    /// Domains that must use SSO. Empty means SSO is not required.
    #[serde(default, deserialize_with = "deserialize_string_list")]
    pub enforce_sso_domains: Vec<String>,
    /// GitHub OAuth, when configured.
    #[serde(default)]
    pub github: Option<GithubAuth>,
    /// Google OIDC, when configured.
    #[serde(default)]
    pub google: Option<GoogleAuth>,
    /// Generic OIDC providers, in config order. TOML only.
    #[serde(default)]
    pub oidc: Vec<OidcAuth>,
}

impl Default for AuthSettings {
    fn default() -> Self {
        Self {
            signup: default_signup(),
            signup_domains: Vec::new(),
            disable_password: false,
            require_email_verification: false,
            enforce_sso_domains: Vec::new(),
            github: None,
            google: None,
            oidc: Vec::new(),
        }
    }
}

/// `[auth.github]`. Linking an existing account by email defaults to on (D148).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubAuth {
    /// OAuth client id.
    pub client_id: String,
    /// OAuth client secret.
    pub client_secret: Secret<String>,
    /// Link a verified email to an existing account. Default true.
    #[serde(default = "default_link_known")]
    pub link_existing_by_email: bool,
}

/// `[auth.google]`. The issuer is fixed to `https://accounts.google.com`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoogleAuth {
    /// OAuth client id.
    pub client_id: String,
    /// OAuth client secret.
    pub client_secret: Secret<String>,
    /// Checked against the verified email and the `hd` claim.
    #[serde(default, deserialize_with = "deserialize_string_list")]
    pub allowed_domains: Vec<String>,
    /// Link a verified email to an existing account. Default true.
    #[serde(default = "default_link_known")]
    pub link_existing_by_email: bool,
}

/// One `[[auth.oidc]]` provider. The id is `oidc:<name>`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcAuth {
    /// `[a-z0-9-]{1,32}`, unique among the OIDC providers.
    pub name: String,
    /// Name shown on the login button.
    pub display_name: String,
    /// `https` issuer, or `http` on a loopback host.
    pub issuer: String,
    /// OAuth client id.
    pub client_id: String,
    /// OAuth client secret.
    pub client_secret: Secret<String>,
    /// Must include `openid`. Default `openid`, `email`, `profile`.
    #[serde(
        default = "default_oidc_scopes",
        deserialize_with = "deserialize_string_list"
    )]
    pub scopes: Vec<String>,
    /// Checked against the verified email.
    #[serde(default, deserialize_with = "deserialize_string_list")]
    pub allowed_domains: Vec<String>,
    /// Org and role for a new user. The org is not required to exist yet.
    #[serde(default)]
    pub auto_provision: Option<AutoProvision>,
    /// Link a verified email to an existing account. Default false.
    #[serde(default)]
    pub link_existing_by_email: bool,
}

/// `auto_provision` on a generic OIDC provider.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutoProvision {
    /// Org slug.
    pub org: String,
    /// `admin`, `member`, or `viewer`. Never `owner`.
    pub role: String,
}

/// Values that create the first admin, org, and project when the database is empty.
///
/// The four `admin_email`, `admin_password`, `org`, and `project` fields are
/// all set or all unset. `secret_key` is independent.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields, default)]
pub struct InitConfig {
    /// Email of the first admin.
    pub admin_email: Option<String>,
    /// Password of the first admin.
    pub admin_password: Option<Secret<String>>,
    /// Name of the first organization.
    pub org: Option<String>,
    /// Name of the first project.
    pub project: Option<String>,
    /// Instance secret used to seal sessions. Optional in this group.
    pub secret_key: Option<Secret<String>>,
}

/// Log format. Absent means the CLI chooses from `isatty`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Multi-line, human-readable log lines.
    Pretty,
    /// One JSON object per line.
    Json,
}

/// Log sink settings. `level` is an `EnvFilter` directive.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LogConfig {
    /// `pretty` or `json`. Absent leaves the choice to the CLI.
    #[serde(default)]
    pub format: Option<LogFormat>,
    /// `EnvFilter` directive. Defaults to `info`.
    #[serde(default = "default_log_level")]
    pub level: String,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            format: None,
            level: default_log_level(),
        }
    }
}

/// How an SMTP connection is encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tls {
    /// `smtp://` — STARTTLS is required.
    Required,
    /// `smtps://` — TLS from the first byte.
    Wrapper,
    /// `smtp://…?tls=none` — a local relay.
    None,
}

/// A parsed `smtp_url`. The password stays inside [`Secret`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmtpUrl {
    /// Encryption for this host.
    pub tls: Tls,
    /// SMTP host, without brackets on an IPv6 address.
    pub host: String,
    /// Explicit port, or 587 / 465 / 25 when the URL omits one.
    pub port: u16,
    /// Username and password. Both are percent-decoded.
    pub credentials: Option<(String, Secret<String>)>,
}

/// Everything `llmobs serve` reads from the environment and `llmobs.toml`.
///
/// Keys that spec 12 lists for later work (gRPC listen address, encryption key,
/// ingest, evaluation workers, telemetry, compaction, gc, deletion, anchor)
/// are not fields. Setting one is an unknown-key error.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Directory that holds `LOCK`, `wal/`, `catalog.db`, and `meta.db`.
    #[serde(default = "default_data_dir")]
    pub data_dir: PathBuf,
    /// Origin used in links and cookies.
    #[serde(default)]
    pub public_url: PublicUrl,
    /// HTTP listen address.
    #[serde(
        default = "default_http_addr",
        deserialize_with = "deserialize_socket_addr"
    )]
    pub http_addr: SocketAddr,
    /// Proxies whose `X-Forwarded-For` is trusted (D142).
    #[serde(default, deserialize_with = "deserialize_proxies")]
    pub trusted_proxies: Vec<IpNet>,
    /// `[storage]` table. `data_dir` is copied from the top-level field.
    ///
    /// `StorageConfig` requires `data_dir` and denies unknown fields, so the
    /// loader rejects a user-set `storage.data_dir`, injects the top-level
    /// directory, then deserializes this table as [`StorageConfig`].
    pub storage: StorageConfig,
    /// Sign-up policy and the auth flags from spec 08.
    #[serde(default)]
    pub auth: AuthSettings,
    /// SMTP URL, including any credentials. Empty does not count as configured.
    #[serde(default, deserialize_with = "deserialize_smtp_url")]
    pub smtp_url: Option<SmtpUrl>,
    /// `From` mailbox. Required when `smtp_url` is set.
    #[serde(default)]
    pub email_from: Option<String>,
    /// First-run admin, org, and project.
    #[serde(default)]
    pub init: InitConfig,
    /// Log format and level.
    #[serde(default)]
    pub log: LogConfig,
}

impl ServerConfig {
    /// Load defaults, then `path`, otherwise `$LLMOBS_CONFIG`, then `LLMOBS_*`.
    ///
    /// A path that is given but missing is [`ConfigError::MissingFile`]. An
    /// unset path and an unset `LLMOBS_CONFIG` mean there is no file.
    ///
    /// # Errors
    ///
    /// Returns a [`ConfigError`] when the file is missing, a key is unknown, or
    /// a value fails validation. Secret values are not part of the message.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let toml = read_toml(path)?;
        let env = std::env::vars().collect::<Vec<_>>();
        let pairs = env
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>();
        load_from(toml.as_deref(), &pairs)
    }

    /// The storage configuration, including the injected data directory.
    pub fn storage_config(&self) -> StorageConfig {
        self.storage.clone()
    }
}

/// Why [`ServerConfig::load`] rejected the configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Figment could not parse or deserialize a value.
    ///
    /// The message names the key. Errors on `smtp_url`, `init.admin_password`,
    /// `init.secret_key`, and any `client_secret` are [`Self::Invalid`]
    /// instead, because figment's type error includes the rejected value. The
    /// error is boxed so [`Result`] stays small.
    #[error(transparent)]
    Figment(Box<figment::Error>),
    /// A value failed a check in this module or in [`StorageConfig::validate`].
    #[error("{key}: {reason}")]
    Invalid {
        /// Dotted config path, for example `auth.signup_domains`.
        key: &'static str,
        /// Static rule. Never the rejected input.
        reason: &'static str,
    },
    /// A provider value failed a check. `key` includes the provider name.
    ///
    /// `reason` is static. It is never a secret, an email, or the issuer URL.
    #[error("{key}: {reason}")]
    Provider {
        /// Dotted path, for example `auth.oidc.okta.issuer`.
        key: String,
        /// Static rule.
        reason: &'static str,
    },
    /// `path` or `$LLMOBS_CONFIG` names a file that does not exist.
    #[error("config file not found: {}", .0.display())]
    MissingFile(PathBuf),
}

fn default_data_dir() -> PathBuf {
    PathBuf::from("./llmobs-data")
}

fn default_http_addr() -> SocketAddr {
    SocketAddr::from(([0, 0, 0, 0], 8080))
}

fn default_signup() -> SignupPolicy {
    SignupPolicy::InviteOnly
}

fn default_link_known() -> bool {
    true
}

fn default_oidc_scopes() -> Vec<String> {
    vec![
        "openid".to_owned(),
        "email".to_owned(),
        "profile".to_owned(),
    ]
}

fn default_log_level() -> String {
    "info".to_owned()
}

fn load_from(toml: Option<&str>, env: &[(&str, &str)]) -> Result<ServerConfig, ConfigError> {
    let mut figment = Figment::new();
    if let Some(toml) = toml {
        figment = figment.merge(Toml::string(toml));
    }
    figment = figment.merge(EnvSource { pairs: env });
    if figment.find_value("storage.data_dir").is_ok() {
        return Err(ConfigError::Invalid {
            key: "storage.data_dir",
            reason: "set data_dir at the top level",
        });
    }
    let data_dir = match figment.find_value("data_dir") {
        Ok(value) => path_from_value(value)?,
        Err(_) => default_data_dir(),
    };
    let data_dir = path_string(&data_dir)?;
    figment = figment.merge(Serialized::default("storage.data_dir", data_dir));
    let mut config: ServerConfig = figment.extract().map_err(map_figment)?;
    validate(&mut config)?;
    Ok(config)
}

fn read_toml(path: Option<&Path>) -> Result<Option<String>, ConfigError> {
    let Some(path) = configured_path(path) else {
        return Ok(None);
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(ConfigError::MissingFile(path))
        }
        Err(error) => Err(ConfigError::Figment(Box::new(figment::Error::from(
            error.to_string(),
        )))),
    }
}

fn configured_path(path: Option<&Path>) -> Option<PathBuf> {
    match path {
        Some(path) => Some(path.to_path_buf()),
        None => std::env::var_os("LLMOBS_CONFIG").map(PathBuf::from),
    }
}

fn path_from_value(value: Value) -> Result<PathBuf, ConfigError> {
    match value.into_string() {
        Some(path) => Ok(PathBuf::from(path)),
        None => Err(ConfigError::Invalid {
            key: "data_dir",
            reason: "expected a string",
        }),
    }
}

fn path_string(path: &Path) -> Result<String, ConfigError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(ConfigError::Invalid {
            key: "data_dir",
            reason: "must be valid UTF-8",
        })
}

fn map_figment(error: figment::Error) -> ConfigError {
    for part in error.clone() {
        if let Some(key) = secret_key(&part.path) {
            // Figment's own text can quote the rejected URL, password included.
            // Only the static reason is kept.
            let invalid_url = part.to_string().contains("invalid URL");
            return ConfigError::Invalid {
                key,
                reason: if invalid_url {
                    "invalid URL"
                } else {
                    "expected a string"
                },
            };
        }
    }
    ConfigError::Figment(Box::new(error))
}

fn secret_key(path: &[String]) -> Option<&'static str> {
    match path {
        [key] if key == "smtp_url" => Some("smtp_url"),
        [parent, key] if parent == "init" && key == "admin_password" => Some("init.admin_password"),
        [parent, key] if parent == "init" && key == "secret_key" => Some("init.secret_key"),
        [section, provider, key]
            if section == "auth" && provider == "github" && key == "client_secret" =>
        {
            Some("auth.github.client_secret")
        }
        [section, provider, key]
            if section == "auth" && provider == "google" && key == "client_secret" =>
        {
            Some("auth.google.client_secret")
        }
        [..] if path.last().is_some_and(|key| key == "client_secret") => {
            Some("auth.oidc.client_secret")
        }
        _ => None,
    }
}

fn validate(config: &mut ServerConfig) -> Result<(), ConfigError> {
    normalize_domains("auth.signup_domains", &mut config.auth.signup_domains)?;
    normalize_domains(
        "auth.enforce_sso_domains",
        &mut config.auth.enforce_sso_domains,
    )?;
    validate_email(config)?;
    if config.auth.require_email_verification && config.smtp_url.is_none() {
        return Err(ConfigError::Invalid {
            key: "auth.require_email_verification",
            reason: "email verification needs `smtp_url`",
        });
    }
    validate_auth(&mut config.auth)?;
    validate_init(&config.init)?;
    config.storage.validate().map_err(|error| match error {
        llmobs_storage::error::ConfigError::Invalid { field, rule } => ConfigError::Invalid {
            key: field,
            reason: rule,
        },
    })?;
    Ok(())
}

fn normalize_domains(key: &'static str, domains: &mut [String]) -> Result<(), ConfigError> {
    for domain in domains {
        *domain = domain.to_lowercase();
        if !domain_ok(domain) {
            return Err(ConfigError::Invalid {
                key,
                reason: "invalid domain",
            });
        }
    }
    Ok(())
}

fn validate_auth(auth: &mut AuthSettings) -> Result<(), ConfigError> {
    if let Some(github) = &auth.github {
        require_client(
            "auth.github.client_id",
            "auth.github.client_secret",
            &github.client_id,
            github.client_secret.expose(),
        )?;
    }
    if let Some(google) = &mut auth.google {
        require_client(
            "auth.google.client_id",
            "auth.google.client_secret",
            &google.client_id,
            google.client_secret.expose(),
        )?;
        normalize_domains("auth.google.allowed_domains", &mut google.allowed_domains)?;
    }
    let mut names = BTreeSet::new();
    for provider in &mut auth.oidc {
        validate_oidc(provider, &mut names)?;
    }
    let configured = auth.github.is_some() || auth.google.is_some() || !auth.oidc.is_empty();
    if auth.disable_password && !configured {
        return Err(ConfigError::Invalid {
            key: "auth.disable_password",
            reason: "a provider is required when passwords are disabled",
        });
    }
    if !auth.enforce_sso_domains.is_empty() && !configured {
        return Err(ConfigError::Invalid {
            key: "auth.enforce_sso_domains",
            reason: "a provider is required when SSO domains are set",
        });
    }
    Ok(())
}

fn require_client(
    id_key: &'static str,
    secret_key: &'static str,
    client_id: &str,
    client_secret: &str,
) -> Result<(), ConfigError> {
    if client_id.is_empty() {
        return Err(ConfigError::Invalid {
            key: id_key,
            reason: "client id is required",
        });
    }
    if client_secret.is_empty() {
        return Err(ConfigError::Invalid {
            key: secret_key,
            reason: "client secret is required",
        });
    }
    Ok(())
}

fn validate_oidc(provider: &mut OidcAuth, names: &mut BTreeSet<String>) -> Result<(), ConfigError> {
    if !oidc_name_ok(&provider.name) {
        return Err(ConfigError::Provider {
            key: "auth.oidc.name".to_owned(),
            reason: "name must be 1-32 characters of [a-z0-9-]",
        });
    }
    if !names.insert(provider.name.clone()) {
        return Err(ConfigError::Provider {
            key: format!("auth.oidc.{}", provider.name),
            reason: "duplicate name",
        });
    }
    let name = provider.name.clone();
    provider.display_name = provider.display_name.trim().to_owned();
    if provider.display_name.is_empty() {
        return Err(ConfigError::Provider {
            key: format!("auth.oidc.{name}.display_name"),
            reason: "display name is required",
        });
    }
    validate_issuer(format!("auth.oidc.{name}.issuer"), &provider.issuer)?;
    if provider.client_id.is_empty() {
        return Err(ConfigError::Provider {
            key: format!("auth.oidc.{name}.client_id"),
            reason: "client id is required",
        });
    }
    if provider.client_secret.expose().is_empty() {
        return Err(ConfigError::Provider {
            key: format!("auth.oidc.{name}.client_secret"),
            reason: "client secret is required",
        });
    }
    if !provider.scopes.iter().any(|scope| scope == "openid") {
        return Err(ConfigError::Provider {
            key: format!("auth.oidc.{name}.scopes"),
            reason: "scopes must include openid",
        });
    }
    normalize_named_domains(
        format!("auth.oidc.{name}.allowed_domains"),
        &mut provider.allowed_domains,
    )?;
    if let Some(provision) = &provider.auto_provision {
        validate_provision(&name, provision)?;
    }
    Ok(())
}

fn oidc_name_ok(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn validate_issuer(key: String, issuer: &str) -> Result<(), ConfigError> {
    let url = match url::Url::parse(issuer) {
        Ok(url) => url,
        Err(_) => {
            return Err(ConfigError::Provider {
                key,
                reason: "invalid issuer",
            });
        }
    };
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ConfigError::Provider {
            key,
            reason: "invalid issuer",
        });
    }
    let loopback = loopback_host(&url);
    if url.scheme() != "https" && !(url.scheme() == "http" && loopback) {
        return Err(ConfigError::Provider {
            key,
            reason: "issuer must be https, or http on a loopback host",
        });
    }
    if multi_tenant_entra(&url) {
        return Err(ConfigError::Provider {
            key,
            reason: "multi-tenant Microsoft issuers are not allowed",
        });
    }
    Ok(())
}

fn loopback_host(url: &url::Url) -> bool {
    match url.host() {
        // `host_str` for IPv6 includes the brackets (`[::1]`). Match the parsed host.
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(addr)) => addr == std::net::Ipv4Addr::LOCALHOST,
        Some(url::Host::Ipv6(addr)) => addr == std::net::Ipv6Addr::LOCALHOST,
        _ => false,
    }
}

fn multi_tenant_entra(url: &url::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if !host.eq_ignore_ascii_case("login.microsoftonline.com") {
        return false;
    }
    let segment = url.path().split('/').find(|segment| !segment.is_empty());
    matches!(
        segment.map(str::to_ascii_lowercase).as_deref(),
        Some("common" | "organizations" | "consumers")
    )
}

fn validate_provision(name: &str, provision: &AutoProvision) -> Result<(), ConfigError> {
    if crate::meta::orgs::validate_slug(&provision.org).is_err() {
        return Err(ConfigError::Provider {
            key: format!("auth.oidc.{name}.auto_provision.org"),
            reason: "invalid org slug",
        });
    }
    let reason = match provision.role.as_str() {
        "admin" | "member" | "viewer" => return Ok(()),
        "owner" => "role must not be owner",
        _ => "invalid role",
    };
    Err(ConfigError::Provider {
        key: format!("auth.oidc.{name}.auto_provision.role"),
        reason,
    })
}

fn normalize_named_domains(key: String, domains: &mut [String]) -> Result<(), ConfigError> {
    for domain in domains {
        *domain = domain.to_lowercase();
        if !domain_ok(domain) {
            return Err(ConfigError::Provider {
                key,
                reason: "invalid domain",
            });
        }
    }
    Ok(())
}

fn domain_ok(domain: &str) -> bool {
    !domain.is_empty()
        && !domain.contains('@')
        && !domain.chars().any(|c| c.is_whitespace() || c.is_control())
        && domain.split('.').all(|label| !label.is_empty())
}

fn validate_email(config: &mut ServerConfig) -> Result<(), ConfigError> {
    let trimmed = config
        .email_from
        .as_deref()
        .map(str::trim)
        .filter(|from| !from.is_empty())
        .map(str::to_owned);
    config.email_from = trimmed;
    if config
        .email_from
        .as_deref()
        .is_some_and(|from| from.parse::<Mailbox>().is_err())
    {
        return Err(ConfigError::Invalid {
            key: "email_from",
            reason: "invalid mailbox",
        });
    }
    if config.smtp_url.is_some() && config.email_from.is_none() {
        return Err(ConfigError::Invalid {
            key: "email_from",
            reason: "required when smtp_url is set",
        });
    }
    Ok(())
}

fn deserialize_smtp_url<'de, D>(deserializer: D) -> Result<Option<SmtpUrl>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(secret) = Option::<Secret<String>>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let raw = secret.expose().trim();
    if raw.is_empty() {
        return Ok(None);
    }
    parse_smtp_url(raw)
        .map(Some)
        .map_err(|()| de::Error::custom("invalid URL"))
}

fn parse_smtp_url(raw: &str) -> Result<SmtpUrl, ()> {
    let url = url::Url::parse(raw).map_err(|_| ())?;
    if url.fragment().is_some() {
        return Err(());
    }
    let path = url.path();
    if !path.is_empty() && path != "/" {
        return Err(());
    }
    let host = url.host_str().filter(|host| !host.is_empty()).ok_or(())?;
    let tls = match url.scheme() {
        "smtp" => match url.query() {
            None => Tls::Required,
            Some("tls=none") => Tls::None,
            Some(_) => return Err(()),
        },
        "smtps" if url.query().is_none() => Tls::Wrapper,
        _ => return Err(()),
    };
    let port = url.port().unwrap_or(match tls {
        Tls::Required => 587,
        Tls::Wrapper => 465,
        Tls::None => 25,
    });
    let credentials = if url.username().is_empty() {
        if url.password().is_some() {
            return Err(());
        }
        None
    } else {
        Some((
            percent_decode(url.username())?,
            Secret(percent_decode(url.password().unwrap_or(""))?),
        ))
    };
    Ok(SmtpUrl {
        tls,
        host: host.to_owned(),
        port,
        credentials,
    })
}

/// `username()` and `password()` keep `%HH` sequences. Userinfo uses `%20`, not `+`.
fn percent_decode(raw: &str) -> Result<String, ()> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(());
            }
            let high = hex_digit(bytes[index + 1])?;
            let low = hex_digit(bytes[index + 2])?;
            out.push((high << 4) | low);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).map_err(|_| ())
}

fn hex_digit(byte: u8) -> Result<u8, ()> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(()),
    }
}

fn validate_init(init: &InitConfig) -> Result<(), ConfigError> {
    let parts = [
        text_set(&init.admin_email),
        secret_set(&init.admin_password),
        text_set(&init.org),
        text_set(&init.project),
    ];
    let set = parts.into_iter().filter(|present| *present).count();
    if set == 0 {
        return Ok(());
    }
    if set != parts.len() {
        return Err(ConfigError::Invalid {
            key: "init",
            reason: INIT_GROUP,
        });
    }
    match init.admin_email.as_deref() {
        Some(email) if email_ok(email) => Ok(()),
        Some(_) => Err(ConfigError::Invalid {
            key: "init.admin_email",
            reason: "invalid email",
        }),
        None => Err(ConfigError::Invalid {
            key: "init",
            reason: INIT_GROUP,
        }),
    }
}

fn text_set(value: &Option<String>) -> bool {
    value.as_ref().is_some_and(|text| !text.trim().is_empty())
}

fn secret_set(value: &Option<Secret<String>>) -> bool {
    value
        .as_ref()
        .is_some_and(|secret| !secret.expose().trim().is_empty())
}

/// Structural email check from the identity plan, without rewriting the value.
fn email_ok(email: &str) -> bool {
    if email.len() > 254 || email.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && !domain.is_empty()
        && !domain.contains('@')
        && domain.contains('.')
        && domain.split('.').all(|label| !label.is_empty())
}

fn parse_public_url(raw: &str) -> Result<PublicUrl, &'static str> {
    let url = url::Url::parse(raw).map_err(|_| "must be an http or https URL")?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err("must use http or https");
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("must not contain user info");
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("must not have a query or fragment");
    }
    if url.path() != "/" && !url.path().is_empty() {
        return Err("must not have a path");
    }
    let host = match url.host() {
        Some(url::Host::Ipv6(addr)) => format!("[{addr}]"),
        Some(url::Host::Ipv4(addr)) => addr.to_string(),
        Some(url::Host::Domain(domain)) => domain.to_owned(),
        None => return Err("must have a host"),
    };
    let origin = match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    };
    Ok(PublicUrl {
        https: url.scheme() == "https",
        origin,
    })
}

fn deserialize_socket_addr<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<SocketAddr, D::Error> {
    let raw = String::deserialize(deserializer)?;
    raw.parse()
        .map_err(|_error| de::Error::custom("invalid socket address"))
}

fn deserialize_proxies<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<IpNet>, D::Error> {
    let items = deserialize_string_list(deserializer)?;
    items
        .iter()
        .map(|item| {
            item.parse::<IpNet>()
                .map_err(|_error| de::Error::custom("invalid CIDR"))
        })
        .collect()
}

fn deserialize_string_list<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<String>, D::Error> {
    deserializer.deserialize_any(StringListVisitor)
}

struct StringListVisitor;

impl<'de> Visitor<'de> for StringListVisitor {
    type Value = Vec<String>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a string or a list of strings")
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Vec<String>, E> {
        Ok(split_csv(value))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<String>, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element::<String>()? {
            let trimmed = item.trim();
            if !trimmed.is_empty() {
                items.push(trimmed.to_owned());
            }
        }
        Ok(items)
    }
}

fn split_csv(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_owned)
        .collect()
}

struct EnvSource<'a> {
    pairs: &'a [(&'a str, &'a str)],
}

impl Provider for EnvSource<'_> {
    fn metadata(&self) -> Metadata {
        Metadata::named("LLMOBS_ environment")
    }

    fn data(&self) -> Result<Map<Profile, Dict>, figment::Error> {
        let mut dict = Dict::new();
        for (name, value) in self.pairs {
            if let Some(dotted) = split_key(name) {
                // `[[auth.oidc]]` is a TOML array. The split form cannot name an
                // element, so those variables are ignored rather than merged.
                if dotted == "auth.oidc" || dotted.starts_with("auth.oidc.") {
                    continue;
                }
                insert_dotted(&mut dict, &dotted, env_value(&dotted, value));
            }
        }
        for (name, value) in self.pairs {
            if let Some(dotted) = alias_key(name) {
                insert_dotted(&mut dict, dotted, env_value(dotted, value));
            }
        }
        Ok(Profile::Default.collect(dict))
    }
}

fn split_key(name: &str) -> Option<String> {
    let (prefix, rest) = name.split_at_checked("LLMOBS_".len())?;
    if !prefix.eq_ignore_ascii_case("LLMOBS_") || is_filtered(name) {
        return None;
    }
    let parts: Vec<&str> = rest.split("__").collect();
    if parts.is_empty() || parts.iter().any(|part| part.is_empty()) {
        return None;
    }
    Some(
        parts
            .into_iter()
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>()
            .join("."),
    )
}

fn is_filtered(name: &str) -> bool {
    name.eq_ignore_ascii_case("LLMOBS_CONFIG") || alias_key(name).is_some()
}

fn alias_key(name: &str) -> Option<&'static str> {
    ALIASES
        .iter()
        .find_map(|(alias, key)| name.eq_ignore_ascii_case(alias).then_some(*key))
}

fn env_value(dotted: &str, raw: &str) -> Value {
    if is_secret_path(dotted) {
        // Keep the bytes as a string. Figment's env parser would turn an
        // all-digit secret into a number and then print that number in a
        // type error.
        Value::from(raw)
    } else {
        parse_env_value(raw)
    }
}

fn is_secret_path(dotted: &str) -> bool {
    matches!(
        dotted,
        "smtp_url" | "init.admin_password" | "init.secret_key"
    ) || dotted.ends_with(".client_secret")
}

fn parse_env_value(raw: &str) -> Value {
    let value: Result<Value, std::convert::Infallible> = raw.parse();
    match value {
        Ok(value) => value,
        Err(never) => match never {},
    }
}

fn insert_dotted(dict: &mut Dict, dotted: &str, value: Value) {
    match figment::util::nest(dotted, value) {
        Value::Dict(_, nested) => merge_dict(dict, nested),
        other => {
            dict.insert(dotted.to_owned(), other);
        }
    }
}

fn merge_dict(into: &mut Dict, from: Dict) {
    for (key, incoming) in from {
        match (into.remove(&key), incoming) {
            (Some(Value::Dict(_, mut left)), Value::Dict(_, right)) => {
                merge_dict(&mut left, right);
                into.insert(key, Value::from(left));
            }
            (_, incoming) => {
                into.insert(key, incoming);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use bytesize::ByteSize;
    use llmobs_storage::config::{ObjectStoreUrl, WalSync};

    use super::{ConfigError, LogFormat, ServerConfig, SmtpUrl, Tls, load_from};
    use crate::api::account::SignupPolicy;

    fn socket(raw: &str) -> SocketAddr {
        raw.parse().unwrap()
    }

    #[test]
    fn defaults_match_spec() {
        let config = load_from(None, &[]).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("./llmobs-data"));
        assert_eq!(config.public_url.origin(), "http://localhost:8080");
        assert!(!config.public_url.is_https());
        assert_eq!(config.http_addr, socket("0.0.0.0:8080"));
        assert!(config.trusted_proxies.is_empty());
        assert_eq!(config.auth.signup, SignupPolicy::InviteOnly);
        assert!(config.auth.signup_domains.is_empty());
        assert!(!config.auth.disable_password);
        assert!(!config.auth.require_email_verification);
        assert!(config.auth.enforce_sso_domains.is_empty());
        assert!(config.smtp_url.is_none());
        assert!(config.email_from.is_none());
        assert!(config.init.admin_email.is_none());
        assert!(config.init.admin_password.is_none());
        assert!(config.init.org.is_none());
        assert!(config.init.project.is_none());
        assert!(config.init.secret_key.is_none());
        assert!(config.log.format.is_none());
        assert_eq!(config.log.level, "info");

        let storage = config.storage_config();
        assert_eq!(storage.data_dir, config.data_dir);
        assert!(storage.object_store.is_none());
        assert_eq!(
            storage.object_store_url(),
            ObjectStoreUrl::Local(config.data_dir.join("objects"))
        );
        assert!(storage.s3.region.is_none());
        assert!(storage.s3.endpoint.is_none());
        assert!(!storage.s3.force_path_style);
        assert!(!storage.s3.allow_http);
        assert_eq!(storage.wal.sync, WalSync::Always);
        assert_eq!(storage.wal.segment_bytes, ByteSize::mib(64));
        assert_eq!(storage.wal.group_max_bytes, ByteSize::mib(8));
        assert_eq!(storage.wal.group_max_wait, Duration::from_millis(2));
        assert_eq!(storage.flush.max_bytes, ByteSize::mib(256));
        assert_eq!(storage.flush.max_age, Duration::from_secs(120));
        assert_eq!(storage.flush.upload_concurrency, 8);
        assert_eq!(storage.memtable.hard_limit, ByteSize::gib(1));
        assert_eq!(storage.query.metadata_cache_bytes, ByteSize::mib(512));
        assert!(storage.query.target_partitions >= 1);
    }

    #[test]
    fn toml_overrides_defaults_and_env_overrides_toml() {
        let toml = r#"
            data_dir = "/from/toml"
            http_addr = "127.0.0.1:9"
        "#;
        let config = load_from(
            Some(toml),
            &[
                ("LLMOBS_HTTP_ADDR", "127.0.0.1:10"),
                ("LLMOBS_DATA_DIR", "/from/env"),
            ],
        )
        .unwrap();
        assert_eq!(config.data_dir, PathBuf::from("/from/env"));
        assert_eq!(config.storage_config().data_dir, PathBuf::from("/from/env"));
        assert_eq!(config.http_addr, socket("127.0.0.1:10"));
    }

    #[test]
    fn alias_overrides_split_form() {
        let config = load_from(
            None,
            &[
                ("LLMOBS_AUTH__SIGNUP", "open"),
                ("LLMOBS_AUTH_SIGNUP", "disabled"),
                ("LLMOBS_LOG__LEVEL", "warn"),
                ("LLMOBS_LOG__FORMAT", "json"),
                ("LLMOBS_LOG", "debug"),
            ],
        )
        .unwrap();
        assert_eq!(config.auth.signup, SignupPolicy::Disabled);
        assert_eq!(config.log.level, "debug");
        assert_eq!(config.log.format, Some(LogFormat::Json));
    }

    #[test]
    fn storage_wal_sync_reads_the_split_env_key() {
        let config = load_from(None, &[("LLMOBS_STORAGE__WAL__SYNC", "5ms")]).unwrap();
        assert_eq!(
            config.storage.wal.sync,
            WalSync::Interval(Duration::from_millis(5))
        );
    }

    #[test]
    fn unknown_keys_name_themselves() {
        let toml_error = load_from(Some("nope = true\n"), &[]).unwrap_err();
        assert!(toml_error.to_string().contains("nope"), "{toml_error}");

        let env_error = load_from(None, &[("LLMOBS_FOO", "1")]).unwrap_err();
        assert!(env_error.to_string().contains("foo"), "{env_error}");
    }

    #[test]
    fn storage_data_dir_is_rejected_and_top_level_is_injected() {
        let error = load_from(Some("[storage]\ndata_dir = \"/secret-path\"\n"), &[]).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("storage.data_dir"), "{text}");
        assert!(!text.contains("secret-path"), "{text}");

        let env_error =
            load_from(None, &[("LLMOBS_STORAGE__DATA_DIR", "/also-secret")]).unwrap_err();
        assert!(env_error.to_string().contains("data_dir"), "{env_error}");
        assert!(
            !env_error.to_string().contains("also-secret"),
            "{env_error}"
        );

        let config = load_from(Some("data_dir = \"/data\"\n"), &[]).unwrap();
        assert_eq!(config.storage_config().data_dir, PathBuf::from("/data"));
    }

    #[test]
    fn comma_lists_and_invalid_cidr() {
        let config = load_from(
            Some(
                r#"
                trusted_proxies = ["127.0.0.1/32"]
                auth.signup_domains = ["Example.COM", ""]

                [auth.github]
                client_id = "gh"
                client_secret = "secret"
                "#,
            ),
            &[(
                "LLMOBS_AUTH_ENFORCE_SSO_DOMAINS",
                "Acme.ORG,, partner.example",
            )],
        )
        .unwrap();
        assert_eq!(config.trusted_proxies.len(), 1);
        assert_eq!(config.auth.signup_domains, vec!["example.com".to_owned()]);
        assert_eq!(
            config.auth.enforce_sso_domains,
            vec!["acme.org".to_owned(), "partner.example".to_owned()]
        );

        let env_lists = load_from(
            None,
            &[("LLMOBS_TRUSTED_PROXIES", "127.0.0.1/32, 10.0.0.0/8")],
        )
        .unwrap();
        assert_eq!(env_lists.trusted_proxies.len(), 2);

        let error = load_from(None, &[("LLMOBS_TRUSTED_PROXIES", "not-a-cidr")]).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("trusted_proxies"), "{text}");
        assert!(!text.contains("not-a-cidr"), "{text}");

        let domain_error =
            load_from(None, &[("LLMOBS_AUTH_SIGNUP_DOMAINS", "bad domain")]).unwrap_err();
        let domain_text = domain_error.to_string();
        assert!(domain_text.contains("auth.signup_domains"), "{domain_text}");
        assert!(!domain_text.contains("bad domain"), "{domain_text}");
    }

    #[test]
    fn public_url_rejects_path_query_and_non_http() {
        for bad in [
            "ftp://localhost",
            "http://localhost/api",
            "http://localhost/?q=1",
        ] {
            let error = load_from(None, &[("LLMOBS_PUBLIC_URL", bad)]).unwrap_err();
            assert!(error.to_string().contains("public_url"), "{bad}: {error}");
        }

        let userinfo = load_from(
            None,
            &[("LLMOBS_PUBLIC_URL", "http://ada:s3cret@localhost")],
        )
        .unwrap_err();
        assert!(!userinfo.to_string().contains("s3cret"), "{userinfo}");

        let config = load_from(None, &[("LLMOBS_PUBLIC_URL", "http://localhost:8080/")]).unwrap();
        assert_eq!(config.public_url.origin(), "http://localhost:8080");
        assert_eq!(config.public_url.join("/api"), "http://localhost:8080/api");
        assert_eq!(config.public_url.join("api"), "http://localhost:8080/api");

        let ipv6 = load_from(None, &[("LLMOBS_PUBLIC_URL", "https://[::1]:8080/")]).unwrap();
        assert_eq!(ipv6.public_url.origin(), "https://[::1]:8080");
        assert!(ipv6.public_url.is_https());
    }

    #[test]
    fn http_addr_errors_name_the_key() {
        let error = load_from(None, &[("LLMOBS_HTTP_ADDR", "nope")]).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("http_addr"), "{text}");
        assert!(!text.contains("nope"), "{text}");
    }

    #[test]
    fn verification_requires_smtp() {
        let error = load_from(Some("auth.require_email_verification = true\n"), &[]).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("auth.require_email_verification"), "{text}");
        assert!(text.contains("smtp_url"), "{text}");

        let blank = load_from(
            Some("smtp_url = \"  \"\nauth.require_email_verification = true\n"),
            &[],
        )
        .unwrap_err();
        assert!(blank.to_string().contains("smtp_url"), "{blank}");

        let config = load_from(
            Some(
                "smtp_url = \"smtp://localhost\"\nemail_from = \"noreply@example.com\"\nauth.require_email_verification = true\n",
            ),
            &[],
        )
        .unwrap();
        assert!(config.auth.require_email_verification);
        let smtp = config.smtp_url.expect("smtp");
        assert_eq!(smtp.tls, Tls::Required);
        assert_eq!(smtp.host, "localhost");
        assert_eq!(smtp.port, 587);
        assert!(smtp.credentials.is_none());
    }

    #[test]
    fn partial_init_and_invalid_email_omit_the_values() {
        let partial =
            load_from(Some("[init]\nadmin_password = \"hunter2hunter2\"\n"), &[]).unwrap_err();
        let partial_text = partial.to_string();
        assert!(partial_text.contains("init"), "{partial_text}");
        assert!(!partial_text.contains("hunter2hunter2"), "{partial_text}");

        let email = load_from(
            Some(
                r#"
                [init]
                admin_email = "not-an-email"
                admin_password = "hunter2hunter2"
                org = "Acme"
                project = "App"
                "#,
            ),
            &[],
        )
        .unwrap_err();
        let email_text = email.to_string();
        assert!(email_text.contains("init.admin_email"), "{email_text}");
        assert!(!email_text.contains("not-an-email"), "{email_text}");
        assert!(!email_text.contains("hunter2hunter2"), "{email_text}");
    }

    #[test]
    fn numeric_looking_password_from_env_stays_a_string() {
        let config = load_from(
            None,
            &[
                ("LLMOBS_INIT_ADMIN_EMAIL", "ada@example.com"),
                ("LLMOBS_INIT_ADMIN_PASSWORD", "9876543210"),
                ("LLMOBS_INIT_ORG", "Acme"),
                ("LLMOBS_INIT_PROJECT", "App"),
            ],
        )
        .unwrap();
        assert_eq!(config.init.admin_password.unwrap().expose(), "9876543210");
    }

    #[test]
    fn numeric_secret_error_omits_the_number() {
        let error = load_from(
            Some(
                r#"
                [init]
                admin_email = "ada@example.com"
                admin_password = 987654321
                org = "Acme"
                project = "App"
                "#,
            ),
            &[],
        )
        .unwrap_err();
        let text = error.to_string();
        assert!(text.contains("admin_password"), "{text}");
        assert!(!text.contains("987654321"), "{text}");
    }

    #[test]
    fn debug_hides_password_and_secret_key() {
        let config = load_from(
            Some(
                r#"
                smtp_url = "smtp://user:s3cret-token@localhost:587"
                email_from = "llmobs <noreply@example.com>"
                [init]
                admin_email = "ada@example.com"
                admin_password = "hunter2hunter2"
                org = "Acme"
                project = "App"
                secret_key = "sk-llmobs-EXAMPLEKEYEXAMPLEKEYEXAMPLE"
                "#,
            ),
            &[],
        )
        .unwrap();
        let text = format!("{config:?}");
        assert!(text.contains("Secret(***)"), "{text}");
        assert!(!text.contains("hunter2hunter2"), "{text}");
        assert!(
            !text.contains("sk-llmobs-EXAMPLEKEYEXAMPLEKEYEXAMPLE"),
            "{text}"
        );
        assert!(!text.contains("s3cret-token"), "{text}");
        assert_eq!(
            config.init.admin_password.unwrap().expose(),
            "hunter2hunter2"
        );
        let (user, password) = config.smtp_url.unwrap().credentials.unwrap();
        assert_eq!(user, "user");
        assert_eq!(password.expose(), "s3cret-token");
    }

    #[test]
    fn smtp_url_parses_the_three_forms() {
        let starttls = load_smtp("smtp://user:pass@localhost");
        assert_eq!(starttls.tls, Tls::Required);
        assert_eq!(starttls.host, "localhost");
        assert_eq!(starttls.port, 587);
        let (user, password) = starttls.credentials.unwrap();
        assert_eq!(user, "user");
        assert_eq!(password.expose(), "pass");

        let explicit = load_smtp("smtp://user:pass@mail.example:2525");
        assert_eq!(explicit.tls, Tls::Required);
        assert_eq!(explicit.port, 2525);

        let wrapper = load_smtp("smtps://user:pass@localhost");
        assert_eq!(wrapper.tls, Tls::Wrapper);
        assert_eq!(wrapper.port, 465);
        let submissions = load_smtp("smtps://user:pass@localhost:465");
        assert_eq!(submissions.port, 465);

        let plain = load_smtp("smtp://localhost?tls=none");
        assert_eq!(plain.tls, Tls::None);
        assert_eq!(plain.host, "localhost");
        assert_eq!(plain.port, 25);
        assert!(plain.credentials.is_none());
        let relay = load_smtp("smtp://localhost:25?tls=none");
        assert_eq!(relay.tls, Tls::None);
        assert_eq!(relay.port, 25);
    }

    #[test]
    fn smtp_url_percent_decodes_credentials() {
        let smtp = load_smtp("smtp://us%65r:s%3Acret-token@localhost");
        let (user, password) = smtp.credentials.unwrap();
        assert_eq!(user, "user");
        assert_eq!(password.expose(), "s:cret-token");
    }

    #[test]
    fn smtp_url_rejects_other_schemes_without_the_password() {
        let error = load_from(
            Some("smtp_url = \"http://user:s3cret-token@localhost\"\n"),
            &[],
        )
        .unwrap_err();
        let display = error.to_string();
        let debug = format!("{error:?}");
        assert!(display.contains("smtp_url"), "{display}");
        assert!(display.contains("invalid URL"), "{display}");
        assert!(!display.contains("s3cret-token"), "{display}");
        assert!(!debug.contains("s3cret-token"), "{debug}");

        for bad in [
            "smtp://localhost?tls=require",
            "smtp://localhost?tls=none&x=1",
            "smtps://localhost?tls=none",
            "ftp://localhost",
        ] {
            let error = load_smtp_error(bad);
            assert!(error.to_string().contains("smtp_url"), "{bad}: {error}");
            assert!(error.to_string().contains("invalid URL"), "{bad}: {error}");
        }
    }

    #[test]
    fn email_from_is_required_with_smtp_and_must_be_a_mailbox() {
        let missing = load_from(Some("smtp_url = \"smtp://localhost\"\n"), &[]).unwrap_err();
        let missing_text = missing.to_string();
        assert!(missing_text.contains("email_from"), "{missing_text}");
        assert!(
            missing_text.contains("required when smtp_url is set"),
            "{missing_text}"
        );
        assert!(!missing_text.contains("localhost"), "{missing_text}");

        let invalid = load_from(
            Some("smtp_url = \"smtp://localhost\"\nemail_from = \"not-a-mailbox\"\n"),
            &[],
        )
        .unwrap_err();
        let invalid_text = invalid.to_string();
        assert!(invalid_text.contains("email_from"), "{invalid_text}");
        assert!(invalid_text.contains("invalid mailbox"), "{invalid_text}");
        assert!(!invalid_text.contains("not-a-mailbox"), "{invalid_text}");
        assert!(!format!("{invalid:?}").contains("not-a-mailbox"));

        let named = load_from(
            Some("smtp_url = \"smtp://localhost\"\nemail_from = \"llmobs <noreply@acme.com>\"\n"),
            &[],
        )
        .unwrap();
        assert_eq!(
            named.email_from.as_deref(),
            Some("llmobs <noreply@acme.com>")
        );

        let bare = load_from(
            Some("smtp_url = \"smtp://localhost\"\nemail_from = \"  noreply@acme.com  \"\n"),
            &[],
        )
        .unwrap();
        assert_eq!(bare.email_from.as_deref(), Some("noreply@acme.com"));
    }

    #[test]
    fn mailer_follows_smtp_url() {
        let absent = load_from(None, &[]).unwrap();
        assert!(
            crate::email::Mailer::from_config(&absent)
                .unwrap()
                .is_none()
        );

        let present = load_from(
            Some("smtp_url = \"smtp://localhost:2525\"\nemail_from = \"noreply@example.com\"\n"),
            &[],
        )
        .unwrap();
        assert!(
            crate::email::Mailer::from_config(&present)
                .unwrap()
                .is_some()
        );
    }

    fn load_smtp(url: &str) -> SmtpUrl {
        let toml = format!("smtp_url = {url:?}\nemail_from = \"noreply@example.com\"\n");
        load_from(Some(&toml), &[])
            .unwrap_or_else(|error| panic!("parse {url}: {error}"))
            .smtp_url
            .unwrap()
    }

    fn load_smtp_error(url: &str) -> ConfigError {
        let toml = format!("smtp_url = {url:?}\nemail_from = \"noreply@example.com\"\n");
        load_from(Some(&toml), &[]).unwrap_err()
    }

    #[test]
    fn missing_config_file_names_the_path() {
        let path = Path::new("/no/such/llmobs-config.toml");
        let error = ServerConfig::load(Some(path)).unwrap_err();
        assert!(
            error.to_string().contains("/no/such/llmobs-config.toml"),
            "{error}"
        );
    }

    #[test]
    fn providers_parse_and_debug_hides_client_secrets() {
        let config = load_from(Some(PROVIDERS_TOML), &[]).expect("providers");
        let github = config.auth.github.as_ref().expect("github");
        assert_eq!(github.client_id, "gh-client");
        assert_eq!(github.client_secret.expose(), "gh-secret-value");
        assert!(github.link_existing_by_email);
        let google = config.auth.google.as_ref().expect("google");
        assert_eq!(google.allowed_domains, vec!["acme.test".to_owned()]);
        assert!(google.link_existing_by_email);
        assert_eq!(config.auth.oidc.len(), 5);
        let okta = &config.auth.oidc[0];
        assert_eq!(okta.name, "okta");
        assert_eq!(okta.display_name, "Okta");
        assert_eq!(
            okta.scopes,
            vec![
                "openid".to_owned(),
                "email".to_owned(),
                "profile".to_owned()
            ]
        );
        assert!(!okta.link_existing_by_email);
        assert_eq!(
            okta.auto_provision.as_ref().map(|item| item.role.as_str()),
            Some("member")
        );
        assert_eq!(config.auth.oidc[1].issuer, "http://127.0.0.1:9");
        assert_eq!(config.auth.oidc[2].issuer, "http://localhost:9");
        assert_eq!(config.auth.oidc[3].issuer, "http://[::1]:9");
        assert!(config.auth.oidc[4].link_existing_by_email);
        assert!(config.auth.disable_password);
        assert_eq!(
            config.auth.enforce_sso_domains,
            vec!["acme.test".to_owned()]
        );
        let rendered = format!("{config:?}");
        for secret in [
            "gh-secret-value",
            "google-secret-value",
            "okta-secret-value",
            "local-secret-value",
            "loop-secret-value",
            "v6-secret-value",
            "entra-secret-value",
        ] {
            assert!(!rendered.contains(secret), "{secret} leaked: {rendered}");
        }
        assert!(rendered.contains("Secret(***)"), "{rendered}");
    }

    #[test]
    fn provider_rules_name_the_key_and_hide_the_value() {
        assert_rejected(
            &oidc("name = \"Okta\""),
            "auth.oidc.name",
            "name must be 1-32 characters of [a-z0-9-]",
        );
        assert_rejected(
            &format!("{}\n{}", oidc("name = \"okta\""), oidc("name = \"okta\"")),
            "auth.oidc.okta",
            "duplicate name",
        );
        assert_rejected(
            &oidc("issuer = \"http://idp.example.com\""),
            "auth.oidc.okta.issuer",
            "issuer must be https, or http on a loopback host",
        );
        assert_rejected(
            &oidc("issuer = \"https://user:s3cret@idp.example.com\""),
            "auth.oidc.okta.issuer",
            "invalid issuer",
        );
        assert_rejected(
            &oidc("issuer = \"https://login.microsoftonline.com/common/v2.0\""),
            "auth.oidc.okta.issuer",
            "multi-tenant Microsoft issuers are not allowed",
        );
        assert_rejected(
            &oidc("issuer = \"https://login.microsoftonline.com/organizations\""),
            "auth.oidc.okta.issuer",
            "multi-tenant Microsoft issuers are not allowed",
        );
        assert_rejected(
            &oidc("issuer = \"https://LOGIN.MICROSOFTONLINE.COM/consumers/v2.0\""),
            "auth.oidc.okta.issuer",
            "multi-tenant Microsoft issuers are not allowed",
        );
        assert_rejected(
            &oidc("auto_provision = { org = \"acme\", role = \"owner\" }"),
            "auth.oidc.okta.auto_provision.role",
            "role must not be owner",
        );
        assert_rejected(
            &oidc("auto_provision = { org = \"x\", role = \"member\" }"),
            "auth.oidc.okta.auto_provision.org",
            "invalid org slug",
        );
        assert_rejected(
            &oidc("scopes = [\"email\"]"),
            "auth.oidc.okta.scopes",
            "scopes must include openid",
        );
        assert_rejected(
            "[auth]\ndisable_password = true\n",
            "auth.disable_password",
            "a provider is required when passwords are disabled",
        );
        assert_rejected(
            "[auth]\nenforce_sso_domains = [\"acme.test\"]\n",
            "auth.enforce_sso_domains",
            "a provider is required when SSO domains are set",
        );
    }

    #[test]
    fn github_client_secret_stays_a_string_and_oidc_env_is_ignored() {
        let config = load_from(
            None,
            &[
                ("LLMOBS_AUTH__GITHUB__CLIENT_ID", "gh-client"),
                ("LLMOBS_AUTH__GITHUB__CLIENT_SECRET", "987654321"),
                ("LLMOBS_AUTH__OIDC__0__NAME", "okta"),
                ("LLMOBS_AUTH__OIDC__0__CLIENT_SECRET", "from-env"),
            ],
        )
        .expect("env github");
        let github = config.auth.github.expect("github");
        assert_eq!(github.client_secret.expose(), "987654321");
        assert!(config.auth.oidc.is_empty());

        let kept = load_from(
            Some(oidc("client_secret = \"from-toml\"").as_str()),
            &[("LLMOBS_AUTH__OIDC__0__CLIENT_SECRET", "from-env")],
        )
        .expect("toml oidc");
        assert_eq!(kept.auth.oidc[0].client_secret.expose(), "from-toml");

        let error = load_from(
            Some("[auth.github]\nclient_id = \"gh\"\nclient_secret = 987654321\n"),
            &[],
        )
        .expect_err("number");
        let text = error.to_string();
        assert_eq!(text, "auth.github.client_secret: expected a string");
        assert!(!text.contains("987654321"), "{text}");
    }

    fn assert_rejected(toml: &str, key: &str, reason: &str) {
        let error = load_from(Some(toml), &[]).expect_err("rejected");
        assert_eq!(error.to_string(), format!("{key}: {reason}"));
    }

    fn oidc(extra: &str) -> String {
        let mut lines = vec![
            "name = \"okta\"".to_owned(),
            "display_name = \"Okta\"".to_owned(),
            "issuer = \"https://acme.okta.com\"".to_owned(),
            "client_id = \"client\"".to_owned(),
            "client_secret = \"secret\"".to_owned(),
        ];
        for line in extra.lines() {
            let Some(key) = line.split('=').next() else {
                continue;
            };
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            lines.retain(|existing| !existing.starts_with(&format!("{key} ")));
            lines.push(line.trim().to_owned());
        }
        format!("[[auth.oidc]]\n{}\n", lines.join("\n"))
    }

    const PROVIDERS_TOML: &str = r#"
[auth]
disable_password = true
enforce_sso_domains = ["Acme.test"]

[auth.github]
client_id = "gh-client"
client_secret = "gh-secret-value"

[auth.google]
client_id = "google-client"
client_secret = "google-secret-value"
allowed_domains = ["Acme.TEST"]

[[auth.oidc]]
name = "okta"
display_name = " Okta "
issuer = "https://acme.okta.com"
client_id = "okta-client"
client_secret = "okta-secret-value"
auto_provision = { org = "acme", role = "member" }

[[auth.oidc]]
name = "local"
display_name = "Local"
issuer = "http://127.0.0.1:9"
client_id = "local-client"
client_secret = "local-secret-value"

[[auth.oidc]]
name = "loop"
display_name = "Loop"
issuer = "http://localhost:9"
client_id = "loop-client"
client_secret = "loop-secret-value"

[[auth.oidc]]
name = "v6"
display_name = "V6"
issuer = "http://[::1]:9"
client_id = "v6-client"
client_secret = "v6-secret-value"

[[auth.oidc]]
name = "entra"
display_name = "Entra"
issuer = "https://login.microsoftonline.com/11111111-1111-1111-1111-111111111111/v2.0"
client_id = "entra-client"
client_secret = "entra-secret-value"
scopes = ["openid", "email"]
link_existing_by_email = true
"#;
}
