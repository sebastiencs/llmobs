//! The model test: append, restart, compaction, GC, deletion and retention
//! against a simple model of the rows. "Rows in = rows out", the M4 exit
//! criterion, plus "covered rows go and the rest stay" for M5.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use arrow::array::AsArray;
use bytesize::ByteSize;
use chrono::{Datelike, Days, NaiveDate};
use datafusion::execution::context::{SessionConfig, SessionContext};
use llmobs_core::ids::TraceId;
use proptest::prelude::*;

use crate::Storage;
use crate::config::{
    CompactionConfig, FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig, WalConfig,
};
use crate::deletion::{Deletion, DeletionScope, DeletionStatus};
use crate::test_support::{span_row, span_rows};

const DAY_NS: i64 = 86_400_000_000_000;

#[derive(Debug, Clone)]
enum Op {
    /// One span. `trace` ∈ 0..6, `span` ∈ 0..12. Its day is `trace % 3`.
    Append { trace: u8, span: u8 },
    /// shutdown + open: flushes everything.
    Restart,
    /// One pass at the model clock, after advancing it by `hours`.
    Compact { hours: u8 },
    /// collect_obsolete(Duration::ZERO).
    Gc,
    /// `register_deletion` of the trace, cut off at the op's own ingest value.
    Delete { trace: u8 },
    /// One deletion pass at the model clock.
    ApplyDeletions,
    /// `apply_retention(project, D + days)`.
    Retention { days: u8 },
}

/// The weights give a 40-op case about 3 deletions and 1 retention.
fn model_ops() -> impl Strategy<Value = Vec<Op>> {
    let op = prop_oneof![
        30 => (0u8..6, 0u8..12).prop_map(|(trace, span)| Op::Append { trace, span }),
        2 => Just(Op::Restart),
        2 => (0u8..72).prop_map(|hours| Op::Compact { hours }),
        1 => Just(Op::Gc),
        3 => (0u8..6).prop_map(|trace| Op::Delete { trace }),
        1 => Just(Op::ApplyDeletions),
        1 => (0u8..4).prop_map(|days| Op::Retention { days }),
    ];
    proptest::collection::vec(op, 1..=40)
}

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 3, 5).unwrap()
}

fn midnight(date: NaiveDate) -> i64 {
    (i64::from(date.num_days_from_ce()) - 719_163) * DAY_NS
}

/// The trace's day: `D + trace % 3`.
fn trace_day(trace: u8) -> NaiveDate {
    day()
        .checked_add_days(Days::new(u64::from(trace % 3)))
        .unwrap_or(day())
}

fn project() -> llmobs_core::ids::ProjectId {
    "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
}

fn config_in(dir: &std::path::Path) -> StorageConfig {
    StorageConfig {
        data_dir: dir.to_path_buf(),
        min_free_bytes: ByteSize::b(0),
        object_store: None,
        s3: S3Config::default(),
        wal: WalConfig::default(),
        flush: FlushConfig::default(),
        memtable: MemtableConfig::default(),
        query: QueryConfig::default(),
        anchor: Default::default(),
        gc: Default::default(),
        compaction: CompactionConfig {
            interval: Duration::from_secs(3_600),
            l0_files: 2,
            ..CompactionConfig::default()
        },
        deletion: Default::default(),
    }
}

/// `(trace_id, span_id, ingest_time)` of every stored spans row.
async fn stored_spans(storage: &Storage) -> Vec<(Vec<u8>, Vec<u8>, i64)> {
    let query = storage.query(project(), Default::default()).await.unwrap();
    let ctx = SessionContext::new_with_config_rt(SessionConfig::new(), Arc::clone(&query.runtime));
    ctx.register_table("spans_t", Arc::clone(&query.spans))
        .unwrap();
    let batches = ctx
        .sql("SELECT trace_id, span_id, ingest_time FROM spans_t")
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();
    let mut rows = Vec::new();
    for batch in batches {
        let ids = batch.column(0).as_fixed_size_binary();
        let spans = batch.column(1).as_fixed_size_binary();
        let ingests = batch
            .column(2)
            .as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        for row in 0..batch.num_rows() {
            rows.push((
                ids.value(row).to_vec(),
                spans.value(row).to_vec(),
                ingests.value(row),
            ));
        }
    }
    rows
}

