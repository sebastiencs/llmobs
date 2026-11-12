//! Parquet encoding for one flush partition.
//!
//! The flush writes each frozen partition through `write_file` from
//! `spawn_blocking`.

#![allow(dead_code)]

use std::sync::Arc;

use arrow::array::{Array, AsArray, RecordBatch};
use arrow::compute::{
    SortColumn, SortOptions, concat_batches, lexsort_to_indices, take_record_batch,
};
use arrow::datatypes::{SchemaRef, TimestampNanosecondType};
use bytes::Bytes;
use llmobs_core::schema::{scores, scores_schema, spans, spans_schema, traces, traces_schema};
use parquet::arrow::ArrowSchemaConverter;
use parquet::arrow::arrow_writer::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::metadata::{KeyValue, SortingColumn};
use parquet::file::properties::{EnabledStatistics, WriterProperties};
use parquet::schema::types::{ColumnPath, SchemaDescriptor};

use crate::catalog::FileTable;
use crate::error::ParquetWriteError;

/// `llmobs.schema_version` written into every file.
///
/// It follows the stored row `schema_version` (spec 01). Rows are version 1.
const SCHEMA_VERSION: &str = "1";

const KEY_TABLE: &str = "llmobs.table";
const KEY_SCHEMA_VERSION: &str = "llmobs.schema_version";
const KEY_LEVEL: &str = "llmobs.level";
const KEY_MIN_TIME: &str = "llmobs.min_time";
const KEY_MAX_TIME: &str = "llmobs.max_time";
const KEY_MIN_INGEST_TIME: &str = "llmobs.min_ingest_time";
const KEY_MAX_INGEST_TIME: &str = "llmobs.max_ingest_time";

/// Columns spec 01 marks `(dict)` for `spans`, in schema order.
///
/// `conventions` is `List(Utf8 dict)`: dictionary encoding applies to the list
/// values. Every other column, including `input`, `output` and `attributes`,
/// is plain.
pub(crate) const SPANS_DICTIONARY_COLUMNS: &[&str] = &[
    spans::PROJECT_ID,
    spans::SPAN_KIND,
    spans::STATUS,
    spans::SERVICE_NAME,
    spans::ENVIRONMENT,
    spans::RELEASE,
    spans::PROVIDER,
    spans::REQUEST_MODEL,
    spans::RESPONSE_MODEL,
    spans::MODEL,
    spans::OPERATION,
    spans::COST_SOURCE,
    spans::INPUT_FORMAT,
    spans::OUTPUT_FORMAT,
    spans::SCOPE_NAME,
    spans::SCOPE_VERSION,
    spans::CONVENTIONS,
];

/// Columns spec 01 marks `(dict)` for `scores`, in schema order.
pub(crate) const SCORES_DICTIONARY_COLUMNS: &[&str] = &[
    scores::TARGET,
    scores::NAME,
    scores::DATA_TYPE,
    scores::SOURCE,
];

/// Low-cardinality strings of `traces`, in schema order. Spec 03 §3.
pub(crate) const TRACES_DICTIONARY_COLUMNS: &[&str] = &[
    traces::NAME,
    traces::STATUS,
    traces::ENVIRONMENT,
    traces::RELEASE,
];

const SPANS_BLOOM_COLUMNS: &[&str] = &[spans::TRACE_ID, spans::SESSION_ID, spans::USER_ID];
const TRACES_BLOOM_COLUMNS: &[&str] = &[traces::TRACE_ID, traces::SESSION_ID, traces::USER_ID];
const SCORES_BLOOM_COLUMNS: &[&str] = &[scores::TRACE_ID, scores::SCORE_ID];

/// Close a row group at this many rows. Spec 03 §3.
const ROW_GROUP_ROWS: usize = 100_000;

/// Close a row group at this encoded size. Spec 03 §3.
const ROW_GROUP_BYTES: usize = 64 * 1024 * 1024;

/// Rows per `write`, so the row-group check runs between slices.
const WRITE_SLICE_ROWS: usize = 8_192;

const DATA_PAGE_BYTES: usize = 1024 * 1024;
const DATA_PAGE_ROWS: usize = 20_000;
const ZSTD_LEVEL: i32 = 3;
const BLOOM_FPP: f64 = 0.01;

struct Bounds {
    min_time: i64,
    max_time: i64,
    min_ingest_time: i64,
    max_ingest_time: i64,
}

