//! Compare-and-set edits for `catalog.db`.
//!
//! The mutex is held across the SQLite transaction and the `publish` call.
//! Lock order: this mutex, then a table memtable, then the `Published` write
//! lock. `publish` must not call back into this committer.
//!
//! `Storage` owns one committer. Flush, and later compaction, are its callers.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::NaiveDate;
use llmobs_core::ids::ProjectId;
use sqlx::{Connection, SqliteConnection};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::db::{
    TableState, check_file_sources, load, load_project_states, load_tombstones, open_catalog_db,
    parse_file_id,
};
use super::{Catalog, FileId, FileMeta, FileState, FileTable, table_index};
use crate::RetentionReport;
use crate::deletion::{Deletion, DeletionScope, DeletionStatus, PendingDeletion, Tombstones};
use crate::error::{CatalogError, CommitError, DeletionError, GcError, MediaError, ObjectError};
use crate::hooks::{HookPoint, Hooks};
use crate::objects::Objects;
use crate::projects::ProjectStates;
use crate::wal::Table;

/// One catalog change. Paths in `add` must already be staged.
#[derive(Debug, Clone)]
pub(crate) struct CatalogEdit {
    /// New active files.
    pub add: Vec<FileMeta>,
    /// Rows to mark obsolete, each with the state it must currently have.
    pub remove: Vec<(FileId, FileState)>,
    /// Compare-and-set. `Some` must be greater than the stored sequence.
    pub flushed_seq: Option<(Table, u64)>,
    /// `(traces file, spans file)` pairs for `file_sources` (D88).
    ///
    /// The traces file must be in `add` with table `Traces`; the spans file
    /// must be in `add` with table `Spans` or already be an active `spans` row.
    pub sources: Vec<(FileId, FileId)>,
    /// Files whose `deletions_applied_seq` rises to the given value: the rewriter
    /// checked them and found no covered row. Each must be active and its stored
    /// value lower than the new one.
    pub raise: Vec<(FileId, u64)>,
}

/// What a commit writes besides the file edit, inside the same transaction.
enum Extra {
    /// Nothing beyond the edit.
    None,
    /// `INSERT OR REPLACE` the retention watermark (spec 03 §8). `keep_from`
    /// is already the max of the stored one and the request.
    Retention {
        /// The project whose watermark rises.
        project: ProjectId,
        /// The watermark to store.
        keep_from: NaiveDate,
    },
    /// Records the project `dropping`, removes its retention watermark and
    /// marks its pending deletions applied (spec 03 §7).
    Drop {
        /// The project being dropped.
        project: ProjectId,
        /// The drop's time, ns: the `dropped_at` and `applied_at` to store.
        now: i64,
    },
}

struct CommitterInner {
    connection: SqliteConnection,
    catalog: Arc<Catalog>,
    flushed: [u64; 2],
    /// The pending deletions as last committed (spec 03 §7).
    tombstones: Arc<Tombstones>,
    hooks: Hooks,
    /// Files a commit removed, held until GC collects them.
    ///
    /// The `Arc` count is the reference count of [`crate::gc`]'s in-use check:
    /// a count above one means an older catalog, a provider or a plan still
    /// names the file, and the object must stay.
    retired: HashMap<FileId, Arc<FileMeta>>,
}

/// The single writer of `catalog.db`.
pub(crate) struct Committer {
    inner: Mutex<CommitterInner>,
}

impl std::fmt::Debug for Committer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Committer")
    }
}

impl Committer {
    /// Open `catalog.db`, migrate, and load the active files and tombstones.
    pub(crate) async fn open(data_dir: &Path, hooks: Hooks) -> Result<Self, CatalogError> {
        let mut connection = open_catalog_db(data_dir).await?;
        let (catalog, state) = load(&mut connection).await?;
        check_file_sources(&mut connection).await?;
        let tombstones = load_tombstones(&mut connection).await?;
        Ok(Self {
            inner: Mutex::new(CommitterInner {
                connection,
                catalog: Arc::new(catalog),
                flushed: flushed_of(state),
                tombstones: Arc::new(tombstones),
                hooks,
                retired: HashMap::new(),
            }),
        })
    }

    /// The catalog last committed, or the one loaded at open.
    pub(crate) async fn catalog(&self) -> Arc<Catalog> {
        Arc::clone(&self.inner.lock().await.catalog)
    }

    /// Highest flushed WAL sequence for `table`. Missing row is `0`.
    pub(crate) async fn flushed_seq(&self, table: Table) -> u64 {
        self.inner.lock().await.flushed[table_index(table)]
    }

    /// The pending deletions as last committed.
    pub(crate) async fn tombstones(&self) -> Arc<Tombstones> {
        Arc::clone(&self.inner.lock().await.tombstones)
    }

    /// The project states stored in `catalog.db`: retention watermarks
    /// (spec 03 §8) and dropped projects (spec 03 §7).
    pub(crate) async fn project_states(&self) -> Result<ProjectStates, CatalogError> {
        let mut inner = self.inner.lock().await;
        load_project_states(&mut inner.connection).await
    }

