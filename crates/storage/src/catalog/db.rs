//! `catalog.db`: one connection, the M2 schema, and the load into [`Catalog`].
//!
//! Wired into `Storage::open` in M2.6. Until then this module is reached from tests.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

use chrono::NaiveDate;
use llmobs_core::ids::ProjectId;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
use sqlx::{ConnectOptions, Row, SqliteConnection};
use uuid::Uuid;

use super::{Catalog, FileId, FileMeta, FileTable, insert_sorted};
use crate::error::CatalogError;

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

/// Enforce spec 03 §3 over `file_sources` (D88), reading `catalog.db` directly.
///
/// Every active `spans` file is the source of exactly one active `traces` file,
/// and every active `traces` file has at least one source, each an active
/// `spans` file of the same `(project, day)`. `Storage::open` starts calling
/// this once flush writes traces files (M3.3); M3.1 tests it on catalogs built
/// with direct SQL.
pub(crate) async fn check_file_sources(
    connection: &mut SqliteConnection,
) -> Result<(), CatalogError> {
    struct Active {
        id: FileId,
        table: FileTable,
        project: String,
        day: String,
    }

    let rows = sqlx::query(
        "SELECT id, table_name, project_id, day FROM data_files WHERE state = 'active'",
    )
    .fetch_all(&mut *connection)
    .await
    .map_err(CatalogError::db)?;
    // BTreeMap: when several invariants fail, report the same file id every time.
    let mut active: BTreeMap<String, Active> = BTreeMap::new();
    for row in &rows {
        let id: String = decode(row.try_get("id"))?;
        let table: String = decode(row.try_get("table_name"))?;
        let file_id = parse_file_id(&id)?;
        let project: String = decode(row.try_get("project_id"))?;
        let day: String = decode(row.try_get("day"))?;
        active.insert(
            id,
            Active {
                id: file_id,
                table: parse_file_table(&table)?,
                project,
                day,
            },
        );
    }

    let source_rows = sqlx::query("SELECT traces_file_id, spans_file_id FROM file_sources")
        .fetch_all(&mut *connection)
        .await
        .map_err(CatalogError::db)?;
    let mut pairs: Vec<(String, String)> = Vec::with_capacity(source_rows.len());
    for row in &source_rows {
        pairs.push((
            decode(row.try_get("traces_file_id"))?,
            decode(row.try_get("spans_file_id"))?,
        ));
    }
    let mut by_spans: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut by_traces: HashMap<&str, Vec<&str>> = HashMap::new();
    for (traces, spans) in &pairs {
        by_spans
            .entry(spans.as_str())
            .or_default()
            .push(traces.as_str());
        by_traces
            .entry(traces.as_str())
            .or_default()
            .push(spans.as_str());
    }

    for (id, info) in &active {
        if info.table != FileTable::Spans {
            continue;
        }
        let derived = by_spans.get(id.as_str()).map_or(0, |traces| {
            traces
                .iter()
                .filter(|traces_file| {
                    active
                        .get(**traces_file)
                        .is_some_and(|file| file.table == FileTable::Traces)
                })
                .count()
        });
        if derived != 1 {
            return Err(CatalogError::SourcesInvariant { file: info.id.0 });
        }
    }
    for (id, info) in &active {
        if info.table != FileTable::Traces {
            continue;
        }
        let sources = by_traces
            .get(id.as_str())
            .map(Vec::as_slice)
            .unwrap_or_default();
        if sources.is_empty() {
            return Err(CatalogError::SourcesInvariant { file: info.id.0 });
        }
        for source in sources {
            let Some(span_file) = active.get(*source) else {
                return Err(CatalogError::SourcesInvariant { file: info.id.0 });
            };
            if span_file.table != FileTable::Spans
                || span_file.project != info.project
                || span_file.day != info.day
            {
                return Err(CatalogError::SourcesInvariant { file: info.id.0 });
            }
        }
    }
    Ok(())
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
        table: parse_file_table(&table)?,
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

fn parse_file_table(text: &str) -> Result<FileTable, CatalogError> {
    match text {
        "spans" => Ok(FileTable::Spans),
        "traces" => Ok(FileTable::Traces),
        "scores" => Ok(FileTable::Scores),
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

#[cfg(test)]
mod tests {
    use super::*;

    const PROJECT: &str = "018f6b4e-7c1a-7c1a-8c1a-000000000001";
    const DAY: &str = "2024-06-01";

    fn file_id(n: u8) -> Uuid {
        Uuid::from_u128(0x018f6b4e_7c1a_7c1a_8c1a_000000000000_u128 + u128::from(n))
    }

    async fn insert_file(connection: &mut SqliteConnection, id: Uuid, table: &str, day: &str) {
        sqlx::query(
            "INSERT INTO data_files (
                id, table_name, project_id, day, level, path, size_bytes, footer_size,
                row_count, min_time, max_time, min_ingest_time, max_ingest_time,
                fully_merged, created_at
             ) VALUES (?, ?, ?, ?, 0, ?, 1, 1, 1, 0, 0, 0, 0, 0, 1)",
        )
        .bind(id.to_string())
        .bind(table)
        .bind(PROJECT)
        .bind(day)
        .bind(format!("objects/{id}"))
        .execute(&mut *connection)
        .await
        .unwrap();
    }

    async fn insert_source(connection: &mut SqliteConnection, traces: Uuid, spans: Uuid) {
        sqlx::query("INSERT INTO file_sources (traces_file_id, spans_file_id) VALUES (?, ?)")
            .bind(traces.to_string())
            .bind(spans.to_string())
            .execute(&mut *connection)
            .await
            .unwrap();
    }

    async fn has_file_sources(connection: &mut SqliteConnection) -> bool {
        let columns: Vec<String> =
            sqlx::query_scalar("SELECT name FROM pragma_table_info('file_sources') ORDER BY cid")
                .fetch_all(&mut *connection)
                .await
                .unwrap();
        let index: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'file_sources_by_spans'",
        )
        .fetch_one(&mut *connection)
        .await
        .unwrap();
        columns == vec!["traces_file_id".to_owned(), "spans_file_id".to_owned()] && index == 1
    }

    /// Builds a catalog with direct SQL, then runs the spec 03 §3 check.
    async fn check_sources(
        files: &[(Uuid, &str, &str)],
        sources: &[(Uuid, Uuid)],
    ) -> Result<(), CatalogError> {
        let dir = tempfile::tempdir().unwrap();
        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        for (id, table, day) in files {
            insert_file(&mut connection, *id, table, day).await;
        }
        for (traces, spans) in sources {
            insert_source(&mut connection, *traces, *spans).await;
        }
        check_file_sources(&mut connection).await
    }

    #[tokio::test]
    async fn migration_applies_to_a_fresh_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        assert!(has_file_sources(&mut connection).await);
    }

    #[tokio::test]
    async fn migration_applies_to_an_m2_catalog() {
        let dir = tempfile::tempdir().unwrap();
        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        // An M2 catalog has only migration 0001 applied. Drop 0002's objects
        // and its migration row, then let open_catalog_db re-apply it.
        sqlx::query("DROP TABLE file_sources")
            .execute(&mut connection)
            .await
            .unwrap();
        sqlx::query("DELETE FROM _sqlx_migrations WHERE version = 2")
            .execute(&mut connection)
            .await
            .unwrap();
        drop(connection);

        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        assert!(has_file_sources(&mut connection).await);
        let versions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&mut connection)
            .await
            .unwrap();
        assert_eq!(versions, 2);
    }

    #[tokio::test]
    async fn deleting_a_data_file_removes_its_sources() {
        for deleted in [1_u8, 2] {
            let dir = tempfile::tempdir().unwrap();
            let mut connection = open_catalog_db(dir.path()).await.unwrap();
            let spans = file_id(1);
            let traces = file_id(2);
            insert_file(&mut connection, spans, "spans", DAY).await;
            insert_file(&mut connection, traces, "traces", DAY).await;
            insert_source(&mut connection, traces, spans).await;
            sqlx::query("DELETE FROM data_files WHERE id = ?")
                .bind(file_id(deleted).to_string())
                .execute(&mut connection)
                .await
                .unwrap();
            let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM file_sources")
                .fetch_one(&mut connection)
                .await
                .unwrap();
            assert_eq!(count, 0);
        }
    }

    #[tokio::test]
    async fn file_table_round_trips_through_table_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        insert_file(&mut connection, file_id(1), "spans", DAY).await;
        insert_file(&mut connection, file_id(2), "traces", DAY).await;
        insert_file(&mut connection, file_id(3), "scores", DAY).await;
        let (catalog, _) = load(&mut connection).await.unwrap();
        assert_eq!(catalog.test_files(FileTable::Spans).len(), 1);
        assert_eq!(catalog.test_files(FileTable::Traces).len(), 1);
        assert_eq!(catalog.test_files(FileTable::Scores).len(), 1);
    }

    #[tokio::test]
    async fn an_unknown_table_name_fails_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut connection = open_catalog_db(dir.path()).await.unwrap();
        insert_file(&mut connection, file_id(1), "unknown", DAY).await;
        let error = load(&mut connection).await.unwrap_err();
        assert!(matches!(error, CatalogError::Corrupt { what } if what == "unknown table_name"));
    }

    #[tokio::test]
    async fn sources_check_accepts_one_traces_file_per_spans_file() {
        let spans = file_id(1);
        let traces = file_id(2);
        assert!(
            check_sources(
                &[(spans, "spans", DAY), (traces, "traces", DAY)],
                &[(traces, spans)],
            )
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn sources_check_rejects_a_spans_file_without_a_traces_file() {
        let spans = file_id(1);
        let error = check_sources(&[(spans, "spans", DAY)], &[])
            .await
            .unwrap_err();
        assert!(matches!(error, CatalogError::SourcesInvariant { file } if file == spans));
    }

    #[tokio::test]
    async fn sources_check_rejects_a_spans_file_with_two_traces_files() {
        let spans = file_id(1);
        let first = file_id(2);
        let second = file_id(3);
        let error = check_sources(
            &[
                (spans, "spans", DAY),
                (first, "traces", DAY),
                (second, "traces", DAY),
            ],
            &[(first, spans), (second, spans)],
        )
        .await
        .unwrap_err();
        assert!(matches!(error, CatalogError::SourcesInvariant { file } if file == spans));
    }

    #[tokio::test]
    async fn sources_check_rejects_a_traces_file_without_a_source() {
        let traces = file_id(1);
        let error = check_sources(&[(traces, "traces", DAY)], &[])
            .await
            .unwrap_err();
        assert!(matches!(error, CatalogError::SourcesInvariant { file } if file == traces));
    }

    #[tokio::test]
    async fn sources_check_rejects_a_source_in_another_day() {
        let spans = file_id(1);
        let traces = file_id(2);
        let error = check_sources(
            &[(spans, "spans", DAY), (traces, "traces", "2024-06-02")],
            &[(traces, spans)],
        )
        .await
        .unwrap_err();
        assert!(matches!(error, CatalogError::SourcesInvariant { file } if file == traces));
    }
}
