//! `[storage]` configuration (D59, D90).
//!
//! Sizes are strings with a binary unit (`"256MiB"`). Durations are strings
//! with a unit (`"120s"`, `"5ms"`). S3 credentials are the standard `AWS_*`
//! environment variables and are not fields of this struct.

use std::path::PathBuf;
use std::time::Duration;

use bytesize::ByteSize;
use serde::de::IntoDeserializer;
use serde::{Deserialize, Deserializer};

use crate::error::ConfigError;

/// Rule named by a rejected `storage.object_store` value.
const OBJECT_STORE_RULE: &str = "must be file://<absolute path> or s3://<bucket>[/<prefix>]";

/// Storage settings. `data_dir` is required, so this struct has no [`Default`].
///
/// The server fills `data_dir` from the top-level data directory (spec 12).
/// It stays a field so storage can be opened on its own.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    /// Directory that holds `LOCK`, `wal/`, and `catalog.db`.
    pub data_dir: PathBuf,
    /// Object store root. Absent means `file://<data_dir>/objects`, resolved by
    /// [`Self::object_store_url`].
    #[serde(default, deserialize_with = "deserialize_object_store")]
    pub object_store: Option<ObjectStoreUrl>,
    /// S3 client settings. Credentials are not here.
    #[serde(default)]
    pub s3: S3Config,
    /// WAL writer settings.
    #[serde(default)]
    pub wal: WalConfig,
    /// Memtable flush settings.
    #[serde(default)]
    pub flush: FlushConfig,
    /// Memtable accounting.
    #[serde(default)]
    pub memtable: MemtableConfig,
    /// Query execution settings.
    #[serde(default)]
    pub query: QueryConfig,
    /// Trace anchor settings (D86).
    #[serde(default)]
    pub anchor: AnchorConfig,
}

impl StorageConfig {
    /// Checks the bounds in spec 03 and D90.
    ///
    /// The error names the field and the rule, and does not include the rejected value.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.wal.segment_bytes < ByteSize::mib(1) {
            return Err(ConfigError::invalid(
                "storage.wal.segment_bytes",
                "must be at least 1MiB",
            ));
        }
        if self.wal.group_max_bytes < ByteSize::kib(64) {
            return Err(ConfigError::invalid(
                "storage.wal.group_max_bytes",
                "must be at least 64KiB",
            ));
        }
        if self.wal.group_max_wait > Duration::from_secs(1) {
            return Err(ConfigError::invalid(
                "storage.wal.group_max_wait",
                "must be at most 1s",
            ));
        }
        if let WalSync::Interval(interval) = self.wal.sync
            && interval.is_zero()
        {
            return Err(ConfigError::invalid(
                "storage.wal.sync",
                "expected \"always\" or a duration greater than 0",
            ));
        }
        if self.memtable.hard_limit < ByteSize::mib(16) {
            return Err(ConfigError::invalid(
                "storage.memtable.hard_limit",
                "must be at least 16MiB",
            ));
        }
        if self.flush.max_bytes < ByteSize::mib(1) {
            return Err(ConfigError::invalid(
                "storage.flush.max_bytes",
                "must be at least 1MiB",
            ));
        }
        if self.flush.max_bytes >= self.memtable.hard_limit {
            return Err(ConfigError::invalid(
                "storage.flush.max_bytes",
                "must be less than storage.memtable.hard_limit",
            ));
        }
        if self.flush.max_age < Duration::from_millis(100) {
            return Err(ConfigError::invalid(
                "storage.flush.max_age",
                "must be at least 100ms",
            ));
        }
        if self.flush.max_age > Duration::from_secs(60 * 60) {
            return Err(ConfigError::invalid(
                "storage.flush.max_age",
                "must be at most 1h",
            ));
        }
        if !(1..=64).contains(&self.flush.upload_concurrency) {
            return Err(ConfigError::invalid(
                "storage.flush.upload_concurrency",
                "must be from 1 to 64",
            ));
        }
        if self.query.target_partitions < 1 {
            return Err(ConfigError::invalid(
                "storage.query.target_partitions",
                "must be at least 1",
            ));
        }
        if !valid_window(self.query.trace_lookback) {
            return Err(ConfigError::invalid(
                "storage.query.trace_lookback",
                "must be greater than 0 and at most 24h",
            ));
        }
        if let Some(window) = self.anchor.window
            && !valid_window(window)
        {
            return Err(ConfigError::invalid(
                "storage.anchor.window",
                "must be greater than 0 and at most 24h",
            ));
        }
        Ok(())
    }

    /// The configured object store, or `data_dir/objects` when the field is absent.
    pub fn object_store_url(&self) -> ObjectStoreUrl {
        self.object_store
            .clone()
            .unwrap_or_else(|| ObjectStoreUrl::Local(self.data_dir.join("objects")))
    }
}

