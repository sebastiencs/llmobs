//! In-memory tables built from `fixtures/api/dataset` for query tests.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use arrow::array::{
    Array, ArrayRef, BooleanBuilder, FixedSizeBinaryBuilder, Float64Builder, Int32Builder,
    Int64Builder, ListBuilder, MapBuilder, RecordBatch, StringBuilder, TimestampNanosecondBuilder,
};
use arrow::datatypes::{DataType, Field, SchemaRef};
use async_trait::async_trait;
use bytes::Bytes;
use datafusion::catalog::Session;
use datafusion::datasource::memory::MemTable;
use datafusion::datasource::{TableProvider, TableType};
use datafusion::error::Result as DfResult;
use datafusion::execution::runtime_env::RuntimeEnv;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown};
use datafusion::physical_plan::ExecutionPlan;
use llmobs_core::batch::{scores_to_record_batch, spans_to_record_batch};
use llmobs_core::enums::{SpanKind, SpanStatus};
use llmobs_core::ids::TraceId;
use llmobs_core::schema::traces_schema;
use llmobs_core::score::Score;
use llmobs_core::span::Span;

use crate::views::{ProjectQuery, QueryContext};

fn dataset_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/api/dataset")
}

/// The test trace id of [`span_row`] and [`score_row`]: the 2 big-endian bytes
/// of `trace + 1`, repeated 8 times. The `+ 1` keeps the id from being all
/// zero, which the id type rejects.
fn trace_id_of(trace: u16) -> TraceId {
    assert!(trace < u16::MAX, "trace must be below u16::MAX");
    let mut trace_bytes = [0_u8; 16];
    let pair = (trace + 1).to_be_bytes();
    for chunk in trace_bytes.as_chunks_mut::<2>().0 {
        chunk.copy_from_slice(&pair);
    }
    TraceId::from_bytes(trace_bytes).unwrap()
}

fn read_text(name: &str) -> String {
    let path = dataset_dir().join(name);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The rows stored in one catalogued file's Parquet object, under `objects/`.
pub(crate) fn object_rows(dir: &std::path::Path, file: &crate::catalog::FileMeta) -> RecordBatch {
    let bytes = fs::read(dir.join("objects").join(&file.path)).unwrap();
    let builder =
        parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes))
            .unwrap();
    let schema = Arc::clone(builder.schema());
    let batches: Vec<RecordBatch> = builder
        .build()
        .unwrap()
        .map(|batch| batch.unwrap())
        .collect();
    arrow::compute::concat_batches(&schema, &batches).unwrap()
}

/// The `trace_id` bytes of every row of a stored batch.
pub(crate) fn trace_ids_of(rows: &RecordBatch) -> Vec<[u8; 16]> {
    let column = rows
        .column_by_name(llmobs_core::schema::spans::TRACE_ID)
        .unwrap();
    let array = column
        .as_any()
        .downcast_ref::<arrow::array::FixedSizeBinaryArray>()
        .unwrap();
    (0..array.len())
        .map(|row| <[u8; 16]>::try_from(array.value(row)).unwrap())
        .collect()
}

pub(crate) fn dataset_spans() -> Vec<Span> {
    serde_json::from_str(&read_text("spans.json"))
        .unwrap_or_else(|error| panic!("spans.json: {error}"))
}

pub(crate) fn dataset_scores() -> Vec<Score> {
    serde_json::from_str(&read_text("scores.json"))
        .unwrap_or_else(|error| panic!("scores.json: {error}"))
}

fn mem_table(schema: SchemaRef, batch: RecordBatch) -> Arc<MemTable> {
    Arc::new(
        MemTable::try_new(schema, vec![vec![batch]])
            .unwrap_or_else(|error| panic!("memtable: {error}")),
    )
}

pub(crate) fn spans_table(spans: &[Span]) -> Arc<MemTable> {
    let batch = spans_to_record_batch(spans).unwrap_or_else(|error| panic!("spans batch: {error}"));
    mem_table(batch.schema(), batch)
}

