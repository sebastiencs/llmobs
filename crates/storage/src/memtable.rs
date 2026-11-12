//! In-memory batches for one table, partitioned by project and UTC day.
//!
//! Spec 03 §2. In M1 a span's day is the UTC day of its own `start_time`.
//! The trace anchor (spec 03 § Trace anchor) replaces that in M3; this module
//! has no anchor map.
//!
//! Each table has an active memtable and at most one frozen memtable. The
//! writer inserts into the active one. The flush task calls `freeze`.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use arrow::array::{Array, BooleanArray, RecordBatch, TimestampNanosecondArray};
use arrow::compute::{concat_batches, filter_record_batch};
use chrono::NaiveDate;
use llmobs_core::ids::ProjectId;

use crate::error::MemtableError;
use crate::wal::Table;

/// Nanoseconds in a UTC day. Days are exact; UTC has no leap seconds here.
const DAY_NS: i64 = 86_400_000_000_000;

/// `NaiveDate::num_days_from_ce` of 1970-01-01.
const UNIX_EPOCH_DAYS_FROM_CE: i64 = 719_163;

/// A batch smaller than this is eligible for coalescing.
const SMALL_BATCH: usize = 1024 * 1024;

/// Coalesce once a partition holds more small batches than this.
const MAX_SMALL_BATCHES: usize = 64;

/// `(project, day)`, ordered by project then day.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct PartitionKey {
    /// Project that owns the rows.
    pub project: ProjectId,
    /// UTC day of the table's time column.
    pub day: NaiveDate,
}

/// One stored batch and the accounting a snapshot needs.
#[derive(Debug, Clone)]
pub(crate) struct MemBatch {
    /// Rows as inserted, or a concatenation of small batches.
    pub batch: Arc<RecordBatch>,
    /// Minimum of the table's time column.
    pub min_time: i64,
    /// Maximum of the table's time column.
    pub max_time: i64,
    /// [`RecordBatch::get_array_memory_size`] of `batch`.
    pub bytes: usize,
    /// Trace partials of `batch`, computed once and shared by every read state
    /// that holds this `MemBatch`. A coalesced batch starts with a fresh cache.
    pub partials: Arc<OnceLock<Arc<RecordBatch>>>,
}

/// Rows accepting inserts, and the flush trigger measured from them.
#[derive(Debug, Default)]
struct Active {
    partitions: BTreeMap<PartitionKey, Vec<MemBatch>>,
    bytes: usize,
    /// Highest WAL seq stored here. `0` when nothing has been inserted.
    max_seq: u64,
    /// When the first batch of this memtable was inserted.
    oldest: Option<Instant>,
}

impl Active {
    /// Records `seq` for a batch that was actually stored.
    fn note(&mut self, seq: u64) {
        if self.oldest.is_none() {
            self.oldest = Some(Instant::now());
        }
        self.max_seq = self.max_seq.max(seq);
    }
}

/// One frozen memtable. A flush retries against this until `drop_frozen`.
#[derive(Debug)]
struct Frozen {
    partitions: Arc<BTreeMap<PartitionKey, Vec<MemBatch>>>,
    bytes: usize,
    max_seq: u64,
}

impl Frozen {
    fn from_active(active: Active) -> Self {
        Self {
            partitions: Arc::new(active.partitions),
            bytes: active.bytes,
            max_seq: active.max_seq,
        }
    }

    fn view(&self) -> FrozenView {
        FrozenView {
            partitions: Arc::clone(&self.partitions),
            max_seq: self.max_seq,
        }
    }
}

/// The frozen partitions a flush encodes. A retry receives this same `Arc`.
#[derive(Debug, Clone)]
pub(crate) struct FrozenView {
    /// Batches grouped by partition, in key order.
    pub partitions: Arc<BTreeMap<PartitionKey, Vec<MemBatch>>>,
    /// Highest WAL seq in `partitions`.
    pub max_seq: u64,
}

