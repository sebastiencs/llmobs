//! Run a console statement and return D98's JSON result.

use std::time::Duration;

use arrow::datatypes::Schema;
use arrow::record_batch::RecordBatch;
use datafusion::dataframe::DataFrame;
use datafusion::physical_plan::common::collect;
use serde_json::{Map, Value};

use super::encode::json_rows;
use super::prepare_sql;
use crate::query_error::{RunError, classify};
use crate::views::QueryContext;

/// One console request.
///
/// `limit` is the JSON row cap. Exports ignore it (Q13); the server passes
/// [`SQL_MAX_ROWS`] there.
#[derive(Debug)]
pub struct SqlRequest<'a> {
    /// The statement, with `{name:Type}` placeholders.
    pub query: &'a str,
    /// JSON values for the placeholders.
    pub params: &'a Map<String, Value>,
    /// Maximum rows to return. `1..=SQL_MAX_ROWS`.
    pub limit: usize,
    /// How long the query may run. Zero is rejected here; the server checks the
    /// spec 09 range.
    pub timeout: Duration,
}

/// One output column. `data_type` is the Arrow type as DataFusion prints it.
#[derive(Debug)]
pub struct SqlColumn {
    /// Output name.
    pub name: String,
    /// Arrow type display.
    pub data_type: String,
}

/// JSON result (D98).
#[derive(Debug)]
pub struct SqlJson {
    /// Output columns, in order.
    pub columns: Vec<SqlColumn>,
    /// One object per row. Keys follow [`Self::columns`].
    pub rows: Vec<Map<String, Value>>,
    /// True when more than `limit` rows matched. `rows` then holds the first `limit`.
    pub truncated: bool,
}

/// Most rows a JSON console response returns.
pub const SQL_MAX_ROWS: usize = 10_000;

impl QueryContext {
    /// Run `request.query` and encode at most `request.limit` rows.
    ///
    /// The timeout wraps planning and execution. Dropping that future drops the
    /// result stream, which cancels DataFusion's tasks.
    ///
    /// # Errors
    ///
    /// [`RunError::InvalidRequest`] for a limit outside `1..=SQL_MAX_ROWS` or a
    /// zero timeout. Planning and parameter errors pass through. Execution
    /// errors are classified. The time limit yields [`RunError::Timeout`].
    pub async fn sql_json(&self, request: SqlRequest<'_>) -> Result<SqlJson, RunError> {
        if !(1..=SQL_MAX_ROWS).contains(&request.limit) {
            return Err(RunError::InvalidRequest(
                "limit must be between 1 and 10000".to_owned(),
            ));
        }
        if request.timeout.is_zero() {
            return Err(RunError::InvalidRequest(
                "timeout must be greater than zero".to_owned(),
            ));
        }
        match tokio::time::timeout(request.timeout, self.execute(request)).await {
            Err(_) => Err(RunError::Timeout),
            Ok(result) => result,
        }
    }

    async fn execute(&self, request: SqlRequest<'_>) -> Result<SqlJson, RunError> {
        let plan = prepare_sql(self, request.query, request.params).await?;
        let frame = DataFrame::new(self.session().state(), plan)
            .limit(0, Some(request.limit + 1))
            .map_err(classify)?;
        let columns = frame
            .schema()
            .fields()
            .iter()
            .map(|field| SqlColumn {
                name: field.name().to_owned(),
                data_type: field.data_type().to_string(),
            })
            .collect();
        let schema = Schema::new(frame.schema().fields().clone());
        // The plan's limit stops the stream at `limit + 1` rows. `collect` pulls
        // that stream; the caller's timeout drops it, which cancels the query.
        let stream = frame.execute_stream().await.map_err(classify)?;
        let batches = collect(stream).await.map_err(classify)?;
        let (batches, truncated) = cap_rows(batches, request.limit);
        let rows = json_rows(&batches, &schema)?;
        Ok(SqlJson {
            columns,
            rows,
            truncated,
        })
    }
}

