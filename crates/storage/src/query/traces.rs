//! `traces` table provider over one snapshot's `traces` files and the spans
//! memtable (D152).
//!
//! A scan reads *partials* — one row per trace per file or per memtable batch —
//! hash-partitions them by `trace_id`, and merges each partition into one row
//! per trace with [`TraceMerger`]. Filters are answered `Exact`, so they are
//! evaluated on the *merged* rows, after the merge, not on the partials.
//!
//! M3.7 adds *list mode* (D153): for the "latest N" plan the provider reads a
//! few short time slices of partials, ranks trace ids, then fetches only the
//! rows it will return.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use arrow::array::{
    Array, ArrayRef, BooleanArray, FixedSizeBinaryArray, RecordBatch, RecordBatchOptions,
    TimestampNanosecondArray,
};
use arrow::compute::{
    SortColumn, SortOptions, concat_batches, filter_record_batch, lexsort_to_indices,
    take_record_batch,
};
use arrow::datatypes::{Schema, SchemaRef};
use async_trait::async_trait;
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::tree_node::TreeNodeRecursion;
use datafusion::common::{DFSchema, DataFusionError, ScalarValue};
use datafusion::datasource::physical_plan::ParquetSource;
use datafusion::error::Result as DfResult;
use datafusion::execution::SessionState;
use datafusion::execution::TaskContext;
use datafusion::execution::memory_pool::{MemoryConsumer, MemoryReservation};
use datafusion::execution::object_store::ObjectStoreUrl;
use datafusion::logical_expr::expr::InList;
use datafusion::logical_expr::{
    BinaryExpr, Expr, Operator, TableProviderFilterPushDown, TableType, col,
};
use datafusion::physical_expr::EquivalenceProperties;
use datafusion::physical_expr::expressions::{Column, NotExpr};
use datafusion::physical_expr::utils::conjunction_opt;
use datafusion::physical_expr::{
    LexOrdering, LexRequirement, OrderingRequirements, Partitioning, PhysicalExpr,
    PhysicalSortExpr, PhysicalSortRequirement,
};
use datafusion::physical_plan::execution_plan::{
    Boundedness, EmissionType, collect as collect_plan,
};
use datafusion::physical_plan::filter::FilterExec;
use datafusion::physical_plan::repartition::RepartitionExec;
use datafusion::physical_plan::sorts::sort::SortExec;
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::union::UnionExec;
use datafusion::physical_plan::{
    DisplayAs, DisplayFormatType, Distribution, ExecutionPlan, InputDistributionRequirements,
    PlanProperties, SendableRecordBatchStream,
};
use datafusion_datasource::file_scan_config::FileScanConfigBuilder;
use datafusion_datasource::memory::MemorySourceConfig;
use datafusion_datasource::source::DataSourceExec;
use futures::stream::{Stream, StreamExt};
use llmobs_core::ids::{ProjectId, TraceId};
use llmobs_core::schema::{traces, traces_schema};

use super::{
    DAY_NS, TimeBounds, distribute, file_day_range, file_groups, file_overlaps, lookup_ids,
    range_overlaps, time_bounds,
};
use crate::QueryOptions;
use crate::catalog::{Catalog, FileMeta, FileTable};
use crate::deletion::{PendingDeletion, applicable, covered_predicate};
use crate::memtable::{MemBatch, MemSnapshot};
use crate::objects::OBJECT_STORE_URL;
use crate::partials::{MergeColumns, TraceMerger};
use crate::verify::Suspects;

/// Traces of one project at one read snapshot (D152/D154, D153 in list mode).
#[derive(Debug)]
pub(crate) struct TracesProvider {
    project: ProjectId,
    catalog: Arc<Catalog>,
    /// The spans memtable of the same read state. Its batches hold unflushed
    /// spans, which the scan turns into partials on demand.
    spans_mem: Arc<MemSnapshot>,
    options: QueryOptions,
    lookback: i64,
    partitions: usize,
    /// The project's pending deletions (spec 03 §7). The `traces` view hides a
    /// covered trace's partials by trace id alone.
    deletions: Arc<[Arc<PendingDeletion>]>,
    suspects: Suspects,
}

impl TracesProvider {
    #[allow(clippy::too_many_arguments)] // one input per field, the query's suspects last
    pub(crate) fn new(
        project: ProjectId,
        catalog: Arc<Catalog>,
        spans_mem: Arc<MemSnapshot>,
        options: QueryOptions,
        lookback: i64,
        partitions: usize,
        deletions: Arc<[Arc<PendingDeletion>]>,
        suspects: Suspects,
    ) -> Self {
        Self {
            project,
            catalog,
            spans_mem,
            options,
            lookback,
            partitions,
            deletions,
            suspects,
        }
    }
}

/// How a `TracesExec` runs. List mode is installed by `TraceListRule` (D153).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Range,
    Lookup,
    List { fetch: usize },
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Range => "range",
            Self::Lookup => "lookup",
            Self::List { .. } => "list",
        }
    }
}

/// Everything a range-mode plan needs. Stored on `TracesExec` so list mode can
/// build fresh inner plans at execution time, and shared by `scan` and list run.
#[derive(Clone)]
struct ScanContext {
    project: ProjectId,
    catalog: Arc<Catalog>,
    spans_mem: Arc<MemSnapshot>,
    /// Owned session state, used to plan physical filter expressions when the
    /// inner slice plans are built during execution.
    session: Arc<SessionState>,
    lookback: i64,
    partitions: usize,
    /// Logical filters pushed `Exact` into the scan.
    filters: Vec<Expr>,
    around: Option<i64>,
    /// The project's pending deletions (spec 03 §7).
    deletions: Arc<[Arc<PendingDeletion>]>,
    /// The query's suspects, for the file plans of this scan.
    suspects: Suspects,
}

impl ScanContext {
    fn new(provider: &TracesProvider, state: &dyn Session, filters: &[Expr]) -> DfResult<Self> {
        let session = state
            .as_any()
            .downcast_ref::<SessionState>()
            .ok_or_else(|| {
                DataFusionError::Internal("traces scan requires a SessionState".into())
            })?;
        Ok(Self {
            project: provider.project,
            catalog: Arc::clone(&provider.catalog),
            spans_mem: Arc::clone(&provider.spans_mem),
            session: Arc::new(session.clone()),
            lookback: provider.lookback,
            partitions: provider.partitions,
            filters: filters.to_vec(),
            around: provider.options.around,
            deletions: Arc::clone(&provider.deletions),
            suspects: provider.suspects.clone(),
        })
    }

    fn session(&self) -> &dyn Session {
        self.session.as_ref()
    }
}

/// One range-mode plan's parameters. `build_range_plan` is the single builder
/// shared by `scan` and list mode's phase-1/phase-2 inner plans.
struct RangeSpec {
    /// Unpadded window: merged `start_time` must lie within it.
    bounds: TimeBounds,
    mode: Mode,
    merge_columns: MergeColumns,
    /// `trace_id IN ids` added to the partial-scan predicate (phase 2).
    scan_ids: Option<BTreeSet<TraceId>>,
    projection: Projection,
    output_schema: SchemaRef,
    limit: Option<usize>,
    /// Wrap the merge in a global sort with `fetch = rank` (phase 1 top-k),
    /// so the plan emits at most `rank` rows and list mode stays bounded.
    rank: Option<usize>,
}