/// Active rows plus at most one frozen memtable.
#[derive(Debug)]
pub(crate) struct Memtable {
    table: Table,
    active: Active,
    frozen: Option<Frozen>,
}

/// A cheap copy of the batches a query sees.
///
/// Cloning copies the `Arc`s, not the columns. A later coalesce replaces the
/// memtable's `Arc`s and leaves this snapshot alone.
#[derive(Debug, Clone)]
pub(crate) struct MemSnapshot {
    /// Batches grouped by partition, in key order.
    pub partitions: BTreeMap<PartitionKey, Vec<MemBatch>>,
}

impl Memtable {
    /// An empty memtable for `table`.
    pub(crate) fn new(table: Table) -> Self {
        Self {
            table,
            active: Active::default(),
            frozen: None,
        }
    }

    /// Bytes of the active and frozen batches, from Arrow's buffer sizes.
    ///
    /// Backpressure compares this with `memtable.hard_limit`.
    pub(crate) fn bytes(&self) -> usize {
        self.active
            .bytes
            .saturating_add(self.frozen.as_ref().map(|frozen| frozen.bytes).unwrap_or(0))
    }

    /// Bytes of the active memtable. The size flush trigger compares this with
    /// `flush.max_bytes`.
    pub(crate) fn active_bytes(&self) -> usize {
        self.active.bytes
    }

    /// Highest WAL seq in the active memtable, or `0` when it is empty.
    pub(crate) fn active_max_seq(&self) -> u64 {
        self.active.max_seq
    }

    /// When the first batch of the active memtable was inserted.
    ///
    /// The age flush trigger (M2.6) waits until this plus `flush.max_age`.
    /// Replay counts as the insert.
    pub(crate) fn active_oldest(&self) -> Option<Instant> {
        self.active.oldest
    }

    /// Active partitions and their batches, in key order.
    ///
    /// A [`Self::snapshot`] also includes the frozen batches.
    pub(crate) fn partitions(&self) -> &BTreeMap<PartitionKey, Vec<MemBatch>> {
        &self.active.partitions
    }

    /// A frozen memtable is waiting to be flushed.
    pub(crate) fn has_frozen(&self) -> bool {
        self.frozen.is_some()
    }

    /// Moves a non-empty active memtable to frozen and returns it.
    ///
    /// A second call, while that frozen memtable is still here, returns the
    /// same view and leaves the active memtable in place. An empty active
    /// memtable with nothing frozen returns `None`.
    pub(crate) fn freeze(&mut self) -> Option<FrozenView> {
        if let Some(frozen) = &self.frozen {
            return Some(frozen.view());
        }
        if self.active.partitions.is_empty() {
            return None;
        }
        let active = std::mem::take(&mut self.active);
        let frozen = Frozen::from_active(active);
        let view = frozen.view();
        self.frozen = Some(frozen);
        Some(view)
    }

    /// Drops the frozen memtable. The active memtable stays.
    pub(crate) fn drop_frozen(&mut self) {
        self.frozen = None;
    }

    /// Appends `batch` under `project`, splitting it when its rows cover more than one day.
    ///
    /// `seq` is the WAL record's sequence number. The day is the UTC day of
    /// [`Table::time_column`]. For spans that is `start_time` in M1; M3
    /// switches spans to the trace anchor. An empty batch changes nothing.
    pub(crate) fn insert(
        &mut self,
        project: ProjectId,
        batch: RecordBatch,
        seq: u64,
    ) -> Result<(), MemtableError> {
        if batch.num_rows() == 0 {
            return Ok(());
        }
        let times = time_array(self.table, &batch)?;
        let mut days = Vec::with_capacity(batch.num_rows());
        for row in 0..batch.num_rows() {
            if times.is_null(row) {
                return Err(MemtableError::BadColumn {
                    column: self.table.time_column(),
                });
            }
            days.push(utc_day(times.value(row))?);
        }
        let same_day = days.iter().all(|day| *day == days[0]);
        if same_day {
            self.store(project, days[0], batch, seq)?;
        } else {
            let mut distinct = days.clone();
            distinct.sort();
            distinct.dedup();
            for day in distinct {
                let mask = BooleanArray::from(
                    days.iter()
                        .map(|row_day| *row_day == day)
                        .collect::<Vec<_>>(),
                );
                let filtered = filter_record_batch(&batch, &mask).map_err(MemtableError::Arrow)?;
                self.store(project, day, filtered, seq)?;
            }
        }
        Ok(())
    }

