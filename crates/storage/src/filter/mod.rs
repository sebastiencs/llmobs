//! Filter DSL: text in, a checked boolean [`Expr`] over one view out.
//!
//! Score functions, list queries and the SQL console are the callers. Until
//! they land, only the tests in this module reference these items.

#![allow(dead_code)]

mod validate;

use datafusion::arrow::datatypes::DataType;
use datafusion::error::DataFusionError;
use datafusion::logical_expr::{Expr, ExprSchemable};
use datafusion::prelude::DataFrame;
use datafusion::sql::sqlparser::ast::ExprWithAlias;

use crate::query_error::{InvalidQuery, Position};
use crate::query_text::{
    FILTER_LIMITS, TextKind, cap_message, expect_end, parse_failure, parser, planning_failure,
    tokenize,
};
use crate::views::{QueryContext, ViewName};
use validate::validate;

/// Which view a filter expression is compiled against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterView {
    /// The `traces` view.
    Traces,
    /// The `spans` view.
    Spans,
    /// The `sessions` view.
    Sessions,
    /// The `scores` view.
    Scores,
}

impl FilterView {
    fn view_name(self) -> ViewName {
        match self {
            Self::Traces => ViewName::Traces,
            Self::Spans => ViewName::Spans,
            Self::Sessions => ViewName::Sessions,
            Self::Scores => ViewName::Scores,
        }
    }

    fn as_str(self) -> &'static str {
        self.view_name().as_str()
    }
}

/// A filter planned against one view.
#[derive(Debug)]
pub(crate) struct Filter {
    /// View the expression was compiled for.
    pub(crate) view: FilterView,
    /// Boolean expression over that view's columns.
    pub(crate) expr: Expr,
    /// True when the expression names the `environment` column.
    pub(crate) references_environment: bool,
}

impl Filter {
    /// Keep rows of `frame` that match. `frame` has this view's columns.
    pub(crate) fn apply(self, frame: DataFrame) -> Result<DataFrame, DataFusionError> {
        frame.filter(self.expr)
    }
}

impl QueryContext {
    /// Compile `text` into a boolean expression over `view`.
    ///
    /// Empty or whitespace-only text is an error. Callers that treat an empty
    /// filter as "no filter" check before calling.
    pub(crate) fn parse_filter(
        &self,
        view: FilterView,
        text: &str,
    ) -> Result<Filter, InvalidQuery> {
        if text.chars().all(char::is_whitespace) {
            return Err(invalid("the filter is empty", None));
        }
        let tokens = tokenize(text, &FILTER_LIMITS, TextKind::Filter)?;
        let mut parsed = parser(tokens);
        let mut ast = parsed
            .parse_expr()
            .map_err(|error| parse_failure(error, &parsed, text))?;
        expect_end(&mut parsed, text)?;
        let schema = self.view_schema(view.view_name());
        let references_environment = validate(schema, &mut ast)?;
        let expr = self
            .session()
            .state()
            .create_logical_expr_from_sql_expr(
                ExprWithAlias {
                    expr: ast,
                    alias: None,
                },
                schema,
            )
            .map_err(|error| planning_failure(&error))?;
        let data_type = expr
            .get_type(schema.as_ref())
            .map_err(|error| planning_failure(&error))?;
        if !matches!(data_type, DataType::Boolean | DataType::Null) {
            return Err(invalid(
                "the filter must be a true/false expression",
                Some(Position { line: 1, column: 1 }),
            ));
        }
        Ok(Filter {
            view,
            expr,
            references_environment,
        })
    }
}