    /// Stores `deletion` with the next `deletion_seq`, then calls `publish` with
    /// the new tombstones before releasing the mutex. A known id changes nothing
    /// and does not call `publish`.
    ///
    /// Validation runs before SQLite is touched, so an invalid request writes
    /// nothing.
    pub(crate) async fn register_deletion<F>(
        &self,
        deletion: &Deletion,
        now: i64,
        publish: F,
    ) -> Result<Registered, DeletionError>
    where
        F: FnOnce(&Arc<Tombstones>),
    {
        let mut inner = self.inner.lock().await;
        if deletion.cutoff <= 0 {
            return Err(DeletionError::Invalid {
                reason: "cutoff must be positive",
            });
        }
        let (kind, trace_ids, users, sessions) = match &deletion.scope {
            DeletionScope::Traces { trace_ids } => {
                if trace_ids.is_empty() {
                    return Err(DeletionError::Invalid {
                        reason: "empty scope",
                    });
                }
                (
                    "traces",
                    trace_ids.iter().map(|trace| *trace.as_bytes()).collect(),
                    HashSet::new(),
                    HashSet::new(),
                )
            }
            DeletionScope::User {
                user_id,
                trace_ids,
                session_ids,
            } => {
                if user_id.is_empty() {
                    return Err(DeletionError::Invalid {
                        reason: "empty user id",
                    });
                }
                (
                    "user",
                    trace_ids.iter().map(|trace| *trace.as_bytes()).collect(),
                    HashSet::from([user_id.clone()]),
                    session_ids.iter().cloned().collect(),
                )
            }
        };
        let known: Option<i64> = sqlx::query_scalar("SELECT 1 FROM deletions WHERE id = ?")
            .bind(deletion.id.to_string())
            .fetch_optional(&mut inner.connection)
            .await
            .map_err(CatalogError::db)?;
        if known.is_some() {
            return Ok(Registered::Known);
        }
        // A dropped project keeps the audit row only: stored already applied,
        // with no scope rows (an applied request keeps none, M5.1). Nothing is
        // published: no pending deletion changed, and a `known_seq` that lags
        // an already-applied deletion is safe for new files (spec 03 §7).
        let dropped: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM projects WHERE project_id = ?")
                .bind(deletion.project.to_string())
                .fetch_optional(&mut inner.connection)
                .await
                .map_err(CatalogError::db)?;
        if dropped.is_some() {
            let seq = inner.tombstones.known_seq() + 1;
            let stored_seq = i64::try_from(seq).map_err(|_| DeletionError::Invalid {
                reason: "integer does not fit the catalog",
            })?;
            let mut transaction = inner.connection.begin().await.map_err(CatalogError::db)?;
            if let Err(error) = sqlx::query(
                "INSERT INTO deletions (
                    id, deletion_seq, project_id, kind, cutoff, status, registered_at, applied_at
                 ) VALUES (?, ?, ?, ?, ?, 'applied', ?, ?)",
            )
            .bind(deletion.id.to_string())
            .bind(stored_seq)
            .bind(deletion.project.to_string())
            .bind(kind)
            .bind(deletion.cutoff)
            .bind(now)
            .bind(now)
            .execute(transaction.as_mut())
            .await
            .map_err(CatalogError::db)
            {
                let _ = transaction.rollback().await;
                return Err(error.into());
            }
            transaction
                .commit()
                .await
                .map_err(|source| DeletionError::Catalog(CatalogError::db(source)))?;
            inner.tombstones = Arc::new(inner.tombstones.with_known_seq(seq));
            return Ok(Registered::New { seq });
        }
        let seq = inner.tombstones.known_seq() + 1;
        // `known_seq` was read from an SQLite INTEGER, so only the `+ 1` above
        // can overflow, and one deletion short of nine quintillion is out of
        // reach. The error keeps the write from ever storing a wrapped value.
        let stored_seq = i64::try_from(seq).map_err(|_| DeletionError::Invalid {
            reason: "integer does not fit the catalog",
        })?;
        let pending = PendingDeletion {
            id: deletion.id,
            seq,
            project: deletion.project,
            cutoff: deletion.cutoff,
            registered_at: now,
            trace_ids,
            users,
            sessions,
        };
        // Production hooks are `Copy`. Tests share one value and have to clone it.
        #[allow(clippy::clone_on_copy)]
        let hooks = inner.hooks.clone();
        let mut transaction = inner.connection.begin().await.map_err(CatalogError::db)?;
        if let Err(error) = sqlx::query(
            "INSERT INTO deletions (
                id, deletion_seq, project_id, kind, cutoff, status, registered_at
             ) VALUES (?, ?, ?, ?, ?, 'pending', ?)",
        )
        .bind(deletion.id.to_string())
        .bind(stored_seq)
        .bind(deletion.project.to_string())
        .bind(kind)
        .bind(deletion.cutoff)
        .bind(now)
        .execute(transaction.as_mut())
        .await
        .map_err(CatalogError::db)
        {
            let _ = transaction.rollback().await;
            return Err(error.into());
        }
        for trace in &pending.trace_ids {
            if let Err(error) =
                sqlx::query("INSERT INTO deletion_traces (deletion_id, trace_id) VALUES (?, ?)")
                    .bind(deletion.id.to_string())
                    .bind(trace.as_slice())
                    .execute(transaction.as_mut())
                    .await
                    .map_err(CatalogError::db)
            {
                let _ = transaction.rollback().await;
                return Err(error.into());
            }
        }
        for user in &pending.users {
            if let Err(error) =
                sqlx::query("INSERT INTO deletion_users (deletion_id, user_id) VALUES (?, ?)")
                    .bind(deletion.id.to_string())
                    .bind(user)
                    .execute(transaction.as_mut())
                    .await
                    .map_err(CatalogError::db)
            {
                let _ = transaction.rollback().await;
                return Err(error.into());
            }
        }
        for session in &pending.sessions {
            if let Err(error) =
                sqlx::query("INSERT INTO deletion_sessions (deletion_id, session_id) VALUES (?, ?)")
                    .bind(deletion.id.to_string())
                    .bind(session)
                    .execute(transaction.as_mut())
                    .await
                    .map_err(CatalogError::db)
            {
                let _ = transaction.rollback().await;
                return Err(error.into());
            }
        }
        hooks.pause(HookPoint::CatalogCommit);
        if let Some(source) = hooks.fail(HookPoint::CatalogCommit) {
            let _ = transaction.rollback().await;
            return Err(DeletionError::Catalog(CatalogError::db(sqlx::Error::Io(
                source,
            ))));
        }
        transaction
            .commit()
            .await
            .map_err(|source| DeletionError::Catalog(CatalogError::db(source)))?;
        inner.tombstones = Arc::new(inner.tombstones.with(pending));
        publish(&inner.tombstones);
        Ok(Registered::New { seq })
    }

    /// Marks a pending deletion applied at `now`, then publishes the tombstones
    /// without it. An id that is not pending is `Ok(false)` and does not call
    /// `publish`.
    ///
    /// An applied request keeps only its id, project, kind, cut-off, sequence
    /// and dates: the ids of the person are erased with the data (spec 03 §7);
    /// the audit record is the server's `meta.db` row.
    pub(crate) async fn mark_applied<F>(
        &self,
        project: ProjectId,
        id: Uuid,
        now: i64,
        publish: F,
    ) -> Result<bool, CatalogError>
    where
        F: FnOnce(&Arc<Tombstones>),
    {
        let mut inner = self.inner.lock().await;
        let mut transaction = inner.connection.begin().await.map_err(CatalogError::db)?;
        let updated = sqlx::query(
            "UPDATE deletions SET status = 'applied', applied_at = ?
             WHERE id = ? AND status = 'pending'",
        )
        .bind(now)
        .bind(id.to_string())
        .execute(transaction.as_mut())
        .await
        .map_err(CatalogError::db)?;
        if updated.rows_affected() == 0 {
            let _ = transaction.rollback().await;
            return Ok(false);
        }
        for delete in [
            "DELETE FROM deletion_traces WHERE deletion_id = ?",
            "DELETE FROM deletion_users WHERE deletion_id = ?",
            "DELETE FROM deletion_sessions WHERE deletion_id = ?",
        ] {
            if let Err(error) = sqlx::query(delete)
                .bind(id.to_string())
                .execute(transaction.as_mut())
                .await
                .map_err(CatalogError::db)
            {
                let _ = transaction.rollback().await;
                return Err(error);
            }
        }
        transaction.commit().await.map_err(CatalogError::db)?;
        inner.tombstones = Arc::new(inner.tombstones.without(project, id));
        publish(&inner.tombstones);
        Ok(true)
    }

    /// `None` for an unknown id.
    pub(crate) async fn deletion_status(
        &self,
        id: Uuid,
    ) -> Result<Option<DeletionStatus>, CatalogError> {
        let mut inner = self.inner.lock().await;
        let row: Option<(String, Option<i64>)> =
            sqlx::query_as("SELECT status, applied_at FROM deletions WHERE id = ?")
                .bind(id.to_string())
                .fetch_optional(&mut inner.connection)
                .await
                .map_err(CatalogError::db)?;
        let Some((status, applied_at)) = row else {
            return Ok(None);
        };
        match status.as_str() {
            "pending" => Ok(Some(DeletionStatus::Pending)),
            "applied" => Ok(Some(DeletionStatus::Applied {
                applied_at: applied_at
                    .ok_or_else(|| CatalogError::corrupt("invalid applied_at"))?,
            })),
            _ => Err(CatalogError::corrupt("invalid deletion status")),
        }
    }

    /// Refreshes the blob's `last_seen_at` to `max(last_seen_at, now)` (spec 03
    /// §9). `true` when the row exists.
    pub(crate) async fn touch_media(
        &self,
        project: ProjectId,
        sha: &str,
        now: i64,
    ) -> Result<bool, CatalogError> {
        let mut inner = self.inner.lock().await;
        let updated = sqlx::query(
            "UPDATE media SET last_seen_at = MAX(last_seen_at, ?)
             WHERE project_id = ? AND sha256 = ?",
        )
        .bind(now)
        .bind(project.to_string())
        .bind(sha)
        .execute(&mut inner.connection)
        .await
        .map_err(CatalogError::db)?;
        Ok(updated.rows_affected() == 1)
    }

    /// `INSERT OR REPLACE` the blob's path in `staged_objects`: two puts of one
    /// blob may race.
    pub(crate) async fn stage_media(&self, path: &str, now: i64) -> Result<(), CatalogError> {
        let mut inner = self.inner.lock().await;
        sqlx::query("INSERT OR REPLACE INTO staged_objects (path, created_at) VALUES (?, ?)")
            .bind(path)
            .bind(now)
            .execute(&mut inner.connection)
            .await
            .map_err(CatalogError::db)?;
        Ok(())
    }

    /// In one transaction: `INSERT OR IGNORE` the `media` row (`first_seen_at =
    /// last_seen_at = now`), then delete the `staged_objects` row of `path`
    /// (spec 03 §9). A dropped project is refused with `Ok(false)`: the
    /// transaction rolls back and the staged row stays, so staged-object GC
    /// deletes the object later.
    pub(crate) async fn insert_media(
        &self,
        project: ProjectId,
        sha: &str,
        mime_type: &str,
        size: u64,
        path: &str,
        now: i64,
    ) -> Result<bool, CatalogError> {
        let stored_size = i64::try_from(size)
            .map_err(|_| CatalogError::corrupt("integer does not fit the catalog"))?;
        let mut inner = self.inner.lock().await;
        let mut transaction = inner.connection.begin().await.map_err(CatalogError::db)?;
        // A put that passed its `is_dropped` check before the drop's commit
        // must not land a row after the drop deleted the project's media
        // (spec 03 §7).
        let dropped: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM projects WHERE project_id = ?")
                .bind(project.to_string())
                .fetch_optional(transaction.as_mut())
                .await
                .map_err(CatalogError::db)?;
        if dropped.is_some() {
            let _ = transaction.rollback().await;
            return Ok(false);
        }
        if let Err(error) = sqlx::query(
            "INSERT OR IGNORE INTO media (
                project_id, sha256, mime_type, size_bytes, first_seen_at, last_seen_at
             ) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(project.to_string())
        .bind(sha)
        .bind(mime_type)
        .bind(stored_size)
        .bind(now)
        .bind(now)
        .execute(transaction.as_mut())
        .await
        .map_err(CatalogError::db)
        {
            let _ = transaction.rollback().await;
            return Err(error);
        }
        if let Err(error) = sqlx::query("DELETE FROM staged_objects WHERE path = ?")
            .bind(path)
            .execute(transaction.as_mut())
            .await
            .map_err(CatalogError::db)
        {
            let _ = transaction.rollback().await;
            return Err(error);
        }
        transaction.commit().await.map_err(CatalogError::db)?;
        Ok(true)
    }

    /// `(mime_type, size)` of the blob's row, or `None`.
    pub(crate) async fn media_row(
        &self,
        project: ProjectId,
        sha: &str,
    ) -> Result<Option<(String, u64)>, CatalogError> {
        let mut inner = self.inner.lock().await;
        let row: Option<(String, i64)> = sqlx::query_as(
            "SELECT mime_type, size_bytes FROM media WHERE project_id = ? AND sha256 = ?",
        )
        .bind(project.to_string())
        .bind(sha)
        .fetch_optional(&mut inner.connection)
        .await
        .map_err(CatalogError::db)?;
        row.map(|(mime_type, size)| {
            let size = u64::try_from(size).map_err(|_| CatalogError::corrupt("invalid size"))?;
            Ok((mime_type, size))
        })
        .transpose()
    }

    /// Projects with at least one media row, in id order.
    pub(crate) async fn media_projects(&self) -> Result<Vec<ProjectId>, CatalogError> {
        let mut inner = self.inner.lock().await;
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT DISTINCT project_id FROM media ORDER BY project_id")
                .fetch_all(&mut inner.connection)
                .await
                .map_err(CatalogError::db)?;
        rows.into_iter()
            .map(|project| {
                project
                    .parse()
                    .map_err(|_| CatalogError::corrupt("media project_id"))
            })
            .collect()
    }

    /// Hashes of the project's rows with `last_seen_at < before`, in hash order.
    pub(crate) async fn stale_media(
        &self,
        project: ProjectId,
        before: i64,
    ) -> Result<Vec<String>, CatalogError> {
        let mut inner = self.inner.lock().await;
        sqlx::query_scalar(
            "SELECT sha256 FROM media WHERE project_id = ? AND last_seen_at < ? ORDER BY sha256",
        )
        .bind(project.to_string())
        .bind(before)
        .fetch_all(&mut inner.connection)
        .await
        .map_err(CatalogError::db)
    }

    /// Deletes one stale blob (spec 03 §9), under the mutex and in this order:
    /// 1. if the row is gone or has `last_seen_at >= before`, return `Ok(None)`;
    /// 2. in one transaction, delete the `media` row and `INSERT OR REPLACE` a
    ///    `staged_objects` row for its path with `created_at = 0`;
    /// 3. hook `MediaBeforeDelete` (pause, then fail → [`MediaError::Fault`]);
    /// 4. run `delete` (the object);
    /// 5. delete the `staged_objects` row.
    ///
    /// Returns the bytes deleted. A crash after step 2 leaves a staged path
    /// with no row, which staged-object GC deletes; never a row without its
    /// object.
    pub(crate) async fn collect_media<F, Fut>(
        &self,
        project: ProjectId,
        sha: &str,
        before: i64,
        hooks: &Hooks,
        delete: F,
    ) -> Result<Option<u64>, MediaError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<u64, ObjectError>>,
    {
        let mut inner = self.inner.lock().await;
        let last_seen: Option<i64> = sqlx::query_scalar(
            "SELECT last_seen_at FROM media WHERE project_id = ? AND sha256 = ?",
        )
        .bind(project.to_string())
        .bind(sha)
        .fetch_optional(&mut inner.connection)
        .await
        .map_err(CatalogError::db)?;
        let Some(last_seen) = last_seen else {
            return Ok(None);
        };
        if last_seen >= before {
            return Ok(None);
        }
        let path = Objects::media_path(project, sha);
        let mut transaction = inner.connection.begin().await.map_err(CatalogError::db)?;
        if let Err(error) = sqlx::query("DELETE FROM media WHERE project_id = ? AND sha256 = ?")
            .bind(project.to_string())
            .bind(sha)
            .execute(transaction.as_mut())
            .await
            .map_err(CatalogError::db)
        {
            let _ = transaction.rollback().await;
            return Err(error.into());
        }
        if let Err(error) =
            sqlx::query("INSERT OR REPLACE INTO staged_objects (path, created_at) VALUES (?, 0)")
                .bind(&path)
                .execute(transaction.as_mut())
                .await
                .map_err(CatalogError::db)
        {
            let _ = transaction.rollback().await;
            return Err(error.into());
        }
        transaction.commit().await.map_err(CatalogError::db)?;
        hooks.pause(HookPoint::MediaBeforeDelete);
        if hooks.fail(HookPoint::MediaBeforeDelete).is_some() {
            return Err(MediaError::Fault);
        }
        let bytes = delete().await?;
        sqlx::query("DELETE FROM staged_objects WHERE path = ?")
            .bind(&path)
            .execute(&mut inner.connection)
            .await
            .map_err(CatalogError::db)?;
        Ok(Some(bytes))
    }

    /// Paths still in `staged_objects`, in path order.
    #[cfg(test)]
    pub(crate) async fn staged_paths(&self) -> Result<Vec<String>, CatalogError> {
        let mut inner = self.inner.lock().await;
        sqlx::query_scalar("SELECT path FROM staged_objects ORDER BY path")
            .fetch_all(&mut inner.connection)
            .await
            .map_err(CatalogError::db)
    }

    /// Hashes of the project's media rows, in hash order.
    pub(crate) async fn media_of(&self, project: ProjectId) -> Result<Vec<String>, CatalogError> {
        let mut inner = self.inner.lock().await;
        sqlx::query_scalar("SELECT sha256 FROM media WHERE project_id = ? ORDER BY sha256")
            .bind(project.to_string())
            .fetch_all(&mut inner.connection)
            .await
            .map_err(CatalogError::db)
    }

    /// Deletes the project's `media` row of `sha`.
    pub(crate) async fn delete_media_row(
        &self,
        project: ProjectId,
        sha: &str,
    ) -> Result<(), CatalogError> {
        let mut inner = self.inner.lock().await;
        sqlx::query("DELETE FROM media WHERE project_id = ? AND sha256 = ?")
            .bind(project.to_string())
            .bind(sha)
            .execute(&mut inner.connection)
            .await
            .map_err(CatalogError::db)?;
        Ok(())
    }

    /// Projects whose drop has not finished its media step, in id order.
    pub(crate) async fn dropping_projects(&self) -> Result<Vec<ProjectId>, CatalogError> {
        let mut inner = self.inner.lock().await;
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT project_id FROM projects WHERE state = 'dropping' ORDER BY project_id",
        )
        .fetch_all(&mut inner.connection)
        .await
        .map_err(CatalogError::db)?;
        rows.into_iter()
            .map(|project| {
                project
                    .parse()
                    .map_err(|_| CatalogError::corrupt("dropped project"))
            })
            .collect()
    }

    /// Marks the project's drop finished: `state = 'dropped'`.
    pub(crate) async fn finish_drop(&self, project: ProjectId) -> Result<(), CatalogError> {
        let mut inner = self.inner.lock().await;
        sqlx::query("UPDATE projects SET state = 'dropped' WHERE project_id = ?")
            .bind(project.to_string())
            .execute(&mut inner.connection)
            .await
            .map_err(CatalogError::db)?;
        Ok(())
    }

    /// For every active traces file with a source among `spans`, all of its
    /// source pairs, deduplicated and in `(traces, spans)` order.
    pub(crate) async fn derived_traces(
        &self,
        spans: &[FileId],
    ) -> Result<Vec<(FileId, FileId)>, CommitError> {
        let mut inner = self.inner.lock().await;
        let mut pairs: BTreeSet<(FileId, FileId)> = BTreeSet::new();
        for chunk in spans.chunks(500) {
            let mut builder = sqlx::QueryBuilder::new(
                "SELECT fs.traces_file_id, fs.spans_file_id FROM file_sources AS fs
                 JOIN data_files AS t ON t.id = fs.traces_file_id
                 WHERE t.state = 'active' AND fs.traces_file_id IN (
                   SELECT traces_file_id FROM file_sources WHERE spans_file_id IN (",
            );
            let mut separated = builder.separated(", ");
            for id in chunk {
                separated.push_bind(id.to_string());
            }
            builder.push("))");
            let rows = builder
                .build_query_as::<(String, String)>()
                .fetch_all(&mut inner.connection)
                .await
                .map_err(CommitError::db)?;
            for (traces, spans) in rows {
                pairs.insert((
                    parse_file_id(&traces).map_err(|_| CommitError::conflict("invalid file id"))?,
                    parse_file_id(&spans).map_err(|_| CommitError::conflict("invalid file id"))?,
                ));
            }
        }
        Ok(pairs.into_iter().collect())
    }

    /// Record `paths` in `staged_objects`. No precondition.
    pub(crate) async fn stage(&self, paths: &[String]) -> Result<(), CatalogError> {
        self.insert_staged(paths, now_ns()).await
    }

    /// [`Self::stage`] with a chosen `created_at`, so tests can age a row.
    #[cfg(test)]
    pub(crate) async fn stage_at(
        &self,
        paths: &[String],
        created_at: i64,
    ) -> Result<(), CatalogError> {
        self.insert_staged(paths, created_at).await
    }

    async fn insert_staged(&self, paths: &[String], created_at: i64) -> Result<(), CatalogError> {
        let mut inner = self.inner.lock().await;
        let mut transaction = inner.connection.begin().await.map_err(CatalogError::db)?;
        for path in paths {
            if let Err(error) =
                sqlx::query("INSERT INTO staged_objects (path, created_at) VALUES (?, ?)")
                    .bind(path)
                    .bind(created_at)
                    .execute(transaction.as_mut())
                    .await
                    .map_err(CatalogError::db)
            {
                let _ = transaction.rollback().await;
                return Err(error);
            }
        }
        transaction.commit().await.map_err(CatalogError::db)?;
        Ok(())
    }

    /// Apply `edit` when its preconditions hold, then call `publish`.
    ///
    /// `publish` runs before this mutex is released. It may lock a memtable
    /// and then `Published`, and it must not call back into this committer.
    ///
    /// A failed precondition rolls back and leaves the staged objects in place.
    /// The in-memory catalog changes only after the SQLite commit succeeds.
    pub(crate) async fn commit<F>(
        &self,
        edit: &CatalogEdit,
        publish: F,
    ) -> Result<Arc<Catalog>, CommitError>
    where
        F: FnOnce(&Arc<Catalog>),
    {
        // Lock order: committer mutex, then (inside `publish`) memtable, then Published.
        let mut inner = self.inner.lock().await;
        let next = commit_inner(&mut inner, edit, &Extra::None).await?;
        publish(&next);
        Ok(next)
    }

    /// Obsoletes every active file of `project` in the days before the
    /// watermark, stores the watermark in the same transaction, then calls
    /// `publish` with the new catalog and the watermark before releasing the
    /// mutex (spec 03 §8). The watermark is the later of the stored one and
    /// `keep_from`: it never moves back.
    pub(crate) async fn apply_retention<F>(
        &self,
        project: ProjectId,
        keep_from: NaiveDate,
        publish: F,
    ) -> Result<RetentionReport, CommitError>
    where
        F: FnOnce(&Arc<Catalog>, NaiveDate),
    {
        let mut inner = self.inner.lock().await;
        // A dropped project has nothing to retain: no commit, no publish.
        let dropped: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM projects WHERE project_id = ?")
                .bind(project.to_string())
                .fetch_optional(&mut inner.connection)
                .await
                .map_err(CommitError::db)?;
        if dropped.is_some() {
            return Ok(RetentionReport {
                files: 0,
                bytes: 0,
                keep_from,
            });
        }
        let stored: Option<String> =
            sqlx::query_scalar("SELECT keep_from FROM retention_watermarks WHERE project_id = ?")
                .bind(project.to_string())
                .fetch_optional(&mut inner.connection)
                .await
                .map_err(CommitError::db)?;
        let stored = stored
            .map(|day| NaiveDate::parse_from_str(&day, "%Y-%m-%d"))
            .transpose()
            .map_err(|_| CommitError::conflict("invalid retention watermark"))?;
        let watermark = stored.map_or(keep_from, |stored| stored.max(keep_from));
        // Every active file of the project before the watermark, all three
        // tables. A `spans` file and its `traces` file share their day, so
        // both sides of every pair go in one edit.
        let mut remove = Vec::new();
        let mut bytes = 0_u64;
        if let Some(last_day) = watermark.pred_opt() {
            for table in [FileTable::Spans, FileTable::Traces, FileTable::Scores] {
                for file in inner
                    .catalog
                    .files(table, project, NaiveDate::MIN..=last_day)
                {
                    remove.push((file.id, FileState::Active));
                    bytes = bytes.saturating_add(file.size);
                }
            }
        }
        let edit = CatalogEdit {
            add: Vec::new(),
            remove,
            flushed_seq: None,
            sources: Vec::new(),
            raise: Vec::new(),
        };
        let next = commit_inner(
            &mut inner,
            &edit,
            &Extra::Retention {
                project,
                keep_from: watermark,
            },
        )
        .await?;
        publish(&next, watermark);
        Ok(RetentionReport {
            files: u64::try_from(edit.remove.len()).unwrap_or(u64::MAX),
            bytes,
            keep_from: watermark,
        })
    }

    /// Obsoletes every active file of `project` in all three tables, records
    /// the project `dropping`, removes its retention watermark and marks its
    /// pending deletions applied, all in one transaction (spec 03 §7), then
    /// calls `publish` with the new catalog and the tombstones without the
    /// project's deletions before releasing the mutex.
    ///
    /// A project already in `projects` is `Ok(false)`: nothing changed and
    /// `publish` was not called.
    pub(crate) async fn drop_project<F>(
        &self,
        project: ProjectId,
        now: i64,
        publish: F,
    ) -> Result<bool, CommitError>
    where
        F: FnOnce(&Arc<Catalog>, &Arc<Tombstones>),
    {
        let mut inner = self.inner.lock().await;
        let known: Option<i64> = sqlx::query_scalar("SELECT 1 FROM projects WHERE project_id = ?")
            .bind(project.to_string())
            .fetch_optional(&mut inner.connection)
            .await
            .map_err(CommitError::db)?;
        if known.is_some() {
            return Ok(false);
        }
        // Every active file of the project, all three tables. A `spans` file
        // and its `traces` file share their day, so both sides of every pair
        // go in one edit.
        let mut remove = Vec::new();
        for table in [FileTable::Spans, FileTable::Traces, FileTable::Scores] {
            for file in inner
                .catalog
                .files(table, project, NaiveDate::MIN..=NaiveDate::MAX)
            {
                remove.push((file.id, FileState::Active));
            }
        }
        let edit = CatalogEdit {
            add: Vec::new(),
            remove,
            flushed_seq: None,
            sources: Vec::new(),
            raise: Vec::new(),
        };
        let next = commit_inner(&mut inner, &edit, &Extra::Drop { project, now }).await?;
        inner.tombstones = Arc::new(inner.tombstones.without_project(project));
        publish(&next, &inner.tombstones);
        Ok(true)
    }

    /// Staged paths whose `created_at` is strictly before `cutoff_ns`.
    pub(crate) async fn staged_before(&self, cutoff_ns: i64) -> Result<Vec<String>, CatalogError> {
        let mut inner = self.inner.lock().await;
        sqlx::query_scalar("SELECT path FROM staged_objects WHERE created_at < ? ORDER BY path")
            .bind(cutoff_ns)
            .fetch_all(&mut inner.connection)
            .await
            .map_err(CatalogError::db)
    }

    /// Delete one staged object that is not a catalog file or a stored media
    /// blob, then its row.
    ///
    /// `delete` runs while this mutex is held, so a commit cannot publish the
    /// same path between the check and the delete. A missing object is success
    /// and still drops the row. A path that is also in `data_files` is left
    /// untouched, and so is the object of a path with a `media` row: only its
    /// staged row goes (spec 03 §9). A staged row re-created at or after
    /// `before` (a `put_media` re-staged it since the listing) is left
    /// completely alone.
    pub(crate) async fn collect_staged_path<F, Fut>(
        &self,
        path: &str,
        hooks: &Hooks,
        before: i64,
        delete: F,
    ) -> Result<StagedRemoval, GcError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<u64, ObjectError>>,
    {
        let mut inner = self.inner.lock().await;
        let published: Option<i64> = sqlx::query_scalar("SELECT 1 FROM data_files WHERE path = ?")
            .bind(path)
            .fetch_optional(&mut inner.connection)
            .await
            .map_err(CatalogError::db)?;
        if published.is_some() {
            return Ok(StagedRemoval::Published);
        }
        if let Some((project, sha)) = media_path_parts(path) {
            let stored: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM media WHERE project_id = ? AND sha256 = ?")
                    .bind(project)
                    .bind(sha)
                    .fetch_optional(&mut inner.connection)
                    .await
                    .map_err(CatalogError::db)?;
            if stored.is_some() {
                sqlx::query("DELETE FROM staged_objects WHERE path = ?")
                    .bind(path)
                    .execute(&mut inner.connection)
                    .await
                    .map_err(CatalogError::db)?;
                return Ok(StagedRemoval::MediaStored);
            }
        }
        let staged: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM staged_objects WHERE path = ? AND created_at < ?")
                .bind(path)
                .bind(before)
                .fetch_optional(&mut inner.connection)
                .await
                .map_err(CatalogError::db)?;
        if staged.is_none() {
            return Ok(StagedRemoval::Gone);
        }
        hooks.pause(HookPoint::GcBeforeDelete);
        if hooks.fail(HookPoint::GcBeforeDelete).is_some() {
            return Err(GcError::Fault);
        }
        let bytes = delete().await?;
        sqlx::query("DELETE FROM staged_objects WHERE path = ?")
            .bind(path)
            .execute(&mut inner.connection)
            .await
            .map_err(CatalogError::db)?;
        Ok(StagedRemoval::Deleted { bytes })
    }

    /// Obsolete files whose `obsoleted_at` is at or before `cutoff`, in
    /// `(obsoleted_at, id)` order, starting after `after` (keyset paging).
    pub(crate) async fn obsolete_before(
        &self,
        cutoff: i64,
        after: Option<(i64, FileId)>,
        limit: u32,
    ) -> Result<Vec<ObsoleteFile>, GcError> {
        let mut inner = self.inner.lock().await;
        let rows = match after {
            Some((time, id)) => {
                sqlx::query_as::<_, (String, String, i64)>(
                    "SELECT id, path, obsoleted_at FROM data_files
                     WHERE state = 'obsolete' AND obsoleted_at <= ?
                       AND (obsoleted_at, id) > (?, ?)
                     ORDER BY obsoleted_at, id LIMIT ?",
                )
                .bind(cutoff)
                .bind(time)
                .bind(id.to_string())
                .bind(i64::from(limit))
                .fetch_all(&mut inner.connection)
                .await
            }
            None => {
                sqlx::query_as::<_, (String, String, i64)>(
                    "SELECT id, path, obsoleted_at FROM data_files
                     WHERE state = 'obsolete' AND obsoleted_at <= ?
                     ORDER BY obsoleted_at, id LIMIT ?",
                )
                .bind(cutoff)
                .bind(i64::from(limit))
                .fetch_all(&mut inner.connection)
                .await
            }
        };
        let rows = rows.map_err(|source| GcError::Catalog(CatalogError::db(source)))?;
        let mut files = Vec::with_capacity(rows.len());
        for (id, path, obsoleted_at) in rows {
            files.push(ObsoleteFile {
                id: parse_file_id(&id).map_err(GcError::Catalog)?,
                path,
                obsoleted_at,
            });
        }
        Ok(files)
    }

    /// Deletes one obsolete file's object, then its row, unless a reader still
    /// holds it.
    ///
    /// Holds the committer mutex throughout, like [`Self::collect_staged_path`].
    pub(crate) async fn collect_obsolete_file<F, Fut>(
        &self,
        file: &ObsoleteFile,
        hooks: &Hooks,
        delete: F,
    ) -> Result<ObsoleteRemoval, GcError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<u64, ObjectError>>,
    {
        let mut inner = self.inner.lock().await;
        if let Some(retired) = inner.retired.get(&file.id)
            && Arc::strong_count(retired) > 1
        {
            return Ok(ObsoleteRemoval::InUse);
        }
        let state: Option<String> = sqlx::query_scalar("SELECT state FROM data_files WHERE id = ?")
            .bind(file.id.to_string())
            .fetch_optional(&mut inner.connection)
            .await
            .map_err(|source| GcError::Catalog(CatalogError::db(source)))?;
        if state.as_deref() != Some("obsolete") {
            inner.retired.remove(&file.id);
            return Ok(ObsoleteRemoval::Gone);
        }
        hooks.pause(HookPoint::GcObsoleteBeforeDelete);
        if hooks.fail(HookPoint::GcObsoleteBeforeDelete).is_some() {
            return Err(GcError::Fault);
        }
        let bytes = delete().await?;
        hooks.pause(HookPoint::GcObsoleteAfterDelete);
        if hooks.fail(HookPoint::GcObsoleteAfterDelete).is_some() {
            // The object is gone and the row stays; the next collection
            // deletes the row (`delete_len` of a missing object is `Ok(0)`).
            return Err(GcError::Fault);
        }
        sqlx::query("DELETE FROM data_files WHERE id = ?")
            .bind(file.id.to_string())
            .execute(&mut inner.connection)
            .await
            .map_err(|source| GcError::Catalog(CatalogError::db(source)))?;
        inner.retired.remove(&file.id);
        Ok(ObsoleteRemoval::Deleted { bytes })
    }
}

/// What [`Committer::register_deletion`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Registered {
    /// Stored with `seq`; the tombstones include it.
    New {
        /// The stored `deletion_seq`.
        seq: u64,
    },
    /// The id was already stored. Nothing changed.
    Known,
}

