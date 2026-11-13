//! Time slices of one compaction job (spec 03 § Slicing).

use std::sync::Arc;

use arrow::array::{Array, AsArray};
use arrow::datatypes::TimestampNanosecondType;
use datafusion::error::DataFusionError;
use futures::StreamExt;
use object_store::ObjectStore;
use object_store::path::Path as ObjectPath;
use parquet::arrow::ProjectionMask;
#[allow(deprecated)]
use parquet::arrow::async_reader::ParquetObjectReader;
use parquet::arrow::async_reader::ParquetRecordBatchStreamBuilder;

use crate::catalog::FileMeta;
use crate::error::CompactionError;

/// Rows with `lower <= time < upper`. `None` is unbounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Slice {
    /// Inclusive lower bound.
    pub(crate) lower: Option<i64>,
    /// Exclusive upper bound.
    pub(crate) upper: Option<i64>,
}

/// `max(1, ceil(input_bytes / target))`. `target` is at least 1.
pub(crate) fn slice_count(input_bytes: u64, target: u64) -> usize {
    let slices = input_bytes.div_ceil(target.max(1));
    usize::try_from(slices).unwrap_or(usize::MAX).max(1)
}

/// Slices from the sorted time values of every input row (spec 03 § Slicing).
///
/// The candidate boundaries are the values at ranks `⌊len × i / count⌋`
/// (`i` = 1 … `count` − 1). A candidate equal to the minimum, or to the
/// previous kept boundary, is dropped, which leaves strictly increasing
/// boundaries above the minimum. Every slice is non-empty: the first holds the
/// minimum, and each later one holds its own lower bound.
pub(crate) fn slices(sorted: &[i64], count: usize) -> Vec<Slice> {
    if count <= 1 || sorted.is_empty() {
        return vec![Slice {
            lower: None,
            upper: None,
        }];
    }
    let len = sorted.len();
    let mut boundaries: Vec<i64> = Vec::new();
    for index in 1..count {
        let candidate = sorted[len * index / count];
        if candidate == sorted[0] {
            continue;
        }
        if boundaries.last() == Some(&candidate) {
            continue;
        }
        boundaries.push(candidate);
    }
    let mut result = Vec::with_capacity(boundaries.len() + 1);
    let mut lower = None;
    for boundary in boundaries {
        result.push(Slice {
            lower,
            upper: Some(boundary),
        });
        lower = Some(boundary);
    }
    result.push(Slice { lower, upper: None });
    result
}

/// The `column` values of every row of `files`, sorted ascending.
///
/// Memory is 8 bytes per input row, bounded by the job's input size.
pub(crate) async fn time_values(
    store: Arc<dyn ObjectStore>,
    files: &[Arc<FileMeta>],
    column: &str,
) -> Result<Vec<i64>, CompactionError> {
    let mut values = Vec::new();
    for file in files {
        #[allow(deprecated)]
        let reader =
            ParquetObjectReader::new(Arc::clone(&store), ObjectPath::from(file.path.as_str()))
                .with_file_size(file.size);
        let builder = ParquetRecordBatchStreamBuilder::new(reader)
            .await
            .map_err(parquet_error)?;
        let mask = ProjectionMask::columns(builder.parquet_schema(), [column]);
        let mut stream = builder
            .with_projection(mask)
            .build()
            .map_err(parquet_error)?;
        while let Some(batch) = stream.next().await {
            let batch = batch.map_err(parquet_error)?;
            let Some(times) = batch
                .column_by_name(column)
                .and_then(|values| values.as_primitive_opt::<TimestampNanosecondType>())
            else {
                return Err(CompactionError::Input {
                    reason: "missing time column",
                });
            };
            for row in 0..batch.num_rows() {
                if times.is_null(row) {
                    return Err(CompactionError::Input {
                        reason: "missing time column",
                    });
                }
                values.push(times.value(row));
            }
        }
    }
    values.sort_unstable();
    Ok(values)
}

