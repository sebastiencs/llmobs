//! The streaming k-way merge of one time slice (spec 03 § During a merge).
//!
//! The inputs are sorted runs, so DataFusion's `SortPreservingMergeExec`
//! merges them with one batch per input in memory. Duplicates are collapsed
//! per group (a trace for spans, a `created_at` value for scores), so a group
//! is never split across chunks.

use std::io::Write;
use std::sync::Arc;

use arrow::array::{Array, AsArray, RecordBatch};
use arrow::compute::concat_batches;
use arrow::datatypes::{DataType, TimestampNanosecondType};
use datafusion::common::ScalarValue;
use datafusion::datasource::physical_plan::ParquetSource;
use datafusion::error::DataFusionError;
use datafusion::execution::TaskContext;
use datafusion::execution::context::SessionConfig;
use datafusion::execution::object_store::ObjectStoreUrl;
use datafusion::execution::runtime_env::RuntimeEnv;
use datafusion::logical_expr::Operator;
use datafusion::physical_expr::LexOrdering;
use datafusion::physical_expr::PhysicalSortExpr;
use datafusion::physical_expr::expressions::{BinaryExpr, Column, Literal};
use datafusion::physical_plan::ExecutionPlan;
use datafusion::physical_plan::execute_stream;
use datafusion::physical_plan::filter::FilterExec;
use datafusion::physical_plan::sorts::sort_preserving_merge::SortPreservingMergeExec;
use datafusion_datasource::PartitionedFile;
use datafusion_datasource::file_groups::FileGroup;
use datafusion_datasource::file_scan_config::FileScanConfigBuilder;
use datafusion_datasource::source::DataSourceExec;
use futures::StreamExt;
use llmobs_core::schema::{scores, scores_schema, spans, spans_schema};

use crate::catalog::{FileMeta, FileTable};
use crate::compaction::slice::Slice;
use crate::dedup;
use crate::error::CompactionError;
use crate::objects::OBJECT_STORE_URL;
use crate::parquet::{FileWriter, sort_columns, time_column};
use crate::partials::partials_from_spans;

/// Process-wide settings of the merge, built once at open (M4.6) and by tests.
pub(crate) struct MergeContext {
    /// Memory pool `compaction.max_memory` and the object store (`Objects::register`).
    pub(crate) runtime: Arc<RuntimeEnv>,
    /// Rows per batch read from the inputs.
    pub(crate) batch_size: usize,
}

/// What `merge_slice` produced.
pub(crate) struct MergedSlice<W: Write + Send> {
    /// The writer the rows went into.
    pub(crate) writer: FileWriter<W>,
    /// `spans` only: one partial batch per emitted chunk (`traces_schema()`).
    pub(crate) partials: Vec<RecordBatch>,
    /// Rows written to `writer`.
    pub(crate) rows: u64,
}

/// Merges `inputs` (one partition of `table`, `Spans` or `Scores`) restricted
/// to `slice` into `writer`.
///
/// For `spans`, the returned partials are the trace partials of the emitted
/// rows, one batch per chunk. `drop_deleted` is ignored for spans.
pub(crate) async fn merge_slice<W: Write + Send + 'static>(
    ctx: &MergeContext,
    table: FileTable,
    inputs: &[Arc<FileMeta>],
    slice: Slice,
    drop_deleted: bool,
    writer: FileWriter<W>,
) -> Result<MergedSlice<W>, CompactionError> {
    if table == FileTable::Traces {
        return Err(CompactionError::Input {
            reason: "traces are not merged",
        });
    }
    let mut files: Vec<&Arc<FileMeta>> = inputs.iter().collect();
    // UUIDv7, so oldest first; ties between equal sort keys keep input order.
    files.sort_unstable_by_key(|file| file.id);
    let plan = merge_plan(table, &files, slice)?;
    let task = Arc::new(
        TaskContext::default()
            .with_session_config(SessionConfig::new().with_batch_size(ctx.batch_size))
            .with_runtime(Arc::clone(&ctx.runtime)),
    );
    let mut stream = execute_stream(Arc::clone(&plan), task).map_err(read_error)?;

    let mut writer = writer;
    let mut partials = Vec::new();
    let mut rows = 0_u64;
    let mut pending: Option<RecordBatch> = None;
    while let Some(item) = stream.next().await {
        let batch = item.map_err(read_error)?;
        let combined = match pending.take() {
            Some(pending) => {
                concat_batches(&batch.schema(), [&pending, &batch]).map_err(arrow_error)?
            }
            None => batch,
        };
        if combined.num_rows() == 0 {
            continue;
        }
        // The first row index whose group equals the last row's group, found by
        // scanning back from the end: a group's rows are contiguous, but a
        // group can straddle two batches.
        let last = combined.num_rows() - 1;
        let mut split = last;
        while split > 0 && same_group(table, &combined, split - 1, last)? {
            split -= 1;
        }
        if split > 0 {
            let chunk = combined.slice(0, split);
            let tail = combined.slice(split, combined.num_rows() - split);
            let (next_writer, next_partials, written) =
                emit_chunk(table, drop_deleted, writer, chunk, partials).await?;
            writer = next_writer;
            partials = next_partials;
            rows = rows.saturating_add(written);
            pending = Some(tail);
        } else {
            pending = Some(combined);
        }
    }
    if let Some(tail) = pending
        && tail.num_rows() > 0
    {
        let (next_writer, next_partials, written) =
            emit_chunk(table, drop_deleted, writer, tail, partials).await?;
        writer = next_writer;
        partials = next_partials;
        rows = rows.saturating_add(written);
    }
    Ok(MergedSlice {
        writer,
        partials,
        rows,
    })
}