/// One `obsolete` row of `data_files`, named by [`Committer::obsolete_before`].
pub(crate) struct ObsoleteFile {
    /// Primary key.
    pub(crate) id: FileId,
    /// Object key relative to the store root.
    pub(crate) path: String,
    /// When the commit that replaced the file marked it obsolete, ns.
    pub(crate) obsoleted_at: i64,
}

/// What [`Committer::collect_obsolete_file`] did with one obsolete file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ObsoleteRemoval {
    /// The object and the row are gone. `bytes` is zero when the object was
    /// already missing.
    Deleted {
        /// Object size before the delete.
        bytes: u64,
    },
    /// An older catalog or a plan still references the file. Nothing was deleted.
    InUse,
    /// The row was gone or no longer obsolete. Nothing was deleted.
    Gone,
}

/// What [`Committer::collect_staged_path`] did with one staged row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StagedRemoval {
    /// The object and the staged row are gone. `bytes` is zero when the object was already missing.
    Deleted {
        /// Object size before the delete.
        bytes: u64,
    },
    /// The path is in `data_files`. The object and the row stay.
    Published,
    /// The staged row was gone. Nothing was deleted.
    Gone,
    /// A staged media path whose blob is stored; only the staged row was removed.
    MediaStored,
}

fn flushed_of(state: TableState) -> [u64; 2] {
    let mut flushed = [0, 0];
    flushed[table_index(Table::Spans)] = state.spans;
    flushed[table_index(Table::Scores)] = state.scores;
    flushed
}

