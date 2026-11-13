//! Trace anchor: keep the spans and partials of a trace that crosses midnight
//! in one partition day (D86).
//!
//! All anchor logic lives here. The flush asks for one partition day per trace
//! and applies the returned plan only after its catalog commit succeeds. At
//! open, the state is rebuilt from recent `traces` files.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;

use arrow::array::{Array, AsArray, RecordBatch, UInt32Array};
use arrow::compute::{concat_batches, take_record_batch};
use arrow::datatypes::TimestampNanosecondType;
use chrono::{Datelike, NaiveDate};
use futures::StreamExt;
use llmobs_core::ids::{ProjectId, TraceId};
use llmobs_core::schema::{spans, spans_schema, traces};
use object_store::path::Path as ObjectPath;
use parquet::arrow::ProjectionMask;
#[allow(deprecated)]
use parquet::arrow::async_reader::ParquetObjectReader;
use parquet::arrow::async_reader::ParquetRecordBatchStreamBuilder;
use parquet::file::metadata::ParquetMetaData;
use parquet::file::statistics::Statistics;

use crate::catalog::{Catalog, FileMeta, FileTable, now_ns};
use crate::error::AnchorError;
use crate::memtable::{FrozenView, PartitionKey};
use crate::objects::Objects;
use crate::verify::{VerifyQueue, is_store_error};

/// Nanoseconds in a UTC day.
pub(crate) const DAY_NS: i64 = 86_400_000_000_000;

/// `NaiveDate::num_days_from_ce` of 1970-01-01.
const UNIX_EPOCH_DAYS_FROM_CE: i64 = 719_163;

/// One remembered anchor. `expires` is an ingest-time deadline, not an event age.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    day: NaiveDate,
    expires: i64,
}

/// Entry changes planned by one flush, applied only after its commit succeeds.
pub(crate) struct AnchorPlan {
    changes: Vec<(ProjectId, TraceId, Entry)>,
    max_ingest: i64,
}

/// One output partition and the exact rows assigned to it.
pub(crate) struct AnchorPartition {
    pub(crate) key: PartitionKey,
    pub(crate) batch: RecordBatch,
}

/// The anchor state of the spans flush task.
#[derive(Debug)]
pub(crate) struct Anchors {
    /// Midnight window, nanoseconds.
    window: i64,
    /// Largest `ingest_time` committed by any flush, or `i64::MIN`.
    clock: i64,
    entries: HashMap<(ProjectId, TraceId), Entry>,
}

impl Anchors {
    pub(crate) fn new(window: i64) -> Self {
        Self {
            window,
            clock: i64::MIN,
            entries: HashMap::new(),
        }
    }

