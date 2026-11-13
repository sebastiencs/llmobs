//! Dashboard metrics.
//!
//! Trace series share one aggregate. Span tokens, TTFT, LLM latency, and cost
//! by model are further aggregates, as are score averages, the two KPI ranges,
//! and top models. Span and score rows are limited to traces that pass the
//! request, through a semi-join, except when the filter is absent and both
//! include flags are on. Empty buckets stay `None`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use arrow::array::{Array, AsArray};
use arrow::datatypes::{Float64Type, Int64Type, TimestampNanosecondType, UInt64Type};
use arrow::record_batch::RecordBatch;
use datafusion::common::{NullHandling, ScalarValue, UnnestOptions};
use datafusion::error::DataFusionError;
use datafusion::functions::datetime::expr_fn::date_bin;
use datafusion::functions_aggregate::expr_fn::{approx_percentile_cont, avg, count, sum};
use datafusion::logical_expr::expr_fn::when;
use datafusion::logical_expr::{Expr, LogicalPlan, SortExpr, col, in_subquery, lit};
use datafusion::prelude::DataFrame;
use llmobs_core::enums::{ScoreTargetKind, SpanKind};
use llmobs_core::names::environment;
use llmobs_core::schema::{scores, spans, traces};

use super::{ListScope, present, scoped_traces};
use crate::filter::{Filter, FilterView};
use crate::query_error::{RunError, classify};
use crate::views::{QueryContext, TimeWindow};

const NS_MINUTE: i64 = 60_000_000_000;
const NS_HOUR: i64 = 60 * NS_MINUTE;
const AUTO_LIMIT: i64 = 120;
const FIXED_LIMIT: i64 = 2_000;
const TOO_MANY_BUCKETS: &str = "too many buckets; pick a wider bucket";

const AUTO_WIDTHS: [BucketWidth; 8] = [
    BucketWidth::M1,
    BucketWidth::M5,
    BucketWidth::M10,
    BucketWidth::M15,
    BucketWidth::M30,
    BucketWidth::H1,
    BucketWidth::H6,
    BucketWidth::H12,
];

const DURATION_NS: &str = "duration_ns";
const BUCKET: &str = "bucket";
const GROUP: &str = "group_key";
const TRACES: &str = "traces";
const ERRORS: &str = "errors";
const P50: &str = "p50";
const P95: &str = "p95";
const P99: &str = "p99";
const COST: &str = "cost";
const VALUE: &str = "value";
const LLM_COUNT: &str = "llm_count";
const TOP_MODELS: usize = 10;

/// Width of one dashboard bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BucketWidth {
    /// One minute.
    M1,
    /// Five minutes.
    M5,
    /// Ten minutes.
    M10,
    /// Fifteen minutes.
    M15,
    /// Thirty minutes.
    M30,
    /// One hour.
    H1,
    /// Six hours.
    H6,
    /// Twelve hours.
    H12,
    /// One day, UTC.
    D1,
}

impl BucketWidth {
    /// Width in nanoseconds.
    pub fn ns(self) -> i64 {
        match self {
            Self::M1 => NS_MINUTE,
            Self::M5 => 5 * NS_MINUTE,
            Self::M10 => 10 * NS_MINUTE,
            Self::M15 => 15 * NS_MINUTE,
            Self::M30 => 30 * NS_MINUTE,
            Self::H1 => NS_HOUR,
            Self::H6 => 6 * NS_HOUR,
            Self::H12 => 12 * NS_HOUR,
            Self::D1 => 24 * NS_HOUR,
        }
    }
}

/// How the dashboard picks a bucket width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    /// Smallest width of at most 120 buckets, else one day.
    Auto,
    /// The given width.
    Fixed(BucketWidth),
}

/// Column a dashboard series is split by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricsGroupBy {
    /// One row per element of `models`.
    Model,
    /// The `environment` column.
    Environment,
    /// The `release` column.
    Release,
}

/// Dashboard metrics over one window.
#[derive(Debug, Clone)]
pub struct MetricsRequest<'a> {
    /// `start_time ∈ [from, to)`.
    pub window: TimeWindow,
    /// Bucket width.
    pub bucket: Bucket,
    /// Filter text over `traces`. Empty means no filter.
    pub filter: Option<&'a str>,
    /// Split the series that follow it. Eval cost, LLM latency, cost by model,
    /// and score averages keep their own keys.
    pub group_by: Option<MetricsGroupBy>,
    /// Keep rows whose environment is the eval environment.
    pub include_eval: bool,
    /// Keep rows that belong to an experiment.
    pub include_experiments: bool,
    /// Time allowed for planning and running.
    pub timeout: Duration,
}

/// One dashboard series.
///
/// Variant order matches the server's `MetricName`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SeriesName {
    /// `count(*)` on `traces`.
    Traces,
    /// Traces whose `error_count` is greater than zero.
    Errors,
    /// Errors divided by traces.
    ErrorRate,
    /// Trace `duration_ns` p50.
    LatencyP50,
    /// Trace `duration_ns` p95.
    LatencyP95,
    /// Trace `duration_ns` p99.
    LatencyP99,
    /// `sum(cost_total)` on `traces`, USD.
    Cost,
    /// Sum of `input_tokens` on `llm` and `embedding` spans.
    InputTokens,
    /// Sum of `output_tokens` on `llm` and `embedding` spans.
    OutputTokens,
    /// Sum of `cache_read_tokens` on `llm` and `embedding` spans.
    CacheReadTokens,
    /// Sum of `cache_write_tokens` on `llm` and `embedding` spans.
    CacheWriteTokens,
    /// `ttft_ns` p50 on `llm` spans.
    TtftP50,
    /// `ttft_ns` p95 on `llm` spans.
    TtftP95,
    /// `duration_ns` p50 on `llm` spans. `group` is the model.
    LlmLatencyP50,
    /// `duration_ns` p95 on `llm` spans. `group` is the model.
    LlmLatencyP95,
    /// `sum(cost_total)` on `llm` spans, USD. `group` is the model.
    CostByModel,
    /// `avg(value_num)` per score name. `group` is the score name.
    ScoreAvg,
    /// `sum(cost_total)` on evaluation traces, USD. Never grouped.
    EvalCost,
}

/// One KPI over the request window and the previous window of the same length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KpiName {
    /// Trace count.
    Traces,
    /// Errors divided by traces.
    ErrorRate,
    /// Trace `duration_ns` p95.
    LatencyP95,
    /// `sum(cost_total)` on traces, USD.
    CostTotal,
    /// `sum(total_tokens)` on traces.
    TotalTokens,
}

/// A KPI value. Absent when that range has no traces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Kpi {
    /// Which measure.
    pub name: KpiName,
    /// Value over `[from, to)`.
    pub value: Option<f64>,
    /// Value over the previous range of the same length.
    pub previous: Option<f64>,
}

/// One model in the cost ranking. At most 10.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelUsageRow {
    /// Model name.
    pub model: String,
    /// Number of `llm` spans.
    pub llm_count: i64,
    /// Sum of `total_tokens`. Absent when every span omitted it.
    pub total_tokens: Option<i64>,
    /// Sum of `cost_total`, USD. Absent when every span omitted it.
    pub cost_total: Option<f64>,
}

/// One bucket of a series.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    /// Bucket start, nanoseconds since the Unix epoch, UTC.
    pub time: i64,
    /// Absent when the bucket has no rows.
    pub value: Option<f64>,
}

/// One named line on the dashboard.
#[derive(Debug, Clone, PartialEq)]
pub struct Series {
    /// Which measure these points are.
    pub name: SeriesName,
    /// Group value. Absent when the series is not split.
    pub group: Option<String>,
    /// One point per bucket, in time order.
    pub points: Vec<Point>,
}

/// Dashboard metrics for one request.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricsData {
    /// Width of every bucket, nanoseconds.
    pub bucket_ns: i64,
    /// The five KPIs, in [`KpiName`] order.
    pub kpis: Vec<Kpi>,
    /// Sorted by [`SeriesName`], then by group (`None` first).
    pub series: Vec<Series>,
    /// At most 10 models, `cost_total` descending, nulls last, then model ascending.
    pub top_models: Vec<ModelUsageRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RowKey {
    time: i64,
    group: Option<String>,
}

struct TraceRow {
    traces: i64,
    errors: i64,
    p50: Option<f64>,
    p95: Option<f64>,
    p99: Option<f64>,
    cost: Option<f64>,
}

