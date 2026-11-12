//! `meta.db`: identity, tenancy, and credentials (spec 04, D136).
//!
//! One writer connection sits behind a [`tokio::sync::Mutex`]. A pool of four
//! read-only connections serves reads. Migrations run only in [`MetaDb::open`]
//! (`llmobs serve`). [`MetaDb::open_existing`] refuses a file whose latest
//! migration is not this binary's.
#![cfg_attr(not(test), allow(dead_code))] // `llmobs serve` opens this from P5 on

pub(crate) mod api_keys;
pub(crate) mod email_verifications;
pub(crate) mod invites;
pub(crate) mod members;
pub(crate) mod orgs;
pub(crate) mod password_resets;
pub(crate) mod personal_tokens;
pub(crate) mod projects;
pub(crate) mod users;
pub(crate) mod web_sessions;

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions, SqliteSynchronous,
};
use sqlx::{ConnectOptions, Connection, SqliteConnection};
use tokio::sync::Mutex;

/// How long a connection waits for SQLite's write lock (D136).
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// `meta.db` for one data directory.
#[derive(Debug)]
pub(crate) struct MetaDb {
    writer: Mutex<SqliteConnection>,
    readers: SqlitePool,
}

impl MetaDb {
    /// Creates `<data_dir>/meta.db` when it is missing, sets the D136 pragmas,
    /// and runs migrations.
    ///
    /// `data_dir` must already exist. `llmobs serve` calls this; admin
    /// commands use [`Self::open_existing`].
    pub(crate) async fn open(data_dir: &Path) -> Result<Self, MetaError> {
        let path = data_dir.join("meta.db");
        let mut writer = connect(&path, true).await?;
        migrator()
            .run(&mut writer)
            .await
            .map_err(|source| MetaError::Migrate { source })?;
        let readers = reader_pool(&path).await?;
        let db = Self {
            writer: Mutex::new(writer),
            readers,
        };
        db.ping().await?;
        Ok(db)
    }

    /// Opens an existing `meta.db` without migrating.
    ///
    /// A missing file is [`MetaError::NotInitialized`]. A latest applied
    /// migration other than this binary's is [`MetaError::SchemaMismatch`].
    /// No migration rows counts as version `0`.
    pub(crate) async fn open_existing(data_dir: &Path) -> Result<Self, MetaError> {
        let path = data_dir.join("meta.db");
        if !path.is_file() {
            return Err(MetaError::NotInitialized);
        }
        let mut writer = connect(&path, false).await?;
        let found = applied_version(&mut writer).await?;
        let expected = expected_version();
        if found != expected {
            return Err(MetaError::SchemaMismatch { found, expected });
        }
        let readers = reader_pool(&path).await?;
        let db = Self {
            writer: Mutex::new(writer),
            readers,
        };
        db.ping().await?;
        Ok(db)
    }

    /// The read-only pool. Four connections, `foreign_keys` and `busy_timeout`
    /// set on each.
    pub(crate) fn reader(&self) -> &SqlitePool {
        &self.readers
    }

    /// Runs `f` on the writer inside `BEGIN IMMEDIATE` and commits.
    ///
    /// `f` returns a boxed future that borrows the connection (`BoxFuture`).
    /// An async closure is not higher-ranked here: it implements
    /// [`AsyncFnOnce`] for one lifetime, so `for<'c> AsyncFnOnce` rejects it.
    /// [`Ok`] commits. [`Err`] rolls back. If the returned future is dropped,
    /// sqlx rolls the transaction back on drop, and the next `write` finds
    /// the connection outside a transaction.
    pub(crate) async fn write<T, F>(&self, f: F) -> Result<T, MetaError>
    where
        F: for<'c> FnOnce(
                &'c mut SqliteConnection,
            )
                -> Pin<Box<dyn Future<Output = Result<T, MetaError>> + Send + 'c>>
            + Send,
    {
        let mut writer = self.writer.lock().await;
        let mut tx = writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(MetaError::from)?;
        match f(&mut tx).await {
            Ok(value) => {
                tx.commit().await.map_err(MetaError::from)?;
                Ok(value)
            }
            Err(error) => {
                tx.rollback().await.map_err(MetaError::from)?;
                Err(error)
            }
        }
    }

    /// `SELECT 1` on the read pool.
    pub(crate) async fn ping(&self) -> Result<(), MetaError> {
        sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(&self.readers)
            .await
            .map(|_| ())
            .map_err(MetaError::from)
    }

    /// Closes the pool and the writer. Close errors are ignored; the process
    /// is shutting the file down.
    pub(crate) async fn close(self) {
        self.readers.close().await;
        let writer = self.writer.into_inner();
        let _ = writer.close().await;
    }
}