    /// Regroups a frozen spans view by anchor day and plans the entry changes.
    ///
    /// The state is only read here; [`Self::commit`] applies the plan after the
    /// catalog commit. A failed attempt therefore recomputes the same days.
    pub(crate) fn partition(
        &self,
        view: &FrozenView,
    ) -> Result<(Vec<AnchorPartition>, AnchorPlan), AnchorError> {
        let mut by_project: BTreeMap<ProjectId, ProjectBatches> = BTreeMap::new();
        for (key, batches) in view.partitions.iter() {
            by_project.entry(key.project).or_default().push((
                key.day,
                batches
                    .iter()
                    .map(|batch| Arc::clone(&batch.batch))
                    .collect(),
            ));
        }

        let mut max_ingest = i64::MIN;
        for groups in by_project.values() {
            for (_, batches) in groups {
                for batch in batches {
                    let ingest = ingest_column(batch)?;
                    for row in 0..batch.num_rows() {
                        if !ingest.is_null(row) {
                            max_ingest = max_ingest.max(ingest.value(row));
                        }
                    }
                }
            }
        }
        let expires = max_ingest.saturating_add(self.window.saturating_mul(2));

        let schema = spans_schema();
        let mut partitions = Vec::new();
        let mut changes = Vec::new();
        for (project, groups) in &by_project {
            let refs: Vec<&RecordBatch> = groups
                .iter()
                .flat_map(|(_, batches)| batches.iter().map(AsRef::as_ref))
                .collect();
            if refs.is_empty() {
                continue;
            }
            let combined = concat_batches(&schema, refs).map_err(AnchorError::Arrow)?;
            let trace = combined
                .column_by_name(spans::TRACE_ID)
                .and_then(|column| column.as_fixed_size_binary_opt())
                .ok_or_else(|| AnchorError::column(spans::TRACE_ID))?;
            let start = combined
                .column_by_name(spans::START_TIME)
                .and_then(|column| column.as_primitive_opt::<TimestampNanosecondType>())
                .ok_or_else(|| AnchorError::column(spans::START_TIME))?;

            let mut by_trace: BTreeMap<Vec<u8>, Vec<u32>> = BTreeMap::new();
            for row in 0..combined.num_rows() {
                if trace.is_null(row) || start.is_null(row) {
                    return Err(AnchorError::column(spans::TRACE_ID));
                }
                let index = u32::try_from(row).map_err(|_| {
                    AnchorError::Arrow(arrow::error::ArrowError::ComputeError(
                        "anchor: too many rows in one flush".to_owned(),
                    ))
                })?;
                by_trace
                    .entry(trace.value(row).to_vec())
                    .or_default()
                    .push(index);
            }

            let mut assigned: BTreeMap<NaiveDate, Vec<u32>> = BTreeMap::new();
            for (trace_bytes, rows) in by_trace {
                let trace_id = TraceId::from_slice(&trace_bytes)
                    .map_err(|_| AnchorError::column(spans::TRACE_ID))?;
                let min_start = rows
                    .iter()
                    .map(|row| start.value(usize::try_from(*row).unwrap_or(0)))
                    .min()
                    .unwrap_or(0);
                let existing = self.entries.get(&(*project, trace_id)).map(|e| e.day);
                let anchor = existing.unwrap_or_else(|| day_of_ns(min_start));
                let anchor_number = day_number(anchor);
                let mut near = false;
                for row in &rows {
                    let time = start.value(usize::try_from(*row).unwrap_or(0));
                    near |= near_midnight(time, self.window);
                    let own = day_of_ns(time);
                    let day = if (day_number(own) - anchor_number).abs() <= 1 {
                        anchor
                    } else {
                        own
                    };
                    assigned.entry(day).or_default().push(*row);
                }
                // Spec 03 § Trace anchor: a near-midnight row makes an entry,
                // and an existing entry is refreshed by the same deadline even
                // when none of this flush's rows is near midnight.
                if near || existing.is_some() {
                    changes.push((
                        *project,
                        trace_id,
                        Entry {
                            day: anchor,
                            expires,
                        },
                    ));
                }
            }

            for (day, mut indices) in assigned {
                indices.sort_unstable();
                let batch = take_record_batch(&combined, &UInt32Array::from(indices))
                    .map_err(AnchorError::Arrow)?;
                partitions.push(AnchorPartition {
                    key: PartitionKey {
                        project: *project,
                        day,
                    },
                    batch,
                });
            }
        }
        partitions.sort_by_key(|partition| partition.key);
        Ok((
            partitions,
            AnchorPlan {
                changes,
                max_ingest,
            },
        ))
    }

    /// Applies a plan after a successful commit: insert or refresh entries,
    /// advance the clock, then drop entries that expired before it.
    pub(crate) fn commit(&mut self, plan: AnchorPlan) {
        for (project, trace, entry) in plan.changes {
            self.entries.insert((project, trace), entry);
        }
        self.clock = self.clock.max(plan.max_ingest);
        self.entries.retain(|_, entry| entry.expires >= self.clock);
    }

