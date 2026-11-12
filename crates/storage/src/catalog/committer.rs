//! Compare-and-set edits for `catalog.db`.
//!
//! The mutex is held across the SQLite transaction and the `publish` call.
//! Lock order: this mutex, then a table memtable, then the `Published` write
//! lock. `publish` must not call back into this committer.
//!
//! `Storage` owns one committer. Flush, and later compaction, are its callers.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::{Connection, SqliteConnection};
use tokio::sync::Mutex;

use super::db::{TableState, check_file_sources, load, open_catalog_db, parse_file_id};
use super::{Catalog, FileId, FileMeta, FileState, FileTable, table_index};
use crate::error::{CatalogError, CommitError, GcError};
use crate::hooks::{HookPoint, Hooks};
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
}

struct CommitterInner {
    connection: SqliteConnection,
    catalog: Arc<Catalog>,
    flushed: [u64; 2],
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
    /// Open `catalog.db`, migrate, and load the active files.
    pub(crate) async fn open(data_dir: &Path, hooks: Hooks) -> Result<Self, CatalogError> {
        let mut connection = open_catalog_db(data_dir).await?;
        let (catalog, state) = load(&mut connection).await?;
        check_file_sources(&mut connection).await?;
        Ok(Self {
            inner: Mutex::new(CommitterInner {
                connection,
                catalog: Arc::new(catalog),
                flushed: flushed_of(state),
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

    /// Paths still in `staged_objects`, in path order.
    #[cfg(test)]
    pub(crate) async fn staged_paths(&self) -> Result<Vec<String>, CatalogError> {
        let mut inner = self.inner.lock().await;
        sqlx::query_scalar("SELECT path FROM staged_objects ORDER BY path")
            .fetch_all(&mut inner.connection)
            .await
            .map_err(CatalogError::db)
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
        publish(&next);
        Ok(next)
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

    /// Delete one staged object that is not a catalog file, then its row.
    ///
    /// `delete` runs while this mutex is held, so a commit cannot publish the
    /// same path between the check and the delete. A missing object is success
    /// and still drops the row. A path that is also in `data_files` is left
    /// untouched.
    pub(crate) async fn collect_staged_path<F, Fut>(
        &self,
        path: &str,
        hooks: &Hooks,
        delete: F,
    ) -> Result<StagedRemoval, GcError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<u64, crate::error::ObjectError>>,
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
        let staged: Option<i64> = sqlx::query_scalar("SELECT 1 FROM staged_objects WHERE path = ?")
            .bind(path)
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
        Fut: Future<Output = Result<u64, crate::error::ObjectError>>,
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
            fully_merged, state, created_at
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'active', ?)",
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
    use llmobs_core::ids::ProjectId;
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
        assert_eq!(migrations, 2);
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
        let dropped: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM pragma_table_info('data_files')
             WHERE name = 'deletions_applied_seq'",
        )
        .fetch_one(&mut again)
        .await
        .unwrap();
        assert_eq!(dropped, 0);
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
            },
            CatalogEdit {
                add: vec![second],
                remove: Vec::new(),
                flushed_seq: None,
                sources: Vec::new(),
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
        };
        committer.stage(&["doomed".to_owned()]).await.unwrap();
        committer.commit(&edit, |_| {}).await.unwrap();
        let remove = CatalogEdit {
            add: Vec::new(),
            remove: vec![(id, FileState::Active)],
            flushed_seq: None,
            sources: Vec::new(),
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
        };
        committer.commit(&added, |_| {}).await.unwrap();
        let before = committer.catalog().await;

        let missing = CatalogEdit {
            add: vec![file(FileTable::Spans, spans, day(1), 2, "missing")],
            remove: Vec::new(),
            flushed_seq: Some((Table::Spans, 4)),
            sources: Vec::new(),
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
}
