//! Encoding for SQL console rows (D98).
//!
//! Binary is lowercase hex. Maps are objects. Lists are arrays. NaN and ±inf
//! are null. Timestamps with a time zone are UTC with a `Z` suffix; naive
//! timestamps have no suffix. Both use 9 fractional digits.
//!
//! CSV writes those JSON values as RFC 4180 fields. NDJSON writes one object
//! per line. Parquet writes the Arrow batches with ZSTD level 1.

use std::sync::Arc;

use arrow::compute::cast;
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::error::ArrowError;
use arrow::json::{ArrayWriter, LineDelimitedWriter, WriterBuilder};
use arrow::record_batch::RecordBatch;
use bytes::Bytes;
use datafusion::error::DataFusionError;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde_json::{Map, Value};

use crate::query_error::RunError;

/// Flush a parquet row group once its encoded size passes this.
const PARQUET_CHUNK_BYTES: usize = 32 * 1024 * 1024;

/// Rows per Parquet row group in exports (parquet's default).
const PARQUET_ROW_GROUP_ROWS: usize = 1024 * 1024;

/// Encode `batches` as JSON objects, one per row, in column order.
///
/// # Errors
///
/// Returns [`RunError::Internal`] when a timestamp cannot be normalized or the
/// JSON writer fails. The message does not include cell values.
pub(crate) fn json_rows(
    batches: &[RecordBatch],
    schema: &Schema,
) -> Result<Vec<Map<String, Value>>, RunError> {
    if batches.iter().all(|batch| batch.num_rows() == 0) {
        return Ok(Vec::new());
    }
    let normalized = batches
        .iter()
        .map(|batch| normalize_timestamps(batch, schema))
        .collect::<Result<Vec<_>, _>>()?;
    let mut buffer = Vec::new();
    let mut writer: ArrayWriter<&mut Vec<u8>> = json_writer().build(&mut buffer);
    for batch in &normalized {
        writer
            .write(batch)
            .map_err(|error| encoding_failure("failed to encode rows", error))?;
    }
    writer
        .finish()
        .map_err(|error| encoding_failure("failed to finish encoded rows", error))?;
    drop(writer);
    serde_json::from_slice(&buffer).map_err(|_| {
        RunError::Internal(DataFusionError::Internal(
            "failed to read encoded rows".into(),
        ))
    })
}

/// Cast every timestamp column to nanoseconds. A zoned value becomes UTC.
fn normalize_timestamps(batch: &RecordBatch, schema: &Schema) -> Result<RecordBatch, RunError> {
    if batch.schema().fields().len() != schema.fields().len() {
        return Err(RunError::Internal(DataFusionError::Internal(
            "encoded batch does not match the output schema".into(),
        )));
    }
    let mut fields: Vec<Arc<Field>> = schema.fields().iter().cloned().collect();
    let mut columns = batch.columns().to_vec();
    for (index, field) in schema.fields().iter().enumerate() {
        let DataType::Timestamp(_, zone) = field.data_type() else {
            continue;
        };
        let target = DataType::Timestamp(
            TimeUnit::Nanosecond,
            zone.as_ref().map(|_| Arc::<str>::from("UTC")),
        );
        if field.data_type() == &target {
            continue;
        }
        let casted = cast(columns[index].as_ref(), &target)
            .map_err(|error| encoding_failure("failed to normalize a timestamp", error))?;
        fields[index] = Arc::new(
            Field::new(field.name(), target, field.is_nullable())
                .with_metadata(field.metadata().clone()),
        );
        columns[index] = casted;
    }
    let schema = Arc::new(Schema::new_with_metadata(fields, schema.metadata().clone()));
    RecordBatch::try_new(schema, columns)
        .map_err(|error| encoding_failure("failed to rebuild a batch", error))
}

fn encoding_failure(message: &str, error: ArrowError) -> RunError {
    RunError::Internal(
        DataFusionError::ArrowError(Box::new(error), None).context(message.to_owned()),
    )
}

fn json_writer() -> WriterBuilder {
    WriterBuilder::new()
        .with_explicit_nulls(true)
        .with_timestamp_tz_format("%Y-%m-%dT%H:%M:%S%.9fZ".to_owned())
        .with_timestamp_format("%Y-%m-%dT%H:%M:%S%.9f".to_owned())
}