    /// Clones the map of `Arc`s, frozen batches before active ones.
    ///
    /// Cost follows the number of stored batches. A reader cannot tell the two
    /// memtables apart.
    pub(crate) fn snapshot(&self) -> Arc<MemSnapshot> {
        let mut partitions = self
            .frozen
            .as_ref()
            .map(|frozen| frozen.partitions.as_ref().clone())
            .unwrap_or_default();
        for (key, batches) in &self.active.partitions {
            partitions
                .entry(*key)
                .or_default()
                .extend(batches.iter().cloned());
        }
        Arc::new(MemSnapshot { partitions })
    }

    fn store(
        &mut self,
        project: ProjectId,
        day: NaiveDate,
        batch: RecordBatch,
        seq: u64,
    ) -> Result<(), MemtableError> {
        if batch.num_rows() == 0 {
            return Ok(());
        }
        let stored = MemBatch::from_batch(self.table, batch)?;
        let key = PartitionKey { project, day };
        let partition = self.active.partitions.entry(key).or_default();
        partition.push(stored);
        coalesce(partition)?;
        self.active.note(seq);
        self.active.bytes = self
            .active
            .partitions
            .values()
            .flatten()
            .map(|batch| batch.bytes)
            .sum();
        Ok(())
    }
}

impl MemBatch {
    fn from_batch(table: Table, batch: RecordBatch) -> Result<Self, MemtableError> {
        let times = time_array(table, &batch)?;
        let mut min_time = i64::MAX;
        let mut max_time = i64::MIN;
        for row in 0..times.len() {
            if times.is_null(row) {
                return Err(MemtableError::BadColumn {
                    column: table.time_column(),
                });
            }
            let value = times.value(row);
            min_time = min_time.min(value);
            max_time = max_time.max(value);
        }
        let bytes = batch.get_array_memory_size();
        Ok(Self {
            batch: Arc::new(batch),
            min_time,
            max_time,
            bytes,
            partials: Arc::new(OnceLock::new()),
        })
    }

    /// Trace partials of this spans batch, computed at most once and cached.
    ///
    /// The `Arc<OnceLock<...>>` is shared by every clone of this `MemBatch`, so
    /// every read state that holds it reuses the same partials. Errors are not
    /// cached (a valid spans batch always builds its partials).
    pub(crate) fn trace_partials(&self) -> Result<Arc<RecordBatch>, crate::error::PartialsError> {
        if let Some(cached) = self.partials.get() {
            return Ok(Arc::clone(cached));
        }
        let built = Arc::new(crate::partials::partials_from_spans(self.batch.as_ref())?);
        // A concurrent reader may have won the race; both values are built from
        // the same immutable batch, so returning either is equivalent.
        let _ = self.partials.set(Arc::clone(&built));
        Ok(Arc::clone(self.partials.get().unwrap_or(&built)))
    }
}

fn time_array(
    table: Table,
    batch: &RecordBatch,
) -> Result<&TimestampNanosecondArray, MemtableError> {
    let column = table.time_column();
    let array = batch
        .column_by_name(column)
        .ok_or(MemtableError::MissingColumn { column })?;
    array
        .as_any()
        .downcast_ref::<TimestampNanosecondArray>()
        .ok_or(MemtableError::BadColumn { column })
}

