//! Collapsing duplicate rows before they are written to a file.
//!
//! Flush and compaction share these: a flush deduplicates the rows of one
//! frozen partition, a merge deduplicates each chunk of merged rows (spec 03
//! §2, §5).

use std::collections::HashMap;

use arrow::array::{Array, AsArray, RecordBatch, UInt32Array};
use arrow::compute::take_record_batch;
use arrow::datatypes::TimestampNanosecondType;

use crate::error::DedupError;

/// Latest version of each span: the greatest `ingest_time` per `(trace_id, span_id)`.
/// An equal `ingest_time` keeps the later row. Kept rows keep their input order.
pub(crate) fn latest_spans(batch: &RecordBatch) -> Result<RecordBatch, DedupError> {
    let trace = fixed(batch, llmobs_core::schema::spans::TRACE_ID)?;
    let span = fixed(batch, llmobs_core::schema::spans::SPAN_ID)?;
    let ingest = batch
        .column_by_name(llmobs_core::schema::spans::INGEST_TIME)
        .and_then(|column| column.as_primitive_opt::<TimestampNanosecondType>())
        .ok_or_else(|| DedupError::new("missing identity column"))?;
    let mut best: HashMap<(Vec<u8>, Vec<u8>), (i64, usize)> = HashMap::new();
    for row in 0..batch.num_rows() {
        if trace.is_null(row) || span.is_null(row) || ingest.is_null(row) {
            return Err(DedupError::new("null span identity"));
        }
        let key = (trace.value(row).to_vec(), span.value(row).to_vec());
        let time = ingest.value(row);
        match best.get(&key) {
            Some((kept, _)) if *kept > time => {}
            _ => {
                best.insert(key, (time, row));
            }
        }
    }
    let mut rows = Vec::with_capacity(best.len());
    for (_, row) in best.into_values() {
        rows.push(u32::try_from(row).map_err(|_| DedupError::new("row index"))?);
    }
    rows.sort_unstable();
    take_record_batch(batch, &UInt32Array::from(rows)).map_err(|source| {
        // The error text can name columns, never values.
        tracing::warn!(%source, "taking deduplicated rows failed");
        DedupError::new("arrow")
    })
}

fn fixed<'a>(
    batch: &'a RecordBatch,
    name: &str,
) -> Result<&'a arrow::array::FixedSizeBinaryArray, DedupError> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_fixed_size_binary_opt())
        .ok_or_else(|| DedupError::new("missing identity column"))
}

#[cfg(test)]
mod tests {
    use arrow::array::{Array, AsArray, FixedSizeBinaryBuilder, RecordBatch};
    use llmobs_core::schema::{spans, spans_schema};

    use super::latest_spans;
    use crate::wal::spans_keyed;

    fn names(batch: &RecordBatch) -> Vec<String> {
        let column = batch
            .column_by_name(spans::NAME)
            .unwrap()
            .as_any()
            .downcast_ref::<arrow::array::StringArray>()
            .unwrap();
        (0..column.len())
            .map(|index| column.value(index).to_owned())
            .collect()
    }

    fn starts(batch: &RecordBatch) -> Vec<i64> {
        let column = batch
            .column_by_name(spans::START_TIME)
            .unwrap()
            .as_primitive_opt::<arrow::datatypes::TimestampNanosecondType>()
            .unwrap();
        (0..column.len()).map(|index| column.value(index)).collect()
    }

    #[test]
    fn the_greatest_ingest_time_wins_in_either_order() {
        let older = spans_keyed(&[10, 11], "older", 1);
        let newer = spans_keyed(&[50, 51], "newer", 1);
        let either = |first: &RecordBatch, second: &RecordBatch| {
            let schema = first.schema();
            let combined = arrow::compute::concat_batches(&schema, [first, second]).unwrap();
            names(&latest_spans(&combined).unwrap())
        };
        assert_eq!(either(&older, &newer), vec!["newer".to_owned(); 2]);
        assert_eq!(either(&newer, &older), vec!["newer".to_owned(); 2]);
    }

    #[test]
    fn an_equal_ingest_time_keeps_the_later_row() {
        let first = spans_keyed(&[10, 11], "first", 1);
        let second = spans_keyed(&[10, 11], "second", 1);
        let schema = first.schema();
        let combined = arrow::compute::concat_batches(&schema, [&first, &second]).unwrap();
        assert_eq!(
            names(&latest_spans(&combined).unwrap()),
            vec!["second".to_owned(); 2]
        );
    }

    #[test]
    fn kept_rows_stay_in_input_order() {
        let older = spans_keyed(&[50, 40], "a", 1);
        let newer = spans_keyed(&[45], "b", 2);
        let schema = older.schema();
        let combined = arrow::compute::concat_batches(&schema, [&older, &newer]).unwrap();
        let kept = latest_spans(&combined).unwrap();
        // `45` (id 2, later) beats `40`, and the survivors keep their input
        // order, not start-time order.
        assert_eq!(starts(&kept), vec![50, 45]);
    }

    #[test]
    fn a_null_span_id_is_an_error() {
        // A null cannot exist in a schema-conformant batch; this models corrupt
        // input, so `span_id` is declared nullable to build it.
        let fields: Vec<_> = spans_schema()
            .fields()
            .iter()
            .map(|field| {
                if field.name() == spans::SPAN_ID {
                    field.as_ref().clone().with_nullable(true)
                } else {
                    field.as_ref().clone()
                }
            })
            .collect();
        let schema = std::sync::Arc::new(arrow::datatypes::Schema::new(fields));
        let source = spans_keyed(&[10], "row", 1);
        let mut columns = source.columns().to_vec();
        let index = source.schema().index_of(spans::SPAN_ID).unwrap();
        let mut nulls = FixedSizeBinaryBuilder::with_capacity(1, 8);
        nulls.append_null();
        columns[index] = std::sync::Arc::new(nulls.finish());
        let batch = RecordBatch::try_new(schema, columns).unwrap();
        let error = latest_spans(&batch).unwrap_err();
        assert_eq!(error.reason(), "null span identity");
    }
}