fn parquet_error(source: parquet::errors::ParquetError) -> CompactionError {
    CompactionError::Read {
        source: DataFusionError::ParquetError(Box::new(source)),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow::array::RecordBatch;
    use object_store::ObjectStore;
    use object_store::ObjectStoreExt;
    use object_store::memory::InMemory;

    use super::{Slice, slice_count, slices, time_values};
    use crate::catalog::{FileMeta, FileTable};
    use crate::parquet::write_file;
    use crate::test_support::{span_row, span_rows};

    fn project() -> llmobs_core::ids::ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn day() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 11, 11).unwrap()
    }

    /// Writes one spans file of `starts` into `store` and returns its metadata.
    async fn spans_input(store: &Arc<dyn ObjectStore>, starts: &[i64]) -> FileMeta {
        let spans: Vec<_> = starts
            .iter()
            .enumerate()
            .map(|(index, start)| {
                span_row(0, u8::try_from(index).unwrap_or(0), *start, start + 200)
            })
            .collect();
        let written = write_file(FileTable::Spans, 0, &[Arc::new(span_rows(&spans))]).unwrap();
        let (path, id) =
            crate::objects::Objects::table_file_path(FileTable::Spans, project(), day(), 0);
        store
            .put(
                &object_store::path::Path::from(path.as_str()),
                object_store::PutPayload::from(written.bytes.clone()),
            )
            .await
            .unwrap();
        FileMeta {
            id,
            table: FileTable::Spans,
            project: project(),
            day: day(),
            level: 0,
            path,
            size: u64::try_from(written.bytes.len()).unwrap_or(u64::MAX),
            footer_size: written.footer_size,
            rows: written.rows,
            min_time: written.min_time,
            max_time: written.max_time,
            min_ingest_time: written.min_ingest_time,
            max_ingest_time: written.max_ingest_time,
            fully_merged: false,
        }
    }

    fn read_spans(bytes: bytes::Bytes) -> RecordBatch {
        use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        let builder = ParquetRecordBatchReaderBuilder::try_new(bytes).unwrap();
        let schema = Arc::clone(builder.schema());
        let batches: Vec<RecordBatch> = builder
            .build()
            .unwrap()
            .map(|batch| batch.unwrap())
            .collect();
        arrow::compute::concat_batches(&schema, &batches).unwrap()
    }

    /// Reads the store back for one file, so the test can see what was written.
    async fn store_bytes(store: &Arc<dyn ObjectStore>, meta: &FileMeta) -> bytes::Bytes {
        store
            .get(&object_store::path::Path::from(meta.path.as_str()))
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
    }

    #[test]
    fn slice_count_is_the_ceiling_of_the_ratio() {
        let target = 512_u64;
        assert_eq!(slice_count(0, target), 1);
        assert_eq!(slice_count(target, target), 1);
        assert_eq!(slice_count(target + 1, target), 2);
        assert_eq!(slice_count(10 * target, target), 10);
    }

    #[test]
    fn one_count_or_no_values_give_one_open_slice() {
        let open = vec![Slice {
            lower: None,
            upper: None,
        }];
        assert_eq!(slices(&[1, 2, 3], 1), open);
        assert_eq!(slices(&[], 4), open);
    }

    #[test]
    fn a_hundred_values_in_four_slices() {
        let values: Vec<i64> = (1..=100).collect();
        assert_eq!(
            slices(&values, 4),
            vec![
                Slice {
                    lower: None,
                    upper: Some(26)
                },
                Slice {
                    lower: Some(26),
                    upper: Some(51)
                },
                Slice {
                    lower: Some(51),
                    upper: Some(76)
                },
                Slice {
                    lower: Some(76),
                    upper: None
                },
            ]
        );
    }

    #[test]
    fn equal_values_and_duplicate_candidates_collapse() {
        // All-equal values: every candidate equals the minimum.
        assert_eq!(
            slices(&[5, 5, 5, 5], 4),
            vec![Slice {
                lower: None,
                upper: None
            }]
        );
        // The candidate at rank 2 equals the minimum and is dropped.
        assert_eq!(
            slices(&[1, 1, 1, 2, 3], 2),
            vec![Slice {
                lower: None,
                upper: None
            }]
        );
        // Two candidates of 5 collapse into one boundary.
        assert_eq!(
            slices(&[1, 2, 5, 5, 5], 4),
            vec![
                Slice {
                    lower: None,
                    upper: Some(2)
                },
                Slice {
                    lower: Some(2),
                    upper: Some(5)
                },
                Slice {
                    lower: Some(5),
                    upper: None
                },
            ]
        );
    }

    #[tokio::test]
    async fn time_values_returns_every_start_time_sorted() {
        let store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let first = spans_input(&store, &[30, 10]).await;
        let second = spans_input(&store, &[20]).await;
        // The files really hold the rows this reads back.
        let one = read_spans(store_bytes(&store, &first).await);
        let two = read_spans(store_bytes(&store, &second).await);
        assert_eq!(one.num_rows() + two.num_rows(), 3);
        let values = time_values(
            Arc::clone(&store),
            &[Arc::new(first), Arc::new(second)],
            llmobs_core::schema::spans::START_TIME,
        )
        .await
        .unwrap();
        assert_eq!(values, vec![10, 20, 30]);
    }
}