/// When the WAL writer calls `fdatasync`.
///
/// `"always"` syncs every group. A duration such as `"5ms"` acknowledges after
/// the write and syncs at most that often: a power loss can drop the last
/// interval, and a process crash does not (spec 03 §1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WalSync {
    /// Sync every committed group.
    #[default]
    Always,
    /// Sync at least this often. The duration is greater than zero.
    Interval(Duration),
}

impl WalSync {
    fn parse(raw: &str) -> Result<Self, ConfigError> {
        if raw == "always" {
            return Ok(Self::Always);
        }
        let duration: Duration = humantime_serde::deserialize(raw.into_deserializer()).map_err(
            |_error: serde::de::value::Error| {
                ConfigError::invalid(
                    "storage.wal.sync",
                    "expected \"always\" or a duration greater than 0",
                )
            },
        )?;
        if duration.is_zero() {
            return Err(ConfigError::invalid(
                "storage.wal.sync",
                "expected \"always\" or a duration greater than 0",
            ));
        }
        Ok(Self::Interval(duration))
    }
}

fn deserialize_wal_sync<'de, D>(deserializer: D) -> Result<WalSync, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    WalSync::parse(&raw).map_err(serde::de::Error::custom)
}

fn default_sync() -> WalSync {
    WalSync::Always
}

fn default_segment_bytes() -> ByteSize {
    ByteSize::mib(64)
}

fn default_group_max_bytes() -> ByteSize {
    ByteSize::mib(8)
}

fn default_group_max_wait() -> Duration {
    Duration::from_millis(2)
}

/// `[storage.wal]`.
///
/// Each field's serde default is the spec value. [`ByteSize`] and [`Duration`]
/// themselves default to zero, so a present section with a missing key must
/// not use those.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WalConfig {
    /// `"always"`, or a duration string greater than zero.
    #[serde(default = "default_sync", deserialize_with = "deserialize_wal_sync")]
    pub sync: WalSync,
    /// Rotate to a new segment after this many bytes. A group may run past it.
    #[serde(default = "default_segment_bytes")]
    pub segment_bytes: ByteSize,
    /// Close a group once it reaches this many bytes, unless it holds one request.
    #[serde(default = "default_group_max_bytes")]
    pub group_max_bytes: ByteSize,
    /// Close a group this long after its first request.
    #[serde(default = "default_group_max_wait", with = "humantime_serde")]
    pub group_max_wait: Duration,
    /// Keep going when a non-tail record is corrupt (spec 03 §1 `--wal-skip-corrupt`).
    #[serde(default)]
    pub skip_corrupt: bool,
}

impl Default for WalConfig {
    fn default() -> Self {
        Self {
            sync: default_sync(),
            segment_bytes: default_segment_bytes(),
            group_max_bytes: default_group_max_bytes(),
            group_max_wait: default_group_max_wait(),
            skip_corrupt: false,
        }
    }
}

fn default_hard_limit() -> ByteSize {
    ByteSize::gib(1)
}

/// `[storage.memtable]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemtableConfig {
    /// Refuse appends once the memtable's accounted bytes exceed this.
    #[serde(default = "default_hard_limit")]
    pub hard_limit: ByteSize,
}

impl Default for MemtableConfig {
    fn default() -> Self {
        Self {
            hard_limit: default_hard_limit(),
        }
    }
}

