//! Filter DSL: text in, a checked boolean [`Expr`] over one view out.
//!
//! Score functions, list queries and the SQL console are the callers.

#![allow(dead_code)]

mod scope;
mod scores;
mod validate;

pub(crate) use scope::{exclusions, search, time_window};

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
use scores::planning_schema;
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
    /// Score names joined while applying, in index order.
    pub(crate) scores: Vec<String>,
}

impl Filter {
    /// Keep rows of `frame` that match.
    ///
    /// Score calls join the latest row of that name from `scores` first. The
    /// result has `frame`'s columns in their original order.
    pub(crate) async fn apply(
        self,
        frame: DataFrame,
        ctx: &QueryContext,
    ) -> Result<DataFrame, DataFusionError> {
        scores::apply(frame, ctx, self.view, &self.scores, self.expr).await
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
        let validated = validate(view, schema, &mut ast)?;
        let planning = planning_schema(schema, &validated.scores)?;
        let expr = self
            .session()
            .state()
            .create_logical_expr_from_sql_expr(
                ExprWithAlias {
                    expr: ast,
                    alias: None,
                },
                &planning,
            )
            .map_err(|error| planning_failure(&error))?;
        let data_type = expr
            .get_type(planning.as_ref())
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
            references_environment: validated.references_environment,
            scores: validated.scores,
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
    use std::num::NonZeroUsize;
    use std::path::PathBuf;
    use std::str::FromStr;
    use std::sync::Arc;

    use datafusion::arrow::array::{Array, AsArray, RecordBatch};
    use datafusion::execution::runtime_env::RuntimeEnv;
    use datafusion::logical_expr::Expr;
    use llmobs_core::enums::{SpanKind, SpanStatus};
    use llmobs_core::ids::{SpanId, TraceId};
    use llmobs_core::names::metadata_key;
    use llmobs_core::score::{Score, ScoreTarget};
    use llmobs_core::span::Span;

    use crate::query_error::{Position, RunError};
    use crate::test_support::{
        dataset_scores, dataset_spans, example_query, live_scores, live_scores_table, spans_table,
        traces_table,
    };
    use crate::views::{ProjectQuery, QueryContext, TimeWindow};

    use super::FilterView;
    use super::scope::{exclusions, search, time_window};

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
            .apply(frame, ctx)
            .await
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

    fn trace_hex(target: &ScoreTarget) -> Option<String> {
        match target {
            ScoreTarget::Trace { trace_id } => Some(trace_id.to_hex()),
            ScoreTarget::Span { .. } | ScoreTarget::Session { .. } => None,
        }
    }

    fn span_hex(target: &ScoreTarget) -> Option<String> {
        match target {
            ScoreTarget::Span { span_id, .. } => Some(span_id.to_hex()),
            ScoreTarget::Trace { .. } | ScoreTarget::Session { .. } => None,
        }
    }

    fn session_of(target: &ScoreTarget) -> Option<&str> {
        match target {
            ScoreTarget::Session { session_id } => Some(session_id),
            ScoreTarget::Trace { .. } | ScoreTarget::Span { .. } => None,
        }
    }

    /// Latest live score of `name` on each trace, by `(updated_at, score_id)`.
    fn latest_on_traces(name: &str) -> Vec<Score> {
        let mut best: BTreeMap<String, Score> = BTreeMap::new();
        for score in live_scores(&dataset_scores()) {
            if score.name != name {
                continue;
            }
            let Some(id) = trace_hex(&score.target) else {
                continue;
            };
            let replace = best.get(&id).is_none_or(|current| {
                (score.updated_at, &score.score_id) > (current.updated_at, &current.score_id)
            });
            if replace {
                best.insert(id, score);
            }
        }
        best.into_values().collect()
    }

    #[tokio::test]
    async fn score_functions_read_the_latest_value() {
        let ctx = context();
        let rows = traces(&dataset_spans());
        let all: BTreeSet<String> = rows.iter().map(|trace| trace.id.clone()).collect();
        let helpful = latest_on_traces("helpfulness");
        let matched: BTreeSet<String> = helpful
            .iter()
            .filter(|score| score.value.value_num().is_some_and(|value| value >= 0.5))
            .filter_map(|score| trace_hex(&score.target))
            .collect();
        assert_eq!(
            matching(
                &ctx,
                FilterView::Traces,
                "score('helpfulness') >= 0.5",
                "trace_id"
            )
            .await,
            matched
        );
        let mut missing = all.clone();
        for id in &matched {
            missing.remove(id);
        }
        assert_eq!(
            matching(
                &ctx,
                FilterView::Traces,
                "score('helpfulness') IS NULL",
                "trace_id"
            )
            .await,
            missing
        );

        let present = matching(
            &ctx,
            FilterView::Traces,
            "has_score('helpfulness')",
            "trace_id",
        )
        .await;
        let absent = matching(
            &ctx,
            FilterView::Traces,
            "NOT has_score('helpfulness')",
            "trace_id",
        )
        .await;
        assert!(present.is_disjoint(&absent));
        assert_eq!(
            present.union(&absent).cloned().collect::<BTreeSet<_>>(),
            all
        );

        let tone = live_scores(&dataset_scores())
            .into_iter()
            .find(|score| score.name == "tone")
            .expect("tone");
        assert_eq!(
            matching(
                &ctx,
                FilterView::Spans,
                "score_label('tone') = 'friendly'",
                "span_id"
            )
            .await,
            span_hex(&tone.target).into_iter().collect()
        );
        assert_eq!(
            matching(&ctx, FilterView::Spans, "score('tone') > 0.5", "span_id").await,
            span_hex(&tone.target).into_iter().collect()
        );
        let resolved = live_scores(&dataset_scores())
            .into_iter()
            .find(|score| score.name == "resolved")
            .expect("resolved");
        assert_eq!(
            matching(
                &ctx,
                FilterView::Sessions,
                "score('resolved') > 0.5",
                "session_id"
            )
            .await,
            session_of(&resolved.target)
                .map(str::to_owned)
                .into_iter()
                .collect()
        );
    }

    fn hand_score(id: &str, value: &str, updated: &str) -> Score {
        let text = format!(
            r#"{{
                "project_id": "01900000-0000-7000-8000-000000000001",
                "score_id": "{id}",
                "target": {{"type": "trace", "trace_id": "a1000000000000000000000000000001"}},
                "name": "q",
                "value": {value},
                "source": "api",
                "created_at": "{updated}",
                "updated_at": "{updated}",
                "deleted": false,
                "ingest_time": "{updated}"
            }}"#
        );
        serde_json::from_str(&text).unwrap_or_else(|error| panic!("{error}"))
    }

    fn context_with_scores(scores: &[Score]) -> QueryContext {
        let spans = dataset_spans();
        QueryContext::new(
            ProjectQuery {
                spans: spans_table(&spans),
                traces: traces_table(&spans),
                scores: live_scores_table(scores),
                runtime: Arc::new(RuntimeEnv::default()),
                target_partitions: NonZeroUsize::new(2).expect("two partitions"),
            },
            None,
        )
        .expect("context")
    }

    const TRACE_1: &str = "a1000000000000000000000000000001";
    const LOW_ID: &str = "01900000-0000-7000-8000-0000000000a1";
    const HIGH_ID: &str = "01900000-0000-7000-8000-0000000000b2";
    const EARLIER: &str = "2026-01-01T00:00:00.000000001Z";
    const LATER: &str = "2026-01-01T00:00:00.000000002Z";

    #[tokio::test]
    async fn the_latest_score_wins() {
        let numeric = |value: f64| format!(r#"{{"data_type":"numeric","value":{value}}}"#);
        let by_time = context_with_scores(&[
            hand_score(LOW_ID, &numeric(1.0), EARLIER),
            hand_score(HIGH_ID, &numeric(2.0), LATER),
        ]);
        assert_eq!(
            matching(&by_time, FilterView::Traces, "score('q') = 2", "trace_id").await,
            BTreeSet::from([TRACE_1.to_owned()])
        );
        assert!(
            matching(&by_time, FilterView::Traces, "score('q') = 1", "trace_id")
                .await
                .is_empty()
        );

        let by_id = context_with_scores(&[
            hand_score(LOW_ID, &numeric(1.0), LATER),
            hand_score(HIGH_ID, &numeric(9.0), LATER),
        ]);
        assert_eq!(
            matching(&by_id, FilterView::Traces, "score('q') = 9", "trace_id").await,
            BTreeSet::from([TRACE_1.to_owned()])
        );

        let categorical = context_with_scores(&[
            hand_score(HIGH_ID, &numeric(1.0), EARLIER),
            hand_score(
                LOW_ID,
                r#"{"data_type":"categorical","value":{"label":"good"}}"#,
                LATER,
            ),
        ]);
        assert_eq!(
            matching(
                &categorical,
                FilterView::Traces,
                "score('q') IS NULL AND score_label('q') = 'good'",
                "trace_id"
            )
            .await,
            BTreeSet::from([TRACE_1.to_owned()])
        );
    }

    #[tokio::test]
    async fn two_score_names_share_one_join_when_repeated() {
        let ctx = context();
        let either = ctx
            .parse_filter(
                FilterView::Traces,
                "score('helpfulness') >= 0.5 OR score('thumbs') >= 0.5",
            )
            .expect("or");
        assert_eq!(
            either.scores,
            vec!["helpfulness".to_owned(), "thumbs".to_owned()]
        );
        let frame = ctx.session().table("traces").await.expect("traces");
        let columns: Vec<String> = frame
            .schema()
            .fields()
            .iter()
            .map(|field| field.name().clone())
            .collect();
        let filtered = either.apply(frame, &ctx).await.expect("apply");
        let actual: Vec<String> = filtered
            .schema()
            .fields()
            .iter()
            .map(|field| field.name().clone())
            .collect();
        assert_eq!(actual, columns);
        drop(filtered);

        assert_eq!(
            matching(
                &ctx,
                FilterView::Traces,
                "score('helpfulness') >= 0.5 OR score('thumbs') >= 0.5",
                "trace_id"
            )
            .await,
            BTreeSet::from([
                "a1000000000000000000000000000001".to_owned(),
                "a1000000000000000000000000000002".to_owned(),
            ])
        );
        assert!(
            matching(
                &ctx,
                FilterView::Traces,
                "score('helpfulness') >= 0.5 AND score('thumbs') >= 0.5",
                "trace_id"
            )
            .await
            .is_empty()
        );
        let repeated = ctx
            .parse_filter(
                FilterView::Traces,
                "score('helpfulness') >= 0.5 AND score('helpfulness') < 1",
            )
            .expect("repeated");
        assert_eq!(repeated.scores, vec!["helpfulness".to_owned()]);
        assert_eq!(
            matching(
                &ctx,
                FilterView::Traces,
                "score('helpfulness') >= 0.5 AND score('helpfulness') < 1",
                "trace_id"
            )
            .await,
            BTreeSet::from(["a1000000000000000000000000000001".to_owned()])
        );
    }

    #[test]
    fn score_calls_that_are_not_one_literal_fail() {
        let ctx = context();
        let cases = [
            (
                FilterView::Spans,
                "score(name) > 1",
                "score() takes one string literal, the score name",
                at(1, 1),
            ),
            (
                FilterView::Spans,
                "score('a', 'b') > 1",
                "score() takes one string literal, the score name",
                at(1, 1),
            ),
            (
                FilterView::Scores,
                "score('x') > 1",
                "score functions are not available on the scores view",
                at(1, 1),
            ),
            (
                FilterView::Spans,
                "__score_0_num > 1",
                "unknown column `__score_0_num`",
                at(1, 1),
            ),
        ];
        for (view, text, message, position) in cases {
            let error = ctx.parse_filter(view, text).expect_err(text);
            assert_eq!(
                (error.message.as_str(), error.position),
                (message, position),
                "{text}"
            );
        }
        let mut calls = Vec::new();
        for name in 'a'..='i' {
            calls.push(format!("score('{name}') > 0"));
        }
        let text = calls.join(" OR ");
        let column = text.find("score('i')").expect("ninth") as u32 + 1;
        let error = ctx
            .parse_filter(FilterView::Traces, &text)
            .expect_err("nine names");
        assert_eq!(error.message, "a filter can use at most 8 score names");
        assert_eq!(error.position, at(1, column));
    }

    fn context_with_spans(spans: &[Span]) -> QueryContext {
        QueryContext::new(
            ProjectQuery {
                spans: spans_table(spans),
                traces: traces_table(spans),
                scores: live_scores_table(&dataset_scores()),
                runtime: Arc::new(RuntimeEnv::default()),
                target_partitions: NonZeroUsize::new(2).expect("two partitions"),
            },
            None,
        )
        .expect("context")
    }

    async fn matching_expr(
        ctx: &QueryContext,
        view: FilterView,
        expr: Expr,
        column: &str,
    ) -> BTreeSet<String> {
        let frame = ctx
            .session()
            .table(view.as_str())
            .await
            .unwrap_or_else(|error| panic!("{}: {error}", view.as_str()));
        let filtered = frame
            .filter(expr)
            .unwrap_or_else(|error| panic!("filter: {error}"));
        strings(&filtered.collect().await.expect("collect"), column)
    }

    fn listed_trace_ids() -> BTreeSet<String> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/api/responses/traces.json");
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        let value: serde_json::Value = serde_json::from_str(&text).expect("traces.json");
        value["data"]
            .as_array()
            .expect("data")
            .iter()
            .map(|row| row["trace_id"].as_str().expect("trace_id").to_owned())
            .collect()
    }

    fn day() -> TimeWindow {
        TimeWindow::new(nanos("2026-01-01T00:00:00Z"), nanos("2026-01-02T00:00:00Z")).expect("day")
    }

    #[tokio::test]
    async fn exclusions_hide_eval_traces_unless_asked() {
        let ctx = context();
        let listed = listed_trace_ids();
        let hidden = exclusions(&ctx, FilterView::Traces, false, false, false)
            .expect("exclusions")
            .expect("both clauses");
        assert_eq!(
            matching_expr(&ctx, FilterView::Traces, hidden, "trace_id").await,
            listed
        );
        assert!(!listed.contains("a1000000000000000000000000000005"));

        let with_eval = exclusions(&ctx, FilterView::Traces, true, false, false)
            .expect("include eval")
            .expect("experiment clause");
        let mut expected = listed.clone();
        expected.insert("a1000000000000000000000000000005".to_owned());
        assert_eq!(
            matching_expr(&ctx, FilterView::Traces, with_eval, "trace_id").await,
            expected
        );

        let by_filter = exclusions(&ctx, FilterView::Traces, false, false, true)
            .expect("environment reference")
            .expect("experiment clause");
        assert_eq!(
            matching_expr(&ctx, FilterView::Traces, by_filter, "trace_id").await,
            expected
        );

        assert!(
            exclusions(&ctx, FilterView::Traces, true, true, false)
                .expect("neither clause")
                .is_none()
        );
    }

    #[tokio::test]
    async fn experiment_traces_stay_hidden_when_eval_is_included() {
        let mut spans = dataset_spans();
        let mut experiment = spans[0].clone();
        experiment.trace_id = TraceId::from_str("a1000000000000000000000000000007").expect("trace");
        experiment.span_id = SpanId::from_str("b100000000000071").expect("span");
        experiment.parent_span_id = None;
        experiment.environment = Some("production".to_owned());
        experiment
            .metadata
            .insert(metadata_key::EXPERIMENT_ID.to_owned(), "exp-1".to_owned());
        spans.push(experiment);
        let ctx = context_with_spans(&spans);
        let listed = listed_trace_ids();
        let experiment_id = "a1000000000000000000000000000007";
        let eval_id = "a1000000000000000000000000000005";

        let default = exclusions(&ctx, FilterView::Traces, false, false, false)
            .expect("default")
            .expect("clauses");
        assert_eq!(
            matching_expr(&ctx, FilterView::Traces, default, "trace_id").await,
            listed
        );

        let eval_only = exclusions(&ctx, FilterView::Traces, true, false, false)
            .expect("eval")
            .expect("experiment clause");
        let mut with_eval = listed.clone();
        with_eval.insert(eval_id.to_owned());
        assert_eq!(
            matching_expr(&ctx, FilterView::Traces, eval_only, "trace_id").await,
            with_eval
        );

        let experiments = exclusions(&ctx, FilterView::Traces, false, true, false)
            .expect("experiments")
            .expect("eval clause");
        let mut with_experiment = listed.clone();
        with_experiment.insert(experiment_id.to_owned());
        assert_eq!(
            matching_expr(&ctx, FilterView::Traces, experiments, "trace_id").await,
            with_experiment
        );
    }

    #[tokio::test]
    async fn time_window_includes_the_start_and_excludes_the_end() {
        let ctx = context();
        let from = nanos("2026-01-01T10:05:00Z");
        let to = nanos("2026-01-01T10:20:00Z");
        let window = TimeWindow::new(from, to).expect("window");
        let rows = traces(&dataset_spans());
        assert!(
            rows.iter().any(|trace| trace.start_time == to),
            "a trace starts on the exclusive boundary"
        );
        let expected: BTreeSet<String> = rows
            .into_iter()
            .filter(|trace| (from..to).contains(&trace.start_time))
            .map(|trace| trace.id)
            .collect();
        assert!(!expected.is_empty());
        assert_eq!(
            matching_expr(&ctx, FilterView::Traces, time_window(window), "trace_id").await,
            expected
        );
    }

    fn text_has(span: &Span, needle: &str) -> bool {
        let needle = needle.to_lowercase();
        [
            span.input.as_deref(),
            span.output.as_deref(),
            Some(span.name.as_str()),
            span.system_instructions.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|text| text.to_lowercase().contains(&needle))
    }

    #[tokio::test]
    async fn search_matches_span_text_literally() {
        let ctx = context();
        let spans = dataset_spans();
        let found = search(FilterView::Spans, "ORDER", Some(day()), &ctx)
            .await
            .expect("search")
            .expect("predicate");
        assert_eq!(
            matching_expr(&ctx, FilterView::Spans, found, "span_id").await,
            ids_where(&spans, |span| text_has(span, "order"))
        );
        assert!(
            search(FilterView::Spans, " \n", None, &ctx)
                .await
                .expect("empty")
                .is_none()
        );

        let mut rows = spans;
        let mut units = rows[0].clone();
        units.trace_id = TraceId::from_str("a1000000000000000000000000000007").expect("trace");
        units.span_id = SpanId::from_str("b100000000000071").expect("span");
        units.parent_span_id = None;
        units.input = Some("bought 1000 units".to_owned());
        units.output = None;
        units.name = "count".to_owned();
        units.system_instructions = None;
        let mut percent = units.clone();
        percent.trace_id = TraceId::from_str("a1000000000000000000000000000008").expect("trace");
        percent.span_id = SpanId::from_str("b100000000000081").expect("span");
        percent.input = Some("save 100% today".to_owned());
        let mut slash = units.clone();
        slash.trace_id = TraceId::from_str("a1000000000000000000000000000009").expect("trace");
        slash.span_id = SpanId::from_str("b100000000000091").expect("span");
        slash.input = Some(r"dir a\b".to_owned());
        rows.extend([units, percent, slash]);
        let ctx = context_with_spans(&rows);

        let percent_hit = search(FilterView::Spans, "100%", Some(day()), &ctx)
            .await
            .expect("percent")
            .expect("predicate");
        assert_eq!(
            matching_expr(&ctx, FilterView::Spans, percent_hit, "span_id").await,
            BTreeSet::from(["b100000000000081".to_owned()])
        );
        let underscore = search(FilterView::Spans, "100_", Some(day()), &ctx)
            .await
            .expect("underscore")
            .expect("predicate");
        assert!(
            matching_expr(&ctx, FilterView::Spans, underscore, "span_id")
                .await
                .is_empty()
        );
        let backslash = search(FilterView::Spans, r"\", Some(day()), &ctx)
            .await
            .expect("backslash")
            .expect("predicate");
        assert_eq!(
            matching_expr(&ctx, FilterView::Spans, backslash, "span_id").await,
            BTreeSet::from(["b100000000000091".to_owned()])
        );
    }

    #[tokio::test]
    async fn search_on_traces_returns_the_owning_trace() {
        let ctx = context();
        let spans = dataset_spans();
        let found = search(FilterView::Traces, "ORDER", Some(day()), &ctx)
            .await
            .expect("search")
            .expect("subquery");
        let expected: BTreeSet<String> = spans
            .iter()
            .filter(|span| text_has(span, "order"))
            .map(|span| span.trace_id.to_hex())
            .collect();
        assert!(!expected.is_empty());
        assert_eq!(
            matching_expr(&ctx, FilterView::Traces, found, "trace_id").await,
            expected
        );

        let missing = search(FilterView::Traces, "ORDER", None, &ctx).await;
        match missing {
            Err(RunError::InvalidRequest(message)) => {
                assert_eq!(message, "search needs a time range");
            }
            other => panic!("expected a missing window, got {other:?}"),
        }
        let spans_missing = search(FilterView::Spans, "ORDER", None, &ctx).await;
        match spans_missing {
            Err(RunError::InvalidRequest(message)) => {
                assert_eq!(message, "search needs a time range");
            }
            other => panic!("expected a missing window, got {other:?}"),
        }
        let sessions = search(FilterView::Sessions, "ORDER", Some(day()), &ctx).await;
        match sessions {
            Err(RunError::InvalidRequest(message)) => {
                assert_eq!(message, "search is not available on this list");
            }
            other => panic!("expected an unsupported list, got {other:?}"),
        }
    }
}
