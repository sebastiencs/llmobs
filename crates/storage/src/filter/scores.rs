//! Latest-score joins for `score`, `score_label` and `has_score` (D97).

use std::sync::Arc;

use datafusion::arrow::datatypes::{DataType, Field, Fields};
use datafusion::common::{DFSchema, DFSchemaRef, ScalarValue};
use datafusion::error::DataFusionError;
use datafusion::functions_window::expr_fn::row_number;
use datafusion::logical_expr::{Expr, ExprFunctionExt, JoinType, col, lit};
use datafusion::prelude::DataFrame;
use llmobs_core::enums::ScoreTargetKind;

use super::FilterView;
use crate::query_error::InvalidQuery;
use crate::query_text::planning_failure;
use crate::views::QueryContext;

/// Distinct score names one filter may join.
pub(crate) const MAX_SCORE_NAMES: usize = 8;

/// View schema plus one nullable triple per distinct score name.
pub(crate) fn planning_schema(
    view: &DFSchema,
    names: &[String],
) -> Result<DFSchemaRef, InvalidQuery> {
    let mut fields: Vec<Arc<Field>> = view.fields().iter().cloned().collect();
    for index in 0..names.len() {
        fields.push(nullable(format!("__score_{index}_num"), DataType::Float64));
        fields.push(nullable(format!("__score_{index}_str"), DataType::Utf8));
        fields.push(nullable(
            format!("__score_{index}_present"),
            DataType::Boolean,
        ));
    }
    DFSchema::from_unqualified_fields(Fields::from(fields), view.metadata().clone())
        .map(Arc::new)
        .map_err(|error| planning_failure(&error))
}

fn nullable(name: String, data_type: DataType) -> Arc<Field> {
    Arc::new(Field::new(name, data_type, true))
}

/// Join `frame` to the latest row of each score name, filter, and drop the
/// score columns so the output is the view's own columns.
pub(crate) async fn apply(
    frame: DataFrame,
    ctx: &QueryContext,
    view: FilterView,
    names: &[String],
    expr: Expr,
) -> Result<DataFrame, DataFusionError> {
    let columns: Vec<String> = frame
        .schema()
        .fields()
        .iter()
        .map(|field| field.name().clone())
        .collect();
    let mut frame = frame;
    if !names.is_empty() {
        let (target, keys) = join_on(view)?;
        for (index, name) in names.iter().enumerate() {
            let latest = latest(ctx, target, keys, index, name).await?;
            let right: Vec<String> = keys
                .iter()
                .map(|key| format!("__score_{index}_{key}"))
                .collect();
            let right_names: Vec<&str> = right.iter().map(String::as_str).collect();
            frame = frame.join(latest, JoinType::Left, keys, &right_names, None)?;
        }
    }
    frame.filter(expr)?.select(columns.into_iter().map(col))
}

fn join_on(view: FilterView) -> Result<(&'static str, &'static [&'static str]), DataFusionError> {
    Ok(match view {
        FilterView::Traces => (ScoreTargetKind::Trace.as_str(), &["trace_id"]),
        FilterView::Spans => (ScoreTargetKind::Span.as_str(), &["trace_id", "span_id"]),
        FilterView::Sessions => (ScoreTargetKind::Session.as_str(), &["session_id"]),
        FilterView::Scores => {
            return Err(DataFusionError::Internal(
                "score functions are not available on the scores view".to_owned(),
            ));
        }
    })
}

async fn latest(
    ctx: &QueryContext,
    target: &str,
    keys: &[&str],
    index: usize,
    name: &str,
) -> Result<DataFrame, DataFusionError> {
    let ranked = ctx
        .session()
        .table("scores")
        .await?
        .filter(col("target").eq(lit(target)).and(col("name").eq(lit(name))))?
        .window(vec![
            row_number()
                .partition_by(keys.iter().copied().map(col).collect())
                .order_by(vec![
                    col("updated_at").sort(false, true),
                    col("score_id").sort(false, true),
                ])
                .build()?
                .alias("__rn"),
        ])?
        .filter(col("__rn").eq(lit(ScalarValue::UInt64(Some(1)))))?;
    let mut projection = Vec::with_capacity(keys.len() + 3);
    for key in keys {
        projection.push(col(*key).alias(format!("__score_{index}_{key}")));
    }
    projection.push(col("value_num").alias(format!("__score_{index}_num")));
    projection.push(col("value_str").alias(format!("__score_{index}_str")));
    projection.push(lit(true).alias(format!("__score_{index}_present")));
    ranked.select(projection)
}