    /// Rebuilds the state from recent `traces` files, before the flush task starts.
    ///
    /// A traces file that cannot be read is skipped and becomes a suspect (spec
    /// 03 §10). A store failure still fails the open, as before.
    pub(crate) async fn rebuild(
        &mut self,
        catalog: &Catalog,
        objects: &Objects,
        verify: &VerifyQueue,
    ) -> Result<(), AnchorError> {
        let started = Instant::now();
        self.clock = catalog
            .files_all(FileTable::Spans)
            .map(|file| file.max_ingest_time)
            .max()
            .unwrap_or(i64::MIN);
        let since = self.clock.saturating_sub(self.window.saturating_mul(2));
        let files: Vec<&Arc<FileMeta>> = catalog
            .files_all(FileTable::Traces)
            .filter(|file| file.max_ingest_time >= since)
            .collect();
        let mut rows_read = 0_usize;
        for file in &files {
            match self.rebuild_file(file, objects, since).await {
                Ok(rows) => rows_read = rows_read.saturating_add(rows),
                Err(AnchorError::Parquet(error)) if is_store_error(&error) => {
                    return Err(AnchorError::Parquet(error));
                }
                Err(_) => {
                    // The anchors collected from this file stay: they are hints.
                    tracing::warn!(
                        file_id = %file.id,
                        "skipping an unreadable traces file in the anchor rebuild"
                    );
                    verify.suspect([file.id], now_ns());
                }
            }
        }
        tracing::info!(
            files = files.len(),
            rows = rows_read,
            entries = self.entries.len(),
            elapsed_ms = started.elapsed().as_millis(),
            "rebuilt trace anchors"
        );
        Ok(())
    }
}

impl Anchors {
    /// Reads the recent row groups of one `traces` file; returns the rows read.
    async fn rebuild_file(
        &mut self,
        file: &FileMeta,
        objects: &Objects,
        since: i64,
    ) -> Result<usize, AnchorError> {
        #[allow(deprecated)]
        let reader =
            ParquetObjectReader::new(objects.store(), ObjectPath::from(file.path.as_str()))
                .with_file_size(file.size);
        let builder = ParquetRecordBatchStreamBuilder::new(reader)
            .await
            .map_err(AnchorError::parquet)?;
        let groups = row_groups_to_read(builder.metadata(), since);
        let mask = ProjectionMask::columns(
            builder.parquet_schema(),
            [
                traces::TRACE_ID,
                traces::START_TIME,
                traces::END_TIME,
                traces::LAST_INGEST_TIME,
            ],
        );
        let mut stream = builder
            .with_row_groups(groups)
            .with_projection(mask)
            .build()
            .map_err(AnchorError::parquet)?;
        let mut rows = 0_usize;
        while let Some(batch) = stream.next().await {
            let batch = batch.map_err(AnchorError::parquet)?;
            rows = rows.saturating_add(batch.num_rows());
            collect_rebuild_rows(self, file, &batch, since)?;
        }
        Ok(rows)
    }
}

fn collect_rebuild_rows(
    anchors: &mut Anchors,
    file: &FileMeta,
    batch: &RecordBatch,
    since: i64,
) -> Result<(), AnchorError> {
    let trace = batch
        .column_by_name(traces::TRACE_ID)
        .and_then(|column| column.as_fixed_size_binary_opt())
        .ok_or_else(|| AnchorError::column(traces::TRACE_ID))?;
    let start = batch
        .column_by_name(traces::START_TIME)
        .and_then(|column| column.as_primitive_opt::<TimestampNanosecondType>())
        .ok_or_else(|| AnchorError::column(traces::START_TIME))?;
    let end = batch
        .column_by_name(traces::END_TIME)
        .and_then(|column| column.as_primitive_opt::<TimestampNanosecondType>())
        .ok_or_else(|| AnchorError::column(traces::END_TIME))?;
    let ingest = batch
        .column_by_name(traces::LAST_INGEST_TIME)
        .and_then(|column| column.as_primitive_opt::<TimestampNanosecondType>())
        .ok_or_else(|| AnchorError::column(traces::LAST_INGEST_TIME))?;
    for row in 0..batch.num_rows() {
        if trace.is_null(row) || start.is_null(row) || ingest.is_null(row) {
            continue;
        }
        let last_ingest = ingest.value(row);
        // A partial's start_time is the trace's earliest start; a child ending
        // near midnight is only visible through end_time. Without it a trace
        // rooted well before midnight loses its anchor on restart.
        let near = near_midnight(start.value(row), anchors.window)
            || (!end.is_null(row) && near_midnight(end.value(row), anchors.window));
        if last_ingest < since || !near {
            continue;
        }
        let Ok(trace_id) = TraceId::from_slice(trace.value(row)) else {
            continue;
        };
        let expires = last_ingest.saturating_add(anchors.window.saturating_mul(2));
        match anchors.entries.get_mut(&(file.project, trace_id)) {
            Some(entry) => {
                if file.day < entry.day {
                    entry.day = file.day;
                }
                entry.expires = entry.expires.max(expires);
            }
            None => {
                anchors.entries.insert(
                    (file.project, trace_id),
                    Entry {
                        day: file.day,
                        expires,
                    },
                );
            }
        }
    }
    Ok(())
}

