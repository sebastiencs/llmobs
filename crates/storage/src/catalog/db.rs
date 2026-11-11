//! `catalog.db`: one connection, the M2 schema, and the load into [`Catalog`].
//!
//! Wired into `Storage::open` in M2.6. Until then this module is reached from tests.
#![allow(dead_code)]

use std::path::Path;
use std::time::Duration;

use chrono::NaiveDate;
use llmobs_core::ids::ProjectId;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sqlx::{ConnectOptions, Row, SqliteConnection};
use uuid::Uuid;

use super::{Catalog, FileId, FileMeta, insert_sorted};
use crate::error::CatalogError;
use crate::wal::Table;

/// `flushed_seq` for the two tables. A missing `table_state` row is `0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TableState {
    /// Spans.
    pub spans: u64,
    /// Scores.
    pub scores: u64,
}

/// Open `<data_dir>/catalog.db`, set the durability pragmas, and migrate.
///
/// The directory must already exist. Pragmas are applied on connect, before
/// the migrations run. This is the only connection: readers use the in-memory
/// catalog.
pub(crate) async fn open_catalog_db(data_dir: &Path) -> Result<SqliteConnection, CatalogError> {
    let path = data_dir.join("catalog.db");
    let options = SqliteConnectOptions::new()
        .filename(&path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .foreign_keys(true)
        .busy_timeout(Duration::from_millis(5000));
    let mut connection = options
        .connect()
        .await
        .map_err(|source| CatalogError::Open { path, source })?;
    sqlx::migrate!("./migrations/catalog")
        .run(&mut connection)
        .await
        .map_err(|source| CatalogError::Migrate { source })?;
    Ok(connection)
}

/// Read every active file and both sequence rows.
pub(crate) async fn load(
    connection: &mut SqliteConnection,
) -> Result<(Catalog, TableState), CatalogError> {
    let rows = sqlx::query(
        "SELECT id, table_name, project_id, day, level, path, size_bytes, footer_size,
                row_count, min_time, max_time, min_ingest_time, max_ingest_time, fully_merged
         FROM data_files
         WHERE state = 'active'",
    )
    .fetch_all(&mut *connection)
    .await
    .map_err(CatalogError::db)?;
    let mut tables = Catalog::empty().tables;
    for row in &rows {
        insert_sorted(&mut tables, file_from_row(row)?);
    }
    let sequences = sqlx::query("SELECT table_name, flushed_seq FROM table_state")
        .fetch_all(&mut *connection)
        .await
        .map_err(CatalogError::db)?;
    let mut state = TableState {
        spans: 0,
        scores: 0,
    };
    for row in &sequences {
        let name: String = decode(row.try_get("table_name"))?;
        let stored: i64 = decode(row.try_get("flushed_seq"))?;
        let stored =
            u64::try_from(stored).map_err(|_| CatalogError::corrupt("invalid flushed_seq"))?;
        match name.as_str() {
            "spans" => state.spans = stored,
            "scores" => state.scores = stored,
            _ => return Err(CatalogError::corrupt("unknown table_name")),
        }
    }
    Ok((Catalog { tables }, state))
}

fn file_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<FileMeta, CatalogError> {
    let level: i64 = decode(row.try_get("level"))?;
    let fully_merged: i64 = decode(row.try_get("fully_merged"))?;
    let day: String = decode(row.try_get("day"))?;
    let id: String = decode(row.try_get("id"))?;
    let project: String = decode(row.try_get("project_id"))?;
    let table: String = decode(row.try_get("table_name"))?;
    Ok(FileMeta {
        id: parse_file_id(&id)?,
        table: parse_table(&table)?,
        project: parse_project(&project)?,
        day: parse_day(&day)?,
        level: parse_level(level)?,
        path: decode(row.try_get("path"))?,
        size: parse_u64(decode(row.try_get("size_bytes"))?, "invalid size")?,
        footer_size: parse_u64(decode(row.try_get("footer_size"))?, "invalid footer_size")?,
        rows: parse_u64(decode(row.try_get("row_count"))?, "invalid row_count")?,
        min_time: decode(row.try_get("min_time"))?,
        max_time: decode(row.try_get("max_time"))?,
        min_ingest_time: decode(row.try_get("min_ingest_time"))?,
        max_ingest_time: decode(row.try_get("max_ingest_time"))?,
        fully_merged: parse_flag(fully_merged)?,
    })
}

fn decode<T>(result: Result<T, sqlx::Error>) -> Result<T, CatalogError> {
    result.map_err(|source| match source {
        sqlx::Error::ColumnDecode { .. } => CatalogError::corrupt("invalid column"),
        other => CatalogError::Db { source: other },
    })
}

fn parse_file_id(text: &str) -> Result<FileId, CatalogError> {
    if text.len() != 36 {
        return Err(CatalogError::corrupt("invalid file id"));
    }
    Uuid::parse_str(text)
        .map(FileId)
        .map_err(|_| CatalogError::corrupt("invalid file id"))
}

fn parse_project(text: &str) -> Result<ProjectId, CatalogError> {
    text.parse()
        .map_err(|_| CatalogError::corrupt("invalid project_id"))
}

fn parse_day(text: &str) -> Result<NaiveDate, CatalogError> {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| CatalogError::corrupt("invalid day"))
}

fn parse_table(text: &str) -> Result<Table, CatalogError> {
    match text {
        "spans" => Ok(Table::Spans),
        "scores" => Ok(Table::Scores),
        _ => Err(CatalogError::corrupt("unknown table_name")),
    }
}

fn parse_level(value: i64) -> Result<u8, CatalogError> {
    match value {
        0 | 1 => u8::try_from(value).map_err(|_| CatalogError::corrupt("invalid level")),
        _ => Err(CatalogError::corrupt("invalid level")),
    }
}

fn parse_u64(value: i64, what: &'static str) -> Result<u64, CatalogError> {
    u64::try_from(value).map_err(|_| CatalogError::corrupt(what))
}

fn parse_flag(value: i64) -> Result<bool, CatalogError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(CatalogError::corrupt("invalid fully_merged")),
    }
}
