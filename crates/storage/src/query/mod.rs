//! Read path over one project's files and memtable.
//!
//! [`crate::ProjectQuery`] carries the raw providers, the shared runtime and
//! `target_partitions`. It does not build a DataFusion session: the query
//! layer does that and registers the user-facing views (spec 03 §6, D89, D91).
//!
//! A scan reads the files and the memtable of one read snapshot. Days
//! are exact. M3 widens span days by one (spec 03 § Trace anchor) and adds the
//! `traces` provider; until then `Storage::query` hands out an empty one.

mod scores;
mod spans;

use std::ops::RangeInclusive;
use std::sync::Arc;

use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use chrono::{Datelike, NaiveDate};
use datafusion::catalog::Session;
use datafusion::common::{DFSchema, ScalarValue};
use datafusion::datasource::physical_plan::ParquetSource;
use datafusion::execution::object_store::ObjectStoreUrl;
use datafusion::logical_expr::{Between, BinaryExpr, Expr, Operator};
use datafusion::physical_expr::utils::conjunction;
use datafusion::physical_plan::ExecutionPlan;
use datafusion::physical_plan::limit::GlobalLimitExec;
use datafusion::physical_plan::union::UnionExec;
use datafusion_datasource::PartitionedFile;
use datafusion_datasource::file_groups::FileGroup;
use datafusion_datasource::file_scan_config::FileScanConfigBuilder;
use datafusion_datasource::memory::MemorySourceConfig;
use datafusion_datasource::source::DataSourceExec;
use llmobs_core::ids::ProjectId;

use crate::catalog::{Catalog, FileMeta, FileTable};
use crate::memtable::{MemBatch, MemSnapshot};
use crate::objects::OBJECT_STORE_URL;
use crate::wal::Table;

pub(crate) use scores::ScoresProvider;
pub(crate) use spans::SpansProvider;

/// Nanoseconds in a UTC day.
const DAY_NS: i64 = 86_400_000_000_000;

/// `NaiveDate::num_days_from_ce` of 1970-01-01.
const UNIX_EPOCH_DAYS_FROM_CE: i64 = 719_163;

/// Inclusive nanosecond bounds extracted from filters.
///
/// A contradictory range (`min > max`) selects nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimeBounds {
    /// Smallest included timestamp, if the filters set one.
    pub min: Option<i64>,
    /// Largest included timestamp, if the filters set one.
    pub max: Option<i64>,
}

impl TimeBounds {
    fn open() -> Self {
        Self {
            min: None,
            max: None,
        }
    }

    /// True when no timestamp can satisfy both ends.
    pub(crate) fn contradicts(&self) -> bool {
        matches!((self.min, self.max), (Some(lo), Some(hi)) if lo > hi)
    }

    fn raise_min(&mut self, value: i64) {
        self.min = Some(self.min.map_or(value, |current| current.max(value)));
    }

    fn lower_max(&mut self, value: i64) {
        self.max = Some(self.max.map_or(value, |current| current.min(value)));
    }

    fn force_empty(&mut self) {
        self.min = Some(0);
        self.max = Some(-1);
    }
}

/// Inclusive bounds of `column` from the top-level conjuncts of `filters`.
///
/// `OR`, `NOT`, casts and functions contribute nothing, so the scan stays a
/// superset of the rows DataFusion keeps. A strict comparison moves by 1 ns.
/// A range that cannot match is [`TimeBounds::contradicts`], not an error.
pub(crate) fn time_bounds(filters: &[Expr], column: &str) -> TimeBounds {
    let mut bounds = TimeBounds::open();
    for filter in filters {
        walk(&mut bounds, filter, column);
    }
    bounds
}

fn walk(bounds: &mut TimeBounds, expr: &Expr, column: &str) {
    match expr {
        Expr::BinaryExpr(binary) if binary.op == Operator::And => {
            walk(bounds, &binary.left, column);
            walk(bounds, &binary.right, column);
        }
        Expr::BinaryExpr(binary) => constrain_binary(bounds, binary, column),
        Expr::Between(between) if !between.negated => constrain_between(bounds, between, column),
        _ => {}
    }
}

