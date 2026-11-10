//! Errors returned by storage operations.
//!
//! Messages name the table, segment, offset, sequence number, or size.
//! They never include row content or attribute values.

use std::path::PathBuf;

use arrow::error::ArrowError;

/// A `[storage]` value failed a parse or a bound check.
///
/// The message names the field and the rule. It never includes the rejected input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    /// `field` is a config path such as `storage.wal.segment_bytes`.
    /// `rule` is a static description of the constraint.
    #[error("{field}: {rule}")]
    Invalid {
        /// Dotted config path, for example `storage.wal.segment_bytes`.
        field: &'static str,
        /// Static rule. Never the rejected input.
        rule: &'static str,
    },
}

impl ConfigError {
    pub(crate) fn invalid(field: &'static str, rule: &'static str) -> Self {
        Self::Invalid { field, rule }
    }
}

/// `Storage::open` failed.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// The configuration is invalid.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// Another process holds `<data_dir>/LOCK`.
    #[error("data directory is locked: {path}")]
    Locked {
        /// The data directory that is already open.
        path: PathBuf,
    },
    /// Creating the data directory or taking the lock failed.
    #[error("{action} {path}: {source}")]
    Io {
        /// What was being done, for example `creating WAL directory`.
        action: &'static str,
        /// Path involved in the failed call.
        path: PathBuf,
        /// The filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// A WAL segment is corrupt somewhere other than a torn tail.
    #[error("corrupt WAL table {table}, segment {segment}, offset {offset}")]
    WalCorrupt {
        /// Table name, `spans` or `scores`.
        table: &'static str,
        /// Segment file name.
        segment: String,
        /// Byte offset of the bad record.
        offset: u64,
    },
    /// Replaying a WAL failed for a reason other than a corrupt record.
    #[error(transparent)]
    WalReplay(#[from] WalError),
}

/// `append_spans` or `append_scores` failed.
///
/// The server maps these to HTTP: `Backpressure` is 429, `WalFailed` and
/// `ShuttingDown` are 503, and `InvalidBatch` is a bug in the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AppendError {
    /// The batch is empty, has the wrong schema, or names another project.
    #[error("invalid batch: {reason}")]
    InvalidBatch {
        /// Static reason. Never row content.
        reason: &'static str,
    },
    /// The memtable is at `storage.memtable.hard_limit`.
    #[error("memtable is at its hard limit")]
    Backpressure,
    /// The WAL writer has failed and will not accept more writes.
    #[error("WAL writer failed")]
    WalFailed,
    /// Shutdown has started.
    #[error("storage is shutting down")]
    ShuttingDown,
}

/// `Storage::query` failed.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// DataFusion rejected the session or the scan.
    #[error(transparent)]
    DataFusion(#[from] datafusion::error::DataFusionError),
}

/// A WAL codec, segment, or replay failure.
///
/// Callers outside the WAL map this into [`OpenError`] or [`AppendError`].
#[derive(Debug, thiserror::Error)]
pub enum WalError {
    /// The IPC payload is not one batch of the table's schema.
    #[error("invalid WAL payload: {reason}")]
    InvalidPayload {
        /// Static reason. Never row content.
        reason: &'static str,
    },
    /// Arrow could not write the record batch.
    #[error("encoding WAL record")]
    Encode(#[source] ArrowError),
}