/// The streaming plan of one slice: one file group per input, a Parquet scan
/// per group in the table's sort order, the slice predicate, and one
/// sort-preserving k-way merge over them. Built by hand; there is no SQL.
pub(crate) fn merge_plan(
    table: FileTable,
    inputs: &[&Arc<FileMeta>],
    slice: Slice,
) -> Result<Arc<dyn ExecutionPlan>, CompactionError> {
    if table == FileTable::Traces {
        return Err(CompactionError::Input {
            reason: "traces are not merged",
        });
    }
    let schema = match table {
        FileTable::Spans => spans_schema(),
        FileTable::Scores => scores_schema(),
        FileTable::Traces => unreachable!("rejected above"),
    };
    let ordering = ordering_of(table, &schema)?;
    let bounded = slice.lower.is_some() || slice.upper.is_some();
    let mut source = ParquetSource::new(Arc::clone(&schema)).with_pushdown_filters(true);
    let mut filter = None;
    if bounded {
        let predicate = slice_predicate(table, &schema, slice)?;
        // The source predicate only prunes; the `FilterExec` makes the slice
        // exact, and it keeps the input order.
        source = source.with_predicate(Arc::clone(&predicate));
        filter = Some(predicate);
    }
    let scan = file_scan(inputs, source)?;
    let input: Arc<dyn ExecutionPlan> = match filter {
        Some(predicate) => Arc::new(
            FilterExec::try_new(predicate, scan)
                .map_err(|source| CompactionError::Read { source })?,
        ),
        None => scan,
    };
    let merge = SortPreservingMergeExec::new(ordering, input).with_round_robin_repartition(false);
    Ok(Arc::new(merge))
}

/// The scan over every input, one file per group: a group with several files
/// would concatenate them, so it would not be sorted.
fn file_scan(
    inputs: &[&Arc<FileMeta>],
    source: ParquetSource,
) -> Result<Arc<dyn ExecutionPlan>, CompactionError> {
    let groups: Vec<FileGroup> = inputs
        .iter()
        .map(|meta| {
            let mut file = PartitionedFile::new(meta.path.clone(), meta.size);
            if let Ok(hint) = usize::try_from(meta.footer_size) {
                file = file.with_metadata_size_hint(hint);
            }
            FileGroup::new(vec![file])
        })
        .collect();
    let url = ObjectStoreUrl::parse(OBJECT_STORE_URL).map_err(read_error)?;
    let builder = FileScanConfigBuilder::new(url, Arc::new(source)).with_file_groups(groups);
    Ok(DataSourceExec::from_data_source(builder.build()))
}

/// The table's `sort_columns` as physical sort expressions, in the order the
/// writer declares.
fn ordering_of(
    table: FileTable,
    schema: &arrow::datatypes::SchemaRef,
) -> Result<LexOrdering, CompactionError> {
    let mut exprs = Vec::new();
    for name in sort_columns(table) {
        let index = schema.index_of(name).map_err(|_| CompactionError::Input {
            reason: "sort column is missing",
        })?;
        exprs.push(PhysicalSortExpr {
            expr: Arc::new(Column::new(name, index)),
            options: arrow::compute::SortOptions {
                descending: false,
                nulls_first: false,
            },
        });
    }
    LexOrdering::new(exprs).ok_or_else(|| CompactionError::Input {
        reason: "empty ordering",
    })
}