/// One encoded Parquet object and the catalog fields that describe it.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WrittenFile {
    /// The complete file, including the footer.
    pub(crate) bytes: Bytes,
    /// Rows in the file.
    pub(crate) rows: u64,
    /// Footer metadata length plus the 8-byte trailer.
    ///
    /// A reader can open the file with one ranged read of this many trailing bytes.
    pub(crate) footer_size: u64,
    /// Smallest event time, nanoseconds. `start_time` for spans, `created_at` for scores.
    pub(crate) min_time: i64,
    /// Largest event time, nanoseconds.
    pub(crate) max_time: i64,
    /// Smallest `ingest_time`, nanoseconds.
    pub(crate) min_ingest_time: i64,
    /// Largest `ingest_time`, nanoseconds.
    pub(crate) max_ingest_time: i64,
}

/// Sorts, encodes and returns the file.
///
/// CPU-bound: call from `spawn_blocking`. `level` is `0` or `1`.
pub(crate) fn write_file(
    table: FileTable,
    level: u8,
    batches: &[Arc<RecordBatch>],
) -> Result<WrittenFile, ParquetWriteError> {
    if level > 1 {
        return Err(ParquetWriteError::invalid("level must be 0 or 1"));
    }
    let sorted = sort_batches(table, batches)?;
    let rows =
        u64::try_from(sorted.num_rows()).map_err(|_| ParquetWriteError::invalid("row count"))?;
    let (min_time, max_time) = i64_bounds(&sorted, time_column(table))?;
    let (min_ingest_time, max_ingest_time) = i64_bounds(&sorted, ingest_column(table))?;
    let bounds = Bounds {
        min_time,
        max_time,
        min_ingest_time,
        max_ingest_time,
    };
    let properties = writer_properties(table, level, rows, &bounds, sorted.schema().as_ref())?;
    let bytes = encode(&sorted, properties)?;
    let footer_size = footer_size(&bytes)?;
    Ok(WrittenFile {
        bytes,
        rows,
        footer_size,
        min_time,
        max_time,
        min_ingest_time,
        max_ingest_time,
    })
}

fn sort_batches(
    table: FileTable,
    batches: &[Arc<RecordBatch>],
) -> Result<RecordBatch, ParquetWriteError> {
    let schema = table_schema(table);
    let mut rows = Vec::new();
    for batch in batches {
        if batch.schema().as_ref() != schema.as_ref() {
            return Err(ParquetWriteError::invalid("batch schema"));
        }
        if batch.num_rows() > 0 {
            rows.push(batch.as_ref());
        }
    }
    if rows.is_empty() {
        return Err(ParquetWriteError::invalid("empty batch"));
    }
    let combined = concat_batches(&schema, rows).map_err(ParquetWriteError::arrow)?;
    let keys = sort_keys(table, &combined)?;
    let indices = lexsort_to_indices(&keys, None).map_err(ParquetWriteError::arrow)?;
    take_record_batch(&combined, &indices).map_err(ParquetWriteError::arrow)
}

fn sort_keys(table: FileTable, batch: &RecordBatch) -> Result<Vec<SortColumn>, ParquetWriteError> {
    let ascending = SortOptions {
        descending: false,
        nulls_first: false,
    };
    let mut keys = Vec::new();
    for name in sort_columns(table) {
        keys.push(SortColumn {
            values: Arc::clone(column(batch, name)?),
            options: Some(ascending),
        });
    }
    Ok(keys)
}

fn table_schema(table: FileTable) -> SchemaRef {
    match table {
        FileTable::Spans => spans_schema(),
        FileTable::Traces => traces_schema(),
        FileTable::Scores => scores_schema(),
    }
}

fn sort_columns(table: FileTable) -> &'static [&'static str] {
    match table {
        FileTable::Spans => &[spans::TRACE_ID, spans::START_TIME],
        FileTable::Traces => &[traces::START_TIME, traces::TRACE_ID],
        FileTable::Scores => &[scores::CREATED_AT],
    }
}

fn time_column(table: FileTable) -> &'static str {
    match table {
        FileTable::Spans => spans::START_TIME,
        FileTable::Traces => traces::START_TIME,
        FileTable::Scores => scores::CREATED_AT,
    }
}

fn ingest_column(table: FileTable) -> &'static str {
    match table {
        FileTable::Spans => spans::INGEST_TIME,
        FileTable::Traces => traces::LAST_INGEST_TIME,
        FileTable::Scores => scores::INGEST_TIME,
    }
}

fn dictionary_columns(table: FileTable) -> &'static [&'static str] {
    match table {
        FileTable::Spans => SPANS_DICTIONARY_COLUMNS,
        FileTable::Traces => TRACES_DICTIONARY_COLUMNS,
        FileTable::Scores => SCORES_DICTIONARY_COLUMNS,
    }
}

fn bloom_columns(table: FileTable) -> &'static [&'static str] {
    match table {
        FileTable::Spans => SPANS_BLOOM_COLUMNS,
        FileTable::Traces => TRACES_BLOOM_COLUMNS,
        FileTable::Scores => SCORES_BLOOM_COLUMNS,
    }
}

