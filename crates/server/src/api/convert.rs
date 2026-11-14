//! Arrow rows → API types (D-G-14).
//!
//! Columns are read by name. A column whose type differs from the one listed
//! in `docs/plans/g-spine/02-m2-read-api.md` is cast first. Strings may be
//! `Utf8View`, so every string read goes through the same cast.

use std::collections::{BTreeMap, HashMap};
use std::str::FromStr;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BooleanArray, FixedSizeBinaryArray, Float64Array, Int64Array, ListArray,
    MapArray, RecordBatch, StringArray, StructArray, TimestampNanosecondArray, UInt16Array,
};
use arrow::compute::cast;
use arrow::datatypes::{DataType, TimeUnit};
use llmobs_core::enums::{
    Convention, CostSource, PayloadFormat, ScoreDataType, ScoreSource, ScoreTargetKind, SpanKind,
    SpanStatus,
};
use llmobs_core::ids::{ProjectId, SpanId, TraceId};
use llmobs_core::schema::{event_field, link_field, scores, spans, traces};
use llmobs_core::score::{Score, ScoreTarget, ScoreValue};
use llmobs_core::span::{Span, SpanEvent, SpanLink};
use uuid::Uuid;

use super::metrics::{
    Kpi, KpiMetric, MetricName, MetricPoint, MetricSeries, MetricsGroupBy, MetricsResponse,
    ModelUsage,
};
use super::payload::Payload;
use super::schema::{ColumnSchema, ColumnType, FunctionSchema, SchemaResponse, ViewSchema};
use super::sessions::SessionSummary;
use super::traces::{ScoreBrief, SpanSummary, TraceSummary, stored_payload};
use super::users::EndUserSummary;
use crate::http::error::ApiError;

/// `duration_ns` of the traces view. The spans table has the same name.
const TRACE_DURATION_NS: &str = "duration_ns";
/// `target_key` of `latest_scores` (`LATEST_SCORE_COLUMNS`).
const TARGET_KEY: &str = "target_key";
/// `trace_count` of the sessions and users views.
const TRACE_COUNT: &str = "trace_count";
/// `session_count` of the users view.
const SESSION_COUNT: &str = "session_count";
/// `first_seen` of the users view.
const FIRST_SEEN: &str = "first_seen";
/// `last_seen` of the users view.
const LAST_SEEN: &str = "last_seen";

/// A stored row does not have the shape the API expects. The text names the column, never a value.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConvertError {
    #[error("column {0} is missing")]
    Missing(&'static str),
    #[error("column {0} has an unexpected type")]
    Type(&'static str),
    #[error("column {0} is null")]
    Null(&'static str),
    #[error("column {0} has an invalid value")]
    Value(&'static str),
}

impl From<ConvertError> for ApiError {
    fn from(error: ConvertError) -> Self {
        Self::Internal(Box::new(error))
    }
}

// ---- Column access -------------------------------------------------------

/// `column`, cast to `data_type` when its type differs.
fn typed(
    column: Option<&ArrayRef>,
    name: &'static str,
    data_type: &DataType,
) -> Result<ArrayRef, ConvertError> {
    let column = column.ok_or(ConvertError::Missing(name))?;
    if column.data_type() == data_type {
        return Ok(Arc::clone(column));
    }
    cast(column, data_type).map_err(|_| ConvertError::Type(name))
}

fn downcast<T: Array + Clone + 'static>(
    array: &ArrayRef,
    name: &'static str,
) -> Result<T, ConvertError> {
    array
        .as_any()
        .downcast_ref::<T>()
        .cloned()
        .ok_or(ConvertError::Type(name))
}

fn column(
    batch: &RecordBatch,
    name: &'static str,
    data_type: &DataType,
) -> Result<ArrayRef, ConvertError> {
    typed(batch.column_by_name(name), name, data_type)
}

fn strings(batch: &RecordBatch, name: &'static str) -> Result<StringArray, ConvertError> {
    downcast(&column(batch, name, &DataType::Utf8)?, name)
}

fn ints(batch: &RecordBatch, name: &'static str) -> Result<Int64Array, ConvertError> {
    downcast(&column(batch, name, &DataType::Int64)?, name)
}

fn floats(batch: &RecordBatch, name: &'static str) -> Result<Float64Array, ConvertError> {
    downcast(&column(batch, name, &DataType::Float64)?, name)
}

fn bools(batch: &RecordBatch, name: &'static str) -> Result<BooleanArray, ConvertError> {
    downcast(&column(batch, name, &DataType::Boolean)?, name)
}

fn times(
    batch: &RecordBatch,
    name: &'static str,
) -> Result<TimestampNanosecondArray, ConvertError> {
    let data_type = DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()));
    downcast(&column(batch, name, &data_type)?, name)
}

/// Lists and maps are downcast only. Their items are cast when read.
fn lists(batch: &RecordBatch, name: &'static str) -> Result<ListArray, ConvertError> {
    downcast(&existing(batch, name)?, name)
}

fn maps(batch: &RecordBatch, name: &'static str) -> Result<MapArray, ConvertError> {
    downcast(&existing(batch, name)?, name)
}

fn existing(batch: &RecordBatch, name: &'static str) -> Result<ArrayRef, ConvertError> {
    batch
        .column_by_name(name)
        .map(Arc::clone)
        .ok_or(ConvertError::Missing(name))
}

/// A struct child, cast to `data_type` when its type differs.
fn child(
    array: &StructArray,
    name: &'static str,
    data_type: &DataType,
) -> Result<ArrayRef, ConvertError> {
    typed(array.column_by_name(name), name, data_type)
}

// ---- Row values ----------------------------------------------------------

fn opt_string(array: &StringArray, row: usize) -> Option<String> {
    (!array.is_null(row)).then(|| array.value(row).to_owned())
}

fn req_string(array: &StringArray, row: usize, name: &'static str) -> Result<String, ConvertError> {
    opt_string(array, row).ok_or(ConvertError::Null(name))
}

fn opt_i64(array: &Int64Array, row: usize) -> Option<i64> {
    (!array.is_null(row)).then(|| array.value(row))
}

fn req_i64(array: &Int64Array, row: usize, name: &'static str) -> Result<i64, ConvertError> {
    opt_i64(array, row).ok_or(ConvertError::Null(name))
}

fn opt_f64(array: &Float64Array, row: usize) -> Option<f64> {
    (!array.is_null(row)).then(|| array.value(row))
}

fn req_bool(array: &BooleanArray, row: usize, name: &'static str) -> Result<bool, ConvertError> {
    (!array.is_null(row))
        .then(|| array.value(row))
        .ok_or(ConvertError::Null(name))
}

fn req_time(
    array: &TimestampNanosecondArray,
    row: usize,
    name: &'static str,
) -> Result<i64, ConvertError> {
    (!array.is_null(row))
        .then(|| array.value(row))
        .ok_or(ConvertError::Null(name))
}

/// A nullable string column parsed with `parse`. A parse failure is `Value(name)`.
fn opt_parsed<T, E>(
    array: &StringArray,
    row: usize,
    name: &'static str,
    parse: impl FnOnce(&str) -> Result<T, E>,
) -> Result<Option<T>, ConvertError> {
    if array.is_null(row) {
        return Ok(None);
    }
    parse(array.value(row))
        .map(Some)
        .map_err(|_| ConvertError::Value(name))
}