/// `[storage.query]` fields used before the memory pool lands (M6).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryConfig {
    /// DataFusion `target_partitions`. Defaults to the process parallelism.
    #[serde(default = "default_target_partitions")]
    pub target_partitions: usize,
    /// Bytes of Parquet footer and page-index metadata DataFusion may cache.
    ///
    /// Zero is allowed. [`ByteSize`] cannot represent a negative size.
    #[serde(default = "default_metadata_cache_bytes")]
    pub metadata_cache_bytes: ByteSize,
    /// Look-back for trace lists, and the default anchor window (D90, D86).
    #[serde(default = "default_trace_lookback", with = "humantime_serde")]
    pub trace_lookback: Duration,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            target_partitions: default_target_partitions(),
            metadata_cache_bytes: default_metadata_cache_bytes(),
            trace_lookback: default_trace_lookback(),
        }
    }
}

fn default_target_partitions() -> usize {
    // `available_parallelism` fails only when the OS cannot report a count.
    // One partition is the minimum `validate` allows.
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

fn default_metadata_cache_bytes() -> ByteSize {
    ByteSize::mib(512)
}

fn default_trace_lookback() -> Duration {
    Duration::from_secs(60 * 60)
}

fn valid_window(window: Duration) -> bool {
    !window.is_zero() && window <= Duration::from_secs(24 * 60 * 60)
}

/// `[storage.anchor]` (D86).
///
/// `None` means the anchor window is `query.trace_lookback`. The resolved
/// value is checked by [`StorageConfig::validate`] and used at open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AnchorConfig {
    /// How close to a UTC midnight a row must start to need an anchor entry.
    #[serde(default, with = "humantime_serde")]
    pub window: Option<Duration>,
}

/// Where flushed Parquet files live.
///
/// Deserialized from `file://<absolute path>` or `s3://<bucket>[/<prefix>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectStoreUrl {
    /// A directory on the local filesystem.
    Local(PathBuf),
    /// An S3 bucket. `prefix` has no leading or trailing slash; empty is the bucket root.
    S3 {
        /// Bucket name.
        bucket: String,
        /// Key prefix under the bucket, without surrounding slashes.
        prefix: String,
    },
}

impl ObjectStoreUrl {
    fn parse(raw: &str) -> Result<Self, ConfigError> {
        if raw.contains(['?', '#']) {
            return Err(ConfigError::invalid(
                "storage.object_store",
                OBJECT_STORE_RULE,
            ));
        }
        if let Some(path) = raw.strip_prefix("file://") {
            let path = PathBuf::from(path);
            if path.is_absolute() {
                return Ok(Self::Local(path));
            }
        } else if let Some(rest) = raw.strip_prefix("s3://") {
            let (bucket, prefix) = match rest.split_once('/') {
                Some((bucket, prefix)) => (bucket, prefix.trim_matches('/')),
                None => (rest, ""),
            };
            if !bucket.is_empty() && !bucket.contains([' ', '/']) {
                return Ok(Self::S3 {
                    bucket: bucket.to_owned(),
                    prefix: prefix.to_owned(),
                });
            }
        }
        Err(ConfigError::invalid(
            "storage.object_store",
            OBJECT_STORE_RULE,
        ))
    }
}

fn deserialize_object_store<'de, D>(deserializer: D) -> Result<Option<ObjectStoreUrl>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    ObjectStoreUrl::parse(&raw)
        .map(Some)
        .map_err(serde::de::Error::custom)
}

/// `[storage.s3]`. Credentials come from the standard `AWS_*` environment.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct S3Config {
    /// Region. Absent means the `AWS_REGION` environment variable.
    #[serde(default)]
    pub region: Option<String>,
    /// Custom endpoint, for MinIO or R2. Absent means AWS.
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Address the bucket as a path (`endpoint/bucket`) instead of a subdomain.
    #[serde(default)]
    pub force_path_style: bool,
    /// Allow an `http://` endpoint. A local MinIO needs this.
    #[serde(default)]
    pub allow_http: bool,
}

fn default_flush_max_bytes() -> ByteSize {
    ByteSize::mib(256)
}

fn default_flush_max_age() -> Duration {
    Duration::from_secs(120)
}

fn default_upload_concurrency() -> usize {
    8
}

