//! The `traces` view.

use std::sync::Arc;

use datafusion::datasource::TableProvider;
use datafusion::logical_expr::LogicalPlan;

use super::{
    DURATION_MS, DURATION_NS, TimeWindow, duration_ms, duration_ns_from_bounds, map_plan, view_plan,
};
use crate::query_error::RunError;
use llmobs_core::schema::{traces, traces_schema};

pub(crate) fn traces_view(
    raw: Arc<dyn TableProvider>,
    window: Option<TimeWindow>,
) -> Result<LogicalPlan, RunError> {
    map_plan(view_plan(
        raw,
        window,
        super::ViewProjection {
            raw_name: "raw_traces",
            schema: traces_schema().as_ref(),
            window_column: traces::START_TIME,
            skip: &[
                traces::PROJECT_ID,
                traces::LAST_INGEST_TIME,
                traces::PARTIAL_COUNT,
            ],
            hex: &[traces::TRACE_ID, traces::ROOT_SPAN_ID],
        },
        |name| {
            if name == traces::END_TIME {
                let duration_ns = duration_ns_from_bounds();
                vec![
                    duration_ns.clone().alias(DURATION_NS),
                    duration_ms(duration_ns).alias(DURATION_MS),
                ]
            } else {
                Vec::new()
            }
        },
    ))
}