fn constrain_binary(bounds: &mut TimeBounds, binary: &BinaryExpr, column: &str) {
    match (binary.left.as_ref(), binary.right.as_ref()) {
        (Expr::Column(col), other) if col.name == column => {
            if let Some(instant) = literal_ns(other) {
                apply_comparison(bounds, binary.op, instant, true);
            }
        }
        (other, Expr::Column(col)) if col.name == column => {
            if let Some(instant) = literal_ns(other) {
                apply_comparison(bounds, binary.op, instant, false);
            }
        }
        _ => {}
    }
}

fn constrain_between(bounds: &mut TimeBounds, between: &Between, column: &str) {
    let Expr::Column(col) = between.expr.as_ref() else {
        return;
    };
    if col.name != column {
        return;
    }
    if let Some(low) = literal_ns(&between.low) {
        bounds.raise_min(low);
    }
    if let Some(high) = literal_ns(&between.high) {
        bounds.lower_max(high);
    }
}

/// `column_on_left` is false when the literal is the left operand.
fn apply_comparison(bounds: &mut TimeBounds, op: Operator, literal: i64, column_on_left: bool) {
    let op = if column_on_left {
        op
    } else {
        match op {
            Operator::Eq => Operator::Eq,
            Operator::Gt => Operator::Lt,
            Operator::GtEq => Operator::LtEq,
            Operator::Lt => Operator::Gt,
            Operator::LtEq => Operator::GtEq,
            _ => return,
        }
    };
    match op {
        Operator::Eq => {
            bounds.raise_min(literal);
            bounds.lower_max(literal);
        }
        Operator::Gt => match literal.checked_add(1) {
            Some(next) => bounds.raise_min(next),
            None => bounds.force_empty(),
        },
        Operator::GtEq => bounds.raise_min(literal),
        Operator::Lt => match literal.checked_sub(1) {
            Some(previous) => bounds.lower_max(previous),
            None => bounds.force_empty(),
        },
        Operator::LtEq => bounds.lower_max(literal),
        _ => {}
    }
}

/// Timestamp literals of any unit, or an RFC 3339 string DataFusion left uncast.
///
/// Overflow while scaling to nanoseconds contributes no bound. The filter
/// still runs above the scan.
fn literal_ns(expr: &Expr) -> Option<i64> {
    let Expr::Literal(value, _) = expr else {
        return None;
    };
    match value {
        ScalarValue::TimestampNanosecond(Some(value), _) => Some(*value),
        ScalarValue::TimestampMicrosecond(Some(value), _) => value.checked_mul(1_000),
        ScalarValue::TimestampMillisecond(Some(value), _) => value.checked_mul(1_000_000),
        ScalarValue::TimestampSecond(Some(value), _) => value.checked_mul(1_000_000_000),
        ScalarValue::Utf8(Some(text))
        | ScalarValue::LargeUtf8(Some(text))
        | ScalarValue::Utf8View(Some(text)) => llmobs_core::time_serde::parse_rfc3339(text).ok(),
        _ => None,
    }
}

/// Batches of `project` whose day and time range overlap `column`'s bounds.
pub(crate) fn rows_for(
    project: ProjectId,
    snapshot: &MemSnapshot,
    time_column: &str,
    filters: &[Expr],
) -> Vec<RecordBatch> {
    let bounds = time_bounds(filters, time_column);
    if bounds.contradicts() {
        return Vec::new();
    }
    let mut selected = Vec::new();
    for (key, batches) in &snapshot.partitions {
        if key.project != project || !day_overlaps(key.day, &bounds) {
            continue;
        }
        for batch in batches {
            if range_overlaps(batch, &bounds) {
                selected.push(RecordBatch::clone(&batch.batch));
            }
        }
    }
    selected
}

fn day_start_ns(day: NaiveDate) -> Option<i64> {
    let days = i64::from(day.num_days_from_ce()) - UNIX_EPOCH_DAYS_FROM_CE;
    days.checked_mul(DAY_NS)
}

