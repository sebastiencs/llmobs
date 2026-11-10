//! In-memory batches for one table, partitioned by project and UTC day.
//!
//! Spec 03 §2. In M1 a span's day is the UTC day of its own `start_time`.
//! The trace anchor (spec 03 § Trace anchor) replaces that in M3; this module
//! has no anchor map.
//!
//! The writer task inserts here. Tests call it until that task lands.
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::Arc;

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
}

/// Active rows of one table. M1 has no frozen memtable.
#[derive(Debug)]
pub(crate) struct Memtable {
    table: Table,
    partitions: BTreeMap<PartitionKey, Vec<MemBatch>>,
    bytes: usize,
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
            partitions: BTreeMap::new(),
            bytes: 0,
        }
    }

    /// Bytes of every stored batch, from Arrow's buffer sizes.
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Partitions and their batches, in key order.
    pub(crate) fn partitions(&self) -> &BTreeMap<PartitionKey, Vec<MemBatch>> {
        &self.partitions
    }

    /// Appends `batch` under `project`, splitting it when its rows cover more than one day.
    ///
    /// The day is the UTC day of [`Table::time_column`]. For spans that is
    /// `start_time` in M1; M3 switches spans to the trace anchor.
    pub(crate) fn insert(
        &mut self,
        project: ProjectId,
        batch: RecordBatch,
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
            self.store(project, days[0], batch)?;
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
                self.store(project, day, filtered)?;
            }
        }
        Ok(())
    }

    /// Clones the map of `Arc`s. Cost follows the number of stored batches.
    pub(crate) fn snapshot(&self) -> Arc<MemSnapshot> {
        Arc::new(MemSnapshot {
            partitions: self.partitions.clone(),
        })
    }

    fn store(
        &mut self,
        project: ProjectId,
        day: NaiveDate,
        batch: RecordBatch,
    ) -> Result<(), MemtableError> {
        if batch.num_rows() == 0 {
            return Ok(());
        }
        let stored = MemBatch::from_batch(self.table, batch)?;
        let key = PartitionKey { project, day };
        let partition = self.partitions.entry(key).or_default();
        partition.push(stored);
        coalesce(partition)?;
        self.bytes = self
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
        })
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
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

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
        table.insert(project(), batch).unwrap();
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
        table.insert(project(), spans_at(&[-1])).unwrap();
        table.insert(project(), spans_at(&[-DAY])).unwrap();
        table.insert(project(), spans_at(&[-DAY - 1])).unwrap();
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
            .insert(project(), scores_to_record_batch(&[score]).unwrap())
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
            table.insert(project(), spans_at(&[offset])).unwrap();
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
            table.insert(project(), spans_at(&[0])).unwrap();
        }
        let key = PartitionKey {
            project: project(),
            day: date(1970, 1, 1),
        };
        let snapshot = table.snapshot();
        let original = Arc::clone(&snapshot.partitions[&key][0].batch);
        for _ in 0..60 {
            table.insert(project(), spans_at(&[0])).unwrap();
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
            table.insert(project(), spans_at(&[0])).unwrap();
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
        table.insert(project(), spans_at(&[0])).unwrap();
        table.insert(other_project(), spans_at(&[0])).unwrap();
        assert_eq!(table.partitions().len(), 2);
    }

    #[test]
    fn extreme_timestamps_do_not_panic() {
        let mut table = Memtable::new(Table::Spans);
        table.insert(project(), spans_at(&[i64::MIN])).unwrap();
        table
            .insert(other_project(), spans_at(&[i64::MAX]))
            .unwrap();
        assert_eq!(table.partitions().len(), 2);
    }
}