fn ingest_column(
    batch: &RecordBatch,
) -> Result<&arrow::array::PrimitiveArray<TimestampNanosecondType>, AnchorError> {
    batch
        .column_by_name(spans::INGEST_TIME)
        .and_then(|column| column.as_primitive_opt::<TimestampNanosecondType>())
        .ok_or_else(|| AnchorError::column(spans::INGEST_TIME))
}

/// Batches of one project, grouped by their own UTC day.
type ProjectBatches = Vec<(NaiveDate, Vec<Arc<RecordBatch>>)>;

/// The midnight closest to `t`; exactly noon rounds up.
fn nearest_midnight(t: i64) -> i64 {
    let days = t.div_euclid(DAY_NS);
    let rest = t.rem_euclid(DAY_NS);
    let days = if rest >= DAY_NS / 2 {
        days.saturating_add(1)
    } else {
        days
    };
    days.saturating_mul(DAY_NS)
}

/// Strictly inside `window` of the nearest midnight.
fn near_midnight(t: i64, window: i64) -> bool {
    t.abs_diff(nearest_midnight(t)) < window as u64
}

fn day_number(day: NaiveDate) -> i64 {
    i64::from(day.num_days_from_ce()) - UNIX_EPOCH_DAYS_FROM_CE
}

fn day_of_ns(ns: i64) -> NaiveDate {
    let days = ns.div_euclid(DAY_NS);
    let ce = days.saturating_add(UNIX_EPOCH_DAYS_FROM_CE);
    i32::try_from(ce)
        .ok()
        .and_then(NaiveDate::from_num_days_from_ce_opt)
        .unwrap_or(NaiveDate::MAX)
}

