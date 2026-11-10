//! User-facing views over the stored providers (D91, D101).

mod hex;
mod scores;
mod session;
mod sessions;
mod spans;
mod traces;

use std::num::NonZeroUsize;
use std::sync::Arc;

use datafusion::arrow::datatypes::{DataType, Schema};
use datafusion::datasource::{TableProvider, ViewTable, provider_as_source};
use datafusion::error::DataFusionError;
use datafusion::execution::runtime_env::RuntimeEnv;
use datafusion::logical_expr::{Expr, LogicalPlan, LogicalPlanBuilder, col, lit};
use datafusion::prelude::{SessionContext, cast, encode};

use crate::query_error::RunError;

// Q6 reads this limit when it tokenizes caller text.
#[allow(unused_imports)]
pub(crate) use session::PARSER_RECURSION_LIMIT;

const DURATION_NS: &str = "duration_ns";
const DURATION_MS: &str = "duration_ms";

/// What `Storage::query` hands the query layer (D91).
///
/// A1 builds this. A2 never looks inside a provider.
#[derive(Debug)]
pub struct ProjectQuery {
    /// Stored `spans` schema, binary ids, `project_id` included.
    pub spans: Arc<dyn TableProvider>,
    /// Stored `traces` schema, binary ids, merge bookkeeping included.
    pub traces: Arc<dyn TableProvider>,
    /// Stored `scores` schema. Live rows only: last writer wins, tombstones dropped.
    pub scores: Arc<dyn TableProvider>,
    /// Per-query memory pool and spill directory.
    pub runtime: Arc<RuntimeEnv>,
    /// DataFusion `target_partitions` for this query.
    pub target_partitions: NonZeroUsize,
}

/// Inclusive `from`, exclusive `to`, nanoseconds since the Unix epoch, UTC (D101).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeWindow {
    from: i64,
    to: i64,
}

impl TimeWindow {
    /// Builds a window. `from` must be strictly before `to`.
    ///
    /// # Errors
    ///
    /// Returns [`RunError::InvalidRequest`] when `from >= to`.
    pub fn new(from: i64, to: i64) -> Result<Self, RunError> {
        if from < to {
            Ok(Self { from, to })
        } else {
            Err(RunError::InvalidRequest(
                "from must be before to".to_owned(),
            ))
        }
    }

    /// Inclusive start, nanoseconds.
    pub fn from(&self) -> i64 {
        self.from
    }

    /// Exclusive end, nanoseconds.
    pub fn to(&self) -> i64 {
        self.to
    }

    /// `column >= from AND column < to`, with timestamp-nanosecond literals.
    pub(crate) fn predicate(&self, column: &str) -> Expr {
        let lower = datafusion::common::ScalarValue::TimestampNanosecond(
            Some(self.from),
            Some("UTC".into()),
        );
        let upper =
            datafusion::common::ScalarValue::TimestampNanosecond(Some(self.to), Some("UTC".into()));
        col(column)
            .gt_eq(Expr::Literal(lower, None))
            .and(col(column).lt(Expr::Literal(upper, None)))
    }
}

/// A view the SQL sandbox can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ViewName {
    /// Merged traces.
    Traces,
    /// One row per span.
    Spans,
    /// One row per session. Registered by the sessions view.
    Sessions,
    /// Live scores.
    Scores,
    /// Server-built experiment rows.
    ExperimentItems,
    /// Server-built dataset rows.
    DatasetItems,
}

impl ViewName {
    /// Unqualified table name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Traces => "traces",
            Self::Spans => "spans",
            Self::Sessions => "sessions",
            Self::Scores => "scores",
            Self::ExperimentItems => "experiment_items",
            Self::DatasetItems => "dataset_items",
        }
    }
}

/// Sandboxed session with the base views registered.
pub struct QueryContext {
    /// Later modules plan against this session. Tests read it too.
    #[allow(dead_code)]
    session: SessionContext,
}

impl std::fmt::Debug for QueryContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QueryContext")
            .finish_non_exhaustive()
    }
}