pub(crate) fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// `(project_id, sha256)` when `path` is a media path, `media/<project>/<64
/// hex>` (spec 03 §9). Anything else is not a media path.
fn media_path_parts(path: &str) -> Option<(&str, &str)> {
    let mut parts = path.split('/');
    if parts.next()? != "media" {
        return None;
    }
    let project = parts.next()?;
    let sha = parts.next()?;
    if parts.next().is_some()
        || sha.len() != 64
        || !sha.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return None;
    }
    Some((project, sha))
}

/// The body of [`Committer::commit`] up to and including the SQLite commit
/// and the in-memory updates, with `extra`'s SQL run after `write_edit` and
/// before the `CatalogCommit` hook. Does not publish; the caller holds the
/// mutex and publishes once everything worked.
async fn commit_inner(
    inner: &mut CommitterInner,
    edit: &CatalogEdit,
    extra: &Extra,
) -> Result<Arc<Catalog>, CommitError> {
    // The transaction borrows the connection, so copy these out first.
    let current = Arc::clone(&inner.catalog);
    // Production hooks are `Copy`. Tests share one value and have to clone it.
    #[allow(clippy::clone_on_copy)]
    let hooks = inner.hooks.clone();
    let mut transaction = inner.connection.begin().await.map_err(CommitError::db)?;
    if let Err(error) = write_edit(transaction.as_mut(), edit).await {
        let _ = transaction.rollback().await;
        return Err(error);
    }
    if let Err(error) = write_extra(transaction.as_mut(), extra).await {
        let _ = transaction.rollback().await;
        return Err(error);
    }
    let (next, removed) = match current.apply(edit) {
        Ok((catalog, removed)) => (Arc::new(catalog), removed),
        Err(error) => {
            let _ = transaction.rollback().await;
            return Err(error);
        }
    };
    hooks.pause(HookPoint::CatalogCommit);
    if let Some(source) = hooks.fail(HookPoint::CatalogCommit) {
        let _ = transaction.rollback().await;
        return Err(CommitError::Db {
            source: sqlx::Error::Io(source),
        });
    }
    transaction.commit().await.map_err(CommitError::db)?;
    for file in removed {
        inner.retired.insert(file.id, file);
    }
    if let Some((table, sequence)) = edit.flushed_seq {
        inner.flushed[table_index(table)] = sequence;
    }
    inner.catalog = Arc::clone(&next);
    Ok(next)
}

/// The SQL of [`Extra`], inside the edit's transaction.
async fn write_extra(connection: &mut SqliteConnection, extra: &Extra) -> Result<(), CommitError> {
    match extra {
        Extra::None => Ok(()),
        Extra::Retention { project, keep_from } => {
            sqlx::query(
                "INSERT OR REPLACE INTO retention_watermarks (project_id, keep_from)
                 VALUES (?, ?)",
            )
            .bind(project.to_string())
            .bind(keep_from.format("%Y-%m-%d").to_string())
            .execute(&mut *connection)
            .await
            .map_err(CommitError::db)?;
            Ok(())
        }
        Extra::Drop { project, now } => {
            sqlx::query(
                "INSERT INTO projects (project_id, state, dropped_at)
                 VALUES (?, 'dropping', ?)",
            )
            .bind(project.to_string())
            .bind(now)
            .execute(&mut *connection)
            .await
            .map_err(CommitError::db)?;
            sqlx::query("DELETE FROM retention_watermarks WHERE project_id = ?")
                .bind(project.to_string())
                .execute(&mut *connection)
                .await
                .map_err(CommitError::db)?;
            sqlx::query(
                "UPDATE deletions SET status = 'applied', applied_at = ?
                 WHERE project_id = ? AND status = 'pending'",
            )
            .bind(now)
            .bind(project.to_string())
            .execute(&mut *connection)
            .await
            .map_err(CommitError::db)?;
            // An applied deletion keeps none of the ids of the person (spec 03
            // §7): erase the scope rows of every deletion of the project, as
            // `mark_applied` does.
            for delete in [
                "DELETE FROM deletion_traces
                 WHERE deletion_id IN (SELECT id FROM deletions WHERE project_id = ?)",
                "DELETE FROM deletion_users
                 WHERE deletion_id IN (SELECT id FROM deletions WHERE project_id = ?)",
                "DELETE FROM deletion_sessions
                 WHERE deletion_id IN (SELECT id FROM deletions WHERE project_id = ?)",
            ] {
                sqlx::query(delete)
                    .bind(project.to_string())
                    .execute(&mut *connection)
                    .await
                    .map_err(CommitError::db)?;
            }
            Ok(())
        }
    }
}

