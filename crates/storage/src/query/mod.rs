//! Read path over one project's files and memtable.
//!
//! [`crate::ProjectQuery`] carries the raw providers, the shared runtime and
//! `target_partitions`. It does not build a DataFusion session: the query
//! layer does that and registers the user-facing views (spec 03 §6, D89, D91).
//!
//! A scan reads the files and the memtable of one read snapshot. Span and
//! trace file days are widened by one for the trace anchor (spec 03 § Trace
//! anchor); memtable pruning stays exact.

mod scores;
mod spans;
mod trace_list;
mod traces;

use std::collections::BTreeSet;
use std::ops::RangeInclusive;
use std::sync::Arc;

use arrow::array::RecordBatch;
use arrow::datatypes::{DataType, SchemaRef};
use chrono::{Datelike, NaiveDate};
use datafusion::catalog::Session;
use datafusion::common::{DFSchema, ScalarValue};
use datafusion::datasource::physical_plan::ParquetSource;
use datafusion::error::DataFusionError;
use datafusion::execution::object_store::ObjectStoreUrl;
use datafusion::logical_expr::{Between, BinaryExpr, Expr, Operator};
use datafusion::physical_expr::PhysicalExpr;
use datafusion::physical_expr::expressions::{Column, NotExpr};
use datafusion::physical_expr::utils::conjunction;
use datafusion::physical_plan::ExecutionPlan;
use datafusion::physical_plan::filter::FilterExec;
use datafusion::physical_plan::limit::GlobalLimitExec;
use datafusion::physical_plan::projection::ProjectionExec;
use datafusion::physical_plan::union::UnionExec;
use datafusion_datasource::PartitionedFile;
use datafusion_datasource::file_groups::FileGroup;
use datafusion_datasource::file_scan_config::FileScanConfigBuilder;
use datafusion_datasource::memory::MemorySourceConfig;
use datafusion_datasource::source::DataSourceExec;
use llmobs_core::ids::ProjectId;
use llmobs_core::ids::TraceId;
use llmobs_core::schema::traces as trace_schema;

use crate::catalog::{Catalog, FileMeta, FileTable};
use crate::deletion::{PendingDeletion, applicable, covered_predicate, key_columns};
use crate::memtable::{MemBatch, MemSnapshot};
use crate::objects::OBJECT_STORE_URL;
use crate::wal::Table;

pub(crate) use scores::ScoresProvider;
pub(crate) use spans::SpansProvider;
pub(crate) use trace_list::TraceListRule;
pub(crate) use traces::TracesProvider;

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

    /// Bounds covering either predicate (`OR`). A side that sets no bound makes
    /// the union open on that end.
    fn union_with(&self, other: &TimeBounds) -> TimeBounds {
        TimeBounds {
            min: self.min.zip(other.min).map(|(a, b)| a.min(b)),
            max: self.max.zip(other.max).map(|(a, b)| a.max(b)),
        }
    }

    /// Bounds satisfying both predicates (`AND`).
    fn intersect_with(&self, other: &TimeBounds) -> TimeBounds {
        TimeBounds {
            min: match (self.min, other.min) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (Some(a), None) | (None, Some(a)) => Some(a),
                (None, None) => None,
            },
            max: match (self.max, other.max) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (Some(a), None) | (None, Some(a)) => Some(a),
                (None, None) => None,
            },
        }
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
        Expr::BinaryExpr(binary) if binary.op == Operator::Or => {
            // The bounds of an OR are the union of each disjunct's bounds. A
            // `column IS NULL` disjunct is dropped: `column` is non-null, so it
            // matches no row. This keeps A2's keyset cursor as a bounded scan.
            let mut disjuncts = Vec::new();
            collect_or(expr, &mut disjuncts);
            let mut union: Option<TimeBounds> = None;
            for disjunct in disjuncts {
                if is_null_on(disjunct, column) {
                    continue;
                }
                let mut separate = TimeBounds::open();
                walk(&mut separate, disjunct, column);
                union = Some(match union {
                    None => separate,
                    Some(acc) => acc.union_with(&separate),
                });
            }
            if let Some(union) = union {
                *bounds = bounds.intersect_with(&union);
            }
        }
        Expr::BinaryExpr(binary) => constrain_binary(bounds, binary, column),
        Expr::Between(between) if !between.negated => constrain_between(bounds, between, column),
        _ => {}
    }
}