impl QueryContext {
    /// Builds the session and registers `spans`, `traces`, `sessions` and `scores`.
    ///
    /// `window`, when set, is written into each base view's scan (D101). `sessions`
    /// is aggregated from the windowed `traces` view.
    ///
    /// # Errors
    ///
    /// Returns [`RunError::Internal`] when a view cannot be planned or registered.
    pub fn new(source: ProjectQuery, window: Option<TimeWindow>) -> Result<Self, RunError> {
        let state = session::session_state(source.runtime, source.target_partitions);
        let session = SessionContext::new_with_state(state);
        register(
            &session,
            ViewName::Spans,
            spans::spans_view(source.spans, window)?,
        )?;
        let traces_plan = traces::traces_view(source.traces, window)?;
        register(
            &session,
            ViewName::Sessions,
            map_plan(sessions::sessions_from_traces(traces_plan.clone()))?,
        )?;
        register(&session, ViewName::Traces, traces_plan)?;
        register(
            &session,
            ViewName::Scores,
            scores::scores_view(source.scores, window)?,
        )?;
        Ok(Self { session })
    }

    #[allow(dead_code)] // filter, SQL and read queries use this from Q6 on
    pub(crate) fn session(&self) -> &SessionContext {
        &self.session
    }
}

fn register(session: &SessionContext, name: ViewName, plan: LogicalPlan) -> Result<(), RunError> {
    session
        .register_table(name.as_str(), Arc::new(ViewTable::new(plan, None)))
        .map(|_| ())
        .map_err(RunError::Internal)
}

/// `CAST(duration_ns AS DOUBLE) / 1e6`.
pub(crate) fn duration_ms(duration_ns: Expr) -> Expr {
    cast(duration_ns, DataType::Float64) / lit(1_000_000.0_f64)
}

/// `CAST(end_time AS BIGINT) - CAST(start_time AS BIGINT)`.
pub(crate) fn duration_ns_from_bounds() -> Expr {
    cast(col(llmobs_core::schema::traces::END_TIME), DataType::Int64)
        - cast(
            col(llmobs_core::schema::traces::START_TIME),
            DataType::Int64,
        )
}

#[derive(Clone, Copy)]
pub(crate) struct ViewProjection<'a> {
    pub(crate) raw_name: &'a str,
    pub(crate) schema: &'a Schema,
    pub(crate) window_column: &'a str,
    pub(crate) skip: &'a [&'a str],
    pub(crate) hex: &'a [&'a str],
}

pub(crate) fn view_plan(
    provider: Arc<dyn TableProvider>,
    window: Option<TimeWindow>,
    projection: ViewProjection<'_>,
    extra_after: impl Fn(&str) -> Vec<Expr>,
) -> Result<LogicalPlan, DataFusionError> {
    let mut exprs = Vec::new();
    for field in projection.schema.fields() {
        let name = field.name().as_str();
        if projection.skip.contains(&name) {
            continue;
        }
        let expr = if projection.hex.contains(&name) {
            encode(col(name), lit("hex")).alias(name)
        } else {
            col(name)
        };
        exprs.push(expr);
        exprs.extend(extra_after(name));
    }
    let mut builder =
        LogicalPlanBuilder::scan(projection.raw_name, provider_as_source(provider), None)?;
    if let Some(window) = window {
        builder = builder.filter(window.predicate(projection.window_column))?;
    }
    builder.project(exprs)?.build()
}

