//! SQL console: validate, plan, bind, run and encode.
//!
//! `{name:Type}` placeholders are bound before planning. [`QueryContext::sql_json`]
//! runs the plan under a timeout and a row cap.

#![allow(dead_code)]

mod encode;
mod params;
mod run;
mod validate;

pub use run::{SQL_MAX_ROWS, SqlColumn, SqlJson, SqlRequest};

use std::collections::HashSet;

use datafusion::logical_expr::LogicalPlan;
use datafusion::prelude::SQLOptions;
use datafusion::sql::parser::Statement as DFStatement;
use datafusion::sql::sqlparser::tokenizer::{Token, TokenWithSpan};
use params::{bind_parameters, bind_placeholders, param_values};
use serde_json::{Map, Value};

use crate::query_error::{InvalidQuery, Position, RunError};
use crate::query_text::{
    SQL_LIMITS, TextKind, cap_message, expect_end, parse_failure, parser, planning_failure,
    tokenize,
};
use crate::views::QueryContext;
use validate::validate_statement;

/// Plan one console statement that has no parameters.
///
/// `{name:Type}` is rewritten here; values are bound by [`prepare_sql`].
pub(crate) async fn plan_sql(ctx: &QueryContext, text: &str) -> Result<LogicalPlan, InvalidQuery> {
    let (tokens, _) = prepared_tokens(text)?;
    plan_tokens(ctx, text, tokens).await
}

/// Plan `text`, substituting `{name:Type}` placeholders from `params`.
pub(crate) async fn prepare_sql(
    ctx: &QueryContext,
    text: &str,
    params: &Map<String, Value>,
) -> Result<LogicalPlan, RunError> {
    let (tokens, placeholders) = prepared_tokens(text)?;
    let plan = plan_tokens(ctx, text, tokens).await?;
    let values = param_values(&placeholders, params)?;
    bind_parameters(plan, &placeholders, values)
}

fn prepared_tokens(
    text: &str,
) -> Result<(Vec<TokenWithSpan>, Vec<params::Placeholder>), InvalidQuery> {
    let tokens = tokenize(text, &SQL_LIMITS, TextKind::Sql)?;
    bind_placeholders(tokens)
}

async fn plan_tokens(
    ctx: &QueryContext,
    text: &str,
    tokens: Vec<TokenWithSpan>,
) -> Result<LogicalPlan, InvalidQuery> {
    let mut parsed = parser(tokens);
    let start = parsed.peek_token();
    let statement = parsed
        .parse_statement()
        .map_err(|error| parse_failure(error, &parsed, text))?;
    if parsed.consume_token(&Token::SemiColon) {
        let trailing = parsed.peek_token_ref();
        if !matches!(trailing.token, Token::EOF) {
            return Err(invalid(
                "only one statement is allowed",
                Some(position(
                    trailing.span.start.line,
                    trailing.span.start.column,
                )),
            ));
        }
    }
    expect_end(&mut parsed, text)?;
    validate_statement(
        &statement,
        &ctx.registered_view_names(),
        text,
        Some(position(start.span.start.line, start.span.start.column)),
    )?;
    let plan = ctx
        .session()
        .state()
        .statement_to_plan(DFStatement::Statement(Box::new(statement)))
        .await
        .map_err(|error| planning_failure(&error))?;
    SQLOptions::new()
        .with_allow_ddl(false)
        .with_allow_dml(false)
        .with_allow_statements(false)
        .verify_plan(&plan)
        .map_err(|_| invalid("only SELECT queries are allowed", None))?;
    unique_names(&plan)?;
    Ok(plan)
}

fn unique_names(plan: &LogicalPlan) -> Result<(), InvalidQuery> {
    let mut seen = HashSet::new();
    for field in plan.schema().fields() {
        if !seen.insert(field.name().to_owned()) {
            return Err(invalid(
                format!(
                    "column name `{}` appears twice; rename one with AS",
                    field.name()
                ),
                None,
            ));
        }
    }
    Ok(())
}

fn invalid(message: impl AsRef<str>, position: Option<Position>) -> InvalidQuery {
    InvalidQuery {
        message: cap_message(message.as_ref()),
        position,
    }
}