/// `time >= lower AND time < upper`, either side present only when bounded.
fn slice_predicate(
    table: FileTable,
    schema: &arrow::datatypes::SchemaRef,
    slice: Slice,
) -> Result<Arc<dyn datafusion::physical_expr::PhysicalExpr>, CompactionError> {
    let name = time_column(table);
    let index = schema.index_of(name).map_err(|_| CompactionError::Input {
        reason: "time column is missing",
    })?;
    let tz = match schema.field(index).data_type() {
        DataType::Timestamp(_, tz) => tz.clone(),
        _ => None,
    };
    let column =
        Arc::new(Column::new(name, index)) as Arc<dyn datafusion::physical_expr::PhysicalExpr>;
    let mut bounds = Vec::new();
    if let Some(lower) = slice.lower {
        bounds.push(Arc::new(BinaryExpr::new(
            Arc::clone(&column),
            Operator::GtEq,
            Arc::new(Literal::new(ScalarValue::TimestampNanosecond(
                Some(lower),
                tz.clone(),
            ))),
        ))
            as Arc<dyn datafusion::physical_expr::PhysicalExpr>);
    }
    if let Some(upper) = slice.upper {
        bounds.push(Arc::new(BinaryExpr::new(
            Arc::clone(&column),
            Operator::Lt,
            Arc::new(Literal::new(ScalarValue::TimestampNanosecond(
                Some(upper),
                tz,
            ))),
        ))
            as Arc<dyn datafusion::physical_expr::PhysicalExpr>);
    }
    let [first, rest @ ..] = bounds.as_slice() else {
        return Err(CompactionError::Input {
            reason: "slice has no bounds",
        });
    };
    let mut predicate = Arc::clone(first);
    for bound in rest {
        predicate = Arc::new(BinaryExpr::new(predicate, Operator::And, Arc::clone(bound)));
    }
    Ok(predicate)
}

/// Whether rows `left` and `right` of `batch` belong to one group: the same
/// `trace_id` for spans, the same `created_at` for scores.
fn same_group(
    table: FileTable,
    batch: &RecordBatch,
    left: usize,
    right: usize,
) -> Result<bool, CompactionError> {
    match table {
        FileTable::Spans => {
            let Some(ids) = batch
                .column_by_name(spans::TRACE_ID)
                .and_then(|column| column.as_fixed_size_binary_opt())
            else {
                return Err(CompactionError::Input {
                    reason: "missing group column",
                });
            };
            if ids.is_null(left) || ids.is_null(right) {
                return Err(CompactionError::Input {
                    reason: "missing group column",
                });
            }
            Ok(ids.value(left) == ids.value(right))
        }
        FileTable::Scores => {
            let Some(times) = batch
                .column_by_name(scores::CREATED_AT)
                .and_then(|column| column.as_primitive_opt::<TimestampNanosecondType>())
            else {
                return Err(CompactionError::Input {
                    reason: "missing group column",
                });
            };
            if times.is_null(left) || times.is_null(right) {
                return Err(CompactionError::Input {
                    reason: "missing group column",
                });
            }
            Ok(times.value(left) == times.value(right))
        }
        FileTable::Traces => Err(CompactionError::Input {
            reason: "traces are not merged",
        }),
    }
}

/// Deduplicates one chunk and writes it, computing the trace partials for
/// spans. The blocking write runs on `spawn_blocking`.
async fn emit_chunk<W: Write + Send + 'static>(
    table: FileTable,
    drop_deleted: bool,
    writer: FileWriter<W>,
    chunk: RecordBatch,
    mut partials: Vec<RecordBatch>,
) -> Result<(FileWriter<W>, Vec<RecordBatch>, u64), CompactionError> {
    match table {
        FileTable::Spans => {
            let rows = dedup::latest_spans(&chunk)?;
            let partial = partials_from_spans(&rows)?;
            let written = u64::try_from(rows.num_rows()).unwrap_or(u64::MAX);
            let writer = blocking_write(writer, rows).await?;
            partials.push(partial);
            Ok((writer, partials, written))
        }
        FileTable::Scores => {
            let rows = dedup::latest_scores(&chunk, drop_deleted)?;
            if rows.num_rows() == 0 {
                return Ok((writer, partials, 0));
            }
            let written = u64::try_from(rows.num_rows()).unwrap_or(u64::MAX);
            let writer = blocking_write(writer, rows).await?;
            Ok((writer, partials, written))
        }
        FileTable::Traces => Err(CompactionError::Input {
            reason: "traces are not merged",
        }),
    }
}