/// A required string column parsed with `parse`. Null is `Null(name)`; a parse failure is `Value(name)`.
fn req_parsed<T, E>(
    array: &StringArray,
    row: usize,
    name: &'static str,
    parse: impl FnOnce(&str) -> Result<T, E>,
) -> Result<T, ConvertError> {
    let text = (!array.is_null(row))
        .then(|| array.value(row))
        .ok_or(ConvertError::Null(name))?;
    parse(text).map_err(|_| ConvertError::Value(name))
}

/// A `List(Utf8)` row. A null list is empty, and null items are skipped.
fn list_strings(
    list: &ListArray,
    row: usize,
    name: &'static str,
) -> Result<Vec<String>, ConvertError> {
    if list.is_null(row) {
        return Ok(Vec::new());
    }
    let items = cast(&list.value(row), &DataType::Utf8).map_err(|_| ConvertError::Type(name))?;
    let items = items
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or(ConvertError::Type(name))?;
    Ok(items.iter().flatten().map(str::to_owned).collect())
}

/// A `Map(Utf8, Utf8)` row. A null map is empty, and an entry whose value is null is skipped.
fn map_strings(
    map: &MapArray,
    row: usize,
    name: &'static str,
) -> Result<BTreeMap<String, String>, ConvertError> {
    let mut out = BTreeMap::new();
    if map.is_null(row) {
        return Ok(out);
    }
    // Entries are `(key, value)` structs. Read them by position: Parquet writers name the
    // fields `key`/`value` or `keys`/`values`.
    let entries = map.value(row);
    let keys = typed(Some(entries.column(0)), name, &DataType::Utf8)?;
    let values = typed(Some(entries.column(1)), name, &DataType::Utf8)?;
    let keys = keys
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or(ConvertError::Type(name))?;
    let values = values
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or(ConvertError::Type(name))?;
    for index in 0..entries.len() {
        if values.is_null(index) {
            continue;
        }
        let key = req_string(keys, index, name)?;
        out.insert(key, values.value(index).to_owned());
    }
    Ok(out)
}

// ---- Converters ----------------------------------------------------------

/// Trace list rows, in row order across batches.
pub(crate) fn trace_summaries(batches: &[RecordBatch]) -> Result<Vec<TraceSummary>, ConvertError> {
    let mut out = Vec::new();
    for batch in batches {
        out.extend(trace_batch(batch)?);
    }
    Ok(out)
}

fn trace_batch(batch: &RecordBatch) -> Result<Vec<TraceSummary>, ConvertError> {
    let trace_id = strings(batch, traces::TRACE_ID)?;
    let root_span_id = strings(batch, traces::ROOT_SPAN_ID)?;
    let has_root = bools(batch, traces::HAS_ROOT)?;
    let name = strings(batch, traces::NAME)?;
    let start_time = times(batch, traces::START_TIME)?;
    let end_time = times(batch, traces::END_TIME)?;
    let duration_ns = ints(batch, TRACE_DURATION_NS)?;
    let status = strings(batch, traces::STATUS)?;
    let session_id = strings(batch, traces::SESSION_ID)?;
    let user_id = strings(batch, traces::USER_ID)?;
    let environment = strings(batch, traces::ENVIRONMENT)?;
    let release = strings(batch, traces::RELEASE)?;
    let tags = lists(batch, traces::TAGS)?;
    let metadata = maps(batch, traces::METADATA)?;
    let input_preview = strings(batch, traces::INPUT_PREVIEW)?;
    let output_preview = strings(batch, traces::OUTPUT_PREVIEW)?;
    let span_count = ints(batch, traces::SPAN_COUNT)?;
    let error_count = ints(batch, traces::ERROR_COUNT)?;
    let llm_count = ints(batch, traces::LLM_COUNT)?;
    let input_tokens = ints(batch, traces::INPUT_TOKENS)?;
    let output_tokens = ints(batch, traces::OUTPUT_TOKENS)?;
    let cache_read_tokens = ints(batch, traces::CACHE_READ_TOKENS)?;
    let cache_write_tokens = ints(batch, traces::CACHE_WRITE_TOKENS)?;
    let reasoning_tokens = ints(batch, traces::REASONING_TOKENS)?;
    let total_tokens = ints(batch, traces::TOTAL_TOKENS)?;
    let cost_total = floats(batch, traces::COST_TOTAL)?;
    let models = lists(batch, traces::MODELS)?;

    let mut out = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        out.push(TraceSummary {
            trace_id: req_parsed(&trace_id, row, traces::TRACE_ID, TraceId::from_str)?,
            root_span_id: opt_parsed(&root_span_id, row, traces::ROOT_SPAN_ID, SpanId::from_str)?,
            has_root: req_bool(&has_root, row, traces::HAS_ROOT)?,
            name: req_string(&name, row, traces::NAME)?,
            start_time: req_time(&start_time, row, traces::START_TIME)?,
            end_time: req_time(&end_time, row, traces::END_TIME)?,
            duration_ns: req_i64(&duration_ns, row, TRACE_DURATION_NS)?,
            status: req_parsed(&status, row, traces::STATUS, SpanStatus::parse)?,
            session_id: opt_string(&session_id, row),
            user_id: opt_string(&user_id, row),
            environment: opt_string(&environment, row),
            release: opt_string(&release, row),
            tags: list_strings(&tags, row, traces::TAGS)?,
            metadata: map_strings(&metadata, row, traces::METADATA)?,
            input_preview: opt_string(&input_preview, row),
            output_preview: opt_string(&output_preview, row),
            span_count: req_i64(&span_count, row, traces::SPAN_COUNT)?,
            error_count: req_i64(&error_count, row, traces::ERROR_COUNT)?,
            llm_count: req_i64(&llm_count, row, traces::LLM_COUNT)?,
            input_tokens: opt_i64(&input_tokens, row),
            output_tokens: opt_i64(&output_tokens, row),
            cache_read_tokens: opt_i64(&cache_read_tokens, row),
            cache_write_tokens: opt_i64(&cache_write_tokens, row),
            reasoning_tokens: opt_i64(&reasoning_tokens, row),
            total_tokens: opt_i64(&total_tokens, row),
            cost_total: opt_f64(&cost_total, row),
            models: list_strings(&models, row, traces::MODELS)?,
            scores: Vec::new(),
        });
    }
    Ok(out)
}

/// Span list rows, in row order across batches. Columns are `SPAN_LIST_COLUMNS`.
pub(crate) fn span_summaries(batches: &[RecordBatch]) -> Result<Vec<SpanSummary>, ConvertError> {
    let mut out = Vec::new();
    for batch in batches {
        out.extend(span_batch(batch)?);
    }
    Ok(out)
}