/// A day that does not fit in `i64` nanoseconds is kept. Dropping it could hide rows.
fn day_overlaps(day: NaiveDate, bounds: &TimeBounds) -> bool {
    if bounds.contradicts() {
        return false;
    }
    let Some(start) = day_start_ns(day) else {
        return true;
    };
    let Some(end) = day.succ_opt().and_then(day_start_ns) else {
        return true;
    };
    if let Some(min) = bounds.min
        && end <= min
    {
        return false;
    }
    if let Some(max) = bounds.max
        && start > max
    {
        return false;
    }
    true
}

fn range_overlaps(batch: &MemBatch, bounds: &TimeBounds) -> bool {
    if let Some(min) = bounds.min
        && batch.max_time < min
    {
        return false;
    }
    if let Some(max) = bounds.max
        && batch.min_time > max
    {
        return false;
    }
    true
}

/// In-memory scan. The limit is applied only when `filters` is empty, because
/// an inexact filter still runs above this plan.
pub(crate) fn memory_plan(
    batches: Vec<RecordBatch>,
    schema: SchemaRef,
    projection: Option<&Vec<usize>>,
    filters: &[Expr],
    limit: Option<usize>,
    target_partitions: usize,
) -> datafusion::error::Result<Arc<dyn ExecutionPlan>> {
    let partitions = distribute(batches, target_partitions);
    let mut source = MemorySourceConfig::try_new(&partitions, schema, projection.cloned())?;
    if filters.is_empty() {
        source = source.with_limit(limit);
    }
    Ok(DataSourceExec::from_data_source(source))
}

/// One file-and-memtable scan. The providers fill this in.
pub(crate) struct ScanUnion<'a> {
    /// Project the files and batches belong to.
    pub project: ProjectId,
    /// Memtable half of the snapshot.
    pub snapshot: &'a MemSnapshot,
    /// File half of the same snapshot.
    pub catalog: &'a Catalog,
    /// `spans` or `scores`.
    pub table: Table,
    /// Event-time column used for day and file pruning.
    pub time_column: &'static str,
    /// Stored schema, before projection.
    pub schema: SchemaRef,
    /// Column indices the caller asked for.
    pub projection: Option<&'a Vec<usize>>,
    /// Filters. All of them are inexact; the file predicate still sees them.
    pub filters: &'a [Expr],
    /// Applied only when `filters` is empty, on the combined plan.
    pub limit: Option<usize>,
    /// Upper bound on file groups and memtable partitions.
    pub target_partitions: usize,
    /// Session that owns the registered object store and plans expressions.
    pub session: &'a dyn Session,
}

/// Files whose day and `[min_time, max_time]` overlap the time filters.
pub(crate) fn overlapping_files(
    catalog: &Catalog,
    table: Table,
    project: ProjectId,
    time_column: &str,
    filters: &[Expr],
) -> Vec<Arc<FileMeta>> {
    let bounds = time_bounds(filters, time_column);
    if bounds.contradicts() {
        return Vec::new();
    }
    let file_table = FileTable::from(table);
    catalog
        .files(file_table, project, file_day_range(&bounds, file_table))
        .filter(|file| file_overlaps(file, &bounds))
        .cloned()
        .collect()
}

/// Union of the Parquet files and the memtable. An empty side is left out.
pub(crate) fn scan_union(
    scan: &ScanUnion<'_>,
) -> datafusion::error::Result<Arc<dyn ExecutionPlan>> {
    let batches = rows_for(scan.project, scan.snapshot, scan.time_column, scan.filters);
    let files = overlapping_files(
        scan.catalog,
        scan.table,
        scan.project,
        scan.time_column,
        scan.filters,
    );
    let both = !batches.is_empty() && !files.is_empty();
    // A limit on each side would drop rows the other side still has.
    let side_limit = if both { None } else { scan.limit };
    let mut plans = Vec::new();
    if !files.is_empty() {
        plans.push(file_plan(scan, &files, side_limit)?);
    }
    if !batches.is_empty() {
        plans.push(memory_plan(
            batches,
            Arc::clone(&scan.schema),
            scan.projection,
            scan.filters,
            side_limit,
            scan.target_partitions,
        )?);
    }
    let plan = if plans.is_empty() {
        memory_plan(
            Vec::new(),
            Arc::clone(&scan.schema),
            scan.projection,
            scan.filters,
            side_limit,
            scan.target_partitions,
        )?
    } else {
        UnionExec::try_new(plans)?
    };
    if both
        && scan.filters.is_empty()
        && let Some(fetch) = scan.limit
    {
        return Ok(Arc::new(GlobalLimitExec::new(plan, 0, Some(fetch))));
    }
    Ok(plan)
}