async fn blocking_write<W: Write + Send + 'static>(
    mut writer: FileWriter<W>,
    rows: RecordBatch,
) -> Result<FileWriter<W>, CompactionError> {
    tokio::task::spawn_blocking(move || writer.write(&rows).map(|()| writer))
        .await
        .map_err(join_error)?
        .map_err(CompactionError::Write)
}

fn join_error(_: tokio::task::JoinError) -> CompactionError {
    CompactionError::Task
}

fn read_error(source: DataFusionError) -> CompactionError {
    CompactionError::Read { source }
}

fn arrow_error(source: arrow::error::ArrowError) -> CompactionError {
    CompactionError::Read {
        source: DataFusionError::External(Box::new(source)),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Seek, SeekFrom};
    use std::sync::Arc;

    use arrow::array::{AsArray, RecordBatch};
    use arrow::compute::{
        SortColumn, SortOptions, concat_batches, lexsort_to_indices, take_record_batch,
    };
    use chrono::NaiveDate;
    use datafusion::execution::memory_pool::GreedyMemoryPool;
    use datafusion::execution::runtime_env::RuntimeEnvBuilder;
    use datafusion::physical_plan::displayable;
    use llmobs_core::batch::scores_to_record_batch;
    use llmobs_core::enums::ScoreSource;
    use llmobs_core::ids::ProjectId;
    use llmobs_core::schema::{scores, spans, traces};
    use llmobs_core::score::{Score, ScoreTarget, ScoreValue};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use uuid::Uuid;

    use super::{MergeContext, merge_plan, merge_slice};
    use crate::catalog::{FileMeta, FileTable};
    use crate::compaction::slice::{Slice, slices, time_values};
    use crate::config::{
        FlushConfig, MemtableConfig, ObjectStoreUrl, QueryConfig, S3Config, StorageConfig,
        WalConfig,
    };
    use crate::hooks::Hooks;
    use crate::objects::Objects;
    use crate::parquet::{FileWriter, write_file};
    use crate::partials::partials_from_spans;
    use crate::test_support::{span_row, span_rows};

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 11).unwrap()
    }

    async fn objects(dir: &tempfile::TempDir) -> Arc<Objects> {
        let config = StorageConfig {
            data_dir: dir.path().to_path_buf(),
            object_store: Some(ObjectStoreUrl::Local(dir.path().join("objects"))),
            s3: S3Config::default(),
            wal: WalConfig::default(),
            flush: FlushConfig::default(),
            memtable: MemtableConfig::default(),
            query: QueryConfig::default(),
            anchor: Default::default(),
            gc: Default::default(),
        };
        Arc::new(Objects::new(&config, Hooks::default()).await.unwrap())
    }

    /// The merge context of the plan: a 64 MiB greedy pool and the store.
    fn ctx(objects: &Arc<Objects>, batch_size: usize) -> MergeContext {
        let runtime = RuntimeEnvBuilder::new()
            .with_memory_pool(Arc::new(GreedyMemoryPool::new(64 << 20)))
            .build_arc()
            .unwrap();
        objects.register(&runtime);
        MergeContext {
            runtime,
            batch_size,
        }
    }

    async fn write_input(
        objects: &Arc<Objects>,
        table: FileTable,
        rows: RecordBatch,
    ) -> Arc<FileMeta> {
        let written = write_file(table, 0, &[Arc::new(rows)]).unwrap();
        let (path, id) = Objects::table_file_path(table, project(), day(), 0);
        objects
            .put_durable(&path, written.bytes.clone())
            .await
            .unwrap();
        Arc::new(FileMeta {
            id,
            table,
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
        })
    }

    /// One merge of one slice into a temp file, read back whole.
    async fn merge_read(
        ctx: &MergeContext,
        table: FileTable,
        inputs: &[Arc<FileMeta>],
        slice: Slice,
        drop_deleted: bool,
    ) -> (RecordBatch, Vec<RecordBatch>) {
        let sink = tempfile::tempfile().unwrap();
        let writer = FileWriter::new(table, 1, sink, 0).unwrap();
        let merged = merge_slice(ctx, table, inputs, slice, drop_deleted, writer)
            .await
            .unwrap();
        assert!(merged.rows > 0);
        let (file, summary) = merged.writer.finish().unwrap();
        assert_eq!(summary.rows, merged.rows);
        let mut file = file;
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer).unwrap();
        let builder = ParquetRecordBatchReaderBuilder::try_new(bytes::Bytes::from(buffer)).unwrap();
        let schema = Arc::clone(builder.schema());
        let batches: Vec<RecordBatch> = builder
            .build()
            .unwrap()
            .map(|batch| batch.unwrap())
            .collect();
        (concat_batches(&schema, &batches).unwrap(), merged.partials)
    }

    /// `(trace_id, span_id, start_time, ingest_time)` of every row.
    fn span_tuples(batch: &RecordBatch) -> Vec<(Vec<u8>, Vec<u8>, i64, i64)> {
        let ids = batch
            .column_by_name(spans::TRACE_ID)
            .unwrap()
            .as_fixed_size_binary();
        let span_ids = batch
            .column_by_name(spans::SPAN_ID)
            .unwrap()
            .as_fixed_size_binary();
        let starts = batch
            .column_by_name(spans::START_TIME)
            .unwrap()
            .as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        let ingests = batch
            .column_by_name(spans::INGEST_TIME)
            .unwrap()
            .as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        (0..batch.num_rows())
            .map(|row| {
                (
                    ids.value(row).to_vec(),
                    span_ids.value(row).to_vec(),
                    starts.value(row),
                    ingests.value(row),
                )
            })
            .collect()
    }

    /// Sorts `batch` by `trace_id` alone, the order partials are compared in.
    fn sorted_by_trace(batch: &RecordBatch) -> RecordBatch {
        let key = SortColumn {
            values: Arc::clone(batch.column_by_name(traces::TRACE_ID).unwrap()),
            options: Some(SortOptions {
                descending: false,
                nulls_first: false,
            }),
        };
        let indices = lexsort_to_indices(&[key], None).unwrap();
        take_record_batch(batch, &indices).unwrap()
    }

    fn assert_columns_equal(actual: &RecordBatch, expected: &RecordBatch) {
        assert_eq!(actual.num_rows(), expected.num_rows());
        for field in expected.schema().fields() {
            let name = field.name();
            assert_eq!(
                actual.column_by_name(name),
                expected.column_by_name(name),
                "{name}"
            );
        }
    }

    fn trace_bytes(trace: u16) -> Vec<u8> {
        let mut bytes = [0_u8; 16];
        let pair = (trace + 1).to_be_bytes();
        for chunk in bytes.as_chunks_mut::<2>().0 {
            chunk.copy_from_slice(&pair);
        }
        bytes.to_vec()
    }

    fn span_bytes(span: u8) -> Vec<u8> {
        vec![span + 1; 8]
    }

    /// Three inputs: traces `A` (0) and `B` (1) have spans in all three, and
    /// `A/1` appears in input 1 with `ingest_time` 10 and in input 3 with 20.
    async fn three_span_inputs(objects: &Arc<Objects>) -> Vec<Arc<FileMeta>> {
        let one = span_rows(&[
            span_row(0, 1, 100, 10),
            span_row(0, 2, 200, 11),
            span_row(1, 1, 150, 12),
        ]);
        let two = span_rows(&[span_row(0, 3, 300, 13), span_row(1, 2, 250, 14)]);
        let three = span_rows(&[span_row(0, 1, 100, 20), span_row(1, 3, 350, 15)]);
        vec![
            write_input(objects, FileTable::Spans, one).await,
            write_input(objects, FileTable::Spans, two).await,
            write_input(objects, FileTable::Spans, three).await,
        ]
    }

    fn open_slice() -> Slice {
        Slice {
            lower: None,
            upper: None,
        }
    }

    #[tokio::test]
    async fn spans_merge_sorted_deduped_with_partials() {
        let dir = tempfile::tempdir().unwrap();
        let objects = objects(&dir).await;
        let ctx = ctx(&objects, 8192);
        let inputs = three_span_inputs(&objects).await;

        let (output, partials) =
            merge_read(&ctx, FileTable::Spans, &inputs, open_slice(), true).await;

        let tuples = span_tuples(&output);
        assert_eq!(tuples.len(), 6, "distinct spans");
        assert!(
            tuples
                .windows(2)
                .all(|pair| { (pair[0].0.clone(), pair[0].2) <= (pair[1].0.clone(), pair[1].2) })
        );
        let a_one = tuples
            .iter()
            .find(|(trace, span, _, _)| *trace == trace_bytes(0) && *span == span_bytes(1))
            .unwrap();
        assert_eq!(a_one.3, 20, "the newer duplicate of A/1 wins");

        // The partials of all chunks equal the partials of the whole output.
        let combined = concat_batches(&partials[0].schema(), &partials).unwrap();
        let actual = sorted_by_trace(&combined);
        let expected = sorted_by_trace(&partials_from_spans(&output).unwrap());
        assert_columns_equal(&actual, &expected);
        assert_eq!(actual.num_rows(), 2, "one partial per trace");
    }

    #[tokio::test]
    async fn two_slices_partition_the_same_rows() {
        let dir = tempfile::tempdir().unwrap();
        let objects = objects(&dir).await;
        let ctx = ctx(&objects, 8192);
        let inputs = three_span_inputs(&objects).await;

        let times = time_values(objects.store(), &inputs, spans::START_TIME)
            .await
            .unwrap();
        let parts = slices(&times, 2);
        assert_eq!(parts.len(), 2, "{parts:?}");
        let (first, first_partials) =
            merge_read(&ctx, FileTable::Spans, &inputs, parts[0], true).await;
        let (second, second_partials) =
            merge_read(&ctx, FileTable::Spans, &inputs, parts[1], true).await;

        let boundary = parts[0].upper.unwrap();
        let first_starts: Vec<i64> = span_tuples(&first).iter().map(|tuple| tuple.2).collect();
        let second_starts: Vec<i64> = span_tuples(&second).iter().map(|tuple| tuple.2).collect();
        assert!(first_starts.iter().all(|start| *start < boundary));
        assert!(second_starts.iter().all(|start| *start >= boundary));

        // The two outputs together equal the one-slice output.
        let mut together = span_tuples(&first);
        together.extend(span_tuples(&second));
        together.sort();
        let (whole, _) = merge_read(&ctx, FileTable::Spans, &inputs, open_slice(), true).await;
        let mut expected = span_tuples(&whole);
        expected.sort();
        assert_eq!(together, expected);

        // Trace B has spans on both sides of the boundary, so each slice's
        // partials hold one row for it.
        for partials in [&first_partials, &second_partials] {
            let combined = concat_batches(&partials[0].schema(), partials).unwrap();
            let sorted = sorted_by_trace(&combined);
            let ids = sorted.column_by_name(traces::TRACE_ID).unwrap();
            let trace_b = ids.as_fixed_size_binary();
            assert_eq!(
                (0..sorted.num_rows())
                    .filter(|row| trace_b.value(*row) == trace_bytes(1).as_slice())
                    .count(),
                1
            );
        }
    }

    #[tokio::test]
    async fn a_trace_straddling_batches_keeps_one_partial() {
        let dir = tempfile::tempdir().unwrap();
        let objects = objects(&dir).await;
        let ctx = ctx(&objects, 3);
        let spans: Vec<_> = (0..7)
            .map(|index| {
                span_row(
                    0,
                    index,
                    1000 + i64::from(index) * 1000,
                    1200 + i64::from(index) * 1000,
                )
            })
            .collect();
        let one = span_rows(&spans);
        let two = span_rows(&[span_row(0, 3, 4000, 5000)]);
        let inputs = vec![
            write_input(&objects, FileTable::Spans, one).await,
            write_input(&objects, FileTable::Spans, two).await,
        ];

        let (output, partials) =
            merge_read(&ctx, FileTable::Spans, &inputs, open_slice(), true).await;

        let tuples = span_tuples(&output);
        assert_eq!(tuples.len(), 7);
        let duplicate = tuples
            .iter()
            .find(|(_, span, _, _)| *span == span_bytes(3))
            .unwrap();
        assert_eq!(duplicate.3, 5000, "the newer duplicate wins");

        assert_eq!(partials.len(), 1, "one partial batch for one group");
        let partial = &partials[0];
        assert_eq!(partial.num_rows(), 1);
        let counts = partial
            .column_by_name(traces::SPAN_COUNT)
            .unwrap()
            .as_primitive::<arrow::datatypes::Int64Type>();
        assert_eq!(counts.value(0), 7);
    }

    fn score_version(
        score_id: Uuid,
        created_at: i64,
        updated_at: i64,
        ingest: i64,
        deleted: bool,
        name: &str,
    ) -> Score {
        Score {
            project_id: project(),
            score_id,
            target: ScoreTarget::Trace {
                trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".parse().unwrap(),
            },
            session_id: None,
            user_id: None,
            name: name.to_owned(),
            value: ScoreValue::Numeric(0.5),
            reason: None,
            source: ScoreSource::Api,
            evaluator_id: None,
            rule_id: None,
            experiment_id: None,
            dataset_item_id: None,
            author_user_id: None,
            metadata: Default::default(),
            created_at,
            updated_at,
            deleted,
            ingest_time: ingest,
        }
    }

    /// Score `s` has versions in three inputs (`updated_at` 1, 3 and 2, the
    /// one at 3 deleted); score `t` has one live version.
    async fn three_score_inputs(objects: &Arc<Objects>) -> Vec<Arc<FileMeta>> {
        let s = Uuid::from_u128(0x11);
        let t = Uuid::from_u128(0x22);
        let one = scores_to_record_batch(&[
            score_version(s, 1000, 1, 100, false, "s-v1"),
            score_version(t, 2000, 5, 500, false, "t"),
        ])
        .unwrap();
        let two = scores_to_record_batch(&[score_version(s, 1000, 3, 300, true, "s-v3")]).unwrap();
        let three =
            scores_to_record_batch(&[score_version(s, 1000, 2, 200, false, "s-v2")]).unwrap();
        vec![
            write_input(objects, FileTable::Scores, one).await,
            write_input(objects, FileTable::Scores, two).await,
            write_input(objects, FileTable::Scores, three).await,
        ]
    }

    fn score_tuples(batch: &RecordBatch) -> Vec<(String, i64, bool, String)> {
        let ids = batch
            .column_by_name(scores::SCORE_ID)
            .unwrap()
            .as_string::<i32>();
        let updated = batch
            .column_by_name(scores::UPDATED_AT)
            .unwrap()
            .as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        let deleted = batch.column_by_name(scores::DELETED).unwrap().as_boolean();
        let names = batch
            .column_by_name(scores::NAME)
            .unwrap()
            .as_string::<i32>();
        (0..batch.num_rows())
            .map(|row| {
                (
                    ids.value(row).to_owned(),
                    updated.value(row),
                    deleted.value(row),
                    names.value(row).to_owned(),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn scores_merge_last_writer_wins() {
        let dir = tempfile::tempdir().unwrap();
        let objects = objects(&dir).await;
        let ctx = ctx(&objects, 8192);
        let inputs = three_score_inputs(&objects).await;
        let s = Uuid::from_u128(0x11).to_string();
        let t = Uuid::from_u128(0x22).to_string();

        let (kept, _) = merge_read(&ctx, FileTable::Scores, &inputs, open_slice(), false).await;
        let kept = score_tuples(&kept);
        assert_eq!(kept.len(), 2, "{kept:?}");
        let s_row = kept.iter().find(|(id, _, _, _)| *id == s).unwrap();
        assert_eq!((s_row.1, s_row.2, s_row.3.as_str()), (3, true, "s-v3"));

        let (dropped, _) = merge_read(&ctx, FileTable::Scores, &inputs, open_slice(), true).await;
        let dropped = score_tuples(&dropped);
        assert_eq!(dropped.len(), 1, "{dropped:?}");
        assert_eq!(dropped[0].0, t);
    }

    #[tokio::test]
    async fn the_plan_merges_without_sorting() {
        let dir = tempfile::tempdir().unwrap();
        let objects = objects(&dir).await;
        let _ctx = ctx(&objects, 8192);
        let inputs = three_span_inputs(&objects).await;
        let ordered: Vec<&Arc<FileMeta>> = inputs.iter().collect();
        let plan = merge_plan(FileTable::Spans, &ordered, open_slice()).unwrap();
        let text = displayable(plan.as_ref()).indent(true).to_string();
        assert!(text.contains("SortPreservingMergeExec"), "{text}");
        assert!(!text.contains("SortExec"), "{text}");
    }
}