fn span_batch(batch: &RecordBatch) -> Result<Vec<SpanSummary>, ConvertError> {
    let trace_id = strings(batch, spans::TRACE_ID)?;
    let span_id = strings(batch, spans::SPAN_ID)?;
    let parent_span_id = strings(batch, spans::PARENT_SPAN_ID)?;
    let name = strings(batch, spans::NAME)?;
    let span_kind = strings(batch, spans::SPAN_KIND)?;
    let start_time = times(batch, spans::START_TIME)?;
    let end_time = times(batch, spans::END_TIME)?;
    let duration_ns = ints(batch, spans::DURATION_NS)?;
    let status = strings(batch, spans::STATUS)?;
    let status_message = strings(batch, spans::STATUS_MESSAGE)?;
    let service_name = strings(batch, spans::SERVICE_NAME)?;
    let session_id = strings(batch, spans::SESSION_ID)?;
    let user_id = strings(batch, spans::USER_ID)?;
    let environment = strings(batch, spans::ENVIRONMENT)?;
    let release = strings(batch, spans::RELEASE)?;
    let tags = lists(batch, spans::TAGS)?;
    let metadata = maps(batch, spans::METADATA)?;
    let provider = strings(batch, spans::PROVIDER)?;
    // Read only to compute `model`; the stored `model` column is not used.
    let request_model = strings(batch, spans::REQUEST_MODEL)?;
    let response_model = strings(batch, spans::RESPONSE_MODEL)?;
    let input_tokens = ints(batch, spans::INPUT_TOKENS)?;
    let output_tokens = ints(batch, spans::OUTPUT_TOKENS)?;
    let cache_read_tokens = ints(batch, spans::CACHE_READ_TOKENS)?;
    let cache_write_tokens = ints(batch, spans::CACHE_WRITE_TOKENS)?;
    let reasoning_tokens = ints(batch, spans::REASONING_TOKENS)?;
    let total_tokens = ints(batch, spans::TOTAL_TOKENS)?;
    let cost_total = floats(batch, spans::COST_TOTAL)?;
    let ttft_ns = ints(batch, spans::TTFT_NS)?;
    let input_preview = strings(batch, spans::INPUT_PREVIEW)?;
    let output_preview = strings(batch, spans::OUTPUT_PREVIEW)?;

    let mut out = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        out.push(SpanSummary {
            trace_id: req_parsed(&trace_id, row, spans::TRACE_ID, TraceId::from_str)?,
            span_id: req_parsed(&span_id, row, spans::SPAN_ID, SpanId::from_str)?,
            parent_span_id: opt_parsed(
                &parent_span_id,
                row,
                spans::PARENT_SPAN_ID,
                SpanId::from_str,
            )?,
            name: req_string(&name, row, spans::NAME)?,
            span_kind: req_parsed(&span_kind, row, spans::SPAN_KIND, SpanKind::parse)?,
            start_time: req_time(&start_time, row, spans::START_TIME)?,
            end_time: req_time(&end_time, row, spans::END_TIME)?,
            duration_ns: req_i64(&duration_ns, row, spans::DURATION_NS)?,
            status: req_parsed(&status, row, spans::STATUS, SpanStatus::parse)?,
            status_message: opt_string(&status_message, row),
            service_name: opt_string(&service_name, row),
            session_id: opt_string(&session_id, row),
            user_id: opt_string(&user_id, row),
            environment: opt_string(&environment, row),
            release: opt_string(&release, row),
            tags: list_strings(&tags, row, spans::TAGS)?,
            metadata: map_strings(&metadata, row, spans::METADATA)?,
            provider: opt_string(&provider, row),
            model: opt_string(&response_model, row).or_else(|| opt_string(&request_model, row)),
            input_tokens: opt_i64(&input_tokens, row),
            output_tokens: opt_i64(&output_tokens, row),
            cache_read_tokens: opt_i64(&cache_read_tokens, row),
            cache_write_tokens: opt_i64(&cache_write_tokens, row),
            reasoning_tokens: opt_i64(&reasoning_tokens, row),
            total_tokens: opt_i64(&total_tokens, row),
            cost_total: opt_f64(&cost_total, row),
            ttft_ns: opt_i64(&ttft_ns, row),
            input_preview: opt_string(&input_preview, row),
            output_preview: opt_string(&output_preview, row),
        });
    }
    Ok(out)
}

/// The columns of one batch, extracted once: a per-row extraction would re-cast every
/// column for every row (D-G-14).
struct SpanColumns {
    trace_id: StringArray,
    span_id: StringArray,
    parent_span_id: StringArray,
    name: StringArray,
    span_kind: StringArray,
    start_time: TimestampNanosecondArray,
    end_time: TimestampNanosecondArray,
    status: StringArray,
    status_message: StringArray,
    service_name: StringArray,
    session_id: StringArray,
    user_id: StringArray,
    environment: StringArray,
    release: StringArray,
    tags: ListArray,
    metadata: MapArray,
    provider: StringArray,
    request_model: StringArray,
    response_model: StringArray,
    operation: StringArray,
    input_tokens: Int64Array,
    output_tokens: Int64Array,
    cache_read_tokens: Int64Array,
    cache_write_tokens: Int64Array,
    reasoning_tokens: Int64Array,
    total_tokens: Int64Array,
    cost_input: Float64Array,
    cost_output: Float64Array,
    cost_total: Float64Array,
    cost_source: StringArray,
    ttft_ns: Int64Array,
    request_params: StringArray,
    input: StringArray,
    input_format: StringArray,
    output: StringArray,
    output_format: StringArray,
    input_preview: StringArray,
    output_preview: StringArray,
    system_instructions: StringArray,
    tool_definitions: StringArray,
    attributes: MapArray,
    resource_attributes: MapArray,
    scope_name: StringArray,
    scope_version: StringArray,
    events: ListArray,
    links: ListArray,
    media_refs: ListArray,
    truncated: ListArray,
    conventions: ListArray,
    ingest_time: TimestampNanosecondArray,
    schema_version: UInt16Array,
}

