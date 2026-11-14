//! The alert measure: one aggregate over a sliding window (spec 07, D-K-2, D-K-11).
//!
//! The filter is a `traces` expression. Trace measures filter the traces
//! directly; span and score measures keep the rows whose trace passes it,
//! through the same semi-join as the dashboard (spec 05). Experiment traces
//! are always excluded; evaluation traces are excluded unless the filter
//! names `environment` (spec 05 § Default exclusions).

use std::sync::Arc;
use std::time::Duration;

use arrow::record_batch::RecordBatch;
use datafusion::common::ScalarValue;
use datafusion::functions_aggregate::expr_fn::{avg, count, sum};
use datafusion::logical_expr::expr_fn::when;
use datafusion::logical_expr::{Expr, LogicalPlan, col, in_subquery, lit};
use llmobs_core::enums::{ScoreDataType, ScoreTargetKind, SpanKind};
use llmobs_core::schema::{scores, spans, traces};

use super::metrics::{f64_at, i64_at, internal, percentile, require};
use super::{ListScope, present, scoped_traces};
use crate::filter::{Filter, FilterView};
use crate::query_error::{RunError, classify};
use crate::views::{QueryContext, TimeWindow, exact_cost_sum};

const COUNT: &str = "alert_count";
const ERRORS: &str = "alert_errors";
const VALUE: &str = "alert_value";
const DURATION_NS: &str = "duration_ns";
const NS_PER_MS: f64 = 1_000_000.0;
const DAY_NS: i64 = 86_400_000_000_000;

/// Which value an alert measures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlertMeasure {
    /// Traces with `error_count > 0` divided by traces.
    ErrorRate,
    /// `sum(cost_total)` on traces, USD.
    Cost,
    /// Trace `duration_ns` p95, in milliseconds.
    TraceLatencyP95,
    /// `duration_ns` p95 of `llm` spans, in milliseconds.
    LlmLatencyP95,
    /// `avg(value_num)` of the live trace and span scores named `name` whose
    /// `value_num` is not null: numeric, boolean (1/0), and categorical with a value.
    ScoreAvg {
        /// Score name.
        name: String,
    },
    /// `avg(value_num)` of the live boolean scores named `name`.
    ScorePassRate {
        /// Score name.
        name: String,
    },
}

/// One alert evaluation.
#[derive(Debug, Clone)]
pub struct AlertValueRequest<'a> {
    /// `start_time ∈ [from, to)` for traces and spans, `created_at` for scores.
    pub window: TimeWindow,
    /// Filter text over `traces`. `None`, empty or all-whitespace means no filter.
    pub filter: Option<&'a str>,
    /// What to measure.
    pub measure: AlertMeasure,
    /// Time allowed for planning and running.
    pub timeout: Duration,
}

/// The sample size and the value over the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlertValue {
    /// Rows the value was computed over: traces, `llm` spans, or scores.
    pub count: i64,
    /// `None` when `count` is 0, except for [`AlertMeasure::Cost`], which is always
    /// `Some` (0.0 when no trace or no cost).
    pub value: Option<f64>,
}

/// Measures one alert over its window.
///
/// # Errors
///
/// [`RunError::Invalid`] for filter text, [`RunError::Timeout`] when the query
/// exceeds `timeout`, and the other [`RunError`] variants from planning and running.
pub async fn alert_value(
    ctx: &QueryContext,
    request: AlertValueRequest<'_>,
) -> Result<AlertValue, RunError> {
    let timeout = request.timeout;
    match tokio::time::timeout(timeout, compute(ctx, request)).await {
        Err(_) => Err(RunError::Timeout),
        Ok(result) => result,
    }
}

/// Parses `text` as a `traces` filter, as [`alert_value`] would, without running anything.
///
/// # Errors
///
/// [`RunError::Invalid`] when the text does not parse or names an unknown column.
/// Empty or all-whitespace text is accepted (it means no filter).
pub fn check_alert_filter(ctx: &QueryContext, text: &str) -> Result<(), RunError> {
    match present(Some(text)) {
        None => Ok(()),
        Some(text) => ctx
            .parse_filter(FilterView::Traces, text)
            .map(|_| ())
            .map_err(RunError::from),
    }
}

