//! Rewrite hex literals on view ID columns into stored-byte comparisons.
//!
//! Joins between two views on an ID are not rewritten: each side's `encode`
//! sits in its own projection, so the join compares the hex strings.

use arrow::datatypes::DataType;
use datafusion::common::tree_node::{Transformed, TreeNode};
use datafusion::common::{DFSchema, ScalarValue};
use datafusion::error::DataFusionError;
use datafusion::logical_expr::expr::{BinaryExpr, InList};
use datafusion::logical_expr::{Expr, ExprSchemable, LogicalPlan, Operator};
use datafusion::optimizer::{ApplyOrder, OptimizerConfig, OptimizerRule};

/// Pushes `encode(id, 'hex')` comparisons down onto the stored bytes.
#[derive(Debug)]
pub(crate) struct HexIdRule;

impl OptimizerRule for HexIdRule {
    fn name(&self) -> &str {
        "hex_id_literals"
    }

    fn apply_order(&self) -> Option<ApplyOrder> {
        Some(ApplyOrder::BottomUp)
    }

    #[allow(deprecated)]
    fn supports_rewrite(&self) -> bool {
        true
    }

    fn rewrite(
        &self,
        plan: LogicalPlan,
        _config: &dyn OptimizerConfig,
    ) -> Result<Transformed<LogicalPlan>, DataFusionError> {
        let schema = expression_schema(&plan)?;
        plan.map_expressions(|expr| rewrite_expr(expr, &schema))
    }
}

fn expression_schema(plan: &LogicalPlan) -> Result<DFSchema, DataFusionError> {
    if let LogicalPlan::TableScan(scan) = plan {
        return DFSchema::try_from_qualified_schema(scan.table_name.clone(), &scan.source.schema());
    }
    let mut schema = DFSchema::empty();
    for input in plan.inputs() {
        schema.merge(input.schema());
    }
    Ok(schema)
}

fn rewrite_expr(expr: Expr, schema: &DFSchema) -> Result<Transformed<Expr>, DataFusionError> {
    expr.transform_up(|expr| Ok(rewrite_one(expr, schema)))
}

fn rewrite_one(expr: Expr, schema: &DFSchema) -> Transformed<Expr> {
    let rewritten = match &expr {
        Expr::BinaryExpr(binary) if matches!(binary.op, Operator::Eq | Operator::NotEq) => {
            rewrite_comparison(binary, schema)
        }
        Expr::InList(in_list) => rewrite_in_list(in_list, schema),
        _ => None,
    };
    match rewritten {
        Some(next) => Transformed::yes(next),
        None => Transformed::no(expr),
    }
}

fn rewrite_comparison(binary: &BinaryExpr, schema: &DFSchema) -> Option<Expr> {
    match (
        encoded_id(&binary.left, schema),
        encoded_id(&binary.right, schema),
    ) {
        (Some((column, width)), None) => Some(compare(
            column,
            binary.op,
            byte_literal(width, hex_literal(&binary.right, width)?),
        )),
        (None, Some((column, width))) => Some(compare(
            column,
            binary.op,
            byte_literal(width, hex_literal(&binary.left, width)?),
        )),
        (Some((left, left_width)), Some((right, right_width))) if left_width == right_width => {
            Some(compare(left, binary.op, right))
        }
        _ => None,
    }
}

fn rewrite_in_list(in_list: &InList, schema: &DFSchema) -> Option<Expr> {
    let (column, width) = encoded_id(&in_list.expr, schema)?;
    if in_list.list.is_empty() {
        return None;
    }
    let mut literals = Vec::with_capacity(in_list.list.len());
    for item in &in_list.list {
        literals.push(byte_literal(width, hex_literal(item, width)?));
    }
    Some(Expr::InList(InList::new(
        Box::new(column),
        literals,
        in_list.negated,
    )))
}