/// `batch`'s columns, read by name.
fn span_columns(batch: &RecordBatch) -> Result<SpanColumns, ConvertError> {
    Ok(SpanColumns {
        trace_id: strings(batch, spans::TRACE_ID)?,
        span_id: strings(batch, spans::SPAN_ID)?,
        parent_span_id: strings(batch, spans::PARENT_SPAN_ID)?,
        name: strings(batch, spans::NAME)?,
        span_kind: strings(batch, spans::SPAN_KIND)?,
        start_time: times(batch, spans::START_TIME)?,
        end_time: times(batch, spans::END_TIME)?,
        status: strings(batch, spans::STATUS)?,
        status_message: strings(batch, spans::STATUS_MESSAGE)?,
        service_name: strings(batch, spans::SERVICE_NAME)?,
        session_id: strings(batch, spans::SESSION_ID)?,
        user_id: strings(batch, spans::USER_ID)?,
        environment: strings(batch, spans::ENVIRONMENT)?,
        release: strings(batch, spans::RELEASE)?,
        tags: lists(batch, spans::TAGS)?,
        metadata: maps(batch, spans::METADATA)?,
        provider: strings(batch, spans::PROVIDER)?,
        request_model: strings(batch, spans::REQUEST_MODEL)?,
        response_model: strings(batch, spans::RESPONSE_MODEL)?,
        operation: strings(batch, spans::OPERATION)?,
        input_tokens: ints(batch, spans::INPUT_TOKENS)?,
        output_tokens: ints(batch, spans::OUTPUT_TOKENS)?,
        cache_read_tokens: ints(batch, spans::CACHE_READ_TOKENS)?,
        cache_write_tokens: ints(batch, spans::CACHE_WRITE_TOKENS)?,
        reasoning_tokens: ints(batch, spans::REASONING_TOKENS)?,
        total_tokens: ints(batch, spans::TOTAL_TOKENS)?,
        cost_input: floats(batch, spans::COST_INPUT)?,
        cost_output: floats(batch, spans::COST_OUTPUT)?,
        cost_total: floats(batch, spans::COST_TOTAL)?,
        cost_source: strings(batch, spans::COST_SOURCE)?,
        ttft_ns: ints(batch, spans::TTFT_NS)?,
        request_params: strings(batch, spans::REQUEST_PARAMS)?,
        input: strings(batch, spans::INPUT)?,
        input_format: strings(batch, spans::INPUT_FORMAT)?,
        output: strings(batch, spans::OUTPUT)?,
        output_format: strings(batch, spans::OUTPUT_FORMAT)?,
        input_preview: strings(batch, spans::INPUT_PREVIEW)?,
        output_preview: strings(batch, spans::OUTPUT_PREVIEW)?,
        system_instructions: strings(batch, spans::SYSTEM_INSTRUCTIONS)?,
        tool_definitions: strings(batch, spans::TOOL_DEFINITIONS)?,
        attributes: maps(batch, spans::ATTRIBUTES)?,
        resource_attributes: maps(batch, spans::RESOURCE_ATTRIBUTES)?,
        scope_name: strings(batch, spans::SCOPE_NAME)?,
        scope_version: strings(batch, spans::SCOPE_VERSION)?,
        events: lists(batch, spans::EVENTS)?,
        links: lists(batch, spans::LINKS)?,
        media_refs: lists(batch, spans::MEDIA_REFS)?,
        truncated: lists(batch, spans::TRUNCATED)?,
        conventions: lists(batch, spans::CONVENTIONS)?,
        ingest_time: times(batch, spans::INGEST_TIME)?,
        schema_version: downcast::<UInt16Array>(
            &column(batch, spans::SCHEMA_VERSION, &DataType::UInt16)?,
            spans::SCHEMA_VERSION,
        )?,
    })
}

impl SpanColumns {
    /// Row `row` as a span. `project_id` comes from the caller.
    fn row(&self, row: usize, project_id: ProjectId) -> Result<Span, ConvertError> {
        Ok(Span {
            project_id,
            trace_id: req_parsed(&self.trace_id, row, spans::TRACE_ID, TraceId::from_str)?,
            span_id: req_parsed(&self.span_id, row, spans::SPAN_ID, SpanId::from_str)?,
            parent_span_id: opt_parsed(
                &self.parent_span_id,
                row,
                spans::PARENT_SPAN_ID,
                SpanId::from_str,
            )?,
            name: req_string(&self.name, row, spans::NAME)?,
            span_kind: req_parsed(&self.span_kind, row, spans::SPAN_KIND, SpanKind::parse)?,
            start_time: req_time(&self.start_time, row, spans::START_TIME)?,
            end_time: req_time(&self.end_time, row, spans::END_TIME)?,
            status: req_parsed(&self.status, row, spans::STATUS, SpanStatus::parse)?,
            status_message: opt_string(&self.status_message, row),
            service_name: opt_string(&self.service_name, row),
            session_id: opt_string(&self.session_id, row),
            user_id: opt_string(&self.user_id, row),
            environment: opt_string(&self.environment, row),
            release: opt_string(&self.release, row),
            tags: list_strings(&self.tags, row, spans::TAGS)?,
            metadata: map_strings(&self.metadata, row, spans::METADATA)?,
            provider: opt_string(&self.provider, row),
            request_model: opt_string(&self.request_model, row),
            response_model: opt_string(&self.response_model, row),
            operation: opt_string(&self.operation, row),
            input_tokens: opt_i64(&self.input_tokens, row),
            output_tokens: opt_i64(&self.output_tokens, row),
            cache_read_tokens: opt_i64(&self.cache_read_tokens, row),
            cache_write_tokens: opt_i64(&self.cache_write_tokens, row),
            reasoning_tokens: opt_i64(&self.reasoning_tokens, row),
            total_tokens: opt_i64(&self.total_tokens, row),
            cost_input: opt_f64(&self.cost_input, row),
            cost_output: opt_f64(&self.cost_output, row),
            cost_total: opt_f64(&self.cost_total, row),
            cost_source: opt_parsed(
                &self.cost_source,
                row,
                spans::COST_SOURCE,
                CostSource::parse,
            )?,
            ttft_ns: opt_i64(&self.ttft_ns, row),
            request_params: opt_string(&self.request_params, row),
            input: opt_string(&self.input, row),
            input_format: opt_parsed(
                &self.input_format,
                row,
                spans::INPUT_FORMAT,
                PayloadFormat::parse,
            )?,
            output: opt_string(&self.output, row),
            output_format: opt_parsed(
                &self.output_format,
                row,
                spans::OUTPUT_FORMAT,
                PayloadFormat::parse,
            )?,
            input_preview: opt_string(&self.input_preview, row),
            output_preview: opt_string(&self.output_preview, row),
            system_instructions: opt_string(&self.system_instructions, row),
            tool_definitions: opt_string(&self.tool_definitions, row),
            attributes: map_strings(&self.attributes, row, spans::ATTRIBUTES)?,
            resource_attributes: map_strings(
                &self.resource_attributes,
                row,
                spans::RESOURCE_ATTRIBUTES,
            )?,
            scope_name: opt_string(&self.scope_name, row),
            scope_version: opt_string(&self.scope_version, row),
            events: span_events(&self.events, row)?,
            links: span_links(&self.links, row)?,
            media_refs: list_strings(&self.media_refs, row, spans::MEDIA_REFS)?,
            truncated: list_strings(&self.truncated, row, spans::TRUNCATED)?,
            conventions: span_conventions(&self.conventions, row)?,
            ingest_time: req_time(&self.ingest_time, row, spans::INGEST_TIME)?,
            schema_version: (!self.schema_version.is_null(row))
                .then(|| self.schema_version.value(row))
                .ok_or(ConvertError::Null(spans::SCHEMA_VERSION))?,
        })
    }
}

/// The one span of a `span_detail` batch. `project_id` comes from the caller.
///
/// A batch without rows is `Null(span_id)`. Row 0 is read.
pub(crate) fn span(batch: &RecordBatch, project_id: ProjectId) -> Result<Span, ConvertError> {
    if batch.num_rows() == 0 {
        return Err(ConvertError::Null(spans::SPAN_ID));
    }
    span_columns(batch)?.row(0, project_id)
}

/// Every row of `batches`, in order. Batches without rows add nothing. Columns are
/// extracted once per batch.
pub(crate) fn spans(
    batches: &[RecordBatch],
    project_id: ProjectId,
) -> Result<Vec<Span>, ConvertError> {
    let mut out = Vec::new();
    for batch in batches.iter().filter(|batch| batch.num_rows() > 0) {
        let columns = span_columns(batch)?;
        for row in 0..batch.num_rows() {
            out.push(columns.row(row, project_id)?);
        }
    }
    Ok(out)
}

