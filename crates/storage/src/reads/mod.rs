//! Queries behind the read endpoints (D92).
//!
//! Each function takes a [`QueryContext`] and a request, runs under the
//! request's timeout, and returns Arrow batches in view-column order. The
//! server maps those batches to API types.

mod cursor;
mod traces;

use arrow::record_batch::RecordBatch;

use crate::views::TimeWindow;

pub use traces::{
    MAX_TRACE_SPANS, SPAN_LIST_COLUMNS, SpanListRequest, SpanSort, TraceDetailData,
    TraceListRequest, TraceSort, span_detail, span_list, trace_detail, trace_list,
};

/// Sort direction for a list. Nulls are last in both directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    /// Smallest values first.
    Asc,
    /// Largest values first.
    Desc,
}

/// Which rows a list includes before the caller's filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListScope {
    /// `start_time ∈ [from, to)` when set. The context itself is not windowed.
    pub window: Option<TimeWindow>,
    /// Keep rows whose environment is the eval environment.
    pub include_eval: bool,
    /// Keep rows that belong to an experiment.
    pub include_experiments: bool,
}

/// One page of a list.
#[derive(Debug)]
pub struct Page {
    /// Rows in sort order, at most [`MAX_PAGE`].
    pub batches: Vec<RecordBatch>,
    /// Cursor for the next page. `None` when this page is the last.
    pub next_cursor: Option<String>,
}

/// Most rows one list page returns.
pub const MAX_PAGE: usize = 1000;

/// Empty or all-whitespace text is no filter and no search.
pub(crate) fn present(text: Option<&str>) -> Option<&str> {
    text.filter(|text| !text.chars().all(char::is_whitespace))
}
