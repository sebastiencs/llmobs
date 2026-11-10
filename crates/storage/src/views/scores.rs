//! The `scores` view.

use std::sync::Arc;

use datafusion::datasource::TableProvider;
use datafusion::logical_expr::{Expr, LogicalPlan};

use super::{TimeWindow, map_plan, view_plan};
use crate::query_error::RunError;
use llmobs_core::schema::{scores, scores_schema};

pub(crate) fn scores_view(
    raw: Arc<dyn TableProvider>,
    window: Option<TimeWindow>,
) -> Result<LogicalPlan, RunError> {
    map_plan(view_plan(
        raw,
        window,
        super::ViewProjection {
            raw_name: "raw_scores",
            schema: scores_schema().as_ref(),
            window_column: scores::CREATED_AT,
            skip: &[scores::PROJECT_ID],
            hex: &[scores::TRACE_ID, scores::SPAN_ID],
        },
        |_name| Vec::<Expr>::new(),
    ))
}
