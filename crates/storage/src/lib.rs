//! Write-ahead log, memtable, Parquet files, catalog, compaction, deletion,
//! retention, media garbage collection, DataFusion table providers, the filter
//! DSL, and the SQL sandbox.
//!
//! See `docs/spec/03-storage.md` and `docs/spec/05-query.md`.

mod filter;
mod query_error;
mod query_text;
mod sql;
mod views;

pub use query_error::{InvalidQuery, Position, RunError};
pub use views::{
    ColumnInfo, ColumnKind, FunctionInfo, ProjectQuery, QueryContext, SqliteView, TimeWindow,
    ViewColumns, ViewName, function_catalog, view_catalog,
};

#[cfg(test)]
mod test_support;