/// `encode(<expr>, 'hex')` whose inner expression is `FixedSizeBinary(n)`.
fn encoded_id(expr: &Expr, schema: &DFSchema) -> Option<(Expr, i32)> {
    let Expr::ScalarFunction(func) = peel(expr) else {
        return None;
    };
    if func.name() != "encode" {
        return None;
    }
    let [inner, format] = func.args.as_slice() else {
        return None;
    };
    if !is_hex_format(format) {
        return None;
    }
    let inner = peel(inner);
    let DataType::FixedSizeBinary(width) = inner.get_type(schema).ok()? else {
        return None;
    };
    if width <= 0 {
        return None;
    }
    Some((inner.clone(), width))
}

fn is_hex_format(expr: &Expr) -> bool {
    text_literal(expr).is_some_and(|text| text.eq_ignore_ascii_case("hex"))
}

fn hex_literal(expr: &Expr, width: i32) -> Option<Vec<u8>> {
    let text = text_literal(expr)?;
    let expected = usize::try_from(width).ok()?.checked_mul(2)?;
    if text.len() != expected || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    hex::decode(text).ok()
}

fn text_literal(expr: &Expr) -> Option<&str> {
    let Expr::Literal(value, _) = peel(expr) else {
        return None;
    };
    match value {
        ScalarValue::Utf8(Some(text))
        | ScalarValue::Utf8View(Some(text))
        | ScalarValue::LargeUtf8(Some(text)) => Some(text),
        _ => None,
    }
}

fn peel(expr: &Expr) -> &Expr {
    match expr {
        Expr::Alias(alias) => peel(&alias.expr),
        other => other,
    }
}

fn compare(left: Expr, op: Operator, right: Expr) -> Expr {
    Expr::BinaryExpr(BinaryExpr::new(Box::new(left), op, Box::new(right)))
}

