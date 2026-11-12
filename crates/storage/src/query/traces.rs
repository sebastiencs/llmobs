//! `traces` table provider over one snapshot's `traces` files and the spans
//! memtable (D152).
//!
//! A scan reads *partials* — one row per trace per file or per memtable batch —
//! hash-partitions them by `trace_id`, and merges each partition into one row
//! per trace with [`TraceMerger`]. Filters are answered `Exact`, so they are
//! evaluated on the *merged* rows, after the merge, not on the partials.

use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use arrow::array::{
    Array, ArrayRef, BooleanArray, RecordBatch, RecordBatchOptions, TimestampNanosecondArray,
};
use arrow::compute::filter_record_batch;
use arrow::datatypes::{Schema, SchemaRef};
use async_trait::async_trait;
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::tree_node::TreeNodeRecursion;
use datafusion::common::{DFSchema, DataFusionError, ScalarValue};
use datafusion::datasource::physical_plan::ParquetSource;
use datafusion::error::Result as DfResult;
use datafusion::execution::TaskContext;
use datafusion::execution::memory_pool::{MemoryConsumer, MemoryReservation};
use datafusion::execution::object_store::ObjectStoreUrl;
use datafusion::logical_expr::expr::InList;
use datafusion::logical_expr::{
    BinaryExpr, Expr, Operator, TableProviderFilterPushDown, TableType, col,
};
use datafusion::physical_expr::EquivalenceProperties;
use datafusion::physical_expr::expressions::Column;
use datafusion::physical_expr::utils::conjunction_opt;
use datafusion::physical_expr::{Partitioning, PhysicalExpr};
use datafusion::physical_plan::execution_plan::{Boundedness, EmissionType};
use datafusion::physical_plan::filter::FilterExec;
use datafusion::physical_plan::repartition::RepartitionExec;
use datafusion::physical_plan::stream::RecordBatchStreamAdapter;
use datafusion::physical_plan::union::UnionExec;
use datafusion::physical_plan::{
    DisplayAs, DisplayFormatType, Distribution, ExecutionPlan, InputDistributionRequirements,
    PlanProperties, SendableRecordBatchStream,
};
use datafusion_datasource::file_scan_config::FileScanConfigBuilder;
use datafusion_datasource::memory::MemorySourceConfig;
use datafusion_datasource::source::DataSourceExec;
use futures::stream::Stream;
use llmobs_core::ids::{ProjectId, TraceId};
use llmobs_core::schema::{traces, traces_schema};

use super::{
    DAY_NS, TimeBounds, distribute, file_day_range, file_groups, file_overlaps, lookup_ids,
    range_overlaps, time_bounds,
};
use crate::QueryOptions;
use crate::catalog::{Catalog, FileMeta, FileTable};
use crate::memtable::{MemBatch, MemSnapshot};
use crate::objects::OBJECT_STORE_URL;
use crate::partials::{MergeColumns, TraceMerger};

/// Traces of one project at one read snapshot (range and lookup, D152/D154).
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
}

impl TracesProvider {
    pub(crate) fn new(
        project: ProjectId,
        catalog: Arc<Catalog>,
        spans_mem: Arc<MemSnapshot>,
        options: QueryOptions,
        lookback: i64,
        partitions: usize,
    ) -> Self {
        Self {
            project,
            catalog,
            spans_mem,
            options,
            lookback,
            partitions,
        }
    }
}

/// Whether the merged start-time bound comes from a range scan or a lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Range,
    Lookup,
}