/// Row groups whose `last_ingest_time` statistics have a max `>= since`.
/// A group without usable statistics is read, so the rebuild is conservative.
fn row_groups_to_read(metadata: &ParquetMetaData, since: i64) -> Vec<usize> {
    metadata
        .row_groups()
        .iter()
        .enumerate()
        .filter(|(_, group)| {
            let chunk = group.columns().iter().find(|column| {
                column
                    .column_descr()
                    .path()
                    .parts()
                    .first()
                    .map(String::as_str)
                    == Some(traces::LAST_INGEST_TIME)
            });
            match chunk.and_then(|column| column.statistics()) {
                Some(Statistics::Int64(stats)) => stats.max_opt().is_none_or(|max| *max >= since),
                _ => true,
            }
        })
        .map(|(index, _)| index)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use arrow::array::{AsArray, FixedSizeBinaryBuilder, RecordBatch, UInt32Array};
    use arrow::compute::take_record_batch;
    use arrow::datatypes::{DataType, Field, Schema, TimeUnit, TimestampNanosecondType};
    use chrono::{Duration as ChronoDuration, NaiveDate};
    use llmobs_core::batch::spans_to_record_batch;
    use llmobs_core::enums::{SpanKind, SpanStatus};
    use llmobs_core::ids::{ProjectId, SpanId, TraceId};
    use llmobs_core::span::Span;
    use parquet::arrow::ArrowWriter;
    use parquet::file::properties::{EnabledStatistics, WriterProperties};

    use super::*;
    use crate::memtable::MemBatch;

    const NS_PER_SEC: i64 = 1_000_000_000;
    const HOUR_NS: i64 = 3_600 * NS_PER_SEC;

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 3, 5).unwrap()
    }

    fn at(day: NaiveDate, hour: i64, minute: i64, second: i64) -> i64 {
        day_number(day) * DAY_NS + (hour * 3_600 + minute * 60 + second) * NS_PER_SEC
    }

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

    fn span(trace_last: u8, span_last: u8, start: i64, ingest: i64) -> Span {
        Span {
            project_id: project(),
            trace_id: trace(trace_last),
            span_id: span_id(span_last),
            parent_span_id: None,
            name: format!("s{span_last}"),
            span_kind: SpanKind::Span,
            start_time: start,
            end_time: start + 100,
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

    /// Builds a frozen view partitioned by each row's own UTC day.
    fn view(spans: &[Span]) -> FrozenView {
        let batch = spans_to_record_batch(spans).unwrap();
        let starts = batch
            .column_by_name(spans::START_TIME)
            .unwrap()
            .as_primitive::<TimestampNanosecondType>();
        let mut by_day: BTreeMap<NaiveDate, Vec<u32>> = BTreeMap::new();
        for row in 0..batch.num_rows() {
            by_day
                .entry(day_of_ns(starts.value(row)))
                .or_default()
                .push(u32::try_from(row).unwrap());
        }
        let mut partitions = BTreeMap::new();
        for (day, indices) in by_day {
            let sliced = take_record_batch(&batch, &UInt32Array::from(indices)).unwrap();
            let stored = MemBatch::from_batch(crate::wal::Table::Spans, sliced).unwrap();
            partitions.insert(
                PartitionKey {
                    project: project(),
                    day,
                },
                vec![stored],
            );
        }
        FrozenView {
            partitions: Arc::new(partitions),
            max_seq: 1,
        }
    }

    fn anchors() -> Anchors {
        Anchors::new(HOUR_NS)
    }

    /// One successful flush: partition, then apply the plan.
    fn flush(anchors: &mut Anchors, spans: &[Span]) -> Vec<(NaiveDate, usize)> {
        let view = view(spans);
        let (partitions, plan) = anchors.partition(&view).unwrap();
        let result = partitions
            .iter()
            .map(|partition| (partition.key.day, partition.batch.num_rows()))
            .collect();
        anchors.commit(plan);
        result
    }

    fn days(partitions: &[AnchorPartition]) -> Vec<NaiveDate> {
        partitions
            .iter()
            .map(|partition| partition.key.day)
            .collect()
    }

    #[test]
    fn nearest_midnight_rounds_exactly_noon_up() {
        let d = day();
        let noon = at(d, 12, 0, 0);
        assert_eq!(nearest_midnight(noon), at(d.succ_opt().unwrap(), 0, 0, 0));
        assert_eq!(nearest_midnight(noon - 1), day_number(d) * DAY_NS);
        assert_eq!(nearest_midnight(at(d, 0, 0, 0)), day_number(d) * DAY_NS);
    }

    #[test]
    fn near_midnight_is_strict_inside_the_window() {
        let midnight = day_number(day()) * DAY_NS;
        assert!(near_midnight(midnight - 1, HOUR_NS));
        assert!(near_midnight(midnight + 1, HOUR_NS));
        assert!(!near_midnight(midnight + HOUR_NS, HOUR_NS));
        assert!(!near_midnight(midnight - HOUR_NS, HOUR_NS));
        assert!(!near_midnight(at(day(), 12, 0, 0), HOUR_NS));
    }

    #[test]
    fn crossing_midnight_keeps_both_flushes_in_day_d() {
        let mut a = anchors();
        let early = at(day(), 23, 59, 50);
        let late = at(day().succ_opt().unwrap(), 0, 0, 10);
        assert_eq!(
            flush(&mut a, &[span(1, 1, early, early + NS_PER_SEC)]),
            vec![(day(), 1)]
        );
        assert_eq!(
            flush(&mut a, &[span(1, 2, late, late + NS_PER_SEC)]),
            vec![(day(), 1)]
        );
    }

    #[test]
    fn one_flush_across_midnight_lands_in_day_d() {
        let mut a = anchors();
        let early = at(day(), 23, 59, 50);
        let late = at(day().succ_opt().unwrap(), 0, 0, 10);
        assert_eq!(
            flush(
                &mut a,
                &[
                    span(1, 1, early, early + NS_PER_SEC),
                    span(1, 2, late, late + NS_PER_SEC),
                ]
            ),
            vec![(day(), 2)]
        );
    }

    #[test]
    fn a_late_root_follows_the_anchor_to_day_d_plus_one() {
        let mut a = anchors();
        let next = day().succ_opt().unwrap();
        let child = at(next, 0, 0, 20);
        let root = at(day(), 23, 59, 40);
        assert_eq!(
            flush(&mut a, &[span(1, 1, child, child + NS_PER_SEC)]),
            vec![(next, 1)]
        );
        assert_eq!(
            flush(&mut a, &[span(1, 2, root, root + NS_PER_SEC)]),
            vec![(next, 1)]
        );
    }

    #[test]
    fn a_row_two_days_from_the_anchor_stays_in_its_own_day() {
        let mut a = anchors();
        let far = day().succ_opt().unwrap().succ_opt().unwrap();
        let first = at(day(), 12, 0, 0);
        let second = at(far, 12, 0, 0);
        assert_eq!(
            flush(
                &mut a,
                &[
                    span(1, 1, first, first + NS_PER_SEC),
                    span(1, 2, second, second + NS_PER_SEC),
                ]
            ),
            vec![(day(), 1), (far, 1)]
        );
    }

    #[test]
    fn a_late_row_two_days_after_the_anchor_stays_in_its_own_day() {
        let mut a = anchors();
        let early = at(day(), 23, 59, 50);
        let far = day().succ_opt().unwrap().succ_opt().unwrap();
        let late = at(far, 12, 0, 0);
        assert_eq!(
            flush(&mut a, &[span(1, 1, early, early + NS_PER_SEC)]),
            vec![(day(), 1)]
        );
        assert_eq!(
            flush(&mut a, &[span(1, 2, late, late + NS_PER_SEC)]),
            vec![(far, 1)]
        );
    }

    #[test]
    fn an_expired_entry_no_longer_anchors_a_late_row() {
        let mut a = anchors();
        let early = at(day(), 23, 59, 50);
        let ingest = early + NS_PER_SEC;
        assert_eq!(
            flush(&mut a, &[span(1, 1, early, ingest)]),
            vec![(day(), 1)]
        );
        // A different trace, ingested past the first entry's expiry, advances
        // the clock and drops it without refreshing it.
        let noon = at(day(), 12, 0, 0);
        let past = ingest + 3 * HOUR_NS;
        flush(&mut a, &[span(2, 2, noon, past)]);
        let late = at(day().succ_opt().unwrap(), 0, 10, 0);
        assert_eq!(
            flush(&mut a, &[span(1, 3, late, late + NS_PER_SEC)]),
            vec![(day().succ_opt().unwrap(), 1)]
        );
    }

    #[test]
    fn backfilled_events_ingested_now_are_anchored_by_their_start_day() {
        let mut a = anchors();
        let old = day() - ChronoDuration::days(30);
        let early = at(old, 23, 59, 50);
        let late = at(old.succ_opt().unwrap(), 0, 0, 10);
        let now = at(day(), 12, 0, 0);
        // Two flushes: the entry made by the first must survive to the second
        // because expiry counts ingest time, not the 30-day-old event time.
        assert_eq!(flush(&mut a, &[span(1, 1, early, now)]), vec![(old, 1)]);
        assert_eq!(
            flush(&mut a, &[span(1, 2, late, now + HOUR_NS)]),
            vec![(old, 1)]
        );
    }

    /// Spec 03 § Trace anchor: an existing entry is refreshed by every flush
    /// with rows for that trace, not only flushes with a near-midnight row.
    #[test]
    fn an_existing_entry_is_refreshed_without_a_midnight_row() {
        let mut a = anchors();
        let early = at(day(), 23, 59, 50);
        let ingest = early + NS_PER_SEC;
        assert_eq!(
            flush(&mut a, &[span(1, 1, early, ingest)]),
            vec![(day(), 1)]
        );
        // Noon is far from midnight and three ingest-hours later. Without a
        // refresh, the original deadline (ingest + 2h) would pass the clock
        // and the entry would be dropped.
        let noon = at(day().succ_opt().unwrap(), 12, 0, 0);
        let past = ingest + 3 * HOUR_NS;
        assert_eq!(flush(&mut a, &[span(1, 2, noon, past)]), vec![(day(), 1)]);
        // The refreshed anchor still pulls a late row back to day D.
        let late = at(day().succ_opt().unwrap(), 0, 0, 10);
        assert_eq!(
            flush(&mut a, &[span(1, 3, late, past + NS_PER_SEC)]),
            vec![(day(), 1)]
        );
    }

    #[test]
    fn a_failed_attempt_changes_no_state_and_assigns_the_same_days() {
        let mut a = anchors();
        let early = at(day(), 23, 59, 50);
        let view = view(&[span(1, 1, early, early + NS_PER_SEC)]);
        let (first, plan) = a.partition(&view).unwrap();
        let (retry, _) = a.partition(&view).unwrap();
        assert_eq!(days(&first), days(&retry));
        assert!(
            a.entries.is_empty(),
            "an uncommitted attempt wrote an entry"
        );
        a.commit(plan);
        assert_eq!(a.entries.len(), 1);
    }

    /// Three row groups, with `last_ingest_time` 10, 20 and 30.
    fn three_row_groups() -> ParquetMetaData {
        let field = |name: &str, data_type: DataType| Field::new(name, data_type, false);
        let timestamp = DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()));
        let schema = Arc::new(Schema::new(vec![
            field(traces::TRACE_ID, DataType::FixedSizeBinary(16)),
            field(traces::START_TIME, timestamp.clone()),
            field(traces::LAST_INGEST_TIME, timestamp),
        ]));
        let mut ids = FixedSizeBinaryBuilder::with_capacity(3, 16);
        let mut starts =
            arrow::array::TimestampNanosecondBuilder::with_capacity(3).with_timezone("UTC");
        let mut ingests =
            arrow::array::TimestampNanosecondBuilder::with_capacity(3).with_timezone("UTC");
        for (index, ingest) in [10_i64, 20, 30].iter().enumerate() {
            let mut bytes = [0_u8; 16];
            bytes[15] = u8::try_from(index + 1).unwrap();
            ids.append_value(bytes).unwrap();
            starts.append_value(*ingest);
            ingests.append_value(*ingest);
        }
        let batch = RecordBatch::try_new(
            Arc::clone(&schema),
            vec![
                Arc::new(ids.finish()),
                Arc::new(starts.finish()),
                Arc::new(ingests.finish()),
            ],
        )
        .unwrap();
        let properties = WriterProperties::builder()
            .set_max_row_group_row_count(Some(1))
            .set_statistics_enabled(EnabledStatistics::Chunk)
            .build();
        let mut buffer = Vec::new();
        {
            let mut writer = ArrowWriter::try_new(&mut buffer, schema, Some(properties)).unwrap();
            writer.write(&batch).unwrap();
            writer.close().unwrap();
        }
        let mut reader = parquet::file::metadata::ParquetMetaDataReader::new();
        reader.try_parse(&bytes::Bytes::from(buffer)).unwrap();
        let metadata = reader.finish().unwrap();
        assert_eq!(metadata.num_row_groups(), 3);
        metadata
    }

    #[test]
    fn row_group_selection_keeps_only_groups_with_a_recent_max() {
        let metadata = three_row_groups();
        assert_eq!(row_groups_to_read(&metadata, 0), vec![0, 1, 2]);
        assert_eq!(row_groups_to_read(&metadata, 20), vec![1, 2]);
        assert_eq!(row_groups_to_read(&metadata, 31), Vec::<usize>::new());
    }
}