/// `(trace_id, span_count, partial_count)` of every stored traces partial.
async fn stored_partials(storage: &Storage) -> Vec<(Vec<u8>, i64, i32)> {
    let query = storage.query(project(), Default::default()).await.unwrap();
    let ctx = SessionContext::new_with_config_rt(SessionConfig::new(), Arc::clone(&query.runtime));
    ctx.register_table("traces_t", Arc::clone(&query.traces))
        .unwrap();
    let batches = ctx
        .sql("SELECT trace_id, span_count, partial_count FROM traces_t")
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();
    let mut rows = Vec::new();
    for batch in batches {
        let ids = batch.column(0).as_fixed_size_binary();
        let spans = batch
            .column(1)
            .as_primitive::<arrow::datatypes::Int64Type>();
        let partials = batch
            .column(2)
            .as_primitive::<arrow::datatypes::Int32Type>();
        for row in 0..batch.num_rows() {
            rows.push((
                ids.value(row).to_vec(),
                spans.value(row),
                partials.value(row),
            ));
        }
    }
    rows
}

/// The id bytes `span_row` builds for `(trace, span)`.
fn key_bytes(trace: u8, span: u8) -> (Vec<u8>, Vec<u8>) {
    let mut trace_bytes = [0_u8; 16];
    let pair = (u16::from(trace) + 1).to_be_bytes();
    for chunk in trace_bytes.as_chunks_mut::<2>().0 {
        chunk.copy_from_slice(&pair);
    }
    (trace_bytes.to_vec(), vec![span + 1; 8])
}

/// The trace id `span_row` builds for `trace`.
fn trace_id(trace: u8) -> TraceId {
    let (bytes, _) = key_bytes(trace, 0);
    TraceId::from_bytes(bytes.as_slice().try_into().unwrap()).unwrap()
}

/// Distinct keys and their greatest `ingest_time`, against the model.
fn check_spans(rows: &[(Vec<u8>, Vec<u8>, i64)], model: &BTreeMap<(u8, u8), i64>) {
    let mut maxima: BTreeMap<(Vec<u8>, Vec<u8>), i64> = BTreeMap::new();
    for (trace, span, ingest) in rows {
        let entry = maxima
            .entry((trace.clone(), span.clone()))
            .or_insert(*ingest);
        if *ingest > *entry {
            *entry = *ingest;
        }
    }
    let expected: BTreeMap<_, _> = model
        .iter()
        .map(|((trace, span), ingest)| (key_bytes(*trace, *span), *ingest))
        .collect();
    assert_eq!(maxima, expected);
}