/// Header line for a CSV export. Names are RFC 4180-quoted.
pub(crate) fn csv_header(schema: &Schema) -> Bytes {
    let mut line = String::new();
    for (index, field) in schema.fields().iter().enumerate() {
        if index > 0 {
            line.push(',');
        }
        push_csv_field(&mut line, field.name());
    }
    line.push_str("\r\n");
    Bytes::from(line)
}

/// One CSV chunk for `batch`. Null is an empty field, strings are their text,
/// numbers and bools are JSON text, and arrays and objects are JSON text.
///
/// # Errors
///
/// Returns [`RunError::Internal`] when JSON encoding fails.
pub(crate) fn csv_rows(batch: &RecordBatch, schema: &Schema) -> Result<Option<Bytes>, RunError> {
    let rows = json_rows(std::slice::from_ref(batch), schema)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let names: Vec<&str> = schema
        .fields()
        .iter()
        .map(|field| field.name().as_str())
        .collect();
    let mut body = String::new();
    for row in &rows {
        for (index, name) in names.iter().enumerate() {
            if index > 0 {
                body.push(',');
            }
            push_csv_field(&mut body, &csv_text(&row[*name]));
        }
        body.push_str("\r\n");
    }
    Ok(Some(Bytes::from(body)))
}

fn csv_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(value) => value.to_string(),
        other => other.to_string(),
    }
}