/// `[storage.flush]`.
///
/// Each field's serde default is the spec value. [`ByteSize`] and [`Duration`]
/// themselves default to zero, so a present section with a missing key must
/// not use those.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlushConfig {
    /// Freeze a memtable once it reaches this many accounted bytes.
    #[serde(default = "default_flush_max_bytes")]
    pub max_bytes: ByteSize,
    /// Freeze a memtable once its oldest row has been buffered this long.
    #[serde(default = "default_flush_max_age", with = "humantime_serde")]
    pub max_age: Duration,
    /// How many Parquet files one flush uploads at a time.
    #[serde(default = "default_upload_concurrency")]
    pub upload_concurrency: usize,
}

impl Default for FlushConfig {
    fn default() -> Self {
        Self {
            max_bytes: default_flush_max_bytes(),
            max_age: default_flush_max_age(),
            upload_concurrency: default_upload_concurrency(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Result<StorageConfig, String> {
        serde_json::from_str(json).map_err(|error| error.to_string())
    }

    #[test]
    fn sizes_and_durations_parse() {
        let config = parse(
            r#"{
                "data_dir": "/data",
                "wal": { "segment_bytes": "256MiB", "group_max_wait": "120s" },
                "memtable": { "hard_limit": "1GiB" }
            }"#,
        )
        .unwrap();
        assert_eq!(config.wal.segment_bytes, ByteSize::mib(256));
        assert_eq!(config.wal.group_max_wait, Duration::from_secs(120));
        assert_eq!(config.memtable.hard_limit, ByteSize::gib(1));
    }

    #[test]
    fn wal_sync_parses_always_and_a_positive_duration() {
        let always = parse(r#"{"data_dir":"/data","wal":{"sync":"always"}}"#).unwrap();
        assert_eq!(always.wal.sync, WalSync::Always);

        let interval = parse(r#"{"data_dir":"/data","wal":{"sync":"5ms"}}"#).unwrap();
        assert_eq!(
            interval.wal.sync,
            WalSync::Interval(Duration::from_millis(5))
        );
    }

    #[test]
    fn wal_sync_rejects_zero_never_and_empty() {
        for value in ["0ms", "never", ""] {
            let error = parse(&format!(
                r#"{{"data_dir":"/data","wal":{{"sync":"{value}"}}}}"#
            ))
            .unwrap_err();
            assert!(
                error.contains("storage.wal.sync"),
                "{value:?} produced {error}"
            );
        }
    }

    #[test]
    fn unknown_keys_fail_at_every_level() {
        let samples = [
            r#"{"data_dir":"/data","extra":1}"#,
            r#"{"data_dir":"/data","wal":{"extra":1}}"#,
            r#"{"data_dir":"/data","memtable":{"extra":1}}"#,
            r#"{"data_dir":"/data","query":{"extra":1}}"#,
            r#"{"data_dir":"/data","flush":{"extra":1}}"#,
            r#"{"data_dir":"/data","anchor":{"extra":1}}"#,
            r#"{"data_dir":"/data","s3":{"extra":1}}"#,
        ];
        for json in samples {
            let error = parse(json).unwrap_err();
            assert!(error.contains("extra"), "{json} produced {error}");
        }
    }

    #[test]
    fn each_bound_fails_validate_with_its_message() {
        let cases = [
            (
                r#"{"data_dir":"/data","wal":{"segment_bytes":"1KiB"}}"#,
                "storage.wal.segment_bytes: must be at least 1MiB",
            ),
            (
                r#"{"data_dir":"/data","wal":{"group_max_bytes":"1KiB"}}"#,
                "storage.wal.group_max_bytes: must be at least 64KiB",
            ),
            (
                r#"{"data_dir":"/data","wal":{"group_max_wait":"2s"}}"#,
                "storage.wal.group_max_wait: must be at most 1s",
            ),
            (
                r#"{"data_dir":"/data","memtable":{"hard_limit":"1MiB"}}"#,
                "storage.memtable.hard_limit: must be at least 16MiB",
            ),
            (
                r#"{"data_dir":"/data","query":{"target_partitions":0}}"#,
                "storage.query.target_partitions: must be at least 1",
            ),
            (
                r#"{"data_dir":"/data","flush":{"max_bytes":"1KiB"}}"#,
                "storage.flush.max_bytes: must be at least 1MiB",
            ),
            (
                r#"{"data_dir":"/data","flush":{"max_bytes":"1GiB"}}"#,
                "storage.flush.max_bytes: must be less than storage.memtable.hard_limit",
            ),
            (
                r#"{"data_dir":"/data","memtable":{"hard_limit":"16MiB"},"flush":{"max_bytes":"16MiB"}}"#,
                "storage.flush.max_bytes: must be less than storage.memtable.hard_limit",
            ),
            (
                r#"{"data_dir":"/data","flush":{"max_age":"99ms"}}"#,
                "storage.flush.max_age: must be at least 100ms",
            ),
            (
                r#"{"data_dir":"/data","flush":{"max_age":"3601s"}}"#,
                "storage.flush.max_age: must be at most 1h",
            ),
            (
                r#"{"data_dir":"/data","flush":{"upload_concurrency":0}}"#,
                "storage.flush.upload_concurrency: must be from 1 to 64",
            ),
            (
                r#"{"data_dir":"/data","flush":{"upload_concurrency":65}}"#,
                "storage.flush.upload_concurrency: must be from 1 to 64",
            ),
            (
                r#"{"data_dir":"/data","query":{"trace_lookback":"0s"}}"#,
                "storage.query.trace_lookback: must be greater than 0 and at most 24h",
            ),
            (
                r#"{"data_dir":"/data","query":{"trace_lookback":"25h"}}"#,
                "storage.query.trace_lookback: must be greater than 0 and at most 24h",
            ),
            (
                r#"{"data_dir":"/data","anchor":{"window":"0s"}}"#,
                "storage.anchor.window: must be greater than 0 and at most 24h",
            ),
            (
                r#"{"data_dir":"/data","anchor":{"window":"25h"}}"#,
                "storage.anchor.window: must be greater than 0 and at most 24h",
            ),
        ];
        for (json, message) in cases {
            let config = parse(json).unwrap();
            let error = config.validate().unwrap_err();
            assert_eq!(error.to_string(), message);
        }
    }

    #[test]
    fn bounds_at_the_limit_pass() {
        let config = parse(
            r#"{
                "data_dir": "/data",
                "wal": {
                    "segment_bytes": "1MiB",
                    "group_max_bytes": "64KiB",
                    "group_max_wait": "1s"
                },
                "memtable": { "hard_limit": "16MiB" },
                "flush": {
                    "max_bytes": "1MiB",
                    "max_age": "100ms",
                    "upload_concurrency": 1
                },
                "query": { "target_partitions": 1, "metadata_cache_bytes": "0B" }
            }"#,
        )
        .unwrap();
        config.validate().unwrap();
        let upper = parse(
            r#"{
                "data_dir": "/data",
                "flush": { "max_age": "1h", "upload_concurrency": 64 }
            }"#,
        )
        .unwrap();
        upper.validate().unwrap();
        assert_eq!(upper.flush.max_age, Duration::from_secs(60 * 60));
        assert_eq!(upper.flush.upload_concurrency, 64);
    }

    #[test]
    fn a_present_section_keeps_the_other_defaults() {
        let config = parse(
            r#"{
                "data_dir": "/data",
                "wal": { "sync": "5ms" },
                "memtable": {},
                "query": {},
                "anchor": {}
            }"#,
        )
        .unwrap();
        assert_eq!(config.wal.sync, WalSync::Interval(Duration::from_millis(5)));
        assert_eq!(config.wal.segment_bytes, ByteSize::mib(64));
        assert_eq!(config.wal.group_max_bytes, ByteSize::mib(8));
        assert_eq!(config.wal.group_max_wait, Duration::from_millis(2));
        assert!(!config.wal.skip_corrupt);
        assert_eq!(config.memtable, MemtableConfig::default());
        assert_eq!(config.query, QueryConfig::default());
        assert_eq!(config.flush, FlushConfig::default());
        assert_eq!(config.s3, S3Config::default());
        assert_eq!(config.anchor, AnchorConfig::default());
    }

    #[test]
    fn anchor_window_defaults_to_trace_lookback_and_parses_durations() {
        let default = parse(r#"{"data_dir":"/data"}"#).unwrap();
        assert_eq!(default.query.trace_lookback, Duration::from_secs(3_600));
        assert_eq!(default.anchor.window, None);

        let lookback = parse(r#"{"data_dir":"/data","query":{"trace_lookback":"30m"}}"#).unwrap();
        assert_eq!(lookback.query.trace_lookback, Duration::from_secs(1_800));
        let override_window = parse(r#"{"data_dir":"/data","anchor":{"window":"2h"}}"#).unwrap();
        assert_eq!(
            override_window.anchor.window,
            Some(Duration::from_secs(7_200))
        );
        assert_eq!(
            override_window.query.trace_lookback,
            Duration::from_secs(3_600)
        );
        override_window.validate().unwrap();
    }

    #[test]
    fn only_data_dir_uses_every_default() {
        let config = parse(r#"{"data_dir":"/data"}"#).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("/data"));
        assert_eq!(config.wal, WalConfig::default());
        assert_eq!(config.memtable, MemtableConfig::default());
        assert_eq!(config.query, QueryConfig::default());
        assert_eq!(config.flush, FlushConfig::default());
        assert_eq!(config.s3, S3Config::default());
        assert_eq!(config.anchor, AnchorConfig::default());
        assert_eq!(
            config.object_store_url(),
            ObjectStoreUrl::Local(PathBuf::from("/data/objects"))
        );
        config.validate().unwrap();
    }

    #[test]
    fn config_errors_do_not_echo_the_rejected_value() {
        let rejected = "x".repeat(80);
        let error = WalSync::parse(&rejected).unwrap_err();
        assert!(!error.to_string().contains(&rejected));
        assert!(error.to_string().contains("storage.wal.sync"));
    }

    #[test]
    fn the_default_object_store_is_under_data_dir() {
        let config = parse(r#"{"data_dir":"/var/lib/llmobs"}"#).unwrap();
        assert_eq!(
            config.object_store_url(),
            ObjectStoreUrl::Local(PathBuf::from("/var/lib/llmobs/objects"))
        );
    }

    #[test]
    fn object_store_urls_parse() {
        let bucket = parse(r#"{"data_dir":"/data","object_store":"s3://bucket"}"#).unwrap();
        assert_eq!(
            bucket.object_store_url(),
            ObjectStoreUrl::S3 {
                bucket: "bucket".to_owned(),
                prefix: String::new(),
            }
        );
        let prefixed = parse(r#"{"data_dir":"/data","object_store":"s3://bucket/a/b"}"#).unwrap();
        assert_eq!(
            prefixed.object_store_url(),
            ObjectStoreUrl::S3 {
                bucket: "bucket".to_owned(),
                prefix: "a/b".to_owned(),
            }
        );
        let local = parse(r#"{"data_dir":"/data","object_store":"file:///var/objects"}"#).unwrap();
        assert_eq!(
            local.object_store_url(),
            ObjectStoreUrl::Local(PathBuf::from("/var/objects"))
        );
    }

    #[test]
    fn object_store_urls_fail_with_the_field_name() {
        for value in ["http://x", "file://relative", "s3://"] {
            let error = parse(&format!(
                r#"{{"data_dir":"/data","object_store":"{value}"}}"#
            ))
            .unwrap_err();
            assert!(
                error.contains("storage.object_store"),
                "{value:?} produced {error}"
            );
        }
        let echoed = parse(r#"{"data_dir":"/data","object_store":"http://x"}"#).unwrap_err();
        assert!(!echoed.contains("http://x"));
        let relative =
            parse(r#"{"data_dir":"/data","object_store":"file://relative"}"#).unwrap_err();
        assert!(!relative.contains("relative"));
    }

    #[test]
    fn an_object_store_error_does_not_echo_the_bucket() {
        let bucket = "bucket-7f3a9c2e-not-a-secret";
        let error = parse(&format!(
            r#"{{"data_dir":"/data","object_store":"s3://{bucket}?x=1"}}"#
        ))
        .unwrap_err();
        assert!(error.contains("storage.object_store"));
        assert!(!error.contains(bucket));
    }
}
