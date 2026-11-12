//! Physical optimizer rule for the "latest N traces" fast path (D153).
//!
//! A2's trace list is `ORDER BY start_time DESC, trace_id DESC LIMIT n+1` over
//! the `traces` view. The rule recognises that plan shape, walks down to the
//! range-mode [`TracesExec`], and switches it to list mode so it reads a few
//! short time slices instead of the whole range.

use std::sync::Arc;

use datafusion::common::ScalarValue;
use datafusion::common::config::ConfigOptions;
use datafusion::common::tree_node::{Transformed, TransformedResult, TreeNode};
use datafusion::error::Result as DfResult;
use datafusion::physical_expr::ScalarFunctionExpr;
use datafusion::physical_expr::expressions::{Column, Literal};
use datafusion::physical_expr::{PhysicalExpr, PhysicalSortExpr};
use datafusion::physical_optimizer::PhysicalOptimizerRule;
use datafusion::physical_plan::ExecutionPlan;
use datafusion::physical_plan::projection::{ProjectionExec, update_expr};
use datafusion::physical_plan::sorts::sort::SortExec;
use datafusion::physical_plan::{
    coalesce_partitions::CoalescePartitionsExec, repartition::RepartitionExec,
};
use llmobs_core::schema::traces;

use super::traces::{TracesExec, rewrite_list};

/// Rewrites the A2 trace-list plan to use list mode (D153).
#[derive(Debug)]
pub(crate) struct TraceListRule;

impl PhysicalOptimizerRule for TraceListRule {
    fn optimize(
        &self,
        plan: Arc<dyn ExecutionPlan>,
        _config: &ConfigOptions,
    ) -> DfResult<Arc<dyn ExecutionPlan>> {
        let Some((target, fetch)) = find_target(&plan) else {
            return Ok(plan);
        };
        // Rebuild every ancestor so their properties reflect the single output
        // partition of list mode. `transform_up` calls `replace_children` on
        // the way back up; the SortExec itself stays (it sorts at most k rows).
        plan.transform_up(|node| {
            if Arc::ptr_eq(&node, &target)
                && let Some(list) = rewrite_list(&node, fetch)
            {
                return Ok(Transformed::yes(list));
            }
            Ok(Transformed::no(node))
        })
        .data()
    }

    fn name(&self) -> &str {
        "TraceListRule"
    }

    fn schema_check(&self) -> bool {
        true
    }
}

/// Finds, top-down, the first matching `SortExec` and the range-mode
/// `TracesExec` at the end of its input chain.
fn find_target(plan: &Arc<dyn ExecutionPlan>) -> Option<(Arc<dyn ExecutionPlan>, usize)> {
    if let Some(sort) = plan.downcast_ref::<SortExec>()
        && let Some(fetch) = sort.fetch()
        && let Some(target) = match_sort(sort, fetch)
    {
        return Some((target, fetch));
    }
    for child in plan.children() {
        if let Some(found) = find_target(child) {
            return Some(found);
        }
    }
    None
}

fn match_sort(sort: &SortExec, _fetch: usize) -> Option<Arc<dyn ExecutionPlan>> {
    let expr = sort.expr();
    if expr.len() != 2 {
        return None;
    }
    let (first, second): (&PhysicalSortExpr, &PhysicalSortExpr) = (&expr[0], &expr[1]);
    if !first.options.descending || !second.options.descending {
        return None;
    }
    walk_to_scan(
        sort.input(),
        Arc::clone(&first.expr),
        Arc::clone(&second.expr),
    )
}

/// Walks from the SortExec's input to a range-mode `TracesExec` through
/// projections, coalesce-partitions and repartition only. Sort expressions are
/// unprojected through each projection. Any other node stops the walk.
fn walk_to_scan(
    node: &Arc<dyn ExecutionPlan>,
    start: Arc<dyn PhysicalExpr>,
    trace: Arc<dyn PhysicalExpr>,
) -> Option<Arc<dyn ExecutionPlan>> {
    if node.downcast_ref::<TracesExec>().is_some() {
        if !is_start_time(&start) || !is_trace_id(&trace) {
            return None;
        }
        return Some(Arc::clone(node));
    }
    let (start, trace) = if let Some(projection) = node.downcast_ref::<ProjectionExec>() {
        (
            update_expr(&start, projection.expr(), true).ok()??,
            update_expr(&trace, projection.expr(), true).ok()??,
        )
    } else if node.is::<CoalescePartitionsExec>() || node.is::<RepartitionExec>() {
        (start, trace)
    } else {
        return None;
    };
    let child = node.children().into_iter().next()?;
    walk_to_scan(child, start, trace)
}

/// At the scan, `start_time` must be a plain column of the input.
fn is_start_time(expr: &Arc<dyn PhysicalExpr>) -> bool {
    expr.downcast_ref::<Column>()
        .is_some_and(|column| column.name() == traces::START_TIME)
}

/// At the scan, `trace_id` may be a plain column or `encode(<id>, 'hex')`.
/// A2's view projects it that way, and lowercase hex sorts like the bytes.
fn is_trace_id(expr: &Arc<dyn PhysicalExpr>) -> bool {
    if let Some(column) = expr.downcast_ref::<Column>() {
        return column.name() == traces::TRACE_ID;
    }
    let Some(function) = expr.downcast_ref::<ScalarFunctionExpr>() else {
        return false;
    };
    if function.fun().name() != "encode" || function.args().len() != 2 {
        return false;
    }
    let Some(column) = function.args()[0].downcast_ref::<Column>() else {
        return false;
    };
    if column.name() != traces::TRACE_ID {
        return false;
    }
    let Some(literal) = function.args()[1].downcast_ref::<Literal>() else {
        return false;
    };
    matches!(
        literal.value(),
        ScalarValue::Utf8(Some(text))
            | ScalarValue::Utf8View(Some(text))
            | ScalarValue::LargeUtf8(Some(text))
            if text.eq_ignore_ascii_case("hex")
    )
}