fn utc_day(nanos: i64) -> Result<NaiveDate, MemtableError> {
    let days = nanos.div_euclid(DAY_NS);
    let from_ce = days
        .checked_add(UNIX_EPOCH_DAYS_FROM_CE)
        .ok_or(MemtableError::TimeOutOfRange)?;
    let from_ce = i32::try_from(from_ce).map_err(|_| MemtableError::TimeOutOfRange)?;
    NaiveDate::from_num_days_from_ce_opt(from_ce).ok_or(MemtableError::TimeOutOfRange)
}

/// Concatenates every small batch, in the order they appear, into one.
///
/// The coalesced batch takes the place of the first small batch. Large batches
/// stay where they are. A run of only small batches therefore keeps row order.
fn coalesce(batches: &mut Vec<MemBatch>) -> Result<(), MemtableError> {
    let small = batches
        .iter()
        .filter(|batch| batch.bytes < SMALL_BATCH)
        .count();
    if small <= MAX_SMALL_BATCHES {
        return Ok(());
    }
    let mut smalls = Vec::with_capacity(small);
    enum Slot {
        Large(MemBatch),
        Coalesced,
    }
    let mut slots = Vec::with_capacity(batches.len() - small + 1);
    for batch in batches.drain(..) {
        if batch.bytes < SMALL_BATCH {
            if smalls.is_empty() {
                slots.push(Slot::Coalesced);
            }
            smalls.push(batch);
        } else {
            slots.push(Slot::Large(batch));
        }
    }
    let coalesced = concat_small(&smalls)?;
    let mut pending = Some(coalesced);
    *batches = slots
        .into_iter()
        .filter_map(|slot| match slot {
            Slot::Large(batch) => Some(batch),
            Slot::Coalesced => pending.take(),
        })
        .collect();
    Ok(())
}