async fn compute(
    ctx: &QueryContext,
    request: AlertValueRequest<'_>,
) -> Result<AlertValue, RunError> {
    let filter = match present(request.filter) {
        Some(text) => Some(ctx.parse_filter(FilterView::Traces, text)?),
        None => None,
    };
    let references_environment = filter
        .as_ref()
        .is_some_and(|filter| filter.references_environment);
    let window = request.window;
    match &request.measure {
        AlertMeasure::ErrorRate => {
            let batches = trace_batches(
                ctx,
                window,
                filter,
                references_environment,
                vec![
                    count(lit(1_i64)).alias(COUNT),
                    sum(error_flag()?).alias(ERRORS),
                ],
            )
            .await?;
            let batch = single_row(&batches)?;
            let count = i64_at(require(batch, COUNT)?, 0)?;
            let errors = f64_at(require(batch, ERRORS)?, 0)?.unwrap_or(0.0);
            Ok(AlertValue {
                count,
                value: (count > 0).then(|| errors / count as f64),
            })
        }
        AlertMeasure::Cost => {
            let batches = trace_batches(
                ctx,
                window,
                filter,
                references_environment,
                vec![
                    count(lit(1_i64)).alias(COUNT),
                    exact_cost_sum().alias(VALUE),
                ],
            )
            .await?;
            let batch = single_row(&batches)?;
            let count = i64_at(require(batch, COUNT)?, 0)?;
            let cost = f64_at(require(batch, VALUE)?, 0)?.unwrap_or(0.0);
            Ok(AlertValue {
                count,
                value: Some(cost),
            })
        }
        AlertMeasure::TraceLatencyP95 => {
            let batches = trace_batches(
                ctx,
                window,
                filter,
                references_environment,
                vec![
                    count(lit(1_i64)).alias(COUNT),
                    percentile(DURATION_NS, 0.95).alias(VALUE),
                ],
            )
            .await?;
            let batch = single_row(&batches)?;
            let count = i64_at(require(batch, COUNT)?, 0)?;
            Ok(AlertValue {
                count,
                value: latency_ms(batch, count)?,
            })
        }
        AlertMeasure::LlmLatencyP95 => {
            let ids = trace_id_plan(ctx, window, filter, references_environment).await?;
            let batches = ctx
                .session()
                .table("spans")
                .await
                .map_err(classify)?
                .filter(
                    window
                        .predicate(spans::START_TIME)
                        .and(col(spans::SPAN_KIND).eq(lit(SpanKind::Llm.as_str())))
                        .and(in_subquery(col(spans::TRACE_ID), Arc::new(ids))),
                )
                .map_err(classify)?
                .aggregate(
                    vec![],
                    vec![
                        count(lit(1_i64)).alias(COUNT),
                        percentile(spans::DURATION_NS, 0.95).alias(VALUE),
                    ],
                )
                .map_err(classify)?
                .collect()
                .await
                .map_err(|error| ctx.fail(error))?;
            let batch = single_row(&batches)?;
            let count = i64_at(require(batch, COUNT)?, 0)?;
            Ok(AlertValue {
                count,
                value: latency_ms(batch, count)?,
            })
        }
        AlertMeasure::ScoreAvg { name } => {
            score_value(ctx, window, filter, references_environment, name, false).await
        }
        AlertMeasure::ScorePassRate { name } => {
            score_value(ctx, window, filter, references_environment, name, true).await
        }
    }
}

/// `1` for a trace with errors, `0` otherwise.
fn error_flag() -> Result<Expr, RunError> {
    when(col(traces::ERROR_COUNT).gt(lit(0_i64)), lit(1_i64))
        .otherwise(lit(0_i64))
        .map_err(classify)
}

/// Runs `aggregates` with no group keys over the scoped and filtered traces.
async fn trace_batches(
    ctx: &QueryContext,
    window: TimeWindow,
    filter: Option<Filter>,
    references_environment: bool,
    aggregates: Vec<Expr>,
) -> Result<Vec<RecordBatch>, RunError> {
    let mut frame = scoped_traces(
        ctx,
        ListScope {
            window: Some(window),
            include_eval: false,
            include_experiments: false,
        },
        references_environment,
    )
    .await?;
    if let Some(filter) = filter {
        frame = filter.apply(frame, ctx).await.map_err(classify)?;
    }
    frame
        .aggregate(vec![], aggregates)
        .map_err(classify)?
        .collect()
        .await
        .map_err(|error| ctx.fail(error))
}