/// Why opening or using `meta.db` failed.
///
/// Display text names paths, migration versions, and static reasons. It does
/// not include row values.
#[derive(Debug, thiserror::Error)]
pub enum MetaError {
    /// The file could not be opened.
    #[error("meta.db {path}: {source}")]
    Open {
        /// `<data_dir>/meta.db`.
        path: PathBuf,
        /// The SQLite error.
        #[source]
        source: sqlx::Error,
    },
    /// A migration could not be applied.
    #[error("meta.db migration: {source}")]
    Migrate {
        /// The migration error.
        #[source]
        source: sqlx::migrate::MigrateError,
    },
    /// `open_existing` found no `meta.db`.
    #[error("meta.db is not initialized")]
    NotInitialized,
    /// The latest applied migration is not the one this binary embeds.
    #[error("meta.db schema version {found}, binary expects {expected}")]
    SchemaMismatch {
        /// Highest `version` in `_sqlx_migrations`, or `0` when that table is empty.
        found: i64,
        /// Highest migration version embedded in this binary.
        expected: i64,
    },
    /// A query failed.
    #[error("meta.db: {0}")]
    Db(sqlx::Error),
    /// A unique constraint failed. Stores map the constraint onto a static reason.
    #[error("{0}")]
    Conflict(&'static str),
    /// A stored value does not decode. The names are static column identifiers.
    #[error("corrupt {table}.{column}")]
    Corrupt {
        /// Table name.
        table: &'static str,
        /// Column name.
        column: &'static str,
    },
}

/// A unique violation becomes `conflict`. Every other database error stays [`MetaError::Db`].
pub(crate) fn unique_or(error: sqlx::Error, conflict: &'static str) -> MetaError {
    if error
        .as_database_error()
        .is_some_and(|db| db.is_unique_violation())
    {
        MetaError::Conflict(conflict)
    } else {
        MetaError::from(error)
    }
}

impl From<sqlx::Error> for MetaError {
    fn from(source: sqlx::Error) -> Self {
        Self::Db(source)
    }
}

fn migrator() -> sqlx::migrate::Migrator {
    sqlx::migrate!("./migrations/meta")
}

fn expected_version() -> i64 {
    migrator()
        .migrations
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap_or(0)
}

async fn connect(path: &Path, create: bool) -> Result<SqliteConnection, MetaError> {
    writer_options(path, create)
        .connect()
        .await
        .map_err(|source| MetaError::Open {
            path: path.to_path_buf(),
            source,
        })
}

fn writer_options(path: &Path, create: bool) -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(create)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .foreign_keys(true)
        .busy_timeout(BUSY_TIMEOUT)
        // sqlx's statement log includes bound values.
        .disable_statement_logging()
}

fn reader_options(path: &Path) -> SqliteConnectOptions {
    // `journal_mode` is not set here. Changing it takes an exclusive lock that
    // `busy_timeout` cannot wait out, and the writer already set WAL on the file.
    SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .synchronous(SqliteSynchronous::Full)
        .foreign_keys(true)
        .busy_timeout(BUSY_TIMEOUT)
        .disable_statement_logging()
}

#[cfg(test)]
pub(crate) async fn test_db() -> (tempfile::TempDir, MetaDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = MetaDb::open(dir.path()).await.unwrap();
    (dir, db)
}

async fn reader_pool(path: &Path) -> Result<SqlitePool, MetaError> {
    SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(reader_options(path))
        .await
        .map_err(|source| MetaError::Open {
            path: path.to_path_buf(),
            source,
        })
}