fn push_csv_field(out: &mut String, text: &str) {
    if text.contains(['"', ',', '\r', '\n']) {
        out.push('"');
        out.push_str(&text.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(text);
    }
}

/// One NDJSON chunk for `batch`, using the same timestamp normalization as JSON rows.
///
/// # Errors
///
/// Returns [`RunError::Internal`] when encoding fails.
pub(crate) fn ndjson_rows(batch: &RecordBatch, schema: &Schema) -> Result<Option<Bytes>, RunError> {
    if batch.num_rows() == 0 {
        return Ok(None);
    }
    let batch = normalize_timestamps(batch, schema)?;
    let mut buffer = Vec::new();
    let mut writer: LineDelimitedWriter<&mut Vec<u8>> = json_writer().build(&mut buffer);
    writer
        .write(&batch)
        .map_err(|error| encoding_failure("failed to encode rows", error))?;
    writer
        .finish()
        .map_err(|error| encoding_failure("failed to finish encoded rows", error))?;
    drop(writer);
    Ok(Some(Bytes::from(buffer)))
}

/// Parquet writer that yields a chunk after each flushed row group.
pub(crate) struct ParquetEncoder {
    writer: ArrowWriter<Vec<u8>>,
    /// Row groups whose bytes were already returned.
    taken_row_groups: usize,
}

impl ParquetEncoder {
    /// # Errors
    ///
    /// Returns [`RunError::Internal`] when the Parquet writer cannot be created.
    pub(crate) fn new(schema: &Schema) -> Result<Self, RunError> {
        Self::with_row_group_rows(schema, PARQUET_ROW_GROUP_ROWS)
    }

    /// # Errors
    ///
    /// Returns [`RunError::Internal`] when the Parquet writer cannot be created.
    fn with_row_group_rows(schema: &Schema, rows: usize) -> Result<Self, RunError> {
        // Callers pass `PARQUET_ROW_GROUP_ROWS` or a test value; `set_max_row_group_row_count` panics on 0.
        debug_assert!(rows > 0);
        // `ZstdLevel::default` is level 1.
        let properties = WriterProperties::builder()
            .set_compression(Compression::ZSTD(ZstdLevel::default()))
            .set_max_row_group_row_count(Some(rows))
            .build();
        let writer = ArrowWriter::try_new(Vec::new(), Arc::new(schema.clone()), Some(properties))
            .map_err(parquet_failure)?;
        Ok(Self {
            writer,
            taken_row_groups: 0,
        })
    }

    /// Write `batch`. Returns the bytes of every row group completed since the
    /// last chunk: one closed by the writer at the row limit, or the open one
    /// once it passes 32 MiB.
    ///
    /// # Errors
    ///
    /// Returns [`RunError::Internal`] when the Parquet writer fails.
    pub(crate) fn push(&mut self, batch: &RecordBatch) -> Result<Option<Bytes>, RunError> {
        self.writer.write(batch).map_err(parquet_failure)?;
        if self.writer.in_progress_size() > PARQUET_CHUNK_BYTES {
            self.writer.flush().map_err(parquet_failure)?;
        }
        if self.writer.flushed_row_groups().len() == self.taken_row_groups {
            return Ok(None);
        }
        self.taken_row_groups = self.writer.flushed_row_groups().len();
        self.take_bytes()
    }

    /// Write the footer and return every byte still buffered.
    ///
    /// # Errors
    ///
    /// Returns [`RunError::Internal`] when the footer cannot be written.
    pub(crate) fn finish(mut self) -> Result<Bytes, RunError> {
        // `close` consumes the writer, so the footer is written with `finish`
        // (the same work) and the buffer is then taken. `flush` of a row group
        // does not flush `TrackedWrite`'s `BufWriter`; `sync` does that first,
        // otherwise `inner_mut` is missing the tail of the chunk.
        self.writer.finish().map_err(parquet_failure)?;
        self.writer.sync().map_err(io_failure)?;
        Ok(Bytes::from(std::mem::take(self.writer.inner_mut())))
    }

    fn take_bytes(&mut self) -> Result<Option<Bytes>, RunError> {
        self.writer.sync().map_err(io_failure)?;
        let bytes = std::mem::take(self.writer.inner_mut());
        Ok((!bytes.is_empty()).then(|| Bytes::from(bytes)))
    }
}

fn parquet_failure(error: parquet::errors::ParquetError) -> RunError {
    RunError::Internal(DataFusionError::ParquetError(Box::new(error)))
}

fn io_failure(error: std::io::Error) -> RunError {
    RunError::Internal(DataFusionError::IoError(error))
}

#[cfg(test)]
mod tests {
    use arrow::array::Int64Array;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    use super::*;

    fn int_schema() -> Schema {
        Schema::new(vec![Field::new("n", DataType::Int64, false)])
    }

    fn int_batch(schema: &Schema, values: std::ops::Range<i64>) -> RecordBatch {
        let array = Int64Array::from(values.collect::<Vec<_>>());
        RecordBatch::try_new(Arc::new(schema.clone()), vec![Arc::new(array)]).expect("batch")
    }

    fn read_back(file: Bytes) -> (usize, Vec<i64>) {
        let builder = ParquetRecordBatchReaderBuilder::try_new(file).expect("parquet");
        let groups = builder.metadata().num_row_groups();
        let values = builder
            .build()
            .expect("reader")
            .map(|batch| batch.expect("batch"))
            .flat_map(|batch| {
                batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .expect("int64")
                    .values()
                    .to_vec()
            })
            .collect();
        (groups, values)
    }

    #[test]
    fn parquet_chunks_follow_row_groups() {
        let schema = int_schema();
        let mut encoder = ParquetEncoder::with_row_group_rows(&schema, 1_000).expect("encoder");
        let mut file = Vec::new();
        for index in 0..10 {
            let batch = int_batch(&schema, index * 1_000..index * 1_000 + 1_000);
            let chunk = encoder
                .push(&batch)
                .expect("push")
                .expect("each batch completes one row group");
            assert!(!chunk.is_empty());
            file.extend_from_slice(&chunk);
        }
        file.extend_from_slice(&encoder.finish().expect("finish"));

        let (groups, values) = read_back(Bytes::from(file));
        assert_eq!(groups, 10);
        assert_eq!(values, (0..10_000).collect::<Vec<i64>>());
    }

    #[test]
    fn a_partial_row_group_waits_for_finish() {
        let schema = int_schema();
        let mut encoder = ParquetEncoder::with_row_group_rows(&schema, 1_000).expect("encoder");
        assert!(
            encoder
                .push(&int_batch(&schema, 0..999))
                .expect("push")
                .is_none()
        );
        let chunk = encoder
            .push(&int_batch(&schema, 999..1_001))
            .expect("push")
            .expect("the group closes at 1,000 rows");
        let mut file = chunk.to_vec();
        file.extend_from_slice(&encoder.finish().expect("finish"));

        let (groups, values) = read_back(Bytes::from(file));
        assert_eq!(groups, 2);
        assert_eq!(values, (0..1_001).collect::<Vec<i64>>());
    }
}