struct TokenSums {
    input: Option<f64>,
    output: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

struct Percentiles {
    p50: Option<f64>,
    p95: Option<f64>,
}

struct LlmRow {
    p50: Option<f64>,
    p95: Option<f64>,
    cost: Option<f64>,
}

struct SeriesRows {
    traces: BTreeMap<RowKey, TraceRow>,
    eval_cost: BTreeMap<i64, Option<f64>>,
    tokens: BTreeMap<RowKey, TokenSums>,
    ttft: BTreeMap<RowKey, Percentiles>,
    llm: BTreeMap<RowKey, LlmRow>,
    scores: BTreeMap<RowKey, Option<f64>>,
}

struct KpiValues {
    traces: Option<f64>,
    error_rate: Option<f64>,
    latency_p95: Option<f64>,
    cost: Option<f64>,
    tokens: Option<f64>,
}

impl KpiValues {
    fn empty() -> Self {
        Self {
            traces: None,
            error_rate: None,
            latency_p95: None,
            cost: None,
            tokens: None,
        }
    }
}

/// Dashboard series, KPIs, and the top models.
///
/// # Errors
///
/// [`RunError::InvalidRequest`] when the buckets would emit more than 2,000
/// points (an explicit bucket, or `Auto` over more than 2,000 aligned daily
/// buckets). [`RunError::Invalid`] for filter text. [`RunError::Timeout`]
/// when the query exceeds `timeout`.
pub async fn metrics(
    ctx: &QueryContext,
    request: MetricsRequest<'_>,
) -> Result<MetricsData, RunError> {
    let timeout = request.timeout;
    match tokio::time::timeout(timeout, compute(ctx, request)).await {
        Err(_) => Err(RunError::Timeout),
        Ok(result) => result,
    }
}

/// Nine collects: trace series, eval cost, token sums, TTFT percentiles, LLM
/// latency with cost by model, score averages, the current KPI range, the
/// previous KPI range, and top models. The trace-id set is one plan, inlined
/// into the span, score, and top-model queries, not a tenth collect.
async fn compute(ctx: &QueryContext, request: MetricsRequest<'_>) -> Result<MetricsData, RunError> {
    let width = resolve(request.window, request.bucket)?;
    let grid = bucket_grid(request.window, width)?;
    let ids = trace_ids(ctx, &request).await?;
    let rows = SeriesRows {
        traces: trace_rows(ctx, &request, width, &grid).await?,
        eval_cost: eval_cost(ctx, &request, width, &grid).await?,
        tokens: token_rows(ctx, &request, width, &grid, ids.as_ref()).await?,
        ttft: ttft_rows(ctx, &request, width, &grid, ids.as_ref()).await?,
        llm: llm_rows(ctx, &request, width, &grid, ids.as_ref()).await?,
        scores: score_rows(ctx, &request, width, &grid, ids.as_ref()).await?,
    };
    let kpis = kpi_pair(ctx, &request).await?;
    let top_models = top_model_rows(ctx, &request, ids.as_ref()).await?;
    Ok(assemble(
        width,
        &grid,
        request.group_by,
        &rows,
        kpis,
        top_models,
    ))
}

fn resolve(window: TimeWindow, bucket: Bucket) -> Result<i64, RunError> {
    match bucket {
        Bucket::Fixed(width) => {
            let count = bucket_count(window, width.ns())?;
            if !(1..=FIXED_LIMIT).contains(&count) {
                return Err(too_many());
            }
            Ok(width.ns())
        }
        Bucket::Auto => {
            for width in AUTO_WIDTHS {
                if bucket_count(window, width.ns())? <= AUTO_LIMIT {
                    return Ok(width.ns());
                }
            }
            let width = BucketWidth::D1.ns();
            if bucket_count(window, width)? > FIXED_LIMIT {
                return Err(too_many());
            }
            Ok(width)
        }
    }
}

fn bucket_count(window: TimeWindow, width: i64) -> Result<i64, RunError> {
    let first = align_down(window.from(), width)?;
    let last_tick = window.to().checked_sub(1).ok_or_else(too_many)?;
    let last = align_down(last_tick, width)?;
    let span = last.checked_sub(first).ok_or_else(too_many)?;
    if span.checked_rem_euclid(width) != Some(0) {
        return Err(too_many());
    }
    span.checked_div(width)
        .and_then(|steps| steps.checked_add(1))
        .ok_or_else(too_many)
}

fn bucket_grid(window: TimeWindow, width: i64) -> Result<Vec<i64>, RunError> {
    let count = bucket_count(window, width)?;
    if count < 1 {
        return Err(too_many());
    }
    let first = align_down(window.from(), width)?;
    let len = usize::try_from(count).map_err(|_| too_many())?;
    let mut times = Vec::with_capacity(len);
    for step in 0..count {
        let at = step
            .checked_mul(width)
            .and_then(|offset| first.checked_add(offset))
            .ok_or_else(too_many)?;
        times.push(at);
    }
    Ok(times)
}

fn align_down(time: i64, width: i64) -> Result<i64, RunError> {
    let remainder = time.checked_rem_euclid(width).ok_or_else(too_many)?;
    time.checked_sub(remainder).ok_or_else(too_many)
}

fn too_many() -> RunError {
    RunError::InvalidRequest(TOO_MANY_BUCKETS.to_owned())
}

async fn trace_rows(
    ctx: &QueryContext,
    request: &MetricsRequest<'_>,
    width: i64,
    grid: &[i64],
) -> Result<BTreeMap<RowKey, TraceRow>, RunError> {
    let filter = parsed(ctx, request.filter)?;
    let references_environment = filter
        .as_ref()
        .is_some_and(|filter| filter.references_environment);
    let mut frame = scoped_traces(
        ctx,
        ListScope {
            window: Some(request.window),
            include_eval: request.include_eval,
            include_experiments: request.include_experiments,
        },
        references_environment,
    )
    .await?;
    if let Some(filter) = filter {
        frame = filter.apply(frame, ctx).await.map_err(classify)?;
    }
    frame = grouped(frame, request.group_by)?;
    frame = project(frame, width, request.group_by)?;
    let batches = aggregate(frame, request.group_by.is_some())?
        .collect()
        .await
        .map_err(|error| ctx.fail(error))?;
    rows_from(&batches, request.group_by.is_some(), grid)
}

/// Eval traces only. The eval exclusion is off; the experiment exclusion still
/// follows `include_experiments`. The caller's filter is applied after
/// `environment = EVAL`, so a filter that excludes that environment yields no
/// rows and every point stays `None`.
async fn eval_cost(
    ctx: &QueryContext,
    request: &MetricsRequest<'_>,
    width: i64,
    grid: &[i64],
) -> Result<BTreeMap<i64, Option<f64>>, RunError> {
    let filter = parsed(ctx, request.filter)?;
    let mut frame = scoped_traces(
        ctx,
        ListScope {
            window: Some(request.window),
            include_eval: true,
            include_experiments: request.include_experiments,
        },
        false,
    )
    .await?;
    frame = frame
        .filter(col(traces::ENVIRONMENT).eq(lit(environment::EVAL)))
        .map_err(classify)?;
    if let Some(filter) = filter {
        frame = filter.apply(frame, ctx).await.map_err(classify)?;
    }
    let batches = frame
        .select(vec![
            bucket_expr(width, traces::START_TIME),
            col(traces::COST_TOTAL),
        ])
        .map_err(classify)?
        .aggregate(
            vec![col(BUCKET)],
            vec![sum(col(traces::COST_TOTAL)).alias(COST)],
        )
        .map_err(classify)?
        .collect()
        .await
        .map_err(|error| ctx.fail(error))?;
    cost_rows(&batches, grid)
}

fn parsed(ctx: &QueryContext, text: Option<&str>) -> Result<Option<Filter>, RunError> {
    match present(text) {
        Some(text) => Ok(Some(ctx.parse_filter(FilterView::Traces, text)?)),
        None => Ok(None),
    }
}

/// Scoped trace ids over the lookback day. `None` skips the semi-join: the
/// filter is absent and both include flags are on.
async fn trace_ids(
    ctx: &QueryContext,
    request: &MetricsRequest<'_>,
) -> Result<Option<LogicalPlan>, RunError> {
    if present(request.filter).is_none() && request.include_eval && request.include_experiments {
        return Ok(None);
    }
    let start = request
        .window
        .from()
        .checked_sub(BucketWidth::D1.ns())
        .ok_or_else(out_of_range)?;
    let window = TimeWindow::new(start, request.window.to())?;
    let filter = parsed(ctx, request.filter)?;
    let references_environment = filter
        .as_ref()
        .is_some_and(|filter| filter.references_environment);
    let mut frame = scoped_traces(
        ctx,
        ListScope {
            window: Some(window),
            include_eval: request.include_eval,
            include_experiments: request.include_experiments,
        },
        references_environment,
    )
    .await?;
    frame = frame
        .filter(at_least(traces::END_TIME, request.window.from()))
        .map_err(classify)?;
    if let Some(filter) = filter {
        frame = filter.apply(frame, ctx).await.map_err(classify)?;
    }
    let frame = frame
        .select(vec![col(traces::TRACE_ID)])
        .map_err(classify)?;
    Ok(Some(frame.logical_plan().clone()))
}

fn out_of_range() -> RunError {
    RunError::InvalidRequest("from is out of range".to_owned())
}

fn at_least(column: &str, ns: i64) -> Expr {
    col(column).gt_eq(Expr::Literal(
        ScalarValue::TimestampNanosecond(Some(ns), Some("UTC".into())),
        None,
    ))
}

async fn span_frame(
    ctx: &QueryContext,
    request: &MetricsRequest<'_>,
    ids: Option<&LogicalPlan>,
) -> Result<DataFrame, RunError> {
    let mut predicate = request.window.predicate(spans::START_TIME);
    if let Some(plan) = ids {
        predicate = predicate.and(in_subquery(col(spans::TRACE_ID), Arc::new(plan.clone())));
    }
    ctx.session()
        .table("spans")
        .await
        .map_err(classify)?
        .filter(predicate)
        .map_err(classify)
}

fn kind_is(kinds: &[SpanKind]) -> Expr {
    col(spans::SPAN_KIND).in_list(kinds.iter().map(|kind| lit(kind.as_str())).collect(), false)
}

fn span_group(group_by: Option<MetricsGroupBy>) -> Option<&'static str> {
    match group_by {
        Some(MetricsGroupBy::Model) => Some(spans::MODEL),
        Some(MetricsGroupBy::Environment) => Some(spans::ENVIRONMENT),
        Some(MetricsGroupBy::Release) => Some(spans::RELEASE),
        None => None,
    }
}

fn with_bucket(
    frame: DataFrame,
    width: i64,
    time: &str,
    group: Option<&str>,
    values: &[&str],
) -> Result<DataFrame, RunError> {
    let mut columns = vec![bucket_expr(width, time)];
    columns.extend(values.iter().map(|name| col(*name)));
    if let Some(name) = group {
        columns.push(col(name).alias(GROUP));
    }
    frame.select(columns).map_err(classify)
}

fn bucket_keys(grouped: bool) -> Vec<Expr> {
    let mut keys = vec![col(BUCKET)];
    if grouped {
        keys.push(col(GROUP));
    }
    keys
}

async fn token_rows(
    ctx: &QueryContext,
    request: &MetricsRequest<'_>,
    width: i64,
    grid: &[i64],
    ids: Option<&LogicalPlan>,
) -> Result<BTreeMap<RowKey, TokenSums>, RunError> {
    let group = span_group(request.group_by);
    let mut predicate = kind_is(&[SpanKind::Llm, SpanKind::Embedding]);
    if let Some(name) = group {
        predicate = predicate.and(col(name).is_not_null());
    }
    let columns = [
        spans::INPUT_TOKENS,
        spans::OUTPUT_TOKENS,
        spans::CACHE_READ_TOKENS,
        spans::CACHE_WRITE_TOKENS,
    ];
    let frame = with_bucket(
        span_frame(ctx, request, ids)
            .await?
            .filter(predicate)
            .map_err(classify)?,
        width,
        spans::START_TIME,
        group,
        &columns,
    )?;
    let batches = frame
        .aggregate(
            bucket_keys(group.is_some()),
            columns
                .iter()
                .map(|name| sum(col(*name)).alias(*name))
                .collect::<Vec<_>>(),
        )
        .map_err(classify)?
        .collect()
        .await
        .map_err(|error| ctx.fail(error))?;
    map_rows(
        value_rows(&batches, group.is_some(), grid, &columns)?,
        |values| {
            let [input, output, cache_read, cache_write] = values
                .try_into()
                .map_err(|_| internal("token aggregate width".to_owned()))?;
            Ok(TokenSums {
                input,
                output,
                cache_read,
                cache_write,
            })
        },
    )
}

async fn ttft_rows(
    ctx: &QueryContext,
    request: &MetricsRequest<'_>,
    width: i64,
    grid: &[i64],
    ids: Option<&LogicalPlan>,
) -> Result<BTreeMap<RowKey, Percentiles>, RunError> {
    let group = span_group(request.group_by);
    let mut predicate = kind_is(&[SpanKind::Llm]).and(col(spans::TTFT_NS).is_not_null());
    if let Some(name) = group {
        predicate = predicate.and(col(name).is_not_null());
    }
    let columns = [spans::TTFT_NS];
    let frame = with_bucket(
        span_frame(ctx, request, ids)
            .await?
            .filter(predicate)
            .map_err(classify)?,
        width,
        spans::START_TIME,
        group,
        &columns,
    )?;
    let batches = frame
        .aggregate(
            bucket_keys(group.is_some()),
            vec![
                percentile(spans::TTFT_NS, 0.5).alias(P50),
                percentile(spans::TTFT_NS, 0.95).alias(P95),
            ],
        )
        .map_err(classify)?
        .collect()
        .await
        .map_err(|error| ctx.fail(error))?;
    map_rows(
        value_rows(&batches, group.is_some(), grid, &[P50, P95])?,
        |values| {
            let [p50, p95] = values
                .try_into()
                .map_err(|_| internal("percentile aggregate width".to_owned()))?;
            Ok(Percentiles { p50, p95 })
        },
    )
}