async fn applied_version(connection: &mut SqliteConnection) -> Result<i64, MetaError> {
    let present: Option<i64> = sqlx::query_scalar(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(&mut *connection)
    .await?;
    if present.is_none() {
        return Ok(0);
    }
    let version: Option<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version DESC LIMIT 1")
            .fetch_optional(&mut *connection)
            .await?;
    Ok(version.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlx::SqliteConnection;

    use super::MetaDb;
    use crate::meta::MetaError;

    async fn pragmas(connection: &mut SqliteConnection) -> (String, i64, i64, i64) {
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        (journal, synchronous, foreign_keys, busy_timeout)
    }

    #[tokio::test]
    async fn open_creates_the_file_and_sets_pragmas() {
        let dir = tempfile::tempdir().unwrap();
        let db = MetaDb::open(dir.path()).await.unwrap();
        assert!(dir.path().join("meta.db").is_file());

        let (journal, synchronous, foreign_keys, busy_timeout) = db
            .write(|connection| Box::pin(async move { Ok(pragmas(connection).await) }))
            .await
            .unwrap();
        assert_eq!(journal, "wal");
        assert_eq!(synchronous, 2);
        assert_eq!(foreign_keys, 1);
        assert_eq!(busy_timeout, 5000);

        let mut reader = db.reader().acquire().await.unwrap();
        let (journal, synchronous, foreign_keys, busy_timeout) = pragmas(&mut reader).await;
        assert_eq!(journal, "wal");
        assert_eq!(synchronous, 2);
        assert_eq!(foreign_keys, 1);
        assert_eq!(busy_timeout, 5000);
    }

    #[tokio::test]
    async fn schema_matches_the_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let db = MetaDb::open(dir.path()).await.unwrap();
        let rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
            "SELECT type, name, sql FROM sqlite_master \
             WHERE name NOT LIKE '\\_sqlx%' ESCAPE '\\' ORDER BY name",
        )
        .fetch_all(db.reader())
        .await
        .unwrap();
        let mut text = String::new();
        for (kind, name, sql) in rows {
            text.push_str(&kind);
            text.push(' ');
            text.push_str(&name);
            text.push('\n');
            if let Some(sql) = sql {
                text.push_str(&sql);
                text.push('\n');
            }
            text.push('\n');
        }
        insta::with_settings!({
            snapshot_path => "../../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_snapshot!("meta_schema", text);
        });
    }

    #[tokio::test]
    async fn open_twice_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let db = MetaDb::open(dir.path()).await.unwrap();
        db.close().await;
        let db = MetaDb::open(dir.path()).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(count, 1);
        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(users, 0);
    }

    #[tokio::test]
    async fn open_existing_checks_the_file_and_the_version() {
        let dir = tempfile::tempdir().unwrap();
        let missing = MetaDb::open_existing(dir.path()).await.unwrap_err();
        assert!(matches!(missing, MetaError::NotInitialized), "{missing}");

        let db = MetaDb::open(dir.path()).await.unwrap();
        db.close().await;
        MetaDb::open_existing(dir.path()).await.unwrap();

        let db = MetaDb::open(dir.path()).await.unwrap();
        db.write(|connection| {
            Box::pin(async move {
                sqlx::query(
                    "INSERT INTO _sqlx_migrations \
                     (version, description, success, checksum, execution_time) \
                     VALUES (2, 'future', 1, ?, 0)",
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

        let error = MetaDb::open_existing(dir.path()).await.unwrap_err();
        match error {
            MetaError::SchemaMismatch { found, expected } => {
                assert_eq!(found, 2);
                assert_eq!(expected, 1);
            }
            other => panic!("{other}"),
        }
    }

    #[tokio::test]
    async fn write_commits_and_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let db = MetaDb::open(dir.path()).await.unwrap();
        insert_org(&db, "org-kept").await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orgs WHERE id = 'org-kept'")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(count, 1);

        let error = db
            .write(|connection| {
                Box::pin(async move {
                    insert_org_on(connection, "org-rolled-back").await?;
                    Err::<(), MetaError>(MetaError::Conflict("rolled back"))
                })
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("rolled back"), "{error}");
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM orgs WHERE id = 'org-rolled-back'")
                .fetch_one(db.reader())
                .await
                .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn dropped_write_leaves_no_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let db = MetaDb::open(dir.path()).await.unwrap();
        let (started, inserted) = tokio::sync::oneshot::channel();
        {
            // The block drops the future, and the writer lock with it, before
            // the next `write`.
            let mut write = std::pin::pin!(db.write(move |connection| {
                Box::pin(async move {
                    insert_org_on(connection, "org-dropped").await?;
                    let _ = started.send(());
                    std::future::pending::<()>().await;
                    Ok(())
                })
            }));
            tokio::select! {
                result = write.as_mut() => panic!("dropped write finished: {result:?}"),
                _ = inserted => {}
            }
        }

        insert_org(&db, "org-after").await.unwrap();
        let dropped: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orgs WHERE id = 'org-dropped'")
            .fetch_one(db.reader())
            .await
            .unwrap();
        let kept: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orgs WHERE id = 'org-after'")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(dropped, 0);
        assert_eq!(kept, 1);
    }

    #[tokio::test]
    async fn readers_cannot_write() {
        let dir = tempfile::tempdir().unwrap();
        let db = MetaDb::open(dir.path()).await.unwrap();
        let mut reader = db.reader().acquire().await.unwrap();
        let error = insert_org_on(&mut reader, "org-reader").await.unwrap_err();
        assert!(error.to_string().contains("readonly"), "{error}");
    }

    #[tokio::test]
    async fn concurrent_writes_all_commit() {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(MetaDb::open(dir.path()).await.unwrap());
        let mut tasks = Vec::new();
        for index in 0..50 {
            let db = Arc::clone(&db);
            tasks.push(tokio::spawn(async move {
                insert_org(&db, &format!("org-{index}")).await.unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM orgs")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(count, 50);
    }

    async fn insert_org(db: &MetaDb, id: &str) -> Result<(), MetaError> {
        let id = id.to_owned();
        db.write(move |connection| {
            Box::pin(async move {
                insert_org_on(connection, &id).await?;
                Ok(())
            })
        })
        .await
    }

    async fn insert_org_on(connection: &mut SqliteConnection, id: &str) -> Result<(), MetaError> {
        sqlx::query(
            "INSERT INTO orgs (id, name, slug, created_at, updated_at) VALUES (?, ?, ?, 0, 0)",
        )
        .bind(id)
        .bind(id)
        .bind(id)
        .execute(&mut *connection)
        .await?;
        Ok(())
    }
}