/// Flattens a chain of `OR`s into its non-OR disjuncts.
fn collect_or<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    match peel(expr) {
        Expr::BinaryExpr(binary) if binary.op == Operator::Or => {
            collect_or(&binary.left, out);
            collect_or(&binary.right, out);
        }
        other => out.push(other),
    }
}

fn is_null_on(expr: &Expr, column: &str) -> bool {
    match peel(expr) {
        Expr::IsNull(inner) => matches!(peel(inner), Expr::Column(col) if col.name == column),
        _ => false,
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
///
/// `Clone` only: `schema` is an `Arc`, so `Copy` cannot hold. The tombstone
/// filter builds its extended-projection copy with a clone.
#[derive(Clone)]
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
    /// `?around=` hint for a single-trace lookup (D154).
    pub around: Option<i64>,
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
    /// The project's pending deletions; files they may cover go through the
    /// tombstone filter (spec 03 §7).
    pub deletions: &'a [Arc<PendingDeletion>],
}

/// The `trace_id`s named by a scan's top-level id conjuncts, or `None` when
/// the scan is not a lookup (D154).
///
/// Only top-level conjuncts are inspected: each filter and each side of an
/// `AND`, recursively. `OR`, `NOT`, `NOT IN`, `!=` and anything else are
/// ignored, never an error; they are still evaluated as ordinary filters.
pub(crate) fn lookup_ids(filters: &[Expr]) -> Option<BTreeSet<TraceId>> {
    let mut found: Option<BTreeSet<TraceId>> = None;
    for filter in filters {
        if let Some(ids) = conjunct_ids(filter) {
            found = Some(match found {
                None => ids,
                Some(existing) => existing.intersection(&ids).copied().collect(),
            });
        }
    }
    found
}

fn conjunct_ids(expr: &Expr) -> Option<BTreeSet<TraceId>> {
    match peel(expr) {
        Expr::BinaryExpr(binary) if binary.op == Operator::And => {
            match (conjunct_ids(&binary.left), conjunct_ids(&binary.right)) {
                (Some(left), Some(right)) => Some(left.intersection(&right).copied().collect()),
                (Some(ids), None) | (None, Some(ids)) => Some(ids),
                (None, None) => None,
            }
        }
        Expr::BinaryExpr(binary) if binary.op == Operator::Eq => {
            let literal = match (is_trace_id(&binary.left), is_trace_id(&binary.right)) {
                (true, false) => &binary.right,
                (false, true) => &binary.left,
                _ => return None,
            };
            let id = TraceId::from_slice(&literal_bytes(literal)?).ok()?;
            Some(std::iter::once(id).collect())
        }
        Expr::InList(in_list) if !in_list.negated && is_trace_id(&in_list.expr) => {
            let mut ids = BTreeSet::new();
            for item in &in_list.list {
                let bytes = literal_bytes(item)?;
                ids.insert(TraceId::from_slice(&bytes).ok()?);
            }
            Some(ids)
        }
        _ => None,
    }
}

fn peel(expr: &Expr) -> &Expr {
    match expr {
        Expr::Alias(alias) => peel(&alias.expr),
        other => other,
    }
}

fn is_trace_id(expr: &Expr) -> bool {
    matches!(peel(expr), Expr::Column(column) if column.name == trace_schema::TRACE_ID)
}

/// A non-null 16-byte id literal, in one of the three forms A2's hex rewrite
/// and the SQL parser produce. `arrow_cast` may reach the provider simplified
/// to `Cast`/`TryCast` or still as the scalar function.
fn literal_bytes(expr: &Expr) -> Option<Vec<u8>> {
    match peel(expr) {
        Expr::Literal(ScalarValue::FixedSizeBinary(16, Some(bytes)), _) if bytes.len() == 16 => {
            Some(bytes.clone())
        }
        Expr::Literal(value, _) => match value {
            ScalarValue::Binary(Some(bytes))
            | ScalarValue::LargeBinary(Some(bytes))
            | ScalarValue::BinaryView(Some(bytes))
                if bytes.len() == 16 =>
            {
                Some(bytes.clone())
            }
            _ => None,
        },
        Expr::Cast(cast) if cast.field.data_type() == &DataType::FixedSizeBinary(16) => {
            literal_bytes(&cast.expr)
        }
        Expr::TryCast(cast) if cast.field.data_type() == &DataType::FixedSizeBinary(16) => {
            literal_bytes(&cast.expr)
        }
        Expr::ScalarFunction(func) if func.name() == "arrow_cast" => {
            literal_bytes(func.args.first()?)
        }
        _ => None,
    }
}

/// Files whose day and `[min_time, max_time]` overlap the time filters.
///
/// A lookup with `around` and no `start_time` bound reads only files in the
/// `±1 day` window; without `around` it reads every file of the project and
/// leaves the pruning to DataFusion's statistics and bloom filters (D154).
pub(crate) fn overlapping_files(
    catalog: &Catalog,
    table: Table,
    project: ProjectId,
    time_column: &str,
    filters: &[Expr],
    around: Option<i64>,
) -> Vec<Arc<FileMeta>> {
    let bounds = time_bounds(filters, time_column);
    if bounds.contradicts() {
        return Vec::new();
    }
    let file_table = FileTable::from(table);
    let files: Vec<Arc<FileMeta>> = catalog
        .files(file_table, project, file_day_range(&bounds, file_table))
        .filter(|file| file_overlaps(file, &bounds))
        .cloned()
        .collect();
    let lookup = lookup_ids(filters).is_some() && bounds.min.is_none() && bounds.max.is_none();
    if lookup && let Some(instant) = around {
        let window = TimeBounds {
            min: Some(instant.saturating_sub(DAY_NS)),
            max: Some(instant.saturating_add(DAY_NS)),
        };
        return files
            .into_iter()
            .filter(|file| file_overlaps(file, &window))
            .collect();
    }
    files
}

/// Union of the Parquet files and the memtable. An empty side is left out.
///
/// Files a pending deletion may cover go through [`gated_file_plan`] instead of
/// [`file_plan`] (spec 03 §7). The gated plan counts as a side of the union,
/// so no side keeps a limit that the other side could starve.
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
        scan.around,
    );
    let mut clean = Vec::with_capacity(files.len());
    let mut gated = Vec::with_capacity(files.len());
    for file in files {
        if applicable(
            scan.deletions,
            Some(file.deletions_applied_seq),
            file.min_ingest_time,
        )
        .is_empty()
        {
            clean.push(file);
        } else {
            gated.push(file);
        }
    }
    let sides = [!clean.is_empty(), !gated.is_empty(), !batches.is_empty()]
        .iter()
        .filter(|side| **side)
        .count();
    let several = sides > 1;
    // A limit on each side would drop rows the other side still has.
    let side_limit = if several { None } else { scan.limit };
    let mut plans = Vec::new();
    if !clean.is_empty() {
        plans.push(file_plan(scan, &clean, side_limit)?);
    }
    if !gated.is_empty() {
        plans.push(gated_file_plan(scan, &gated)?);
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
    if several
        && scan.filters.is_empty()
        && let Some(fetch) = scan.limit
    {
        return Ok(Arc::new(GlobalLimitExec::new(plan, 0, Some(fetch))));
    }
    Ok(plan)
}