async fn write_edit(
    connection: &mut SqliteConnection,
    edit: &CatalogEdit,
) -> Result<(), CommitError> {
    for file in &edit.add {
        let staged: Option<i64> = sqlx::query_scalar("SELECT 1 FROM staged_objects WHERE path = ?")
            .bind(&file.path)
            .fetch_optional(&mut *connection)
            .await
            .map_err(CommitError::db)?;
        if staged.is_none() {
            return Err(CommitError::conflict("path is not staged"));
        }
    }
    // An added file may not be of a dropped project (spec 03 §7): a flush or
    // compaction prepared before the drop fails here and retries.
    for file in &edit.add {
        let dropped: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM projects WHERE project_id = ?")
                .bind(file.project.to_string())
                .fetch_optional(&mut *connection)
                .await
                .map_err(CommitError::db)?;
        if dropped.is_some() {
            return Err(CommitError::conflict("project is dropped"));
        }
    }
    // An added file may not be of a day the project's retention watermark has
    // dropped (spec 03 §8): a flush or compaction prepared before the
    // retention fails here and retries. Both columns are `YYYY-MM-DD`, which
    // compares the same as text as it does as a date.
    for file in &edit.add {
        let keep_from: Option<String> =
            sqlx::query_scalar("SELECT keep_from FROM retention_watermarks WHERE project_id = ?")
                .bind(file.project.to_string())
                .fetch_optional(&mut *connection)
                .await
                .map_err(CommitError::db)?;
        if let Some(keep_from) = keep_from
            && file.day.format("%Y-%m-%d").to_string() < keep_from
        {
            return Err(CommitError::conflict(
                "file day is before the retention watermark",
            ));
        }
    }
    for (id, state) in &edit.remove {
        let current: Option<String> =
            sqlx::query_scalar("SELECT state FROM data_files WHERE id = ?")
                .bind(id.to_string())
                .fetch_optional(&mut *connection)
                .await
                .map_err(CommitError::db)?;
        if current.as_deref() != Some(state.as_str()) {
            return Err(CommitError::conflict("file is not in the expected state"));
        }
    }
    let removed_ids: HashSet<&FileId> = edit.remove.iter().map(|(id, _)| id).collect();
    for (id, _) in &edit.raise {
        if removed_ids.contains(id) {
            return Err(CommitError::conflict("raised file is also removed"));
        }
    }
    for (id, sequence) in &edit.raise {
        let current: Option<(String, i64)> =
            sqlx::query_as("SELECT state, deletions_applied_seq FROM data_files WHERE id = ?")
                .bind(id.to_string())
                .fetch_optional(&mut *connection)
                .await
                .map_err(CommitError::db)?;
        let Some((state, stored)) = current else {
            return Err(CommitError::conflict("raised file is not active"));
        };
        if state != "active" {
            return Err(CommitError::conflict("raised file is not active"));
        }
        let stored = u64::try_from(stored).map_err(|_| {
            CommitError::conflict("deletion sequence is not greater than the stored value")
        })?;
        if stored >= *sequence {
            return Err(CommitError::conflict(
                "deletion sequence is not greater than the stored value",
            ));
        }
        let stored_sequence = i64::try_from(*sequence)
            .map_err(|_| CommitError::conflict("integer does not fit the catalog"))?;
        sqlx::query("UPDATE data_files SET deletions_applied_seq = ? WHERE id = ?")
            .bind(stored_sequence)
            .bind(id.to_string())
            .execute(&mut *connection)
            .await
            .map_err(CommitError::db)?;
    }
    if let Some((table, sequence)) = edit.flushed_seq {
        let stored: Option<i64> =
            sqlx::query_scalar("SELECT flushed_seq FROM table_state WHERE table_name = ?")
                .bind(table.name())
                .fetch_optional(&mut *connection)
                .await
                .map_err(CommitError::db)?;
        let stored = match stored {
            None => 0,
            Some(value) => u64::try_from(value).map_err(|_| {
                CommitError::conflict("flushed sequence is not greater than the stored value")
            })?,
        };
        if sequence <= stored {
            return Err(CommitError::conflict(
                "flushed sequence is not greater than the stored value",
            ));
        }
    }
    for (traces, spans) in &edit.sources {
        let traces_added = edit
            .add
            .iter()
            .any(|file| file.id == *traces && file.table == FileTable::Traces);
        if !traces_added {
            return Err(CommitError::conflict(
                "source traces file is not added as a traces file",
            ));
        }
        let spans_added = edit
            .add
            .iter()
            .any(|file| file.id == *spans && file.table == FileTable::Spans);
        if spans_added {
            continue;
        }
        let current: Option<(String, String)> =
            sqlx::query_as("SELECT table_name, state FROM data_files WHERE id = ?")
                .bind(spans.to_string())
                .fetch_optional(&mut *connection)
                .await
                .map_err(CommitError::db)?;
        match current {
            Some((table, state)) if table == "spans" && state == "active" => {}
            _ => {
                return Err(CommitError::conflict(
                    "source spans file is not an active spans file",
                ));
            }
        }
    }
    // A removed file leaves with its dependants (spec 03 §5): a `spans` file
    // with the active `traces` files derived from it, a `traces` file with all
    // of its sources.
    let removed: HashSet<String> = edit.remove.iter().map(|(id, _)| id.to_string()).collect();
    for (id, _) in &edit.remove {
        let table: Option<String> =
            sqlx::query_scalar("SELECT table_name FROM data_files WHERE id = ?")
                .bind(id.to_string())
                .fetch_optional(&mut *connection)
                .await
                .map_err(CommitError::db)?;
        match table.as_deref() {
            Some("spans") => {
                let traces: Vec<String> = sqlx::query_scalar(
                    "SELECT fs.traces_file_id FROM file_sources AS fs
                     JOIN data_files AS t ON t.id = fs.traces_file_id
                     WHERE fs.spans_file_id = ? AND t.state = 'active'",
                )
                .bind(id.to_string())
                .fetch_all(&mut *connection)
                .await
                .map_err(CommitError::db)?;
                if traces.iter().any(|traces| !removed.contains(traces)) {
                    return Err(CommitError::conflict(
                        "a removed spans file keeps its traces file",
                    ));
                }
            }
            Some("traces") => {
                let spans: Vec<String> = sqlx::query_scalar(
                    "SELECT spans_file_id FROM file_sources WHERE traces_file_id = ?",
                )
                .bind(id.to_string())
                .fetch_all(&mut *connection)
                .await
                .map_err(CommitError::db)?;
                if spans.iter().any(|spans| !removed.contains(spans)) {
                    return Err(CommitError::conflict(
                        "a removed traces file keeps a source",
                    ));
                }
            }
            _ => {}
        }
    }

    let created_at = now_ns();
    for file in &edit.add {
        insert_file(connection, file, created_at).await?;
        sqlx::query("DELETE FROM staged_objects WHERE path = ?")
            .bind(&file.path)
            .execute(&mut *connection)
            .await
            .map_err(CommitError::db)?;
    }
    for (traces, spans) in &edit.sources {
        sqlx::query("INSERT INTO file_sources (traces_file_id, spans_file_id) VALUES (?, ?)")
            .bind(traces.to_string())
            .bind(spans.to_string())
            .execute(&mut *connection)
            .await
            .map_err(CommitError::db)?;
    }
    for (id, _) in &edit.remove {
        sqlx::query("UPDATE data_files SET state = 'obsolete', obsoleted_at = ? WHERE id = ?")
            .bind(created_at)
            .bind(id.to_string())
            .execute(&mut *connection)
            .await
            .map_err(CommitError::db)?;
    }
    if let Some((table, sequence)) = edit.flushed_seq {
        let sequence = i64::try_from(sequence)
            .map_err(|_| CommitError::conflict("integer does not fit the catalog"))?;
        sqlx::query(
            "INSERT INTO table_state (table_name, flushed_seq) VALUES (?, ?)
             ON CONFLICT(table_name) DO UPDATE SET flushed_seq = excluded.flushed_seq",
        )
        .bind(table.name())
        .bind(sequence)
        .execute(&mut *connection)
        .await
        .map_err(CommitError::db)?;
    }
    Ok(())
}

async fn insert_file(
    connection: &mut SqliteConnection,
    file: &FileMeta,
    created_at: i64,
) -> Result<(), CommitError> {
    if file.level > 1 {
        return Err(CommitError::conflict("invalid level"));
    }
    sqlx::query(
        "INSERT INTO data_files (
            id, table_name, project_id, day, level, path, size_bytes, footer_size,
            row_count, min_time, max_time, min_ingest_time, max_ingest_time,
            fully_merged, deletions_applied_seq, state, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'active', ?)",
    )
    .bind(file.id.to_string())
    .bind(file.table.as_str())
    .bind(file.project.to_string())
    .bind(file.day.format("%Y-%m-%d").to_string())
    .bind(i64::from(file.level))
    .bind(&file.path)
    .bind(fit_i64(file.size)?)
    .bind(fit_i64(file.footer_size)?)
    .bind(fit_i64(file.rows)?)
    .bind(file.min_time)
    .bind(file.max_time)
    .bind(file.min_ingest_time)
    .bind(file.max_ingest_time)
    .bind(i64::from(file.fully_merged))
    .bind(fit_i64(file.deletions_applied_seq)?)
    .bind(created_at)
    .execute(&mut *connection)
    .await
    .map_err(CommitError::db)?;
    Ok(())
}