fn writer_properties(
    table: FileTable,
    level: u8,
    rows: u64,
    bounds: &Bounds,
    schema: &arrow::datatypes::Schema,
) -> Result<WriterProperties, ParquetWriteError> {
    let zstd = ZstdLevel::try_new(ZSTD_LEVEL).map_err(ParquetWriteError::parquet)?;
    let descriptor = ArrowSchemaConverter::new()
        .convert(schema)
        .map_err(ParquetWriteError::parquet)?;
    let mut builder = WriterProperties::builder()
        .set_compression(Compression::ZSTD(zstd))
        .set_dictionary_enabled(false)
        .set_data_page_size_limit(DATA_PAGE_BYTES)
        .set_data_page_row_count_limit(DATA_PAGE_ROWS)
        .set_statistics_enabled(EnabledStatistics::Page)
        .set_offset_index_disabled(false)
        .set_max_row_group_row_count(None)
        .set_sorting_columns(Some(sorting_columns(table, &descriptor)?))
        .set_key_value_metadata(Some(file_metadata(table, level, bounds)));
    for name in dictionary_columns(table) {
        let mut matched = false;
        for column in descriptor.columns() {
            if column.path().parts().first().map(String::as_str) == Some(*name) {
                builder = builder.set_column_dictionary_enabled(column.path().clone(), true);
                matched = true;
            }
        }
        if !matched {
            return Err(ParquetWriteError::invalid("dictionary column"));
        }
    }
    for name in bloom_columns(table) {
        let path = leaf_path(&descriptor, name)?;
        builder = builder
            .set_column_bloom_filter_enabled(path.clone(), true)
            .set_column_bloom_filter_fpp(path.clone(), BLOOM_FPP)
            .set_column_bloom_filter_max_ndv(path, rows.max(1));
    }
    Ok(builder.build())
}

fn sorting_columns(
    table: FileTable,
    descriptor: &SchemaDescriptor,
) -> Result<Vec<SortingColumn>, ParquetWriteError> {
    let mut columns = Vec::new();
    for name in sort_columns(table) {
        columns.push(SortingColumn {
            column_idx: leaf_index(descriptor, name)?,
            descending: false,
            nulls_first: false,
        });
    }
    Ok(columns)
}

fn leaf_path(descriptor: &SchemaDescriptor, name: &str) -> Result<ColumnPath, ParquetWriteError> {
    let index = usize::try_from(leaf_index(descriptor, name)?)
        .map_err(|_| ParquetWriteError::invalid("column index"))?;
    descriptor
        .columns()
        .get(index)
        .map(|column| column.path().clone())
        .ok_or_else(|| ParquetWriteError::invalid("missing column"))
}

fn leaf_index(descriptor: &SchemaDescriptor, name: &str) -> Result<i32, ParquetWriteError> {
    let mut found = None;
    for (index, column) in descriptor.columns().iter().enumerate() {
        let parts = column.path().parts();
        if parts.len() == 1 && parts[0] == name {
            if found.is_some() {
                return Err(ParquetWriteError::invalid("duplicate column"));
            }
            found =
                Some(i32::try_from(index).map_err(|_| ParquetWriteError::invalid("column index"))?);
        }
    }
    found.ok_or_else(|| ParquetWriteError::invalid("missing column"))
}

fn file_metadata(table: FileTable, level: u8, bounds: &Bounds) -> Vec<KeyValue> {
    let pairs = [
        (KEY_TABLE, table.as_str().to_owned()),
        (KEY_SCHEMA_VERSION, SCHEMA_VERSION.to_owned()),
        (KEY_LEVEL, level.to_string()),
        (KEY_MIN_TIME, bounds.min_time.to_string()),
        (KEY_MAX_TIME, bounds.max_time.to_string()),
        (KEY_MIN_INGEST_TIME, bounds.min_ingest_time.to_string()),
        (KEY_MAX_INGEST_TIME, bounds.max_ingest_time.to_string()),
    ];
    pairs
        .into_iter()
        .map(|(key, value)| KeyValue::new(key.to_owned(), value))
        .collect()
}

fn encode(batch: &RecordBatch, properties: WriterProperties) -> Result<Bytes, ParquetWriteError> {
    let mut buffer = Vec::new();
    {
        let mut writer = ArrowWriter::try_new(&mut buffer, batch.schema(), Some(properties))
            .map_err(ParquetWriteError::parquet)?;
        let mut offset = 0;
        while offset < batch.num_rows() {
            let room = ROW_GROUP_ROWS.saturating_sub(writer.in_progress_rows());
            let len = WRITE_SLICE_ROWS
                .min(room.max(1))
                .min(batch.num_rows() - offset);
            writer
                .write(&batch.slice(offset, len))
                .map_err(ParquetWriteError::parquet)?;
            offset += len;
            if writer.in_progress_rows() >= ROW_GROUP_ROWS
                || writer.in_progress_size() >= ROW_GROUP_BYTES
            {
                writer.flush().map_err(ParquetWriteError::parquet)?;
            }
        }
        writer.finish().map_err(ParquetWriteError::parquet)?;
    }
    Ok(Bytes::from(buffer))
}