/// Last writer wins per `score_id` by `(updated_at, ingest_time)`. Deleted winners are dropped.
pub(crate) fn live_scores(scores: &[Score]) -> Vec<Score> {
    let mut latest = BTreeMap::new();
    for score in scores {
        let replace = latest.get(&score.score_id).is_none_or(|current: &Score| {
            (score.updated_at, score.ingest_time) > (current.updated_at, current.ingest_time)
        });
        if replace {
            latest.insert(score.score_id, score.clone());
        }
    }
    latest
        .into_values()
        .filter(|score| !score.deleted)
        .collect()
}

pub(crate) fn live_scores_table(scores: &[Score]) -> Arc<MemTable> {
    let live = live_scores(scores);
    let batch =
        scores_to_record_batch(&live).unwrap_or_else(|error| panic!("scores batch: {error}"));
    mem_table(batch.schema(), batch)
}

pub(crate) fn traces_table(spans: &[Span]) -> Arc<MemTable> {
    let batch = trace_batch(spans);
    assert_eq!(batch.schema().as_ref(), traces_schema().as_ref());
    mem_table(traces_schema(), batch)
}

/// One span of the fixed test project, with the other fields as `wal::spans_keyed`
/// sets them.
///
/// `trace_id` is the 2 big-endian bytes of `trace + 1`, repeated 8 times;
/// `span_id` is 8 bytes of `span + 1`. The `+ 1` keeps the ids from being all
/// zero, which the id types reject. `trace` must be below `u16::MAX` and `span`
/// below `u8::MAX`; the builder panics otherwise. A test that needs another
/// field (`parent_span_id`, `input`) sets it on the returned `Span`.
pub(crate) fn span_row(trace: u16, span: u8, start: i64, ingest: i64) -> Span {
    assert!(span < u8::MAX, "span must be below u8::MAX");
    let project_id: llmobs_core::ids::ProjectId =
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap();
    let span_bytes = [span + 1; 8];
    Span {
        project_id,
        trace_id: trace_id_of(trace),
        span_id: llmobs_core::ids::SpanId::from_bytes(span_bytes).unwrap(),
        parent_span_id: None,
        name: "root".to_owned(),
        span_kind: SpanKind::Span,
        start_time: start,
        end_time: start.saturating_add(100),
        status: SpanStatus::Ok,
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
        ingest_time: ingest,
        schema_version: 1,
    }
}

/// `spans_to_record_batch(spans)`, unwrapped.
pub(crate) fn span_rows(spans: &[Span]) -> RecordBatch {
    spans_to_record_batch(spans).unwrap_or_else(|error| panic!("spans batch: {error}"))
}

/// One score of the fixed test project on the given trace, the score twin of
/// [`span_row`]: `trace_id` is the 2 big-endian bytes of `trace + 1`, repeated.
/// A test that needs `session_id`, `user_id` or another target sets it on the
/// returned `Score`; a session score has `target: ScoreTarget::Session { .. }`
/// and no trace id.
pub(crate) fn score_row(
    id: u128,
    trace: u16,
    created_at: i64,
    updated_at: i64,
    ingest: i64,
    deleted: bool,
    name: &str,
) -> Score {
    Score {
        project_id: "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap(),
        score_id: uuid::Uuid::from_u128(id),
        target: llmobs_core::score::ScoreTarget::Trace {
            trace_id: trace_id_of(trace),
        },
        session_id: None,
        user_id: None,
        name: name.to_owned(),
        value: llmobs_core::score::ScoreValue::Numeric(0.5),
        reason: None,
        source: llmobs_core::enums::ScoreSource::Api,
        evaluator_id: None,
        rule_id: None,
        experiment_id: None,
        dataset_item_id: None,
        author_user_id: None,
        metadata: BTreeMap::new(),
        created_at,
        updated_at,
        deleted,
        ingest_time: ingest,
    }
}

