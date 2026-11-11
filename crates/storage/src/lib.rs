//! Write-ahead log, memtable, Parquet files, catalog, compaction, deletion,
//! retention, media garbage collection, DataFusion table providers, the filter
//! DSL, and the SQL sandbox.
//!
//! See `docs/spec/03-storage.md` and `docs/spec/05-query.md`.

mod filter;
mod query_error;
mod query_text;
mod reads;
mod sql;
mod views;

pub use query_error::{InvalidQuery, Position, RunError};
pub use reads::{
    ListScope, MAX_PAGE, MAX_TRACE_SPANS, Page, SPAN_LIST_COLUMNS, SortOrder, SpanListRequest,
    SpanSort, TraceDetailData, TraceListRequest, TraceSort, span_detail, span_list, trace_detail,
    trace_list,
};
pub use sql::{EXPORT_MAX_ROWS, ExportFormat, SQL_MAX_ROWS, SqlColumn, SqlJson, SqlRequest};
pub use views::{
    ColumnInfo, ColumnKind, FunctionInfo, ProjectQuery, QueryContext, SqliteView, TimeWindow,
    ViewColumns, ViewName, function_catalog, view_catalog,
};

#[cfg(test)]
mod test_support;