fn byte_literal(width: i32, bytes: Vec<u8>) -> Expr {
    Expr::Literal(ScalarValue::FixedSizeBinary(width, Some(bytes)), None)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;

    use arrow::array::{Array, AsArray, RecordBatch};
    use datafusion::execution::runtime_env::RuntimeEnv;
    use llmobs_core::ids::TraceId;
    use llmobs_core::score::ScoreTarget;

    use crate::test_support::{
        RecordingTable, dataset_scores, dataset_spans, live_scores, live_scores_table, rows,
        spans_table, traces_table,
    };
    use crate::views::{ProjectQuery, QueryContext};

    struct Recorded {
        ctx: QueryContext,
        spans: Arc<RecordingTable>,
        traces: Arc<RecordingTable>,
        scores: Arc<RecordingTable>,
    }

    fn recording() -> Recorded {
        let spans = dataset_spans();
        let scores = dataset_scores();
        let span_table = RecordingTable::new(spans_table(&spans));
        let trace_table = RecordingTable::new(traces_table(&spans));
        let score_table = RecordingTable::new(live_scores_table(&scores));
        let ctx = QueryContext::new(
            ProjectQuery {
                spans: span_table.provider(),
                traces: trace_table.provider(),
                scores: score_table.provider(),
                runtime: Arc::new(RuntimeEnv::default()),
                target_partitions: NonZeroUsize::new(2).expect("two partitions"),
            },
            None,
        )
        .expect("context");
        Recorded {
            ctx,
            spans: span_table,
            traces: trace_table,
            scores: score_table,
        }
    }

    fn strings(batches: &[RecordBatch], column: &str) -> Vec<String> {
        let mut values = Vec::new();
        for batch in batches {
            let array = batch
                .column_by_name(column)
                .unwrap_or_else(|| panic!("missing {column}"))
                .as_string::<i32>();
            for row in 0..array.len() {
                assert!(!array.is_null(row), "{column} null");
                values.push(array.value(row).to_owned());
            }
        }
        values
    }

    fn assert_byte_filter(filters: &[String], prefix: &str) {
        assert!(
            filters.iter().any(|filter| filter.starts_with(prefix)),
            "expected a filter starting with {prefix:?}, got {filters:?}"
        );
        assert!(
            filters.iter().all(|filter| !filter.contains("encode(")),
            "filter still encodes: {filters:?}"
        );
    }

    #[tokio::test]
    async fn equality_pushes_trace_bytes() {
        let trace = "a1000000000000000000000000000001";
        let recorded = recording();
        let found = rows(
            &recorded.ctx,
            &format!("SELECT name FROM spans WHERE trace_id = '{trace}'"),
        )
        .await;
        assert_byte_filter(&recorded.spans.filters(), "trace_id = FixedSizeBinary(16");
        let expected: Vec<String> = dataset_spans()
            .iter()
            .filter(|span| span.trace_id.to_hex() == trace)
            .map(|span| span.name.clone())
            .collect();
        let mut names = strings(&found, "name");
        names.sort();
        let mut expected = expected;
        expected.sort();
        assert_eq!(names, expected);
    }

    #[tokio::test]
    async fn uppercase_hex_pushes_the_same_bytes() {
        let trace = "A1000000000000000000000000000001";
        let recorded = recording();
        let found = rows(
            &recorded.ctx,
            &format!("SELECT name FROM spans WHERE trace_id = '{trace}'"),
        )
        .await;
        assert_byte_filter(&recorded.spans.filters(), "trace_id = FixedSizeBinary(16");
        assert_eq!(
            strings(&found, "name").len(),
            dataset_spans()
                .iter()
                .filter(|span| span.trace_id.to_hex() == trace.to_ascii_lowercase())
                .count()
        );
    }

    #[tokio::test]
    async fn in_list_pushes_trace_bytes() {
        let ids: Vec<String> = dataset_spans()
            .iter()
            .map(|span| span.trace_id.to_hex())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .take(5)
            .collect();
        assert_eq!(ids.len(), 5);
        let list = ids
            .iter()
            .map(|id| format!("'{id}'"))
            .collect::<Vec<_>>()
            .join(", ");
        let recorded = recording();
        let found = rows(
            &recorded.ctx,
            &format!("SELECT trace_id FROM traces WHERE trace_id IN ({list})"),
        )
        .await;
        let filters = recorded.traces.filters();
        assert!(
            filters.iter().all(|filter| !filter.contains("encode(")),
            "{filters:?}"
        );
        assert!(
            filters
                .iter()
                .any(|filter| filter.contains("FixedSizeBinary")),
            "{filters:?}"
        );
        let mut actual = strings(&found, "trace_id");
        actual.sort();
        assert_eq!(actual, ids);
    }

    #[tokio::test]
    async fn span_parent_and_root_ids_are_rewritten() {
        let span = &dataset_spans()[0];
        let span_id = span.span_id.to_hex();
        let recorded = recording();
        let found = rows(
            &recorded.ctx,
            &format!("SELECT span_id FROM spans WHERE span_id = '{span_id}'"),
        )
        .await;
        assert_byte_filter(&recorded.spans.filters(), "span_id = FixedSizeBinary(8");
        assert_eq!(strings(&found, "span_id"), vec![span_id]);

        let spans = dataset_spans();
        let parent = spans
            .iter()
            .find_map(|span| span.parent_span_id)
            .expect("a child span")
            .to_hex();
        let recorded = recording();
        let _ = rows(
            &recorded.ctx,
            &format!("SELECT span_id FROM spans WHERE parent_span_id = '{parent}'"),
        )
        .await;
        assert_byte_filter(
            &recorded.spans.filters(),
            "parent_span_id = FixedSizeBinary(8",
        );

        let root = span.span_id.to_hex();
        let recorded = recording();
        let _ = rows(
            &recorded.ctx,
            &format!("SELECT trace_id FROM traces WHERE root_span_id = '{root}'"),
        )
        .await;
        assert_byte_filter(
            &recorded.traces.filters(),
            "root_span_id = FixedSizeBinary(8",
        );
    }

    #[tokio::test]
    async fn invalid_literals_stay_on_the_hex_string() {
        for literal in [
            "abc",
            "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
            "b100000000000011",
        ] {
            let recorded = recording();
            let found = rows(
                &recorded.ctx,
                &format!("SELECT trace_id FROM traces WHERE trace_id = '{literal}'"),
            )
            .await;
            let filters = recorded.traces.filters();
            assert!(
                filters.iter().any(|filter| filter.contains("encode(")),
                "{literal}: {filters:?}"
            );
            assert!(strings(&found, "trace_id").is_empty(), "{literal}");
        }
    }

    #[tokio::test]
    async fn mixed_in_list_is_not_rewritten() {
        let trace = "a1000000000000000000000000000001";
        let recorded = recording();
        let found = rows(
            &recorded.ctx,
            &format!("SELECT trace_id FROM traces WHERE trace_id IN ('{trace}', 'nope')"),
        )
        .await;
        let filters = recorded.traces.filters();
        assert!(
            filters.iter().any(|filter| filter.contains("encode(")),
            "{filters:?}"
        );
        assert_eq!(strings(&found, "trace_id"), vec![trace.to_owned()]);
    }

    #[tokio::test]
    async fn not_equal_excludes_one_trace() {
        let trace = "a1000000000000000000000000000001";
        let recorded = recording();
        let found = rows(
            &recorded.ctx,
            &format!("SELECT trace_id FROM traces WHERE trace_id <> '{trace}'"),
        )
        .await;
        assert_byte_filter(&recorded.traces.filters(), "trace_id != FixedSizeBinary(16");
        let ids = strings(&found, "trace_id");
        let all: std::collections::BTreeSet<String> = dataset_spans()
            .iter()
            .map(|span| span.trace_id.to_hex())
            .collect();
        assert_eq!(ids.len(), all.len() - 1);
        assert!(ids.iter().all(|id| id != trace));
    }

    #[tokio::test]
    async fn join_on_hex_ids_counts_matching_pairs() {
        let recorded = recording();
        let found = rows(
            &recorded.ctx,
            "SELECT count(*) AS n FROM spans s JOIN scores c ON s.trace_id = c.trace_id",
        )
        .await;
        let actual = i64_value(&found);
        let spans = dataset_spans();
        let scores = live_scores(&dataset_scores());
        let expected = spans
            .iter()
            .map(|span| {
                scores
                    .iter()
                    .filter(|score| score_trace(score) == Some(span.trace_id))
                    .count()
            })
            .sum::<usize>() as i64;
        assert_eq!(actual, expected);
        assert!(expected > 0);
    }

    #[tokio::test]
    async fn subquery_pushes_bytes_into_scores() {
        let trace = "a1000000000000000000000000000001";
        let recorded = recording();
        let found = rows(
            &recorded.ctx,
            &format!(
                "SELECT name FROM traces WHERE trace_id IN (SELECT trace_id FROM scores WHERE trace_id = '{trace}')"
            ),
        )
        .await;
        assert_byte_filter(&recorded.scores.filters(), "trace_id = FixedSizeBinary(16");
        let names = strings(&found, "name");
        assert_eq!(names.len(), 1);
        let expected = dataset_spans()
            .iter()
            .find(|span| span.trace_id == trace_id(trace) && span.parent_span_id.is_none())
            .expect("root")
            .name
            .clone();
        assert_eq!(names, vec![expected]);
    }

    fn i64_value(batches: &[RecordBatch]) -> i64 {
        let batch = batches.first().expect("a batch");
        batch
            .column(0)
            .as_primitive::<arrow::datatypes::Int64Type>()
            .value(0)
    }

    fn score_trace(score: &llmobs_core::score::Score) -> Option<TraceId> {
        match score.target {
            ScoreTarget::Span { trace_id, .. } | ScoreTarget::Trace { trace_id } => Some(trace_id),
            ScoreTarget::Session { .. } => None,
        }
    }

    fn trace_id(hex_id: &str) -> TraceId {
        let mut bytes = [0; 16];
        hex::decode_to_slice(hex_id, &mut bytes).expect("trace hex");
        TraceId::from_bytes(bytes).expect("trace id")
    }
}