/// The trace ids a span or score measure keeps: traces that end at or after the
/// window start and start within the window's day of lookback, that pass the
/// filter, with the default exclusions applied.
async fn trace_id_plan(
    ctx: &QueryContext,
    window: TimeWindow,
    filter: Option<Filter>,
    references_environment: bool,
) -> Result<LogicalPlan, RunError> {
    let start = window
        .from()
        .checked_sub(DAY_NS)
        .ok_or_else(|| RunError::InvalidRequest("from is out of range".to_owned()))?;
    let wide = TimeWindow::new(start, window.to())?;
    let mut frame = scoped_traces(
        ctx,
        ListScope {
            window: Some(wide),
            include_eval: false,
            include_experiments: false,
        },
        references_environment,
    )
    .await?;
    frame = frame
        .filter(col(traces::END_TIME).gt_eq(Expr::Literal(
            ScalarValue::TimestampNanosecond(Some(window.from()), Some("UTC".into())),
            None,
        )))
        .map_err(classify)?;
    if let Some(filter) = filter {
        frame = filter.apply(frame, ctx).await.map_err(classify)?;
    }
    let frame = frame
        .select(vec![col(traces::TRACE_ID)])
        .map_err(classify)?;
    Ok(frame.logical_plan().clone())
}

/// `avg(value_num)` of the live trace and span scores named `name`, optionally
/// only boolean ones, on traces that pass the filter.
async fn score_value(
    ctx: &QueryContext,
    window: TimeWindow,
    filter: Option<Filter>,
    references_environment: bool,
    name: &str,
    pass_rate: bool,
) -> Result<AlertValue, RunError> {
    let ids = trace_id_plan(ctx, window, filter, references_environment).await?;
    let mut predicate = window
        .predicate(scores::CREATED_AT)
        .and(col(scores::NAME).eq(lit(name)))
        .and(col(scores::TARGET).in_list(
            vec![
                lit(ScoreTargetKind::Trace.as_str()),
                lit(ScoreTargetKind::Span.as_str()),
            ],
            false,
        ))
        .and(col(scores::VALUE_NUM).is_not_null())
        .and(in_subquery(col(scores::TRACE_ID), Arc::new(ids)));
    if pass_rate {
        predicate = predicate.and(col(scores::DATA_TYPE).eq(lit(ScoreDataType::Boolean.as_str())));
    }
    let batches = ctx
        .session()
        .table("scores")
        .await
        .map_err(classify)?
        .filter(predicate)
        .map_err(classify)?
        .aggregate(
            vec![],
            vec![
                count(lit(1_i64)).alias(COUNT),
                avg(col(scores::VALUE_NUM)).alias(VALUE),
            ],
        )
        .map_err(classify)?
        .collect()
        .await
        .map_err(|error| ctx.fail(error))?;
    let batch = single_row(&batches)?;
    let count = i64_at(require(batch, COUNT)?, 0)?;
    let value = if count == 0 {
        None
    } else {
        f64_at(require(batch, VALUE)?, 0)?
    };
    Ok(AlertValue { count, value })
}

/// A p95 in nanoseconds, as milliseconds. `None` when `count` is 0.
fn latency_ms(batch: &RecordBatch, count: i64) -> Result<Option<f64>, RunError> {
    if count == 0 {
        return Ok(None);
    }
    Ok(f64_at(require(batch, VALUE)?, 0)?.map(|ns| ns / NS_PER_MS))
}

