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

use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use figment::providers::{Format, Serialized, Toml};
use figment::value::{Dict, Map, Value};
use figment::{Figment, Metadata, Profile, Provider};
use ipnet::IpNet;
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

/// Who may register, and the related auth flags (spec 08).
///
/// OAuth provider tables arrive with the providers. They are not fields here.
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
}

impl Default for AuthSettings {
    fn default() -> Self {
        Self {
            signup: default_signup(),
            signup_domains: Vec::new(),
            disable_password: false,
            require_email_verification: false,
            enforce_sso_domains: Vec::new(),
        }
    }
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
    #[serde(default)]
    pub smtp_url: Option<Secret<String>>,
    /// `From` address for outgoing mail.
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
    /// and `init.secret_key` are [`Self::Invalid`] instead, because figment's
    /// type error includes the rejected value. The error is boxed so
    /// [`Result`] stays small.
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
            return ConfigError::Invalid {
                key,
                reason: "expected a string",
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
        _ => None,
    }
}

fn validate(config: &mut ServerConfig) -> Result<(), ConfigError> {
    normalize_domains("auth.signup_domains", &mut config.auth.signup_domains)?;
    normalize_domains(
        "auth.enforce_sso_domains",
        &mut config.auth.enforce_sso_domains,
    )?;
    if config.auth.require_email_verification && !smtp_configured(&config.smtp_url) {
        return Err(ConfigError::Invalid {
            key: "auth.require_email_verification",
            reason: "email verification needs `smtp_url`",
        });
    }
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

fn domain_ok(domain: &str) -> bool {
    !domain.is_empty()
        && !domain.contains('@')
        && !domain.chars().any(|c| c.is_whitespace() || c.is_control())
        && domain.split('.').all(|label| !label.is_empty())
}

fn smtp_configured(smtp_url: &Option<Secret<String>>) -> bool {
    smtp_url
        .as_ref()
        .is_some_and(|url| !url.expose().trim().is_empty())
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
        // all-digit password into a number and then print that number in a
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
    )
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

    use super::{LogFormat, ServerConfig, load_from};
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
            Some("smtp_url = \"smtp://localhost\"\nauth.require_email_verification = true\n"),
            &[],
        )
        .unwrap();
        assert!(config.auth.require_email_verification);
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
}