fn file_plan(
    scan: &ScanUnion<'_>,
    files: &[Arc<FileMeta>],
    limit: Option<usize>,
) -> datafusion::error::Result<Arc<dyn ExecutionPlan>> {
    let df_schema = DFSchema::try_from(Arc::clone(&scan.schema))?;
    let mut physical = Vec::with_capacity(scan.filters.len());
    for filter in scan.filters {
        physical.push(
            scan.session
                .create_physical_expr(filter.clone(), &df_schema)?,
        );
    }
    let mut source = ParquetSource::new(Arc::clone(&scan.schema))
        .with_pushdown_filters(true)
        .with_enable_page_index(true);
    if !physical.is_empty() {
        source = source.with_predicate(conjunction(physical));
    }
    let url = ObjectStoreUrl::parse(OBJECT_STORE_URL)
        .map_err(|error| datafusion::error::DataFusionError::Internal(error.to_string()))?;
    let mut builder = FileScanConfigBuilder::new(url, Arc::new(source))
        .with_file_groups(file_groups(files, scan.target_partitions));
    if scan.filters.is_empty() {
        builder = builder.with_limit(limit);
    }
    let builder = builder.with_projection_indices(scan.projection.cloned())?;
    Ok(DataSourceExec::from_data_source(builder.build()))
}

/// Whole files, packed into at most `target` groups by object size.
fn file_groups(files: &[Arc<FileMeta>], target: usize) -> Vec<FileGroup> {
    let mut partitioned: Vec<PartitionedFile> = files
        .iter()
        .map(|meta| {
            let mut file = PartitionedFile::new(meta.path.clone(), meta.size);
            if let Ok(hint) = usize::try_from(meta.footer_size) {
                file = file.with_metadata_size_hint(hint);
            }
            file
        })
        .collect();
    let slots = target.max(1).min(partitioned.len().max(1));
    partitioned.sort_by(|left, right| {
        right
            .object_meta
            .size
            .cmp(&left.object_meta.size)
            .then_with(|| left.object_meta.location.cmp(&right.object_meta.location))
    });
    let mut groups: Vec<(u64, Vec<PartitionedFile>)> = Vec::with_capacity(slots);
    for _ in 0..slots {
        groups.push((0, Vec::new()));
    }
    for file in partitioned {
        let mut chosen = 0;
        for (index, (size, _)) in groups.iter().enumerate().skip(1) {
            if *size < groups[chosen].0 {
                chosen = index;
            }
        }
        groups[chosen].0 = groups[chosen].0.saturating_add(file.object_meta.size);
        groups[chosen].1.push(file);
    }
    groups
        .into_iter()
        .filter(|(_, files)| !files.is_empty())
        .map(|(_, files)| FileGroup::new(files))
        .collect()
}

/// Days that can hold a timestamp inside `bounds`. A bound that does not
/// convert stays open, so a day that does not fit in `i64` nanoseconds is kept.
fn day_range(bounds: &TimeBounds) -> RangeInclusive<NaiveDate> {
    let start = bounds.min.and_then(day_of_ns).unwrap_or(NaiveDate::MIN);
    let end = bounds.max.and_then(day_of_ns).unwrap_or(NaiveDate::MAX);
    if start > end {
        return NaiveDate::MIN..=NaiveDate::MAX;
    }
    start..=end
}

/// File day pruning for `spans` and `traces` widens by one day on each side:
/// an anchor partition may hold rows whose own day is one day away (D86).
/// `scores` files stay in their own day; memtable pruning stays exact.
fn file_day_range(bounds: &TimeBounds, table: FileTable) -> RangeInclusive<NaiveDate> {
    let range = day_range(bounds);
    match table {
        FileTable::Spans | FileTable::Traces => {
            let start = range.start().pred_opt().unwrap_or(NaiveDate::MIN);
            let end = range.end().succ_opt().unwrap_or(NaiveDate::MAX);
            start..=end
        }
        FileTable::Scores => range,
    }
}