fn position(line: u64, column: u64) -> Position {
    Position {
        line: u32::try_from(line).unwrap_or(u32::MAX),
        column: u32::try_from(column).unwrap_or(u32::MAX),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::query_error::RunError;
    use crate::test_support::example_query;
    use crate::views::SqliteView;
    use datafusion::arrow::array::Int64Array;
    use datafusion::arrow::datatypes::{DataType, Field, Schema};
    use datafusion::arrow::record_batch::RecordBatch;
    use datafusion::datasource::memory::MemTable;

    fn context() -> QueryContext {
        QueryContext::new(example_query(), None).expect("example project")
    }

    fn at(line: u32, column: u32) -> Option<Position> {
        Some(Position { line, column })
    }

    async fn expect_plan(text: &str) -> LogicalPlan {
        plan_sql(&context(), text)
            .await
            .unwrap_or_else(|error| panic!("{text}: {error} at {:?}", error.position))
    }

    #[tokio::test]
    async fn read_queries_plan() {
        let queries = [
            "SELECT * FROM spans",
            "SELECT model, sum(cost_total) FROM spans GROUP BY model",
            "WITH s AS (SELECT span_id FROM spans) SELECT * FROM s UNION ALL SELECT * FROM s",
            "VALUES (1), (2)",
            "SELECT * FROM (SELECT span_id FROM spans) t",
            "SELECT t.trace_id, s.span_id FROM traces t JOIN spans s ON t.trace_id = s.trace_id",
            "SELECT row_number() OVER (PARTITION BY trace_id ORDER BY start_time) FROM spans",
            "SELECT unnest(tags) FROM traces",
            "EXPLAIN SELECT name FROM spans",
            "WITH spans AS (SELECT 1 AS n) SELECT n FROM spans",
        ];
        for text in queries {
            expect_plan(text).await;
        }
        let explained = expect_plan("EXPLAIN SELECT name FROM spans").await;
        let names: Vec<&str> = explained
            .schema()
            .fields()
            .iter()
            .map(|field| field.name().as_str())
            .collect();
        assert_eq!(names, ["plan_type", "plan"]);
    }

    #[tokio::test]
    async fn duplicate_output_names_are_rejected() {
        let error = plan_sql(
            &context(),
            "SELECT a.name, b.name FROM spans a JOIN spans b ON a.span_id = b.span_id",
        )
        .await
        .expect_err("duplicate");
        assert_eq!(
            error.message,
            "column name `name` appears twice; rename one with AS"
        );
        assert_eq!(error.position, None);
    }

    #[tokio::test]
    async fn an_unknown_column_keeps_the_planner_position() {
        let error = plan_sql(&context(), "SELECT nme FROM spans")
            .await
            .expect_err("column");
        assert_eq!(error.position, at(1, 8));
        assert!(error.message.contains("nme"), "{}", error.message);
    }

    #[tokio::test]
    async fn text_limits_apply_to_the_query() {
        let over = "a".repeat(65 * 1024);
        let error = plan_sql(&context(), &over).await.expect_err("bytes");
        assert_eq!(error.message, "the query is longer than 65536 bytes");
        assert_eq!(error.position, None);

        let mut text = String::from("SELECT name FROM spans WHERE true");
        for _ in 0..512 {
            text.push_str(" OR true");
        }
        plan_sql(&context(), &text).await.expect("512 operators");
        let column = text.len() as u32 + 2;
        text.push_str(" OR true");
        let error = plan_sql(&context(), &text).await.expect_err("513");
        assert_eq!(error.message, "the query has more than 512 operators");
        assert_eq!(error.position, at(1, column));
    }

    #[tokio::test]
    async fn a_full_size_query_runs_on_a_two_megabyte_stack() {
        let mut expr = String::from("true");
        for _ in 0..512 {
            expr.push_str(" OR true");
        }
        let expr = format!("{}{expr}{}", "(".repeat(45), ")".repeat(45));
        let sql = format!("SELECT name FROM spans WHERE {expr}");
        let joined = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime");
                runtime.block_on(async move {
                    let ctx = context();
                    let plan = plan_sql(&ctx, &sql).await.expect("plan");
                    let frame = ctx
                        .session()
                        .execute_logical_plan(plan)
                        .await
                        .expect("execute");
                    frame.collect().await.expect("run");
                });
            })
            .expect("thread")
            .join();
        if let Err(payload) = joined {
            std::panic::resume_unwind(payload);
        }
    }

    #[tokio::test]
    async fn sqlite_views_register_once() {
        let ctx = context();
        let schema = Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, false)]));
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![Arc::new(Int64Array::from(vec![1_i64]))],
        )
        .expect("batch");
        let table = MemTable::try_new(schema, vec![vec![batch]]).expect("table");
        ctx.register_sqlite_view(SqliteView::ExperimentItems, Arc::new(table))
            .expect("register");
        let plan = plan_sql(&ctx, "SELECT * FROM experiment_items")
            .await
            .expect("plan");
        let frame = ctx
            .session()
            .execute_logical_plan(plan)
            .await
            .expect("execute");
        let rows = frame.collect().await.expect("collect");
        assert_eq!(rows.iter().map(RecordBatch::num_rows).sum::<usize>(), 1);

        let again = Arc::new(
            MemTable::try_new(
                Arc::new(Schema::new(vec![Field::new("n", DataType::Int64, true)])),
                vec![vec![]],
            )
            .expect("empty"),
        );
        match ctx.register_sqlite_view(SqliteView::ExperimentItems, again) {
            Err(RunError::InvalidRequest(message)) => {
                assert_eq!(message, "experiment_items is already registered");
            }
            other => panic!("expected a second registration to fail, got {other:?}"),
        }
    }

    fn json_params(value: &Value) -> Map<String, Value> {
        value.as_object().expect("json object").clone()
    }

    async fn column(text: &str, params: &Map<String, Value>, name: &str) -> Vec<String> {
        let ctx = context();
        let plan = prepare_sql(&ctx, text, params)
            .await
            .unwrap_or_else(|error| panic!("{text}: {error}"));
        let frame = ctx
            .session()
            .execute_logical_plan(plan)
            .await
            .unwrap_or_else(|error| panic!("{text}: {error}"));
        let batches = frame
            .collect()
            .await
            .unwrap_or_else(|error| panic!("{text}: {error}"));
        let mut values = Vec::new();
        for batch in batches {
            let array = batch
                .column_by_name(name)
                .unwrap_or_else(|| panic!("{text}: no column {name}"));
            for row in 0..array.len() {
                values.push(cell(array, row));
            }
        }
        values
    }

    fn cell(array: &dyn arrow::array::Array, row: usize) -> String {
        use arrow::array::{Int64Array, StringArray};
        if array.is_null(row) {
            return "NULL".to_string();
        }
        if let Some(values) = array.as_any().downcast_ref::<StringArray>() {
            return values.value(row).to_string();
        }
        if let Some(values) = array.as_any().downcast_ref::<Int64Array>() {
            return values.value(row).to_string();
        }
        panic!("column type {}", array.data_type());
    }

    async fn same(parameterized: &str, params: Value, literal: &str, name: &str) {
        let mut bound = column(parameterized, &json_params(&params), name).await;
        let mut plain = column(literal, &Map::new(), name).await;
        bound.sort();
        plain.sort();
        assert_eq!(bound, plain, "{parameterized} vs {literal}");
        assert!(!bound.is_empty(), "{parameterized} matched no rows");
    }

    #[tokio::test]
    async fn each_parameter_kind_filters_rows() {
        same(
            "SELECT span_id FROM spans WHERE name = {n:String}",
            serde_json::json!({"n": "support-agent"}),
            "SELECT span_id FROM spans WHERE name = 'support-agent'",
            "span_id",
        )
        .await;
        same(
            "SELECT trace_id FROM traces WHERE span_count >= {c:Int}",
            serde_json::json!({"c": 2}),
            "SELECT trace_id FROM traces WHERE span_count >= 2",
            "trace_id",
        )
        .await;
        same(
            "SELECT trace_id FROM traces WHERE cost_total > {c:Float}",
            serde_json::json!({"c": 0}),
            "SELECT trace_id FROM traces WHERE cost_total > 0",
            "trace_id",
        )
        .await;
        same(
            "SELECT trace_id FROM traces WHERE has_root = {r:Bool}",
            serde_json::json!({"r": true}),
            "SELECT trace_id FROM traces WHERE has_root = true",
            "trace_id",
        )
        .await;

        let instant = "2026-01-01T10:05:00Z";
        let millis = chrono::DateTime::parse_from_rfc3339(instant)
            .unwrap_or_else(|error| panic!("{instant}: {error}"))
            .timestamp_millis();
        let mut from_text = column(
            "SELECT trace_id FROM traces WHERE start_time >= {t:Timestamp}",
            &json_params(&serde_json::json!({"t": instant})),
            "trace_id",
        )
        .await;
        let mut from_millis = column(
            "SELECT trace_id FROM traces WHERE start_time >= {t:Timestamp}",
            &json_params(&serde_json::json!({"t": millis})),
            "trace_id",
        )
        .await;
        from_text.sort();
        from_millis.sort();
        assert_eq!(from_text, from_millis);
        let all = column("SELECT trace_id FROM traces", &Map::new(), "trace_id").await;
        assert!(!from_text.is_empty());
        assert!(from_text.len() < all.len());
    }

    #[tokio::test]
    async fn a_repeated_name_binds_once_and_two_types_fail() {
        same(
            "SELECT span_id FROM spans WHERE name = {n:String} OR name = {n:String}",
            serde_json::json!({"n": "support-agent"}),
            "SELECT span_id FROM spans WHERE name = 'support-agent'",
            "span_id",
        )
        .await;
        let plan = plan_sql(
            &context(),
            "SELECT span_id FROM spans WHERE name = {n:String} OR name = {n:String}",
        )
        .await
        .expect("plan");
        assert_eq!(plan.get_parameter_types().expect("types").len(), 1);

        let error = prepare_sql(&context(), "SELECT {n:String}, {n:Int}", &Map::new())
            .await
            .expect_err("two types");
        match error {
            RunError::Invalid(error) => {
                assert_eq!(error.message, "parameter `n` is declared with two types");
            }
            other => panic!("expected invalid query, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bad_parameter_values_are_rejected() {
        let cases = [
            (
                "SELECT name FROM spans WHERE name = {n:String}",
                serde_json::json!({}),
                "missing value for parameter n",
            ),
            (
                "SELECT 1",
                serde_json::json!({"n": 1}),
                "unknown parameter n",
            ),
            (
                "SELECT {c:Int} AS v",
                serde_json::json!({"c": 1.5}),
                "parameter c must be an integer",
            ),
            (
                "SELECT {c:Int} AS v",
                serde_json::json!({"c": "1"}),
                "parameter c must be an integer",
            ),
            (
                "SELECT {t:Timestamp} AS v",
                serde_json::json!({"t": i64::MAX}),
                "parameter t is out of range",
            ),
        ];
        let mut mismatches = String::new();
        for (text, params, message) in cases {
            match prepare_sql(&context(), text, &json_params(&params)).await {
                Err(RunError::InvalidRequest(actual)) if actual == message => {}
                Err(error) => mismatches.push_str(&format!("{text}: {error}\n")),
                Ok(_) => mismatches.push_str(&format!("{text}: planned\n")),
            }
        }
        assert!(mismatches.is_empty(), "{mismatches}");
    }

    #[tokio::test]
    async fn an_unknown_parameter_type_points_at_the_type() {
        let error = prepare_sql(&context(), "SELECT {x:Date}", &Map::new())
            .await
            .expect_err("date");
        match error {
            RunError::Invalid(error) => {
                assert_eq!(
                    error.message,
                    "unknown parameter type `Date`; use String, Int, Float, Bool or Timestamp"
                );
                assert_eq!(error.position, at(1, 11));
            }
            other => panic!("expected invalid query, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn raw_placeholders_are_rejected() {
        for text in ["SELECT $1", "SELECT ?"] {
            let error = prepare_sql(&context(), text, &Map::new())
                .await
                .expect_err(text);
            match error {
                RunError::Invalid(error) => {
                    assert_eq!(error.message, "write parameters as {name:Type}");
                    assert_eq!(error.position, at(1, 8), "{text}");
                }
                other => panic!("{text}: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_string_literal_is_not_a_parameter() {
        let values = column("SELECT '{n:String}' AS s", &Map::new(), "s").await;
        assert_eq!(values, ["{n:String}"]);
        match prepare_sql(
            &context(),
            "SELECT '{n:String}' AS s",
            &json_params(&serde_json::json!({"n": "x"})),
        )
        .await
        {
            Err(RunError::InvalidRequest(message)) => {
                assert_eq!(message, "unknown parameter n");
            }
            other => panic!("expected an unknown parameter, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_bare_parameter_keeps_its_declared_type() {
        let values = column(
            "SELECT {x:Int} AS v",
            &json_params(&serde_json::json!({"x": 3})),
            "v",
        )
        .await;
        assert_eq!(values, ["3"]);
    }

    #[tokio::test]
    async fn a_string_parameter_casts_to_the_column_type() {
        same(
            "SELECT trace_id FROM traces WHERE span_count = {s:String}",
            serde_json::json!({"s": "2"}),
            "SELECT trace_id FROM traces WHERE span_count = 2",
            "trace_id",
        )
        .await;
        match prepare_sql(
            &context(),
            "SELECT trace_id FROM traces WHERE span_count = {s:String}",
            &json_params(&serde_json::json!({"s": "abc"})),
        )
        .await
        {
            Err(RunError::InvalidRequest(message)) => {
                assert_eq!(message, "parameter s does not fit where it is used (Int64)");
            }
            other => panic!("expected a cast failure, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn limit_accepts_an_integer_parameter() {
        let values = column(
            "SELECT name FROM spans ORDER BY name LIMIT {n:Int}",
            &json_params(&serde_json::json!({"n": 2})),
            "name",
        )
        .await;
        let expected = column(
            "SELECT name FROM spans ORDER BY name LIMIT 2",
            &Map::new(),
            "name",
        )
        .await;
        assert_eq!(values, expected);
        assert_eq!(values.len(), 2);
    }
}