pub(crate) fn map_plan(
    plan: Result<LogicalPlan, DataFusionError>,
) -> Result<LogicalPlan, RunError> {
    plan.map_err(RunError::Internal)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::{DateTime, SecondsFormat, Utc};
    use datafusion::arrow::array::{Array, AsArray, RecordBatch};
    use llmobs_core::time_serde::parse_rfc3339;
    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::{RecordingTable, dataset_spans, example_query, rows};

    fn rfc3339(ns: i64) -> String {
        let secs = ns.div_euclid(1_000_000_000);
        let nanos = ns.rem_euclid(1_000_000_000) as u32;
        DateTime::<Utc>::from_timestamp(secs, nanos)
            .expect("fixture timestamps fit chrono")
            .to_rfc3339_opts(SecondsFormat::Nanos, true)
    }

    fn context() -> QueryContext {
        QueryContext::new(example_query(), None).expect("example context")
    }

    fn same_columns(actual: &[String], expected: &[&str]) {
        let expected: Vec<String> = expected.iter().map(|name| (*name).to_owned()).collect();
        assert_eq!(actual, expected);
    }

    async fn field_names(ctx: &QueryContext, table: &str) -> Vec<String> {
        let frame = ctx.session().table(table).await.expect(table);
        frame
            .schema()
            .fields()
            .iter()
            .map(|field| field.name().to_owned())
            .collect()
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
        if let Some(values) = column.as_primitive_opt::<datafusion::arrow::datatypes::Int64Type>() {
            return values.value(row).into();
        }
        let values = column.as_primitive::<datafusion::arrow::datatypes::TimestampNanosecondType>();
        Some(values.value(row))
    }

    fn f64_cell(batch: &RecordBatch, row: usize, name: &str) -> Option<f64> {
        let column = batch.column_by_name(name).expect(name);
        if column.is_null(row) {
            return None;
        }
        Some(
            column
                .as_primitive::<datafusion::arrow::datatypes::Float64Type>()
                .value(row),
        )
    }

    fn list_cell(batch: &RecordBatch, row: usize, name: &str) -> Value {
        let column = batch.column_by_name(name).expect(name).as_list::<i32>();
        if column.is_null(row) {
            return Value::Null;
        }
        let values = column.value(row);
        let strings = values.as_string::<i32>();
        Value::Array(
            (0..strings.len())
                .map(|index| Value::String(strings.value(index).to_owned()))
                .collect(),
        )
    }

    fn trace_rows(batches: &[RecordBatch]) -> BTreeMap<String, Value> {
        let mut rows = BTreeMap::new();
        for batch in batches {
            for row in 0..batch.num_rows() {
                let id = string_cell(batch, row, "trace_id").expect("trace_id");
                let value = json!({
                    "start_time": rfc3339(i64_cell(batch, row, "start_time").expect("start")),
                    "end_time": rfc3339(i64_cell(batch, row, "end_time").expect("end")),
                    "name": string_cell(batch, row, "name"),
                    "status": string_cell(batch, row, "status"),
                    "span_count": i64_cell(batch, row, "span_count"),
                    "error_count": i64_cell(batch, row, "error_count"),
                    "total_tokens": i64_cell(batch, row, "total_tokens"),
                    "cost_total": f64_cell(batch, row, "cost_total"),
                    "tags": list_cell(batch, row, "tags"),
                    "models": list_cell(batch, row, "models"),
                });
                rows.insert(id, value);
            }
        }
        rows
    }

    #[tokio::test]
    async fn traces_table_agrees_with_the_fixture() {
        let ctx = context();
        let actual = trace_rows(&rows(&ctx, "SELECT * FROM traces").await);
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/api/responses/traces-include-eval.json"
        ))
        .expect("fixture json");
        let expected = fixture["data"].as_array().expect("data");
        assert_eq!(actual.len(), expected.len());
        for trace in expected {
            let id = trace["trace_id"].as_str().expect("id");
            let row = actual.get(id).unwrap_or_else(|| panic!("missing {id}"));
            for field in [
                "start_time",
                "end_time",
                "name",
                "status",
                "span_count",
                "error_count",
                "tags",
                "models",
            ] {
                assert_eq!(&row[field], &trace[field], "{id} {field}");
            }
            match (&row["total_tokens"], &trace["total_tokens"]) {
                (Value::Null, Value::Null) => {}
                (Value::Number(left), Value::Number(right)) => {
                    assert_eq!(left.as_i64(), right.as_i64(), "{id} total_tokens");
                }
                (left, right) => panic!("{id} total_tokens {left} vs {right}"),
            }
            match (&row["cost_total"], &trace["cost_total"]) {
                (Value::Null, Value::Null) => {}
                (Value::Number(left), Value::Number(right)) => {
                    let left = left.as_f64().expect("left cost");
                    let right = right.as_f64().expect("right cost");
                    assert!((left - right).abs() < 1e-12, "{id} cost {left} vs {right}");
                }
                (left, right) => panic!("{id} cost_total {left} vs {right}"),
            }
        }
    }

    #[tokio::test]
    async fn view_columns_follow_the_spec() {
        let ctx = context();
        let spans = field_names(&ctx, "spans").await;
        same_columns(
            &spans,
            &[
                "trace_id",
                "span_id",
                "parent_span_id",
                "name",
                "span_kind",
                "start_time",
                "end_time",
                "duration_ns",
                "duration_ms",
                "status",
                "status_message",
                "service_name",
                "session_id",
                "user_id",
                "environment",
                "release",
                "tags",
                "metadata",
                "provider",
                "request_model",
                "response_model",
                "model",
                "operation",
                "input_tokens",
                "output_tokens",
                "cache_read_tokens",
                "cache_write_tokens",
                "reasoning_tokens",
                "total_tokens",
                "cost_input",
                "cost_output",
                "cost_total",
                "cost_source",
                "ttft_ns",
                "request_params",
                "input",
                "input_format",
                "output",
                "output_format",
                "input_preview",
                "output_preview",
                "system_instructions",
                "tool_definitions",
                "attributes",
                "resource_attributes",
                "scope_name",
                "scope_version",
                "events",
                "links",
                "media_refs",
                "truncated",
                "conventions",
                "ingest_time",
                "schema_version",
            ],
        );
        let traces = field_names(&ctx, "traces").await;
        same_columns(
            &traces,
            &[
                "trace_id",
                "start_time",
                "end_time",
                "duration_ns",
                "duration_ms",
                "name",
                "root_span_id",
                "has_root",
                "status",
                "session_id",
                "user_id",
                "environment",
                "release",
                "tags",
                "metadata",
                "input_preview",
                "output_preview",
                "span_count",
                "error_count",
                "llm_count",
                "input_tokens",
                "output_tokens",
                "cache_read_tokens",
                "cache_write_tokens",
                "reasoning_tokens",
                "total_tokens",
                "cost_total",
                "models",
            ],
        );
        let sessions = field_names(&ctx, "sessions").await;
        same_columns(
            &sessions,
            &[
                "session_id",
                "start_time",
                "end_time",
                "duration_ns",
                "duration_ms",
                "trace_count",
                "error_count",
                "total_tokens",
                "cost_total",
                "user_id",
                "tags",
            ],
        );
        let scores = field_names(&ctx, "scores").await;
        same_columns(
            &scores,
            &[
                "score_id",
                "target",
                "trace_id",
                "span_id",
                "session_id",
                "user_id",
                "name",
                "data_type",
                "value_num",
                "value_str",
                "reason",
                "source",
                "evaluator_id",
                "rule_id",
                "experiment_id",
                "dataset_item_id",
                "author_user_id",
                "metadata",
                "created_at",
                "updated_at",
                "deleted",
                "ingest_time",
            ],
        );
    }

    #[tokio::test]
    async fn trace_one_ids_and_duration() {
        let ctx = context();
        let batches = rows(
            &ctx,
            "SELECT trace_id, root_span_id, duration_ns, duration_ms FROM traces WHERE name = 'support-agent'",
        )
        .await;
        let mut found = false;
        for batch in &batches {
            for row in 0..batch.num_rows() {
                if string_cell(batch, row, "trace_id").as_deref()
                    != Some("a1000000000000000000000000000001")
                {
                    continue;
                }
                found = true;
                assert_eq!(
                    string_cell(batch, row, "root_span_id").as_deref(),
                    Some("b100000000000011")
                );
                let duration_ns = i64_cell(batch, row, "duration_ns").expect("duration_ns");
                assert_eq!(duration_ns, 4_200_000_000);
                let duration_ms = f64_cell(batch, row, "duration_ms").expect("duration_ms");
                assert!((duration_ms - (duration_ns as f64 / 1e6)).abs() < 1e-9);
            }
        }
        assert!(found, "trace 1 is named support-agent");
    }

    #[tokio::test]
    async fn span_ids_are_lowercase_hex() {
        let ctx = context();
        let batches = rows(&ctx, "SELECT span_id, parent_span_id FROM spans").await;
        let mut saw_null_parent = false;
        for batch in &batches {
            for row in 0..batch.num_rows() {
                let span_id = string_cell(batch, row, "span_id").expect("span_id");
                assert_eq!(span_id.len(), 16, "{span_id}");
                assert!(
                    span_id
                        .chars()
                        .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
                );
                match string_cell(batch, row, "parent_span_id") {
                    None => saw_null_parent = true,
                    Some(parent) => {
                        assert_eq!(parent.len(), 16, "{parent}");
                        assert!(
                            parent
                                .chars()
                                .all(|ch| ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase())
                        );
                    }
                }
            }
        }
        assert!(saw_null_parent);
    }

    #[tokio::test]
    async fn score_count_matches_live_rows() {
        let ctx = context();
        let live = crate::test_support::live_scores(&crate::test_support::dataset_scores()).len();
        let batches = rows(&ctx, "SELECT count(*) AS n FROM scores").await;
        assert_eq!(i64_cell(&batches[0], 0, "n"), Some(live as i64));
    }

    #[tokio::test]
    async fn time_window_filters_every_scan() {
        let from = parse_rfc3339("2026-01-01T10:05:00Z").expect("from");
        let to = parse_rfc3339("2026-01-01T10:20:00Z").expect("to");
        let window = TimeWindow::new(from, to).expect("window");
        let ctx = QueryContext::new(example_query(), Some(window)).expect("windowed");

        let traces = rows(&ctx, "SELECT trace_id, start_time FROM traces").await;
        let mut ids = Vec::new();
        for batch in &traces {
            for row in 0..batch.num_rows() {
                let start = i64_cell(batch, row, "start_time").expect("start");
                assert!((from..to).contains(&start), "{start}");
                ids.push(string_cell(batch, row, "trace_id").expect("id"));
            }
        }
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/api/responses/traces-include-eval.json"
        ))
        .expect("fixture");
        let expected: Vec<_> = fixture["data"]
            .as_array()
            .expect("data")
            .iter()
            .filter(|trace| {
                let start =
                    parse_rfc3339(trace["start_time"].as_str().expect("start")).expect("ns");
                (from..to).contains(&start)
            })
            .map(|trace| trace["trace_id"].as_str().expect("id").to_owned())
            .collect();
        ids.sort();
        let mut expected = expected;
        expected.sort();
        assert_eq!(ids, expected);
        assert!(
            expected
                .iter()
                .all(|id| id != "a1000000000000000000000000000004")
        );

        let spans = dataset_spans();
        let span_count = spans
            .iter()
            .filter(|span| (from..to).contains(&span.start_time))
            .count();
        let counted = rows(&ctx, "SELECT count(*) AS n FROM spans").await;
        assert_eq!(i64_cell(&counted[0], 0, "n"), Some(span_count as i64));

        let scores = crate::test_support::live_scores(&crate::test_support::dataset_scores());
        let score_count = scores
            .iter()
            .filter(|score| (from..to).contains(&score.created_at))
            .count();
        let counted = rows(&ctx, "SELECT count(*) AS n FROM scores").await;
        assert_eq!(i64_cell(&counted[0], 0, "n"), Some(score_count as i64));

        let spans_log = RecordingTable::new(crate::test_support::spans_table(&spans));
        let traces_table = crate::test_support::traces_table(&spans);
        let traces_log = RecordingTable::new(traces_table);
        let scores_table = crate::test_support::live_scores_table(&scores);
        let recorded = QueryContext::new(
            ProjectQuery {
                spans: spans_log.provider(),
                traces: traces_log.provider(),
                scores: scores_table,
                runtime: Arc::new(RuntimeEnv::default()),
                target_partitions: NonZeroUsize::new(2).expect("partitions"),
            },
            Some(window),
        )
        .expect("recorded");
        let from_text = from.to_string();
        let to_text = to.to_string();
        // DataFusion pushes the two comparisons as separate filters.
        let assert_bounds = |filters: &[String], scans: usize| {
            let lowers = filters
                .iter()
                .filter(|filter| filter.contains(&from_text))
                .count();
            let uppers = filters
                .iter()
                .filter(|filter| filter.contains(&to_text))
                .count();
            assert!(lowers >= scans && uppers >= scans, "{filters:?}");
        };

        rows(
            &recorded,
            "SELECT count(*) FROM spans a JOIN spans b ON a.span_id = b.span_id",
        )
        .await;
        assert_bounds(&spans_log.filters(), 2);

        spans_log.clear();
        rows(
            &recorded,
            "SELECT count(*) FROM traces WHERE trace_id IN (SELECT trace_id FROM spans)",
        )
        .await;
        assert_bounds(&spans_log.filters(), 1);
        assert_bounds(&traces_log.filters(), 1);

        spans_log.clear();
        rows(
            &recorded,
            "WITH x AS (SELECT * FROM spans) SELECT count(*) FROM x",
        )
        .await;
        assert_bounds(&spans_log.filters(), 1);

        assert!(matches!(
            TimeWindow::new(5, 5),
            Err(RunError::InvalidRequest(_))
        ));
        assert!(matches!(
            TimeWindow::new(6, 5),
            Err(RunError::InvalidRequest(_))
        ));
    }
}