/// `events` items: `time`, `name` and a nullable `attributes` JSON text.
fn span_events(list: &ListArray, row: usize) -> Result<Vec<SpanEvent>, ConvertError> {
    if list.is_null(row) {
        return Ok(Vec::new());
    }
    let items = list.value(row);
    let items = downcast::<StructArray>(&items, spans::EVENTS)?;
    let time_type = DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()));
    let time = downcast::<TimestampNanosecondArray>(
        &child(&items, event_field::TIME, &time_type)?,
        spans::EVENTS,
    )?;
    let name = downcast::<StringArray>(
        &child(&items, event_field::NAME, &DataType::Utf8)?,
        spans::EVENTS,
    )?;
    let attributes = downcast::<StringArray>(
        &child(&items, event_field::ATTRIBUTES, &DataType::Utf8)?,
        spans::EVENTS,
    )?;
    let mut out = Vec::with_capacity(items.len());
    for index in 0..items.len() {
        out.push(SpanEvent {
            time: req_time(&time, index, spans::EVENTS)?,
            name: req_string(&name, index, spans::EVENTS)?,
            attributes: opt_string(&attributes, index),
        });
    }
    Ok(out)
}

/// `links` items: `FixedSizeBinary` ids and a nullable `attributes` JSON text.
fn span_links(list: &ListArray, row: usize) -> Result<Vec<SpanLink>, ConvertError> {
    if list.is_null(row) {
        return Ok(Vec::new());
    }
    let items = list.value(row);
    let items = downcast::<StructArray>(&items, spans::LINKS)?;
    let trace_id = downcast::<FixedSizeBinaryArray>(
        &child(&items, link_field::TRACE_ID, &DataType::FixedSizeBinary(16))?,
        spans::LINKS,
    )?;
    let span_id = downcast::<FixedSizeBinaryArray>(
        &child(&items, link_field::SPAN_ID, &DataType::FixedSizeBinary(8))?,
        spans::LINKS,
    )?;
    let attributes = downcast::<StringArray>(
        &child(&items, link_field::ATTRIBUTES, &DataType::Utf8)?,
        spans::LINKS,
    )?;
    let mut out = Vec::with_capacity(items.len());
    for index in 0..items.len() {
        out.push(SpanLink {
            trace_id: TraceId::from_slice(trace_id.value(index))
                .map_err(|_| ConvertError::Value(spans::LINKS))?,
            span_id: SpanId::from_slice(span_id.value(index))
                .map_err(|_| ConvertError::Value(spans::LINKS))?,
            attributes: opt_string(&attributes, index),
        });
    }
    Ok(out)
}

/// `conventions` items, each parsed with `Convention::parse`.
fn span_conventions(list: &ListArray, row: usize) -> Result<Vec<Convention>, ConvertError> {
    list_strings(list, row, spans::CONVENTIONS)?
        .iter()
        .map(|text| Convention::parse(text).map_err(|_| ConvertError::Value(spans::CONVENTIONS)))
        .collect()
}

/// Session list rows (sessions view), in row order across batches.
pub(crate) fn session_summaries(
    batches: &[RecordBatch],
) -> Result<Vec<SessionSummary>, ConvertError> {
    let mut out = Vec::new();
    for batch in batches {
        out.extend(session_batch(batch)?);
    }
    Ok(out)
}

fn session_batch(batch: &RecordBatch) -> Result<Vec<SessionSummary>, ConvertError> {
    let session_id = strings(batch, traces::SESSION_ID)?;
    let start_time = times(batch, traces::START_TIME)?;
    let end_time = times(batch, traces::END_TIME)?;
    let duration_ns = ints(batch, TRACE_DURATION_NS)?;
    let trace_count = ints(batch, TRACE_COUNT)?;
    let error_count = ints(batch, traces::ERROR_COUNT)?;
    let total_tokens = ints(batch, traces::TOTAL_TOKENS)?;
    let cost_total = floats(batch, traces::COST_TOTAL)?;
    let user_id = strings(batch, traces::USER_ID)?;
    let tags = lists(batch, traces::TAGS)?;

    let mut out = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        out.push(SessionSummary {
            session_id: req_string(&session_id, row, traces::SESSION_ID)?,
            start_time: req_time(&start_time, row, traces::START_TIME)?,
            end_time: req_time(&end_time, row, traces::END_TIME)?,
            duration_ns: req_i64(&duration_ns, row, TRACE_DURATION_NS)?,
            trace_count: req_i64(&trace_count, row, TRACE_COUNT)?,
            error_count: req_i64(&error_count, row, traces::ERROR_COUNT)?,
            total_tokens: opt_i64(&total_tokens, row),
            cost_total: opt_f64(&cost_total, row),
            user_id: opt_string(&user_id, row),
            tags: list_strings(&tags, row, traces::TAGS)?,
            scores: Vec::new(),
        });
    }
    Ok(out)
}

/// End-user list rows (users view), in row order across batches.
pub(crate) fn end_user_summaries(
    batches: &[RecordBatch],
) -> Result<Vec<EndUserSummary>, ConvertError> {
    let mut out = Vec::new();
    for batch in batches {
        out.extend(end_user_batch(batch)?);
    }
    Ok(out)
}

fn end_user_batch(batch: &RecordBatch) -> Result<Vec<EndUserSummary>, ConvertError> {
    let user_id = strings(batch, traces::USER_ID)?;
    let first_seen = times(batch, FIRST_SEEN)?;
    let last_seen = times(batch, LAST_SEEN)?;
    let trace_count = ints(batch, TRACE_COUNT)?;
    let session_count = ints(batch, SESSION_COUNT)?;
    let error_count = ints(batch, traces::ERROR_COUNT)?;
    let total_tokens = ints(batch, traces::TOTAL_TOKENS)?;
    let cost_total = floats(batch, traces::COST_TOTAL)?;

    let mut out = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        out.push(EndUserSummary {
            user_id: req_string(&user_id, row, traces::USER_ID)?,
            first_seen: req_time(&first_seen, row, FIRST_SEEN)?,
            last_seen: req_time(&last_seen, row, LAST_SEEN)?,
            trace_count: req_i64(&trace_count, row, TRACE_COUNT)?,
            session_count: req_i64(&session_count, row, SESSION_COUNT)?,
            error_count: req_i64(&error_count, row, traces::ERROR_COUNT)?,
            total_tokens: opt_i64(&total_tokens, row),
            cost_total: opt_f64(&cost_total, row),
        });
    }
    Ok(out)
}

/// The input and output of a trace's root span, as `(input, output)`.
pub(crate) type RootPayloads = (Option<Payload>, Option<Payload>);