/// The one batch that has rows. An aggregate with no group keys returns exactly
/// one row, also over zero input rows, so any other count is an internal error.
fn single_row(batches: &[RecordBatch]) -> Result<&RecordBatch, RunError> {
    let mut found = None;
    for batch in batches {
        if batch.num_rows() == 0 {
            continue;
        }
        if found.is_some() || batch.num_rows() != 1 {
            return Err(internal("alert aggregate row count".to_owned()));
        }
        found = Some(batch);
    }
    found.ok_or_else(|| internal("alert aggregate row count".to_owned()))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::time::Duration;

    use datafusion::execution::runtime_env::RuntimeEnv;
    use llmobs_core::enums::{SpanKind, SpanStatus};
    use llmobs_core::names::{environment, metadata_key};
    use llmobs_core::score::{Score, ScoreTarget, ScoreValue};
    use llmobs_core::span::Span;

    use super::*;
    use crate::test_support::{live_scores_table, score_row, span_row, spans_table, traces_table};
    use crate::views::ProjectQuery;

    const T0: i64 = 1_700_000_000_000_000_000;
    const MIN: i64 = 60_000_000_000;

    fn context(spans: &[Span], scores: &[Score]) -> QueryContext {
        let query = ProjectQuery {
            spans: spans_table(spans),
            traces: traces_table(spans),
            scores: live_scores_table(scores),
            runtime: Arc::new(RuntimeEnv::default()),
            target_partitions: NonZeroUsize::new(2).expect("two partitions"),
            optimizer_rules: Vec::new(),
            suspects: crate::verify::Suspects::detached(),
        };
        QueryContext::new(query, None).expect("context")
    }

    fn window() -> TimeWindow {
        TimeWindow::new(T0, T0 + 10 * MIN).expect("window")
    }

    /// A root span of `trace` starting at `start`, in the test window unless the
    /// test says otherwise.
    fn root(trace: u16, start: i64) -> Span {
        span_row(trace, 0, start, 1)
    }

    /// A root span of `trace` at `T0 + MIN`.
    fn in_window(trace: u16) -> Span {
        root(trace, T0 + MIN)
    }

    /// An `llm` child of `parent`, lasting `duration` ns.
    fn llm_child(parent: &Span, trace: u16, start: i64, duration: i64) -> Span {
        let mut child = span_row(trace, 1, start, 1);
        child.span_kind = SpanKind::Llm;
        child.parent_span_id = Some(parent.span_id);
        child.end_time = start + duration;
        child
    }

    /// A score on `trace` created at `T0 + MIN`, named `name`, with `value`.
    fn score(id: u128, trace: u16, name: &str, value: ScoreValue) -> Score {
        let mut score = score_row(id, trace, T0 + MIN, T0 + MIN, T0 + MIN, false, name);
        score.value = value;
        score
    }

    async fn measure(
        ctx: &QueryContext,
        measure: AlertMeasure,
        filter: Option<&str>,
    ) -> Result<AlertValue, RunError> {
        alert_value(
            ctx,
            AlertValueRequest {
                window: window(),
                filter,
                measure,
                timeout: Duration::from_secs(30),
            },
        )
        .await
    }

    fn assert_close(actual: Option<f64>, expected: f64, tolerance: f64) {
        let actual = actual.expect("a value");
        assert!(
            (actual - expected).abs() <= tolerance,
            "{actual} is not {expected} within {tolerance}"
        );
    }

    #[tokio::test]
    async fn error_rate_counts_traces_with_errors() {
        let mut spans: Vec<Span> = (0..4).map(in_window).collect();
        spans[1].status = SpanStatus::Error;
        let ctx = context(&spans, &[]);
        let value = measure(&ctx, AlertMeasure::ErrorRate, None)
            .await
            .expect("value");
        assert_eq!(value.count, 4);
        assert_close(value.value, 0.25, 1e-9);
    }

    #[tokio::test]
    async fn error_rate_without_traces_is_none() {
        let ctx = context(&[], &[]);
        let value = measure(&ctx, AlertMeasure::ErrorRate, None)
            .await
            .expect("value");
        assert_eq!(
            value,
            AlertValue {
                count: 0,
                value: None
            }
        );
    }

    #[tokio::test]
    async fn traces_outside_the_window_are_ignored() {
        let spans = vec![root(0, T0 - 1), root(1, T0 + 10 * MIN), in_window(2)];
        let ctx = context(&spans, &[]);
        let value = measure(&ctx, AlertMeasure::ErrorRate, None)
            .await
            .expect("value");
        assert_eq!(value.count, 1);
    }

    #[tokio::test]
    async fn cost_sums_and_is_zero_without_traces() {
        let mut spans: Vec<Span> = (0..3).map(in_window).collect();
        spans[0].cost_total = Some(1.5);
        spans[1].cost_total = Some(2.25);
        let ctx = context(&spans, &[]);
        let value = measure(&ctx, AlertMeasure::Cost, None)
            .await
            .expect("value");
        assert_eq!(value.count, 3);
        assert_close(value.value, 3.75, 1e-9);

        let ctx = context(&[], &[]);
        let value = measure(&ctx, AlertMeasure::Cost, None)
            .await
            .expect("value");
        assert_eq!(
            value,
            AlertValue {
                count: 0,
                value: Some(0.0)
            }
        );
    }

    #[tokio::test]
    async fn trace_latency_is_milliseconds() {
        let spans: Vec<Span> = (1..=20_u64)
            .map(|k| {
                let mut span = in_window(u16::try_from(k).expect("small"));
                span.end_time = span.start_time + i64::try_from(k).expect("small") * 1_000_000;
                span
            })
            .collect();
        let ctx = context(&spans, &[]);
        let value = measure(&ctx, AlertMeasure::TraceLatencyP95, None)
            .await
            .expect("value");
        assert_eq!(value.count, 20);
        let p95 = value.value.expect("a value");
        assert!((18.0..=20.0).contains(&p95), "p95 {p95}");
    }

    #[tokio::test]
    async fn llm_latency_reads_llm_spans_only() {
        let mut spans = Vec::new();
        for trace in 0..3_u16 {
            let parent = in_window(trace);
            let child = llm_child(&parent, trace, T0 + MIN, 5_000_000);
            spans.push(parent);
            spans.push(child);
        }
        let mut slow = in_window(3);
        slow.end_time = slow.start_time + 50_000_000;
        spans.push(slow);
        let ctx = context(&spans, &[]);
        let value = measure(&ctx, AlertMeasure::LlmLatencyP95, None)
            .await
            .expect("value");
        assert_eq!(value.count, 3);
        assert_close(value.value, 5.0, 0.5);
    }

    #[tokio::test]
    async fn score_avg_and_pass_rate() {
        let spans = vec![
            in_window(0),
            in_window(1),
            in_window(2),
            in_window(3),
            in_window(4),
            in_window(5),
            in_window(6),
        ];
        let scores = vec![
            score(1, 0, "helpfulness", ScoreValue::Numeric(0.2)),
            score(2, 1, "helpfulness", ScoreValue::Numeric(0.8)),
            score(3, 2, "ok", ScoreValue::Boolean(true)),
            score(4, 3, "ok", ScoreValue::Boolean(true)),
            score(5, 4, "ok", ScoreValue::Boolean(false)),
            score(
                6,
                5,
                "ok",
                ScoreValue::Categorical {
                    label: "fine".to_owned(),
                    value: None,
                },
            ),
            score(7, 6, "other", ScoreValue::Numeric(1.0)),
        ];
        let ctx = context(&spans, &scores);
        let avg = measure(
            &ctx,
            AlertMeasure::ScoreAvg {
                name: "helpfulness".to_owned(),
            },
            None,
        )
        .await
        .expect("value");
        assert_eq!(avg.count, 2);
        assert_close(avg.value, 0.5, 1e-9);

        let rate = measure(
            &ctx,
            AlertMeasure::ScorePassRate {
                name: "ok".to_owned(),
            },
            None,
        )
        .await
        .expect("value");
        assert_eq!(rate.count, 3);
        assert_close(rate.value, 2.0 / 3.0, 1e-9);
    }

    #[tokio::test]
    async fn score_on_a_filtered_out_trace_is_ignored() {
        let mut prod = in_window(0);
        prod.environment = Some("prod".to_owned());
        let mut dev = in_window(1);
        dev.environment = Some("dev".to_owned());
        let scores = vec![
            score(1, 0, "helpfulness", ScoreValue::Numeric(0.5)),
            score(2, 1, "helpfulness", ScoreValue::Numeric(0.5)),
        ];
        let ctx = context(&[prod, dev], &scores);
        let value = measure(
            &ctx,
            AlertMeasure::ScoreAvg {
                name: "helpfulness".to_owned(),
            },
            Some("environment = 'prod'"),
        )
        .await
        .expect("value");
        assert_eq!(value.count, 1);
    }

    /// One normal, one evaluation and one experiment trace.
    fn three_kinds() -> Vec<Span> {
        let normal = in_window(0);
        let mut eval = in_window(1);
        eval.environment = Some(environment::EVAL.to_owned());
        let mut experiment = in_window(2);
        experiment
            .metadata
            .insert(metadata_key::EXPERIMENT_ID.to_owned(), "e1".to_owned());
        vec![normal, eval, experiment]
    }

    #[tokio::test]
    async fn evaluation_and_experiment_traces_are_excluded() {
        let ctx = context(&three_kinds(), &[]);
        let value = measure(&ctx, AlertMeasure::ErrorRate, None)
            .await
            .expect("value");
        assert_eq!(value.count, 1);

        let filter = format!("environment = '{}'", environment::EVAL);
        let value = measure(&ctx, AlertMeasure::ErrorRate, Some(&filter))
            .await
            .expect("value");
        assert_eq!(value.count, 1);
    }

    #[tokio::test]
    async fn scores_ignore_eval_and_experiment_traces() {
        let scores: Vec<Score> = (0..3_u16)
            .map(|trace| {
                score(
                    u128::from(trace) + 1,
                    trace,
                    "helpfulness",
                    ScoreValue::Numeric(1.0),
                )
            })
            .collect();
        let ctx = context(&three_kinds(), &scores);
        let value = measure(
            &ctx,
            AlertMeasure::ScoreAvg {
                name: "helpfulness".to_owned(),
            },
            None,
        )
        .await
        .expect("value");
        assert_eq!(value.count, 1);
    }

    #[tokio::test]
    async fn a_trace_started_before_the_window_still_counts_its_spans() {
        // Starts before the window, ends inside it: its llm child is in the window.
        let mut early = root(0, T0 - 2 * MIN);
        early.end_time = T0 + 2 * MIN;
        let child = llm_child(&early, 0, T0 + MIN, 5_000_000);
        // Ends before the window: nothing from it counts.
        let mut gone = root(1, T0 - 3 * MIN);
        gone.end_time = T0 - MIN;
        let gone_child = llm_child(&gone, 1, T0 - 2 * MIN, 5_000_000);
        let ctx = context(&[early, child, gone, gone_child], &[]);
        let value = measure(&ctx, AlertMeasure::LlmLatencyP95, None)
            .await
            .expect("value");
        assert_eq!(value.count, 1);
    }

    #[tokio::test]
    async fn session_scores_are_not_counted() {
        let spans = vec![in_window(0)];
        let mut session = score(1, 0, "helpfulness", ScoreValue::Numeric(0.5));
        session.target = ScoreTarget::Session {
            session_id: "s1".to_owned(),
        };
        let ctx = context(&spans, &[session]);
        let value = measure(
            &ctx,
            AlertMeasure::ScoreAvg {
                name: "helpfulness".to_owned(),
            },
            None,
        )
        .await
        .expect("value");
        assert_eq!(
            value,
            AlertValue {
                count: 0,
                value: None
            }
        );
    }

    #[tokio::test]
    async fn blank_filter_is_no_filter() {
        let mut spans: Vec<Span> = (0..4).map(in_window).collect();
        spans[0].status = SpanStatus::Error;
        let ctx = context(&spans, &[]);
        let none = measure(&ctx, AlertMeasure::ErrorRate, None)
            .await
            .expect("value");
        let blank = measure(&ctx, AlertMeasure::ErrorRate, Some("  "))
            .await
            .expect("value");
        assert_eq!(none, blank);
    }

    #[tokio::test]
    async fn filter_errors_are_invalid() {
        let ctx = context(&[in_window(0)], &[]);
        let result = measure(&ctx, AlertMeasure::ErrorRate, Some("nonsense_column = 1")).await;
        assert!(matches!(result, Err(RunError::Invalid(_))), "{result:?}");
    }

    #[test]
    fn check_alert_filter_accepts_empty_and_rejects_bad_text() {
        let ctx = context(&[], &[]);
        assert!(check_alert_filter(&ctx, "").is_ok());
        assert!(check_alert_filter(&ctx, "  ").is_ok());
        assert!(matches!(
            check_alert_filter(&ctx, "error_count >"),
            Err(RunError::Invalid(_))
        ));
        assert!(check_alert_filter(&ctx, "error_count > 0").is_ok());
    }
}