async fn run_case(ops: Vec<Op>) {
    let dir = tempfile::tempdir().unwrap();
    let config = config_in(dir.path());
    let mut storage = Storage::open(config.clone()).await.unwrap();
    // The clock starts at D 01:00.
    let mut clock = midnight(day()) + 3_600 * 1_000_000_000;
    let mut model: BTreeMap<(u8, u8), i64> = BTreeMap::new();
    // The retention watermark, and every registered deletion
    // `(trace, cutoff, id)`.
    let mut keep_from: Option<NaiveDate> = None;
    let mut deletions: Vec<(u8, i64, uuid::Uuid)> = Vec::new();

    for (index, op) in ops.iter().enumerate() {
        match op.clone() {
            Op::Append { trace, span } => {
                // The trace's own day, 1 h after midnight, plus `span` seconds.
                let start = midnight(day())
                    + DAY_NS * i64::from(trace % 3)
                    + 3_600 * 1_000_000_000
                    + i64::from(span) * 1_000_000_000;
                // The op index in ns keeps later appends strictly newer.
                let ingest = clock + index as i64;
                storage
                    .append_spans(
                        project(),
                        span_rows(&[span_row(u16::from(trace), span, start, ingest)]),
                    )
                    .await
                    .unwrap();
                // Storage hides a row of a day the watermark has dropped and
                // never flushes it (spec 03 §8), so the model never has it.
                if keep_from.is_none_or(|keep| trace_day(trace) >= keep) {
                    model.insert((trace, span), ingest);
                }
            }
            Op::Restart => {
                storage.shutdown().await.unwrap();
                storage = Storage::open(config.clone()).await.unwrap();
            }
            Op::Compact { hours } => {
                clock += i64::from(hours) * 3_600 * 1_000_000_000;
                storage.compact(clock).await.unwrap();
            }
            Op::Gc => {
                storage.collect_obsolete(Duration::ZERO).await.unwrap();
            }
            Op::Delete { trace } => {
                // Every row of the trace so far is covered: each was appended
                // at a lower index, and the clock only rises.
                let cutoff = clock + index as i64;
                let id = uuid::Uuid::now_v7();
                storage
                    .register_deletion(Deletion {
                        id,
                        project: project(),
                        cutoff,
                        scope: DeletionScope::Traces {
                            trace_ids: vec![trace_id(trace)],
                        },
                    })
                    .await
                    .unwrap();
                deletions.push((trace, cutoff, id));
                model.retain(|(key_trace, _), _| *key_trace != trace);
            }
            Op::ApplyDeletions => {
                let report = storage.apply_deletions(clock).await.unwrap();
                assert_eq!(report.failed, 0, "a deletion job failed");
            }
            Op::Retention { days } => {
                let keep = day()
                    .checked_add_days(Days::new(u64::from(days)))
                    .unwrap_or(day());
                storage.apply_retention(project(), keep).await.unwrap();
                keep_from = Some(keep_from.map_or(keep, |current| current.max(keep)));
                model.retain(|(trace, _), _| trace_day(*trace) >= keep);
            }
        }
        check_spans(&stored_spans(&storage).await, &model);
    }

    // Quiesce, in this order:
    // 1. everything in files, so no memtable batch blocks "applied";
    storage.shutdown().await.unwrap();
    storage = Storage::open(config.clone()).await.unwrap();
    // 2. the clock must be past the newest ingest by `close_delay` too, or a
    //    late L0 file is legitimately not closed yet (G13) and `compact` at a
    //    fixed clock can never make progress.
    let sixth = day().checked_add_days(Days::new(6)).unwrap_or(day());
    let close_delay = i64::try_from(config.compaction.close_delay.as_nanos()).unwrap_or(i64::MAX);
    clock = clock
        .max(midnight(sixth))
        .saturating_add(close_delay)
        .saturating_add(1_000_000);
    // 3. every deletion applied: while one is pending, the `traces` view
    //    hides a covered trace's partials whole and the checks below would
    //    not match.
    for _ in 0..10 {
        let report = storage.apply_deletions(clock).await.unwrap();
        assert_eq!(report.failed, 0, "a deletion job failed");
        if deletions_status(&storage, &deletions).await {
            break;
        }
    }
    assert!(
        deletions_status(&storage, &deletions).await,
        "a deletion is still pending after 10 passes"
    );
    // 4. every partition one file;
    for _ in 0..10 {
        let report = storage.compact(clock).await.unwrap();
        assert_eq!(report.failed, 0, "a compaction job failed");
        if report.jobs == 0 {
            break;
        }
    }
    // 5. nothing obsolete.
    storage.collect_obsolete(Duration::ZERO).await.unwrap();

    // 6. One row per key, at the model's version.
    let rows = stored_spans(&storage).await;
    check_spans(&rows, &model);
    let mut by_key: BTreeMap<(Vec<u8>, Vec<u8>), i64> = BTreeMap::new();
    for (trace, span, ingest) in &rows {
        let key = (trace.clone(), span.clone());
        assert!(
            by_key.insert(key, *ingest).is_none(),
            "a span appears twice after compaction"
        );
    }
    let expected: BTreeMap<_, _> = model
        .iter()
        .map(|((trace, span), ingest)| (key_bytes(*trace, *span), *ingest))
        .collect();
    assert_eq!(by_key, expected);

    // One partial per trace, counting its model keys.
    let mut per_trace: BTreeMap<u8, usize> = BTreeMap::new();
    for (trace, _) in model.keys() {
        *per_trace.entry(*trace).or_default() += 1;
    }
    let partials = stored_partials(&storage).await;
    let mut merged: BTreeMap<Vec<u8>, i64> = BTreeMap::new();
    for (trace, span_count, partial_count) in &partials {
        assert_eq!(*partial_count, 1, "a trace was not fully merged");
        *merged.entry(trace.clone()).or_default() += span_count;
    }
    let mut expected: BTreeMap<Vec<u8>, i64> = BTreeMap::new();
    for (trace, count) in per_trace {
        expected.insert(key_bytes(trace, 0).0, count as i64);
    }
    assert_eq!(merged, expected, "{partials:?}");

    // 7. No row a registered deletion covers remains in any active file.
    for file in storage.catalog_files(crate::wal::Table::Spans) {
        let rows = crate::test_support::object_rows(dir.path(), &file);
        let traces = crate::test_support::trace_ids_of(&rows);
        let ingests = rows
            .column_by_name(llmobs_core::schema::spans::INGEST_TIME)
            .unwrap()
            .as_primitive::<arrow::datatypes::TimestampNanosecondType>();
        for (row, trace) in traces.iter().enumerate() {
            let covered = deletions.iter().any(|(deleted, cutoff, _)| {
                *trace == *trace_id(*deleted).as_bytes() && ingests.value(row) <= *cutoff
            });
            assert!(!covered, "a covered row remains in {}", file.path);
        }
    }

    storage.shutdown().await.unwrap();
}

/// Whether every registered deletion is `Applied`.
async fn deletions_status(storage: &Storage, deletions: &[(u8, i64, uuid::Uuid)]) -> bool {
    let mut all = true;
    for (_, _, id) in deletions {
        match storage.deletion_status(*id).await.unwrap() {
            DeletionStatus::Applied { .. } => {}
            DeletionStatus::Pending => all = false,
        }
    }
    all
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16))]

    /// Append, restart, compaction, GC, deletion and retention keep the rows
    /// the model expects.
    #[test]
    fn compaction_matches_the_model(ops in model_ops()) {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(run_case(ops));
    }
}