fn footer_size(bytes: &[u8]) -> Result<u64, ParquetWriteError> {
    let Some(tail) = bytes.get(bytes.len().saturating_sub(8)..) else {
        return Err(ParquetWriteError::invalid("parquet footer"));
    };
    if tail.len() != 8 || &tail[4..] != b"PAR1" {
        return Err(ParquetWriteError::invalid("parquet footer"));
    }
    let mut length = [0_u8; 4];
    length.copy_from_slice(&tail[..4]);
    let metadata = u64::from(u32::from_le_bytes(length));
    let total = metadata + 8;
    if total > bytes.len() as u64 {
        return Err(ParquetWriteError::invalid("parquet footer"));
    }
    Ok(total)
}

fn i64_bounds(batch: &RecordBatch, name: &str) -> Result<(i64, i64), ParquetWriteError> {
    let values = column(batch, name)?.as_primitive::<TimestampNanosecondType>();
    let min = arrow::compute::min(values)
        .ok_or_else(|| ParquetWriteError::invalid("empty time column"))?;
    let max = arrow::compute::max(values)
        .ok_or_else(|| ParquetWriteError::invalid("empty time column"))?;
    Ok((min, max))
}

fn column<'a>(batch: &'a RecordBatch, name: &str) -> Result<&'a Arc<dyn Array>, ParquetWriteError> {
    batch
        .column_by_name(name)
        .ok_or_else(|| ParquetWriteError::invalid("missing column"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use arrow::array::{
        ArrayRef, AsArray, BooleanArray, FixedSizeBinaryBuilder, Float64Array, Int32Array,
        Int64Array, ListBuilder, MapBuilder, RecordBatch, StringArray, StringBuilder,
        TimestampNanosecondBuilder,
    };
    use arrow::compute::{
        SortColumn, SortOptions, concat_batches, lexsort_to_indices, take_record_batch,
    };
    use arrow::datatypes::{DataType, Field, TimestampNanosecondType};
    use llmobs_core::batch::{scores_to_record_batch, spans_to_record_batch};
    use llmobs_core::enums::{
        Convention, CostSource, PayloadFormat, ScoreSource, SpanKind, SpanStatus,
    };
    use llmobs_core::ids::{ProjectId, SpanId, TraceId};
    use llmobs_core::schema::{scores, scores_schema, spans, spans_schema, traces, traces_schema};
    use llmobs_core::score::{Score, ScoreTarget, ScoreValue};
    use llmobs_core::span::{Span, SpanEvent, SpanLink};
    use parquet::arrow::arrow_reader::{ArrowReaderOptions, ParquetRecordBatchReaderBuilder};
    use parquet::basic::{Compression, Encoding};
    use parquet::bloom_filter::Sbbf;
    use parquet::file::metadata::{PageIndexPolicy, ParquetMetaDataReader};
    use uuid::Uuid;

    use super::{
        SCHEMA_VERSION, SCORES_DICTIONARY_COLUMNS, SPANS_DICTIONARY_COLUMNS,
        TRACES_DICTIONARY_COLUMNS, write_file,
    };
    use crate::catalog::FileTable;
    use crate::error::ParquetWriteError;
    use crate::wal::spans_named;

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn trace(last: u8) -> TraceId {
        let mut bytes = [0_u8; 16];
        bytes[15] = last;
        TraceId::from_bytes(bytes).unwrap()
    }

    fn span_id(last: u8) -> SpanId {
        let mut bytes = [0_u8; 8];
        bytes[7] = last;
        SpanId::from_bytes(bytes).unwrap()
    }

    fn span(trace_last: u8, start: i64, ingest: i64) -> Span {
        let mut metadata = BTreeMap::new();
        metadata.insert("plan".to_owned(), "\"pro\"".to_owned());
        let mut attributes = BTreeMap::new();
        attributes.insert("http.method".to_owned(), "\"GET\"".to_owned());
        let mut resource = BTreeMap::new();
        resource.insert("host.name".to_owned(), "\"api\"".to_owned());
        Span {
            project_id: project(),
            trace_id: trace(trace_last),
            span_id: span_id(trace_last),
            parent_span_id: Some(span_id(9)),
            name: format!("span-{trace_last}"),
            span_kind: SpanKind::Llm,
            start_time: start,
            end_time: start + 10,
            status: SpanStatus::Ok,
            status_message: Some("fine".to_owned()),
            service_name: Some("api".to_owned()),
            session_id: Some("session".to_owned()),
            user_id: Some("user".to_owned()),
            environment: Some("production".to_owned()),
            release: Some("abc".to_owned()),
            tags: vec!["beta".to_owned()],
            metadata,
            provider: Some("openai".to_owned()),
            request_model: Some("gpt".to_owned()),
            response_model: Some("gpt".to_owned()),
            operation: Some("chat".to_owned()),
            input_tokens: Some(3),
            output_tokens: Some(4),
            cache_read_tokens: Some(1),
            cache_write_tokens: Some(1),
            reasoning_tokens: Some(1),
            total_tokens: Some(7),
            cost_input: Some(0.1),
            cost_output: Some(0.2),
            cost_total: Some(0.3),
            cost_source: Some(CostSource::Computed),
            ttft_ns: Some(5),
            request_params: Some("{}".to_owned()),
            input: Some("hello".to_owned()),
            input_format: Some(PayloadFormat::Text),
            output: Some("world".to_owned()),
            output_format: Some(PayloadFormat::Text),
            input_preview: Some("hello".to_owned()),
            output_preview: Some("world".to_owned()),
            system_instructions: Some("[]".to_owned()),
            tool_definitions: Some("[]".to_owned()),
            attributes,
            resource_attributes: resource,
            scope_name: Some("llmobs".to_owned()),
            scope_version: Some("0".to_owned()),
            events: vec![SpanEvent {
                time: start,
                name: "exception".to_owned(),
                attributes: Some("{}".to_owned()),
            }],
            links: vec![SpanLink {
                trace_id: trace(trace_last),
                span_id: span_id(9),
                attributes: None,
            }],
            media_refs: vec!["ab".repeat(32)],
            truncated: vec!["input".to_owned()],
            conventions: vec![Convention::Llmobs],
            ingest_time: ingest,
            schema_version: 1,
        }
    }

    fn score(created_at: i64, name: &str) -> Score {
        let mut metadata = BTreeMap::new();
        metadata.insert("judge".to_owned(), "small".to_owned());
        Score {
            project_id: project(),
            score_id: Uuid::from_u128(u128::try_from(created_at).unwrap_or(1)),
            target: ScoreTarget::Trace { trace_id: trace(1) },
            session_id: Some("session".to_owned()),
            user_id: Some("user".to_owned()),
            name: name.to_owned(),
            value: ScoreValue::Categorical {
                label: "good".to_owned(),
                value: Some(1.0),
            },
            reason: Some("because".to_owned()),
            source: ScoreSource::Api,
            evaluator_id: Some(Uuid::from_u128(2)),
            rule_id: Some(Uuid::from_u128(3)),
            experiment_id: Some(Uuid::from_u128(4)),
            dataset_item_id: Some(Uuid::from_u128(5)),
            author_user_id: Some(Uuid::from_u128(6)),
            metadata,
            created_at,
            updated_at: created_at + 1,
            deleted: false,
            ingest_time: created_at + 2,
        }
    }

    fn read_rows(bytes: &bytes::Bytes, schema: &arrow::datatypes::SchemaRef) -> RecordBatch {
        let options = ArrowReaderOptions::new().with_page_index_policy(PageIndexPolicy::Required);
        let reader = ParquetRecordBatchReaderBuilder::try_new_with_options(bytes.clone(), options)
            .unwrap()
            .build()
            .unwrap();
        let batches: Vec<_> = reader.map(|batch| batch.unwrap()).collect();
        concat_batches(schema, &batches).unwrap()
    }

    fn metadata(bytes: &bytes::Bytes) -> parquet::file::metadata::ParquetMetaData {
        let mut reader =
            ParquetMetaDataReader::new().with_page_index_policy(PageIndexPolicy::Required);
        reader.try_parse(&bytes.clone()).unwrap();
        reader.finish().unwrap()
    }

    fn sorted_like_the_writer(table: FileTable, batch: &RecordBatch) -> RecordBatch {
        let names: &[&str] = match table {
            FileTable::Spans => &[spans::TRACE_ID, spans::START_TIME],
            FileTable::Traces => &[traces::START_TIME, traces::TRACE_ID],
            FileTable::Scores => &[scores::CREATED_AT],
        };
        let keys: Vec<_> = names
            .iter()
            .map(|name| SortColumn {
                values: Arc::clone(batch.column_by_name(name).unwrap()),
                options: Some(SortOptions {
                    descending: false,
                    nulls_first: false,
                }),
            })
            .collect();
        let indices = lexsort_to_indices(&keys, None).unwrap();
        take_record_batch(batch, &indices).unwrap()
    }

    fn timestamp(rows: usize) -> TimestampNanosecondBuilder {
        TimestampNanosecondBuilder::with_capacity(rows).with_timezone("UTC")
    }

    fn strings(value: &str, rows: usize) -> ArrayRef {
        Arc::new(StringArray::from(vec![value; rows]))
    }

    fn utf8_list(value: &str, rows: usize) -> ArrayRef {
        let mut builder = ListBuilder::with_capacity(StringBuilder::new(), rows)
            .with_field(Arc::new(Field::new("item", DataType::Utf8, false)));
        for _ in 0..rows {
            builder.values().append_value(value);
            builder.append(true);
        }
        Arc::new(builder.finish())
    }

    fn metadata_map(rows: usize) -> ArrayRef {
        let mut builder =
            MapBuilder::with_capacity(None, StringBuilder::new(), StringBuilder::new(), rows);
        for _ in 0..rows {
            builder.keys().append_value("key");
            builder.values().append_value("value");
            builder.append(true).unwrap();
        }
        Arc::new(builder.finish())
    }

    /// A small `traces_schema()` batch: `(trace id byte, start_time, last_ingest_time)`.
    fn traces_batch(rows: &[(u8, i64, i64)]) -> RecordBatch {
        let count = rows.len();
        let project = project().to_string();
        let mut trace_id = FixedSizeBinaryBuilder::with_capacity(count, 16);
        let mut root_span_id = FixedSizeBinaryBuilder::with_capacity(count, 8);
        let mut start_time = timestamp(count);
        let mut end_time = timestamp(count);
        let mut last_ingest_time = timestamp(count);
        for (last, start, ingest) in rows {
            trace_id.append_value(trace(*last).as_bytes()).unwrap();
            root_span_id
                .append_value(span_id(*last).as_bytes())
                .unwrap();
            start_time.append_value(*start);
            end_time.append_value(*start + 10);
            last_ingest_time.append_value(*ingest);
        }
        let columns: Vec<ArrayRef> = vec![
            strings(&project, count),
            Arc::new(trace_id.finish()),
            Arc::new(start_time.finish()),
            Arc::new(end_time.finish()),
            strings("trace", count),
            Arc::new(root_span_id.finish()),
            Arc::new(BooleanArray::from(vec![false; count])),
            strings("ok", count),
            strings("session", count),
            strings("user", count),
            strings("production", count),
            strings("abc", count),
            utf8_list("tag", count),
            metadata_map(count),
            strings("in", count),
            strings("out", count),
            Arc::new(Int64Array::from(vec![1_i64; count])),
            Arc::new(Int64Array::from(vec![0_i64; count])),
            Arc::new(Int64Array::from(vec![0_i64; count])),
            Arc::new(Int64Array::from(vec![1_i64; count])),
            Arc::new(Int64Array::from(vec![1_i64; count])),
            Arc::new(Int64Array::from(vec![1_i64; count])),
            Arc::new(Int64Array::from(vec![1_i64; count])),
            Arc::new(Int64Array::from(vec![1_i64; count])),
            Arc::new(Int64Array::from(vec![1_i64; count])),
            Arc::new(Float64Array::from(vec![0.5_f64; count])),
            utf8_list("model", count),
            Arc::new(last_ingest_time.finish()),
            Arc::new(Int32Array::from(vec![1_i32; count])),
        ];
        RecordBatch::try_new(traces_schema(), columns).unwrap()
    }

    fn assert_columns_equal(actual: &RecordBatch, expected: &RecordBatch) {
        assert_eq!(actual.num_rows(), expected.num_rows());
        assert_eq!(actual.num_columns(), expected.num_columns());
        for index in 0..actual.num_columns() {
            assert_eq!(
                actual.column(index),
                expected.column(index),
                "{}",
                expected.schema().field(index).name()
            );
        }
    }

    fn dictionary_names_follow_the_schema(schema: &arrow::datatypes::Schema, columns: &[&str]) {
        let positions: Vec<_> = columns
            .iter()
            .map(|name| schema.index_of(name).expect(name))
            .collect();
        assert!(positions.windows(2).all(|pair| pair[0] < pair[1]));
        let mut unique = columns.to_vec();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), columns.len());
    }

    #[test]
    fn dictionary_lists_match_the_schemas() {
        dictionary_names_follow_the_schema(spans_schema().as_ref(), SPANS_DICTIONARY_COLUMNS);
        dictionary_names_follow_the_schema(traces_schema().as_ref(), TRACES_DICTIONARY_COLUMNS);
        dictionary_names_follow_the_schema(scores_schema().as_ref(), SCORES_DICTIONARY_COLUMNS);
        assert_eq!(SCHEMA_VERSION, "1");
    }

    #[test]
    fn spans_round_trip_sorted_with_the_file_settings() {
        let batch =
            spans_to_record_batch(&[span(2, 1, 30), span(1, 50, 10), span(1, 20, 40)]).unwrap();
        let written = write_file(FileTable::Spans, 0, &[Arc::new(batch.clone())]).unwrap();
        let expected = sorted_like_the_writer(FileTable::Spans, &batch);
        assert_columns_equal(&read_rows(&written.bytes, &spans_schema()), &expected);
        assert_eq!(written.rows, 3);
        assert_eq!(
            (written.min_time, written.max_time),
            i64_pair(&expected, spans::START_TIME)
        );
        assert_eq!(
            (written.min_ingest_time, written.max_ingest_time),
            i64_pair(&expected, spans::INGEST_TIME)
        );
        assert_file_settings(
            &written.bytes,
            "spans",
            "0",
            &written,
            SPANS_DICTIONARY_COLUMNS,
            &[spans::TRACE_ID, spans::SESSION_ID, spans::USER_ID],
            &[spans::TRACE_ID, spans::START_TIME],
        );
        let trace_bytes = expected
            .column_by_name(spans::TRACE_ID)
            .unwrap()
            .as_fixed_size_binary()
            .value(0)
            .to_vec();
        assert!(bloom_contains(
            &written.bytes,
            spans::TRACE_ID,
            &trace_bytes
        ));
    }

    #[test]
    fn scores_round_trip_sorted_with_the_file_settings() {
        let batch =
            scores_to_record_batch(&[score(20, "accuracy"), score(10, "helpfulness")]).unwrap();
        let written = write_file(FileTable::Scores, 1, &[Arc::new(batch.clone())]).unwrap();
        let expected = sorted_like_the_writer(FileTable::Scores, &batch);
        assert_columns_equal(&read_rows(&written.bytes, &scores_schema()), &expected);
        assert_eq!(
            (written.min_time, written.max_time),
            i64_pair(&expected, scores::CREATED_AT)
        );
        assert_eq!(
            (written.min_ingest_time, written.max_ingest_time),
            i64_pair(&expected, scores::INGEST_TIME)
        );
        assert_file_settings(
            &written.bytes,
            "scores",
            "1",
            &written,
            SCORES_DICTIONARY_COLUMNS,
            &[scores::TRACE_ID, scores::SCORE_ID],
            &[scores::CREATED_AT],
        );
        let trace_bytes = expected
            .column_by_name(scores::TRACE_ID)
            .unwrap()
            .as_fixed_size_binary()
            .value(0)
            .to_vec();
        assert!(bloom_contains(
            &written.bytes,
            scores::TRACE_ID,
            &trace_bytes
        ));
    }

    #[test]
    fn traces_round_trip_sorted_with_the_file_settings() {
        let batch = traces_batch(&[(2, 1, 30), (1, 1, 10), (3, 20, 40)]);
        let written = write_file(FileTable::Traces, 0, &[Arc::new(batch.clone())]).unwrap();
        let expected = sorted_like_the_writer(FileTable::Traces, &batch);
        assert_columns_equal(&read_rows(&written.bytes, &traces_schema()), &expected);
        assert_eq!(written.rows, 3);
        assert_eq!(
            (written.min_time, written.max_time),
            i64_pair(&expected, traces::START_TIME)
        );
        assert_eq!(
            (written.min_ingest_time, written.max_ingest_time),
            i64_pair(&expected, traces::LAST_INGEST_TIME)
        );
        assert_file_settings(
            &written.bytes,
            "traces",
            "0",
            &written,
            TRACES_DICTIONARY_COLUMNS,
            &[traces::TRACE_ID, traces::SESSION_ID, traces::USER_ID],
            &[traces::START_TIME, traces::TRACE_ID],
        );
        let trace_bytes = expected
            .column_by_name(traces::TRACE_ID)
            .unwrap()
            .as_fixed_size_binary()
            .value(0)
            .to_vec();
        assert!(bloom_contains(
            &written.bytes,
            traces::TRACE_ID,
            &trace_bytes
        ));
    }

    #[test]
    fn many_small_rows_use_three_row_groups() {
        let starts: Vec<i64> = (0..1_000).collect();
        let batch = Arc::new(spans_named(&starts, "row"));
        let batches = vec![Arc::clone(&batch); 250];
        let written = write_file(FileTable::Spans, 0, &batches).unwrap();
        assert_eq!(written.rows, 250_000);
        assert_eq!(metadata(&written.bytes).num_row_groups(), 3);
    }

    #[test]
    fn empty_and_foreign_batches_are_rejected() {
        let spans = spans_to_record_batch(&[span(1, 1, 1)]).unwrap();
        let empty = spans.slice(0, 0);
        let error = write_file(FileTable::Spans, 0, &[Arc::new(empty)]).unwrap_err();
        assert!(matches!(error, ParquetWriteError::Invalid { .. }));
        let scores = scores_to_record_batch(&[score(1, "accuracy")]).unwrap();
        let error = write_file(FileTable::Spans, 0, &[Arc::new(scores)]).unwrap_err();
        assert!(matches!(error, ParquetWriteError::Invalid { .. }));
        let error = write_file(FileTable::Spans, 2, &[Arc::new(spans)]).unwrap_err();
        assert!(matches!(error, ParquetWriteError::Invalid { .. }));
    }

    fn i64_pair(batch: &RecordBatch, name: &str) -> (i64, i64) {
        let values = batch
            .column_by_name(name)
            .unwrap()
            .as_primitive::<TimestampNanosecondType>();
        (
            arrow::compute::min(values).unwrap(),
            arrow::compute::max(values).unwrap(),
        )
    }

    fn assert_file_settings(
        bytes: &bytes::Bytes,
        table: &str,
        level: &str,
        written: &super::WrittenFile,
        dictionary: &[&str],
        bloom: &[&str],
        sort: &[&str],
    ) {
        let file = metadata(bytes);
        assert!(file.column_index().is_some());
        assert!(file.offset_index().is_some());
        let pairs = file.file_metadata().key_value_metadata().unwrap();
        let value = |key: &str| {
            pairs
                .iter()
                .find(|pair| pair.key == key)
                .and_then(|pair| pair.value.clone())
                .unwrap()
        };
        assert_eq!(value("llmobs.table"), table);
        assert_eq!(value("llmobs.schema_version"), SCHEMA_VERSION);
        assert_eq!(value("llmobs.level"), level);
        assert_eq!(value("llmobs.min_time"), written.min_time.to_string());
        assert_eq!(value("llmobs.max_time"), written.max_time.to_string());
        assert_eq!(
            value("llmobs.min_ingest_time"),
            written.min_ingest_time.to_string()
        );
        assert_eq!(
            value("llmobs.max_ingest_time"),
            written.max_ingest_time.to_string()
        );
        let tail = &bytes[bytes.len() - 8..];
        let metadata_len = u32::from_le_bytes(tail[..4].try_into().unwrap());
        assert_eq!(&tail[4..], b"PAR1");
        assert_eq!(written.footer_size, u64::from(metadata_len) + 8);

        let dictionary: std::collections::HashSet<_> = dictionary.iter().copied().collect();
        let bloom: std::collections::HashSet<_> = bloom.iter().copied().collect();
        for group_index in 0..file.num_row_groups() {
            let group = file.row_group(group_index);
            let declared: Vec<_> = group
                .sorting_columns()
                .unwrap()
                .iter()
                .map(|column| {
                    group
                        .schema_descr()
                        .column(usize::try_from(column.column_idx).unwrap())
                        .name()
                        .to_owned()
                })
                .collect();
            assert_eq!(declared, sort);
            assert!(
                group
                    .sorting_columns()
                    .unwrap()
                    .iter()
                    .all(|column| { !column.descending && !column.nulls_first })
            );
            for column_index in 0..group.num_columns() {
                let column = group.column(column_index);
                let root = column.column_path().parts()[0].as_str();
                assert!(
                    matches!(column.compression(), Compression::ZSTD(_)),
                    "{root}"
                );
                let encoded = column.encodings().any(|encoding| {
                    matches!(
                        encoding,
                        Encoding::RLE_DICTIONARY | Encoding::PLAIN_DICTIONARY
                    )
                });
                assert_eq!(encoded, dictionary.contains(root), "{root}");
                assert_eq!(
                    column.bloom_filter_offset().is_some(),
                    bloom.contains(root),
                    "{root}"
                );
                assert!(column.offset_index_offset().is_some(), "{root}");
                assert!(column.column_index_offset().is_some(), "{root}");
            }
        }
    }

    fn bloom_contains(bytes: &bytes::Bytes, column: &str, value: &[u8]) -> bool {
        let file = metadata(bytes);
        let cursor = bytes.clone();
        for group_index in 0..file.num_row_groups() {
            let group = file.row_group(group_index);
            for column_index in 0..group.num_columns() {
                let chunk = group.column(column_index);
                let parts = chunk.column_path().parts();
                if parts.len() == 1 && parts[0] == column {
                    let Some(filter) = Sbbf::read_from_column_chunk(chunk, &cursor).unwrap() else {
                        continue;
                    };
                    if filter.check(value) {
                        return true;
                    }
                }
            }
        }
        false
    }
}