#[async_trait]
impl TableProvider for TracesProvider {
    fn schema(&self) -> SchemaRef {
        traces_schema()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    /// Every filter is evaluated by `TracesExec` on the merged rows (D152).
    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> DfResult<Vec<TableProviderFilterPushDown>> {
        Ok(vec![TableProviderFilterPushDown::Exact; filters.len()])
    }

    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> DfResult<Arc<dyn ExecutionPlan>> {
        let schema = traces_schema();
        let ctx = ScanContext::new(self, state, filters)?;
        let bounds = time_bounds(filters, traces::START_TIME);
        let ids = lookup_ids(filters);
        let requested = requested_names(projection, filters, &schema)?;
        let requested_refs: Vec<&str> = requested.iter().map(String::as_str).collect();
        let merge_columns = MergeColumns::new(&requested_refs);
        let (projection_kind, output_schema) = output_of(projection, &schema)?;
        let mode = if ids.is_some() {
            Mode::Lookup
        } else {
            Mode::Range
        };
        Ok(build_range_plan(
            &ctx,
            RangeSpec {
                bounds,
                mode,
                merge_columns,
                scan_ids: ids,
                projection: projection_kind,
                output_schema,
                limit,
                rank: None,
            },
        )?)
    }
}

/// Builds one complete range-mode `TracesExec` (with its repartition and scans),
/// optionally wrapped in a global top-k sort for list mode's phase 1.
fn build_range_plan(ctx: &ScanContext, spec: RangeSpec) -> DfResult<Arc<dyn ExecutionPlan>> {
    let schema = traces_schema();
    let padded = padded_bounds(spec.bounds, ctx.lookback);

    let input_names = spec.merge_columns.input();
    let output_names = spec.merge_columns.output();
    let input_indices = indices_of(&schema, &input_names)?;
    let output_indices = indices_of(&schema, &output_names)?;
    let input_schema: SchemaRef = Arc::new(project_schema(&schema, &input_indices));
    let merged_schema: SchemaRef = Arc::new(project_schema(&schema, &output_indices));

    let full_df = DFSchema::try_from(Arc::clone(&schema))?;
    let input_df = DFSchema::try_from(Arc::clone(&input_schema))?;
    let merged_df = DFSchema::try_from(Arc::clone(&merged_schema))?;

    // Partial-level predicate: only the padded window and the trace ids. A
    // filter that passes only after the merge is never pushed down here.
    let scan_exprs = scan_exprs(&padded, &spec.scan_ids);
    let file_pred = to_physical(ctx.session(), &full_df, &scan_exprs)?;
    let mem_pred = to_physical(ctx.session(), &input_df, &scan_exprs)?;

    let files = trace_files(
        &ctx.catalog,
        ctx.project,
        &padded,
        &ctx.filters,
        &spec.bounds,
        ctx.around,
    );
    // A `traces` file is gated by `deletions_applied_seq` alone: its
    // `min_ingest_time` is the smallest per-trace *latest* ingest time, so it
    // cannot bound a cut-off (spec 03 §7).
    let mut clean = Vec::with_capacity(files.len());
    let mut gated = Vec::with_capacity(files.len());
    for file in files {
        if applicable(&ctx.deletions, Some(file.deletions_applied_seq), i64::MIN).is_empty() {
            clean.push(file);
        } else {
            gated.push(file);
        }
    }
    let batches = mem_batches(&ctx.spans_mem, ctx.project, &padded);
    let mut partials = Vec::with_capacity(batches.len());
    for batch in &batches {
        let cached = batch.trace_partials().map_err(partial_err)?;
        partials.push((*cached).clone());
    }

    let mut plans: Vec<Arc<dyn ExecutionPlan>> = Vec::new();
    if !clean.is_empty() {
        plans.push(file_plan(
            &schema,
            &input_indices,
            &clean,
            &input_df,
            file_pred.as_ref(),
            ctx.partitions,
            &ctx.suspects,
        )?);
    }
    if !gated.is_empty() {
        // The merge input always holds `trace_id`, so the trace-id-only rule
        // fits the input schema. A `None` predicate (no deletion has trace
        // ids) means no filter.
        let gated_plan = file_plan(
            &schema,
            &input_indices,
            &gated,
            &input_df,
            file_pred.as_ref(),
            ctx.partitions,
            &ctx.suspects,
        )?;
        let gated_plan = match covered_predicate(FileTable::Traces, &input_schema, &ctx.deletions)?
        {
            Some(predicate) => Arc::new(FilterExec::try_new(
                Arc::new(NotExpr::new(predicate)),
                gated_plan,
            )?) as Arc<dyn ExecutionPlan>,
            None => gated_plan,
        };
        plans.push(gated_plan);
    }
    plans.push(mem_plan(
        &schema,
        &input_indices,
        partials,
        ctx.partitions,
        mem_pred,
    )?);
    let union = UnionExec::try_new(plans)?;

    let trace_id_index = input_names
        .iter()
        .position(|name| *name == traces::TRACE_ID)
        .ok_or_else(|| DataFusionError::Internal("trace_id missing from merge input".into()))?;
    let key: Arc<dyn PhysicalExpr> = Arc::new(Column::new(traces::TRACE_ID, trace_id_index));
    let partitions = if spec.limit.is_some() || spec.rank.is_some() {
        1
    } else {
        ctx.partitions
    };
    let repartition: Arc<dyn ExecutionPlan> = Arc::new(RepartitionExec::try_new(
        union,
        Partitioning::Hash(vec![Arc::clone(&key)], partitions),
    )?);
    // Partials of one trace end up next to each other, so the merge holds one
    // chunk of traces at a time; the sort spills when its share is refused (D-A1-13).
    let ordering = LexOrdering::new([PhysicalSortExpr::new(
        Arc::clone(&key),
        SortOptions::default(),
    )])
    .ok_or_else(|| DataFusionError::Internal("empty trace_id ordering".into()))?;
    let sorted: Arc<dyn ExecutionPlan> =
        Arc::new(SortExec::new(ordering, repartition).with_preserve_partitioning(true));

    // Merged-level filters, converted once against the merged output schema.
    let mut physical_filters = Vec::with_capacity(ctx.filters.len());
    for filter in &ctx.filters {
        physical_filters.push(
            ctx.session
                .create_physical_expr(filter.clone(), &merged_df)?,
        );
    }
    let predicate = conjunction_opt(physical_filters);

    let columns = input_names.join(", ");
    let display = format!(
        "TracesExec: mode={}, files={}, mem_batches={}, columns=[{columns}]",
        spec.mode.as_str(),
        clean.len() + gated.len(),
        batches.len(),
    );

    let exec: Arc<dyn ExecutionPlan> = Arc::new(TracesExec {
        input: sorted,
        properties: Arc::new(properties_for(
            &spec.output_schema,
            partitions,
            EmissionType::Incremental,
        )),
        scan_ctx: ctx.clone(),
        mode: spec.mode,
        merge_columns: spec.merge_columns,
        key: vec![key],
        predicate,
        bounds: spec.bounds,
        projection: spec.projection,
        output_schema: Arc::clone(&spec.output_schema),
        limit: spec.limit,
        display,
    });
    let Some(fetch) = spec.rank else {
        return Ok(exec);
    };
    // A single-partition TopK: the slice merge is still memory-reserved by its
    // own TracesExec, but the sort emits only `fetch` rows, so the caller
    // never holds an unbounded set of matching traces.
    let start_index = spec.output_schema.index_of(traces::START_TIME)?;
    let trace_index = spec.output_schema.index_of(traces::TRACE_ID)?;
    let descending = SortOptions {
        descending: true,
        nulls_first: false,
    };
    let ordering = LexOrdering::new([
        PhysicalSortExpr::new(
            Arc::new(Column::new(traces::START_TIME, start_index)),
            descending,
        ),
        PhysicalSortExpr::new(
            Arc::new(Column::new(traces::TRACE_ID, trace_index)),
            descending,
        ),
    ])
    .ok_or_else(|| DataFusionError::Internal("empty top-k ordering".into()))?;
    let sort = SortExec::new(ordering, exec).with_fetch(Some(fetch));
    Ok(Arc::new(sort))
}

fn properties_for(schema: &SchemaRef, partitions: usize, emission: EmissionType) -> PlanProperties {
    PlanProperties::new(
        EquivalenceProperties::new(Arc::clone(schema)),
        Partitioning::UnknownPartitioning(partitions),
        emission,
        Boundedness::Bounded,
    )
}

/// `[a - L, b + L]`, with a missing end left open. With no bounds at all
/// nothing is padded. All arithmetic saturates so an extreme hint cannot wrap.
fn padded_bounds(bounds: TimeBounds, lookback: i64) -> TimeBounds {
    if bounds.min.is_none() && bounds.max.is_none() {
        return TimeBounds {
            min: None,
            max: None,
        };
    }
    TimeBounds {
        min: bounds.min.map(|a| a.saturating_sub(lookback)),
        max: bounds.max.map(|b| b.saturating_add(lookback)),
    }
}

/// Projection names plus every filter's columns, as owned strings.
fn requested_names(
    projection: Option<&Vec<usize>>,
    filters: &[Expr],
    schema: &SchemaRef,
) -> DfResult<Vec<String>> {
    let mut names: Vec<String> = Vec::new();
    match projection {
        None => names.extend(schema.fields().iter().map(|field| field.name().clone())),
        Some(indices) => {
            for &index in indices {
                names.push(schema.field(index).name().clone());
            }
        }
    }
    for filter in filters {
        for column in filter.column_refs() {
            names.push(column.name.clone());
        }
    }
    Ok(names)
}

fn filter_names(filters: &[Expr]) -> Vec<String> {
    let mut names = Vec::new();
    for filter in filters {
        for column in filter.column_refs() {
            names.push(column.name.clone());
        }
    }
    names
}

fn projection_names(projection: &Projection, schema: &SchemaRef) -> Vec<String> {
    match projection {
        Projection::All => schema.fields().iter().map(|f| f.name().clone()).collect(),
        Projection::Indices(names) => names.clone(),
        Projection::Empty => Vec::new(),
    }
}

fn indices_of(schema: &SchemaRef, names: &[&str]) -> DfResult<Vec<usize>> {
    names
        .iter()
        .map(|name| Ok(schema.index_of(name)?))
        .collect()
}

fn project_schema(schema: &SchemaRef, indices: &[usize]) -> Schema {
    Schema::new(
        indices
            .iter()
            .map(|&index| schema.field(index).clone())
            .collect::<Vec<_>>(),
    )
}

fn output_of(
    projection: Option<&Vec<usize>>,
    schema: &SchemaRef,
) -> DfResult<(Projection, SchemaRef)> {
    match projection {
        None => Ok((Projection::All, Arc::clone(schema))),
        Some(indices) if indices.is_empty() => Ok((Projection::Empty, Arc::new(Schema::empty()))),
        Some(indices) => {
            let names: Vec<String> = indices
                .iter()
                .map(|&index| schema.field(index).name().clone())
                .collect();
            let fields: Vec<_> = indices
                .iter()
                .map(|&index| schema.field(index).clone())
                .collect();
            Ok((Projection::Indices(names), Arc::new(Schema::new(fields))))
        }
    }
}

fn ts_lit(value: i64) -> Expr {
    Expr::Literal(
        ScalarValue::TimestampNanosecond(Some(value), Some(Arc::from("UTC"))),
        None,
    )
}

fn bound_expr(op: Operator, value: i64) -> Expr {
    Expr::BinaryExpr(BinaryExpr::new(
        Box::new(col(traces::START_TIME)),
        op,
        Box::new(ts_lit(value)),
    ))
}

fn in_ids_expr(ids: &BTreeSet<TraceId>) -> Expr {
    let list: Vec<Expr> = ids
        .iter()
        .map(|id| {
            Expr::Literal(
                ScalarValue::FixedSizeBinary(16, Some(id.as_bytes().to_vec())),
                None,
            )
        })
        .collect();
    Expr::InList(InList::new(Box::new(col(traces::TRACE_ID)), list, false))
}

/// Predicate pushed onto the partials: `start_time` in the padded window and
/// `trace_id IN ids`. Both are safe to evaluate before the merge.
fn scan_exprs(padded: &TimeBounds, ids: &Option<BTreeSet<TraceId>>) -> Vec<Expr> {
    let mut exprs = Vec::new();
    if let Some(min) = padded.min {
        exprs.push(bound_expr(Operator::GtEq, min));
    }
    if let Some(max) = padded.max {
        exprs.push(bound_expr(Operator::LtEq, max));
    }
    if let Some(ids) = ids {
        if ids.is_empty() {
            exprs.push(Expr::Literal(ScalarValue::Boolean(Some(false)), None));
        } else {
            exprs.push(in_ids_expr(ids));
        }
    }
    exprs
}

fn to_physical(
    session: &dyn Session,
    df_schema: &DFSchema,
    exprs: &[Expr],
) -> DfResult<Option<Arc<dyn PhysicalExpr>>> {
    let mut physical = Vec::with_capacity(exprs.len());
    for expr in exprs {
        physical.push(session.create_physical_expr(expr.clone(), df_schema)?);
    }
    Ok(conjunction_opt(physical))
}

/// Active `traces` files: one-day anchor slack, `[min_time, max_time]` against
/// the padded window, and the `around` hint for lookups with no time bound.
fn trace_files(
    catalog: &Catalog,
    project: ProjectId,
    padded: &TimeBounds,
    filters: &[Expr],
    bounds: &TimeBounds,
    around: Option<i64>,
) -> Vec<Arc<FileMeta>> {
    if bounds.contradicts() {
        return Vec::new();
    }
    let files: Vec<Arc<FileMeta>> = catalog
        .files(
            FileTable::Traces,
            project,
            file_day_range(padded, FileTable::Traces),
        )
        .filter(|file| file_overlaps(file, padded))
        .cloned()
        .collect();
    let lookup_without_bounds =
        lookup_ids(filters).is_some() && bounds.min.is_none() && bounds.max.is_none();
    if lookup_without_bounds && let Some(instant) = around {
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

/// Spans memtable batches of the project, active and frozen, every day, whose
/// `[min_time, max_time]` overlaps the padded window (all of them when open).
fn mem_batches(snapshot: &MemSnapshot, project: ProjectId, padded: &TimeBounds) -> Vec<MemBatch> {
    let mut selected = Vec::new();
    for (key, batches) in &snapshot.partitions {
        if key.project != project {
            continue;
        }
        for batch in batches {
            if range_overlaps(batch, padded) {
                // `MemBatch::clone` shares the `Arc<OnceLock<...>>` partials
                // cache with every other read state holding this batch.
                selected.push(batch.clone());
            }
        }
    }
    selected
}

fn file_plan(
    schema: &SchemaRef,
    input_indices: &[usize],
    files: &[Arc<FileMeta>],
    _input_df: &DFSchema,
    predicate: Option<&Arc<dyn PhysicalExpr>>,
    partitions: usize,
    suspects: &Suspects,
) -> DfResult<Arc<dyn ExecutionPlan>> {
    suspects.record(files);
    let mut source = ParquetSource::new(Arc::clone(schema))
        .with_pushdown_filters(true)
        .with_enable_page_index(true);
    if let Some(predicate) = predicate {
        source = source.with_predicate(Arc::clone(predicate));
    }
    let url = ObjectStoreUrl::parse(OBJECT_STORE_URL)
        .map_err(|error| DataFusionError::Internal(error.to_string()))?;
    let builder = FileScanConfigBuilder::new(url, Arc::new(source))
        .with_file_groups(file_groups(files, partitions))
        // No work stealing: each partition reads only its own files.
        .with_preserve_order(true)
        .with_projection_indices(Some(input_indices.to_vec()))?;
    Ok(DataSourceExec::from_data_source(builder.build()))
}

fn mem_plan(
    schema: &SchemaRef,
    input_indices: &[usize],
    partials: Vec<RecordBatch>,
    partitions: usize,
    predicate: Option<Arc<dyn PhysicalExpr>>,
) -> DfResult<Arc<dyn ExecutionPlan>> {
    let mut distributed = distribute(partials, partitions);
    // Keep a stable number of plan partitions even when there are fewer batches
    // than `partitions`; empty partitions emit nothing.
    while distributed.len() < partitions {
        distributed.push(Vec::new());
    }
    let source = MemorySourceConfig::try_new(
        &distributed,
        Arc::clone(schema),
        Some(input_indices.to_vec()),
    )?;
    let mut plan: Arc<dyn ExecutionPlan> = DataSourceExec::from_data_source(source);
    if let Some(predicate) = predicate {
        plan = Arc::new(FilterExec::try_new(predicate, plan)?);
    }
    Ok(plan)
}

fn partial_err(error: crate::error::PartialsError) -> DataFusionError {
    DataFusionError::External(Box::new(error))
}

/// How the merged batch becomes the scan's declared output.
#[derive(Debug, Clone)]
enum Projection {
    /// `SELECT *`: the merged batch already carries every traces column.
    All,
    /// Named columns, in projection order.
    Indices(Vec<String>),
    /// `SELECT count(*)`: batches with no columns and an explicit row count.
    Empty,
}

/// Merges one hash partition's partials into traces, or runs the list fast path.
pub(crate) struct TracesExec {
    input: Arc<dyn ExecutionPlan>,
    properties: Arc<PlanProperties>,
    scan_ctx: ScanContext,
    mode: Mode,
    merge_columns: MergeColumns,
    key: Vec<Arc<dyn PhysicalExpr>>,
    /// Filters, against the merged output schema. `None` when there are none.
    predicate: Option<Arc<dyn PhysicalExpr>>,
    /// Raw `[a, b]` the merged `start_time` must lie in.
    bounds: TimeBounds,
    projection: Projection,
    output_schema: SchemaRef,
    limit: Option<usize>,
    display: String,
}

impl fmt::Debug for TracesExec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TracesExec")
            .field("display", &self.display)
            .finish_non_exhaustive()
    }
}

/// If `plan` is a range-mode `TracesExec`, return it rewritten to list mode.
/// Called by `TraceListRule` (D153).
pub(crate) fn rewrite_list(
    plan: &Arc<dyn ExecutionPlan>,
    fetch: usize,
) -> Option<Arc<dyn ExecutionPlan>> {
    let exec = plan.downcast_ref::<TracesExec>()?;
    if exec.mode != Mode::Range {
        return None;
    }
    let mut new = exec.clone();
    new.mode = Mode::List { fetch };
    new.display = exec
        .display
        .replacen("mode=range", &format!("mode=list, fetch={fetch}"), 1);
    // List mode emits one stream; the ancestors are rebuilt by `transform_up`.
    new.properties = Arc::new(properties_for(&new.output_schema, 1, EmissionType::Final));
    Some(Arc::new(new))
}

impl DisplayAs for TracesExec {
    fn fmt_as(&self, _t: DisplayFormatType, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display)
    }
}

impl ExecutionPlan for TracesExec {
    fn name(&self) -> &str {
        "TracesExec"
    }

    fn properties(&self) -> &Arc<PlanProperties> {
        &self.properties
    }

    fn children(&self) -> Vec<&Arc<dyn ExecutionPlan>> {
        vec![&self.input]
    }

    fn apply_expressions(
        &self,
        _f: &mut dyn FnMut(&Arc<dyn PhysicalExpr>) -> datafusion::error::Result<TreeNodeRecursion>,
    ) -> datafusion::error::Result<TreeNodeRecursion> {
        Ok(TreeNodeRecursion::Continue)
    }