/// The root input and output of each trace, keyed by trace id. A trace without a root has no
/// entry. The first row of a trace id wins.
pub(crate) fn root_payloads(
    batches: &[RecordBatch],
) -> Result<HashMap<TraceId, RootPayloads>, ConvertError> {
    let mut out = HashMap::new();
    for batch in batches {
        let trace_id = strings(batch, spans::TRACE_ID)?;
        let input = strings(batch, spans::INPUT)?;
        let input_format = strings(batch, spans::INPUT_FORMAT)?;
        let output = strings(batch, spans::OUTPUT)?;
        let output_format = strings(batch, spans::OUTPUT_FORMAT)?;
        for row in 0..batch.num_rows() {
            let id = req_parsed(&trace_id, row, spans::TRACE_ID, TraceId::from_str)?;
            if out.contains_key(&id) {
                continue;
            }
            let input_format = opt_parsed(
                &input_format,
                row,
                spans::INPUT_FORMAT,
                PayloadFormat::parse,
            )?;
            let output_format = opt_parsed(
                &output_format,
                row,
                spans::OUTPUT_FORMAT,
                PayloadFormat::parse,
            )?;
            let payloads = (
                stored_payload(input_format, opt_string(&input, row)),
                stored_payload(output_format, opt_string(&output, row)),
            );
            out.insert(id, payloads);
        }
    }
    Ok(out)
}

/// Latest-score rows grouped by `target_key`. Each group keeps row order.
pub(crate) fn score_briefs(
    batches: &[RecordBatch],
) -> Result<HashMap<String, Vec<ScoreBrief>>, ConvertError> {
    let mut out: HashMap<String, Vec<ScoreBrief>> = HashMap::new();
    for batch in batches {
        let target_key = strings(batch, TARGET_KEY)?;
        let score_id = strings(batch, scores::SCORE_ID)?;
        let name = strings(batch, scores::NAME)?;
        let data_type = strings(batch, scores::DATA_TYPE)?;
        let value_num = floats(batch, scores::VALUE_NUM)?;
        let value_str = strings(batch, scores::VALUE_STR)?;
        let source = strings(batch, scores::SOURCE)?;
        for row in 0..batch.num_rows() {
            let key = req_string(&target_key, row, TARGET_KEY)?;
            let brief = ScoreBrief {
                score_id: req_parsed(&score_id, row, scores::SCORE_ID, Uuid::parse_str)?,
                name: req_string(&name, row, scores::NAME)?,
                value: score_value(
                    &req_string(&data_type, row, scores::DATA_TYPE)?,
                    opt_f64(&value_num, row),
                    opt_string(&value_str, row),
                )?,
                source: req_parsed(&source, row, scores::SOURCE, ScoreSource::parse)?,
            };
            out.entry(key).or_default().push(brief);
        }
    }
    Ok(out)
}

/// The API value of one stored score (`data_type`, `value_num`, `value_str`).
pub(crate) fn score_value(
    data_type: &str,
    value_num: Option<f64>,
    value_str: Option<String>,
) -> Result<ScoreValue, ConvertError> {
    let data_type =
        ScoreDataType::parse(data_type).map_err(|_| ConvertError::Value(scores::DATA_TYPE))?;
    match data_type {
        ScoreDataType::Numeric => Ok(ScoreValue::Numeric(
            value_num.ok_or(ConvertError::Null(scores::VALUE_NUM))?,
        )),
        ScoreDataType::Boolean => Ok(ScoreValue::Boolean(
            value_num.ok_or(ConvertError::Null(scores::VALUE_NUM))? != 0.0,
        )),
        ScoreDataType::Categorical => Ok(ScoreValue::Categorical {
            label: value_str.ok_or(ConvertError::Null(scores::VALUE_STR))?,
            value: value_num,
        }),
        ScoreDataType::Text => Ok(ScoreValue::Text(
            value_str.ok_or(ConvertError::Null(scores::VALUE_STR))?,
        )),
    }
}

/// Score rows of the scores view, in row order across batches. `project_id` is the caller's project.
pub(crate) fn scores(
    batches: &[RecordBatch],
    project_id: ProjectId,
) -> Result<Vec<Score>, ConvertError> {
    let mut out = Vec::new();
    for batch in batches {
        out.extend(score_batch(batch, project_id)?);
    }
    Ok(out)
}

fn score_batch(batch: &RecordBatch, project_id: ProjectId) -> Result<Vec<Score>, ConvertError> {
    let score_id = strings(batch, scores::SCORE_ID)?;
    let target = strings(batch, scores::TARGET)?;
    let trace_id = strings(batch, scores::TRACE_ID)?;
    let span_id = strings(batch, scores::SPAN_ID)?;
    let session_id = strings(batch, scores::SESSION_ID)?;
    let user_id = strings(batch, scores::USER_ID)?;
    let name = strings(batch, scores::NAME)?;
    let data_type = strings(batch, scores::DATA_TYPE)?;
    let value_num = floats(batch, scores::VALUE_NUM)?;
    let value_str = strings(batch, scores::VALUE_STR)?;
    let reason = strings(batch, scores::REASON)?;
    let source = strings(batch, scores::SOURCE)?;
    let evaluator_id = strings(batch, scores::EVALUATOR_ID)?;
    let rule_id = strings(batch, scores::RULE_ID)?;
    let experiment_id = strings(batch, scores::EXPERIMENT_ID)?;
    let dataset_item_id = strings(batch, scores::DATASET_ITEM_ID)?;
    let author_user_id = strings(batch, scores::AUTHOR_USER_ID)?;
    let metadata = maps(batch, scores::METADATA)?;
    let created_at = times(batch, scores::CREATED_AT)?;
    let updated_at = times(batch, scores::UPDATED_AT)?;
    let deleted = bools(batch, scores::DELETED)?;
    let ingest_time = times(batch, scores::INGEST_TIME)?;

    let mut out = Vec::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        let kind = req_parsed(&target, row, scores::TARGET, ScoreTargetKind::parse)?;
        let (score_target, score_session) = match kind {
            ScoreTargetKind::Span => (
                ScoreTarget::Span {
                    trace_id: req_parsed(&trace_id, row, scores::TRACE_ID, TraceId::from_str)?,
                    span_id: req_parsed(&span_id, row, scores::SPAN_ID, SpanId::from_str)?,
                },
                opt_string(&session_id, row),
            ),
            ScoreTargetKind::Trace => (
                ScoreTarget::Trace {
                    trace_id: req_parsed(&trace_id, row, scores::TRACE_ID, TraceId::from_str)?,
                },
                opt_string(&session_id, row),
            ),
            // The target's id is stored in `session_id`, so the score has no other session.
            ScoreTargetKind::Session => (
                ScoreTarget::Session {
                    session_id: req_string(&session_id, row, scores::SESSION_ID)?,
                },
                None,
            ),
        };
        out.push(Score {
            project_id,
            score_id: req_parsed(&score_id, row, scores::SCORE_ID, Uuid::parse_str)?,
            target: score_target,
            session_id: score_session,
            user_id: opt_string(&user_id, row),
            name: req_string(&name, row, scores::NAME)?,
            value: score_value(
                &req_string(&data_type, row, scores::DATA_TYPE)?,
                opt_f64(&value_num, row),
                opt_string(&value_str, row),
            )?,
            reason: opt_string(&reason, row),
            source: req_parsed(&source, row, scores::SOURCE, ScoreSource::parse)?,
            evaluator_id: opt_parsed(&evaluator_id, row, scores::EVALUATOR_ID, Uuid::parse_str)?,
            rule_id: opt_parsed(&rule_id, row, scores::RULE_ID, Uuid::parse_str)?,
            experiment_id: opt_parsed(&experiment_id, row, scores::EXPERIMENT_ID, Uuid::parse_str)?,
            dataset_item_id: opt_parsed(
                &dataset_item_id,
                row,
                scores::DATASET_ITEM_ID,
                Uuid::parse_str,
            )?,
            author_user_id: opt_parsed(
                &author_user_id,
                row,
                scores::AUTHOR_USER_ID,
                Uuid::parse_str,
            )?,
            metadata: map_strings(&metadata, row, scores::METADATA)?,
            created_at: req_time(&created_at, row, scores::CREATED_AT)?,
            updated_at: req_time(&updated_at, row, scores::UPDATED_AT)?,
            deleted: req_bool(&deleted, row, scores::DELETED)?,
            ingest_time: req_time(&ingest_time, row, scores::INGEST_TIME)?,
        });
    }
    Ok(out)
}