async fn llm_rows(
    ctx: &QueryContext,
    request: &MetricsRequest<'_>,
    width: i64,
    grid: &[i64],
    ids: Option<&LogicalPlan>,
) -> Result<BTreeMap<RowKey, LlmRow>, RunError> {
    let columns = [DURATION_NS, spans::COST_TOTAL];
    let frame = with_bucket(
        span_frame(ctx, request, ids)
            .await?
            .filter(kind_is(&[SpanKind::Llm]).and(col(spans::MODEL).is_not_null()))
            .map_err(classify)?,
        width,
        spans::START_TIME,
        Some(spans::MODEL),
        &columns,
    )?;
    let batches = frame
        .aggregate(
            bucket_keys(true),
            vec![
                percentile(DURATION_NS, 0.5).alias(P50),
                percentile(DURATION_NS, 0.95).alias(P95),
                sum(col(spans::COST_TOTAL)).alias(COST),
            ],
        )
        .map_err(classify)?
        .collect()
        .await
        .map_err(|error| ctx.fail(error))?;
    map_rows(
        value_rows(&batches, true, grid, &[P50, P95, COST])?,
        |values| {
            let [p50, p95, cost] = values
                .try_into()
                .map_err(|_| internal("llm aggregate width".to_owned()))?;
            Ok(LlmRow { p50, p95, cost })
        },
    )
}

async fn score_rows(
    ctx: &QueryContext,
    request: &MetricsRequest<'_>,
    width: i64,
    grid: &[i64],
    ids: Option<&LogicalPlan>,
) -> Result<BTreeMap<RowKey, Option<f64>>, RunError> {
    let mut predicate = request
        .window
        .predicate(scores::CREATED_AT)
        .and(col(scores::TARGET).in_list(
            vec![
                lit(ScoreTargetKind::Trace.as_str()),
                lit(ScoreTargetKind::Span.as_str()),
            ],
            false,
        ))
        .and(col(scores::VALUE_NUM).is_not_null());
    if let Some(plan) = ids {
        predicate = predicate.and(in_subquery(col(scores::TRACE_ID), Arc::new(plan.clone())));
    }
    let frame = with_bucket(
        ctx.session()
            .table("scores")
            .await
            .map_err(classify)?
            .filter(predicate)
            .map_err(classify)?,
        width,
        scores::CREATED_AT,
        Some(scores::NAME),
        &[scores::VALUE_NUM],
    )?;
    let batches = frame
        .aggregate(
            bucket_keys(true),
            vec![avg(col(scores::VALUE_NUM)).alias(VALUE)],
        )
        .map_err(classify)?
        .collect()
        .await
        .map_err(|error| ctx.fail(error))?;
    map_rows(value_rows(&batches, true, grid, &[VALUE])?, |values| {
        let [value] = values
            .try_into()
            .map_err(|_| internal("score aggregate width".to_owned()))?;
        Ok(value)
    })
}

async fn kpi_pair(ctx: &QueryContext, request: &MetricsRequest<'_>) -> Result<Vec<Kpi>, RunError> {
    let current = kpi_range(ctx, request, request.window).await?;
    let previous = kpi_range(ctx, request, previous_window(request.window)?).await?;
    Ok(vec![
        Kpi {
            name: KpiName::Traces,
            value: current.traces,
            previous: previous.traces,
        },
        Kpi {
            name: KpiName::ErrorRate,
            value: current.error_rate,
            previous: previous.error_rate,
        },
        Kpi {
            name: KpiName::LatencyP95,
            value: current.latency_p95,
            previous: previous.latency_p95,
        },
        Kpi {
            name: KpiName::CostTotal,
            value: current.cost,
            previous: previous.cost,
        },
        Kpi {
            name: KpiName::TotalTokens,
            value: current.tokens,
            previous: previous.tokens,
        },
    ])
}

fn previous_window(window: TimeWindow) -> Result<TimeWindow, RunError> {
    let length = window
        .to()
        .checked_sub(window.from())
        .ok_or_else(out_of_range)?;
    let start = window.from().checked_sub(length).ok_or_else(out_of_range)?;
    TimeWindow::new(start, window.from())
}

async fn kpi_range(
    ctx: &QueryContext,
    request: &MetricsRequest<'_>,
    window: TimeWindow,
) -> Result<KpiValues, RunError> {
    let filter = parsed(ctx, request.filter)?;
    let references_environment = filter
        .as_ref()
        .is_some_and(|filter| filter.references_environment);
    let mut frame = scoped_traces(
        ctx,
        ListScope {
            window: Some(window),
            include_eval: request.include_eval,
            include_experiments: request.include_experiments,
        },
        references_environment,
    )
    .await?;
    if let Some(filter) = filter {
        frame = filter.apply(frame, ctx).await.map_err(classify)?;
    }
    let errors = when(col(traces::ERROR_COUNT).gt(lit(0_i64)), lit(1_i64))
        .otherwise(lit(0_i64))
        .map_err(classify)?;
    let batches = frame
        .aggregate(
            vec![],
            vec![
                count(lit(1_i64)).alias(TRACES),
                sum(errors).alias(ERRORS),
                percentile(DURATION_NS, 0.95).alias(P95),
                sum(col(traces::COST_TOTAL)).alias(COST),
                sum(col(traces::TOTAL_TOKENS)).alias(traces::TOTAL_TOKENS),
            ],
        )
        .map_err(classify)?
        .collect()
        .await
        .map_err(|error| ctx.fail(error))?;
    kpi_values(&batches)
}

fn kpi_values(batches: &[RecordBatch]) -> Result<KpiValues, RunError> {
    let mut found = None;
    for batch in batches {
        if batch.num_rows() == 0 {
            continue;
        }
        let traces = require(batch, TRACES)?;
        let errors = require(batch, ERRORS)?;
        let latency = require(batch, P95)?;
        let cost = require(batch, COST)?;
        let tokens = require(batch, traces::TOTAL_TOKENS)?;
        for index in 0..batch.num_rows() {
            if found.is_some() {
                return Err(internal("duplicate kpi row".to_owned()));
            }
            let count = i64_at(traces, index)?;
            found = Some(if count == 0 {
                KpiValues::empty()
            } else {
                KpiValues {
                    traces: Some(count as f64),
                    error_rate: Some(i64_at(errors, index)? as f64 / count as f64),
                    latency_p95: f64_at(latency, index)?,
                    cost: f64_at(cost, index)?,
                    tokens: f64_at(tokens, index)?,
                }
            });
        }
    }
    Ok(found.unwrap_or_else(KpiValues::empty))
}

async fn top_model_rows(
    ctx: &QueryContext,
    request: &MetricsRequest<'_>,
    ids: Option<&LogicalPlan>,
) -> Result<Vec<ModelUsageRow>, RunError> {
    let batches = span_frame(ctx, request, ids)
        .await?
        .filter(kind_is(&[SpanKind::Llm]).and(col(spans::MODEL).is_not_null()))
        .map_err(classify)?
        .aggregate(
            vec![col(spans::MODEL)],
            vec![
                count(lit(1_i64)).alias(LLM_COUNT),
                sum(col(spans::TOTAL_TOKENS)).alias(spans::TOTAL_TOKENS),
                sum(col(spans::COST_TOTAL)).alias(COST),
            ],
        )
        .map_err(classify)?
        .sort(vec![
            col(COST).sort(false, false),
            col(spans::MODEL).sort(true, false),
        ])
        .map_err(classify)?
        .limit(0, Some(TOP_MODELS))
        .map_err(classify)?
        .collect()
        .await
        .map_err(|error| ctx.fail(error))?;
    model_rows(&batches)
}

fn model_rows(batches: &[RecordBatch]) -> Result<Vec<ModelUsageRow>, RunError> {
    let mut rows = Vec::new();
    for batch in batches {
        if batch.num_rows() == 0 {
            continue;
        }
        let models = require(batch, spans::MODEL)?;
        let counts = require(batch, LLM_COUNT)?;
        let tokens = require(batch, spans::TOTAL_TOKENS)?;
        let cost = require(batch, COST)?;
        for index in 0..batch.num_rows() {
            let model =
                string_at(models, index)?.ok_or_else(|| internal("model is null".to_owned()))?;
            rows.push(ModelUsageRow {
                model,
                llm_count: i64_at(counts, index)?,
                total_tokens: i64_opt(tokens, index)?,
                cost_total: f64_at(cost, index)?,
            });
        }
    }
    Ok(rows)
}

fn map_rows<T>(
    raw: BTreeMap<RowKey, Vec<Option<f64>>>,
    convert: impl Fn(Vec<Option<f64>>) -> Result<T, RunError>,
) -> Result<BTreeMap<RowKey, T>, RunError> {
    raw.into_iter()
        .map(|(key, values)| Ok((key, convert(values)?)))
        .collect()
}

fn grouped(frame: DataFrame, group_by: Option<MetricsGroupBy>) -> Result<DataFrame, RunError> {
    match group_by {
        Some(MetricsGroupBy::Model) => frame
            .unnest_columns_with_options(
                &[traces::MODELS],
                UnnestOptions::new().with_null_handling(NullHandling::Drop),
            )
            .map_err(classify)?
            .filter(col(traces::MODELS).is_not_null())
            .map_err(classify),
        Some(MetricsGroupBy::Environment) => frame
            .filter(col(traces::ENVIRONMENT).is_not_null())
            .map_err(classify),
        Some(MetricsGroupBy::Release) => frame
            .filter(col(traces::RELEASE).is_not_null())
            .map_err(classify),
        None => Ok(frame),
    }
}

fn project(
    frame: DataFrame,
    width: i64,
    group_by: Option<MetricsGroupBy>,
) -> Result<DataFrame, RunError> {
    let mut columns = vec![
        bucket_expr(width, traces::START_TIME),
        col(traces::ERROR_COUNT),
        col(DURATION_NS),
        col(traces::COST_TOTAL),
    ];
    if let Some(name) = group_column(group_by) {
        columns.push(col(name).alias(GROUP));
    }
    frame.select(columns).map_err(classify)
}

fn group_column(group_by: Option<MetricsGroupBy>) -> Option<&'static str> {
    match group_by {
        Some(MetricsGroupBy::Model) => Some(traces::MODELS),
        Some(MetricsGroupBy::Environment) => Some(traces::ENVIRONMENT),
        Some(MetricsGroupBy::Release) => Some(traces::RELEASE),
        None => None,
    }
}

/// `date_bin(width, start_time, epoch)` plus `count`, error rows,
/// `approx_percentile_cont(duration_ns, q)` for 0.5/0.95/0.99, and
/// `sum(cost_total)`. The group key is included only when `grouped`.
fn aggregate(frame: DataFrame, grouped: bool) -> Result<DataFrame, RunError> {
    let mut keys = vec![col(BUCKET)];
    if grouped {
        keys.push(col(GROUP));
    }
    let errors = when(col(traces::ERROR_COUNT).gt(lit(0_i64)), lit(1_i64))
        .otherwise(lit(0_i64))
        .map_err(classify)?;
    frame
        .aggregate(
            keys,
            vec![
                count(lit(1_i64)).alias(TRACES),
                sum(errors).alias(ERRORS),
                percentile(DURATION_NS, 0.5).alias(P50),
                percentile(DURATION_NS, 0.95).alias(P95),
                percentile(DURATION_NS, 0.99).alias(P99),
                sum(col(traces::COST_TOTAL)).alias(COST),
            ],
        )
        .map_err(classify)
}