fn cap_rows(batches: Vec<RecordBatch>, limit: usize) -> (Vec<RecordBatch>, bool) {
    let total: usize = batches.iter().map(RecordBatch::num_rows).sum();
    if total <= limit {
        return (batches, false);
    }
    let mut kept = Vec::new();
    let mut remaining = limit;
    for batch in batches {
        if remaining == 0 {
            break;
        }
        if batch.num_rows() <= remaining {
            remaining -= batch.num_rows();
            kept.push(batch);
        } else {
            kept.push(batch.slice(0, remaining));
            remaining = 0;
        }
    }
    (kept, true)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::query_error::RunError;
    use crate::test_support::{dataset_spans, example_query};
    use crate::views::{QueryContext, SqliteView, TimeWindow};
    use datafusion::arrow::array::Int64Array;
    use datafusion::arrow::datatypes::{DataType, Field, Schema};
    use datafusion::arrow::record_batch::RecordBatch;
    use datafusion::common::Diagnostic;
    use datafusion::datasource::TableProvider;
    use datafusion::datasource::memory::MemTable;
    use datafusion::error::DataFusionError;
    use datafusion::execution::runtime_env::{RuntimeEnv, RuntimeEnvBuilder};
    use llmobs_core::time_serde::parse_rfc3339;
    use serde_json::Value;

    fn context() -> QueryContext {
        QueryContext::new(example_query(), None).expect("example project")
    }

    fn empty_params() -> Map<String, Value> {
        Map::new()
    }

    fn request<'a>(query: &'a str, params: &'a Map<String, Value>, limit: usize) -> SqlRequest<'a> {
        SqlRequest {
            query,
            params,
            limit,
            timeout: Duration::from_secs(30),
        }
    }

    async fn run(query: &str, limit: usize) -> SqlJson {
        context()
            .sql_json(request(query, &empty_params(), limit))
            .await
            .unwrap_or_else(|error| panic!("{query}: {error}"))
    }

    fn numbers_match(left: &Value, right: &Value) -> bool {
        match (left, right) {
            (Value::Null, Value::Null) => true,
            (Value::Number(left), Value::Number(right)) => match (left.as_i64(), right.as_i64()) {
                (Some(left), Some(right)) => left == right,
                _ => {
                    let left = left.as_f64().expect("left");
                    let right = right.as_f64().expect("right");
                    (left - right).abs() < 1e-12
                }
            },
            _ => left == right,
        }
    }

    #[tokio::test]
    async fn json_rows_match_the_trace_fixture() {
        let result = run(
            "SELECT trace_id, name, start_time, tags, metadata, cost_total \
             FROM traces ORDER BY start_time DESC LIMIT 3",
            10,
        )
        .await;
        assert!(!result.truncated);
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/api/responses/traces.json"
        ))
        .expect("fixture");
        let expected = fixture["data"].as_array().expect("data");
        assert_eq!(result.rows.len(), 3);
        let fields = [
            "trace_id",
            "name",
            "start_time",
            "tags",
            "metadata",
            "cost_total",
        ];
        for (row, trace) in result.rows.iter().zip(expected) {
            let keys: Vec<_> = row.keys().cloned().collect();
            assert_eq!(keys, fields);
            for field in fields {
                let actual = &row[field];
                let expected = &trace[field];
                assert!(
                    numbers_match(actual, expected),
                    "{field}: {actual} vs {expected}"
                );
            }
            let start = row["start_time"].as_str().expect("start");
            assert!(start.ends_with('Z'), "{start}");
            assert!(start.contains('.'), "{start}");
            assert_eq!(
                start.split_once('.').expect("fraction").1.len(),
                10,
                "{start}"
            );
        }
        let types: Vec<_> = result
            .columns
            .iter()
            .map(|column| format!("{} {}", column.name, column.data_type))
            .collect::<Vec<_>>();
        assert_eq!(
            types,
            [
                "trace_id Utf8",
                "name Utf8",
                "start_time Timestamp(ns, \"UTC\")",
                "tags List(non-null Utf8)",
                "metadata Map(\"entries\": non-null Struct(\"keys\": non-null Utf8, \"values\": Utf8), unsorted)",
                "cost_total Float64",
            ]
        );
    }

    #[tokio::test]
    async fn the_row_cap_sets_truncated() {
        let query = "SELECT trace_id FROM traces ORDER BY start_time LIMIT 5";
        let two = run(query, 2).await;
        assert_eq!(two.rows.len(), 2);
        assert!(two.truncated);
        let five = run(query, 5).await;
        assert_eq!(five.rows.len(), 5);
        assert!(!five.truncated);
    }

    #[tokio::test]
    async fn a_limit_outside_the_range_is_rejected() {
        for limit in [0, 10_001] {
            match context()
                .sql_json(request("SELECT 1", &empty_params(), limit))
                .await
            {
                Err(RunError::InvalidRequest(message)) => {
                    assert_eq!(message, "limit must be between 1 and 10000");
                }
                other => panic!("limit {limit}: {other:?}"),
            }
        }
        let params = empty_params();
        let mut zero = request("SELECT 1", &params, 1);
        zero.timeout = Duration::ZERO;
        match context().sql_json(zero).await {
            Err(RunError::InvalidRequest(message)) => {
                assert_eq!(message, "timeout must be greater than zero");
            }
            other => panic!("zero timeout: {other:?}"),
        }
    }

    #[tokio::test]
    async fn special_values_follow_the_json_rules() {
        let result = run(
            "SELECT CAST('NaN' AS DOUBLE) AS x, X'00ff' AS b, CAST(1 AS TIMESTAMP) AS naive",
            10,
        )
        .await;
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0]["x"], Value::Null);
        assert_eq!(result.rows[0]["b"], Value::String("00ff".to_owned()));
        let naive = result.rows[0]["naive"].as_str().expect("naive");
        assert!(!naive.contains('Z'), "{naive}");
        assert!(naive.contains('T'), "{naive}");
    }

    #[tokio::test]
    async fn division_by_zero_is_a_failed_query() {
        let error = context()
            .sql_json(request("SELECT 1 / 0", &empty_params(), 10))
            .await
            .expect_err("divide");
        match error {
            RunError::Failed(message) => {
                assert!(message.to_ascii_lowercase().contains("zero"), "{message}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn an_oversized_regex_is_a_failed_query() {
        let error = context()
            .sql_json(request(
                "SELECT regexp_like(name, '(((a{100}){100}){100})') FROM spans",
                &empty_params(),
                10,
            ))
            .await
            .expect_err("regex");
        match error {
            RunError::Failed(message) => assert!(message.contains("CompiledTooBig"), "{message}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    fn integers(rows: usize) -> Arc<dyn TableProvider> {
        let values: Vec<i64> = (0..rows as i64).collect();
        let schema = Arc::new(Schema::new(vec![Field::new("v", DataType::Int64, false)]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![Arc::new(Int64Array::from(values))],
        )
        .expect("batch");
        Arc::new(MemTable::try_new(schema, vec![vec![batch]]).expect("table"))
    }

    fn runtime(bytes: usize) -> Arc<RuntimeEnv> {
        Arc::new(
            RuntimeEnvBuilder::new()
                .with_memory_limit(bytes, 1.0)
                .build()
                .expect("runtime"),
        )
    }

    fn context_with(runtime: Arc<RuntimeEnv>) -> QueryContext {
        let mut source = example_query();
        source.runtime = runtime;
        QueryContext::new(source, None).expect("context")
    }

    #[tokio::test]
    async fn a_sort_past_the_memory_limit_is_too_large() {
        let ctx = context_with(runtime(1 << 20));
        ctx.register_sqlite_view(SqliteView::ExperimentItems, integers(200_000))
            .expect("register");
        let error = ctx
            .sql_json(request(
                "SELECT v FROM experiment_items ORDER BY v",
                &empty_params(),
                10,
            ))
            .await
            .expect_err("memory");
        assert!(matches!(error, RunError::TooLarge), "{error:?}");
    }

    #[tokio::test]
    async fn a_cross_join_hits_the_timeout() {
        let ctx = context();
        ctx.register_sqlite_view(SqliteView::ExperimentItems, integers(20_000))
            .expect("register");
        let started = Instant::now();
        let error = ctx
            .sql_json(SqlRequest {
                // count(*) of this join is the product of the table sizes, so it
                // never runs. The sum has to read every pair.
                query: "SELECT sum(a.v + b.v) FROM experiment_items a, experiment_items b",
                params: &empty_params(),
                limit: 10,
                timeout: Duration::from_millis(50),
            })
            .await
            .expect_err("timeout");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "returned in {:?}",
            started.elapsed()
        );
        assert!(matches!(error, RunError::Timeout), "{error:?}");
    }

    #[tokio::test]
    async fn cancelling_a_query_releases_its_memory() {
        let runtime = runtime(1 << 30);
        let ctx = context_with(Arc::clone(&runtime));
        ctx.register_sqlite_view(SqliteView::ExperimentItems, integers(20_000))
            .expect("register");
        let error = ctx
            .sql_json(SqlRequest {
                // `a.v, b.v` is rejected: output names are unique (Q10). The row cap
                // turns a plain ORDER BY into a top-k, which finishes immediately,
                // so the window has to read the whole join.
                query: "SELECT a_v, sum(b_v) OVER () FROM ( \
                        SELECT a.v AS a_v, b.v AS b_v \
                        FROM experiment_items a, experiment_items b)",
                params: &empty_params(),
                limit: 10,
                timeout: Duration::from_millis(50),
            })
            .await
            .expect_err("cancel");
        assert!(matches!(error, RunError::Timeout), "{error:?}");
        let started = Instant::now();
        loop {
            if runtime.memory_pool.reserved() == 0 {
                break;
            }
            if started.elapsed() > Duration::from_secs(10) {
                panic!("reserved {}", runtime.memory_pool.reserved());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn wrappers_are_classified_by_the_root() {
        let planned = DataFusionError::Plan("bad plan".into())
            .with_diagnostic(Diagnostic::new_error("planned badly", None));
        match classify(planned) {
            RunError::Invalid(error) => assert_eq!(error.message, "planned badly"),
            other => panic!("diagnostic: {other:?}"),
        }
        let exhausted = DataFusionError::ResourcesExhausted("full".into()).context("sorting");
        assert!(matches!(classify(exhausted), RunError::TooLarge));
        let shared =
            DataFusionError::Shared(Arc::new(DataFusionError::ResourcesExhausted("full".into())));
        assert!(matches!(classify(shared), RunError::TooLarge));
    }

    #[tokio::test]
    async fn a_statement_that_is_not_a_query_stays_invalid() {
        let error = context()
            .sql_json(request("INSERT INTO spans VALUES (1)", &empty_params(), 10))
            .await
            .expect_err("insert");
        match error {
            RunError::Invalid(error) => {
                assert_eq!(error.message, "only SELECT queries are allowed");
                assert_eq!(error.position.map(|position| position.column), Some(1));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_missing_parameter_is_an_invalid_request() {
        let error = context()
            .sql_json(request("SELECT {n:String} AS s", &empty_params(), 10))
            .await
            .expect_err("missing");
        match error {
            RunError::InvalidRequest(message) => {
                assert_eq!(message, "missing value for parameter n");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn a_time_window_limits_the_span_count() {
        let from = parse_rfc3339("2026-01-01T10:05:00Z").expect("from");
        let to = parse_rfc3339("2026-01-01T10:20:00Z").expect("to");
        let window = TimeWindow::new(from, to).expect("window");
        let ctx = QueryContext::new(example_query(), Some(window)).expect("windowed");
        let result = ctx
            .sql_json(request("SELECT count(*) FROM spans", &empty_params(), 10))
            .await
            .expect("count");
        let expected = dataset_spans()
            .iter()
            .filter(|span| (from..to).contains(&span.start_time))
            .count();
        let count = result.rows[0]
            .values()
            .next()
            .expect("column")
            .as_i64()
            .expect("count");
        assert_eq!(count, expected as i64);
        assert!(count > 0);
        assert!(count < dataset_spans().len() as i64);
    }
}
