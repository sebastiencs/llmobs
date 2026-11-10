//! The `sessions` view, one row per `session_id` aggregated from `traces`.

use arrow::datatypes::DataType;
use datafusion::common::ScalarValue;
use datafusion::error::DataFusionError;
use datafusion::functions::expr_fn::coalesce;
use datafusion::functions_aggregate::expr_fn::{array_agg, count, max, min, sum};
use datafusion::functions_aggregate::first_last::first_value_udaf;
use datafusion::functions_nested::expr_fn::{array_distinct, array_sort, flatten};
use datafusion::logical_expr::expr::NullTreatment;
use datafusion::logical_expr::{Expr, ExprFunctionExt, LogicalPlan, LogicalPlanBuilder, col, lit};
use llmobs_core::schema::traces;

use super::{DURATION_MS, DURATION_NS, duration_ms, duration_ns_from_bounds};

const TRACE_COUNT: &str = "trace_count";

/// Aggregate a `traces` view plan into the `sessions` columns.
///
/// # Errors
///
/// Returns a [`DataFusionError`] when the aggregate or projection cannot be built.
pub(crate) fn sessions_from_traces(
    traces_plan: LogicalPlan,
) -> Result<LogicalPlan, DataFusionError> {
    let grouped = LogicalPlanBuilder::new(traces_plan)
        .filter(col(traces::SESSION_ID).is_not_null())?
        .aggregate(
            vec![col(traces::SESSION_ID)],
            vec![
                min(col(traces::START_TIME)).alias(traces::START_TIME),
                max(col(traces::END_TIME)).alias(traces::END_TIME),
                count(lit(1_i64)).alias(TRACE_COUNT),
                sum(col(traces::ERROR_COUNT)).alias(traces::ERROR_COUNT),
                sum(col(traces::TOTAL_TOKENS)).alias(traces::TOTAL_TOKENS),
                sum(col(traces::COST_TOTAL)).alias(traces::COST_TOTAL),
                user_id()?.alias(traces::USER_ID),
                array_agg(col(traces::TAGS)).alias(traces::TAGS),
            ],
        )?
        .project(vec![
            // The filter drops null session ids. `coalesce` only tells the schema
            // the column is non-null; the fallback is unused.
            coalesce(vec![col(traces::SESSION_ID), lit("")]).alias(traces::SESSION_ID),
            coalesce(vec![col(traces::START_TIME), epoch()]).alias(traces::START_TIME),
            coalesce(vec![col(traces::END_TIME), epoch()]).alias(traces::END_TIME),
            col(TRACE_COUNT),
            // `sum` is nullable for an empty group. A session group is never empty.
            coalesce(vec![col(traces::ERROR_COUNT), lit(0_i64)]).alias(traces::ERROR_COUNT),
            col(traces::TOTAL_TOKENS),
            col(traces::COST_TOTAL),
            col(traces::USER_ID),
            coalesce(vec![sorted_tags(), empty_tags()]).alias(traces::TAGS),
        ])?
        .build()?;

    let duration_ns = duration_ns_from_bounds();
    LogicalPlanBuilder::new(grouped)
        .project(vec![
            col(traces::SESSION_ID),
            col(traces::START_TIME),
            col(traces::END_TIME),
            duration_ns.clone().alias(DURATION_NS),
            duration_ms(duration_ns).alias(DURATION_MS),
            col(TRACE_COUNT),
            col(traces::ERROR_COUNT),
            col(traces::TOTAL_TOKENS),
            col(traces::COST_TOTAL),
            col(traces::USER_ID),
            col(traces::TAGS),
        ])?
        .build()
}

/// First non-null `user_id` by `(start_time, trace_id)`.
fn user_id() -> Result<Expr, DataFusionError> {
    first_value_udaf()
        .call(vec![col(traces::USER_ID)])
        .order_by(vec![
            col(traces::START_TIME).sort(true, true),
            col(traces::TRACE_ID).sort(true, true),
        ])
        .null_treatment(NullTreatment::IgnoreNulls)
        .build()
}

fn sorted_tags() -> Expr {
    array_sort(
        array_distinct(flatten(col(traces::TAGS))),
        lit("ASC"),
        lit("NULLS LAST"),
    )
}

fn empty_tags() -> Expr {
    lit(ScalarValue::List(ScalarValue::new_list(
        &[],
        &DataType::Utf8,
        false,
    )))
}