/// The API dashboard response. Storage names map variant by variant.
pub(crate) fn metrics_response(
    data: llmobs_storage::MetricsData,
    from: i64,
    to: i64,
    group_by: Option<MetricsGroupBy>,
) -> MetricsResponse {
    MetricsResponse {
        from,
        to,
        bucket_ns: data.bucket_ns,
        group_by,
        kpis: data
            .kpis
            .into_iter()
            .map(|kpi| Kpi {
                metric: kpi_metric(kpi.name),
                value: kpi.value,
                previous: kpi.previous,
            })
            .collect(),
        series: data
            .series
            .into_iter()
            .map(|series| MetricSeries {
                metric: metric_name(series.name),
                group: series.group,
                points: series
                    .points
                    .into_iter()
                    .map(|point| MetricPoint {
                        time: point.time,
                        value: point.value,
                    })
                    .collect(),
            })
            .collect(),
        top_models: data
            .top_models
            .into_iter()
            .map(|row| ModelUsage {
                model: row.model,
                llm_count: row.llm_count,
                total_tokens: row.total_tokens,
                cost_total: row.cost_total,
            })
            .collect(),
    }
}

fn kpi_metric(name: llmobs_storage::KpiName) -> KpiMetric {
    use llmobs_storage::KpiName;
    match name {
        KpiName::Traces => KpiMetric::Traces,
        KpiName::ErrorRate => KpiMetric::ErrorRate,
        KpiName::LatencyP95 => KpiMetric::LatencyP95,
        KpiName::CostTotal => KpiMetric::CostTotal,
        KpiName::TotalTokens => KpiMetric::TotalTokens,
    }
}

fn metric_name(name: llmobs_storage::SeriesName) -> MetricName {
    use llmobs_storage::SeriesName;
    match name {
        SeriesName::Traces => MetricName::Traces,
        SeriesName::Errors => MetricName::Errors,
        SeriesName::ErrorRate => MetricName::ErrorRate,
        SeriesName::LatencyP50 => MetricName::LatencyP50,
        SeriesName::LatencyP95 => MetricName::LatencyP95,
        SeriesName::LatencyP99 => MetricName::LatencyP99,
        SeriesName::Cost => MetricName::Cost,
        SeriesName::InputTokens => MetricName::InputTokens,
        SeriesName::OutputTokens => MetricName::OutputTokens,
        SeriesName::CacheReadTokens => MetricName::CacheReadTokens,
        SeriesName::CacheWriteTokens => MetricName::CacheWriteTokens,
        SeriesName::TtftP50 => MetricName::TtftP50,
        SeriesName::TtftP95 => MetricName::TtftP95,
        SeriesName::LlmLatencyP50 => MetricName::LlmLatencyP50,
        SeriesName::LlmLatencyP95 => MetricName::LlmLatencyP95,
        SeriesName::CostByModel => MetricName::CostByModel,
        SeriesName::ScoreAvg => MetricName::ScoreAvg,
        SeriesName::EvalCost => MetricName::EvalCost,
    }
}

/// The API catalog of views and functions. Storage names map variant by variant.
pub(crate) fn schema_response() -> SchemaResponse {
    SchemaResponse {
        views: llmobs_storage::view_catalog()
            .into_iter()
            .map(|view| ViewSchema {
                name: view_name(view.view),
                description: view.description.to_owned(),
                columns: view
                    .columns
                    .into_iter()
                    .map(|column| ColumnSchema {
                        name: column.name.to_owned(),
                        column_type: column_type(column.kind),
                        nullable: column.nullable,
                        description: column.description.to_owned(),
                    })
                    .collect(),
            })
            .collect(),
        functions: llmobs_storage::function_catalog()
            .into_iter()
            .map(|function| FunctionSchema {
                name: function.name.to_owned(),
                signature: function.signature.to_owned(),
                description: function.description.to_owned(),
                in_filter: function.in_filter,
                in_sql: function.in_sql,
            })
            .collect(),
    }
}

fn view_name(name: llmobs_storage::ViewName) -> super::schema::ViewName {
    use super::schema::ViewName as Api;
    use llmobs_storage::ViewName;
    match name {
        ViewName::Traces => Api::Traces,
        ViewName::Spans => Api::Spans,
        ViewName::Sessions => Api::Sessions,
        ViewName::Scores => Api::Scores,
        ViewName::ExperimentItems => Api::ExperimentItems,
        ViewName::DatasetItems => Api::DatasetItems,
    }
}

fn column_type(kind: llmobs_storage::ColumnKind) -> ColumnType {
    use llmobs_storage::ColumnKind;
    match kind {
        ColumnKind::String => ColumnType::String,
        ColumnKind::Integer => ColumnType::Integer,
        ColumnKind::Float => ColumnType::Float,
        ColumnKind::Boolean => ColumnType::Boolean,
        ColumnKind::Timestamp => ColumnType::Timestamp,
        ColumnKind::StringList => ColumnType::StringList,
        ColumnKind::StringMap => ColumnType::StringMap,
        ColumnKind::Json => ColumnType::Json,
    }
}

#[cfg(test)]
mod tests {
    use arrow::datatypes::{FieldRef, Fields, Schema};
    use llmobs_storage::{
        QueryContext, ScoreTargets, SortOrder as StorageSortOrder, SpanListRequest, SpanSort,
        TraceListRequest, TraceSort,
    };

    use super::*;
    use crate::routes::read::{READ_TIMEOUT, list_scope, open_query};
    use crate::test_support::TestApp;

    /// A project with the example dataset loaded. The app is returned so it outlives the context.
    async fn example_context() -> (TestApp, QueryContext) {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        app.load_example_dataset(seeded.project.id).await;
        let ctx = open_query(&app.state, seeded.project.id, None)
            .await
            .unwrap();
        (app, ctx)
    }

