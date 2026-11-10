//! Write-ahead log, memtable, Parquet files, catalog, compaction, deletion,
//! retention, media garbage collection, DataFusion table providers, the filter
//! DSL, and the SQL sandbox.
//!
//! See `docs/spec/03-storage.md` and `docs/spec/05-query.md`.

mod query_error;
mod views;

pub use query_error::RunError;
pub use views::{ProjectQuery, QueryContext, TimeWindow, ViewName};

#[cfg(test)]
mod test_support;