fn day_of_ns(ns: i64) -> Option<NaiveDate> {
    let days = ns.div_euclid(DAY_NS);
    let ce = days.checked_add(UNIX_EPOCH_DAYS_FROM_CE)?;
    NaiveDate::from_num_days_from_ce_opt(i32::try_from(ce).ok()?)
}

fn file_overlaps(file: &FileMeta, bounds: &TimeBounds) -> bool {
    if let Some(min) = bounds.min
        && file.max_time < min
    {
        return false;
    }
    if let Some(max) = bounds.max
        && file.min_time > max
    {
        return false;
    }
    true
}

/// At most `target` partitions, filled so row counts stay close.
fn distribute(batches: Vec<RecordBatch>, target: usize) -> Vec<Vec<RecordBatch>> {
    let target = target.max(1);
    if batches.is_empty() {
        return vec![Vec::new()];
    }
    let slots = target.min(batches.len());
    let mut partitions = vec![Vec::new(); slots];
    let mut rows = vec![0usize; slots];
    for batch in batches {
        let mut slot = 0;
        for index in 1..slots {
            if rows[index] < rows[slot] {
                slot = index;
            }
        }
        rows[slot] = rows[slot].saturating_add(batch.num_rows());
        partitions[slot].push(batch);
    }
    partitions
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::{Array, TimestampNanosecondArray};
    use datafusion::common::ScalarValue;
    use datafusion::logical_expr::{Cast, Expr, Operator, col, lit};
    use llmobs_core::ids::ProjectId;
    use proptest::prelude::*;

    use super::*;
    use crate::memtable::Memtable;
    use crate::wal::{Table, spans_at};

    fn column_cmp(op: Operator, literal: Expr) -> Expr {
        Expr::BinaryExpr(BinaryExpr::new(
            Box::new(col("start_time")),
            op,
            Box::new(literal),
        ))
    }

    fn nano(value: i64, tz: Option<&str>) -> Expr {
        Expr::Literal(
            ScalarValue::TimestampNanosecond(Some(value), tz.map(Arc::from)),
            None,
        )
    }

    fn bounds_of(expr: Expr) -> TimeBounds {
        time_bounds(&[expr], "start_time")
    }

    #[test]
    fn comparisons_are_inclusive_and_strict_ones_move_by_one_nanosecond() {
        let cases = [
            (Operator::Eq, Some(10), Some(10)),
            (Operator::Gt, Some(11), None),
            (Operator::GtEq, Some(10), None),
            (Operator::Lt, None, Some(9)),
            (Operator::LtEq, None, Some(10)),
        ];
        for (op, min, max) in cases {
            let bounds = bounds_of(column_cmp(op, nano(10, Some("UTC"))));
            assert_eq!(bounds, TimeBounds { min, max }, "{op:?}");
        }
    }

    #[test]
    fn a_literal_on_the_left_flips_the_comparison() {
        let expr = Expr::BinaryExpr(BinaryExpr::new(
            Box::new(nano(10, None)),
            Operator::Gt,
            Box::new(col("start_time")),
        ));
        assert_eq!(
            bounds_of(expr),
            TimeBounds {
                min: None,
                max: Some(9)
            }
        );
    }

    #[test]
    fn between_and_conjunctions_intersect() {
        let between = Expr::Between(Between::new(
            Box::new(col("start_time")),
            false,
            Box::new(nano(3, None)),
            Box::new(nano(8, None)),
        ));
        assert_eq!(
            bounds_of(between),
            TimeBounds {
                min: Some(3),
                max: Some(8)
            }
        );
        let both = column_cmp(Operator::GtEq, nano(5, None))
            .and(column_cmp(Operator::LtEq, nano(7, None)));
        assert_eq!(
            bounds_of(both),
            TimeBounds {
                min: Some(5),
                max: Some(7)
            }
        );
    }

    #[test]
    fn contradictory_bounds_are_empty_not_an_error() {
        let expr = column_cmp(Operator::GtEq, nano(8, None))
            .and(column_cmp(Operator::LtEq, nano(5, None)));
        let bounds = bounds_of(expr);
        assert!(bounds.contradicts());
        let past_max = bounds_of(column_cmp(Operator::Gt, nano(i64::MAX, None)));
        assert!(past_max.contradicts());
    }

    #[test]
    fn or_not_and_casts_contribute_no_bound() {
        let comparison = column_cmp(Operator::GtEq, nano(5, None));
        let or = comparison.clone().or(lit(false));
        assert_eq!(bounds_of(or), TimeBounds::open());
        let not = Expr::Not(Box::new(comparison.clone()));
        assert_eq!(bounds_of(not), TimeBounds::open());
        let cast = Expr::Cast(Cast::new(
            Box::new(comparison),
            arrow::datatypes::DataType::Boolean,
        ));
        assert_eq!(bounds_of(cast), TimeBounds::open());
        let negated = Expr::Between(Between::new(
            Box::new(col("start_time")),
            true,
            Box::new(nano(1, None)),
            Box::new(nano(2, None)),
        ));
        assert_eq!(bounds_of(negated), TimeBounds::open());
    }

    #[test]
    fn every_literal_form_converts_to_nanoseconds() {
        let cases = [
            (nano(42, Some("America/New_York")), 42),
            (
                Expr::Literal(ScalarValue::TimestampMicrosecond(Some(2), None), None),
                2_000,
            ),
            (
                Expr::Literal(ScalarValue::TimestampMillisecond(Some(2), None), None),
                2_000_000,
            ),
            (
                Expr::Literal(ScalarValue::TimestampSecond(Some(2), None), None),
                2_000_000_000,
            ),
            (
                Expr::Literal(
                    ScalarValue::Utf8(Some("1970-01-01T00:00:01Z".to_owned())),
                    None,
                ),
                1_000_000_000,
            ),
        ];
        for (literal, expected) in cases {
            assert_eq!(
                bounds_of(column_cmp(Operator::Eq, literal)),
                TimeBounds {
                    min: Some(expected),
                    max: Some(expected)
                }
            );
        }
        let overflow = Expr::Literal(ScalarValue::TimestampSecond(Some(i64::MAX), None), None);
        assert_eq!(
            bounds_of(column_cmp(Operator::Eq, overflow)),
            TimeBounds::open()
        );
    }

    fn start_times(batches: &[RecordBatch]) -> Vec<i64> {
        let mut times = Vec::new();
        for batch in batches {
            let column = batch
                .column_by_name("start_time")
                .unwrap()
                .as_any()
                .downcast_ref::<TimestampNanosecondArray>()
                .unwrap();
            for index in 0..column.len() {
                times.push(column.value(index));
            }
        }
        times.sort_unstable();
        times
    }

    proptest! {
        #[test]
        fn pruning_keeps_every_row_an_unpruned_scan_would_keep(
            days in proptest::collection::vec(0i64..5, 1..8),
            lo in 0i64..6,
            hi in 0i64..6,
        ) {
            let project = "018f6b4e-7c1a-7c1a-8c1a-000000000001"
                .parse::<ProjectId>()
                .unwrap();
            let mut table = Memtable::new(Table::Spans);
            for (index, day) in days.iter().copied().enumerate() {
                let start = day * DAY_NS + i64::try_from(index).unwrap() + 1;
                table
                    .insert(
                        project,
                        spans_at(&[start]),
                        u64::try_from(index).unwrap_or(u64::MAX),
                    )
                    .unwrap();
            }
            let snapshot = table.snapshot();
            let all = rows_for(project, &snapshot, "not_a_time_column", &[]);
            let filter = column_cmp(Operator::GtEq, nano(lo * DAY_NS, None))
                .and(column_cmp(Operator::Lt, nano((hi + 1) * DAY_NS, None)));
            let selected = rows_for(project, &snapshot, "start_time", &[filter]);
            let in_range = |times: Vec<i64>| {
                times
                    .into_iter()
                    .filter(|time| *time >= lo * DAY_NS && *time < (hi + 1) * DAY_NS)
                    .collect::<Vec<_>>()
            };
            prop_assert_eq!(in_range(start_times(&selected)), in_range(start_times(&all)));
        }
    }
}
