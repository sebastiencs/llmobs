//! JSON encoding for SQL console rows (D98).
//!
//! Binary is lowercase hex. Maps are objects. Lists are arrays. NaN and ±inf
//! are null. Timestamps with a time zone are UTC with a `Z` suffix; naive
//! timestamps have no suffix. Both use 9 fractional digits.

use std::sync::Arc;

use arrow::compute::cast;
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::error::ArrowError;
use arrow::json::{ArrayWriter, WriterBuilder};
use arrow::record_batch::RecordBatch;
use datafusion::error::DataFusionError;
use serde_json::{Map, Value};

use crate::query_error::RunError;

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
    let mut writer: ArrayWriter<&mut Vec<u8>> = WriterBuilder::new()
        .with_explicit_nulls(true)
        .with_timestamp_tz_format("%Y-%m-%dT%H:%M:%S%.9fZ".to_owned())
        .with_timestamp_format("%Y-%m-%dT%H:%M:%S%.9f".to_owned())
        .build(&mut buffer);
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