impl Mode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Range => "range",
            Self::Lookup => "lookup",
        }
    }
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
        let bounds = time_bounds(filters, traces::START_TIME);
        let ids = lookup_ids(filters);
        let padded = padded_bounds(bounds, self.lookback);

        // Columns the merge reads and produces, in traces_schema() order.
        let requested = requested_names(projection, filters, &schema)?;
        let requested_refs: Vec<&str> = requested.iter().map(String::as_str).collect();
        let merge_columns = MergeColumns::new(&requested_refs);
        let input_names = merge_columns.input();
        let output_names = merge_columns.output();
        let input_indices = indices_of(&schema, &input_names)?;
        let output_indices = indices_of(&schema, &output_names)?;
        let input_schema: SchemaRef = Arc::new(project_schema(&schema, &input_indices));
        let merged_schema: SchemaRef = Arc::new(project_schema(&schema, &output_indices));
        let (output_schema, projection_kind) = output_of(projection, &schema)?;

        let full_df = DFSchema::try_from(Arc::clone(&schema))?;
        let input_df = DFSchema::try_from(Arc::clone(&input_schema))?;
        let merged_df = DFSchema::try_from(Arc::clone(&merged_schema))?;

        // The scan predicate is applied to *partials*, so it only contains the
        // padded time window and the trace ids. Filter predicates that only
        // pass after the merge (e.g. cost_total > 1.0) are never pushed here.
        let scan_exprs = scan_exprs(&padded, &ids);
        let file_pred = to_physical(state, &full_df, &scan_exprs)?;
        let mem_pred = to_physical(state, &input_df, &scan_exprs)?;

        let files = trace_files(
            &self.catalog,
            self.project,
            &padded,
            filters,
            &bounds,
            self.options.around,
        );
        let batches = mem_batches(&self.spans_mem, self.project, &padded);
        let mut partials = Vec::with_capacity(batches.len());
        for batch in &batches {
            let cached = batch.trace_partials().map_err(partial_err)?;
            partials.push((*cached).clone());
        }

        let mut plans: Vec<Arc<dyn ExecutionPlan>> = Vec::new();
        if !files.is_empty() {
            plans.push(file_plan(
                &schema,
                &input_indices,
                &files,
                &input_df,
                file_pred.as_ref(),
                self.partitions,
            )?);
        }
        plans.push(mem_plan(
            &schema,
            &input_indices,
            partials,
            self.partitions,
            mem_pred,
        )?);
        let union = UnionExec::try_new(plans)?;

        let trace_id_index = input_names
            .iter()
            .position(|name| *name == traces::TRACE_ID)
            .ok_or_else(|| DataFusionError::Internal("trace_id missing from merge input".into()))?;
        let key: Arc<dyn PhysicalExpr> = Arc::new(Column::new(traces::TRACE_ID, trace_id_index));
        let partitions = if limit.is_some() { 1 } else { self.partitions };
        let repartition: Arc<dyn ExecutionPlan> = Arc::new(RepartitionExec::try_new(
            union,
            Partitioning::Hash(vec![Arc::clone(&key)], partitions),
        )?);

        // Filters are converted once, against the merged output schema.
        let mut physical_filters = Vec::with_capacity(filters.len());
        for filter in filters {
            physical_filters.push(state.create_physical_expr(filter.clone(), &merged_df)?);
        }
        let predicate = conjunction_opt(physical_filters);

        let columns = input_names.join(", ");
        let mode = if ids.is_some() {
            Mode::Lookup
        } else {
            Mode::Range
        };
        let display = format!(
            "TracesExec: mode={}, files={}, mem_batches={}, columns=[{columns}]",
            mode.as_str(),
            files.len(),
            batches.len(),
        );

        Ok(Arc::new(TracesExec {
            input: repartition,
            properties: Arc::new(PlanProperties::new(
                EquivalenceProperties::new(Arc::clone(&output_schema)),
                Partitioning::UnknownPartitioning(partitions),
                EmissionType::Incremental,
                Boundedness::Bounded,
            )),
            merge_columns,
            key: vec![key],
            predicate,
            bounds,
            projection: projection_kind,
            output_schema,
            limit,
            display,
        }))
    }
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
) -> DfResult<(SchemaRef, Projection)> {
    match projection {
        None => Ok((Arc::clone(schema), Projection::All)),
        Some(indices) if indices.is_empty() => Ok((Arc::new(Schema::empty()), Projection::Empty)),
        Some(indices) => {
            let names: Vec<String> = indices
                .iter()
                .map(|&index| schema.field(index).name().clone())
                .collect();
            let fields: Vec<_> = indices
                .iter()
                .map(|&index| schema.field(index).clone())
                .collect();
            Ok((Arc::new(Schema::new(fields)), Projection::Indices(names)))
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
) -> DfResult<Arc<dyn ExecutionPlan>> {
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

/// Merges one hash partition's partials into traces.
struct TracesExec {
    input: Arc<dyn ExecutionPlan>,
    properties: Arc<PlanProperties>,
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
        // step with the child so the number of execute() partitions matches.
        let partitions = this
            .input
            .properties()
            .output_partitioning()
            .partition_count();
        this.properties = Arc::new(PlanProperties::new(
            EquivalenceProperties::new(Arc::clone(&this.output_schema)),
            Partitioning::UnknownPartitioning(partitions),
            EmissionType::Incremental,
            Boundedness::Bounded,
        ));
        Ok(Arc::new(this))
    }

    fn input_distribution_requirements(&self) -> InputDistributionRequirements {
        InputDistributionRequirements::new(vec![Distribution::KeyPartitioned(self.key.clone())])
    }

    fn execute(
        &self,
        partition: usize,
        context: Arc<TaskContext>,
    ) -> datafusion::error::Result<SendableRecordBatchStream> {
        let input = self.input.execute(partition, Arc::clone(&context))?;
        let reservation =
            MemoryConsumer::new(format!("TracesExec[{partition}]")).register(context.memory_pool());
        let batch_size = context.session_config().batch_size();
        let stream = MergeStream {
            input,
            merger: Some(TraceMerger::new(self.merge_columns.clone())),
            reservation,
            accounted: 0,
            predicate: self.predicate.clone(),
            bounds: self.bounds,
            projection: self.projection.clone(),
            output_schema: Arc::clone(&self.output_schema),
            batch_size,
            limit: self.limit,
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

struct MergeStream {
    input: SendableRecordBatchStream,
    merger: Option<TraceMerger>,
    reservation: MemoryReservation,
    accounted: usize,
    predicate: Option<Arc<dyn PhysicalExpr>>,
    bounds: TimeBounds,
    projection: Projection,
    output_schema: SchemaRef,
    batch_size: usize,
    limit: Option<usize>,
    outstanding: VecDeque<RecordBatch>,
    done: bool,
}

impl MergeStream {
    fn push_batch(&mut self, batch: &RecordBatch) -> datafusion::error::Result<()> {
        let merger = self
            .merger
            .as_mut()
            .ok_or_else(|| DataFusionError::Internal("merge already finished".into()))?;
        merger.push(batch).map_err(partial_err)?;
        let size = merger.size();
        let grown = size.saturating_sub(self.accounted);
        if grown > 0 {
            // A failed grow is DataFusion's ResourcesExhausted, which A2 maps
            // to query_too_large. The reservation frees itself on drop.
            self.reservation.try_grow(grown)?;
            self.accounted = size;
        }
        Ok(())
    }

    fn finish(&mut self) -> datafusion::error::Result<()> {
        let merger = self
            .merger
            .take()
            .ok_or_else(|| DataFusionError::Internal("merge already finished".into()))?;
        let merged = merger.finish().map_err(partial_err)?;
        let mask = self.keep_mask(&merged)?;
        let filtered = filter_record_batch(&merged, &mask)?;
        let rows = match self.limit {
            Some(limit) => filtered.num_rows().min(limit),
            None => filtered.num_rows(),
        };
        let projected = self.project(&filtered.slice(0, rows))?;
        let total = projected.num_rows();
        let mut offset = 0;
        while offset < total {
            let length = self.batch_size.min(total - offset);
            self.outstanding.push_back(projected.slice(offset, length));
            offset += length;
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
                    if let Err(error) = this.finish() {
                        return Poll::Ready(Some(Err(error)));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
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

        let query = storage.query(project(), Default::default()).unwrap();
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
            .unwrap();
        let ctx = session(&query);
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

        let query = storage.query(project(), Default::default()).unwrap();
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
        let query = storage.query(project(), Default::default()).unwrap();
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

        let query = storage.query(project(), Default::default()).unwrap();
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
            let query = storage.query(project(), options).unwrap();
            let ctx = session(&query);
            ctx.register_table("traces", Arc::clone(&query.traces))
                .unwrap();
            let sql = format!("SELECT * FROM traces WHERE {}", eq(t1));
            let batches = collect(&ctx, &sql).await;
            assert_eq!(batches.iter().map(RecordBatch::num_rows).sum::<usize>(), 1);
        }

        let query = storage.query(project(), Default::default()).unwrap();
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

        let query = storage.query(project(), Default::default()).unwrap();
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

        let query = storage.query(project(), Default::default()).unwrap();
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
        let q1 = s1.query(project(), Default::default()).unwrap();
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
        let q2 = s2.query(project(), Default::default()).unwrap();
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

        let query = storage.query(project(), Default::default()).unwrap();
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
        let query = storage.query(project(), Default::default()).unwrap();
        let after = concat(
            &collect_off(&query, "SELECT * FROM traces").await,
            traces_schema(),
        );
        assert_eq!(after.num_rows(), 1);
        assert_eq!(int64_column(&after, traces::SPAN_COUNT), vec![2]);
        drop(commit);
        storage.shutdown().await.unwrap();
    }

    /// Group 10 (Memory): a 1 MiB pool fails a 50,000-trace full scan with
    /// DataFusion's ResourcesExhausted, not an allocation abort.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_small_memory_pool_fails_with_resources_exhausted() {
        use datafusion::execution::memory_pool::GreedyMemoryPool;
        use datafusion::execution::runtime_env::RuntimeEnvBuilder;

        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        let mut spans = Vec::with_capacity(50_000);
        for index in 0..50_000_u32 {
            let mut bytes = [0_u8; 16];
            bytes[12..].copy_from_slice(&(index + 1).to_be_bytes());
            let trace = TraceId::from_bytes(bytes).unwrap();
            spans.push(base_span(
                trace,
                span_id(1),
                20_000 * DAY + i64::from(index),
            ));
        }
        storage
            .append_spans(project(), spans_batch(&spans))
            .await
            .unwrap();
        let storage = reopen(dir.path(), storage).await;

        let pool = Arc::new(GreedyMemoryPool::new(1024 * 1024));
        let runtime = RuntimeEnvBuilder::new()
            .with_memory_pool(pool)
            .build()
            .unwrap();
        crate::objects::Objects::new(&config_in(dir.path()), Hooks::default())
            .await
            .unwrap()
            .register(&runtime);
        let mut query = storage.query(project(), Default::default()).unwrap();
        query.runtime = Arc::new(runtime);
        let ctx = session(&query);
        ctx.register_table("traces", Arc::clone(&query.traces))
            .unwrap();

        let error = ctx
            .sql("SELECT * FROM traces")
            .await
            .unwrap()
            .collect()
            .await
            .unwrap_err();
        let text = error.to_string();
        assert!(
            matches!(
                &error,
                datafusion::common::DataFusionError::ResourcesExhausted(_)
            ) || text.to_lowercase().contains("exhaust"),
            "expected ResourcesExhausted, got: {text}"
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

        let query = storage.query(project(), Default::default()).unwrap();
        let ctx = session(&query);
        ctx.register_table("traces", Arc::clone(&query.traces))
            .unwrap();
        assert_eq!(
            rows(&collect(&ctx, "SELECT * FROM traces LIMIT 2").await),
            2
        );
        assert_eq!(rows(&collect(&ctx, "SELECT * FROM traces").await), 3);

        let explained = collect(&ctx, "EXPLAIN SELECT * FROM traces LIMIT 2").await;
        let line = plan_text(&explained)
            .lines()
            .find(|line| line.contains("TracesExec:"))
            .unwrap()
            .to_owned();
        assert!(line.contains("mode=range"), "{line}");
        storage.shutdown().await.unwrap();
    }
}