    async fn trace_rows(ctx: &QueryContext, include_eval: bool) -> Vec<TraceSummary> {
        let page = llmobs_storage::trace_list(
            ctx,
            TraceListRequest {
                filter: None,
                search: None,
                scope: list_scope(None, None, include_eval, false).unwrap(),
                sort: TraceSort::StartTime,
                order: StorageSortOrder::Desc,
                limit: 50,
                cursor: None,
                timeout: READ_TIMEOUT,
            },
        )
        .await
        .unwrap();
        trace_summaries(&page.batches).unwrap()
    }

    async fn span_page(ctx: &QueryContext) -> llmobs_storage::Page {
        llmobs_storage::span_list(
            ctx,
            SpanListRequest {
                filter: None,
                search: None,
                scope: list_scope(None, None, false, false).unwrap(),
                sort: SpanSort::StartTime,
                order: StorageSortOrder::Desc,
                limit: 50,
                cursor: None,
                timeout: READ_TIMEOUT,
            },
        )
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn trace_rows_match_the_dataset() {
        let (_app, ctx) = example_context().await;
        let traces = trace_rows(&ctx, true).await;
        let ids: Vec<String> = traces.iter().map(|t| t.trace_id.to_string()).collect();
        assert_eq!(
            ids,
            [
                "a1000000000000000000000000000006",
                "a1000000000000000000000000000004",
                "a1000000000000000000000000000003",
                "a1000000000000000000000000000002",
                "a1000000000000000000000000000005",
                "a1000000000000000000000000000001",
            ]
        );

        let trace1 = &traces[5];
        assert!(trace1.has_root);
        assert_eq!(
            trace1
                .root_span_id
                .as_ref()
                .map(ToString::to_string)
                .as_deref(),
            Some("b100000000000011")
        );
        assert_eq!(trace1.span_count, 4);
        assert_eq!(trace1.tags, ["beta"]);
        assert!((trace1.cost_total.unwrap() - 0.00306).abs() < 1e-12);
        assert_eq!(trace1.status, SpanStatus::Ok);
        assert!(trace1.scores.is_empty());

        let trace6 = &traces[0];
        assert!(!trace6.has_root);
        assert!(trace6.root_span_id.is_none());

        let trace3 = &traces[2];
        assert_eq!(trace3.status, SpanStatus::Error);
        assert_eq!(trace3.error_count, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn span_rows_match_the_dataset() {
        let (_app, ctx) = example_context().await;
        let spans = span_summaries(&span_page(&ctx).await.batches).unwrap();
        assert_eq!(spans.len(), 12);
        assert_eq!(spans[0].span_id.to_string(), "b100000000000061");
        let span12 = spans
            .iter()
            .find(|span| span.span_id.to_string() == "b100000000000012")
            .unwrap();
        assert_eq!(span12.model.as_deref(), Some("claude-sonnet-4-5"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn span_rows_read_utf8_view_strings_the_same() {
        let (_app, ctx) = example_context().await;
        let page = span_page(&ctx).await;
        let expected = span_summaries(&page.batches).unwrap();
        let views: Vec<RecordBatch> = page.batches.iter().map(to_utf8_view).collect();
        assert_eq!(
            views[0].column_by_name(spans::NAME).unwrap().data_type(),
            &DataType::Utf8View
        );
        assert_eq!(span_summaries(&views).unwrap(), expected);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn latest_score_briefs_group_by_target() {
        let (_app, ctx) = example_context().await;
        let targets = [
            "a1000000000000000000000000000001",
            "a1000000000000000000000000000002",
            "a1000000000000000000000000000003",
        ];
        let batches =
            llmobs_storage::latest_scores(&ctx, ScoreTargets::Traces(&targets), READ_TIMEOUT)
                .await
                .unwrap();
        let briefs = score_briefs(&batches).unwrap();

        let trace1 = &briefs[targets[0]];
        assert_eq!(trace1.len(), 1);
        assert_eq!(trace1[0].name, "helpfulness");
        assert_eq!(trace1[0].value, ScoreValue::Numeric(0.9));
        assert_eq!(trace1[0].source, ScoreSource::LlmJudge);
        assert_eq!(
            trace1[0].score_id,
            Uuid::parse_str("01900000-0000-7000-8000-000000000302").unwrap()
        );
        assert_eq!(briefs[targets[1]][0].value, ScoreValue::Boolean(true));
        assert_eq!(
            briefs[targets[2]][0].value,
            ScoreValue::Text("Needs retry with backoff.".to_owned())
        );
    }

    #[test]
    fn score_value_maps_each_data_type() {
        assert_eq!(
            score_value("numeric", Some(0.5), None).unwrap(),
            ScoreValue::Numeric(0.5)
        );
        assert_eq!(
            score_value("boolean", Some(0.0), None).unwrap(),
            ScoreValue::Boolean(false)
        );
        assert_eq!(
            score_value("categorical", Some(1.0), Some("friendly".to_owned())).unwrap(),
            ScoreValue::Categorical {
                label: "friendly".to_owned(),
                value: Some(1.0),
            }
        );
        assert_eq!(
            score_value("text", None, Some("note".to_owned())).unwrap(),
            ScoreValue::Text("note".to_owned())
        );
    }

    #[test]
    fn score_value_names_the_missing_column() {
        assert!(matches!(
            score_value("numeric", None, None),
            Err(ConvertError::Null("value_num"))
        ));
        assert!(matches!(
            score_value("text", None, None),
            Err(ConvertError::Null("value_str"))
        ));
        assert!(matches!(
            score_value("unknown", None, None),
            Err(ConvertError::Value("data_type"))
        ));
    }

    #[test]
    fn a_missing_column_is_reported_by_name() {
        let batch = RecordBatch::try_from_iter([(
            traces::TRACE_ID,
            Arc::new(StringArray::from(vec!["a1000000000000000000000000000001"])) as ArrayRef,
        )])
        .unwrap();
        let error = trace_summaries(&[batch]).unwrap_err();
        assert!(matches!(error, ConvertError::Missing(_)));
    }

    /// `Utf8` becomes `Utf8View` everywhere, including list items and map entries.
    fn utf8_view(data_type: &DataType) -> DataType {
        match data_type {
            DataType::Utf8 => DataType::Utf8View,
            DataType::List(item) => DataType::List(with_view(item)),
            DataType::Map(entries, sorted) => DataType::Map(with_view(entries), *sorted),
            DataType::Struct(fields) => DataType::Struct(fields.iter().map(with_view).collect()),
            other => other.clone(),
        }
    }

    fn with_view(field: &FieldRef) -> FieldRef {
        Arc::new(
            field
                .as_ref()
                .clone()
                .with_data_type(utf8_view(field.data_type())),
        )
    }

    fn to_utf8_view(batch: &RecordBatch) -> RecordBatch {
        let columns: Vec<ArrayRef> = batch
            .columns()
            .iter()
            .map(|column| cast(column, &utf8_view(column.data_type())).unwrap())
            .collect();
        let fields: Fields = batch
            .schema()
            .fields()
            .iter()
            .zip(&columns)
            .map(|(field, column)| {
                Arc::new(
                    field
                        .as_ref()
                        .clone()
                        .with_data_type(column.data_type().clone()),
                )
            })
            .collect();
        RecordBatch::try_new(Arc::new(Schema::new(fields)), columns).unwrap()
    }
}