/// `scores_to_record_batch(scores)`, unwrapped.
pub(crate) fn score_rows(scores: &[Score]) -> RecordBatch {
    scores_to_record_batch(scores).unwrap_or_else(|error| panic!("scores batch: {error}"))
}

pub(crate) fn example_query() -> ProjectQuery {
    let spans = dataset_spans();
    let scores = dataset_scores();
    ProjectQuery {
        spans: spans_table(&spans),
        traces: traces_table(&spans),
        scores: live_scores_table(&scores),
        runtime: Arc::new(RuntimeEnv::default()),
        target_partitions: NonZeroUsize::new(2).expect("two partitions"),
        optimizer_rules: Vec::new(),
    }
}

pub(crate) async fn rows(ctx: &QueryContext, sql: &str) -> Vec<RecordBatch> {
    ctx.session()
        .sql(sql)
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .collect()
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
}

/// Wraps a [`MemTable`], answers `Inexact` for every filter, and records each filter's display.
#[derive(Debug)]
pub(crate) struct RecordingTable {
    inner: Arc<MemTable>,
    filters: Arc<Mutex<Vec<String>>>,
}

impl RecordingTable {
    pub(crate) fn new(inner: Arc<MemTable>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            filters: Arc::new(Mutex::new(Vec::new())),
        })
    }

    pub(crate) fn provider(self: &Arc<Self>) -> Arc<dyn TableProvider> {
        Arc::clone(self) as Arc<dyn TableProvider>
    }

    pub(crate) fn filters(&self) -> Vec<String> {
        self.filters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn clear(&self) {
        self.filters
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }
}