fn percentile(column: &str, quantile: f64) -> Expr {
    approx_percentile_cont(SortExpr::new(col(column), true, true), lit(quantile), None)
}

fn bucket_expr(width: i64, time: &str) -> Expr {
    date_bin(
        Expr::Literal(ScalarValue::new_interval_mdn(0, 0, width), None),
        col(time),
        Expr::Literal(
            ScalarValue::TimestampNanosecond(Some(0), Some("UTC".into())),
            None,
        ),
    )
    .alias(BUCKET)
}

fn rows_from(
    batches: &[RecordBatch],
    grouped: bool,
    grid: &[i64],
) -> Result<BTreeMap<RowKey, TraceRow>, RunError> {
    let mut rows = BTreeMap::new();
    for batch in batches {
        if batch.num_rows() == 0 {
            continue;
        }
        let times = require(batch, BUCKET)?;
        let traces = require(batch, TRACES)?;
        let errors = require(batch, ERRORS)?;
        let p50 = require(batch, P50)?;
        let p95 = require(batch, P95)?;
        let p99 = require(batch, P99)?;
        let cost = require(batch, COST)?;
        let groups = if grouped {
            Some(require(batch, GROUP)?)
        } else {
            None
        };
        for index in 0..batch.num_rows() {
            let time = time_at(times, index)?;
            on_grid(grid, time)?;
            let group = match groups {
                Some(column) => Some(
                    string_at(column, index)?
                        .ok_or_else(|| internal("group key is null".to_owned()))?,
                ),
                None => None,
            };
            let row = TraceRow {
                traces: i64_at(traces, index)?,
                errors: i64_at(errors, index)?,
                p50: f64_at(p50, index)?,
                p95: f64_at(p95, index)?,
                p99: f64_at(p99, index)?,
                cost: f64_at(cost, index)?,
            };
            if rows.insert(RowKey { time, group }, row).is_some() {
                return Err(internal("duplicate aggregate row".to_owned()));
            }
        }
    }
    Ok(rows)
}

fn cost_rows(
    batches: &[RecordBatch],
    grid: &[i64],
) -> Result<BTreeMap<i64, Option<f64>>, RunError> {
    let mut rows = BTreeMap::new();
    for batch in batches {
        if batch.num_rows() == 0 {
            continue;
        }
        let times = require(batch, BUCKET)?;
        let cost = require(batch, COST)?;
        for index in 0..batch.num_rows() {
            let time = time_at(times, index)?;
            on_grid(grid, time)?;
            if rows.insert(time, f64_at(cost, index)?).is_some() {
                return Err(internal("duplicate eval-cost bucket".to_owned()));
            }
        }
    }
    Ok(rows)
}

fn on_grid(grid: &[i64], time: i64) -> Result<(), RunError> {
    if grid.binary_search(&time).is_ok() {
        Ok(())
    } else {
        Err(internal(format!("bucket {time} is outside the grid")))
    }
}

fn value_rows(
    batches: &[RecordBatch],
    grouped: bool,
    grid: &[i64],
    columns: &[&str],
) -> Result<BTreeMap<RowKey, Vec<Option<f64>>>, RunError> {
    let mut rows = BTreeMap::new();
    for batch in batches {
        if batch.num_rows() == 0 {
            continue;
        }
        let times = require(batch, BUCKET)?;
        let groups = if grouped {
            Some(require(batch, GROUP)?)
        } else {
            None
        };
        let values = columns
            .iter()
            .map(|name| require(batch, name))
            .collect::<Result<Vec<_>, _>>()?;
        for index in 0..batch.num_rows() {
            let time = time_at(times, index)?;
            on_grid(grid, time)?;
            let group = match groups {
                Some(column) => Some(
                    string_at(column, index)?
                        .ok_or_else(|| internal("group key is null".to_owned()))?,
                ),
                None => None,
            };
            let picked = values
                .iter()
                .map(|column| f64_at(*column, index))
                .collect::<Result<Vec<_>, _>>()?;
            if rows.insert(RowKey { time, group }, picked).is_some() {
                return Err(internal("duplicate aggregate row".to_owned()));
            }
        }
    }
    Ok(rows)
}

fn assemble(
    width: i64,
    grid: &[i64],
    group_by: Option<MetricsGroupBy>,
    rows: &SeriesRows,
    kpis: Vec<Kpi>,
    top_models: Vec<ModelUsageRow>,
) -> MetricsData {
    let groups = group_keys(group_by, &rows.traces);
    let mut series = Vec::new();
    fill(
        &mut series,
        SeriesName::Traces,
        &groups,
        grid,
        &rows.traces,
        traces_of,
    );
    fill(
        &mut series,
        SeriesName::Errors,
        &groups,
        grid,
        &rows.traces,
        errors_of,
    );
    fill(
        &mut series,
        SeriesName::ErrorRate,
        &groups,
        grid,
        &rows.traces,
        rate_of,
    );
    fill(
        &mut series,
        SeriesName::LatencyP50,
        &groups,
        grid,
        &rows.traces,
        p50_of,
    );
    fill(
        &mut series,
        SeriesName::LatencyP95,
        &groups,
        grid,
        &rows.traces,
        p95_of,
    );
    fill(
        &mut series,
        SeriesName::LatencyP99,
        &groups,
        grid,
        &rows.traces,
        p99_of,
    );
    fill(
        &mut series,
        SeriesName::Cost,
        &groups,
        grid,
        &rows.traces,
        cost_of,
    );
    let token_groups = group_keys(group_by, &rows.tokens);
    fill(
        &mut series,
        SeriesName::InputTokens,
        &token_groups,
        grid,
        &rows.tokens,
        input_of,
    );
    fill(
        &mut series,
        SeriesName::OutputTokens,
        &token_groups,
        grid,
        &rows.tokens,
        output_of,
    );
    fill(
        &mut series,
        SeriesName::CacheReadTokens,
        &token_groups,
        grid,
        &rows.tokens,
        cache_read_of,
    );
    fill(
        &mut series,
        SeriesName::CacheWriteTokens,
        &token_groups,
        grid,
        &rows.tokens,
        cache_write_of,
    );
    let ttft_groups = group_keys(group_by, &rows.ttft);
    fill(
        &mut series,
        SeriesName::TtftP50,
        &ttft_groups,
        grid,
        &rows.ttft,
        pair_p50,
    );
    fill(
        &mut series,
        SeriesName::TtftP95,
        &ttft_groups,
        grid,
        &rows.ttft,
        pair_p95,
    );
    let models = present_groups(&rows.llm);
    fill(
        &mut series,
        SeriesName::LlmLatencyP50,
        &models,
        grid,
        &rows.llm,
        llm_p50,
    );
    fill(
        &mut series,
        SeriesName::LlmLatencyP95,
        &models,
        grid,
        &rows.llm,
        llm_p95,
    );
    fill(
        &mut series,
        SeriesName::CostByModel,
        &models,
        grid,
        &rows.llm,
        llm_cost,
    );
    let names = present_groups(&rows.scores);
    fill(
        &mut series,
        SeriesName::ScoreAvg,
        &names,
        grid,
        &rows.scores,
        score_of,
    );
    series.push(Series {
        name: SeriesName::EvalCost,
        group: None,
        points: grid
            .iter()
            .map(|time| Point {
                time: *time,
                value: rows.eval_cost.get(time).copied().flatten(),
            })
            .collect(),
    });
    series.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.group.cmp(&right.group))
    });
    MetricsData {
        bucket_ns: width,
        kpis,
        series,
        top_models,
    }
}

fn group_keys<T>(
    group_by: Option<MetricsGroupBy>,
    rows: &BTreeMap<RowKey, T>,
) -> Vec<Option<String>> {
    if group_by.is_none() {
        vec![None]
    } else {
        present_groups(rows)
    }
}

fn present_groups<T>(rows: &BTreeMap<RowKey, T>) -> Vec<Option<String>> {
    let mut groups: Vec<String> = rows.keys().filter_map(|key| key.group.clone()).collect();
    groups.sort();
    groups.dedup();
    groups.into_iter().map(Some).collect()
}

fn fill<T>(
    series: &mut Vec<Series>,
    name: SeriesName,
    groups: &[Option<String>],
    grid: &[i64],
    rows: &BTreeMap<RowKey, T>,
    pick: fn(Option<&T>) -> Option<f64>,
) {
    for group in groups {
        let points = grid
            .iter()
            .map(|time| Point {
                time: *time,
                value: pick(rows.get(&RowKey {
                    time: *time,
                    group: group.clone(),
                })),
            })
            .collect();
        series.push(Series {
            name,
            group: group.clone(),
            points,
        });
    }
}

fn traces_of(row: Option<&TraceRow>) -> Option<f64> {
    row.map(|row| row.traces as f64)
}

fn errors_of(row: Option<&TraceRow>) -> Option<f64> {
    row.map(|row| row.errors as f64)
}

/// Errors divided by traces. An empty bucket has no row, so the point stays
/// `None` rather than zero.
fn rate_of(row: Option<&TraceRow>) -> Option<f64> {
    let row = row?;
    if row.traces == 0 {
        None
    } else {
        Some(row.errors as f64 / row.traces as f64)
    }
}

fn p50_of(row: Option<&TraceRow>) -> Option<f64> {
    row.and_then(|row| row.p50)
}

fn p95_of(row: Option<&TraceRow>) -> Option<f64> {
    row.and_then(|row| row.p95)
}

fn p99_of(row: Option<&TraceRow>) -> Option<f64> {
    row.and_then(|row| row.p99)
}

fn cost_of(row: Option<&TraceRow>) -> Option<f64> {
    row.and_then(|row| row.cost)
}

fn input_of(row: Option<&TokenSums>) -> Option<f64> {
    row.and_then(|row| row.input)
}

fn output_of(row: Option<&TokenSums>) -> Option<f64> {
    row.and_then(|row| row.output)
}

fn cache_read_of(row: Option<&TokenSums>) -> Option<f64> {
    row.and_then(|row| row.cache_read)
}

fn cache_write_of(row: Option<&TokenSums>) -> Option<f64> {
    row.and_then(|row| row.cache_write)
}

fn pair_p50(row: Option<&Percentiles>) -> Option<f64> {
    row.and_then(|row| row.p50)
}

fn pair_p95(row: Option<&Percentiles>) -> Option<f64> {
    row.and_then(|row| row.p95)
}

fn llm_p50(row: Option<&LlmRow>) -> Option<f64> {
    row.and_then(|row| row.p50)
}

fn llm_p95(row: Option<&LlmRow>) -> Option<f64> {
    row.and_then(|row| row.p95)
}

fn llm_cost(row: Option<&LlmRow>) -> Option<f64> {
    row.and_then(|row| row.cost)
}

fn score_of(row: Option<&Option<f64>>) -> Option<f64> {
    row.copied().flatten()
}

fn require<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a dyn Array, RunError> {
    match batch.column_by_name(name) {
        Some(column) => Ok(column.as_ref()),
        None => Err(missing_column(batch, name)),
    }
}