    #[allow(deprecated)]
    fn with_new_children(
        self: Arc<Self>,
        children: Vec<Arc<dyn ExecutionPlan>>,
    ) -> datafusion::error::Result<Arc<dyn ExecutionPlan>> {
        let mut this = (*self).clone();
        this.input = children
            .into_iter()
            .next()
            .ok_or_else(|| DataFusionError::Internal("TracesExec needs one child".into()))?;
        // The optimizer may add or remove the hash repartition (for a single
        // input partition it is unnecessary). Keep our output partitioning in
        // step with the child so the number of execute() partitions matches;
        // list mode always stays a single stream.
        let (partitions, emission) = if matches!(this.mode, Mode::List { .. }) {
            (1, EmissionType::Final)
        } else {
            let partitions = this
                .input
                .properties()
                .output_partitioning()
                .partition_count();
            (partitions, EmissionType::Incremental)
        };
        this.properties = Arc::new(properties_for(&this.output_schema, partitions, emission));
        Ok(Arc::new(this))
    }

    fn required_input_ordering(&self) -> Vec<Option<OrderingRequirements>> {
        if !matches!(self.mode, Mode::Range | Mode::Lookup) {
            // List mode never executes its input.
            return vec![None];
        }
        // The key is non-empty: `build_range_plan` sorts by it. `new` returns
        // `None` only for an empty requirement, so one element always gives `Some`.
        let requirement = LexRequirement::new([PhysicalSortRequirement::new(
            Arc::clone(&self.key[0]),
            Some(SortOptions::default()),
        )]);
        vec![requirement.map(OrderingRequirements::new)]
    }

    fn input_distribution_requirements(&self) -> InputDistributionRequirements {
        if self.limit.is_some() {
            // Step 8: a limit uses one output partition. A key-partitioning
            // requirement lets EnforceDistribution repartition back to
            // target_partitions and repeat the whole merge once per partition.
            return InputDistributionRequirements::new(vec![Distribution::SinglePartition]);
        }
        InputDistributionRequirements::new(vec![Distribution::KeyPartitioned(self.key.clone())])
    }

    fn execute(
        &self,
        partition: usize,
        context: Arc<TaskContext>,
    ) -> datafusion::error::Result<SendableRecordBatchStream> {
        if let Mode::List { fetch } = self.mode {
            let ctx = self.scan_ctx.clone();
            let bounds = self.bounds;
            let projection = self.projection.clone();
            let output_schema = Arc::clone(&self.output_schema);
            let batch_size = context.session_config().batch_size();
            let stream = futures::stream::once(async move {
                run_list(
                    ctx,
                    fetch,
                    bounds,
                    projection,
                    output_schema,
                    context,
                    batch_size,
                )
                .await
            })
            .flat_map(|result: DfResult<Vec<RecordBatch>>| {
                let items: Vec<DfResult<RecordBatch>> = match result {
                    Ok(batches) => batches.into_iter().map(Ok).collect(),
                    Err(error) => vec![Err(error)],
                };
                futures::stream::iter(items)
            });
            return Ok(Box::pin(RecordBatchStreamAdapter::new(
                Arc::clone(&self.output_schema),
                stream,
            )));
        }

        let input = self.input.execute(partition, Arc::clone(&context))?;
        let reservation =
            MemoryConsumer::new(format!("TracesExec[{partition}]")).register(context.memory_pool());
        let stream = MergeStream {
            input,
            merger: TraceMerger::new(self.merge_columns.clone()),
            reservation,
            merge_columns: self.merge_columns.clone(),
            predicate: self.predicate.clone(),
            bounds: self.bounds,
            projection: self.projection.clone(),
            output_schema: Arc::clone(&self.output_schema),
            remaining: self.limit,
            outstanding: VecDeque::new(),
            done: false,
        };
        Ok(Box::pin(RecordBatchStreamAdapter::new(
            Arc::clone(&self.output_schema),
            stream,
        )))
    }
}

impl Clone for TracesExec {
    fn clone(&self) -> Self {
        Self {
            input: Arc::clone(&self.input),
            properties: Arc::clone(&self.properties),
            scan_ctx: self.scan_ctx.clone(),
            mode: self.mode,
            merge_columns: self.merge_columns.clone(),
            key: self.key.clone(),
            predicate: self.predicate.clone(),
            bounds: self.bounds,
            projection: self.projection.clone(),
            output_schema: Arc::clone(&self.output_schema),
            limit: self.limit,
            display: self.display.clone(),
        }
    }
}

/// Most input rows merged before the complete traces are emitted (D-A1-13).
/// It bounds the merge's own reservation, which can't spill: about 2 KB per
/// trace, so ~2 MB per partition.
const MERGE_CHUNK_ROWS: usize = 1024;

/// Merges one partition's partials, which arrive sorted by `trace_id`. Every
/// trace whose id is below the last id of the current chunk is complete, so it
/// is emitted, and the merge holds at most one chunk of traces.
struct MergeStream {
    input: SendableRecordBatchStream,
    /// Partials of traces not yet known to be complete.
    merger: TraceMerger,
    /// Holds `merger.size()`.
    reservation: MemoryReservation,
    merge_columns: MergeColumns,
    predicate: Option<Arc<dyn PhysicalExpr>>,
    bounds: TimeBounds,
    projection: Projection,
    output_schema: SchemaRef,
    /// Rows `limit` still allows; `None` without a limit.
    remaining: Option<usize>,
    outstanding: VecDeque<RecordBatch>,
    done: bool,
}

impl MergeStream {
    fn push_batch(&mut self, batch: &RecordBatch) -> DfResult<()> {
        let mut offset = 0;
        while offset < batch.num_rows() && !self.done {
            let length = MERGE_CHUNK_ROWS.min(batch.num_rows() - offset);
            self.push_chunk(&batch.slice(offset, length))?;
            offset += length;
        }
        Ok(())
    }

    fn push_chunk(&mut self, chunk: &RecordBatch) -> DfResult<()> {
        if chunk.num_rows() == 0 {
            return Ok(());
        }
        // The last trace may continue in the next chunk, so its rows wait.
        let cut = last_run_start(chunk)?;
        if cut > 0 {
            self.merge(&chunk.slice(0, cut))?;
            self.emit()?;
            if self.done {
                return Ok(());
            }
        }
        self.merge(&chunk.slice(cut, chunk.num_rows() - cut))
    }

    fn merge(&mut self, partials: &RecordBatch) -> DfResult<()> {
        self.merger.push(partials).map_err(partial_err)?;
        // A refused resize is DataFusion's ResourcesExhausted, which A2 maps to
        // query_too_large. The reservation frees itself on drop.
        self.reservation.try_resize(self.merger.size())
    }

    /// Finishes the traces the merger holds and queues their rows, up to
    /// `remaining` when there is a limit.
    fn emit(&mut self) -> DfResult<()> {
        let merger = std::mem::replace(
            &mut self.merger,
            TraceMerger::new(self.merge_columns.clone()),
        );
        let merged = merger.finish().map_err(partial_err)?;
        self.reservation.free();
        let mask = self.keep_mask(&merged)?;
        let filtered = filter_record_batch(&merged, &mask)?;
        let rows = match self.remaining {
            Some(remaining) => {
                let rows = filtered.num_rows().min(remaining);
                self.remaining = Some(remaining - rows);
                rows
            }
            None => filtered.num_rows(),
        };
        if self.remaining == Some(0) {
            self.done = true;
        }
        if rows > 0 {
            let projected = self.project(&filtered.slice(0, rows))?;
            self.outstanding.push_back(projected);
        }
        Ok(())
    }

    /// Rows whose merged `start_time` is in `[a, b]` and that pass every filter.
    fn keep_mask(&self, merged: &RecordBatch) -> datafusion::error::Result<BooleanArray> {
        let rows = merged.num_rows();
        let mut keep = vec![true; rows];
        if self.bounds.min.is_some() || self.bounds.max.is_some() {
            let start = merged
                .column_by_name(traces::START_TIME)
                .ok_or_else(|| DataFusionError::Internal("merged start_time missing".into()))?
                .as_any()
                .downcast_ref::<TimestampNanosecondArray>()
                .ok_or_else(|| DataFusionError::Internal("merged start_time type".into()))?;
            for (row, slot) in keep.iter_mut().enumerate() {
                let value = start.value(row);
                if self.bounds.min.is_some_and(|min| value < min)
                    || self.bounds.max.is_some_and(|max| value > max)
                {
                    *slot = false;
                }
            }
        }
        if let Some(predicate) = &self.predicate {
            let value = predicate.evaluate(merged)?;
            let array = value.into_array(rows)?;
            let flags = array
                .as_any()
                .downcast_ref::<BooleanArray>()
                .ok_or_else(|| DataFusionError::Internal("filter is not boolean".into()))?;
            for (row, slot) in keep.iter_mut().enumerate() {
                if flags.is_null(row) || !flags.value(row) {
                    *slot = false;
                }
            }
        }
        Ok(BooleanArray::from(keep))
    }

    fn project(&self, filtered: &RecordBatch) -> datafusion::error::Result<RecordBatch> {
        match &self.projection {
            Projection::All => Ok(filtered.clone()),
            Projection::Indices(names) => {
                let columns: Vec<ArrayRef> = names
                    .iter()
                    .map(|name| {
                        filtered
                            .column_by_name(name)
                            .ok_or_else(|| {
                                DataFusionError::Internal(format!("merged column {name} missing"))
                            })
                            .map(Arc::clone)
                    })
                    .collect::<datafusion::error::Result<Vec<_>>>()?;
                Ok(RecordBatch::try_new(
                    Arc::clone(&self.output_schema),
                    columns,
                )?)
            }
            Projection::Empty => Ok(RecordBatch::try_new_with_options(
                Arc::clone(&self.output_schema),
                Vec::new(),
                &RecordBatchOptions::new().with_row_count(Some(filtered.num_rows())),
            )?),
        }
    }
}

/// The first row of the trailing run of equal `trace_id`s in `chunk`, which
/// must be non-empty and sorted by `trace_id`.
fn last_run_start(chunk: &RecordBatch) -> DfResult<usize> {
    let ids = chunk
        .column_by_name(traces::TRACE_ID)
        .ok_or_else(|| DataFusionError::Internal("merge input has no trace_id".into()))?
        .as_any()
        .downcast_ref::<FixedSizeBinaryArray>()
        .ok_or_else(|| DataFusionError::Internal("merge input trace_id type".into()))?;
    let last_row = chunk.num_rows() - 1;
    let last = ids.value(last_row);
    let mut start = last_row;
    while start > 0 && ids.value(start - 1) == last {
        start -= 1;
    }
    Ok(start)
}

