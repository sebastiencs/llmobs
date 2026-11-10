//! `[storage]` configuration (D59, D90).
//!
//! Sizes are strings with a binary unit (`"256MiB"`). Durations are strings
//! with a unit (`"120s"`, `"5ms"`). Only the fields M1 reads are here; later
//! milestones add theirs.

use std::path::PathBuf;
use std::time::Duration;

use bytesize::ByteSize;
use serde::de::IntoDeserializer;
use serde::{Deserialize, Deserializer};

use crate::error::ConfigError;

/// Storage settings. `data_dir` is required, so this struct has no [`Default`].
///
/// The server fills `data_dir` from the top-level data directory (spec 12).
/// It stays a field so storage can be opened on its own.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    /// Directory that holds `LOCK`, `wal/`, and, later, `catalog.db`.
    pub data_dir: PathBuf,
    /// WAL writer settings.
    #[serde(default)]
    pub wal: WalConfig,
    /// Memtable accounting.
    #[serde(default)]
    pub memtable: MemtableConfig,
    /// Query execution settings used in M1.
    #[serde(default)]
    pub query: QueryConfig,
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
        if self.query.target_partitions < 1 {
            return Err(ConfigError::invalid(
                "storage.query.target_partitions",
                "must be at least 1",
            ));
        }
        Ok(())
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
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            target_partitions: default_target_partitions(),
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
                "query": { "target_partitions": 1 }
            }"#,
        )
        .unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn a_present_section_keeps_the_other_defaults() {
        let config = parse(
            r#"{
                "data_dir": "/data",
                "wal": { "sync": "5ms" },
                "memtable": {},
                "query": {}
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
    }

    #[test]
    fn only_data_dir_uses_every_default() {
        let config = parse(r#"{"data_dir":"/data"}"#).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("/data"));
        assert_eq!(config.wal, WalConfig::default());
        assert_eq!(config.memtable, MemtableConfig::default());
        assert_eq!(config.query, QueryConfig::default());
        config.validate().unwrap();
    }

    #[test]
    fn config_errors_do_not_echo_the_rejected_value() {
        let rejected = "x".repeat(80);
        let error = WalSync::parse(&rejected).unwrap_err();
        assert!(!error.to_string().contains(&rejected));
        assert!(error.to_string().contains("storage.wal.sync"));
    }
}