fn missing_column(batch: &RecordBatch, name: &str) -> RunError {
    let schema = batch.schema();
    let columns: Vec<&str> = schema
        .fields()
        .iter()
        .map(|field| field.name().as_str())
        .collect();
    internal(format!("missing column {name}; columns {columns:?}"))
}

fn time_at(column: &dyn Array, index: usize) -> Result<i64, RunError> {
    let Some(values) = column.as_primitive_opt::<TimestampNanosecondType>() else {
        return Err(internal(format!("bucket has type {}", column.data_type())));
    };
    if values.is_null(index) {
        return Err(internal("bucket is null".to_owned()));
    }
    Ok(values.value(index))
}

fn i64_at(column: &dyn Array, index: usize) -> Result<i64, RunError> {
    if let Some(values) = column.as_primitive_opt::<Int64Type>() {
        if values.is_null(index) {
            return Err(internal("aggregate count is null".to_owned()));
        }
        return Ok(values.value(index));
    }
    if let Some(values) = column.as_primitive_opt::<UInt64Type>() {
        if values.is_null(index) {
            return Err(internal("aggregate count is null".to_owned()));
        }
        return i64::try_from(values.value(index))
            .map_err(|_| internal("aggregate count overflows i64".to_owned()));
    }
    Err(internal(format!(
        "aggregate count has type {}",
        column.data_type()
    )))
}

fn i64_opt(column: &dyn Array, index: usize) -> Result<Option<i64>, RunError> {
    let is_int = column.as_primitive_opt::<Int64Type>().is_some()
        || column.as_primitive_opt::<UInt64Type>().is_some();
    if !is_int {
        return Err(internal(format!(
            "aggregate count has type {}",
            column.data_type()
        )));
    }
    if column.is_null(index) {
        return Ok(None);
    }
    Ok(Some(i64_at(column, index)?))
}

fn f64_at(column: &dyn Array, index: usize) -> Result<Option<f64>, RunError> {
    if let Some(values) = column.as_primitive_opt::<Float64Type>() {
        return Ok((!values.is_null(index)).then(|| values.value(index)));
    }
    if let Some(values) = column.as_primitive_opt::<Int64Type>() {
        return Ok((!values.is_null(index)).then(|| values.value(index) as f64));
    }
    Err(internal(format!(
        "aggregate value has type {}",
        column.data_type()
    )))
}

fn string_at(column: &dyn Array, index: usize) -> Result<Option<String>, RunError> {
    if column.is_null(index) {
        return Ok(None);
    }
    if let Some(values) = column.as_string_opt::<i32>() {
        return Ok(Some(values.value(index).to_owned()));
    }
    if let Some(values) = column.as_string_opt::<i64>() {
        return Ok(Some(values.value(index).to_owned()));
    }
    if let Some(values) = column.as_string_view_opt() {
        return Ok(Some(values.value(index).to_owned()));
    }
    Err(internal(format!(
        "group key has type {}",
        column.data_type()
    )))
}

