//! Dashboard metrics computed on `traces`.
//!
//! Trace series share one aggregate, grouped by `date_bin` and, when asked,
//! the group key. `EvalCost` is a second aggregate: the eval environment only,
//! never grouped. Empty buckets stay `None`.

use std::collections::BTreeMap;
use std::time::Duration;

use arrow::array::{Array, AsArray};
use arrow::datatypes::{Float64Type, Int64Type, TimestampNanosecondType, UInt64Type};
use arrow::record_batch::RecordBatch;
use datafusion::common::{NullHandling, ScalarValue, UnnestOptions};
use datafusion::error::DataFusionError;
use datafusion::functions::datetime::expr_fn::date_bin;
use datafusion::functions_aggregate::expr_fn::{approx_percentile_cont, count, sum};
use datafusion::logical_expr::expr_fn::when;
use datafusion::logical_expr::{Expr, SortExpr, col, lit};
use datafusion::prelude::DataFrame;
use llmobs_core::names::environment;
use llmobs_core::schema::traces;

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
    /// Split the trace series. `EvalCost` stays ungrouped.
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
/// The variants computed here are the trace series. Later phases add the rest
/// of the server's `MetricName`, in this same order, before [`SeriesName::EvalCost`].
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
    /// `sum(cost_total)` on evaluation traces, USD. Never grouped.
    EvalCost,
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

/// Dashboard series for one request.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricsData {
    /// Width of every bucket, nanoseconds.
    pub bucket_ns: i64,
    /// Sorted by [`SeriesName`], then by group (`None` first).
    pub series: Vec<Series>,
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

/// Time series for the trace dashboard.
///
/// # Errors
///
/// [`RunError::InvalidRequest`] when a fixed bucket would emit more than
/// 2,000 points. [`RunError::Invalid`] for filter text. [`RunError::Timeout`]
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

async fn compute(ctx: &QueryContext, request: MetricsRequest<'_>) -> Result<MetricsData, RunError> {
    let width = resolve(request.window, request.bucket)?;
    let grid = bucket_grid(request.window, width)?;
    let rows = trace_rows(ctx, &request, width, &grid).await?;
    let eval_cost = eval_cost(ctx, &request, width, &grid).await?;
    Ok(assemble(width, &grid, request.group_by, &rows, &eval_cost))
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
            Ok(BucketWidth::D1.ns())
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
        .map_err(classify)?;
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
        .select(vec![bucket_expr(width), col(traces::COST_TOTAL)])
        .map_err(classify)?
        .aggregate(
            vec![col(BUCKET)],
            vec![sum(col(traces::COST_TOTAL)).alias(COST)],
        )
        .map_err(classify)?
        .collect()
        .await
        .map_err(classify)?;
    cost_rows(&batches, grid)
}

fn parsed(ctx: &QueryContext, text: Option<&str>) -> Result<Option<Filter>, RunError> {
    match present(text) {
        Some(text) => Ok(Some(ctx.parse_filter(FilterView::Traces, text)?)),
        None => Ok(None),
    }
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
        bucket_expr(width),
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
                percentile(0.5).alias(P50),
                percentile(0.95).alias(P95),
                percentile(0.99).alias(P99),
                sum(col(traces::COST_TOTAL)).alias(COST),
            ],
        )
        .map_err(classify)
}

fn percentile(quantile: f64) -> Expr {
    approx_percentile_cont(
        SortExpr::new(col(DURATION_NS), true, true),
        lit(quantile),
        None,
    )
}

fn bucket_expr(width: i64) -> Expr {
    date_bin(
        Expr::Literal(ScalarValue::new_interval_mdn(0, 0, width), None),
        col(traces::START_TIME),
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

fn assemble(
    width: i64,
    grid: &[i64],
    group_by: Option<MetricsGroupBy>,
    rows: &BTreeMap<RowKey, TraceRow>,
    eval_cost: &BTreeMap<i64, Option<f64>>,
) -> MetricsData {
    let groups = group_keys(group_by, rows);
    let mut series = Vec::new();
    push(
        &mut series,
        SeriesName::Traces,
        &groups,
        grid,
        rows,
        traces_of,
    );
    push(
        &mut series,
        SeriesName::Errors,
        &groups,
        grid,
        rows,
        errors_of,
    );
    push(
        &mut series,
        SeriesName::ErrorRate,
        &groups,
        grid,
        rows,
        rate_of,
    );
    push(
        &mut series,
        SeriesName::LatencyP50,
        &groups,
        grid,
        rows,
        p50_of,
    );
    push(
        &mut series,
        SeriesName::LatencyP95,
        &groups,
        grid,
        rows,
        p95_of,
    );
    push(
        &mut series,
        SeriesName::LatencyP99,
        &groups,
        grid,
        rows,
        p99_of,
    );
    push(&mut series, SeriesName::Cost, &groups, grid, rows, cost_of);
    series.push(Series {
        name: SeriesName::EvalCost,
        group: None,
        points: grid
            .iter()
            .map(|time| Point {
                time: *time,
                value: eval_cost.get(time).copied().flatten(),
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
        series,
    }
}

fn group_keys(
    group_by: Option<MetricsGroupBy>,
    rows: &BTreeMap<RowKey, TraceRow>,
) -> Vec<Option<String>> {
    if group_by.is_none() {
        return vec![None];
    }
    let mut groups: Vec<String> = rows.keys().filter_map(|key| key.group.clone()).collect();
    groups.sort();
    groups.dedup();
    groups.into_iter().map(Some).collect()
}

fn push(
    series: &mut Vec<Series>,
    name: SeriesName,
    groups: &[Option<String>],
    grid: &[i64],
    rows: &BTreeMap<RowKey, TraceRow>,
    pick: fn(Option<&TraceRow>) -> Option<f64>,
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
    use std::collections::BTreeSet;
    use std::time::Duration;

    use llmobs_core::names::environment;
    use llmobs_core::time_serde::parse_rfc3339;
    use serde_json::Value;

    use super::{
        Bucket, BucketWidth, MetricsData, MetricsGroupBy, MetricsRequest, Series, SeriesName,
        metrics,
    };
    use crate::query_error::RunError;
    use crate::test_support::example_query;
    use crate::views::{QueryContext, TimeWindow};

    const TIMEOUT: Duration = Duration::from_secs(30);
    const M10: i64 = 600_000_000_000;
    const MINUTE: i64 = 60_000_000_000;
    const DAY: i64 = 86_400_000_000_000;

    struct Sample {
        start: i64,
        duration: i64,
        error_count: i64,
        cost: Option<f64>,
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
        let names: Vec<_> = data.series.iter().map(|series| series.name).collect();
        assert_eq!(
            names,
            vec![
                SeriesName::Traces,
                SeriesName::Errors,
                SeriesName::ErrorRate,
                SeriesName::LatencyP50,
                SeriesName::LatencyP95,
                SeriesName::LatencyP99,
                SeriesName::Cost,
                SeriesName::EvalCost,
            ]
        );
        for series in &data.series {
            assert!(series.group.is_none());
            let actual: Vec<i64> = series.points.iter().map(|point| point.time).collect();
            assert_eq!(actual, times);
        }
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
            if series.name == SeriesName::EvalCost {
                continue;
            }
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
                    SeriesName::LatencyP50
                    | SeriesName::LatencyP95
                    | SeriesName::LatencyP99
                    | SeriesName::EvalCost => {}
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
}
