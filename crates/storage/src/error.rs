//! Errors returned by storage operations.
//!
//! Messages name the table, segment, offset, sequence number, or size.
//! They never include row content or attribute values.

use std::io;
use std::path::{Path, PathBuf};

use arrow::error::ArrowError;
use uuid::Uuid;

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
        source: io::Error,
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
    /// `catalog.db` could not be opened or migrated.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// The trace anchor state could not be rebuilt.
    #[error(transparent)]
    Anchor(#[from] AnchorError),
    /// The object store could not be opened, or a startup cleanup failed.
    #[error(transparent)]
    Objects(#[from] ObjectError),
    /// The shared DataFusion runtime could not be built.
    #[error("building the query runtime")]
    Runtime,
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

/// Shutdown failed, or a later maintenance call failed.
///
/// Returned by `shutdown`, `apply_retention` and (M5.7) `drop_project`.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The table's writer stopped after a WAL error.
    #[error("WAL writer for {table} failed")]
    WalFailed {
        /// `spans` or `scores`.
        table: &'static str,
    },
    /// The table's writer task panicked.
    #[error("WAL writer for {table} panicked")]
    WriterPanicked {
        /// `spans` or `scores`.
        table: &'static str,
    },
    /// The catalog commit failed.
    #[error(transparent)]
    Commit(#[from] CommitError),
    /// `catalog.db` failed.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
}

/// `Storage::query` failed.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// DataFusion rejected the session or the scan.
    #[error(transparent)]
    DataFusion(#[from] datafusion::error::DataFusionError),
}

/// Inserting a batch into a memtable failed.
///
/// The message names the column. It never includes row content.
#[derive(Debug, thiserror::Error)]
pub enum MemtableError {
    /// A timestamp cannot be turned into a UTC day.
    #[error("time column is outside the supported range")]
    TimeOutOfRange,
    /// The table's time column is absent.
    #[error("batch is missing column {column}")]
    MissingColumn {
        /// Column name, for example `start_time`.
        column: &'static str,
    },
    /// The time column is not nanosecond timestamps, or it contains a null.
    #[error("column {column} has an unexpected type or a null")]
    BadColumn {
        /// Column name.
        column: &'static str,
    },
    /// Filtering or concatenating batches failed.
    #[error("building a memtable batch")]
    Arrow(#[source] ArrowError),
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
    /// A record failed its checksum, ran backwards, or was not a batch of this table.
    ///
    /// `segment` is the file name. `offset` is the byte where the bad record starts.
    #[error("corrupt WAL table {table}, segment {segment}, offset {offset}")]
    Corrupt {
        /// `spans` or `scores`.
        table: &'static str,
        /// Segment file name, for example `00000000000000000042.wal`.
        segment: String,
        /// Byte offset of the corrupt record.
        offset: u64,
    },
    /// A segment file or its directory could not be read or written.
    #[error("{action} {path}: {source}")]
    Io {
        /// What was being done, for example `syncing WAL segment`.
        action: &'static str,
        /// File or directory involved in the failed call.
        path: PathBuf,
        /// The filesystem error.
        #[source]
        source: io::Error,
    },
}

impl WalError {
    /// An I/O failure that names the action and the path.
    pub(crate) fn io(action: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Opening or reading `catalog.db` failed.
///
/// `Corrupt` uses a static description; `SourcesInvariant` names the offending
/// file id. Neither carries row content, attribute values or secrets.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// The database file could not be opened.
    #[error("catalog {path}: {source}")]
    Open {
        /// `<data_dir>/catalog.db`.
        path: PathBuf,
        /// The SQLite error.
        #[source]
        source: sqlx::Error,
    },
    /// A migration could not be applied.
    #[error("catalog migration: {source}")]
    Migrate {
        /// The migration error.
        #[source]
        source: sqlx::migrate::MigrateError,
    },
    /// A query failed.
    #[error("catalog database: {source}")]
    Db {
        /// The SQLite error.
        #[source]
        source: sqlx::Error,
    },
    /// A stored value is not one the catalog can represent.
    #[error("corrupt catalog: {what}")]
    Corrupt {
        /// Static description, for example `unknown table_name`.
        what: &'static str,
    },
    /// Spec 03 §3's `file_sources` invariant failed for one file.
    #[error("catalog file_sources invariant: file {file}")]
    SourcesInvariant {
        /// The offending `data_files.id`.
        file: Uuid,
    },
}

/// One staged-object collection failed.
///
/// [`GcError::Object`] names the key. Keys contain project ids, so callers log
/// this error at debug.
#[derive(Debug, thiserror::Error)]
pub enum GcError {
    /// `catalog.db` rejected the read or the row delete.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// The object store rejected the delete.
    #[error(transparent)]
    Object(#[from] ObjectError),
    /// A test fault armed at a GC hook point.
    #[error("garbage collection was interrupted")]
    Fault,
}

impl CatalogError {
    pub(crate) fn db(source: sqlx::Error) -> Self {
        Self::Db { source }
    }

    pub(crate) fn corrupt(what: &'static str) -> Self {
        Self::Corrupt { what }
    }
}

/// A catalog commit failed.
///
/// A conflict leaves the staged objects in place. After either variant the
/// in-memory catalog is unchanged.
#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    /// A precondition failed and the transaction rolled back.
    #[error("catalog conflict: {reason}")]
    Conflict {
        /// Static reason. Never a path or a cell value.
        reason: &'static str,
    },
    /// SQLite failed, or an injected `CatalogCommit` fault fired.
    #[error("catalog database: {source}")]
    Db {
        /// The SQLite error, or the injected fault wrapped as I/O.
        #[source]
        source: sqlx::Error,
    },
}

impl CommitError {
    pub(crate) fn db(source: sqlx::Error) -> Self {
        Self::Db { source }
    }

    pub(crate) fn conflict(reason: &'static str) -> Self {
        Self::Conflict { reason }
    }
}

/// `register_deletion` or `deletion_status` failed.
#[derive(Debug, thiserror::Error)]
pub enum DeletionError {
    /// The request is malformed. `reason` is static and never holds an id from the scope.
    #[error("invalid deletion: {reason}")]
    Invalid {
        /// Static reason. Never a trace id, user id or session id.
        reason: &'static str,
    },
    /// No deletion with this id was registered.
    #[error("deletion not found")]
    NotFound,
    /// `catalog.db` failed.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
}

/// `put_media`, `get_media` or a media collection failed (spec 03 §9).
///
/// [`MediaError::Object`] names the object key, which holds the blob's hash:
/// media code logs [`MediaError::label`] instead of the error text.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// The blob is malformed. `reason` is static.
    #[error("invalid media: {reason}")]
    Invalid {
        /// Static reason. Never blob content.
        reason: &'static str,
    },
    /// The given hash does not match the bytes.
    #[error("media hash does not match its bytes")]
    HashMismatch,
    /// No `media` row for this hash.
    #[error("media not found")]
    NotFound,
    /// The requested byte range is outside the blob.
    #[error("media range is outside the blob")]
    InvalidRange,
    /// The object store rejected the read or the write.
    #[error(transparent)]
    Object(#[from] ObjectError),
    /// `catalog.db` failed.
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    /// Reading the live set failed.
    #[error("media query: {0}")]
    Query(#[from] datafusion::error::DataFusionError),
    /// An injected test fault.
    #[error("injected fault")]
    Fault,
}

impl MediaError {
    /// The variant name in snake case. Media logs print this instead of the
    /// error text: [`MediaError::Object`] displays an object path, which holds
    /// the blob's hash (spec 03 §9).
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Invalid { .. } => "invalid",
            Self::HashMismatch => "hash_mismatch",
            Self::NotFound => "not_found",
            Self::InvalidRange => "invalid_range",
            Self::Object(_) => "object",
            Self::Catalog(_) => "catalog",
            Self::Query(_) => "query",
            Self::Fault => "fault",
        }
    }
}

/// An object-store read, write, or delete failed.
///
/// `path` is an object key or a filesystem path. The message never includes
/// object bytes.
#[derive(Debug, thiserror::Error)]
pub enum ObjectError {
    /// A local filesystem call failed.
    #[error("{action} {path}: {source}")]
    Io {
        /// What was being done, for example `writing object`.
        action: &'static str,
        /// Filesystem path involved in the failed call.
        path: PathBuf,
        /// The filesystem error.
        #[source]
        source: io::Error,
    },
    /// The final object name is already present.
    ///
    /// Names are unique, so this is a caller bug or a retried upload that
    /// actually landed.
    #[error("object already exists: {path}")]
    AlreadyExists {
        /// Relative object key.
        path: String,
    },
    /// A remote `put` or multipart upload failed.
    ///
    /// The object may already exist: a failed completion is ambiguous. The
    /// flush retries under a new name and leaves this key for GC.
    #[error("object upload {path}: {source}")]
    Upload {
        /// Object key that was being written.
        path: String,
        /// The object-store error.
        #[source]
        source: Box<object_store::Error>,
    },
    /// A store operation other than an upload failed.
    #[error("object store {action} {path}: {source}")]
    Store {
        /// What was being done, for example `deleting object`.
        action: &'static str,
        /// Object key.
        path: String,
        /// The object-store error.
        #[source]
        source: Box<object_store::Error>,
    },
    /// The object key is not a relative path of normal segments.
    #[error("invalid object path")]
    InvalidPath,
    /// The S3 client or the local store could not be built.
    #[error("object store client: {source}")]
    Client {
        /// The object-store error. It does not include credentials.
        #[source]
        source: Box<object_store::Error>,
    },
}

impl ObjectError {
    pub(crate) fn io(action: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.to_path_buf(),
            source,
        }
    }

    pub(crate) fn upload(path: &str, source: object_store::Error) -> Self {
        Self::Upload {
            path: path.to_owned(),
            source: Box::new(source),
        }
    }

    pub(crate) fn store(action: &'static str, path: &str, source: object_store::Error) -> Self {
        Self::Store {
            action,
            path: path.to_owned(),
            source: Box::new(source),
        }
    }

    pub(crate) fn client(source: object_store::Error) -> Self {
        Self::Client {
            source: Box::new(source),
        }
    }
}

/// Encoding a partition as one Parquet file failed.
///
/// The message names the table or a static reason. It never includes row content.
#[derive(Debug, thiserror::Error)]
pub enum ParquetWriteError {
    /// The batches cannot be written: empty, the wrong schema, or a bad level.
    #[error("parquet write: {reason}")]
    Invalid {
        /// Static reason. Never a cell value.
        reason: &'static str,
    },
    /// An Arrow kernel failed while sorting or measuring the batches.
    #[error("parquet write: {source}")]
    Arrow {
        /// The Arrow error.
        #[source]
        source: ArrowError,
    },
    /// The Parquet writer failed.
    #[error("parquet write: {source}")]
    Parquet {
        /// The Parquet error.
        #[source]
        source: Box<parquet::errors::ParquetError>,
    },
}

/// The trace anchor could not partition rows or rebuild its state.
#[derive(Debug, thiserror::Error)]
pub enum AnchorError {
    /// A required column of a spans or traces batch is missing or mistyped.
    #[error("anchor: missing or invalid column {name}")]
    Column {
        /// Column name. Never a row value.
        name: &'static str,
    },
    /// Arrow rejected a batch operation.
    #[error("anchor: {0}")]
    Arrow(#[from] ArrowError),
    /// An async Parquet read during the startup rebuild failed.
    #[error("anchor rebuild: {0}")]
    Parquet(#[source] Box<parquet::errors::ParquetError>),
}

impl AnchorError {
    pub(crate) fn column(name: &'static str) -> Self {
        Self::Column { name }
    }

    pub(crate) fn parquet(source: parquet::errors::ParquetError) -> Self {
        Self::Parquet(Box::new(source))
    }
}

/// Building or merging trace partials failed.
#[derive(Debug, thiserror::Error)]
pub enum PartialsError {
    /// A required column is missing, has the wrong type, or holds an invalid id.
    #[error("partials column {name} is missing or invalid")]
    Column {
        /// The `traces` or `spans` column name. Never a row value.
        name: String,
    },
    /// A `trace_id` value is not a valid id.
    #[error("partials trace id is invalid")]
    InvalidTraceId,
    /// Arrow rejected the output batch.
    #[error("partials: {0}")]
    Arrow(#[from] ArrowError),
}

impl PartialsError {
    pub(crate) fn column(name: &str) -> Self {
        Self::Column {
            name: name.to_owned(),
        }
    }
}

/// A compaction job or pass failed. No variant carries row content.
#[derive(Debug, thiserror::Error)]
pub enum CompactionError {
    /// Reading or merging the input files failed.
    #[error("reading compaction inputs failed")]
    Read {
        /// The DataFusion error. Names files and columns, never values.
        #[source]
        source: datafusion::error::DataFusionError,
    },
    /// Encoding an output failed.
    #[error(transparent)]
    Write(#[from] ParquetWriteError),
    /// Writing, publishing or deleting an object failed.
    #[error(transparent)]
    Object(#[from] ObjectError),
    /// The catalog refused or failed the edit, or a catalog read failed.
    #[error(transparent)]
    Commit(#[from] CommitError),
    /// Recording the output paths in `staged_objects` failed (`Committer::stage`).
    #[error(transparent)]
    Stage(#[from] CatalogError),
    /// Duplicate collapsing failed on corrupt input.
    #[error(transparent)]
    Dedup(#[from] DedupError),
    /// Computing trace partials failed.
    #[error(transparent)]
    Partials(#[from] PartialsError),
    /// The job's inputs break an invariant it relies on.
    #[error("compaction input: {reason}")]
    Input {
        /// Static reason. Never a cell value.
        reason: &'static str,
    },
    /// A blocking task panicked or was cancelled.
    #[error("compaction task failed")]
    Task,
    /// A test hook failed the job.
    #[error("compaction stopped by a test hook")]
    Fault,
    /// The storage is shutting down; the job gave up before committing.
    #[error("compaction stopped for shutdown")]
    Stopped,
    /// The covered-row filter failed while merging or checking inputs.
    #[error("deletion filter: {reason}")]
    Deletion {
        /// Static reason. Never a cell value.
        reason: &'static str,
    },
}

impl CompactionError {
    /// The variant name in snake case. The job failure logs print this instead
    /// of the error text: a DataFusion error raised by a plan that holds the
    /// covered-row predicate may quote its literals (spec 03 §7).
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Read { .. } => "read",
            Self::Write(_) => "write",
            Self::Object(_) => "object",
            Self::Commit(_) => "commit",
            Self::Stage(_) => "stage",
            Self::Dedup(_) => "dedup",
            Self::Partials(_) => "partials",
            Self::Input { .. } => "input",
            Self::Task => "task",
            Self::Fault => "fault",
            Self::Stopped => "stopped",
            Self::Deletion { .. } => "deletion",
        }
    }
}

/// A batch could not be deduplicated. `reason` is a static description, never row content.
#[derive(Debug, thiserror::Error)]
#[error("dedup failed: {reason}")]
pub struct DedupError {
    /// Static reason. Never a cell value.
    reason: &'static str,
}

impl DedupError {
    pub(crate) fn new(reason: &'static str) -> Self {
        Self { reason }
    }

    pub(crate) fn reason(&self) -> &'static str {
        self.reason
    }
}

/// A memtable flush failed before the catalog commit.
///
/// The frozen memtable stays. The task retries with new file names.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FlushError {
    /// Sorting or encoding the frozen batches failed.
    #[error("flush prepare: {0}")]
    Prepare(ParquetWriteError),
    /// Anchor partitioning failed.
    #[error("flush anchor: {0}")]
    Anchor(#[from] AnchorError),
    /// Computing the trace partials of a spans file failed.
    #[error("flush partials: {0}")]
    Partials(PartialsError),
    /// A span identity column was missing or null.
    #[error("flush dedup: {reason}")]
    Dedup {
        /// Static reason. Never a cell value.
        reason: &'static str,
    },
    /// The covered-row filter failed while preparing the flush.
    #[error("deletion filter: {reason}")]
    Deletion {
        /// Static reason. Never a cell value.
        reason: &'static str,
    },
    /// Recording the paths in `staged_objects` failed.
    #[error("flush stage: {0}")]
    Stage(CatalogError),
    /// A durable object write failed. The object may already exist.
    #[error("flush write: {0}")]
    Write(ObjectError),
    /// The catalog edit failed. Staged objects stay for GC.
    #[error("flush commit: {0}")]
    Commit(CommitError),
    /// `spawn_blocking` or an upload task panicked.
    #[error("flush task panicked")]
    Task,
    /// A test hook returned a fault.
    #[error("flush fault")]
    Fault,
}

impl FlushError {
    pub(crate) fn dedup(reason: &'static str) -> Self {
        Self::Dedup { reason }
    }

    pub(crate) fn prepare(source: ParquetWriteError) -> Self {
        Self::Prepare(source)
    }

    pub(crate) fn partials(source: PartialsError) -> Self {
        Self::Partials(source)
    }

    pub(crate) fn stage(source: CatalogError) -> Self {
        Self::Stage(source)
    }

    pub(crate) fn write(source: ObjectError) -> Self {
        Self::Write(source)
    }

    pub(crate) fn commit(source: CommitError) -> Self {
        Self::Commit(source)
    }
}

impl ParquetWriteError {
    pub(crate) fn invalid(reason: &'static str) -> Self {
        Self::Invalid { reason }
    }

    pub(crate) fn arrow(source: ArrowError) -> Self {
        Self::Arrow { source }
    }

    pub(crate) fn parquet(source: parquet::errors::ParquetError) -> Self {
        Self::Parquet {
            source: Box::new(source),
        }
    }
}