impl Stream for MergeStream {
    type Item = datafusion::error::Result<RecordBatch>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            if let Some(batch) = this.outstanding.pop_front() {
                return Poll::Ready(Some(Ok(batch)));
            }
            if this.done {
                return Poll::Ready(None);
            }
            match this.input.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(batch))) => {
                    if let Err(error) = this.push_batch(&batch) {
                        this.done = true;
                        return Poll::Ready(Some(Err(error)));
                    }
                }
                Poll::Ready(Some(Err(error))) => {
                    this.done = true;
                    return Poll::Ready(Some(Err(error)));
                }
                Poll::Ready(None) => {
                    this.done = true;
                    if let Err(error) = this.emit() {
                        return Poll::Ready(Some(Err(error)));
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------- list mode

const MIN15: i64 = 15 * 60 * 1_000_000_000;
const HOUR1: i64 = 60 * 60 * 1_000_000_000;
const HOUR6: i64 = 6 * HOUR1;
const HOUR24: i64 = 24 * HOUR1;

/// Inclusive slices, newest first, intersected with `[a, b]`. Stops when a
/// slice's intersection is empty (it lies entirely before `a`).
fn slices(a: i64, b: i64) -> Vec<TimeBounds> {
    let raw = [
        (b.saturating_sub(MIN15), b),
        (
            b.saturating_sub(HOUR1),
            b.saturating_sub(MIN15).saturating_sub(1),
        ),
        (
            b.saturating_sub(HOUR6),
            b.saturating_sub(HOUR1).saturating_sub(1),
        ),
        (
            b.saturating_sub(HOUR24),
            b.saturating_sub(HOUR6).saturating_sub(1),
        ),
        (a, b.saturating_sub(HOUR24).saturating_sub(1)),
    ];
    let mut out = Vec::with_capacity(raw.len());
    for (lo, hi) in raw {
        let lo = lo.max(a);
        let hi = hi.min(b);
        if lo > hi {
            break;
        }
        out.push(TimeBounds {
            min: Some(lo),
            max: Some(hi),
        });
    }
    out
}

/// All `traces` files and memtable batches, used to bound an open-ended list.
fn all_sources(ctx: &ScanContext) -> (Vec<Arc<FileMeta>>, Vec<MemBatch>) {
    let open = TimeBounds {
        min: None,
        max: None,
    };
    let files: Vec<Arc<FileMeta>> = ctx
        .catalog
        .files(
            FileTable::Traces,
            ctx.project,
            file_day_range(&open, FileTable::Traces),
        )
        .cloned()
        .collect();
    let batches = mem_batches(&ctx.spans_mem, ctx.project, &open);
    (files, batches)
}

/// Replace open ends with data bounds. `None` means there is no data at all.
fn data_bounds(ctx: &ScanContext, bounds: TimeBounds) -> Option<(i64, i64)> {
    let (files, batches) = all_sources(ctx);
    if files.is_empty() && batches.is_empty() {
        return None;
    }
    let min = files
        .iter()
        .map(|f| f.min_time)
        .chain(batches.iter().map(|b| b.min_time))
        .min()?;
    let max = files
        .iter()
        .map(|f| f.max_time)
        .chain(batches.iter().map(|b| b.max_time))
        .max()?;
    Some((bounds.min.unwrap_or(min), bounds.max.unwrap_or(max)))
}

struct Candidate {
    start: i64,
    id: TraceId,
    slice: usize,
}

fn newest_first(left: &Candidate, right: &Candidate) -> Ordering {
    right
        .start
        .cmp(&left.start)
        .then_with(|| right.id.as_bytes().cmp(left.id.as_bytes()))
}

/// Phase 1: rank `(start_time, trace_id)` over slices. Phase 2: fetch the kept
/// ids with full columns. Returns the projected rows, newest first, at most k.
async fn run_list(
    ctx: ScanContext,
    fetch: usize,
    bounds: TimeBounds,
    projection: Projection,
    output_schema: SchemaRef,
    context: Arc<TaskContext>,
    batch_size: usize,
) -> DfResult<Vec<RecordBatch>> {
    let schema = traces_schema();
    let filters = filter_names(&ctx.filters);
    let phase1_cols = MergeColumns::new(&refs(&filters));
    let mut full = projection_names(&projection, &schema);
    full.extend(filters.iter().cloned());
    let phase2_cols = MergeColumns::new(&refs(&full));

    let Some((a, b)) = data_bounds(&ctx, bounds) else {
        return Ok(Vec::new());
    };
    if a > b {
        return Ok(Vec::new());
    }
    let windows = slices(a, b);

    // Phase 1, rank.
    let phase1_schema: SchemaRef = Arc::new(project_schema(
        &schema,
        &indices_of(&schema, &phase1_cols.output())?,
    ));
    let mut seen: BTreeSet<TraceId> = BTreeSet::new();
    let mut candidates: Vec<Candidate> = Vec::new();
    for (index, window) in windows.iter().copied().enumerate() {
        let plan = build_range_plan(
            &ctx,
            RangeSpec {
                bounds: window,
                mode: Mode::Range,
                merge_columns: phase1_cols.clone(),
                scan_ids: None,
                projection: Projection::All,
                output_schema: Arc::clone(&phase1_schema),
                limit: None,
                rank: Some(fetch),
            },
        )?;
        for batch in collect_plan(plan, Arc::clone(&context)).await? {
            let start = batch
                .column_by_name(traces::START_TIME)
                .ok_or_else(|| DataFusionError::Internal("phase 1 start_time missing".into()))?
                .as_any()
                .downcast_ref::<TimestampNanosecondArray>()
                .ok_or_else(|| DataFusionError::Internal("phase 1 start_time type".into()))?;
            let ids = batch
                .column_by_name(traces::TRACE_ID)
                .ok_or_else(|| DataFusionError::Internal("phase 1 trace_id missing".into()))?
                .as_any()
                .downcast_ref::<FixedSizeBinaryArray>()
                .ok_or_else(|| DataFusionError::Internal("phase 1 trace_id type".into()))?;
            for row in 0..batch.num_rows() {
                let id = TraceId::from_slice(ids.value(row)).map_err(|_| {
                    DataFusionError::External(Box::new(crate::error::PartialsError::InvalidTraceId))
                })?;
                if seen.insert(id) {
                    candidates.push(Candidate {
                        start: start.value(row),
                        id,
                        slice: index,
                    });
                }
            }
        }
        // Keep only the best k so far, and rebuild `seen` from them: both
        // collections stay bounded by k even when a slice is very selective.
        // A trace dropped here sorts below the current top-k, so re-seeing it
        // in an older slice cannot change the answer.
        candidates.sort_by(newest_first);
        candidates.truncate(fetch);
        seen = candidates.iter().map(|candidate| candidate.id).collect();
        // A slice with no matching trace is not empty; only an exhausted
        // intersection stops the walk. Stop once the total reaches k.
        if candidates.len() >= fetch {
            break;
        }
    }
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    // Phase 2, fetch, grouped by the slice each id was ranked in.
    let mut groups: BTreeMap<usize, Vec<TraceId>> = BTreeMap::new();
    for candidate in &candidates {
        groups
            .entry(candidate.slice)
            .or_default()
            .push(candidate.id);
    }
    let phase2_schema: SchemaRef = Arc::new(project_schema(
        &schema,
        &indices_of(&schema, &phase2_cols.output())?,
    ));
    let mut fetched: Vec<RecordBatch> = Vec::new();
    for (index, ids) in groups {
        let wanted = ids.len();
        let plan = build_range_plan(
            &ctx,
            RangeSpec {
                bounds: windows[index],
                mode: Mode::Range,
                merge_columns: phase2_cols.clone(),
                scan_ids: Some(ids.into_iter().collect()),
                projection: Projection::All,
                output_schema: Arc::clone(&phase2_schema),
                limit: None,
                rank: None,
            },
        )?;
        let batches = collect_plan(plan, Arc::clone(&context)).await?;
        let rows: usize = batches.iter().map(RecordBatch::num_rows).sum();
        // Re-applying the filters held in phase 1; the inner plan uses the
        // same slice window, so exactly the ranked ids come back. A missing
        // row means phase 1 and phase 2 disagreed on the merged values.
        debug_assert_eq!(rows, wanted, "phase 2 lost or duplicated ranked rows");
        fetched.extend(batches);
    }
    if fetched.is_empty() {
        return Ok(Vec::new());
    }

    // Sort by (start_time DESC, trace_id DESC), take k, project and batch.
    let merged = concat_batches(&phase2_schema, fetched.iter())?;
    let start = Arc::clone(
        merged
            .column_by_name(traces::START_TIME)
            .ok_or_else(|| DataFusionError::Internal("merged start_time missing".into()))?,
    );
    let trace = Arc::clone(
        merged
            .column_by_name(traces::TRACE_ID)
            .ok_or_else(|| DataFusionError::Internal("merged trace_id missing".into()))?,
    );
    let descending = Some(SortOptions {
        descending: true,
        nulls_first: false,
    });
    let indices = lexsort_to_indices(
        &[
            SortColumn {
                values: start,
                options: descending,
            },
            SortColumn {
                values: trace,
                options: descending,
            },
        ],
        Some(fetch),
    )?;
    let sorted = take_record_batch(&merged, &indices)?;
    let final_batch = project_final(&sorted, &projection, &output_schema)?;
    let total = final_batch.num_rows();
    let mut out = Vec::new();
    let mut offset = 0;
    while offset < total {
        let length = batch_size.min(total - offset);
        out.push(final_batch.slice(offset, length));
        offset += length;
    }
    Ok(out)
}

fn refs(names: &[String]) -> Vec<&str> {
    names.iter().map(String::as_str).collect()
}

fn project_final(
    sorted: &RecordBatch,
    projection: &Projection,
    output_schema: &SchemaRef,
) -> DfResult<RecordBatch> {
    match projection {
        Projection::All => Ok(sorted.clone()),
        Projection::Indices(names) => {
            let columns: Vec<ArrayRef> = names
                .iter()
                .map(|name| {
                    sorted
                        .column_by_name(name)
                        .ok_or_else(|| {
                            DataFusionError::Internal(format!("merged column {name} missing"))
                        })
                        .map(Arc::clone)
                })
                .collect::<DfResult<Vec<_>>>()?;
            Ok(RecordBatch::try_new(Arc::clone(output_schema), columns)?)
        }
        Projection::Empty => Ok(RecordBatch::try_new_with_options(
            Arc::clone(output_schema),
            Vec::new(),
            &RecordBatchOptions::new().with_row_count(Some(sorted.num_rows())),
        )?),
    }
}

#[cfg(test)]
mod tests {
    use bytesize::ByteSize;
    use std::collections::BTreeMap;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use arrow::array::{Array, ArrayRef, AsArray, Int64Array, RecordBatch, StringArray};
    use arrow::compute::{SortColumn, concat_batches, lexsort_to_indices, take_record_batch};
    use arrow::datatypes::{Field, Float64Type, Int32Type, Schema, SchemaRef};
    use datafusion::catalog::TableProvider;
    use datafusion::common::ScalarValue;
    use datafusion::execution::context::{SessionConfig, SessionContext};
    use datafusion::logical_expr::{BinaryExpr, Expr, Operator, col};
    use datafusion::physical_plan::execution_plan::collect as collect_plan;
    use llmobs_core::batch::spans_to_record_batch;
    use llmobs_core::enums::{SpanKind, SpanStatus};
    use llmobs_core::ids::{ProjectId, SpanId, TraceId};
    use llmobs_core::schema::{traces, traces_schema};
    use llmobs_core::span::Span;

    use crate::ProjectQuery;
    use crate::QueryOptions;
    use crate::config::{
        FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig, WalConfig,
    };
    use crate::hooks::{HookPoint, Hooks};
    use crate::test_support::{dataset_spans, example_query, trace_batch};
    use crate::views::QueryContext;
    use crate::wal::spans_at;
    use crate::{
        ListScope, Page, SortOrder, TraceDetailData, TraceListRequest, TraceSort, trace_detail,
        trace_list,
    };

    const TIMEOUT: Duration = Duration::from_secs(30);
    const DAY: i64 = 86_400_000_000_000;
    const HOUR: i64 = 3_600_000_000_000;
    const MIN: i64 = 60_000_000_000;
    const SEC: i64 = 1_000_000_000;

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    /// The project id carried by `fixtures/api/dataset/spans.json`.
    fn dataset_project() -> ProjectId {
        "01900000-0000-7000-8000-000000000001".parse().unwrap()
    }

    fn config_in(dir: &Path) -> StorageConfig {
        StorageConfig {
            data_dir: dir.to_path_buf(),
            min_free_bytes: ByteSize::b(0),
            object_store: None,
            s3: S3Config::default(),
            wal: WalConfig::default(),
            flush: FlushConfig::default(),
            memtable: MemtableConfig::default(),
            query: QueryConfig {
                target_partitions: 2,
                ..QueryConfig::default()
            },
            anchor: Default::default(),
            gc: Default::default(),
            compaction: Default::default(),
            deletion: Default::default(),
        }
    }

    fn aged(dir: &Path) -> StorageConfig {
        let mut config = config_in(dir);
        config.flush.max_age = Duration::from_millis(100);
        config
    }

    fn session(query: &ProjectQuery) -> SessionContext {
        SessionContext::new_with_config_rt(
            SessionConfig::new().with_target_partitions(query.target_partitions.get()),
            Arc::clone(&query.runtime),
        )
    }

    async fn collect(ctx: &SessionContext, sql: &str) -> Vec<RecordBatch> {
        ctx.sql(sql).await.unwrap().collect().await.unwrap()
    }

    fn count_of(batches: &[RecordBatch]) -> i64 {
        batches
            .iter()
            .map(|batch| {
                batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .unwrap()
                    .value(0)
            })
            .sum()
    }

    /// A flush pause blocks a Tokio worker; run the query on its own runtime.
    async fn collect_off(query: &ProjectQuery, sql: &str) -> Vec<RecordBatch> {
        let runtime = Arc::clone(&query.runtime);
        let provider = Arc::clone(&query.traces);
        let sql = sql.to_owned();
        tokio::task::spawn_blocking(move || {
            let local = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            local.block_on(async move {
                let ctx = SessionContext::new_with_config_rt(
                    SessionConfig::new().with_target_partitions(2),
                    runtime,
                );
                ctx.register_table("traces", provider).unwrap();
                ctx.sql(&sql).await.unwrap().collect().await.unwrap()
            })
        })
        .await
        .unwrap()
    }

    /// Shutdown drives one flush of the active memtable; reopen reads the files.
    async fn reopen(dir: &Path, storage: crate::Storage) -> crate::Storage {
        storage.shutdown().await.unwrap();
        drop(storage);
        crate::Storage::open(config_in(dir)).await.unwrap()
    }

    fn plan_text(batches: &[RecordBatch]) -> String {
        let mut text = String::new();
        for batch in batches {
            for column in batch.columns() {
                if let Some(values) = column.as_any().downcast_ref::<StringArray>() {
                    for index in 0..values.len() {
                        text.push_str(values.value(index));
                        text.push('\n');
                    }
                }
            }
        }
        text
    }

    fn trace_id(last: u8) -> TraceId {
        let mut bytes = [0_u8; 16];
        bytes[15] = last;
        TraceId::from_bytes(bytes).unwrap()
    }

    fn span_id(last: u8) -> SpanId {
        let mut bytes = [0_u8; 8];
        bytes[7] = last;
        SpanId::from_bytes(bytes).unwrap()
    }

    fn base_span(trace: TraceId, span: SpanId, start: i64) -> Span {
        Span {
            project_id: project(),
            trace_id: trace,
            span_id: span,
            parent_span_id: None,
            name: format!("span-{start}"),
            span_kind: SpanKind::Span,
            start_time: start,
            end_time: start + 10,
            status: SpanStatus::Unset,
            status_message: None,
            service_name: None,
            session_id: None,
            user_id: None,
            environment: None,
            release: None,
            tags: Vec::new(),
            metadata: BTreeMap::new(),
            provider: None,
            request_model: None,
            response_model: None,
            operation: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            reasoning_tokens: None,
            total_tokens: None,
            cost_input: None,
            cost_output: None,
            cost_total: None,
            cost_source: None,
            ttft_ns: None,
            request_params: None,
            input: None,
            input_format: None,
            output: None,
            output_format: None,
            input_preview: None,
            output_preview: None,
            system_instructions: None,
            tool_definitions: None,
            attributes: BTreeMap::new(),
            resource_attributes: BTreeMap::new(),
            scope_name: None,
            scope_version: None,
            events: Vec::new(),
            links: Vec::new(),
            media_refs: Vec::new(),
            truncated: Vec::new(),
            conventions: Vec::new(),
            ingest_time: start + 1,
            schema_version: 1,
        }
    }

    fn spans_batch(spans: &[Span]) -> RecordBatch {
        spans_to_record_batch(spans).unwrap()
    }

    fn next(state: &mut u64) -> usize {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (*state >> 33) as usize
    }

    /// Concatenates batches with a known schema (empty input stays empty).
    fn concat(batches: &[RecordBatch], schema: SchemaRef) -> RecordBatch {
        if batches.is_empty() {
            return RecordBatch::new_empty(schema);
        }
        concat_batches(&schema, batches.iter()).unwrap()
    }

    fn sort_by_trace_id(batch: &RecordBatch) -> RecordBatch {
        let column = Arc::clone(batch.column_by_name(traces::TRACE_ID).unwrap());
        let indices = lexsort_to_indices(
            &[SortColumn {
                values: column,
                options: None,
            }],
            None,
        )
        .unwrap();
        take_record_batch(batch, &indices).unwrap()
    }

    fn drop_column(batch: &RecordBatch, name: &str) -> RecordBatch {
        let fields: Vec<Field> = batch
            .schema()
            .fields()
            .iter()
            .filter(|field| field.name() != name)
            .map(|field| field.as_ref().clone())
            .collect();
        let columns: Vec<ArrayRef> = fields
            .iter()
            .map(|field| Arc::clone(batch.column_by_name(field.name()).unwrap()))
            .collect();
        RecordBatch::try_new(Arc::new(Schema::new(fields)), columns).unwrap()
    }

    /// Compares every column exactly, except `cost_total` within a relative 1e-9.
    fn assert_batches_equal(actual: &RecordBatch, expected: &RecordBatch) {
        assert_eq!(actual.schema(), expected.schema(), "schema");
        assert_eq!(actual.num_rows(), expected.num_rows(), "rows");
        for field in actual.schema().fields() {
            let name = field.name();
            let left = actual.column_by_name(name).unwrap();
            let right = expected.column_by_name(name).unwrap();
            if name == traces::COST_TOTAL {
                let l = left.as_primitive::<Float64Type>();
                let r = right.as_primitive::<Float64Type>();
                for row in 0..l.len() {
                    assert_eq!(l.is_null(row), r.is_null(row), "cost null {row}");
                    if !l.is_null(row) {
                        let (a, b) = (l.value(row), r.value(row));
                        let tolerance = 1e-9 * a.abs().max(b.abs()).max(1.0);
                        assert!((a - b).abs() <= tolerance, "cost {a} vs {b}");
                    }
                }
            } else {
                assert_eq!(left, right, "column {name}");
            }
        }
    }

    fn ts_literal(value: i64) -> Expr {
        Expr::Literal(
            ScalarValue::TimestampNanosecond(Some(value), Some(Arc::from("UTC"))),
            None,
        )
    }

    fn start_ge(value: i64) -> Expr {
        Expr::BinaryExpr(BinaryExpr::new(
            Box::new(col(traces::START_TIME)),
            Operator::GtEq,
            Box::new(ts_literal(value)),
        ))
    }

    fn start_lt(value: i64) -> Expr {
        Expr::BinaryExpr(BinaryExpr::new(
            Box::new(col(traces::START_TIME)),
            Operator::Lt,
            Box::new(ts_literal(value)),
        ))
    }

    /// Plans the raw provider directly with `Expr` filters, so the timestamp
    /// literals keep the exact `TimestampNanosecond` type `time_bounds` reads.
    async fn scan_with(
        ctx: &SessionContext,
        provider: &Arc<dyn TableProvider>,
        projection: Option<Vec<usize>>,
        filters: Vec<Expr>,
        limit: Option<usize>,
    ) -> Vec<RecordBatch> {
        let state = ctx.state();
        let plan = provider
            .scan(&state, projection.as_ref(), &filters, limit)
            .await
            .unwrap();
        collect_plan(plan, ctx.task_ctx()).await.unwrap()
    }

    fn window(from: i64, to: i64) -> Vec<Expr> {
        vec![start_ge(from), start_lt(to)]
    }

    fn partial_counts(batch: &RecordBatch) -> BTreeMap<Vec<u8>, i32> {
        let ids = batch
            .column_by_name(traces::TRACE_ID)
            .unwrap()
            .as_fixed_size_binary();
        let values = batch
            .column_by_name(traces::PARTIAL_COUNT)
            .unwrap()
            .as_primitive::<Int32Type>();
        (0..batch.num_rows())
            .map(|row| (ids.value(row).to_vec(), values.value(row)))
            .collect()
    }

    fn int64_column(batch: &RecordBatch, name: &str) -> Vec<i64> {
        batch
            .column_by_name(name)
            .unwrap()
            .as_primitive::<arrow::datatypes::Int64Type>()
            .values()
            .to_vec()
    }

    // ---------------------------------------------------------------- tests

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_flushed_trace_merges_from_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        storage
            .append_spans(project(), spans_at(&[1, 2]))
            .await
            .unwrap();
        let storage = reopen(dir.path(), storage).await;

        let query = storage.query(project(), Default::default()).await.unwrap();
        let ctx = session(&query);
        ctx.register_table("traces", Arc::clone(&query.traces))
            .unwrap();

        assert_eq!(
            count_of(&collect(&ctx, "SELECT count(*) FROM traces").await),
            1
        );
        let all = collect(&ctx, "SELECT * FROM traces").await;
        assert_eq!(all.iter().map(RecordBatch::num_rows).sum::<usize>(), 1);
        assert_eq!(int64_column(&all[0], traces::SPAN_COUNT), vec![2]);

        let explained = collect(&ctx, "EXPLAIN SELECT * FROM traces").await;
        let text = plan_text(&explained);
        assert!(text.contains("TracesExec: mode=range"), "{text}");
        assert!(text.contains("columns=["), "{text}");
        storage.shutdown().await.unwrap();
    }

    /// Group 1 (Oracle): the A2 dataset appended in random-size batches, three
    /// shutdown-driven flushes, the last part left in the memtable as one batch.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn oracle_full_scan_matches_the_fixture() {
        oracle_full_scan(None).await;
    }

    /// The Oracle with two-row batches: the sort's output batches end in the
    /// middle of most runs of partials.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn oracle_full_scan_with_tiny_batches_matches_the_fixture() {
        oracle_full_scan(Some(2)).await;
    }

    /// The Oracle scan; `batch_size` overrides the session's batch size.
    async fn oracle_full_scan(batch_size: Option<usize>) {
        let spans = dataset_spans();
        let mut rng: u64 = 0x2545_F491_4F6C_DD1D;
        let n = spans.len();
        let mut cuts = Vec::new();
        while cuts.len() < 3 {
            let cut = 1 + next(&mut rng) % (n - 1);
            if !cuts.contains(&cut) {
                cuts.push(cut);
            }
        }
        cuts.sort_unstable();
        let mut bounds = vec![0];
        bounds.extend(cuts.iter().copied());
        bounds.push(n);
        let groups: Vec<Vec<Span>> = bounds
            .windows(2)
            .map(|w| spans[w[0]..w[1]].to_vec())
            .collect();
        assert_eq!(groups.len(), 4);

        // One partial per group that holds the trace (a flush group is deduped
        // into one partial; the final memtable group is one batch).
        let mut expected_partial: BTreeMap<Vec<u8>, i32> = BTreeMap::new();
        for group in &groups {
            let mut seen = BTreeMap::new();
            for span in group {
                *seen.entry(span.trace_id.as_bytes().to_vec()).or_insert(0) += 1;
            }
            for (id, _) in seen {
                *expected_partial.entry(id).or_insert(0) += 1;
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let mut storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        for (index, group) in groups.iter().enumerate() {
            let batches: Vec<Vec<Span>> = if index == groups.len() - 1 {
                vec![group.clone()]
            } else {
                let mut out = Vec::new();
                let mut at = 0;
                while at < group.len() {
                    let take = (1 + next(&mut rng) % 3).min(group.len() - at);
                    out.push(group[at..at + take].to_vec());
                    at += take;
                }
                out
            };
            for batch in batches {
                storage
                    .append_spans(dataset_project(), spans_batch(&batch))
                    .await
                    .unwrap();
            }
            if index < groups.len() - 1 {
                storage = reopen(dir.path(), storage).await;
            }
        }

        let query = storage
            .query(dataset_project(), Default::default())
            .await
            .unwrap();
        let ctx = match batch_size {
            Some(size) => SessionContext::new_with_config_rt(
                SessionConfig::new()
                    .with_target_partitions(query.target_partitions.get())
                    .with_batch_size(size),
                Arc::clone(&query.runtime),
            ),
            None => session(&query),
        };
        ctx.register_table("traces", Arc::clone(&query.traces))
            .unwrap();
        let batches = collect(&ctx, "SELECT * FROM traces").await;
        let actual = sort_by_trace_id(&concat(&batches, traces_schema()));
        let expected = sort_by_trace_id(&trace_batch(&spans));

        assert_eq!(partial_counts(&actual), expected_partial, "partial_count");
        assert_batches_equal(
            &drop_column(&actual, traces::PARTIAL_COUNT),
            &drop_column(&expected, traces::PARTIAL_COUNT),
        );
        storage.shutdown().await.unwrap();
    }

    /// Group 2 (Late partials): one trace in three flushes plus the memtable.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn late_partials_from_three_flushes_and_the_memtable_merge() {
        let trace = trace_id(1);
        let d = 20_000 * DAY;
        let dir = tempfile::tempdir().unwrap();
        let mut storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        for group in 0..3 {
            let mut span = base_span(trace, span_id(group + 1), d + i64::from(group) * MIN);
            span.cost_total = Some(0.5);
            if group > 0 {
                span.parent_span_id = Some(span_id(1));
            }
            storage
                .append_spans(project(), spans_batch(&[span]))
                .await
                .unwrap();
            storage = reopen(dir.path(), storage).await;
        }
        let mut span = base_span(trace, span_id(4), d + 3 * MIN);
        span.cost_total = Some(0.5);
        span.parent_span_id = Some(span_id(1));
        storage
            .append_spans(project(), spans_batch(&[span]))
            .await
            .unwrap();

        let query = storage.query(project(), Default::default()).await.unwrap();
        let ctx = session(&query);
        ctx.register_table("traces", Arc::clone(&query.traces))
            .unwrap();
        let batches = collect(&ctx, "SELECT * FROM traces").await;
        let all = concat(&batches, traces_schema());
        assert_eq!(all.num_rows(), 1);
        assert_eq!(int64_column(&all, traces::SPAN_COUNT), vec![4]);
        assert_eq!(partial_counts(&all)[&trace.as_bytes().to_vec()], 4);
        let cost = all
            .column_by_name(traces::COST_TOTAL)
            .unwrap()
            .as_primitive::<Float64Type>();
        assert!((cost.value(0) - 2.0).abs() < 1e-9, "cost {}", cost.value(0));
        storage.shutdown().await.unwrap();
    }

    /// Group 4 (Duplicates): the same span in two flushes is counted twice.
    /// This is the documented limit in spec 03 §6 until compaction merges files.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn duplicates_across_flushes_are_counted_twice() {
        let span = base_span(trace_id(1), span_id(1), 20_000 * DAY + 100);
        let dir = tempfile::tempdir().unwrap();
        let mut storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        for _ in 0..2 {
            storage
                .append_spans(project(), spans_batch(std::slice::from_ref(&span)))
                .await
                .unwrap();
            storage = reopen(dir.path(), storage).await;
        }
        let query = storage.query(project(), Default::default()).await.unwrap();
        let ctx = session(&query);
        ctx.register_table("traces", Arc::clone(&query.traces))
            .unwrap();
        let batches = collect(&ctx, "SELECT * FROM traces").await;
        let all = concat(&batches, traces_schema());
        assert_eq!(all.num_rows(), 1);
        assert_eq!(int64_column(&all, traces::SPAN_COUNT), vec![2]);
        storage.shutdown().await.unwrap();
    }

    /// Group 7 (Projection): select a subset, EXPLAIN columns, empty projection.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn projection_and_count_star() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        let mut cheap = base_span(trace_id(1), span_id(1), 20_000 * DAY + 100);
        cheap.cost_total = Some(0.25);
        let mut dear = base_span(trace_id(2), span_id(1), 20_000 * DAY + 200);
        dear.cost_total = Some(2.0);
        storage
            .append_spans(project(), spans_batch(&[cheap, dear]))
            .await
            .unwrap();
        let storage = reopen(dir.path(), storage).await;

        let query = storage.query(project(), Default::default()).await.unwrap();
        let ctx = session(&query);
        ctx.register_table("traces", Arc::clone(&query.traces))
            .unwrap();

        let subset_batches = collect(&ctx, "SELECT trace_id, cost_total FROM traces").await;
        let subset = concat(&subset_batches, subset_batches[0].schema());
        assert_eq!(subset.num_rows(), 2);
        assert_eq!(subset.num_columns(), 2);

        let explained = collect(&ctx, "EXPLAIN SELECT trace_id, cost_total FROM traces").await;
        let text = plan_text(&explained);
        let line = text
            .lines()
            .find(|line| line.contains("TracesExec:"))
            .unwrap();
        assert!(line.contains("mode=range"), "{line}");
        assert!(!line.contains(traces::INPUT_PREVIEW), "{line}");

        assert_eq!(
            count_of(&collect(&ctx, "SELECT count(*) FROM traces WHERE cost_total > 1.0").await),
            1
        );
        assert_eq!(
            count_of(&collect(&ctx, "SELECT count(*) FROM traces").await),
            2
        );
        storage.shutdown().await.unwrap();
    }

    /// Group 8 (Lookups): id equality with and without `around`, unknown id, IN.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn lookups_with_and_without_around() {
        let t1 = trace_id(1);
        let t2 = trace_id(2);
        let d = 20_000 * DAY;
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        storage
            .append_spans(
                project(),
                spans_batch(&[
                    base_span(t1, span_id(1), d + 100),
                    base_span(t2, span_id(1), d + 200),
                ]),
            )
            .await
            .unwrap();
        let storage = reopen(dir.path(), storage).await;

        let eq = |id: TraceId| {
            format!(
                "trace_id = arrow_cast(X'{}', 'FixedSizeBinary(16)')",
                hex::encode(id.as_bytes())
            )
        };

        for options in [
            QueryOptions::default(),
            QueryOptions {
                around: Some(d + 100),
            },
        ] {
            let query = storage.query(project(), options).await.unwrap();
            let ctx = session(&query);
            ctx.register_table("traces", Arc::clone(&query.traces))
                .unwrap();
            let sql = format!("SELECT * FROM traces WHERE {}", eq(t1));
            let batches = collect(&ctx, &sql).await;
            assert_eq!(batches.iter().map(RecordBatch::num_rows).sum::<usize>(), 1);
        }

        let query = storage.query(project(), Default::default()).await.unwrap();
        let ctx = session(&query);
        ctx.register_table("traces", Arc::clone(&query.traces))
            .unwrap();
        let unknown = format!(
            "SELECT * FROM traces WHERE trace_id = arrow_cast(X'{}', 'FixedSizeBinary(16)')",
            hex::encode(trace_id(99).as_bytes())
        );
        assert_eq!(
            collect(&ctx, &unknown)
                .await
                .iter()
                .map(RecordBatch::num_rows)
                .sum::<usize>(),
            0
        );
        let both = format!(
            "SELECT * FROM traces WHERE trace_id IN (arrow_cast(X'{}', 'FixedSizeBinary(16)'), arrow_cast(X'{}', 'FixedSizeBinary(16)'))",
            hex::encode(t1.as_bytes()),
            hex::encode(t2.as_bytes()),
        );
        assert_eq!(
            collect(&ctx, &both)
                .await
                .iter()
                .map(RecordBatch::num_rows)
                .sum::<usize>(),
            2
        );

        let explained = collect(
            &ctx,
            &format!("EXPLAIN SELECT * FROM traces WHERE {}", eq(t1)),
        )
        .await;
        let line = plan_text(&explained)
            .lines()
            .find(|line| line.contains("TracesExec:"))
            .unwrap()
            .to_owned();
        assert!(line.contains("mode=lookup"), "{line}");
        storage.shutdown().await.unwrap();
    }

    fn rows(batches: &[RecordBatch]) -> usize {
        batches.iter().map(RecordBatch::num_rows).sum()
    }

    fn timestamp_column(batch: &RecordBatch, name: &str) -> Vec<i64> {
        batch
            .column_by_name(name)
            .unwrap()
            .as_primitive::<arrow::datatypes::TimestampNanosecondType>()
            .values()
            .to_vec()
    }

    struct Release {
        hooks: Hooks,
        point: HookPoint,
    }

    impl Drop for Release {
        fn drop(&mut self) {
            self.hooks.release(self.point);
        }
    }

    async fn wait_held(storage: &crate::Storage, point: HookPoint) {
        let started = Instant::now();
        while storage.hooks().waiting_at(point) == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "flush did not reach {point:?}"
            );
            tokio::task::yield_now().await;
        }
    }

    /// Root partial flushed to a file; late partial left in the memtable.
    async fn window_storage(dir: &Path, root: i64, late_offset: i64) -> crate::Storage {
        let root_span = base_span(trace_id(1), span_id(1), root);
        let mut late_span = base_span(trace_id(1), span_id(2), root + late_offset);
        late_span.parent_span_id = Some(span_id(1));
        let storage = crate::Storage::open(config_in(dir)).await.unwrap();
        storage
            .append_spans(project(), spans_batch(&[root_span]))
            .await
            .unwrap();
        let storage = reopen(dir, storage).await;
        storage
            .append_spans(project(), spans_batch(&[late_span]))
            .await
            .unwrap();
        storage
    }

    fn assert_pages_equal(a: &Page, b: &Page) {
        assert_eq!(a.next_cursor, b.next_cursor);
        let schema = a
            .batches
            .first()
            .or_else(|| b.batches.first())
            .map(RecordBatch::schema)
            .unwrap_or_else(|| Arc::new(Schema::empty()));
        assert_batches_equal(
            &concat(&a.batches, Arc::clone(&schema)),
            &concat(&b.batches, schema),
        );
    }

    fn assert_batch_vecs_equal(a: &[RecordBatch], b: &[RecordBatch]) {
        let schema = a
            .first()
            .or_else(|| b.first())
            .map(RecordBatch::schema)
            .unwrap_or_else(|| Arc::new(Schema::empty()));
        assert_batches_equal(&concat(a, Arc::clone(&schema)), &concat(b, schema));
    }

    // ---------------------------------------------------------------- more tests

    /// Group 3 (Midnight): a trace from 23:59:30 to 00:00:30 is placed by its
    /// merged start_time.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_trace_crossing_midnight_uses_its_merged_start() {
        let d = 20_000 * DAY;
        let root = d + 23 * HOUR + 59 * MIN + 30 * SEC;
        let late = d + DAY + 30 * SEC;
        let root_span = base_span(trace_id(1), span_id(1), root);
        let mut late_span = base_span(trace_id(1), span_id(2), late);
        late_span.parent_span_id = Some(span_id(1));
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        storage
            .append_spans(project(), spans_batch(&[root_span, late_span]))
            .await
            .unwrap();
        let storage = reopen(dir.path(), storage).await;

        let query = storage.query(project(), Default::default()).await.unwrap();
        let ctx = session(&query);
        let in_window = scan_with(
            &ctx,
            &query.traces,
            None,
            window(d + 23 * HOUR, d + DAY),
            None,
        )
        .await;
        let all = concat(&in_window, traces_schema());
        assert_eq!(all.num_rows(), 1);
        assert_eq!(
            timestamp_column(&all, traces::START_TIME),
            vec![root],
            "merged start"
        );

        let later = scan_with(
            &ctx,
            &query.traces,
            None,
            window(d + DAY, d + DAY + HOUR),
            None,
        )
        .await;
        assert_eq!(rows(&later), 0, "merged start is before the later window");
        storage.shutdown().await.unwrap();
    }

    /// Group 5 (Filters that pass only after the merge).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn filters_that_only_pass_after_the_merge() {
        let d = 20_000 * DAY + 10 * HOUR;
        let [t1, t2, t3] = [trace_id(1), trace_id(2), trace_id(3)];
        let mut r1 = base_span(t1, span_id(1), d + 100);
        r1.cost_total = Some(0.6);
        let mut r2 = base_span(t2, span_id(1), d + 200);
        r2.session_id = Some("s".to_owned());
        let r3 = base_span(t3, span_id(1), d + 300); // environment is null
        let mut c1 = base_span(t1, span_id(2), d + 110);
        c1.cost_total = Some(0.6);
        c1.parent_span_id = Some(span_id(1));
        let mut c2 = base_span(t2, span_id(2), d + 210);
        c2.parent_span_id = Some(span_id(1));
        let mut c3 = base_span(t3, span_id(2), d + 310);
        c3.environment = Some("prod".to_owned());
        c3.parent_span_id = Some(span_id(1));

        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        storage
            .append_spans(project(), spans_batch(&[r1, r2, r3]))
            .await
            .unwrap();
        let storage = reopen(dir.path(), storage).await;
        storage
            .append_spans(project(), spans_batch(&[c1, c2, c3]))
            .await
            .unwrap();

        let query = storage.query(project(), Default::default()).await.unwrap();
        let ctx = session(&query);
        ctx.register_table("traces", Arc::clone(&query.traces))
            .unwrap();

        let by_cost = concat(
            &collect(&ctx, "SELECT * FROM traces WHERE cost_total > 1.0").await,
            traces_schema(),
        );
        assert_eq!(by_cost.num_rows(), 1);
        let cost = by_cost
            .column_by_name(traces::COST_TOTAL)
            .unwrap()
            .as_primitive::<Float64Type>();
        assert!((cost.value(0) - 1.2).abs() < 1e-9, "{}", cost.value(0));

        let by_session = concat(
            &collect(&ctx, "SELECT * FROM traces WHERE session_id = 's'").await,
            traces_schema(),
        );
        assert_eq!(by_session.num_rows(), 1);
        assert_eq!(int64_column(&by_session, traces::SPAN_COUNT), vec![2]);

        let by_env = collect(&ctx, "SELECT * FROM traces WHERE environment = 'prod'").await;
        assert_eq!(rows(&by_env), 1);
        storage.shutdown().await.unwrap();
    }

    /// Group 6 (Window): the padded lookback window has a documented limit.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_lookback_window_has_a_documented_limit() {
        let root = 20_000 * DAY + 10 * HOUR;
        // A late partial 50 min after the root, with a window starting between
        // them: the root is still gathered, so the merged start is before the
        // window and the trace is not returned.
        let dir1 = tempfile::tempdir().unwrap();
        let s1 = window_storage(dir1.path(), root, 50 * MIN).await;
        let q1 = s1.query(project(), Default::default()).await.unwrap();
        let ctx1 = session(&q1);
        let early = scan_with(
            &ctx1,
            &q1.traces,
            None,
            window(root + 25 * MIN, root + 2 * HOUR),
            None,
        )
        .await;
        assert_eq!(rows(&early), 0, "merged start is before the window");
        s1.shutdown().await.unwrap();

        // A late partial 70 min after the root (beyond L = 1 hour): the root
        // partial falls outside the padded window, so the trace is returned
        // with the late partial's start. This is the documented window limit.
        let dir2 = tempfile::tempdir().unwrap();
        let s2 = window_storage(dir2.path(), root, 70 * MIN).await;
        let q2 = s2.query(project(), Default::default()).await.unwrap();
        let ctx2 = session(&q2);
        let late = scan_with(
            &ctx2,
            &q2.traces,
            None,
            window(root + 65 * MIN, root + 3 * HOUR),
            None,
        )
        .await;
        let all = concat(&late, traces_schema());
        assert_eq!(all.num_rows(), 1);
        assert_eq!(
            timestamp_column(&all, traces::START_TIME),
            vec![root + 70 * MIN]
        );
        s2.shutdown().await.unwrap();
    }

    /// Group 9 (Flush race): a query before and after the commit sees each
    /// trace exactly once with the full span count.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_query_during_a_flush_sees_each_trace_once() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(aged(dir.path())).await.unwrap();
        let hooks = storage.hooks().clone();
        hooks.hold(HookPoint::FlushAfterWrite);
        let release = Release {
            hooks: hooks.clone(),
            point: HookPoint::FlushAfterWrite,
        };
        storage
            .append_spans(project(), spans_at(&[1, 2]))
            .await
            .unwrap();
        wait_held(&storage, HookPoint::FlushAfterWrite).await;

        let query = storage.query(project(), Default::default()).await.unwrap();
        let before = concat(
            &collect_off(&query, "SELECT * FROM traces").await,
            traces_schema(),
        );
        assert_eq!(before.num_rows(), 1);
        assert_eq!(int64_column(&before, traces::SPAN_COUNT), vec![2]);

        // Let the commit happen, but pause just after it (files in catalog).
        hooks.hold(HookPoint::FlushAfterCommit);
        let commit = Release {
            hooks: hooks.clone(),
            point: HookPoint::FlushAfterCommit,
        };
        drop(release);
        wait_held(&storage, HookPoint::FlushAfterCommit).await;
        let query = storage.query(project(), Default::default()).await.unwrap();
        let after = concat(
            &collect_off(&query, "SELECT * FROM traces").await,
            traces_schema(),
        );
        assert_eq!(after.num_rows(), 1);
        assert_eq!(int64_column(&after, traces::SPAN_COUNT), vec![2]);
        drop(commit);
        storage.shutdown().await.unwrap();
    }

    /// A trace id whose last four bytes are `value`, so ids sort by `value`.
    fn numbered_trace(value: u32) -> TraceId {
        let mut bytes = [0_u8; 16];
        bytes[12..].copy_from_slice(&value.to_be_bytes());
        TraceId::from_bytes(bytes).unwrap()
    }

    /// One single-span trace per `numbered_trace(1..=count)`, all in one day.
    fn numbered_spans(count: u32) -> Vec<Span> {
        (1..=count)
            .map(|value| {
                base_span(
                    numbered_trace(value),
                    span_id(1),
                    20_000 * DAY + i64::from(value),
                )
            })
            .collect()
    }

    /// The `trace_id` and `span_count` columns of a query result, by trace id.
    fn span_counts_by_trace(batches: &[RecordBatch]) -> BTreeMap<Vec<u8>, i64> {
        let mut counts = BTreeMap::new();
        for batch in batches {
            let ids = batch
                .column_by_name(traces::TRACE_ID)
                .unwrap()
                .as_fixed_size_binary();
            let spans = int64_column(batch, traces::SPAN_COUNT);
            for (row, count) in spans.into_iter().enumerate() {
                counts.insert(ids.value(row).to_vec(), count);
            }
        }
        counts
    }

    /// Sum of the spill counts of every operator in `plan`.
    fn spill_count_of(plan: &Arc<dyn datafusion::physical_plan::ExecutionPlan>) -> usize {
        let own = plan
            .metrics()
            .and_then(|metrics| metrics.spill_count())
            .unwrap_or(0);
        own + plan
            .children()
            .into_iter()
            .map(spill_count_of)
            .sum::<usize>()
    }

    /// M6.8 (D-A1-13): a pool too small for the merge's sorts spills them, and
    /// the merge still returns every trace, as the default-memory query does.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_small_pool_spills_the_merge_and_returns_every_trace() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        storage
            .append_spans(project(), spans_batch(&numbered_spans(50_000)))
            .await
            .unwrap();
        storage.shutdown().await.unwrap();

        let expected = {
            let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
            let query = storage.query(project(), Default::default()).await.unwrap();
            let ctx = session(&query);
            ctx.register_table("traces", Arc::clone(&query.traces))
                .unwrap();
            let counts = span_counts_by_trace(
                &collect(&ctx, "SELECT trace_id, span_count FROM traces").await,
            );
            storage.shutdown().await.unwrap();
            counts
        };
        assert_eq!(expected.len(), 50_000);

        let mut config = config_in(dir.path());
        config.query.max_memory = ByteSize::mib(8);
        config.query.initial_memory = ByteSize::mib(1);
        config.query.max_memory_total = Some(ByteSize::mib(8));
        let storage = crate::Storage::open(config).await.unwrap();
        let query = storage.query(project(), Default::default()).await.unwrap();
        // Each sort reserves 1 MiB to merge its spill files, not the default 10 MiB.
        let ctx = SessionContext::new_with_config_rt(
            SessionConfig::new()
                .with_target_partitions(2)
                .with_batch_size(1024)
                .with_sort_spill_reservation_bytes(1024 * 1024),
            Arc::clone(&query.runtime),
        );
        ctx.register_table("traces", Arc::clone(&query.traces))
            .unwrap();
        let plan = ctx
            .sql("SELECT trace_id, span_count FROM traces")
            .await
            .unwrap()
            .create_physical_plan()
            .await
            .unwrap();
        let batches = collect_plan(Arc::clone(&plan), ctx.task_ctx())
            .await
            .unwrap();
        assert!(spill_count_of(&plan) > 0, "the merge did not spill");
        assert_eq!(span_counts_by_trace(&batches), expected);
        storage.shutdown().await.unwrap();
    }

    /// M6.8: a pool below the sort's merge reservation fails as too large
    /// (A2 maps ResourcesExhausted to query_too_large), whatever the traces.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_pool_below_the_sort_floor_fails_as_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        storage
            .append_spans(project(), spans_batch(&numbered_spans(50_000)))
            .await
            .unwrap();
        storage.shutdown().await.unwrap();

        let mut config = config_in(dir.path());
        config.query.max_memory = ByteSize::mib(1);
        config.query.initial_memory = ByteSize::mib(1);
        config.query.max_memory_total = Some(ByteSize::mib(1));
        let storage = crate::Storage::open(config).await.unwrap();
        let ctx = QueryContext::new(
            storage.query(project(), Default::default()).await.unwrap(),
            None,
        )
        .unwrap();
        let params = serde_json::Map::new();
        let result = ctx
            .sql_json(crate::SqlRequest {
                query: "SELECT * FROM traces",
                params: &params,
                limit: 50,
                timeout: TIMEOUT,
            })
            .await;
        match result {
            Err(crate::RunError::TooLarge) => {}
            Err(other) => panic!("expected too large, got {other:?}"),
            Ok(_) => panic!("the sorts fit in one megabyte"),
        }
        drop(ctx);
        storage.shutdown().await.unwrap();
    }

    /// M6.8: runs longer than one merge chunk merge into one row each. One
    /// partition, 3,000 single-span traces, and one trace in the middle of the
    /// id order with a span in each of three flushes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn runs_longer_than_a_chunk_merge_into_one_row() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_in(dir.path());
        config.query.target_partitions = 1;
        let day = 20_000 * DAY;
        // Even ids for the 3,000 traces, so the split trace (id 3001) sorts
        // between 3,000 and 3,002.
        let mut spans: Vec<Span> = (1..=3_000_u32)
            .map(|value| {
                base_span(
                    numbered_trace(2 * value),
                    span_id(1),
                    day + i64::from(value),
                )
            })
            .collect();
        spans.push(base_span(numbered_trace(3_001), span_id(1), day + 5_000));
        let storage = crate::Storage::open(config.clone()).await.unwrap();
        storage
            .append_spans(project(), spans_batch(&spans))
            .await
            .unwrap();
        storage.shutdown().await.unwrap();
        for index in 2..=3_u8 {
            let storage = crate::Storage::open(config.clone()).await.unwrap();
            let span = base_span(
                numbered_trace(3_001),
                span_id(index),
                day + 5_000 + i64::from(index),
            );
            storage
                .append_spans(project(), spans_batch(&[span]))
                .await
                .unwrap();
            storage.shutdown().await.unwrap();
        }

        let storage = crate::Storage::open(config).await.unwrap();
        let query = storage.query(project(), Default::default()).await.unwrap();
        let ctx = session(&query);
        ctx.register_table("traces", Arc::clone(&query.traces))
            .unwrap();
        let batches = collect(&ctx, "SELECT trace_id, span_count FROM traces").await;
        assert_eq!(rows(&batches), 3_001);
        let counts = span_counts_by_trace(&batches);
        assert_eq!(counts.len(), 3_001);
        assert_eq!(counts[&numbered_trace(3_001).as_bytes().to_vec()], 3);
        assert_eq!(counts.values().filter(|count| **count != 1).count(), 1);
        storage.shutdown().await.unwrap();
    }

    /// M6.8: the merge input is sorted by `trace_id`, straight under `TracesExec`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_merge_input_is_sorted_by_trace_id() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        storage
            .append_spans(project(), spans_batch(&numbered_spans(100)))
            .await
            .unwrap();
        let storage = reopen(dir.path(), storage).await;
        let query = storage.query(project(), Default::default()).await.unwrap();
        let ctx = session(&query);
        ctx.register_table("traces", Arc::clone(&query.traces))
            .unwrap();
        let frame = ctx
            .table("traces")
            .await
            .unwrap()
            .filter(start_ge(0))
            .unwrap();
        let explained = frame
            .explain(false, false)
            .unwrap()
            .collect()
            .await
            .unwrap();
        let text = plan_text(&explained);
        let lines: Vec<&str> = text.lines().collect();
        let at = lines
            .iter()
            .position(|line| line.contains("TracesExec:"))
            .unwrap();
        let below = lines[at + 1..].iter().find(|line| !line.trim().is_empty());
        let below = below.unwrap_or(&"");
        assert!(
            below.contains("SortExec: expr=[trace_id@")
                || below.contains("SortPreservingMergeExec: [trace_id@"),
            "{text}"
        );
        storage.shutdown().await.unwrap();
    }

    /// Group 11 (A2 over the real engine): trace_list (default + include_eval)
    /// and trace_detail return the same rows as over example_query().
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a2_reads_unchanged_over_the_real_provider() {
        let spans = dataset_spans();
        let dir = tempfile::tempdir().unwrap();
        let mut storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        // Ingested as in the Oracle: three flushes, last part in the memtable.
        let chunks: Vec<&[Span]> = spans.chunks(4).collect();
        for (index, chunk) in chunks.iter().enumerate() {
            storage
                .append_spans(dataset_project(), spans_batch(chunk))
                .await
                .unwrap();
            if index < chunks.len() - 1 {
                storage = reopen(dir.path(), storage).await;
            }
        }

        let real = QueryContext::new(
            storage
                .query(dataset_project(), Default::default())
                .await
                .unwrap(),
            None,
        )
        .unwrap();
        let fixture = QueryContext::new(example_query(), None).unwrap();

        let request = |include_eval: bool| TraceListRequest {
            filter: None,
            search: None,
            scope: ListScope {
                window: None,
                include_eval,
                include_experiments: false,
            },
            sort: TraceSort::StartTime,
            order: SortOrder::Desc,
            limit: 50,
            cursor: None,
            timeout: TIMEOUT,
        };

        for include_eval in [false, true] {
            let from_real = trace_list(&real, request(include_eval)).await.unwrap();
            let from_fixture = trace_list(&fixture, request(include_eval)).await.unwrap();
            assert_pages_equal(&from_real, &from_fixture);
        }

        for id in [
            "a1000000000000000000000000000001",
            "a1000000000000000000000000000002",
        ] {
            let a: TraceDetailData = trace_detail(&real, id, TIMEOUT).await.unwrap().unwrap();
            let b: TraceDetailData = trace_detail(&fixture, id, TIMEOUT).await.unwrap().unwrap();
            assert_eq!(a.truncated, b.truncated);
            assert_batches_equal(&a.trace, &b.trace);
            assert_batch_vecs_equal(&a.spans, &b.spans);
        }
        storage.shutdown().await.unwrap();
    }

    /// Step 8 (Limit): a limit uses one output partition and stops after k rows.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_limit_stops_after_k_rows() {
        let d = 20_000 * DAY + 10 * HOUR;
        let spans: Vec<Span> = (1..=3)
            .map(|index| base_span(trace_id(index), span_id(1), d + i64::from(index) * MIN))
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        storage
            .append_spans(project(), spans_batch(&spans))
            .await
            .unwrap();
        let storage = reopen(dir.path(), storage).await;

        let query = storage.query(project(), Default::default()).await.unwrap();
        let ctx = session(&query);
        ctx.register_table("traces", Arc::clone(&query.traces))
            .unwrap();
        assert_eq!(
            rows(&collect(&ctx, "SELECT * FROM traces LIMIT 2").await),
            2
        );
        assert_eq!(rows(&collect(&ctx, "SELECT * FROM traces").await), 3);

        let explained = collect(&ctx, "EXPLAIN SELECT * FROM traces LIMIT 2").await;
        let plan = plan_text(&explained);
        let line = plan
            .lines()
            .find(|line| line.contains("TracesExec:"))
            .unwrap()
            .to_owned();
        assert!(line.contains("mode=range"), "{line}");
        // One output partition: EnforceDistribution must not repartition the
        // single-partition limit scan back to target_partitions, which would
        // repeat the whole merge once per partition. Any repartition left is
        // the single-partition one.
        for line in plan.lines().filter(|line| line.contains("RepartitionExec")) {
            assert!(
                line.contains("partitioning=Hash([trace_id@") && line.contains("], 1)"),
                "limit scan was repartitioned: {line}"
            );
        }
        // The sorted partitions merge with SortPreservingMergeExec (D-A1-13).
        assert!(plan.contains("SortPreservingMergeExec"), "{plan}");
        storage.shutdown().await.unwrap();
    }

    // ------------------------------------------------------------ M3.7 tests

    use proptest::prelude::*;

    /// A list-mode context (with `TraceListRule`) and a range-mode context
    /// (rules cleared), both over the same providers.
    async fn query_contexts(
        storage: &crate::Storage,
        project: ProjectId,
    ) -> (QueryContext, QueryContext) {
        let query = storage.query(project, Default::default()).await.unwrap();
        let range = ProjectQuery {
            spans: Arc::clone(&query.spans),
            traces: Arc::clone(&query.traces),
            scores: Arc::clone(&query.scores),
            runtime: Arc::clone(&query.runtime),
            target_partitions: query.target_partitions,
            optimizer_rules: Vec::new(),

            suspects: crate::verify::Suspects::detached(),
        };
        (
            QueryContext::new(query, None).unwrap(),
            QueryContext::new(range, None).unwrap(),
        )
    }

    fn request<'a>(
        filter: Option<&'a str>,
        limit: usize,
        cursor: Option<&'a str>,
    ) -> TraceListRequest<'a> {
        TraceListRequest {
            filter,
            search: None,
            scope: ListScope {
                window: None,
                include_eval: true,
                include_experiments: true,
            },
            sort: TraceSort::StartTime,
            order: SortOrder::Desc,
            limit,
            cursor,
            timeout: TIMEOUT,
        }
    }

    fn page_batch(page: &Page) -> RecordBatch {
        let schema = page
            .batches
            .first()
            .map(RecordBatch::schema)
            .unwrap_or_else(|| Arc::new(Schema::empty()));
        concat(&page.batches, schema)
    }

    /// Equivalence: list mode returns exactly the same rows as range mode.
    async fn assert_equivalent(
        list: &QueryContext,
        range: &QueryContext,
        filter: Option<&str>,
        limit: usize,
        cursor: Option<&str>,
    ) {
        let a = trace_list(list, request(filter, limit, cursor))
            .await
            .unwrap();
        let b = trace_list(range, request(filter, limit, cursor))
            .await
            .unwrap();
        assert_eq!(a.next_cursor, b.next_cursor, "next_cursor");
        assert_batches_equal(&page_batch(&a), &page_batch(&b));
    }

    /// Generate `count` traces, each with 1–3 spans no more than L/2 apart,
    /// spread over three days, with random filterable fields.
    fn generated_spans(seed: u64, count: usize) -> Vec<Span> {
        let mut rng = seed | 1;
        let base = 20_000 * DAY + 10 * HOUR;
        let unit = (3 * DAY) / (1_i64 << 31);
        let mut spans = Vec::new();
        for trace_index in 0..count {
            let trace = trace_id(u8::try_from(trace_index % 250 + 1).unwrap());
            let root = base + i64::from(next(&mut rng) as u32) * unit;
            let span_count = 1 + next(&mut rng) % 3;
            for span_index in 0..span_count {
                let start = root + i64::try_from(span_index).unwrap() * 5 * MIN;
                let mut span =
                    base_span(trace, span_id(u8::try_from(span_index + 1).unwrap()), start);
                span.cost_total = Some((next(&mut rng) % 200) as f64 / 100.0);
                span.environment = match next(&mut rng) % 3 {
                    0 => Some("prod".to_owned()),
                    1 => Some("eval".to_owned()),
                    _ => None,
                };
                span.session_id = match next(&mut rng) % 3 {
                    0 => Some("s1".to_owned()),
                    1 => Some("s2".to_owned()),
                    _ => None,
                };
                span.tags = if next(&mut rng).is_multiple_of(2) {
                    vec!["beta".to_owned()]
                } else {
                    vec!["alpha".to_owned()]
                };
                span.metadata.insert(
                    "channel".to_owned(),
                    if next(&mut rng).is_multiple_of(2) {
                        "email".to_owned()
                    } else {
                        "chat".to_owned()
                    },
                );
                if span_index > 0 {
                    span.parent_span_id = Some(span_id(1));
                }
                spans.push(span);
            }
        }
        spans
    }

    /// Append with `groups - 1` flushes and leave the last part in the memtable.
    async fn ingest(dir: &Path, spans: &[Span], groups: usize) -> crate::Storage {
        let chunk_size = spans.len().div_ceil(groups).max(1);
        let mut storage = crate::Storage::open(config_in(dir)).await.unwrap();
        for (index, chunk) in spans.chunks(chunk_size).enumerate() {
            storage
                .append_spans(project(), spans_batch(chunk))
                .await
                .unwrap();
            if index < groups - 1 {
                storage = reopen(dir, storage).await;
            }
        }
        storage
    }

    async fn equivalence_case(seed: u64, filter_case: usize, limit: usize, cursor_at: usize) {
        let spans = generated_spans(seed, 40);
        let dir = tempfile::tempdir().unwrap();
        let storage = ingest(dir.path(), &spans, 3).await;
        let (list, range) = query_contexts(&storage, project()).await;
        let filter = match filter_case {
            0 => None,
            1 => Some("cost_total > 0.5"),
            2 => Some("environment = 'prod'"),
            3 => Some("session_id = 's1'"),
            4 => Some("tags @> ['beta']"),
            _ => Some("metadata['channel'] = 'email'"),
        };
        assert_equivalent(&list, &range, filter, limit, None).await;

        // A2's cursor predicate after a random row.
        let first = trace_list(&range, request(None, cursor_at, None))
            .await
            .unwrap();
        if let Some(cursor) = first.next_cursor.as_deref() {
            assert_equivalent(&list, &range, None, limit, Some(cursor)).await;
        }
        storage.shutdown().await.unwrap();
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(32))]

        /// Group 1 (Equivalence): list mode == range mode + sort + truncate.
        #[test]
        fn list_mode_matches_range_mode(
            seed in any::<u64>(),
            filter_case in 0usize..6,
            limit in prop_oneof![Just(1usize), Just(3usize), Just(50usize), Just(500usize)],
            cursor_at in 1usize..10,
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(equivalence_case(seed, filter_case, limit, cursor_at));
        }
    }

    async fn plan_line(frame: datafusion::prelude::DataFrame) -> String {
        let explained = frame
            .explain(false, false)
            .unwrap()
            .collect()
            .await
            .unwrap();
        let text = plan_text(&explained);
        text.lines()
            .find(|line| line.contains("TracesExec:"))
            .unwrap_or("")
            .to_owned()
    }

    fn plan_ts(value: i64) -> Expr {
        Expr::Literal(
            ScalarValue::TimestampNanosecond(Some(value), Some(Arc::from("UTC"))),
            None,
        )
    }

    /// Rebuilds the A2 list query (`table("traces")` + filter + sort + limit)
    /// from the same pieces `list_frame` uses, then reads its physical plan.
    /// Group 2 (Plan shapes): list mode for the four list shapes, range mode
    /// for the three shapes the rule must not touch.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn plan_shapes_through_a2() {
        use crate::filter::FilterView;
        use datafusion::logical_expr::{SortExpr, lit};

        let desc_sorts = || -> Vec<SortExpr> {
            vec![
                col("start_time").sort(false, false),
                col("trace_id").sort(false, false),
            ]
        };
        let spans = generated_spans(0x2545_F491_4F6C_DD1D, 20);
        let dir = tempfile::tempdir().unwrap();
        let storage = ingest(dir.path(), &spans, 2).await;
        let (list, _) = query_contexts(&storage, project()).await;
        let session = list.session();

        // The default list.
        let frame = session
            .table("traces")
            .await
            .unwrap()
            .sort(desc_sorts())
            .unwrap()
            .limit(0, Some(51))
            .unwrap();
        assert!(plan_line(frame).await.contains("mode=list, fetch=51"));

        // With a window.
        let windowed = session
            .table("traces")
            .await
            .unwrap()
            .filter(
                col("start_time")
                    .gt_eq(plan_ts(20_000 * DAY))
                    .and(col("start_time").lt(plan_ts(20_003 * DAY))),
            )
            .unwrap()
            .sort(desc_sorts())
            .unwrap()
            .limit(0, Some(51))
            .unwrap();
        assert!(plan_line(windowed).await.contains("mode=list, fetch=51"));

        // With A2's cursor predicate after a row.
        let at = 20_001 * DAY;
        let cursor = col("start_time")
            .lt(plan_ts(at))
            .or(col("start_time")
                .eq(plan_ts(at))
                .and(col("trace_id").lt(lit("ff"))))
            .or(col("start_time").is_null());
        let with_cursor = session
            .table("traces")
            .await
            .unwrap()
            .filter(cursor)
            .unwrap()
            .sort(desc_sorts())
            .unwrap()
            .limit(0, Some(51))
            .unwrap();
        assert!(plan_line(with_cursor).await.contains("mode=list, fetch=51"));

        // With a plain filter.
        let plain = session
            .table("traces")
            .await
            .unwrap()
            .filter(col("cost_total").gt(lit(0.0_f64)))
            .unwrap()
            .sort(desc_sorts())
            .unwrap()
            .limit(0, Some(51))
            .unwrap();
        assert!(plan_line(plain).await.contains("mode=list, fetch=51"));

        // A score-function filter puts a join in the way: no rewrite.
        let score = list
            .parse_filter(FilterView::Traces, "score('helpfulness') >= 0.5")
            .unwrap();
        let scored = score
            .apply(session.table("traces").await.unwrap(), &list)
            .await
            .unwrap()
            .sort(desc_sorts())
            .unwrap()
            .limit(0, Some(51))
            .unwrap();
        assert!(plan_line(scored).await.contains("mode=range"));

        // A different sort column: no rewrite.
        let by_cost = session
            .table("traces")
            .await
            .unwrap()
            .sort(vec![
                col("cost_total").sort(false, false),
                col("trace_id").sort(false, false),
            ])
            .unwrap()
            .limit(0, Some(51))
            .unwrap();
        assert!(plan_line(by_cost).await.contains("mode=range"));

        // Ascending order: no rewrite.
        let ascending = session
            .table("traces")
            .await
            .unwrap()
            .sort(vec![
                col("start_time").sort(true, false),
                col("trace_id").sort(true, false),
            ])
            .unwrap()
            .limit(0, Some(51))
            .unwrap();
        assert!(plan_line(ascending).await.contains("mode=range"));

        storage.shutdown().await.unwrap();
    }

    fn hex_ids(page: &Page) -> Vec<String> {
        let mut ids = Vec::new();
        for batch in &page.batches {
            let column = batch
                .column_by_name(traces::TRACE_ID)
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            for row in 0..column.len() {
                ids.push(column.value(row).to_owned());
            }
        }
        ids
    }

    /// Group 3 (Paging): walking pages of 2 over the real engine gives the
    /// same ids as one page of 50.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn paging_over_the_real_engine() {
        let spans = generated_spans(0x51ED_270B, 30);
        let dir = tempfile::tempdir().unwrap();
        let storage = ingest(dir.path(), &spans, 2).await;
        let (list, _) = query_contexts(&storage, project()).await;

        let mut walked = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..60 {
            let page = trace_list(&list, request(None, 2, cursor.as_deref()))
                .await
                .unwrap();
            walked.extend(hex_ids(&page));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        let full = trace_list(&list, request(None, 50, None)).await.unwrap();
        assert_eq!(walked, hex_ids(&full));
        storage.shutdown().await.unwrap();
    }

    /// Group 4 (Slices): 55 traces within 24 h of the newest plus 5 older ones,
    /// k = 50. Phase 1 reaches k inside the 24 h slices and stops before the
    /// oldest slice; the result is still the 50 newest traces.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn slices_read_newest_traces_over_three_days() {
        let b = 20_000 * DAY + 12 * HOUR;
        let mut spans: Vec<Span> = (0..55)
            .map(|index| {
                let mut span = base_span(
                    trace_id(u8::try_from(index + 1).unwrap()),
                    span_id(1),
                    b - i64::from(index) * 20 * MIN,
                );
                span.cost_total = Some(1.0);
                span
            })
            .collect();
        for index in 0..5 {
            let mut span = base_span(
                trace_id(u8::try_from(60 + index).unwrap()),
                span_id(1),
                b - 2 * DAY - i64::from(index) * HOUR,
            );
            span.cost_total = Some(1.0);
            spans.push(span);
        }
        let dir = tempfile::tempdir().unwrap();
        let storage = ingest(dir.path(), &spans, 3).await;
        let (list, range) = query_contexts(&storage, project()).await;
        assert_equivalent(&list, &range, None, 50, None).await;
        let page = trace_list(&list, request(None, 50, None)).await.unwrap();
        assert_eq!(hex_ids(&page).len(), 50);
        storage.shutdown().await.unwrap();
    }

    /// Group 4 (Slices): a filter matching only traces older than 24 h still
    /// finds them — every slice is searched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn slices_reach_traces_older_than_a_day() {
        let b = 20_000 * DAY + 12 * HOUR;
        let mut spans: Vec<Span> = (0..20)
            .map(|index| {
                let mut span = base_span(
                    trace_id(u8::try_from(index + 1).unwrap()),
                    span_id(1),
                    b - 2 * DAY + i64::from(index) * MIN,
                );
                span.cost_total = Some(1.0);
                span
            })
            .collect();
        let mut fresh = base_span(trace_id(200), span_id(1), b);
        fresh.cost_total = Some(0.0);
        spans.push(fresh);
        let dir = tempfile::tempdir().unwrap();
        let storage = ingest(dir.path(), &spans, 2).await;
        let (list, range) = query_contexts(&storage, project()).await;
        assert_equivalent(&list, &range, Some("cost_total > 0.5"), 50, None).await;
        let page = trace_list(&list, request(Some("cost_total > 0.5"), 50, None))
            .await
            .unwrap();
        assert_eq!(hex_ids(&page).len(), 20);
        storage.shutdown().await.unwrap();
    }

    /// Group 4 (Slices): a 3-hour gap right before the newest data must not
    /// end the search (empty slices are not a stop condition).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn slices_skip_empty_windows() {
        let b = 20_000 * DAY + 12 * HOUR;
        let mut spans: Vec<Span> = (0..20)
            .map(|index| {
                let mut span = base_span(
                    trace_id(u8::try_from(index + 1).unwrap()),
                    span_id(1),
                    b - 3 * HOUR + i64::from(index) * MIN,
                );
                span.cost_total = Some(1.0);
                span
            })
            .collect();
        let mut top = base_span(trace_id(201), span_id(1), b);
        top.cost_total = Some(1.0);
        spans.push(top);
        let dir = tempfile::tempdir().unwrap();
        let storage = ingest(dir.path(), &spans, 2).await;
        let (list, range) = query_contexts(&storage, project()).await;
        assert_equivalent(&list, &range, None, 50, None).await;
        let page = trace_list(&list, request(None, 50, None)).await.unwrap();
        assert_eq!(hex_ids(&page).len(), 21);
        storage.shutdown().await.unwrap();
    }

    /// Group 4 (Slices): an open upper bound starts from the newest data,
    /// not the wall clock. Data far in the past is still returned.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn open_upper_bound_starts_from_data() {
        let mut spans = generated_spans(0x1357_9BDF, 10);
        // 1970-era timestamps: nowhere near wall-clock "now".
        for (index, span) in spans.iter_mut().enumerate() {
            span.start_time = 100 + i64::try_from(index).unwrap() * 1_000;
            span.end_time = span.start_time + 10;
            span.ingest_time = span.start_time + 1;
        }
        let dir = tempfile::tempdir().unwrap();
        let storage = ingest(dir.path(), &spans, 2).await;
        let (list, range) = query_contexts(&storage, project()).await;
        assert_equivalent(&list, &range, None, 50, None).await;
        let page = trace_list(&list, request(None, 50, None)).await.unwrap();
        assert_eq!(hex_ids(&page).len(), 10);
        storage.shutdown().await.unwrap();
    }

    /// Group 5 (Window limit): a trace whose late partial starts 2 h after its
    /// root and inside the newest slice appears in list mode with its late
    /// start. This is the documented window limit (partials more than L apart).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn window_limit_late_partial_appears_with_late_start() {
        let late = 20_000 * DAY + 12 * HOUR;
        let root = late - 3 * HOUR;
        let trace = trace_id(7);
        let root_span = base_span(trace, span_id(1), root);
        let mut late_span = base_span(trace, span_id(2), late);
        late_span.parent_span_id = Some(span_id(1));

        let dir = tempfile::tempdir().unwrap();
        let mut storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        storage
            .append_spans(project(), spans_batch(&[root_span]))
            .await
            .unwrap();
        storage = reopen(dir.path(), storage).await;
        storage
            .append_spans(project(), spans_batch(&[late_span]))
            .await
            .unwrap();
        let (list, _) = query_contexts(&storage, project()).await;

        let page = trace_list(&list, request(None, 50, None)).await.unwrap();
        assert_eq!(hex_ids(&page).len(), 1);
        let start = page.batches[0]
            .column_by_name(traces::START_TIME)
            .unwrap()
            .as_any()
            .downcast_ref::<arrow::array::TimestampNanosecondArray>()
            .unwrap();
        assert_eq!(start.value(0), late, "list mode uses the late start");
        storage.shutdown().await.unwrap();
    }
}