fn epoch() -> Expr {
    Expr::Literal(
        ScalarValue::TimestampNanosecond(Some(0), Some("UTC".into())),
        None,
    )
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::datatypes::{DataType, Field, TimeUnit};
    use chrono::{DateTime, SecondsFormat, Utc};
    use datafusion::arrow::array::{Array, AsArray, RecordBatch};
    use datafusion::execution::runtime_env::RuntimeEnv;
    use llmobs_core::ids::{SpanId, TraceId};
    use llmobs_core::span::Span;
    use serde_json::Value;

    use crate::test_support::{
        dataset_scores, dataset_spans, example_query, live_scores_table, rows, spans_table,
        traces_table,
    };
    use crate::views::{ProjectQuery, QueryContext};

    fn rfc3339(ns: i64) -> String {
        let secs = ns.div_euclid(1_000_000_000);
        let nanos = ns.rem_euclid(1_000_000_000) as u32;
        DateTime::<Utc>::from_timestamp(secs, nanos)
            .expect("timestamp fits chrono")
            .to_rfc3339_opts(SecondsFormat::Nanos, true)
    }

    fn context() -> QueryContext {
        QueryContext::new(example_query(), None).expect("example context")
    }

    async fn batches() -> Vec<RecordBatch> {
        let ctx = context();
        rows(&ctx, "SELECT * FROM sessions ORDER BY session_id").await
    }

    fn string_cell(batch: &RecordBatch, row: usize, name: &str) -> Option<String> {
        let column = batch.column_by_name(name).expect(name);
        if column.is_null(row) {
            return None;
        }
        Some(column.as_string::<i32>().value(row).to_owned())
    }

    fn i64_cell(batch: &RecordBatch, row: usize, name: &str) -> Option<i64> {
        let column = batch.column_by_name(name).expect(name);
        if column.is_null(row) {
            return None;
        }
        if let Some(values) = column.as_primitive_opt::<arrow::datatypes::Int64Type>() {
            return Some(values.value(row));
        }
        let values = column.as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        Some(values.value(row))
    }

    fn f64_cell(batch: &RecordBatch, row: usize, name: &str) -> Option<f64> {
        let column = batch.column_by_name(name).expect(name);
        if column.is_null(row) {
            return None;
        }
        Some(
            column
                .as_primitive::<arrow::datatypes::Float64Type>()
                .value(row),
        )
    }

    fn list_cell(batch: &RecordBatch, row: usize, name: &str) -> Vec<String> {
        let column = batch.column_by_name(name).expect(name).as_list::<i32>();
        assert!(!column.is_null(row), "{name} is null");
        let values = column.value(row);
        let strings = values.as_string::<i32>();
        (0..strings.len())
            .map(|index| strings.value(index).to_owned())
            .collect()
    }

    #[tokio::test]
    async fn sessions_match_the_fixture() {
        let actual = batches().await;
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/api/responses/sessions-include-eval.json"
        ))
        .expect("sessions fixture");
        let expected = fixture["data"].as_array().expect("data");
        let mut seen = 0;
        for batch in &actual {
            for row in 0..batch.num_rows() {
                let id = string_cell(batch, row, "session_id").expect("session_id");
                let session = expected
                    .iter()
                    .find(|session| session["session_id"] == id)
                    .unwrap_or_else(|| panic!("missing {id}"));
                assert_eq!(
                    rfc3339(i64_cell(batch, row, "start_time").expect("start")),
                    session["start_time"].as_str().expect("start")
                );
                assert_eq!(
                    rfc3339(i64_cell(batch, row, "end_time").expect("end")),
                    session["end_time"].as_str().expect("end")
                );
                let duration_ns = i64_cell(batch, row, "duration_ns").expect("duration_ns");
                assert_eq!(
                    duration_ns,
                    session["duration_ns"].as_i64().expect("duration")
                );
                let duration_ms = f64_cell(batch, row, "duration_ms").expect("duration_ms");
                assert!((duration_ms - duration_ns as f64 / 1e6).abs() < 1e-9);
                assert_eq!(
                    i64_cell(batch, row, "trace_count"),
                    session["trace_count"].as_i64()
                );
                assert_eq!(
                    i64_cell(batch, row, "error_count"),
                    session["error_count"].as_i64()
                );
                assert_eq!(
                    i64_cell(batch, row, "total_tokens"),
                    session["total_tokens"].as_i64()
                );
                match (
                    f64_cell(batch, row, "cost_total"),
                    session["cost_total"].as_f64(),
                ) {
                    (None, None) => {}
                    (Some(left), Some(right)) => {
                        assert!((left - right).abs() < 1e-12, "{id} cost {left} vs {right}");
                    }
                    (left, right) => panic!("{id} cost_total {left:?} vs {right:?}"),
                }
                assert_eq!(
                    string_cell(batch, row, "user_id").as_deref(),
                    session["user_id"].as_str()
                );
                let tags: Vec<Value> = list_cell(batch, row, "tags")
                    .into_iter()
                    .map(Value::String)
                    .collect();
                assert_eq!(Value::Array(tags), session["tags"]);
                seen += 1;
            }
        }
        assert_eq!(seen, expected.len());
    }

    #[tokio::test]
    async fn session_rules_skip_null_user_and_null_session() {
        let template = dataset_spans().into_iter().next().expect("a span");
        let spans = vec![
            session_span(&template, 1, 1_000, Some("s1"), None, &["b", "a"]),
            session_span(&template, 2, 2_000, Some("s1"), Some("u2"), &[]),
            session_span(&template, 3, 3_000, Some("s1"), Some("u9"), &["a"]),
            session_span(&template, 4, 4_000, None, Some("other"), &["z"]),
        ];
        let ctx = QueryContext::new(query_for(&spans), None).expect("context");
        let found = rows(&ctx, "SELECT * FROM sessions").await;
        assert_eq!(found.iter().map(RecordBatch::num_rows).sum::<usize>(), 1);
        let batch = &found[0];
        assert_eq!(string_cell(batch, 0, "user_id").as_deref(), Some("u2"));
        assert_eq!(list_cell(batch, 0, "tags"), ["a", "b"]);
        assert_eq!(i64_cell(batch, 0, "total_tokens"), None);
        assert_eq!(i64_cell(batch, 0, "trace_count"), Some(3));
        assert_eq!(string_cell(batch, 0, "session_id").as_deref(), Some("s1"));
    }

    #[tokio::test]
    async fn session_schema_nullability() {
        let ctx = context();
        let frame = ctx.session().table("sessions").await.expect("sessions");
        let schema = frame.schema();
        let field = |name: &str| schema.field_with_name(None, name).expect(name).clone();
        let timestamp = DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()));
        let tags = DataType::List(Arc::new(Field::new("item", DataType::Utf8, false)));
        let expected = [
            ("session_id", DataType::Utf8, false),
            ("start_time", timestamp.clone(), false),
            ("end_time", timestamp, false),
            ("duration_ns", DataType::Int64, false),
            ("duration_ms", DataType::Float64, false),
            ("trace_count", DataType::Int64, false),
            ("error_count", DataType::Int64, false),
            ("total_tokens", DataType::Int64, true),
            ("cost_total", DataType::Float64, true),
            ("user_id", DataType::Utf8, true),
            ("tags", tags, false),
        ];
        assert_eq!(schema.fields().len(), expected.len());
        for (name, data_type, nullable) in expected {
            let column = field(name);
            assert_eq!(column.data_type(), &data_type, "{name}");
            assert_eq!(column.is_nullable(), nullable, "{name}");
        }
    }

    fn query_for(spans: &[Span]) -> ProjectQuery {
        ProjectQuery {
            spans: spans_table(spans),
            traces: traces_table(spans),
            scores: live_scores_table(&dataset_scores()),
            runtime: Arc::new(RuntimeEnv::default()),
            target_partitions: std::num::NonZeroUsize::new(2).expect("two partitions"),
        }
    }

    fn session_span(
        template: &Span,
        nth: u8,
        start: i64,
        session_id: Option<&str>,
        user_id: Option<&str>,
        tags: &[&str],
    ) -> Span {
        let mut span = template.clone();
        span.trace_id = trace_id(nth);
        span.span_id = span_id(nth);
        span.parent_span_id = None;
        span.start_time = start;
        span.end_time = start + 1;
        span.session_id = session_id.map(str::to_owned);
        span.user_id = user_id.map(str::to_owned);
        span.tags = tags.iter().map(|tag| (*tag).to_owned()).collect();
        span.total_tokens = None;
        span.input_tokens = None;
        span.output_tokens = None;
        span.cache_read_tokens = None;
        span.cache_write_tokens = None;
        span.reasoning_tokens = None;
        span.cost_total = None;
        span
    }

    fn trace_id(nth: u8) -> TraceId {
        let mut bytes = [0; 16];
        bytes[15] = nth;
        TraceId::from_bytes(bytes).expect("non-zero trace id")
    }

    fn span_id(nth: u8) -> SpanId {
        let mut bytes = [0; 8];
        bytes[7] = nth;
        SpanId::from_bytes(bytes).expect("non-zero span id")
    }
}
