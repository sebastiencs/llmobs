//! The `spans` view.

use std::sync::Arc;

use datafusion::datasource::TableProvider;
use datafusion::logical_expr::{LogicalPlan, col};

use super::{DURATION_MS, DURATION_NS, TimeWindow, duration_ms, map_plan, view_plan};
use crate::query_error::RunError;
use llmobs_core::schema::{spans, spans_schema};

pub(crate) fn spans_view(
    raw: Arc<dyn TableProvider>,
    window: Option<TimeWindow>,
) -> Result<LogicalPlan, RunError> {
    map_plan(view_plan(
        raw,
        window,
        super::ViewProjection {
            raw_name: "raw_spans",
            schema: spans_schema().as_ref(),
            window_column: spans::START_TIME,
            skip: &[spans::PROJECT_ID],
            hex: &[spans::TRACE_ID, spans::SPAN_ID, spans::PARENT_SPAN_ID],
        },
        |name| {
            if name == DURATION_NS {
                vec![duration_ms(col(DURATION_NS)).alias(DURATION_MS)]
            } else {
                Vec::new()
            }
        },
    ))
}