fn internal(message: String) -> RunError {
    RunError::Internal(DataFusionError::Internal(message))
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;
    use std::collections::{BTreeMap, BTreeSet};
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::time::Duration;

    use super::resolve;
    use datafusion::execution::runtime_env::RuntimeEnv;
    use llmobs_core::enums::SpanKind;
    use llmobs_core::ids::{SpanId, TraceId};
    use llmobs_core::names::environment;
    use llmobs_core::score::ScoreTarget;
    use llmobs_core::span::Span;
    use llmobs_core::time_serde::parse_rfc3339;
    use serde_json::Value;

    use super::{
        Bucket, BucketWidth, Kpi, KpiName, MetricsData, MetricsGroupBy, MetricsRequest,
        ModelUsageRow, Series, SeriesName, metrics,
    };
    use crate::query_error::RunError;
    use crate::test_support::{
        dataset_scores, dataset_spans, example_query, live_scores, live_scores_table, spans_table,
        traces_table,
    };
    use crate::views::{ProjectQuery, QueryContext, TimeWindow};

    const TIMEOUT: Duration = Duration::from_secs(30);
    const M10: i64 = 600_000_000_000;
    const MINUTE: i64 = 60_000_000_000;
    const DAY: i64 = 86_400_000_000_000;

    struct Sample {
        start: i64,
        duration: i64,
        error_count: i64,
        cost: Option<f64>,
        total_tokens: Option<i64>,
        models: Vec<String>,
        environment: Option<String>,
        status: String,
    }

    fn context() -> QueryContext {
        QueryContext::new(example_query(), None).expect("context")
    }

    fn at(text: &str) -> i64 {
        parse_rfc3339(text).unwrap_or_else(|_| panic!("time {text}"))
    }

    fn request<'a>(from: &str, to: &str, bucket: Bucket) -> MetricsRequest<'a> {
        MetricsRequest {
            window: TimeWindow::new(at(from), at(to)).expect("window"),
            bucket,
            filter: None,
            group_by: None,
            include_eval: false,
            include_experiments: false,
            timeout: TIMEOUT,
        }
    }

    async fn run(request: MetricsRequest<'_>) -> MetricsData {
        metrics(&context(), request)
            .await
            .unwrap_or_else(|error| panic!("{error:?}"))
    }

    fn samples(name: &str) -> Vec<Sample> {
        let text = match name {
            "traces.json" => include_str!("../../../../fixtures/api/responses/traces.json"),
            "traces-include-eval.json" => {
                include_str!("../../../../fixtures/api/responses/traces-include-eval.json")
            }
            other => panic!("unknown fixture {other}"),
        };
        let value: Value =
            serde_json::from_str(text).unwrap_or_else(|error| panic!("{name}: {error}"));
        value["data"]
            .as_array()
            .unwrap_or_else(|| panic!("{name} data"))
            .iter()
            .map(|row| Sample {
                start: at(row["start_time"].as_str().expect("start")),
                duration: row["duration_ns"].as_i64().expect("duration"),
                error_count: row["error_count"].as_i64().expect("errors"),
                cost: row.get("cost_total").and_then(Value::as_f64),
                total_tokens: row.get("total_tokens").and_then(Value::as_i64),
                models: row["models"]
                    .as_array()
                    .expect("models")
                    .iter()
                    .map(|model| model.as_str().expect("model").to_owned())
                    .collect(),
                environment: row
                    .get("environment")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                status: row["status"].as_str().expect("status").to_owned(),
            })
            .collect()
    }

    fn in_bucket(samples: &[Sample], from: i64, to: i64, bucket: i64, width: i64) -> Vec<&Sample> {
        samples
            .iter()
            .filter(|sample| {
                sample.start >= from
                    && sample.start < to
                    && sample.start - sample.start.rem_euclid(width) == bucket
            })
            .collect()
    }

    fn named<'a>(data: &'a MetricsData, name: SeriesName, group: Option<&str>) -> &'a Series {
        data.series
            .iter()
            .find(|series| series.name == name && series.group.as_deref() == group)
            .unwrap_or_else(|| panic!("missing {name:?} group={group:?}"))
    }

    fn value_at(series: &Series, time: i64) -> Option<f64> {
        series
            .points
            .iter()
            .find(|point| point.time == time)
            .unwrap_or_else(|| panic!("no point at {time}"))
            .value
    }

    fn assert_times(data: &MetricsData, bucket_ns: i64, times: &[i64]) {
        assert_eq!(data.bucket_ns, bucket_ns);
        assert_eq!(data.kpis.len(), 5);
        let mut ordered = data.series.clone();
        ordered.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.group.cmp(&right.group))
        });
        assert_eq!(data.series, ordered);
        for series in &data.series {
            if !split_by_itself(series.name) {
                assert!(series.group.is_none(), "{:?} is grouped", series.name);
            }
            let actual: Vec<i64> = series.points.iter().map(|point| point.time).collect();
            assert_eq!(actual, times, "{:?}", series.name);
        }
    }

    fn split_by_itself(name: SeriesName) -> bool {
        matches!(
            name,
            SeriesName::LlmLatencyP50
                | SeriesName::LlmLatencyP95
                | SeriesName::CostByModel
                | SeriesName::ScoreAvg
        )
    }

    fn assert_close(actual: Option<f64>, expected: Option<f64>) {
        match (actual, expected) {
            (None, None) => {}
            (Some(actual), Some(expected)) => {
                assert!((actual - expected).abs() < 1e-12, "{actual} vs {expected}");
            }
            other => panic!("{other:?}"),
        }
    }

    fn cost_sum(rows: &[&Sample]) -> Option<f64> {
        if rows.is_empty() {
            return None;
        }
        let mut sum = 0.0;
        let mut any = false;
        for row in rows {
            if let Some(cost) = row.cost {
                sum += cost;
                any = true;
            }
        }
        any.then_some(sum)
    }

    #[tokio::test]
    async fn bucket_grid_follows_the_width() {
        let auto = run(request(
            "2026-01-01T10:00:00Z",
            "2026-01-01T11:00:00Z",
            Bucket::Auto,
        ))
        .await;
        let start = at("2026-01-01T10:00:00Z");
        let minute: Vec<i64> = (0..60).map(|step| start + step * MINUTE).collect();
        assert_eq!(minute[59], at("2026-01-01T10:59:00Z"));
        assert_times(&auto, MINUTE, &minute);
        assert_eq!(BucketWidth::M1.ns(), MINUTE);

        let ten = run(request(
            "2026-01-01T10:00:00Z",
            "2026-01-01T11:00:00Z",
            Bucket::Fixed(BucketWidth::M10),
        ))
        .await;
        let six = [
            "2026-01-01T10:00:00Z",
            "2026-01-01T10:10:00Z",
            "2026-01-01T10:20:00Z",
            "2026-01-01T10:30:00Z",
            "2026-01-01T10:40:00Z",
            "2026-01-01T10:50:00Z",
        ]
        .map(at);
        assert_times(&ten, M10, &six);
        assert_eq!(BucketWidth::M10.ns(), M10);

        let partial = run(request(
            "2026-01-01T10:03:00Z",
            "2026-01-01T10:27:00Z",
            Bucket::Fixed(BucketWidth::M10),
        ))
        .await;
        let three = [
            "2026-01-01T10:00:00Z",
            "2026-01-01T10:10:00Z",
            "2026-01-01T10:20:00Z",
        ]
        .map(at);
        assert_times(&partial, M10, &three);
        let from = at("2026-01-01T10:03:00Z");
        let to = at("2026-01-01T10:27:00Z");
        let expected = in_bucket(&samples("traces.json"), from, to, three[0], M10).len();
        assert_eq!(
            value_at(named(&partial, SeriesName::Traces, None), three[0]),
            Some(expected as f64)
        );

        let day = run(request(
            "2026-01-01T10:00:00Z",
            "2026-01-01T10:30:00Z",
            Bucket::Fixed(BucketWidth::D1),
        ))
        .await;
        assert_times(&day, DAY, &[at("2026-01-01T00:00:00Z")]);
        assert_eq!(BucketWidth::D1.ns(), DAY);
    }

    #[tokio::test]
    async fn fixed_m10_matches_traces_json() {
        let from = at("2026-01-01T10:00:00Z");
        let to = at("2026-01-01T11:00:00Z");
        let data = run(request(
            "2026-01-01T10:00:00Z",
            "2026-01-01T11:00:00Z",
            Bucket::Fixed(BucketWidth::M10),
        ))
        .await;
        let loaded = samples("traces.json");
        for series in &data.series {
            for point in &series.points {
                let rows = in_bucket(&loaded, from, to, point.time, M10);
                match series.name {
                    SeriesName::Traces => {
                        let expected = if rows.is_empty() {
                            None
                        } else {
                            Some(rows.len() as f64)
                        };
                        assert_eq!(point.value, expected);
                    }
                    SeriesName::Errors => {
                        let expected = if rows.is_empty() {
                            None
                        } else {
                            Some(rows.iter().filter(|row| row.error_count > 0).count() as f64)
                        };
                        assert_eq!(point.value, expected);
                    }
                    SeriesName::ErrorRate => {
                        let expected = if rows.is_empty() {
                            None
                        } else {
                            let errors = rows.iter().filter(|row| row.error_count > 0).count();
                            Some(errors as f64 / rows.len() as f64)
                        };
                        assert_close(point.value, expected);
                    }
                    SeriesName::Cost => assert_close(point.value, cost_sum(&rows)),
                    _ => {}
                }
            }
        }
    }

    #[tokio::test]
    async fn latency_is_exact_for_one_trace_and_inside_the_range() {
        let from = at("2026-01-01T10:00:00Z");
        let to = at("2026-01-01T11:00:00Z");
        let data = run(request(
            "2026-01-01T10:00:00Z",
            "2026-01-01T11:00:00Z",
            Bucket::Fixed(BucketWidth::M10),
        ))
        .await;
        let loaded = samples("traces.json");
        for point in &named(&data, SeriesName::Traces, None).points {
            let rows = in_bucket(&loaded, from, to, point.time, M10);
            let p50 = value_at(named(&data, SeriesName::LatencyP50, None), point.time);
            let p95 = value_at(named(&data, SeriesName::LatencyP95, None), point.time);
            let p99 = value_at(named(&data, SeriesName::LatencyP99, None), point.time);
            let durations: Vec<i64> = rows.iter().map(|row| row.duration).collect();
            if durations.len() == 1 {
                let exact = Some(durations[0] as f64);
                assert_eq!(p50, exact);
                assert_eq!(p95, exact);
                assert_eq!(p99, exact);
            } else if durations.len() > 1 {
                let min = *durations.iter().min().expect("min") as f64;
                let max = *durations.iter().max().expect("max") as f64;
                for value in [p50, p95, p99] {
                    let value = value.expect("percentile");
                    assert!(value >= min && value <= max, "{value} not in {min}..={max}");
                }
            } else {
                assert_eq!(p50, None);
                assert_eq!(p95, None);
                assert_eq!(p99, None);
            }
        }
    }

    #[tokio::test]
    async fn group_by_model_counts_each_model_once_per_trace() {
        let from = at("2026-01-01T10:00:00Z");
        let to = at("2026-01-01T11:00:00Z");
        let mut request = request(
            "2026-01-01T10:00:00Z",
            "2026-01-01T11:00:00Z",
            Bucket::Fixed(BucketWidth::M10),
        );
        request.group_by = Some(MetricsGroupBy::Model);
        let data = run(request).await;
        let loaded = samples("traces.json");
        let models: BTreeSet<_> = loaded
            .iter()
            .filter(|sample| sample.start >= from && sample.start < to)
            .flat_map(|sample| sample.models.iter().cloned())
            .collect();
        let actual: BTreeSet<_> = data
            .series
            .iter()
            .filter(|series| series.name == SeriesName::Traces)
            .map(|series| series.group.clone().expect("model group"))
            .collect();
        assert_eq!(actual, models);
        assert!(
            data.series
                .iter()
                .all(|series| { series.name == SeriesName::EvalCost || series.group.is_some() })
        );
        assert!(named(&data, SeriesName::EvalCost, None).group.is_none());
        let mut ordered = data.series.clone();
        ordered.sort_by(|left, right| {
            left.name
                .cmp(&right.name)
                .then_with(|| left.group.cmp(&right.group))
        });
        assert_eq!(data.series, ordered);
        for time in named(&data, SeriesName::EvalCost, None)
            .points
            .iter()
            .map(|point| point.time)
        {
            let pairs: usize = in_bucket(&loaded, from, to, time, M10)
                .iter()
                .map(|row| row.models.len())
                .sum();
            let sum: f64 = data
                .series
                .iter()
                .filter(|series| series.name == SeriesName::Traces)
                .map(|series| value_at(series, time).unwrap_or(0.0))
                .sum();
            if pairs == 0 {
                assert!(
                    data.series
                        .iter()
                        .filter(|series| series.name == SeriesName::Traces)
                        .all(|series| value_at(series, time).is_none())
                );
            } else {
                assert!((sum - pairs as f64).abs() < 1e-12, "{sum} vs {pairs}");
            }
        }
    }

    #[tokio::test]
    async fn group_by_environment_includes_eval_and_skips_null() {
        let from = at("2026-01-01T10:00:00Z");
        let to = at("2026-01-01T11:00:00Z");
        let mut request = request(
            "2026-01-01T10:00:00Z",
            "2026-01-01T11:00:00Z",
            Bucket::Fixed(BucketWidth::M10),
        );
        request.group_by = Some(MetricsGroupBy::Environment);
        request.include_eval = true;
        let data = run(request).await;
        let loaded = samples("traces-include-eval.json");
        // The fixture has no null environment. Groups are exactly those values,
        // so a null key is not returned.
        assert!(loaded.iter().all(|sample| sample.environment.is_some()));
        let expected: BTreeSet<_> = loaded
            .iter()
            .filter(|sample| sample.start >= from && sample.start < to)
            .filter_map(|sample| sample.environment.clone())
            .collect();
        let actual: BTreeSet<_> = data
            .series
            .iter()
            .filter(|series| series.name == SeriesName::Traces)
            .map(|series| series.group.clone().expect("environment"))
            .collect();
        assert_eq!(actual, expected);
        assert!(actual.contains(environment::EVAL));
        assert!(
            data.series
                .iter()
                .all(|series| { series.name == SeriesName::EvalCost || series.group.is_some() })
        );
    }

    #[tokio::test]
    async fn eval_cost_ignores_include_eval_and_is_none_when_filtered_out() {
        let raw: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/api/responses/traces-include-eval.json"
        ))
        .expect("include-eval");
        let eval = raw["data"]
            .as_array()
            .expect("data")
            .iter()
            .find(|row| row["environment"].as_str() == Some(environment::EVAL))
            .expect("eval trace");
        // Trace 5 starts at 10:01 with cost 0.0008. Its metadata is empty, so
        // the experiment exclusion does not remove it.
        assert_eq!(eval["start_time"], "2026-01-01T10:01:00.000000000Z");
        assert!(eval["metadata"].as_object().expect("metadata").is_empty());
        let cost = eval["cost_total"].as_f64().expect("cost");
        assert!((cost - 0.0008).abs() < 1e-12);

        let hidden = run(request(
            "2026-01-01T10:00:00Z",
            "2026-01-01T11:00:00Z",
            Bucket::Fixed(BucketWidth::M10),
        ))
        .await;
        let mut included = request(
            "2026-01-01T10:00:00Z",
            "2026-01-01T11:00:00Z",
            Bucket::Fixed(BucketWidth::M10),
        );
        included.include_eval = true;
        let included = run(included).await;
        let hidden_cost = named(&hidden, SeriesName::EvalCost, None);
        let included_cost = named(&included, SeriesName::EvalCost, None);
        assert_eq!(hidden_cost, included_cost);
        let bucket = at("2026-01-01T10:00:00Z");
        assert_close(value_at(hidden_cost, bucket), Some(cost));
        for point in &hidden_cost.points {
            if point.time != bucket {
                assert_eq!(point.value, None);
            }
        }

        // No eval row matches, so the sum is absent rather than zero.
        let mut filtered = request(
            "2026-01-01T10:00:00Z",
            "2026-01-01T11:00:00Z",
            Bucket::Fixed(BucketWidth::M10),
        );
        filtered.filter = Some("environment = 'production'");
        let filtered = run(filtered).await;
        assert!(
            named(&filtered, SeriesName::EvalCost, None)
                .points
                .iter()
                .all(|point| point.value.is_none())
        );
    }

    #[tokio::test]
    async fn status_filter_keeps_error_traces_and_a_parse_error_is_invalid() {
        let from = at("2026-01-01T10:00:00Z");
        let to = at("2026-01-01T11:00:00Z");
        let mut filtered = request(
            "2026-01-01T10:00:00Z",
            "2026-01-01T11:00:00Z",
            Bucket::Fixed(BucketWidth::M10),
        );
        filtered.filter = Some("status = 'error'");
        let data = run(filtered).await;
        let loaded = samples("traces.json");
        let traces = named(&data, SeriesName::Traces, None);
        for point in &traces.points {
            let rows = in_bucket(&loaded, from, to, point.time, M10);
            let errors = rows.iter().filter(|row| row.status == "error").count();
            let expected = if errors == 0 {
                None
            } else {
                Some(errors as f64)
            };
            assert_eq!(point.value, expected);
        }

        let mut broken = request(
            "2026-01-01T10:00:00Z",
            "2026-01-01T11:00:00Z",
            Bucket::Fixed(BucketWidth::M10),
        );
        broken.filter = Some("status =");
        let error = metrics(&context(), broken).await.expect_err("parse error");
        assert!(matches!(error, RunError::Invalid(_)), "{error:?}");
    }

    #[tokio::test]
    async fn two_thousand_one_buckets_are_rejected() {
        let span = 2_001i64.checked_mul(BucketWidth::M1.ns()).expect("span");
        let request = MetricsRequest {
            window: TimeWindow::new(0, span).expect("window"),
            bucket: Bucket::Fixed(BucketWidth::M1),
            filter: None,
            group_by: None,
            include_eval: false,
            include_experiments: false,
            timeout: TIMEOUT,
        };
        match metrics(&context(), request).await {
            Err(RunError::InvalidRequest(message)) => {
                assert_eq!(message, "too many buckets; pick a wider bucket");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn auto_buckets_stop_at_two_thousand_days() {
        let day = BucketWidth::D1.ns();
        let window = |from, to| TimeWindow::new(from, to).expect("window");
        assert_eq!(
            resolve(window(0, 2_000 * day), Bucket::Auto).expect("2,000 days"),
            day
        );
        for to in [2_000 * day + 1, i64::MAX] {
            match resolve(window(0, to), Bucket::Auto) {
                Err(RunError::InvalidRequest(message)) => {
                    assert_eq!(message, "too many buckets; pick a wider bucket");
                }
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(
            resolve(window(i64::MIN, i64::MAX), Bucket::Auto),
            Err(RunError::InvalidRequest(_))
        ));
    }

    #[tokio::test]
    async fn metrics_reject_an_auto_window_past_two_thousand_days() {
        let span = 2_001i64.checked_mul(BucketWidth::D1.ns()).expect("span");
        let request = MetricsRequest {
            window: TimeWindow::new(0, span).expect("window"),
            bucket: Bucket::Auto,
            filter: None,
            group_by: None,
            include_eval: false,
            include_experiments: false,
            timeout: TIMEOUT,
        };
        match metrics(&context(), request).await {
            Err(RunError::InvalidRequest(message)) => {
                assert_eq!(message, "too many buckets; pick a wider bucket");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn exactly_two_thousand_fixed_buckets_are_accepted() {
        let span = 2_000i64.checked_mul(BucketWidth::M1.ns()).expect("span");
        assert_eq!(
            resolve(
                TimeWindow::new(0, span).expect("window"),
                Bucket::Fixed(BucketWidth::M1)
            )
            .expect("2,000 buckets"),
            BucketWidth::M1.ns()
        );
    }

    #[derive(Clone)]
    struct SpanSample {
        trace_id: String,
        start: i64,
        duration: i64,
        kind: String,
        model: Option<String>,
        input_tokens: Option<i64>,
        output_tokens: Option<i64>,
        cache_read_tokens: Option<i64>,
        cache_write_tokens: Option<i64>,
        total_tokens: Option<i64>,
        ttft_ns: Option<i64>,
        cost_total: Option<f64>,
    }

    fn span_samples() -> Vec<SpanSample> {
        dataset_spans()
            .into_iter()
            .map(|span| SpanSample {
                trace_id: span.trace_id.to_string(),
                start: span.start_time,
                duration: span.end_time.saturating_sub(span.start_time),
                kind: span.span_kind.as_str().to_owned(),
                model: span.response_model.or(span.request_model),
                input_tokens: span.input_tokens,
                output_tokens: span.output_tokens,
                cache_read_tokens: span.cache_read_tokens,
                cache_write_tokens: span.cache_write_tokens,
                total_tokens: span.total_tokens,
                ttft_ns: span.ttft_ns,
                cost_total: span.cost_total,
            })
            .collect()
    }

    fn fixture_text(name: &str) -> &'static str {
        match name {
            "traces.json" => include_str!("../../../../fixtures/api/responses/traces.json"),
            "traces-include-eval.json" => {
                include_str!("../../../../fixtures/api/responses/traces-include-eval.json")
            }
            other => panic!("unknown fixture {other}"),
        }
    }

    /// Trace ids whose `start_time` is in `[from - 1 day, to)` and `end_time >= from`.
    fn scoped_ids(name: &str, from: i64, to: i64) -> BTreeSet<String> {
        let value: Value = serde_json::from_str(fixture_text(name)).expect(name);
        let lookback = from - DAY;
        value["data"]
            .as_array()
            .expect("data")
            .iter()
            .filter_map(|row| {
                let start = at(row["start_time"].as_str().expect("start"));
                let end = at(row["end_time"].as_str().expect("end"));
                (start >= lookback && start < to && end >= from)
                    .then(|| row["trace_id"].as_str().expect("trace").to_owned())
            })
            .collect()
    }

    fn kept_spans(ids: &BTreeSet<String>, from: i64, to: i64, kinds: &[&str]) -> Vec<SpanSample> {
        span_samples()
            .into_iter()
            .filter(|span| {
                ids.contains(&span.trace_id)
                    && span.start >= from
                    && span.start < to
                    && kinds.contains(&span.kind.as_str())
            })
            .collect()
    }

    fn bucket_spans(spans: &[SpanSample], bucket: i64, width: i64) -> Vec<&SpanSample> {
        spans
            .iter()
            .filter(|span| span.start - span.start.rem_euclid(width) == bucket)
            .collect()
    }

    fn sum_i64(values: impl Iterator<Item = Option<i64>>) -> Option<f64> {
        let mut total = None;
        for value in values.flatten() {
            total = Some(total.unwrap_or(0) + value);
        }
        total.map(|value| value as f64)
    }

    fn sum_f64(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
        let mut total = None;
        for value in values.flatten() {
            total = Some(total.unwrap_or(0.0) + value);
        }
        total
    }

    fn assert_column(
        data: &MetricsData,
        name: SeriesName,
        group: Option<&str>,
        spans: &[SpanSample],
        width: i64,
        pick: fn(&SpanSample) -> Option<i64>,
    ) {
        for point in &named(data, name, group).points {
            let rows = bucket_spans(spans, point.time, width);
            assert_close(point.value, sum_i64(rows.into_iter().map(pick)));
        }
    }

    fn assert_tokens(data: &MetricsData, ids: &BTreeSet<String>, from: i64, to: i64) {
        let spans = kept_spans(
            ids,
            from,
            to,
            &[SpanKind::Llm.as_str(), SpanKind::Embedding.as_str()],
        );
        assert_column(data, SeriesName::InputTokens, None, &spans, M10, |span| {
            span.input_tokens
        });
        assert_column(data, SeriesName::OutputTokens, None, &spans, M10, |span| {
            span.output_tokens
        });
        assert_column(
            data,
            SeriesName::CacheReadTokens,
            None,
            &spans,
            M10,
            |span| span.cache_read_tokens,
        );
        assert_column(
            data,
            SeriesName::CacheWriteTokens,
            None,
            &spans,
            M10,
            |span| span.cache_write_tokens,
        );
    }

    fn usage_rows(spans: &[SpanSample]) -> Vec<ModelUsageRow> {
        let mut rows: BTreeMap<String, ModelUsageRow> = BTreeMap::new();
        for span in spans.iter().filter(|span| span.model.is_some()) {
            let model = span.model.clone().expect("model");
            let row = rows.entry(model.clone()).or_insert(ModelUsageRow {
                model,
                llm_count: 0,
                total_tokens: None,
                cost_total: None,
            });
            row.llm_count += 1;
            if let Some(tokens) = span.total_tokens {
                row.total_tokens = Some(row.total_tokens.unwrap_or(0) + tokens);
            }
            if let Some(cost) = span.cost_total {
                row.cost_total = Some(row.cost_total.unwrap_or(0.0) + cost);
            }
        }
        let mut rows: Vec<_> = rows.into_values().collect();
        rows.sort_by(by_cost);
        rows.truncate(10);
        rows
    }

    fn by_cost(left: &ModelUsageRow, right: &ModelUsageRow) -> Ordering {
        match (left.cost_total, right.cost_total) {
            (Some(left_cost), Some(right_cost)) => right_cost
                .total_cmp(&left_cost)
                .then_with(|| left.model.cmp(&right.model)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => left.model.cmp(&right.model),
        }
    }

    fn hour() -> (i64, i64, MetricsRequest<'static>) {
        (
            at("2026-01-01T10:00:00Z"),
            at("2026-01-01T11:00:00Z"),
            request(
                "2026-01-01T10:00:00Z",
                "2026-01-01T11:00:00Z",
                Bucket::Fixed(BucketWidth::M10),
            ),
        )
    }

    #[tokio::test]
    async fn token_sums_match_llm_and_embedding_spans() {
        let (from, to, request) = hour();
        let data = run(request).await;
        let ids = scoped_ids("traces.json", from, to);
        assert_tokens(&data, &ids, from, to);
        let spans = kept_spans(&ids, from, to, &[SpanKind::Llm.as_str()]);
        let with_ttft: Vec<_> = spans
            .iter()
            .filter(|span| span.ttft_ns.is_some())
            .cloned()
            .collect();
        for point in &named(&data, SeriesName::TtftP50, None).points {
            let rows = bucket_spans(&with_ttft, point.time, M10);
            if rows.len() == 1 {
                assert_eq!(point.value, Some(rows[0].ttft_ns.expect("ttft") as f64));
                assert_eq!(
                    value_at(named(&data, SeriesName::TtftP95, None), point.time),
                    point.value
                );
            }
        }
    }

    #[tokio::test]
    async fn llm_latency_p50_matches_one_span_buckets() {
        let (from, to, request) = hour();
        let data = run(request).await;
        let spans = kept_spans(
            &scoped_ids("traces.json", from, to),
            from,
            to,
            &[SpanKind::Llm.as_str()],
        );
        let spans: Vec<_> = spans
            .into_iter()
            .filter(|span| span.model.is_some())
            .collect();
        let models: BTreeSet<_> = spans.iter().filter_map(|span| span.model.clone()).collect();
        let actual: BTreeSet<_> = data
            .series
            .iter()
            .filter(|series| series.name == SeriesName::LlmLatencyP50)
            .map(|series| series.group.clone().expect("model"))
            .collect();
        assert_eq!(actual, models);
        for model in &models {
            let series = named(&data, SeriesName::LlmLatencyP50, Some(model));
            for point in &series.points {
                let rows: Vec<_> = bucket_spans(&spans, point.time, M10)
                    .into_iter()
                    .filter(|span| span.model.as_deref() == Some(model))
                    .collect();
                if rows.len() == 1 {
                    let exact = Some(rows[0].duration as f64);
                    assert_eq!(point.value, exact);
                    assert_eq!(
                        value_at(
                            named(&data, SeriesName::LlmLatencyP95, Some(model)),
                            point.time
                        ),
                        exact
                    );
                }
            }
        }
    }

    #[tokio::test]
    async fn cost_by_model_and_top_models_match_the_dataset() {
        let (from, to, request) = hour();
        let data = run(request).await;
        let spans = kept_spans(
            &scoped_ids("traces.json", from, to),
            from,
            to,
            &[SpanKind::Llm.as_str()],
        );
        let spans: Vec<_> = spans
            .into_iter()
            .filter(|span| span.model.is_some())
            .collect();
        let models: BTreeSet<_> = spans.iter().filter_map(|span| span.model.clone()).collect();
        for model in &models {
            for point in &named(&data, SeriesName::CostByModel, Some(model)).points {
                let rows: Vec<_> = bucket_spans(&spans, point.time, M10)
                    .into_iter()
                    .filter(|span| span.model.as_deref() == Some(model))
                    .collect();
                assert_close(
                    point.value,
                    sum_f64(rows.into_iter().map(|span| span.cost_total)),
                );
            }
        }
        assert_eq!(data.top_models, usage_rows(&spans));
    }

    #[tokio::test]
    async fn top_models_keeps_ten_by_cost_then_name() {
        let template = dataset_spans().into_iter().next().expect("span");
        let start = at("2026-01-01T10:05:00Z");
        let mut built = Vec::new();
        for n in 0..12u8 {
            let mut span = template.clone();
            span.trace_id = numbered_trace(n + 1);
            span.span_id = numbered_span(n + 1);
            span.parent_span_id = None;
            span.span_kind = SpanKind::Llm;
            span.start_time = start;
            span.end_time = start + 1_000_000;
            span.environment = Some("production".to_owned());
            span.metadata.clear();
            span.response_model = Some(format!("m{n:02}"));
            span.request_model = None;
            span.cost_total = (n > 0).then_some(f64::from(n));
            span.total_tokens = Some(i64::from(n) * 10);
            span.ttft_ns = None;
            built.push(span);
        }
        let data = metrics(
            &context_of(&built),
            request(
                "2026-01-01T10:00:00Z",
                "2026-01-01T11:00:00Z",
                Bucket::Fixed(BucketWidth::M10),
            ),
        )
        .await
        .expect("metrics");
        let expected = usage_rows(
            &built
                .iter()
                .map(|span| SpanSample {
                    trace_id: span.trace_id.to_string(),
                    start: span.start_time,
                    duration: span.end_time - span.start_time,
                    kind: span.span_kind.as_str().to_owned(),
                    model: span.response_model.clone(),
                    input_tokens: span.input_tokens,
                    output_tokens: span.output_tokens,
                    cache_read_tokens: span.cache_read_tokens,
                    cache_write_tokens: span.cache_write_tokens,
                    total_tokens: span.total_tokens,
                    ttft_ns: span.ttft_ns,
                    cost_total: span.cost_total,
                })
                .collect::<Vec<_>>(),
        );
        assert_eq!(data.top_models.len(), 10);
        assert_eq!(data.top_models, expected);
        assert!(
            data.top_models
                .iter()
                .all(|row| row.model != "m00" && row.model != "m01")
        );
        assert_eq!(data.top_models[0].model, "m11");
        assert_eq!(data.top_models[0].cost_total, Some(11.0));
        assert_eq!(data.top_models[9].model, "m02");
    }

    fn numbered_trace(n: u8) -> TraceId {
        let mut bytes = [0; 16];
        bytes[15] = n;
        TraceId::from_bytes(bytes).expect("trace id")
    }

    fn numbered_span(n: u8) -> SpanId {
        let mut bytes = [0; 8];
        bytes[7] = n;
        SpanId::from_bytes(bytes).expect("span id")
    }

    fn context_of(spans: &[Span]) -> QueryContext {
        QueryContext::new(
            ProjectQuery {
                spans: spans_table(spans),
                traces: traces_table(spans),
                scores: live_scores_table(&dataset_scores()),
                runtime: Arc::new(RuntimeEnv::default()),
                target_partitions: NonZeroUsize::new(2).expect("two partitions"),
                optimizer_rules: Vec::new(),

                suspects: crate::verify::Suspects::detached(),
            },
            None,
        )
        .expect("context")
    }

    #[tokio::test]
    async fn score_average_uses_live_trace_and_span_scores() {
        let (from, to, request) = hour();
        let data = run(request).await;
        let ids = scoped_ids("traces.json", from, to);
        let mut sums: BTreeMap<(i64, String), (f64, i64)> = BTreeMap::new();
        let mut session_names = BTreeSet::new();
        for score in live_scores(&dataset_scores()) {
            let trace_id = match &score.target {
                ScoreTarget::Trace { trace_id } | ScoreTarget::Span { trace_id, .. } => {
                    trace_id.to_string()
                }
                ScoreTarget::Session { .. } => {
                    session_names.insert(score.name);
                    continue;
                }
            };
            if !ids.contains(&trace_id) || score.created_at < from || score.created_at >= to {
                continue;
            }
            let Some(value) = score.value.value_num() else {
                continue;
            };
            let bucket = score.created_at - score.created_at.rem_euclid(M10);
            let entry = sums.entry((bucket, score.name)).or_insert((0.0, 0));
            entry.0 += value;
            entry.1 += 1;
        }
        assert!(!session_names.is_empty());
        let actual: BTreeSet<_> = data
            .series
            .iter()
            .filter(|series| series.name == SeriesName::ScoreAvg)
            .map(|series| series.group.clone().expect("score name"))
            .collect();
        let names: BTreeSet<_> = sums.keys().map(|(_, name)| name.clone()).collect();
        assert_eq!(actual, names);
        assert!(actual.is_disjoint(&session_names));
        for series in data
            .series
            .iter()
            .filter(|series| series.name == SeriesName::ScoreAvg)
        {
            let name = series.group.clone().expect("score name");
            for point in &series.points {
                let expected = sums
                    .get(&(point.time, name.clone()))
                    .map(|(sum, count)| sum / *count as f64);
                assert_close(point.value, expected);
            }
        }
    }

    #[tokio::test]
    async fn kpis_cover_the_window_and_the_previous_window() {
        let (from, to, asked) = hour();
        let data = run(asked).await;
        assert_eq!(
            data.kpis.iter().map(|kpi| kpi.name).collect::<Vec<_>>(),
            vec![
                KpiName::Traces,
                KpiName::ErrorRate,
                KpiName::LatencyP95,
                KpiName::CostTotal,
                KpiName::TotalTokens,
            ]
        );
        let loaded = samples("traces.json");
        assert_kpis(&data, &loaded, from, to);
        assert!(data.kpis.iter().all(|kpi| kpi.previous.is_none()));

        let previous_from = at("2026-01-01T10:15:00Z");
        let previous_to = at("2026-01-01T10:30:00Z");
        let later = run(request(
            "2026-01-01T10:15:00Z",
            "2026-01-01T10:30:00Z",
            Bucket::Fixed(BucketWidth::M10),
        ))
        .await;
        let traces = kpi(&later, KpiName::Traces);
        let current = loaded
            .iter()
            .filter(|sample| sample.start >= previous_from && sample.start < previous_to)
            .count();
        let previous = loaded
            .iter()
            .filter(|sample| {
                sample.start >= at("2026-01-01T10:00:00Z") && sample.start < previous_from
            })
            .count();
        assert_eq!(traces.value, Some(current as f64));
        assert_eq!(traces.previous, Some(previous as f64));
        assert!(traces.previous.is_some_and(|value| value > 0.0));
        assert_kpis(&later, &loaded, previous_from, previous_to);
    }

    fn kpi(data: &MetricsData, name: KpiName) -> &Kpi {
        data.kpis
            .iter()
            .find(|kpi| kpi.name == name)
            .unwrap_or_else(|| panic!("missing {name:?}"))
    }

    fn assert_kpis(data: &MetricsData, loaded: &[Sample], from: i64, to: i64) {
        let rows: Vec<_> = loaded
            .iter()
            .filter(|sample| sample.start >= from && sample.start < to)
            .collect();
        assert_eq!(kpi(data, KpiName::Traces).value, Some(rows.len() as f64));
        let errors = rows.iter().filter(|row| row.error_count > 0).count();
        assert_close(
            kpi(data, KpiName::ErrorRate).value,
            Some(errors as f64 / rows.len() as f64),
        );
        let durations: Vec<f64> = rows.iter().map(|row| row.duration as f64).collect();
        let latency = kpi(data, KpiName::LatencyP95).value.expect("p95");
        let min = durations.iter().copied().fold(f64::INFINITY, f64::min);
        let max = durations.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!(
            latency >= min && latency <= max,
            "{latency} not in {min}..={max}"
        );
        if durations.len() == 1 {
            assert_eq!(kpi(data, KpiName::LatencyP95).value, Some(durations[0]));
        }
        assert_close(kpi(data, KpiName::CostTotal).value, cost_sum(&rows));
        assert_close(
            kpi(data, KpiName::TotalTokens).value,
            sum_i64(rows.iter().map(|row| row.total_tokens)),
        );
    }

    #[tokio::test]
    async fn filter_keeps_token_series_on_one_trace() {
        let from = at("2026-01-01T10:00:00Z");
        let to = at("2026-01-01T11:00:00Z");
        let trace = "a1000000000000000000000000000001";
        let text = format!("trace_id = '{trace}'");
        let filtered = MetricsRequest {
            window: TimeWindow::new(from, to).expect("window"),
            bucket: Bucket::Fixed(BucketWidth::M10),
            filter: Some(&text),
            group_by: None,
            include_eval: false,
            include_experiments: false,
            timeout: TIMEOUT,
        };
        let data = run(filtered).await;
        let mut ids = BTreeSet::new();
        ids.insert(trace.to_owned());
        assert_tokens(&data, &ids, from, to);
        let all = kept_spans(
            &scoped_ids("traces.json", from, to),
            from,
            to,
            &[SpanKind::Llm.as_str(), SpanKind::Embedding.as_str()],
        );
        let only = kept_spans(
            &ids,
            from,
            to,
            &[SpanKind::Llm.as_str(), SpanKind::Embedding.as_str()],
        );
        let all_input = sum_i64(all.iter().map(|span| span.input_tokens));
        let one_input = sum_i64(only.iter().map(|span| span.input_tokens));
        assert!(all_input.is_some_and(|value| Some(value) != one_input));
    }

    #[tokio::test]
    async fn include_eval_adds_trace_five_spans_and_hides_them_by_default() {
        let (from, to, hidden_request) = hour();
        let hidden = run(hidden_request).await;
        let mut included_request = request(
            "2026-01-01T10:00:00Z",
            "2026-01-01T11:00:00Z",
            Bucket::Fixed(BucketWidth::M10),
        );
        included_request.include_eval = true;
        let included = run(included_request).await;
        let hidden_ids = scoped_ids("traces.json", from, to);
        let included_ids = scoped_ids("traces-include-eval.json", from, to);
        assert_tokens(&hidden, &hidden_ids, from, to);
        assert_tokens(&included, &included_ids, from, to);
        assert_span_models(&hidden, &hidden_ids, from, to);
        assert_span_models(&included, &included_ids, from, to);
        let extra: BTreeSet<_> = included_ids.difference(&hidden_ids).cloned().collect();
        let added = kept_spans(
            &extra,
            from,
            to,
            &[SpanKind::Llm.as_str(), SpanKind::Embedding.as_str()],
        );
        assert_eq!(
            sum_i64(added.iter().map(|span| span.input_tokens)),
            Some(600.0)
        );
        let hidden_input = series_sum(&hidden, SeriesName::InputTokens, None);
        let included_input = series_sum(&included, SeriesName::InputTokens, None);
        assert_close(
            included_input,
            hidden_input.map(|value| value + 600.0).or(Some(600.0)),
        );
    }

    fn assert_span_models(data: &MetricsData, ids: &BTreeSet<String>, from: i64, to: i64) {
        let spans = kept_spans(ids, from, to, &[SpanKind::Llm.as_str()]);
        let spans: Vec<_> = spans
            .into_iter()
            .filter(|span| span.model.is_some())
            .collect();
        let with_ttft: Vec<_> = spans
            .iter()
            .filter(|span| span.ttft_ns.is_some())
            .cloned()
            .collect();
        for point in &named(data, SeriesName::TtftP50, None).points {
            let rows = bucket_spans(&with_ttft, point.time, M10);
            if rows.len() == 1 {
                assert_eq!(point.value, Some(rows[0].ttft_ns.expect("ttft") as f64));
            }
        }
        for model in spans.iter().filter_map(|span| span.model.clone()) {
            for point in &named(data, SeriesName::CostByModel, Some(&model)).points {
                let rows: Vec<_> = bucket_spans(&spans, point.time, M10)
                    .into_iter()
                    .filter(|span| span.model.as_deref() == Some(model.as_str()))
                    .collect();
                assert_close(
                    point.value,
                    sum_f64(rows.into_iter().map(|span| span.cost_total)),
                );
            }
            for point in &named(data, SeriesName::LlmLatencyP50, Some(&model)).points {
                let rows: Vec<_> = bucket_spans(&spans, point.time, M10)
                    .into_iter()
                    .filter(|span| span.model.as_deref() == Some(model.as_str()))
                    .collect();
                if rows.len() == 1 {
                    assert_eq!(point.value, Some(rows[0].duration as f64));
                }
            }
        }
    }

    fn series_sum(data: &MetricsData, name: SeriesName, group: Option<&str>) -> Option<f64> {
        sum_f64(
            named(data, name, group)
                .points
                .iter()
                .map(|point| point.value),
        )
    }
}