#[async_trait]
impl TableProvider for RecordingTable {
    fn schema(&self) -> SchemaRef {
        self.inner.schema()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> DfResult<Arc<dyn ExecutionPlan>> {
        {
            let mut recorded = self
                .filters
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            recorded.extend(filters.iter().map(|filter| filter.to_string()));
        }
        self.inner.scan(state, projection, filters, limit).await
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> DfResult<Vec<TableProviderFilterPushDown>> {
        Ok(vec![TableProviderFilterPushDown::Inexact; filters.len()])
    }
}

pub(crate) fn trace_batch(spans: &[Span]) -> RecordBatch {
    let mut grouped: BTreeMap<TraceId, Vec<&Span>> = BTreeMap::new();
    for span in spans {
        grouped.entry(span.trace_id).or_default().push(span);
    }
    let rows = grouped.len();
    let mut columns = TraceColumns::new(rows);
    for (trace_id, group) in grouped {
        columns.append(trace_id, &group);
    }
    RecordBatch::try_new(traces_schema(), columns.finish())
        .unwrap_or_else(|error| panic!("trace batch: {error}"))
}

struct TraceColumns {
    project_id: StringBuilder,
    trace_id: FixedSizeBinaryBuilder,
    start_time: TimestampNanosecondBuilder,
    end_time: TimestampNanosecondBuilder,
    name: StringBuilder,
    root_span_id: FixedSizeBinaryBuilder,
    has_root: BooleanBuilder,
    status: StringBuilder,
    session_id: StringBuilder,
    user_id: StringBuilder,
    environment: StringBuilder,
    release: StringBuilder,
    tags: ListBuilder<StringBuilder>,
    metadata: MapBuilder<StringBuilder, StringBuilder>,
    input_preview: StringBuilder,
    output_preview: StringBuilder,
    span_count: Int64Builder,
    error_count: Int64Builder,
    llm_count: Int64Builder,
    input_tokens: Int64Builder,
    output_tokens: Int64Builder,
    cache_read_tokens: Int64Builder,
    cache_write_tokens: Int64Builder,
    reasoning_tokens: Int64Builder,
    total_tokens: Int64Builder,
    cost_total: Float64Builder,
    models: ListBuilder<StringBuilder>,
    last_ingest_time: TimestampNanosecondBuilder,
    partial_count: Int32Builder,
}

impl TraceColumns {
    fn new(rows: usize) -> Self {
        Self {
            project_id: StringBuilder::with_capacity(rows, rows * 36),
            trace_id: FixedSizeBinaryBuilder::with_capacity(rows, 16),
            start_time: timestamp(rows),
            end_time: timestamp(rows),
            name: StringBuilder::new(),
            root_span_id: FixedSizeBinaryBuilder::with_capacity(rows, 8),
            has_root: BooleanBuilder::with_capacity(rows),
            status: StringBuilder::new(),
            session_id: StringBuilder::new(),
            user_id: StringBuilder::new(),
            environment: StringBuilder::new(),
            release: StringBuilder::new(),
            tags: utf8_list(rows),
            metadata: MapBuilder::with_capacity(
                None,
                StringBuilder::new(),
                StringBuilder::new(),
                rows,
            ),
            input_preview: StringBuilder::new(),
            output_preview: StringBuilder::new(),
            span_count: Int64Builder::with_capacity(rows),
            error_count: Int64Builder::with_capacity(rows),
            llm_count: Int64Builder::with_capacity(rows),
            input_tokens: Int64Builder::with_capacity(rows),
            output_tokens: Int64Builder::with_capacity(rows),
            cache_read_tokens: Int64Builder::with_capacity(rows),
            cache_write_tokens: Int64Builder::with_capacity(rows),
            reasoning_tokens: Int64Builder::with_capacity(rows),
            total_tokens: Int64Builder::with_capacity(rows),
            cost_total: Float64Builder::with_capacity(rows),
            models: utf8_list(rows),
            last_ingest_time: timestamp(rows),
            partial_count: Int32Builder::with_capacity(rows),
        }
    }

    fn append(&mut self, trace_id: TraceId, spans: &[&Span]) {
        let mut ordered = spans.to_vec();
        ordered.sort_by_key(|span| (span.start_time, span.span_id));
        let root = ordered
            .iter()
            .copied()
            .filter(|span| span.parent_span_id.is_none())
            .min_by_key(|span| (span.start_time, span.span_id));
        let earliest = ordered[0];
        let start_time = ordered
            .iter()
            .map(|span| span.start_time)
            .min()
            .unwrap_or(0);
        let end_time = ordered
            .iter()
            .map(|span| span.end_time)
            .max()
            .unwrap_or(start_time);
        let status = if ordered.iter().any(|span| span.status == SpanStatus::Error) {
            SpanStatus::Error
        } else {
            root.map(|span| span.status).unwrap_or_default()
        };
        let mut tags = BTreeSet::new();
        let mut metadata = root.map(|span| span.metadata.clone()).unwrap_or_default();
        let mut models = BTreeSet::new();
        for span in &ordered {
            tags.extend(span.tags.iter().cloned());
            if root.is_none_or(|root| root.span_id != span.span_id) {
                for (key, value) in &span.metadata {
                    metadata.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
            if let Some(model) = span.response_model.clone().or(span.request_model.clone()) {
                models.insert(model);
            }
        }
        self.project_id
            .append_value(earliest.project_id.to_string());
        self.trace_id
            .append_value(trace_id.as_bytes())
            .unwrap_or_else(|error| panic!("trace id: {error}"));
        self.start_time.append_value(start_time);
        self.end_time.append_value(end_time);
        self.name.append_value(
            root.map(|span| span.name.as_str())
                .unwrap_or(earliest.name.as_str()),
        );
        match root {
            Some(span) => self
                .root_span_id
                .append_value(span.span_id.as_bytes())
                .unwrap_or_else(|error| panic!("root span id: {error}")),
            None => self.root_span_id.append_null(),
        }
        self.has_root.append_value(root.is_some());
        self.status.append_value(status.as_str());
        append_opt(
            &mut self.session_id,
            first_set(root, &ordered, |span| &span.session_id),
        );
        append_opt(
            &mut self.user_id,
            first_set(root, &ordered, |span| &span.user_id),
        );
        append_opt(
            &mut self.environment,
            first_set(root, &ordered, |span| &span.environment),
        );
        append_opt(
            &mut self.release,
            first_set(root, &ordered, |span| &span.release),
        );
        for tag in &tags {
            self.tags.values().append_value(tag);
        }
        self.tags.append(true);
        for (key, value) in &metadata {
            self.metadata.keys().append_value(key);
            self.metadata.values().append_value(value);
        }
        self.metadata
            .append(true)
            .unwrap_or_else(|error| panic!("metadata: {error}"));
        append_opt(
            &mut self.input_preview,
            preview(root, &ordered, |span| &span.input_preview),
        );
        append_opt(
            &mut self.output_preview,
            preview(root, &ordered, |span| &span.output_preview),
        );
        self.span_count.append_value(ordered.len() as i64);
        self.error_count.append_value(
            ordered
                .iter()
                .filter(|span| span.status == SpanStatus::Error)
                .count() as i64,
        );
        self.llm_count.append_value(
            ordered
                .iter()
                .filter(|span| span.span_kind == SpanKind::Llm)
                .count() as i64,
        );
        self.input_tokens
            .append_option(sum_i64(ordered.iter().map(|span| span.input_tokens)));
        self.output_tokens
            .append_option(sum_i64(ordered.iter().map(|span| span.output_tokens)));
        self.cache_read_tokens
            .append_option(sum_i64(ordered.iter().map(|span| span.cache_read_tokens)));
        self.cache_write_tokens
            .append_option(sum_i64(ordered.iter().map(|span| span.cache_write_tokens)));
        self.reasoning_tokens
            .append_option(sum_i64(ordered.iter().map(|span| span.reasoning_tokens)));
        self.total_tokens
            .append_option(sum_i64(ordered.iter().map(|span| span.total_tokens)));
        self.cost_total
            .append_option(sum_f64(ordered.iter().map(|span| span.cost_total)));
        for model in &models {
            self.models.values().append_value(model);
        }
        self.models.append(true);
        let last_ingest = ordered
            .iter()
            .map(|span| span.ingest_time)
            .max()
            .unwrap_or(0);
        self.last_ingest_time.append_value(last_ingest);
        self.partial_count.append_value(1);
    }

    fn finish(mut self) -> Vec<ArrayRef> {
        vec![
            Arc::new(self.project_id.finish()),
            Arc::new(self.trace_id.finish()),
            Arc::new(self.start_time.finish()),
            Arc::new(self.end_time.finish()),
            Arc::new(self.name.finish()),
            Arc::new(self.root_span_id.finish()),
            Arc::new(self.has_root.finish()),
            Arc::new(self.status.finish()),
            Arc::new(self.session_id.finish()),
            Arc::new(self.user_id.finish()),
            Arc::new(self.environment.finish()),
            Arc::new(self.release.finish()),
            Arc::new(self.tags.finish()),
            Arc::new(self.metadata.finish()),
            Arc::new(self.input_preview.finish()),
            Arc::new(self.output_preview.finish()),
            Arc::new(self.span_count.finish()),
            Arc::new(self.error_count.finish()),
            Arc::new(self.llm_count.finish()),
            Arc::new(self.input_tokens.finish()),
            Arc::new(self.output_tokens.finish()),
            Arc::new(self.cache_read_tokens.finish()),
            Arc::new(self.cache_write_tokens.finish()),
            Arc::new(self.reasoning_tokens.finish()),
            Arc::new(self.total_tokens.finish()),
            Arc::new(self.cost_total.finish()),
            Arc::new(self.models.finish()),
            Arc::new(self.last_ingest_time.finish()),
            Arc::new(self.partial_count.finish()),
        ]
    }
}

fn timestamp(rows: usize) -> TimestampNanosecondBuilder {
    TimestampNanosecondBuilder::with_capacity(rows).with_timezone("UTC")
}

fn utf8_list(rows: usize) -> ListBuilder<StringBuilder> {
    ListBuilder::with_capacity(StringBuilder::new(), rows).with_field(Arc::new(Field::new(
        "item",
        DataType::Utf8,
        false,
    )))
}

fn append_opt(builder: &mut StringBuilder, value: Option<String>) {
    match value {
        Some(value) => builder.append_value(value),
        None => builder.append_null(),
    }
}

fn first_set(
    root: Option<&Span>,
    ordered: &[&Span],
    field: impl Fn(&Span) -> &Option<String>,
) -> Option<String> {
    if let Some(value) = root.and_then(|span| field(span).clone()) {
        return Some(value);
    }
    ordered.iter().find_map(|span| field(span).clone())
}

fn preview(
    root: Option<&Span>,
    ordered: &[&Span],
    field: impl Fn(&Span) -> &Option<String>,
) -> Option<String> {
    if let Some(value) = root.and_then(|span| field(span).clone()) {
        return Some(value);
    }
    ordered
        .iter()
        .filter(|span| span.span_kind == SpanKind::Llm)
        .find_map(|span| field(span).clone())
}

fn sum_i64(values: impl Iterator<Item = Option<i64>>) -> Option<i64> {
    let mut total = None;
    for value in values.flatten() {
        *total.get_or_insert(0) += value;
    }
    total
}

fn sum_f64(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    let mut total = None;
    for value in values.flatten() {
        *total.get_or_insert(0.0) += value;
    }
    total
}

mod fault_store {
    use std::collections::VecDeque;
    use std::fmt;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, PoisonError};
    use std::time::Duration;

    use async_trait::async_trait;
    use futures::stream::BoxStream;
    use object_store::path::Path;
    use object_store::{
        CopyOptions, GetOptions, GetResult, ListResult, MultipartUpload, ObjectMeta, ObjectStore,
        PutMultipartOptions, PutOptions, PutPayload, PutResult, Result, UploadPart,
    };

    /// One injected failure for the next `put` or multipart upload.
    #[derive(Debug, Clone)]
    pub(crate) enum FaultKind {
        /// Return an error and write nothing.
        FailBefore,
        /// Write, then return an error. The object may exist.
        FailAfter,
        /// Wait, then forward the call.
        Delay(Duration),
    }

    /// [`ObjectStore`] that can fail or delay its next upload.
    ///
    /// This is a second implementation of the external trait so the remote
    /// `put_durable` path runs in CI without AWS (D119).
    #[derive(Debug, Clone)]
    pub(crate) struct FaultStore {
        inner: Arc<dyn ObjectStore>,
        fault: Arc<Mutex<VecDeque<FaultKind>>>,
        /// When set, every upload fails before writing and the queue is left alone.
        hold: Arc<AtomicBool>,
        /// When set, every read (`get_opts`, and so `head` and range reads) fails.
        fail_reads: Arc<AtomicBool>,
    }

    impl FaultStore {
        pub(crate) fn new(inner: Arc<dyn ObjectStore>) -> Self {
            Self {
                inner,
                fault: Arc::new(Mutex::new(VecDeque::new())),
                hold: Arc::new(AtomicBool::new(false)),
                fail_reads: Arc::new(AtomicBool::new(false)),
            }
        }

        /// Makes every read fail while `on` is set.
        pub(crate) fn fail_reads(&self, on: bool) {
            self.fail_reads.store(on, Ordering::SeqCst);
        }

        /// Replaces the queue with one fault. A later upload consumes it.
        pub(crate) fn arm(&self, kind: FaultKind) {
            let mut queue = self.fault.lock().unwrap_or_else(PoisonError::into_inner);
            queue.clear();
            queue.push_back(kind);
        }

        /// Appends a fault. Uploads consume the queue from the front.
        pub(crate) fn enqueue(&self, kind: FaultKind) {
            self.fault
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push_back(kind);
        }

        /// Fail every upload before it writes, until [`Self::release`].
        pub(crate) fn hold_failures(&self) {
            self.hold.store(true, Ordering::SeqCst);
        }

        /// Lets uploads reach the inner store again.
        pub(crate) fn release(&self) {
            self.hold.store(false, Ordering::SeqCst);
        }

        fn take(&self) -> Option<FaultKind> {
            if self.hold.load(Ordering::SeqCst) {
                return Some(FaultKind::FailBefore);
            }
            self.fault
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .pop_front()
        }
    }

    impl fmt::Display for FaultStore {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "FaultStore({})", self.inner.as_ref())
        }
    }

    fn injected() -> object_store::Error {
        object_store::Error::Generic {
            store: "fault",
            source: Box::new(std::io::Error::other("injected fault")),
        }
    }

    struct FaultMultipart {
        inner: Box<dyn MultipartUpload>,
        fail_after: bool,
    }

    impl fmt::Debug for FaultMultipart {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("FaultMultipart")
                .field("fail_after", &self.fail_after)
                .finish_non_exhaustive()
        }
    }