fn invalid(message: impl AsRef<str>, position: Option<Position>) -> InvalidQuery {
    InvalidQuery {
        message: cap_message(message.as_ref()),
        position,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use datafusion::arrow::array::{Array, AsArray, RecordBatch};
    use llmobs_core::enums::{SpanKind, SpanStatus};
    use llmobs_core::span::Span;

    use crate::query_error::Position;
    use crate::test_support::{dataset_scores, dataset_spans, example_query, live_scores};
    use crate::views::QueryContext;

    use super::FilterView;

    fn context() -> QueryContext {
        QueryContext::new(example_query(), None).expect("example project")
    }

    fn at(line: u32, column: u32) -> Option<Position> {
        Some(Position { line, column })
    }

    async fn matching(
        ctx: &QueryContext,
        view: FilterView,
        text: &str,
        column: &str,
    ) -> BTreeSet<String> {
        let filter = ctx
            .parse_filter(view, text)
            .unwrap_or_else(|error| panic!("{text}: {error} at {:?}", error.position));
        let frame = ctx
            .session()
            .table(view.as_str())
            .await
            .unwrap_or_else(|error| panic!("{text}: {error}"));
        let filtered = filter
            .apply(frame)
            .unwrap_or_else(|error| panic!("{text}: {error}"));
        let batches = filtered
            .collect()
            .await
            .unwrap_or_else(|error| panic!("{text}: {error}"));
        strings(&batches, column)
    }

    fn strings(batches: &[RecordBatch], column: &str) -> BTreeSet<String> {
        let mut values = BTreeSet::new();
        for batch in batches {
            let array = batch
                .column_by_name(column)
                .unwrap_or_else(|| panic!("missing column {column}"));
            if let Some(strings) = array.as_string_opt::<i32>() {
                values.extend(strings.iter().flatten().map(str::to_owned));
                continue;
            }
            if let Some(strings) = array.as_string_view_opt() {
                values.extend(strings.iter().flatten().map(str::to_owned));
                continue;
            }
            panic!("{column} has type {}", array.data_type());
        }
        values
    }

    fn ids_where(spans: &[Span], keep: impl Fn(&Span) -> bool) -> BTreeSet<String> {
        spans
            .iter()
            .filter(|span| keep(span))
            .map(|span| span.span_id.to_hex())
            .collect()
    }

    fn model(span: &Span) -> Option<&str> {
        span.response_model
            .as_deref()
            .or(span.request_model.as_deref())
    }

    struct Trace {
        id: String,
        start_time: i64,
        has_root: bool,
        session_id: Option<String>,
        tags: BTreeSet<String>,
        cost_total: Option<f64>,
        metadata: BTreeMap<String, String>,
    }

    fn traces(spans: &[Span]) -> Vec<Trace> {
        let mut groups: BTreeMap<String, Vec<&Span>> = BTreeMap::new();
        for span in spans {
            groups.entry(span.trace_id.to_hex()).or_default().push(span);
        }
        groups
            .into_iter()
            .map(|(id, mut group)| {
                group.sort_by_key(|span| (span.start_time, span.span_id));
                let root = group
                    .iter()
                    .copied()
                    .filter(|span| span.parent_span_id.is_none())
                    .min_by_key(|span| (span.start_time, span.span_id));
                let start_time = group.iter().map(|span| span.start_time).min().unwrap_or(0);
                let mut tags = BTreeSet::new();
                let mut metadata = root.map(|span| span.metadata.clone()).unwrap_or_default();
                let mut cost_total = None;
                for span in &group {
                    tags.extend(span.tags.iter().cloned());
                    if root.is_none_or(|root| root.span_id != span.span_id) {
                        for (key, value) in &span.metadata {
                            metadata.entry(key.clone()).or_insert_with(|| value.clone());
                        }
                    }
                    if let Some(value) = span.cost_total {
                        *cost_total.get_or_insert(0.0) += value;
                    }
                }
                let session_id = root
                    .and_then(|span| span.session_id.clone())
                    .or_else(|| group.iter().find_map(|span| span.session_id.clone()));
                Trace {
                    id,
                    start_time,
                    has_root: root.is_some(),
                    session_id,
                    tags,
                    cost_total,
                    metadata,
                }
            })
            .collect()
    }

    fn nanos(text: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(text)
            .expect(text)
            .timestamp_nanos_opt()
            .expect(text)
    }

    #[tokio::test]
    async fn span_filters_match_the_dataset() {
        let ctx = context();
        let spans = dataset_spans();
        assert_eq!(
            matching(&ctx, FilterView::Spans, "status = 'error'", "span_id").await,
            ids_where(&spans, |span| span.status == SpanStatus::Error)
        );
        assert_eq!(
            matching(&ctx, FilterView::Spans, "model LIKE 'claude%'", "span_id").await,
            ids_where(&spans, |span| {
                model(span).is_some_and(|model| model.starts_with("claude"))
            })
        );
        assert_eq!(
            matching(&ctx, FilterView::Spans, "input ILIKE '%order%'", "span_id").await,
            ids_where(&spans, |span| {
                span.input
                    .as_deref()
                    .is_some_and(|input| input.to_lowercase().contains("order"))
            })
        );
        assert_eq!(
            matching(
                &ctx,
                FilterView::Spans,
                "span_kind IN ('llm', 'tool')",
                "span_id"
            )
            .await,
            ids_where(&spans, |span| {
                matches!(span.span_kind, SpanKind::Llm | SpanKind::Tool)
            })
        );
        assert_eq!(
            matching(
                &ctx,
                FilterView::Spans,
                "duration_ns BETWEEN 1e8 AND 5e9",
                "span_id"
            )
            .await,
            ids_where(&spans, |span| {
                let duration = span.end_time - span.start_time;
                (100_000_000..=5_000_000_000).contains(&duration)
            })
        );
        assert_eq!(
            matching(
                &ctx,
                FilterView::Spans,
                "attributes['llmobs.eval.evaluator_id'] = '01900000-0000-7000-8000-000000000201'",
                "span_id"
            )
            .await,
            ids_where(&spans, |span| {
                span.attributes.get("llmobs.eval.evaluator_id")
                    == Some(&"01900000-0000-7000-8000-000000000201".to_owned())
            })
        );
    }

    #[tokio::test]
    async fn trace_filters_match_the_dataset() {
        let ctx = context();
        let rows = traces(&dataset_spans());
        let ids = |keep: &dyn Fn(&Trace) -> bool| -> BTreeSet<String> {
            rows.iter()
                .filter(|trace| keep(trace))
                .map(|trace| trace.id.clone())
                .collect()
        };
        assert_eq!(
            matching(
                &ctx,
                FilterView::Traces,
                "cost_total > 0.001 AND tags @> ['beta']",
                "trace_id"
            )
            .await,
            ids(&|trace| {
                trace.cost_total.is_some_and(|cost| cost > 0.001) && trace.tags.contains("beta")
            })
        );
        assert_eq!(
            matching(
                &ctx,
                FilterView::Traces,
                "metadata['channel'] = 'email'",
                "trace_id"
            )
            .await,
            ids(&|trace| trace.metadata.get("channel") == Some(&"email".to_owned()))
        );
        assert_eq!(
            matching(&ctx, FilterView::Traces, "NOT has_root", "trace_id").await,
            ids(&|trace| !trace.has_root)
        );
        let bound = nanos("2026-01-01T10:05:00Z");
        assert_eq!(
            matching(
                &ctx,
                FilterView::Traces,
                "start_time >= '2026-01-01T10:05:00Z'",
                "trace_id"
            )
            .await,
            ids(&|trace| trace.start_time >= bound)
        );
        assert_eq!(
            matching(
                &ctx,
                FilterView::Traces,
                "start_time > now() - INTERVAL '3650 days'",
                "trace_id"
            )
            .await,
            rows.iter().map(|trace| trace.id.clone()).collect()
        );
    }

    #[tokio::test]
    async fn session_and_score_filters_match_the_dataset() {
        let ctx = context();
        let rows = traces(&dataset_spans());
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for trace in &rows {
            if let Some(session) = &trace.session_id {
                *counts.entry(session.clone()).or_default() += 1;
            }
        }
        let sessions = counts
            .into_iter()
            .filter(|(_, count)| *count >= 2)
            .map(|(session, _)| session)
            .collect();
        assert_eq!(
            matching(&ctx, FilterView::Sessions, "trace_count >= 2", "session_id").await,
            sessions
        );

        let scores = live_scores(&dataset_scores());
        let expected = scores
            .iter()
            .filter(|score| {
                score.name == "helpfulness"
                    && score.value.value_num().is_some_and(|value| value > 0.5)
            })
            .map(|score| score.score_id.to_string())
            .collect();
        assert_eq!(
            matching(
                &ctx,
                FilterView::Scores,
                "name = 'helpfulness' AND value_num > 0.5",
                "score_id"
            )
            .await,
            expected
        );
    }

    #[tokio::test]
    async fn casts_do_not_fail_on_bad_values_and_names_resolve() {
        let ctx = context();
        let spans = dataset_spans();
        assert_eq!(
            matching(
                &ctx,
                FilterView::Spans,
                "attributes['llmobs.eval.evaluator_id']::DOUBLE > 0",
                "span_id"
            )
            .await,
            BTreeSet::new()
        );
        assert_eq!(
            matching(
                &ctx,
                FilterView::Traces,
                "metadata['channel']::DOUBLE > 0",
                "trace_id"
            )
            .await,
            BTreeSet::new()
        );
        assert_eq!(
            matching(
                &ctx,
                FilterView::Spans,
                "CAST(duration_ns AS DOUBLE) > 0",
                "span_id"
            )
            .await,
            ids_where(&spans, |span| span.end_time > span.start_time)
        );

        let quoted = ctx
            .parse_filter(FilterView::Spans, "\"name\" = 'x'")
            .expect("quoted name");
        let bare = ctx
            .parse_filter(FilterView::Spans, "NAME = 'x'")
            .expect("bare name");
        assert_eq!(quoted.expr.to_string(), bare.expr.to_string());
        assert!(
            quoted
                .expr
                .to_string()
                .split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
                .any(|word| word == "name"),
            "{}",
            quoted.expr
        );

        let with_environment = ctx
            .parse_filter(
                FilterView::Spans,
                "environment = 'production' OR status = 'ok'",
            )
            .expect("environment");
        assert!(with_environment.references_environment);
        let without = ctx
            .parse_filter(FilterView::Spans, "status = 'ok'")
            .expect("status");
        assert!(!without.references_environment);
    }

    #[test]
    fn rejected_filters_name_the_problem_and_its_position() {
        let ctx = context();
        let cases = [
            ("nme = 'a'", "unknown column `nme`", at(1, 1)),
            (
                "spans.name = 'a'",
                "a qualified name is not allowed in a filter",
                at(1, 1),
            ),
            (
                "trace_id IN (SELECT trace_id FROM spans)",
                "a subquery is not allowed in a filter",
                at(1, 1),
            ),
            (
                "EXISTS (SELECT 1)",
                "a subquery is not allowed in a filter",
                at(1, 9),
            ),
            (
                "CASE WHEN true THEN true END",
                "CASE is not allowed in a filter",
                at(1, 1),
            ),
            ("sleep(1) > 0", "unknown function `sleep`", at(1, 1)),
            ("count(*) > 1", "unknown function `count`", at(1, 1)),
            (
                "lower(DISTINCT name) = 'a'",
                "DISTINCT is not allowed in a filter",
                at(1, 1),
            ),
            (
                "row_number() OVER () > 1",
                "a window function is not allowed in a filter",
                at(1, 1),
            ),
            (
                "name = 'a' extra",
                "unexpected text after the expression",
                at(1, 12),
            ),
            (
                "cost_total + 1",
                "the filter must be a true/false expression",
                at(1, 1),
            ),
            ("name['k'] = 'a'", "`name` is not a map column", at(1, 1)),
            (
                "metadata[1] = 'a'",
                "map keys must be string literals",
                at(1, 10),
            ),
            (
                "CAST(name AS INT) = 1",
                "casts to `INT` are not allowed",
                at(1, 6),
            ),
            (
                "x'00' = name",
                "a hex literal is not allowed in a filter",
                at(1, 1),
            ),
            ("score('x') > 0", "unknown function `score`", at(1, 1)),
            ("", "the filter is empty", None),
            (" \n\t", "the filter is empty", None),
        ];
        for (text, message, position) in cases {
            let error = ctx.parse_filter(FilterView::Spans, text).expect_err(text);
            assert_eq!(
                (error.message.as_str(), error.position),
                (message, position),
                "{text}"
            );
        }
    }

    #[test]
    fn a_full_size_filter_runs_on_a_two_megabyte_stack() {
        let mut body = String::from("status = 'ok'");
        for _ in 0..127 {
            body.push_str(" OR true");
        }
        let text = format!("{}{body}{}", "(".repeat(40), ")".repeat(40));
        let joined = std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime");
                runtime.block_on(async move {
                    let ctx = context();
                    let found = matching(&ctx, FilterView::Spans, &text, "span_id").await;
                    assert_eq!(found.len(), dataset_spans().len());
                });
            })
            .expect("thread")
            .join();
        if let Err(payload) = joined {
            std::panic::resume_unwind(payload);
        }
    }
}