fn fit_i64(value: u64) -> Result<i64, CommitError> {
    i64::try_from(value).map_err(|_| CommitError::conflict("integer does not fit the catalog"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex, PoisonError};

    use chrono::NaiveDate;
    use llmobs_core::ids::{ProjectId, TraceId};
    use sqlx::Row;

    use super::{load, open_catalog_db, *};
    use crate::error::CatalogError;

    fn project(which: &str) -> ProjectId {
        which.parse().unwrap()
    }

    fn day(value: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2024, 6, value).unwrap()
    }

    fn file(
        table: FileTable,
        project: ProjectId,
        day: NaiveDate,
        min_time: i64,
        path: &str,
    ) -> FileMeta {
        FileMeta {
            id: FileId::new_v7(),
            table,
            project,
            day,
            level: if table == FileTable::Scores { 1 } else { 0 },
            path: path.to_owned(),
            size: 128,
            footer_size: 16,
            rows: 3,
            min_time,
            max_time: min_time + 10,
            min_ingest_time: min_time + 1,
            max_ingest_time: min_time + 11,
            fully_merged: table == FileTable::Scores,
            deletions_applied_seq: 0,
        }
    }

    async fn opened() -> (tempfile::TempDir, Committer) {
        let dir = tempfile::tempdir().unwrap();
        let committer = Committer::open(dir.path(), Hooks::default()).await.unwrap();
        (dir, committer)
    }

    fn ids(catalog: &Catalog) -> BTreeSet<FileId> {
        catalog
            .tables
            .iter()
            .flat_map(|table| table.values())
            .flat_map(|days| days.values())
            .flat_map(|files| files.iter().map(|file| file.id))
            .collect()
    }

    #[tokio::test]
    async fn open_rejects_a_catalog_without_a_traces_file() {
        // M3.3 wires the spec 03 §3 check into open. An M2-style catalog with
        // spans files but no derived traces files must fail to open.
        let dir = tempfile::tempdir().unwrap();
        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        sqlx::query(
            "INSERT INTO data_files (
                id, table_name, project_id, day, level, path, size_bytes, footer_size,
                row_count, min_time, max_time, min_ingest_time, max_ingest_time,
                fully_merged, created_at
             ) VALUES (?, 'spans', ?, '2024-06-01', 0, 'spans/one', 1, 1, 1, 0, 0, 0, 0, 0, 1)",
        )
        .bind("018f6b4e-7c1a-7c1a-8c1a-000000000091")
        .bind("018f6b4e-7c1a-7c1a-8c1a-000000000001")
        .execute(&mut connection)
        .await
        .unwrap();
        drop(connection);
        let error = Committer::open(dir.path(), Hooks::default())
            .await
            .unwrap_err();
        assert!(
            matches!(error, CatalogError::SourcesInvariant { .. }),
            "{error}"
        );
    }

    #[tokio::test]
    async fn open_sets_pragmas_and_migrates_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        assert!(dir.path().join("catalog.db").is_file());

        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(journal, "wal");
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(synchronous, 2);
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(foreign_keys, 1);
        let busy: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(busy, 5000);

        let mut again = open_catalog_db(dir.path()).await.unwrap();
        let migrations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&mut again)
            .await
            .unwrap();
        assert_eq!(migrations, 6);
        let indexes: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master
             WHERE type = 'index' AND name LIKE 'data_files_%'
             ORDER BY name",
        )
        .fetch_all(&mut again)
        .await
        .unwrap();
        assert_eq!(
            indexes,
            vec!["data_files_gc".to_owned(), "data_files_lookup".to_owned()]
        );
        let deletions_applied: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('data_files')
             WHERE name = 'deletions_applied_seq'",
        )
        .fetch_one(&mut again)
        .await
        .unwrap();
        assert_eq!(deletions_applied, 1);
        let _ = connection;
    }

    #[tokio::test]
    async fn stage_and_commit_reload_the_same_catalog() {
        let (dir, committer) = opened().await;
        let spans_project = project("018f6b4e-7c1a-7c1a-8c1a-000000000001");
        let scores_project = project("018f6b4e-7c1a-7c1a-8c1a-000000000002");
        let later = file(FileTable::Spans, spans_project, day(2), 20, "spans/later");
        let earlier = file(FileTable::Spans, spans_project, day(2), 10, "spans/earlier");
        let scores = file(FileTable::Scores, scores_project, day(3), 5, "scores/one");
        let edit = CatalogEdit {
            add: vec![later, earlier, scores],
            remove: Vec::new(),
            flushed_seq: Some((Table::Spans, 4)),
            sources: Vec::new(),
            raise: Vec::new(),
        };
        committer
            .stage(
                &edit
                    .add
                    .iter()
                    .map(|file| file.path.clone())
                    .collect::<Vec<_>>(),
            )
            .await
            .unwrap();
        let published = committer.commit(&edit, |_| {}).await.unwrap();
        let times: Vec<_> = published
            .files(FileTable::Spans, spans_project, day(2)..=day(2))
            .map(|file| file.min_time)
            .collect();
        assert_eq!(times, vec![10, 20]);
        assert_eq!(
            published
                .files(FileTable::Scores, scores_project, day(3)..=day(3))
                .count(),
            1
        );

        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        let (loaded, state) = load(&mut connection).await.unwrap();
        assert_eq!(loaded, *published);
        assert_eq!(state.spans, 4);
        assert_eq!(state.scores, 0);
        assert!(Arc::ptr_eq(&published, &committer.catalog().await));
    }

    #[tokio::test]
    async fn an_unchanged_project_keeps_its_map() {
        let (_dir, committer) = opened().await;
        let left = project("018f6b4e-7c1a-7c1a-8c1a-000000000011");
        let right = project("018f6b4e-7c1a-7c1a-8c1a-000000000012");
        let first = file(FileTable::Spans, left, day(1), 1, "left");
        let second = file(FileTable::Spans, right, day(1), 1, "right");
        for edit in [
            CatalogEdit {
                add: vec![first],
                remove: Vec::new(),
                flushed_seq: None,
                sources: Vec::new(),
                raise: Vec::new(),
            },
            CatalogEdit {
                add: vec![second],
                remove: Vec::new(),
                flushed_seq: None,
                sources: Vec::new(),
                raise: Vec::new(),
            },
        ] {
            committer.stage(&[edit.add[0].path.clone()]).await.unwrap();
            committer.commit(&edit, |_| {}).await.unwrap();
        }
        let before = committer.catalog().await;
        let left_map = Arc::clone(before.tables[0].get(&left).unwrap());
        let extra = file(FileTable::Spans, right, day(2), 2, "right-again");
        let edit = CatalogEdit {
            add: vec![extra],
            remove: Vec::new(),
            flushed_seq: None,
            sources: Vec::new(),
            raise: Vec::new(),
        };
        committer.stage(&["right-again".to_owned()]).await.unwrap();
        let after = committer.commit(&edit, |_| {}).await.unwrap();
        assert!(Arc::ptr_eq(&left_map, after.tables[0].get(&left).unwrap()));
    }

    #[tokio::test]
    async fn remove_marks_the_file_obsolete() {
        let (dir, committer) = opened().await;
        let spans = project("018f6b4e-7c1a-7c1a-8c1a-000000000021");
        let meta = file(FileTable::Spans, spans, day(1), 1, "doomed");
        let id = meta.id;
        let edit = CatalogEdit {
            add: vec![meta],
            remove: Vec::new(),
            flushed_seq: None,
            sources: Vec::new(),
            raise: Vec::new(),
        };
        committer.stage(&["doomed".to_owned()]).await.unwrap();
        committer.commit(&edit, |_| {}).await.unwrap();
        let remove = CatalogEdit {
            add: Vec::new(),
            remove: vec![(id, FileState::Active)],
            flushed_seq: None,
            sources: Vec::new(),
            raise: Vec::new(),
        };
        let published = committer.commit(&remove, |_| {}).await.unwrap();
        assert!(
            published
                .files(FileTable::Spans, spans, day(1)..=day(1))
                .next()
                .is_none()
        );

        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        let (loaded, _) = load(&mut connection).await.unwrap();
        assert_eq!(loaded, *published);
        let row = sqlx::query("SELECT state, obsoleted_at FROM data_files WHERE id = ?")
            .bind(id.to_string())
            .fetch_one(&mut connection)
            .await
            .unwrap();
        let state: String = row.try_get("state").unwrap();
        let obsoleted_at: Option<i64> = row.try_get("obsoleted_at").unwrap();
        assert_eq!(state, "obsolete");
        assert!(obsoleted_at.is_some());
    }

    #[tokio::test]
    async fn conflicts_roll_back_and_leave_memory_unchanged() {
        let (dir, committer) = opened().await;
        let spans = project("018f6b4e-7c1a-7c1a-8c1a-000000000031");
        let kept = file(FileTable::Spans, spans, day(1), 1, "kept");
        let id = kept.id;
        committer.stage(&["kept".to_owned()]).await.unwrap();
        let added = CatalogEdit {
            add: vec![kept],
            remove: Vec::new(),
            flushed_seq: Some((Table::Spans, 3)),
            sources: Vec::new(),
            raise: Vec::new(),
        };
        committer.commit(&added, |_| {}).await.unwrap();
        let before = committer.catalog().await;

        let missing = CatalogEdit {
            add: vec![file(FileTable::Spans, spans, day(1), 2, "missing")],
            remove: Vec::new(),
            flushed_seq: Some((Table::Spans, 4)),
            sources: Vec::new(),
            raise: Vec::new(),
        };
        let error = committer.commit(&missing, |_| {}).await.unwrap_err();
        assert!(matches!(
            error,
            CommitError::Conflict {
                reason: "path is not staged"
            }
        ));

        let wrong = CatalogEdit {
            add: Vec::new(),
            remove: vec![(id, FileState::Obsolete)],
            flushed_seq: Some((Table::Spans, 4)),
            sources: Vec::new(),
            raise: Vec::new(),
        };
        let error = committer.commit(&wrong, |_| {}).await.unwrap_err();
        assert!(matches!(
            error,
            CommitError::Conflict {
                reason: "file is not in the expected state"
            }
        ));
        assert_eq!(FileState::Quarantined.as_str(), "quarantined");

        let behind = CatalogEdit {
            add: Vec::new(),
            remove: Vec::new(),
            flushed_seq: Some((Table::Spans, 3)),
            sources: Vec::new(),
            raise: Vec::new(),
        };
        let error = committer.commit(&behind, |_| {}).await.unwrap_err();
        assert!(matches!(
            error,
            CommitError::Conflict {
                reason: "flushed sequence is not greater than the stored value"
            }
        ));

        assert!(Arc::ptr_eq(&before, &committer.catalog().await));
        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        let (loaded, state) = load(&mut connection).await.unwrap();
        assert_eq!(loaded, *before);
        assert_eq!(state.spans, 3);
        let staged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM staged_objects")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(staged, 0);
        let active: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM data_files WHERE state = 'active'")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(active, 1);
    }

    #[tokio::test]
    async fn an_injected_commit_fault_leaves_the_database_and_memory_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = Hooks::default();
        let committer = Committer::open(dir.path(), hooks.clone()).await.unwrap();
        let spans = project("018f6b4e-7c1a-7c1a-8c1a-000000000041");
        let meta = file(FileTable::Spans, spans, day(1), 1, "faulty");
        committer.stage(&["faulty".to_owned()]).await.unwrap();
        hooks.arm_fail(HookPoint::CatalogCommit);
        let edit = CatalogEdit {
            add: vec![meta],
            remove: Vec::new(),
            flushed_seq: Some((Table::Spans, 1)),
            sources: Vec::new(),
            raise: Vec::new(),
        };
        let before = committer.catalog().await;
        let error = committer.commit(&edit, |_| {}).await.unwrap_err();
        assert!(matches!(error, CommitError::Db { .. }));
        assert!(Arc::ptr_eq(&before, &committer.catalog().await));

        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        let (loaded, state) = load(&mut connection).await.unwrap();
        assert_eq!(loaded, *before);
        assert_eq!(state.spans, 0);
        let staged: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM staged_objects WHERE path = 'faulty'")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(staged, 1);
    }

    #[tokio::test]
    async fn corrupt_rows_fail_load() {
        let project = "018f6b4e-7c1a-7c1a-8c1a-000000000051";
        let id = "018f6b4e-7c1a-7c1a-8c1a-000000000052";
        let cases = [
            ("nope", id, project, "2024-06-01", "unknown table_name"),
            ("spans", id, project, "not-a-date", "invalid day"),
            (
                "spans",
                "not-a-uuid",
                project,
                "2024-06-01",
                "invalid file id",
            ),
            (
                "spans",
                id,
                "not-a-uuid",
                "2024-06-01",
                "invalid project_id",
            ),
        ];
        for (table, id, project, day, what) in cases {
            let dir = tempfile::tempdir().unwrap();
            let mut connection = open_catalog_db(dir.path()).await.unwrap();
            sqlx::query(
                "INSERT INTO data_files (
                    id, table_name, project_id, day, level, path, size_bytes, footer_size,
                    row_count, min_time, max_time, min_ingest_time, max_ingest_time,
                    fully_merged, created_at
                ) VALUES (?, ?, ?, ?, 0, 'p', 1, 1, 1, 0, 0, 0, 0, 0, 1)",
            )
            .bind(id)
            .bind(table)
            .bind(project)
            .bind(day)
            .execute(&mut connection)
            .await
            .unwrap();
            let error = load(&mut connection).await.unwrap_err();
            assert!(
                matches!(error, CatalogError::Corrupt { what: got } if got == what),
                "{error}"
            );
            assert!(!error.to_string().contains(id));
            assert!(!error.to_string().contains(day));
            assert!(!error.to_string().contains(project));
        }
    }

    #[tokio::test]
    async fn concurrent_disjoint_commits_only_grow() {
        let (dir, committer) = opened().await;
        let committer = Arc::new(committer);
        let published = Arc::new(Mutex::new(Vec::new()));
        let mut tasks = Vec::new();
        for index in 0..16 {
            let committer = Arc::clone(&committer);
            let published = Arc::clone(&published);
            tasks.push(tokio::spawn(async move {
                let path = format!("objects/{index}");
                let meta = file(
                    FileTable::Spans,
                    project("018f6b4e-7c1a-7c1a-8c1a-000000000061"),
                    day(1 + index),
                    i64::from(index),
                    &path,
                );
                let id = meta.id;
                committer.stage(&[path]).await.unwrap();
                let edit = CatalogEdit {
                    add: vec![meta],
                    remove: Vec::new(),
                    flushed_seq: None,
                    sources: Vec::new(),
                    raise: Vec::new(),
                };
                committer
                    .commit(&edit, {
                        let published = Arc::clone(&published);
                        move |catalog| {
                            published
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .push(Arc::clone(catalog));
                        }
                    })
                    .await
                    .unwrap();
                id
            }));
        }
        let mut expected = BTreeSet::new();
        for task in tasks {
            expected.insert(task.await.unwrap());
        }
        let published = published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert_eq!(published.len(), 16);
        let mut seen = BTreeSet::new();
        for catalog in published.iter() {
            let current = ids(catalog);
            assert!(seen.is_subset(&current));
            assert!(current.len() > seen.len());
            seen = current;
        }
        assert_eq!(seen, expected);
        let final_catalog = committer.catalog().await;
        assert_eq!(ids(&final_catalog), expected);

        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        let (loaded, _) = load(&mut connection).await.unwrap();
        assert_eq!(ids(&loaded), expected);
    }

    async fn sources(connection: &mut SqliteConnection) -> Vec<(String, String)> {
        sqlx::query_as("SELECT traces_file_id, spans_file_id FROM file_sources")
            .fetch_all(&mut *connection)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_commit_with_sources_inserts_its_rows() {
        let (dir, committer) = opened().await;
        let project = project("018f6b4e-7c1a-7c1a-8c1a-000000000071");
        let spans = file(FileTable::Spans, project, day(1), 1, "spans/sourced");
        let traces = file(FileTable::Traces, project, day(1), 1, "traces/sourced");
        let spans_id = spans.id;
        let traces_id = traces.id;
        let edit = CatalogEdit {
            add: vec![spans, traces],
            remove: Vec::new(),
            flushed_seq: None,
            sources: vec![(traces_id, spans_id)],
            raise: Vec::new(),
        };
        committer
            .stage(&["spans/sourced".to_owned(), "traces/sourced".to_owned()])
            .await
            .unwrap();
        committer.commit(&edit, |_| {}).await.unwrap();

        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        assert_eq!(
            sources(&mut connection).await,
            vec![(traces_id.to_string(), spans_id.to_string())]
        );
    }

    #[tokio::test]
    async fn a_source_can_point_at_an_already_active_spans_file() {
        let (dir, committer) = opened().await;
        let project = project("018f6b4e-7c1a-7c1a-8c1a-000000000072");
        let spans = file(FileTable::Spans, project, day(1), 1, "spans/active");
        let spans_id = spans.id;
        let first = CatalogEdit {
            add: vec![spans],
            remove: Vec::new(),
            flushed_seq: None,
            sources: Vec::new(),
            raise: Vec::new(),
        };
        committer.stage(&["spans/active".to_owned()]).await.unwrap();
        committer.commit(&first, |_| {}).await.unwrap();

        let traces = file(FileTable::Traces, project, day(1), 1, "traces/derived");
        let traces_id = traces.id;
        let second = CatalogEdit {
            add: vec![traces],
            remove: Vec::new(),
            flushed_seq: None,
            sources: vec![(traces_id, spans_id)],
            raise: Vec::new(),
        };
        committer
            .stage(&["traces/derived".to_owned()])
            .await
            .unwrap();
        committer.commit(&second, |_| {}).await.unwrap();

        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        assert_eq!(
            sources(&mut connection).await,
            vec![(traces_id.to_string(), spans_id.to_string())]
        );
    }

    /// Two active `(spans, traces)` pairs of one project and day:
    /// `(S1, T1 (←S1), S2, T2 (←S2))`.
    async fn two_pairs() -> (tempfile::TempDir, Committer, [FileId; 4]) {
        let (dir, committer) = opened().await;
        let spans_project = project("018f6b4e-7c1a-7c1a-8c1a-000000000074");
        let mut staged = Vec::new();
        let mut add = Vec::new();
        let mut sources = Vec::new();
        let mut ids = [FileId::new_v7(); 4];
        for index in 0..2 {
            let spans = file(
                FileTable::Spans,
                spans_project,
                day(1),
                i64::try_from(index).unwrap_or(0) + 1,
                &format!("spans/input-{index}"),
            );
            let traces = file(
                FileTable::Traces,
                spans_project,
                day(1),
                i64::try_from(index).unwrap_or(0) + 1,
                &format!("traces/input-{index}"),
            );
            ids[index * 2] = spans.id;
            ids[index * 2 + 1] = traces.id;
            sources.push((traces.id, spans.id));
            staged.push(spans.path.clone());
            staged.push(traces.path.clone());
            add.push(spans);
            add.push(traces);
        }
        committer.stage(&staged).await.unwrap();
        committer
            .commit(
                &CatalogEdit {
                    add,
                    remove: Vec::new(),
                    flushed_seq: None,
                    sources,
                    raise: Vec::new(),
                },
                |_| {},
            )
            .await
            .unwrap();
        (dir, committer, ids)
    }

    #[tokio::test]
    async fn a_compaction_shaped_edit_commits() {
        let (dir, committer, ids) = two_pairs().await;
        let spans_project = project("018f6b4e-7c1a-7c1a-8c1a-000000000074");
        let spans_out = file(FileTable::Spans, spans_project, day(1), 3, "spans/output");
        let traces_out = file(FileTable::Traces, spans_project, day(1), 3, "traces/output");
        let spans_id = spans_out.id;
        let traces_id = traces_out.id;
        committer
            .stage(&["spans/output".to_owned(), "traces/output".to_owned()])
            .await
            .unwrap();
        let edit = CatalogEdit {
            add: vec![spans_out, traces_out],
            remove: ids.iter().map(|id| (*id, FileState::Active)).collect(),
            flushed_seq: None,
            sources: vec![(traces_id, spans_id)],
            raise: Vec::new(),
        };
        committer.commit(&edit, |_| {}).await.unwrap();

        // The open-time spec 03 §3 check passes over the committed result.
        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        check_file_sources(&mut connection).await.unwrap();
        let active: Vec<(String, String)> = sqlx::query_as(
            "SELECT table_name, path FROM data_files WHERE state = 'active' ORDER BY path",
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        assert_eq!(
            active,
            vec![
                ("spans".to_owned(), "spans/output".to_owned()),
                ("traces".to_owned(), "traces/output".to_owned()),
            ]
        );
        let obsolete: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM data_files WHERE state = 'obsolete'")
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(obsolete, 4);
        // The input pairs' `file_sources` rows stay until GC deletes the rows.
        assert_eq!(sources(&mut connection).await.len(), 3);
    }

    #[tokio::test]
    async fn removing_a_spans_file_without_its_traces_file_is_a_conflict() {
        let (dir, committer, ids) = two_pairs().await;
        let [spans_one, traces_one, _spans_two, _traces_two] = ids;
        let before = committer.catalog().await;

        let edit = CatalogEdit {
            add: Vec::new(),
            remove: vec![(spans_one, FileState::Active)],
            flushed_seq: None,
            sources: Vec::new(),
            raise: Vec::new(),
        };
        let error = committer.commit(&edit, |_| {}).await.unwrap_err();
        assert!(
            matches!(
                error,
                CommitError::Conflict {
                    reason: "a removed spans file keeps its traces file"
                }
            ),
            "{error}"
        );

        // Nothing changed: rows and states are as before, and the in-memory
        // catalog is the same.
        assert!(Arc::ptr_eq(&before, &committer.catalog().await));
        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        let states: Vec<(String, String)> =
            sqlx::query_as("SELECT path, state FROM data_files ORDER BY path")
                .fetch_all(&mut connection)
                .await
                .unwrap();
        assert_eq!(states.len(), 4);
        assert!(states.iter().all(|(_, state)| state == "active"));
        assert_eq!(sources(&mut connection).await.len(), 2);
        let _ = traces_one;
    }

    #[tokio::test]
    async fn removing_a_traces_file_without_its_source_is_a_conflict() {
        let (_dir, committer, ids) = two_pairs().await;
        let [_spans_one, traces_one, _spans_two, _traces_two] = ids;
        let edit = CatalogEdit {
            add: Vec::new(),
            remove: vec![(traces_one, FileState::Active)],
            flushed_seq: None,
            sources: Vec::new(),
            raise: Vec::new(),
        };
        let error = committer.commit(&edit, |_| {}).await.unwrap_err();
        assert!(
            matches!(
                error,
                CommitError::Conflict {
                    reason: "a removed traces file keeps a source"
                }
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn sources_with_unknown_files_are_conflicts() {
        for (traces_in_add, reason) in [
            (true, "source spans file is not an active spans file"),
            (false, "source traces file is not added as a traces file"),
        ] {
            let (dir, committer) = opened().await;
            let project = project("018f6b4e-7c1a-7c1a-8c1a-000000000073");
            let traces = file(FileTable::Traces, project, day(1), 1, "traces/orphan");
            let traces_id = traces.id;
            let unknown = FileId::new_v7();
            let edit = CatalogEdit {
                add: if traces_in_add {
                    vec![traces]
                } else {
                    Vec::new()
                },
                remove: Vec::new(),
                flushed_seq: None,
                sources: vec![(traces_id, unknown)],
                raise: Vec::new(),
            };
            committer
                .stage(&["traces/orphan".to_owned()])
                .await
                .unwrap();
            let error = committer.commit(&edit, |_| {}).await.unwrap_err();
            assert!(
                matches!(error, CommitError::Conflict { reason: got } if got == reason),
                "{error}"
            );

            let mut connection = open_catalog_db(dir.path()).await.unwrap();
            assert!(sources(&mut connection).await.is_empty());
            let active: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM data_files WHERE state = 'active'")
                    .fetch_one(&mut connection)
                    .await
                    .unwrap();
            assert_eq!(active, 0);
        }
    }

    /// The `Arc<FileMeta>` of `id` in `catalog`, to compare pointers across a
    /// raise.
    fn file_arc(catalog: &Catalog, id: FileId) -> Arc<FileMeta> {
        catalog
            .tables
            .iter()
            .flat_map(|table| table.values())
            .flat_map(|days| days.values())
            .flatten()
            .find(|file| file.id == id)
            .map(Arc::clone)
            .unwrap()
    }

    fn trace_id(low: u8) -> TraceId {
        let mut bytes = [0_u8; 16];
        bytes[15] = low;
        TraceId::from_bytes(bytes).unwrap()
    }

    fn deletion(id: Uuid, project: ProjectId, scope: DeletionScope) -> Deletion {
        Deletion {
            id,
            project,
            cutoff: 1_000,
            scope,
        }
    }

    #[tokio::test]
    async fn deletions_applied_seq_round_trips_through_the_catalog() {
        let (dir, committer) = opened().await;
        let spans = project("018f6b4e-7c1a-7c1a-8c1a-000000000081");
        let mut applied = file(FileTable::Spans, spans, day(1), 1, "spans/applied");
        applied.deletions_applied_seq = 7;
        committer
            .stage(&["spans/applied".to_owned()])
            .await
            .unwrap();
        let edit = CatalogEdit {
            add: vec![applied],
            remove: Vec::new(),
            flushed_seq: None,
            sources: Vec::new(),
            raise: Vec::new(),
        };
        committer.commit(&edit, |_| {}).await.unwrap();

        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        let (loaded, _) = load(&mut connection).await.unwrap();
        let files = loaded.test_files(FileTable::Spans);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].deletions_applied_seq, 7);
    }

    #[tokio::test]
    async fn raising_an_active_file_updates_sqlite_and_the_catalog() {
        let (dir, committer) = opened().await;
        let spans = project("018f6b4e-7c1a-7c1a-8c1a-000000000082");
        let raised = file(FileTable::Spans, spans, day(1), 1, "spans/raised");
        let other = file(FileTable::Spans, spans, day(1), 2, "spans/other");
        let raised_id = raised.id;
        let other_id = other.id;
        let edit = CatalogEdit {
            add: vec![raised, other],
            remove: Vec::new(),
            flushed_seq: None,
            sources: Vec::new(),
            raise: Vec::new(),
        };
        committer
            .stage(&["spans/raised".to_owned(), "spans/other".to_owned()])
            .await
            .unwrap();
        committer.commit(&edit, |_| {}).await.unwrap();
        let before = committer.catalog().await;
        let raised_before = file_arc(&before, raised_id);
        let other_before = file_arc(&before, other_id);

        let raise = CatalogEdit {
            add: Vec::new(),
            remove: Vec::new(),
            flushed_seq: None,
            sources: Vec::new(),
            raise: vec![(raised_id, 3)],
        };
        let published = committer.commit(&raise, |_| {}).await.unwrap();
        let raised_after = file_arc(&published, raised_id);
        assert_eq!(raised_after.deletions_applied_seq, 3);
        assert!(!Arc::ptr_eq(&raised_before, &raised_after));
        assert!(Arc::ptr_eq(&other_before, &file_arc(&published, other_id)));

        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        let sequences: Vec<(String, i64)> =
            sqlx::query_as("SELECT path, deletions_applied_seq FROM data_files ORDER BY path")
                .fetch_all(&mut connection)
                .await
                .unwrap();
        assert_eq!(
            sequences,
            vec![
                ("spans/other".to_owned(), 0),
                ("spans/raised".to_owned(), 3),
            ]
        );
    }

    #[tokio::test]
    async fn raise_conflicts_leave_everything_unchanged() {
        let (dir, committer) = opened().await;
        let spans = project("018f6b4e-7c1a-7c1a-8c1a-000000000083");
        let raised = file(FileTable::Spans, spans, day(1), 1, "spans/raised");
        let doomed = file(FileTable::Spans, spans, day(1), 2, "spans/doomed");
        let raised_id = raised.id;
        let doomed_id = doomed.id;
        let edit = CatalogEdit {
            add: vec![raised, doomed],
            remove: Vec::new(),
            flushed_seq: None,
            sources: Vec::new(),
            raise: Vec::new(),
        };
        committer
            .stage(&["spans/raised".to_owned(), "spans/doomed".to_owned()])
            .await
            .unwrap();
        committer.commit(&edit, |_| {}).await.unwrap();
        let remove = CatalogEdit {
            add: Vec::new(),
            remove: vec![(doomed_id, FileState::Active)],
            flushed_seq: None,
            sources: Vec::new(),
            raise: Vec::new(),
        };
        committer.commit(&remove, |_| {}).await.unwrap();
        let before = committer.catalog().await;

        // A stored value of 0 and a raise to 0: not greater.
        let not_greater = CatalogEdit {
            add: Vec::new(),
            remove: Vec::new(),
            flushed_seq: None,
            sources: Vec::new(),
            raise: vec![(raised_id, 0)],
        };
        let error = committer.commit(&not_greater, |_| {}).await.unwrap_err();
        assert!(
            matches!(
                error,
                CommitError::Conflict {
                    reason: "deletion sequence is not greater than the stored value"
                }
            ),
            "{error}"
        );

        let obsolete = CatalogEdit {
            add: Vec::new(),
            remove: Vec::new(),
            flushed_seq: None,
            sources: Vec::new(),
            raise: vec![(doomed_id, 5)],
        };
        let error = committer.commit(&obsolete, |_| {}).await.unwrap_err();
        assert!(
            matches!(
                error,
                CommitError::Conflict {
                    reason: "raised file is not active"
                }
            ),
            "{error}"
        );

        let also_removed = CatalogEdit {
            add: Vec::new(),
            remove: vec![(raised_id, FileState::Active)],
            flushed_seq: None,
            sources: Vec::new(),
            raise: vec![(raised_id, 5)],
        };
        let error = committer.commit(&also_removed, |_| {}).await.unwrap_err();
        assert!(
            matches!(
                error,
                CommitError::Conflict {
                    reason: "raised file is also removed"
                }
            ),
            "{error}"
        );

        assert!(Arc::ptr_eq(&before, &committer.catalog().await));
        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        let states: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT path, state, deletions_applied_seq FROM data_files ORDER BY path",
        )
        .fetch_all(&mut connection)
        .await
        .unwrap();
        assert_eq!(
            states,
            vec![
                ("spans/doomed".to_owned(), "obsolete".to_owned(), 0),
                ("spans/raised".to_owned(), "active".to_owned(), 0),
            ]
        );
    }

    #[tokio::test]
    async fn registrations_take_the_next_seq_and_publish_the_tombstones() {
        let (dir, committer) = opened().await;
        let spans = project("018f6b4e-7c1a-7c1a-8c1a-000000000084");
        let first_id = Uuid::now_v7();
        let second_id = Uuid::now_v7();
        let mut published: Vec<Arc<Tombstones>> = Vec::new();
        let first = deletion(
            first_id,
            spans,
            DeletionScope::Traces {
                trace_ids: vec![trace_id(1)],
            },
        );
        let second = deletion(
            second_id,
            spans,
            DeletionScope::User {
                user_id: "user".to_owned(),
                trace_ids: vec![trace_id(2)],
                session_ids: vec!["session".to_owned()],
            },
        );
        let registered = committer
            .register_deletion(&first, 100, |tombstones| {
                published.push(Arc::clone(tombstones));
            })
            .await
            .unwrap();
        assert_eq!(registered, Registered::New { seq: 1 });
        let registered = committer
            .register_deletion(&second, 200, |tombstones| {
                published.push(Arc::clone(tombstones));
            })
            .await
            .unwrap();
        assert_eq!(registered, Registered::New { seq: 2 });

        assert_eq!(published.len(), 2);
        assert_eq!(published[0].known_seq(), 1);
        let after_second = &published[1];
        assert_eq!(after_second.known_seq(), 2);
        let pending = after_second.for_project(spans);
        let ids: Vec<_> = pending.iter().map(|deletion| deletion.id).collect();
        assert_eq!(ids, vec![first_id, second_id]);

        let registered = committer
            .register_deletion(&first, 300, |_| {
                unreachable!("a known id does not publish");
            })
            .await
            .unwrap();
        assert_eq!(registered, Registered::Known);
        assert_eq!(published.len(), 2);
        assert_eq!(committer.tombstones().await.known_seq(), 2);
        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM deletions")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(rows, 2);
    }

    #[tokio::test]
    async fn invalid_requests_write_nothing() {
        let (dir, committer) = opened().await;
        let spans = project("018f6b4e-7c1a-7c1a-8c1a-000000000085");
        let cases = [
            (
                Deletion {
                    id: Uuid::now_v7(),
                    project: spans,
                    cutoff: 0,
                    scope: DeletionScope::Traces {
                        trace_ids: vec![trace_id(1)],
                    },
                },
                "cutoff must be positive",
            ),
            (
                deletion(
                    Uuid::now_v7(),
                    spans,
                    DeletionScope::Traces { trace_ids: vec![] },
                ),
                "empty scope",
            ),
            (
                deletion(
                    Uuid::now_v7(),
                    spans,
                    DeletionScope::User {
                        user_id: String::new(),
                        trace_ids: vec![trace_id(1)],
                        session_ids: vec!["session".to_owned()],
                    },
                ),
                "empty user id",
            ),
        ];
        for (request, reason) in cases {
            let error = committer
                .register_deletion(&request, 100, |_| {
                    unreachable!("an invalid request does not publish");
                })
                .await
                .unwrap_err();
            assert!(
                matches!(error, DeletionError::Invalid { reason: got } if got == reason),
                "{error}"
            );
        }
        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM deletions")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(rows, 0);
        assert_eq!(committer.tombstones().await.known_seq(), 0);
    }

    #[tokio::test]
    async fn a_commit_fault_rolls_back_a_registration() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = Hooks::default();
        let committer = Committer::open(dir.path(), hooks.clone()).await.unwrap();
        let spans = project("018f6b4e-7c1a-7c1a-8c1a-000000000086");
        let first = deletion(
            Uuid::now_v7(),
            spans,
            DeletionScope::Traces {
                trace_ids: vec![trace_id(1)],
            },
        );
        committer
            .register_deletion(&first, 100, |_| {})
            .await
            .unwrap();
        let before = committer.tombstones().await;

        let second = deletion(
            Uuid::now_v7(),
            spans,
            DeletionScope::Traces {
                trace_ids: vec![trace_id(2)],
            },
        );
        hooks.arm_fail(HookPoint::CatalogCommit);
        let error = committer
            .register_deletion(&second, 200, |_| {
                unreachable!("a failed registration does not publish");
            })
            .await
            .unwrap_err();
        assert!(matches!(error, DeletionError::Catalog(_)), "{error}");
        let after = committer.tombstones().await;
        assert_eq!(*after, *before);

        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM deletions")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(rows, 1);

        // The rolled-back sequence number was not consumed.
        let retried = committer
            .register_deletion(&second, 300, |_| {})
            .await
            .unwrap();
        assert_eq!(retried, Registered::New { seq: 2 });
    }

    #[tokio::test]
    async fn mark_applied_erases_the_scope_and_reload_keeps_the_rest() {
        let (dir, committer) = opened().await;
        let spans = project("018f6b4e-7c1a-7c1a-8c1a-000000000087");
        let first_id = Uuid::now_v7();
        let second_id = Uuid::now_v7();
        let first = deletion(
            first_id,
            spans,
            DeletionScope::Traces {
                trace_ids: vec![trace_id(1), trace_id(2)],
            },
        );
        let second = deletion(
            second_id,
            spans,
            DeletionScope::User {
                user_id: "user".to_owned(),
                trace_ids: vec![trace_id(3)],
                session_ids: vec!["session-a".to_owned(), "session-b".to_owned()],
            },
        );
        committer
            .register_deletion(&first, 100, |_| {})
            .await
            .unwrap();
        committer
            .register_deletion(&second, 200, |_| {})
            .await
            .unwrap();
        assert!(
            committer
                .mark_applied(spans, first_id, 300, |_| {})
                .await
                .unwrap()
        );

        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        let first_traces: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM deletion_traces WHERE deletion_id = ?")
                .bind(first_id.to_string())
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(first_traces, 0);
        for (count_sql, expected) in [
            (
                "SELECT COUNT(*) FROM deletion_traces WHERE deletion_id = ?",
                1,
            ),
            (
                "SELECT COUNT(*) FROM deletion_users WHERE deletion_id = ?",
                1,
            ),
            (
                "SELECT COUNT(*) FROM deletion_sessions WHERE deletion_id = ?",
                2,
            ),
        ] {
            let count: i64 = sqlx::query_scalar(count_sql)
                .bind(second_id.to_string())
                .fetch_one(&mut connection)
                .await
                .unwrap();
            assert_eq!(count, expected, "{count_sql}");
        }
        drop(connection);

        let reopened = Committer::open(dir.path(), Hooks::default()).await.unwrap();
        let tombstones = reopened.tombstones().await;
        assert_eq!(tombstones.known_seq(), 2);
        let pending = tombstones.for_project(spans);
        assert_eq!(pending.len(), 1);
        let deletion = &pending[0];
        assert_eq!(deletion.id, second_id);
        assert_eq!(deletion.trace_ids, HashSet::from([*trace_id(3).as_bytes()]));
        assert_eq!(deletion.users, HashSet::from(["user".to_owned()]));
        assert_eq!(
            deletion.sessions,
            HashSet::from(["session-a".to_owned(), "session-b".to_owned()])
        );
        assert_eq!(deletion.cutoff, 1_000);
        assert_eq!(deletion.registered_at, 200);
    }

    #[tokio::test]
    async fn deletion_status_reports_pending_applied_and_unknown() {
        let (_dir, committer) = opened().await;
        let spans = project("018f6b4e-7c1a-7c1a-8c1a-000000000088");
        let id = Uuid::now_v7();
        assert_eq!(committer.deletion_status(id).await.unwrap(), None);
        let request = deletion(
            id,
            spans,
            DeletionScope::Traces {
                trace_ids: vec![trace_id(1)],
            },
        );
        committer
            .register_deletion(&request, 100, |_| {})
            .await
            .unwrap();
        assert_eq!(
            committer.deletion_status(id).await.unwrap(),
            Some(DeletionStatus::Pending)
        );
        assert!(
            committer
                .mark_applied(spans, id, 5_000, |_| {})
                .await
                .unwrap()
        );
        assert_eq!(
            committer.deletion_status(id).await.unwrap(),
            Some(DeletionStatus::Applied { applied_at: 5_000 })
        );
        // An applied id is no longer pending: nothing to do, no publish.
        assert!(
            !committer
                .mark_applied(spans, id, 6_000, |_| {})
                .await
                .unwrap()
        );
    }

    // M5.6 test 7: a put that re-staged a path since GC's listing is left
    // alone, because the race sits between the listing and this per-path check.
    #[tokio::test]
    async fn a_re_staged_media_path_is_left_alone() {
        let (dir, committer) = opened().await;
        let sha = "ab".repeat(32);
        let path = Objects::media_path(project("018f6b4e-7c1a-7c1a-8c1a-000000000089"), &sha);
        committer.stage_media(&path, 100).await.unwrap();
        // A second put re-staged the path after GC listed it with cut-off 500.
        committer.stage_media(&path, 1_000).await.unwrap();
        let hooks = Hooks::default();
        let mut deleted = 0_u32;
        let removal = committer
            .collect_staged_path(&path, &hooks, 500, || {
                deleted += 1;
                async { Ok(0_u64) }
            })
            .await
            .unwrap();
        assert_eq!(removal, StagedRemoval::Gone);
        assert_eq!(deleted, 0, "the object was deleted anyway");
        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        let created_at: i64 =
            sqlx::query_scalar("SELECT created_at FROM staged_objects WHERE path = ?")
                .bind(&path)
                .fetch_one(&mut connection)
                .await
                .unwrap();
        assert_eq!(created_at, 1_000, "the staged row was removed or aged");
    }
}