/// The file side for files a pending deletion may cover: the scan projection is
/// extended with the key columns the covered-row predicate reads, a filter
/// drops the covered rows, and a projection restores the requested columns
/// (spec 03 §7).
///
/// The predicate is built against every pending deletion of the project, not
/// only the applicable ones: applying a deletion to a file already clean of it
/// removes nothing, and the cut-off is tested per row.
fn gated_file_plan(
    scan: &ScanUnion<'_>,
    gated: &[Arc<FileMeta>],
) -> datafusion::error::Result<Arc<dyn ExecutionPlan>> {
    let table = FileTable::from(scan.table);
    let requested: Vec<usize> = scan
        .projection
        .cloned()
        .unwrap_or_else(|| (0..scan.schema.fields().len()).collect());
    let mut indices = requested.clone();
    let mut missing: Vec<usize> = Vec::new();
    for column in key_columns(table) {
        let index = scan.schema.index_of(column).map_err(|_| {
            DataFusionError::Plan(format!("{column} is missing from the stored schema"))
        })?;
        if !indices.contains(&index) && !missing.contains(&index) {
            missing.push(index);
        }
    }
    // Appended key columns go in schema order, so the projected schema is
    // deterministic.
    missing.sort_unstable();
    indices.extend(missing.iter().copied());

    // A copy of the scan with the extended projection, and no limit: the filter
    // runs above it, so a limit here could drop rows the filter keeps.
    let extended = ScanUnion {
        projection: Some(&indices),
        ..scan.clone()
    };
    let mut plan = file_plan(&extended, gated, None)?;
    let projected = Arc::new(scan.schema.project(&indices)?);
    if let Some(predicate) = covered_predicate(table, &projected, scan.deletions)? {
        plan = Arc::new(FilterExec::try_new(
            Arc::new(NotExpr::new(predicate)),
            plan,
        )?);
    }
    if !missing.is_empty() {
        let keeps: Vec<(Arc<dyn PhysicalExpr>, String)> = (0..requested.len())
            .map(|position| {
                let name = scan.schema.field(indices[position]).name().to_owned();
                (
                    Arc::new(Column::new(&name, position)) as Arc<dyn PhysicalExpr>,
                    name,
                )
            })
            .collect();
        plan = Arc::new(ProjectionExec::try_new(keeps, plan)?);
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
        .map_err(|error| DataFusionError::Internal(error.to_string()))?;
    let mut builder = FileScanConfigBuilder::new(url, Arc::new(source))
        .with_file_groups(file_groups(files, scan.target_partitions))
        // No work stealing: each partition reads only its own files.
        .with_preserve_order(true);
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
        let cast = Expr::Cast(Cast::new(Box::new(comparison), DataType::Boolean));
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
    fn an_or_unions_bounds_and_drops_a_null_side() {
        // A2's keyset cursor: (st < x) OR (st = x AND ties) OR st IS NULL.
        // `start_time IS NULL` can't match (the column is non-null), so the
        // union is (-inf, x]: b = x, no lower bound.
        let cursor = column_cmp(Operator::Lt, nano(10, None))
            .or(column_cmp(Operator::Eq, nano(10, None)).and(col("name").eq(lit("x"))))
            .or(Expr::IsNull(Box::new(col("start_time"))));
        assert_eq!(
            bounds_of(cursor),
            TimeBounds {
                min: None,
                max: Some(10)
            }
        );
        // Two bounded sides: the union takes the weaker end of each.
        let both = column_cmp(Operator::GtEq, nano(5, None))
            .and(column_cmp(Operator::LtEq, nano(8, None)))
            .or(column_cmp(Operator::GtEq, nano(1, None))
                .and(column_cmp(Operator::LtEq, nano(3, None))));
        assert_eq!(
            bounds_of(both),
            TimeBounds {
                min: Some(1),
                max: Some(8)
            }
        );
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

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn id(last: u8) -> TraceId {
        let mut bytes = [0_u8; 16];
        bytes[15] = last;
        TraceId::from_bytes(bytes).unwrap()
    }

    fn fixed(bytes: &[u8]) -> Expr {
        Expr::Literal(ScalarValue::FixedSizeBinary(16, Some(bytes.to_vec())), None)
    }

    fn binary(bytes: &[u8]) -> Expr {
        Expr::Literal(ScalarValue::Binary(Some(bytes.to_vec())), None)
    }

    /// `arrow_cast(X'..', 'FixedSizeBinary(16)')`, the form S1 found. The SQL
    /// simplifier turns it into a `Cast` over the binary literal; the unsimplified
    /// scalar-function form is checked separately.
    fn arrow_cast_cast(bytes: &[u8]) -> Expr {
        Expr::Cast(Cast::new(
            Box::new(binary(bytes)),
            DataType::FixedSizeBinary(16),
        ))
    }

    fn arrow_cast_fn(bytes: &[u8]) -> Expr {
        datafusion::functions::core::expr_fn::arrow_cast(binary(bytes), lit("FixedSizeBinary(16)"))
    }

    fn eq(left: Expr, right: Expr) -> Expr {
        Expr::BinaryExpr(BinaryExpr::new(
            Box::new(left),
            Operator::Eq,
            Box::new(right),
        ))
    }

    fn in_list(expr: Expr, list: Vec<Expr>, negated: bool) -> Expr {
        Expr::InList(datafusion::logical_expr::expr::InList::new(
            Box::new(expr),
            list,
            negated,
        ))
    }

    fn trace_col() -> Expr {
        col(trace_schema::TRACE_ID)
    }

    fn good_id() -> Expr {
        fixed(id(3).as_bytes())
    }

    #[test]
    fn lookup_ids_accepts_the_three_literal_forms_and_in_lists() {
        let seven = id(7);
        let bytes = seven.as_bytes();
        let want: BTreeSet<TraceId> = [seven].into_iter().collect();
        let forms = [
            eq(trace_col(), fixed(bytes)),
            eq(fixed(bytes), trace_col()),
            eq(trace_col(), binary(bytes)),
            eq(trace_col(), arrow_cast_cast(bytes)),
            eq(trace_col(), arrow_cast_fn(bytes)),
            in_list(trace_col(), vec![fixed(bytes)], false),
            in_list(trace_col(), vec![binary(bytes)], false),
            in_list(trace_col(), vec![arrow_cast_cast(bytes)], false),
        ];
        for expr in forms {
            assert_eq!(
                lookup_ids(std::slice::from_ref(&expr)),
                Some(want.clone()),
                "{expr}"
            );
        }
        // `IN ()` is a lookup with no ids.
        assert_eq!(
            lookup_ids(&[in_list(trace_col(), vec![], false)]),
            Some(BTreeSet::new())
        );
        let eight = id(8);
        assert_eq!(
            lookup_ids(&[in_list(
                trace_col(),
                vec![fixed(bytes), fixed(eight.as_bytes())],
                false,
            )]),
            Some([seven, eight].into_iter().collect())
        );
    }

    #[test]
    fn lookup_ids_intersects_several_id_conjuncts() {
        let one = eq(trace_col(), fixed(id(1).as_bytes()));
        let two = eq(trace_col(), fixed(id(2).as_bytes()));
        assert_eq!(
            lookup_ids(&[one.clone(), two.clone()]),
            Some(BTreeSet::new())
        );
        assert_eq!(
            lookup_ids(std::slice::from_ref(&one.clone().and(two))),
            Some(BTreeSet::new())
        );
        assert_eq!(
            lookup_ids(std::slice::from_ref(&one)),
            Some([id(1)].into_iter().collect())
        );
        assert_eq!(
            lookup_ids(&[one, col("name").eq(lit("x"))]),
            Some([id(1)].into_iter().collect())
        );
    }

    #[test]
    fn lookup_ids_ignores_non_id_conjuncts() {
        let cases = [
            eq(trace_col(), binary(&[0_u8; 15])),
            eq(trace_col(), col("other")),
            eq(trace_col(), good_id()).or(col("name").eq(lit("x"))),
            Expr::Not(Box::new(eq(trace_col(), good_id()))),
            Expr::BinaryExpr(BinaryExpr::new(
                Box::new(trace_col()),
                Operator::NotEq,
                Box::new(good_id()),
            )),
            in_list(trace_col(), vec![good_id()], true),
            in_list(
                trace_col(),
                vec![good_id(), Expr::Literal(ScalarValue::Binary(None), None)],
                false,
            ),
            in_list(trace_col(), vec![good_id(), binary(&[0_u8; 15])], false),
            col("name").eq(lit("x")),
        ];
        for expr in cases {
            assert_eq!(lookup_ids(std::slice::from_ref(&expr)), None, "{expr}");
        }
    }

    fn file_meta(day: NaiveDate, min_time: i64, max_time: i64) -> FileMeta {
        FileMeta {
            id: crate::catalog::FileId::new_v7(),
            table: FileTable::Spans,
            project: project(),
            day,
            level: 0,
            path: format!("tables/spans/day={day}/0.parquet"),
            size: 1,
            footer_size: 1,
            rows: 1,
            min_time,
            max_time,
            min_ingest_time: min_time,
            max_ingest_time: max_time,
            fully_merged: false,
            deletions_applied_seq: 0,
        }
    }

    fn catalog_with(files: Vec<FileMeta>) -> Catalog {
        Catalog::empty()
            .apply(&crate::catalog::CatalogEdit {
                add: files,
                remove: Vec::new(),
                flushed_seq: None,
                sources: Vec::new(),
                raise: Vec::new(),
            })
            .unwrap()
            .0
    }

    #[test]
    fn span_file_days_widen_by_one_day_on_each_side() {
        let day = NaiveDate::from_ymd_opt(2026, 3, 5).unwrap();
        let base = (i64::from(day.num_days_from_ce()) - 719_163) * DAY_NS;
        let bounds = TimeBounds {
            min: Some(base),
            max: Some(base + 10),
        };
        let range = file_day_range(&bounds, FileTable::Spans);
        assert_eq!(*range.start(), day.pred_opt().unwrap());
        assert_eq!(*range.end(), day.succ_opt().unwrap());
        let exact = file_day_range(&bounds, FileTable::Scores);
        assert_eq!(*exact.start(), day);
        assert_eq!(*exact.end(), day);
    }

    #[test]
    fn around_selects_only_the_files_within_one_day_of_the_hint() {
        let day = NaiveDate::from_ymd_opt(2026, 3, 5).unwrap();
        let earlier = day.pred_opt().unwrap().pred_opt().unwrap();
        let later = day.succ_opt().unwrap().succ_opt().unwrap();
        let base = (i64::from(day.num_days_from_ce()) - 719_163) * DAY_NS;
        let catalog = catalog_with(vec![
            file_meta(earlier, base - 2 * DAY_NS, base - 2 * DAY_NS + 10),
            file_meta(day, base, base + 10),
            file_meta(later, base + 2 * DAY_NS, base + 2 * DAY_NS + 10),
        ]);
        let filter = eq(trace_col(), fixed(id(1).as_bytes()));
        let selected = overlapping_files(
            &catalog,
            Table::Spans,
            project(),
            "start_time",
            std::slice::from_ref(&filter),
            Some(base),
        );
        assert_eq!(selected.len(), 1, "{selected:?}");
        assert_eq!(selected[0].day, day);
        // The file groups of the physical plan hold the same single file.
        let groups = file_groups(&selected, 2);
        assert_eq!(groups.len(), 1);
    }

    #[test]
    fn a_lookup_without_around_reads_files_from_every_day() {
        let day = NaiveDate::from_ymd_opt(2026, 3, 5).unwrap();
        let next = day.succ_opt().unwrap();
        let base = (i64::from(day.num_days_from_ce()) - 719_163) * DAY_NS;
        let catalog = catalog_with(vec![
            file_meta(day, base, base + 10),
            file_meta(next, base + DAY_NS, base + DAY_NS + 10),
        ]);
        let filter = eq(trace_col(), fixed(id(1).as_bytes()));
        let selected = overlapping_files(
            &catalog,
            Table::Spans,
            project(),
            "start_time",
            std::slice::from_ref(&filter),
            None,
        );
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[0].day, day);
        assert_eq!(selected[1].day, next);
    }
}
