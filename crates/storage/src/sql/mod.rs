//! SQL console: validate, plan, bind, run and encode.
//!
//! Q11 binds parameters. Q12 runs the plan. Until then the tests are the callers.

#![allow(dead_code)]

mod validate;

use std::collections::HashSet;

use datafusion::logical_expr::LogicalPlan;
use datafusion::prelude::SQLOptions;
use datafusion::sql::parser::Statement as DFStatement;
use datafusion::sql::sqlparser::tokenizer::Token;

use crate::query_error::{InvalidQuery, Position};
use crate::query_text::{
    SQL_LIMITS, TextKind, cap_message, expect_end, parse_failure, parser, planning_failure,
    tokenize,
};
use crate::views::QueryContext;
use validate::validate_statement;

/// Plan one console statement.
///
/// Q11 inserts parameter binding between tokenization and parsing.
pub(crate) async fn plan_sql(ctx: &QueryContext, text: &str) -> Result<LogicalPlan, InvalidQuery> {
    let tokens = tokenize(text, &SQL_LIMITS, TextKind::Sql)?;
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
}