fn concat_small(smalls: &[MemBatch]) -> Result<MemBatch, MemtableError> {
    let Some(first) = smalls.first() else {
        return Err(MemtableError::Arrow(
            arrow::error::ArrowError::InvalidArgumentError("no batches to coalesce".into()),
        ));
    };
    let schema = first.batch.schema();
    let refs: Vec<&RecordBatch> = smalls.iter().map(|batch| batch.batch.as_ref()).collect();
    let merged = concat_batches(&schema, refs).map_err(MemtableError::Arrow)?;
    let bytes = merged.get_array_memory_size();
    let min_time = smalls
        .iter()
        .map(|batch| batch.min_time)
        .min()
        .unwrap_or(first.min_time);
    let max_time = smalls
        .iter()
        .map(|batch| batch.max_time)
        .max()
        .unwrap_or(first.max_time);
    Ok(MemBatch {
        batch: Arc::new(merged),
        min_time,
        max_time,
        bytes,
        partials: Arc::new(OnceLock::new()),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::Instant;

    use arrow::array::{Array, TimestampNanosecondArray};
    use chrono::NaiveDate;
    use llmobs_core::batch::scores_to_record_batch;
    use llmobs_core::enums::ScoreSource;
    use llmobs_core::ids::{ProjectId, TraceId};
    use llmobs_core::schema::spans::START_TIME;
    use llmobs_core::score::{Score, ScoreTarget, ScoreValue};

    use super::*;
    use crate::wal::spans_at;

    const DAY: i64 = DAY_NS;

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn other_project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000002".parse().unwrap()
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).unwrap()
    }

    fn start_times(batch: &MemBatch) -> Vec<i64> {
        let column = batch
            .batch
            .column_by_name(START_TIME)
            .unwrap()
            .as_any()
            .downcast_ref::<TimestampNanosecondArray>()
            .unwrap();
        (0..column.len()).map(|row| column.value(row)).collect()
    }

    #[test]
    fn a_batch_spanning_three_days_lands_in_three_partitions() {
        let mut table = Memtable::new(Table::Spans);
        let batch = spans_at(&[0, 50, DAY, DAY + 10, 2 * DAY]);
        table.insert(project(), batch, 1).unwrap();
        let parts = table.partitions();
        assert_eq!(parts.len(), 3);
        let day0 = &parts[&PartitionKey {
            project: project(),
            day: date(1970, 1, 1),
        }];
        let day1 = &parts[&PartitionKey {
            project: project(),
            day: date(1970, 1, 2),
        }];
        let day2 = &parts[&PartitionKey {
            project: project(),
            day: date(1970, 1, 3),
        }];
        assert_eq!(day0[0].batch.num_rows(), 2);
        assert_eq!(day0[0].min_time, 0);
        assert_eq!(day0[0].max_time, 50);
        assert_eq!(day1[0].batch.num_rows(), 2);
        assert_eq!(day1[0].min_time, DAY);
        assert_eq!(day1[0].max_time, DAY + 10);
        assert_eq!(day2[0].batch.num_rows(), 1);
        assert_eq!(day2[0].min_time, 2 * DAY);
        assert_eq!(day2[0].max_time, 2 * DAY);
    }

    #[test]
    fn times_before_1970_land_on_the_previous_day() {
        let mut table = Memtable::new(Table::Spans);
        table.insert(project(), spans_at(&[-1]), 1).unwrap();
        table.insert(project(), spans_at(&[-DAY]), 2).unwrap();
        table.insert(project(), spans_at(&[-DAY - 1]), 3).unwrap();
        let parts = table.partitions();
        assert!(parts.contains_key(&PartitionKey {
            project: project(),
            day: date(1969, 12, 31),
        }));
        assert!(parts.contains_key(&PartitionKey {
            project: project(),
            day: date(1969, 12, 30),
        }));
        let dec_31 = &parts[&PartitionKey {
            project: project(),
            day: date(1969, 12, 31),
        }];
        assert_eq!(dec_31.len(), 2);
        assert_eq!(dec_31[0].min_time, -1);
        assert_eq!(dec_31[1].min_time, -DAY);
    }

    #[test]
    fn scores_use_created_at() {
        let mut table = Memtable::new(Table::Scores);
        let score = Score {
            project_id: project(),
            score_id: "018f6b4e-7c1a-7c1a-8c1a-0000000000ab".parse().unwrap(),
            target: ScoreTarget::Trace {
                trace_id: "4bf92f3577b34da6a3ce929d0e0e4736"
                    .parse::<TraceId>()
                    .unwrap(),
            },
            session_id: None,
            user_id: None,
            name: "accuracy".to_owned(),
            value: ScoreValue::Numeric(0.5),
            reason: None,
            source: ScoreSource::Api,
            evaluator_id: None,
            rule_id: None,
            experiment_id: None,
            dataset_item_id: None,
            author_user_id: None,
            metadata: BTreeMap::new(),
            created_at: DAY + 5,
            updated_at: DAY + 6,
            deleted: false,
            ingest_time: DAY + 7,
        };
        table
            .insert(project(), scores_to_record_batch(&[score]).unwrap(), 1)
            .unwrap();
        assert!(table.partitions().contains_key(&PartitionKey {
            project: project(),
            day: date(1970, 1, 2),
        }));
    }

    #[test]
    fn coalescing_keeps_row_order_and_limits_small_batches() {
        let mut table = Memtable::new(Table::Spans);
        for offset in 0..100 {
            table
                .insert(
                    project(),
                    spans_at(&[offset]),
                    u64::try_from(offset).unwrap(),
                )
                .unwrap();
        }
        let batches = &table.partitions()[&PartitionKey {
            project: project(),
            day: date(1970, 1, 1),
        }];
        let small = batches
            .iter()
            .filter(|batch| batch.bytes < SMALL_BATCH)
            .count();
        assert!(small <= MAX_SMALL_BATCHES);
        assert!(batches.len() < 100);
        let times: Vec<i64> = batches.iter().flat_map(start_times).collect();
        assert_eq!(times, (0..100).collect::<Vec<_>>());
        let recomputed: usize = batches
            .iter()
            .map(|batch| batch.batch.get_array_memory_size())
            .sum();
        assert_eq!(table.bytes(), recomputed);
        assert_eq!(
            batches.iter().map(|batch| batch.bytes).sum::<usize>(),
            recomputed
        );
    }

    #[test]
    fn a_snapshot_keeps_the_batches_from_before_coalescing() {
        let mut table = Memtable::new(Table::Spans);
        for _ in 0..10 {
            table.insert(project(), spans_at(&[0]), 1).unwrap();
        }
        let key = PartitionKey {
            project: project(),
            day: date(1970, 1, 1),
        };
        let snapshot = table.snapshot();
        let original = Arc::clone(&snapshot.partitions[&key][0].batch);
        for _ in 0..60 {
            table.insert(project(), spans_at(&[0]), 1).unwrap();
        }
        assert_eq!(snapshot.partitions[&key].len(), 10);
        assert!(Arc::ptr_eq(&snapshot.partitions[&key][0].batch, &original));
        assert!(table.partitions()[&key].len() < 70);
        assert!(
            table.partitions()[&key]
                .iter()
                .all(|batch| !Arc::ptr_eq(&batch.batch, &original))
        );
    }

    #[test]
    fn ten_thousand_inserts_do_not_keep_every_batch() {
        let mut table = Memtable::new(Table::Spans);
        for _ in 0..10_000 {
            table.insert(project(), spans_at(&[0]), 1).unwrap();
        }
        let batches = &table.partitions()[&PartitionKey {
            project: project(),
            day: date(1970, 1, 1),
        }];
        let small = batches
            .iter()
            .filter(|batch| batch.bytes < SMALL_BATCH)
            .count();
        assert!(small <= MAX_SMALL_BATCHES);
        let rows: usize = batches.iter().map(|batch| batch.batch.num_rows()).sum();
        assert_eq!(rows, 10_000);
        assert!(
            batches.len() <= MAX_SMALL_BATCHES + 8,
            "stored {} batches",
            batches.len()
        );
    }

    #[test]
    fn projects_do_not_share_a_partition() {
        let mut table = Memtable::new(Table::Spans);
        table.insert(project(), spans_at(&[0]), 1).unwrap();
        table.insert(other_project(), spans_at(&[0]), 1).unwrap();
        assert_eq!(table.partitions().len(), 2);
    }

    #[test]
    fn extreme_timestamps_do_not_panic() {
        let mut table = Memtable::new(Table::Spans);
        table.insert(project(), spans_at(&[i64::MIN]), 1).unwrap();
        table
            .insert(other_project(), spans_at(&[i64::MAX]), 2)
            .unwrap();
        assert_eq!(table.partitions().len(), 2);
    }

    fn key() -> PartitionKey {
        PartitionKey {
            project: project(),
            day: date(1970, 1, 1),
        }
    }

    fn snapshot_times(snapshot: &MemSnapshot) -> Vec<i64> {
        snapshot
            .partitions
            .values()
            .flatten()
            .flat_map(start_times)
            .collect()
    }

    fn batch_ptrs(snapshot: &MemSnapshot) -> Vec<*const ()> {
        snapshot
            .partitions
            .values()
            .flatten()
            .map(|batch| Arc::as_ptr(&batch.batch) as *const ())
            .collect()
    }

    #[test]
    fn freeze_moves_every_row_and_snapshots_keep_them() {
        let mut table = Memtable::new(Table::Spans);
        table.insert(project(), spans_at(&[1, 2]), 4).unwrap();
        table.insert(project(), spans_at(&[3]), 7).unwrap();
        let before = table.snapshot();
        let bytes = table.bytes();
        let view = table.freeze().expect("active rows");
        let after = table.snapshot();
        assert_eq!(snapshot_times(&before), vec![1, 2, 3]);
        assert_eq!(snapshot_times(&after), snapshot_times(&before));
        assert_eq!(batch_ptrs(&after), batch_ptrs(&before));
        assert!(table.partitions().is_empty());
        assert_eq!(table.bytes(), bytes);
        assert_eq!(table.active_bytes(), 0);
        assert_eq!(view.max_seq, 7);
        assert_eq!(table.active_max_seq(), 0);
        assert!(table.active_oldest().is_none());
    }

    #[test]
    fn inserts_after_a_freeze_stay_active_and_a_retry_returns_the_same_view() {
        let mut table = Memtable::new(Table::Spans);
        table.insert(project(), spans_at(&[1]), 4).unwrap();
        let view = table.freeze().expect("active rows");
        table.insert(project(), spans_at(&[2]), 9).unwrap();
        assert_eq!(start_times(&table.partitions()[&key()][0]), vec![2]);
        assert_eq!(snapshot_times(&table.snapshot()), vec![1, 2]);
        let again = table.freeze().expect("frozen rows");
        assert!(Arc::ptr_eq(&view.partitions, &again.partitions));
        assert_eq!(again.max_seq, 4);
        assert_eq!(snapshot_times(&table.snapshot()), vec![1, 2]);
        assert_eq!(table.active_max_seq(), 9);
    }

    #[test]
    fn drop_frozen_removes_only_the_frozen_rows() {
        let mut table = Memtable::new(Table::Spans);
        table.insert(project(), spans_at(&[1]), 4).unwrap();
        table.freeze().expect("active rows");
        table.insert(project(), spans_at(&[2]), 9).unwrap();
        let active = table.active_bytes();
        table.drop_frozen();
        assert_eq!(snapshot_times(&table.snapshot()), vec![2]);
        assert_eq!(table.bytes(), active);
        assert_eq!(table.active_max_seq(), 9);
        table.drop_frozen();
        assert_eq!(snapshot_times(&table.snapshot()), vec![2]);
    }

    #[test]
    fn bytes_count_the_active_and_frozen_memtables() {
        let mut table = Memtable::new(Table::Spans);
        table.insert(project(), spans_at(&[1]), 1).unwrap();
        let frozen_bytes = table.bytes();
        table.freeze().expect("active rows");
        table.insert(project(), spans_at(&[2]), 2).unwrap();
        assert_eq!(table.bytes(), frozen_bytes + table.active_bytes());
        assert!(table.active_bytes() > 0);
        assert!(table.bytes() > table.active_bytes());
    }

    #[test]
    fn max_seq_and_oldest_follow_the_active_memtable() {
        let mut table = Memtable::new(Table::Spans);
        assert!(table.freeze().is_none());
        assert!(table.active_oldest().is_none());
        table.insert(project(), spans_at(&[]), 99).unwrap();
        assert!(table.active_oldest().is_none());
        assert_eq!(table.active_max_seq(), 0);
        let started = Instant::now();
        table.insert(project(), spans_at(&[1]), 3).unwrap();
        let oldest = table.active_oldest().expect("first insert");
        assert!(oldest >= started);
        table.insert(project(), spans_at(&[2]), 8).unwrap();
        assert_eq!(table.active_oldest(), Some(oldest));
        table.insert(project(), spans_at(&[3]), 5).unwrap();
        assert_eq!(table.active_max_seq(), 8);
        let view = table.freeze().expect("active rows");
        assert_eq!(view.max_seq, 8);
        assert!(table.active_oldest().is_none());
        assert_eq!(table.active_max_seq(), 0);
        table.insert(project(), spans_at(&[4]), 11).unwrap();
        let restarted = table.active_oldest().expect("new active memtable");
        assert!(restarted >= oldest);
        assert_eq!(table.active_max_seq(), 11);
    }
}