    #[async_trait]
    impl MultipartUpload for FaultMultipart {
        fn put_part(&mut self, data: PutPayload) -> UploadPart {
            self.inner.put_part(data)
        }

        async fn complete(&mut self) -> Result<PutResult> {
            let result = self.inner.complete().await?;
            if self.fail_after {
                return Err(injected());
            }
            Ok(result)
        }

        async fn abort(&mut self) -> Result<()> {
            self.inner.abort().await
        }
    }

    #[async_trait]
    impl ObjectStore for FaultStore {
        async fn put_opts(
            &self,
            location: &Path,
            payload: PutPayload,
            opts: PutOptions,
        ) -> Result<PutResult> {
            match self.take() {
                Some(FaultKind::FailBefore) => Err(injected()),
                Some(FaultKind::FailAfter) => {
                    self.inner.put_opts(location, payload, opts).await?;
                    Err(injected())
                }
                Some(FaultKind::Delay(duration)) => {
                    tokio::time::sleep(duration).await;
                    self.inner.put_opts(location, payload, opts).await
                }
                None => self.inner.put_opts(location, payload, opts).await,
            }
        }

        async fn put_multipart_opts(
            &self,
            location: &Path,
            opts: PutMultipartOptions,
        ) -> Result<Box<dyn MultipartUpload>> {
            match self.take() {
                Some(FaultKind::FailBefore) => Err(injected()),
                Some(FaultKind::FailAfter) => {
                    let inner = self.inner.put_multipart_opts(location, opts).await?;
                    Ok(Box::new(FaultMultipart {
                        inner,
                        fail_after: true,
                    }))
                }
                Some(FaultKind::Delay(duration)) => {
                    tokio::time::sleep(duration).await;
                    self.inner.put_multipart_opts(location, opts).await
                }
                None => self.inner.put_multipart_opts(location, opts).await,
            }
        }

        async fn get_opts(&self, location: &Path, options: GetOptions) -> Result<GetResult> {
            if self.fail_reads.load(Ordering::SeqCst) {
                return Err(injected());
            }
            self.inner.get_opts(location, options).await
        }

        fn delete_stream(
            &self,
            locations: BoxStream<'static, Result<Path>>,
        ) -> BoxStream<'static, Result<Path>> {
            self.inner.delete_stream(locations)
        }

        fn list(&self, prefix: Option<&Path>) -> BoxStream<'static, Result<ObjectMeta>> {
            self.inner.list(prefix)
        }

        async fn list_with_delimiter(&self, prefix: Option<&Path>) -> Result<ListResult> {
            self.inner.list_with_delimiter(prefix).await
        }

        async fn copy_opts(&self, from: &Path, to: &Path, options: CopyOptions) -> Result<()> {
            self.inner.copy_opts(from, to, options).await
        }
    }
}

pub(crate) use fault_store::{FaultKind, FaultStore};
