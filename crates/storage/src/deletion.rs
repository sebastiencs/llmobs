//! Deletion requests, their tombstone set, and the covered-row rule (spec 03 §7).
//!
//! [`covered_predicate`] is the one place the covered-row rule exists (D-A1-3):
//! queries, flush and compaction all use it.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use arrow::array::{Array, AsArray, RecordBatch};
use arrow::compute::{filter_record_batch, not, prep_null_mask_filter};
use arrow::datatypes::{DataType, SchemaRef};
use chrono::NaiveDate;
use datafusion::common::ScalarValue;
use datafusion::error::DataFusionError;
use datafusion::logical_expr::Operator;
use datafusion::physical_expr::PhysicalExpr;
use datafusion::physical_expr::expressions::{BinaryExpr, Column, IsNotNullExpr, Literal, in_list};
use llmobs_core::ids::{ProjectId, TraceId};
use llmobs_core::schema::{scores, spans, traces};
use uuid::Uuid;

use crate::catalog::FileTable;
use crate::error::QueryError;
use crate::memtable::{MemBatch, MemSnapshot, PartitionKey};
use crate::wal::Table;

/// One deletion request handed over by the server (spec 03 §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deletion {
    /// The server's `meta.db` id, UUIDv7. Registration is idempotent by it.
    pub id: Uuid,
    /// The project whose data the deletion covers.
    pub project: ProjectId,
    /// Rows with `ingest_time` at or before this (ns) are covered.
    pub cutoff: i64,
    /// Which rows the deletion covers.
    pub scope: DeletionScope,
}

/// Which rows a deletion covers (spec 03 §7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeletionScope {
    /// The rows of the listed traces.
    Traces {
        /// Covered trace ids.
        trace_ids: Vec<TraceId>,
    },
    /// The rows of one user: their trace ids, their user id and, on `scores`
    /// only, their session ids.
    User {
        /// The user the deletion is about.
        user_id: String,
        /// Covered trace ids.
        trace_ids: Vec<TraceId>,
        /// Covered session ids; they match `scores` rows only.
        session_ids: Vec<String>,
    },
}

/// How far a deletion has progressed (spec 03 §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeletionStatus {
    /// Registered; its rows are hidden by tombstones.
    Pending,
    /// Every file and memtable batch that could hold a covered row has been
    /// rewritten or checked, at `applied_at` (ns).
    Applied {
        /// When the deletion was marked applied, ns.
        applied_at: i64,
    },
}

/// A pending deletion as the read path and the jobs use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingDeletion {
    pub(crate) id: Uuid,
    pub(crate) seq: u64,
    pub(crate) project: ProjectId,
    pub(crate) cutoff: i64,
    pub(crate) registered_at: i64,
    pub(crate) trace_ids: HashSet<[u8; 16]>,
    /// Empty for a `Traces` scope, one entry for a `User` scope.
    pub(crate) users: HashSet<String>,
    /// Empty for a `Traces` scope.
    pub(crate) sessions: HashSet<String>,
}

/// Every pending deletion, by project, plus the known sequence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Tombstones {
    known_seq: u64,
    by_project: HashMap<ProjectId, Arc<[Arc<PendingDeletion>]>>,
}

impl Tombstones {
    /// The highest `deletion_seq` registered so far, pending or applied, or 0.
    pub(crate) fn known_seq(&self) -> u64 {
        self.known_seq
    }

    /// The project's pending deletions in `seq` order; empty when it has none.
    pub(crate) fn for_project(&self, project: ProjectId) -> Arc<[Arc<PendingDeletion>]> {
        self.by_project
            .get(&project)
            .map_or_else(|| Arc::from(Vec::new()), Arc::clone)
    }

    /// Every pending deletion, in `seq` order.
    pub(crate) fn all(&self) -> Vec<Arc<PendingDeletion>> {
        let mut all: Vec<_> = self
            .by_project
            .values()
            .flat_map(|deletions| deletions.iter().map(Arc::clone))
            .collect();
        all.sort_by_key(|deletion| deletion.seq);
        all
    }

    /// A copy with `deletion` added and `known_seq = max(known_seq, deletion.seq)`.
    ///
    /// The project's slice stays sorted by `seq`.
    pub(crate) fn with(&self, deletion: PendingDeletion) -> Tombstones {
        let project = deletion.project;
        let seq = deletion.seq;
        let mut by_project = self.by_project.clone();
        let mut list: Vec<Arc<PendingDeletion>> = by_project
            .remove(&project)
            .map(|slice| slice.iter().cloned().collect())
            .unwrap_or_default();
        let position = list.partition_point(|existing| existing.seq <= seq);
        list.insert(position, Arc::new(deletion));
        by_project.insert(project, list.into());
        Tombstones {
            known_seq: self.known_seq.max(seq),
            by_project,
        }
    }

    /// A copy without the deletion `id` of `project`. `known_seq` is unchanged.
    ///
    /// A project whose last deletion goes is removed from the map.
    pub(crate) fn without(&self, project: ProjectId, id: Uuid) -> Tombstones {
        let mut by_project = self.by_project.clone();
        if let Some(slice) = by_project.remove(&project) {
            let mut list: Vec<Arc<PendingDeletion>> = slice.iter().cloned().collect();
            list.retain(|deletion| deletion.id != id);
            if !list.is_empty() {
                by_project.insert(project, list.into());
            }
        }
        Tombstones {
            known_seq: self.known_seq,
            by_project,
        }
    }

    /// The tombstones loaded from `catalog.db`: `deletions` in any order, under
    /// the `known_seq` read beside them (the highest sequence overall, pending
    /// or applied). Each project's slice is sorted by `seq`.
    pub(crate) fn loaded(known_seq: u64, deletions: Vec<PendingDeletion>) -> Tombstones {
        let mut by_project: HashMap<ProjectId, Vec<Arc<PendingDeletion>>> = HashMap::new();
        for deletion in deletions {
            by_project
                .entry(deletion.project)
                .or_default()
                .push(Arc::new(deletion));
        }
        Tombstones {
            known_seq,
            by_project: by_project
                .into_iter()
                .map(|(project, mut list)| {
                    list.sort_by_key(|deletion| deletion.seq);
                    (project, list.into())
                })
                .collect(),
        }
    }
}

/// The columns each table's covered-row predicate reads (spec 03 §7).
pub(crate) fn key_columns(table: FileTable) -> &'static [&'static str] {
    match table {
        FileTable::Spans => &[spans::TRACE_ID, spans::USER_ID, spans::INGEST_TIME],
        FileTable::Scores => &[
            scores::TRACE_ID,
            scores::USER_ID,
            scores::SESSION_ID,
            scores::INGEST_TIME,
        ],
        FileTable::Traces => &[traces::TRACE_ID],
    }
}

/// `Some(expr)` that is true for rows covered by any of `deletions`, against
/// `schema` (the table's stored schema, or a projection of it that holds the
/// columns the rule reads). `None` when `deletions` is empty.
///
/// Per deletion `d`: `ingest_time <= d.cutoff AND (T OR U [OR S])`, where `T` is
/// `trace_id IS NOT NULL AND trace_id IN …`, `U` the same on `user_id` and `S`
/// on `session_id` (`scores` only). A partial carries no per-row ingest time,
/// so [`FileTable::Traces`] gets the trace-id term only (D-A1-3). Every `IN`
/// term is guarded by `IS NOT NULL`, whatever the column's nullability, so the
/// expression is never null.
pub(crate) fn covered_predicate(
    table: FileTable,
    schema: &SchemaRef,
    deletions: &[Arc<PendingDeletion>],
) -> Result<Option<Arc<dyn PhysicalExpr>>, DataFusionError> {
    let mut covered: Option<Arc<dyn PhysicalExpr>> = None;
    for deletion in deletions {
        let mut terms: Vec<Arc<dyn PhysicalExpr>> = Vec::new();
        if !deletion.trace_ids.is_empty() {
            let mut ids: Vec<&[u8; 16]> = deletion.trace_ids.iter().collect();
            ids.sort_unstable();
            terms.push(in_term(
                schema,
                spans::TRACE_ID,
                |id| ScalarValue::FixedSizeBinary(16, Some(id.to_vec())),
                &ids,
            )?);
        }
        if table != FileTable::Traces && !deletion.users.is_empty() {
            let mut users: Vec<&String> = deletion.users.iter().collect();
            users.sort_unstable();
            terms.push(utf8_term(schema, spans::USER_ID, &users)?);
        }
        if table == FileTable::Scores && !deletion.sessions.is_empty() {
            let mut sessions: Vec<&String> = deletion.sessions.iter().collect();
            sessions.sort_unstable();
            terms.push(utf8_term(schema, scores::SESSION_ID, &sessions)?);
        }
        // A deletion whose terms are all absent for this table covers no row of it.
        let Some(mut expression) = or_together(terms) else {
            continue;
        };
        if table != FileTable::Traces {
            let cutoff = Literal::new(ScalarValue::TimestampNanosecond(
                Some(deletion.cutoff),
                timestamp_timezone(schema, spans::INGEST_TIME)?,
            ));
            let ingest = Arc::new(Column::new(
                spans::INGEST_TIME,
                column_index(schema, spans::INGEST_TIME)?,
            )) as Arc<dyn PhysicalExpr>;
            let within = Arc::new(BinaryExpr::new(ingest, Operator::LtEq, Arc::new(cutoff)))
                as Arc<dyn PhysicalExpr>;
            expression = Arc::new(BinaryExpr::new(within, Operator::And, expression));
        }
        covered = Some(match covered {
            None => expression,
            Some(existing) => Arc::new(BinaryExpr::new(existing, Operator::Or, expression)),
        });
    }
    Ok(covered)
}

/// The deletions that may cover rows of a file or batch: those with `seq`
/// above `applied` (`None` for a memtable batch) and `cutoff` at or after
/// `min_ingest_time` (spec 03 §7).
pub(crate) fn applicable(
    deletions: &[Arc<PendingDeletion>],
    applied: Option<u64>,
    min_ingest_time: i64,
) -> Vec<Arc<PendingDeletion>> {
    deletions
        .iter()
        .filter(|deletion| {
            applied.is_none_or(|applied| deletion.seq > applied)
                && deletion.cutoff >= min_ingest_time
        })
        .cloned()
        .collect()
}

/// The rows of `batch` not covered by `predicate`. `batch` holds every column
/// `predicate` reads.
///
/// A null predicate value counts as "not covered": `prep_null_mask_filter`
/// turns it into `false` before the negation, a second guard behind the
/// `IS NOT NULL` terms of [`covered_predicate`].
pub(crate) fn drop_covered(
    batch: &RecordBatch,
    predicate: &Arc<dyn PhysicalExpr>,
) -> Result<RecordBatch, DataFusionError> {
    let covered = predicate.evaluate(batch)?;
    let covered = covered.into_array(batch.num_rows())?;
    let covered = covered.as_boolean_opt().ok_or_else(|| {
        DataFusionError::Internal("the covered-row predicate is not boolean".into())
    })?;
    // `prep_null_mask_filter` assumes nulls are present; with the `IS NOT NULL`
    // guards they are not, and the mask is used as is.
    let covered = if covered.null_count() > 0 {
        prep_null_mask_filter(covered)
    } else {
        covered.clone()
    };
    let keep = not(&covered)?;
    Ok(filter_record_batch(batch, &keep)?)
}

/// A filtered copy of `snapshot` for `project`: every batch without its covered
/// rows, other projects' partitions left out, and the partitions of days before
/// the project's retention watermark dropped entirely (spec 03 §8). Batches no
/// deletion can cover are kept as they are, sharing their `Arc`s and partials
/// cache.
pub(crate) fn project_snapshot(
    table: Table,
    snapshot: &MemSnapshot,
    project: ProjectId,
    keep_from: Option<NaiveDate>,
    deletions: &[Arc<PendingDeletion>],
) -> Result<Arc<MemSnapshot>, QueryError> {
    let schema = table.schema();
    let file_table = FileTable::from(table);
    let mut partitions: BTreeMap<PartitionKey, Vec<MemBatch>> = BTreeMap::new();
    for (key, batches) in &snapshot.partitions {
        if key.project != project {
            continue;
        }
        if keep_from.is_some_and(|keep| key.day < keep) {
            continue;
        }
        let mut kept = Vec::with_capacity(batches.len());
        for batch in batches {
            let covering = applicable(deletions, None, batch.min_ingest_time);
            if covering.is_empty() {
                kept.push(batch.clone());
                continue;
            }
            let Some(predicate) = covered_predicate(file_table, &schema, &covering)
                .map_err(QueryError::DataFusion)?
            else {
                kept.push(batch.clone());
                continue;
            };
            let filtered =
                drop_covered(&batch.batch, &predicate).map_err(QueryError::DataFusion)?;
            if filtered.num_rows() == 0 {
                continue;
            }
            let rebuilt = MemBatch::from_batch(table, filtered).map_err(|error| {
                QueryError::DataFusion(DataFusionError::External(Box::new(error)))
            })?;
            kept.push(rebuilt);
        }
        if !kept.is_empty() {
            partitions.insert(*key, kept);
        }
    }
    Ok(Arc::new(MemSnapshot { partitions }))
}

/// `col IS NOT NULL AND col IN (values)`, with the values already sorted.
fn in_term<T, F>(
    schema: &SchemaRef,
    column: &str,
    literal: F,
    values: &[T],
) -> Result<Arc<dyn PhysicalExpr>, DataFusionError>
where
    F: Fn(&T) -> ScalarValue,
{
    let expr =
        Arc::new(Column::new(column, column_index(schema, column)?)) as Arc<dyn PhysicalExpr>;
    let list: Vec<Arc<dyn PhysicalExpr>> = values
        .iter()
        .map(|value| Arc::new(Literal::new(literal(value))) as Arc<dyn PhysicalExpr>)
        .collect();
    let membership = in_list(Arc::clone(&expr), list, &false, schema.as_ref())?;
    let guard = Arc::new(IsNotNullExpr::new(Arc::clone(&expr)));
    Ok(Arc::new(BinaryExpr::new(guard, Operator::And, membership)))
}

/// [`in_term`] on a `Utf8` column.
fn utf8_term(
    schema: &SchemaRef,
    column: &str,
    values: &[&String],
) -> Result<Arc<dyn PhysicalExpr>, DataFusionError> {
    in_term(
        schema,
        column,
        |value| ScalarValue::Utf8(Some((*value).to_owned())),
        values,
    )
}

fn or_together(terms: Vec<Arc<dyn PhysicalExpr>>) -> Option<Arc<dyn PhysicalExpr>> {
    let mut terms = terms.into_iter();
    let first = terms.next()?;
    Some(terms.fold(first, |acc, term| {
        Arc::new(BinaryExpr::new(acc, Operator::Or, term))
    }))
}

fn column_index(schema: &SchemaRef, name: &str) -> Result<usize, DataFusionError> {
    schema.index_of(name).map_err(|_| {
        // A caller bug: the caller must project the columns the rule reads.
        DataFusionError::Plan(format!("{name} is missing from the scan schema"))
    })
}

/// The timezone of a timestamp column, kept so literals compare equal to it.
fn timestamp_timezone(schema: &SchemaRef, name: &str) -> Result<Option<Arc<str>>, DataFusionError> {
    match schema.field(column_index(schema, name)?).data_type() {
        DataType::Timestamp(_, timezone) => Ok(timezone.clone()),
        other => Err(DataFusionError::Plan(format!(
            "{name} is {other}, not a timestamp"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use arrow::array::Array;
    use llmobs_core::schema::{scores_schema, spans_schema, traces_schema};
    use llmobs_core::score::ScoreTarget;

    use super::*;
    use crate::test_support::{score_row, score_rows, span_row, span_rows, trace_batch};

    fn project(which: u8) -> ProjectId {
        format!("018f6b4e-7c1a-7c1a-8c1a-0000000000{which:02x}")
            .parse()
            .unwrap()
    }

    fn pending(project: ProjectId, seq: u64) -> PendingDeletion {
        PendingDeletion {
            id: Uuid::from_u128(u128::from(seq)),
            seq,
            project,
            cutoff: 1,
            registered_at: 2,
            trace_ids: HashSet::new(),
            users: HashSet::new(),
            sessions: HashSet::new(),
        }
    }

    /// A deletion of the first project, `seq` 1, with an explicit scope.
    fn scoped(
        traces: &[u16],
        users: &[&str],
        sessions: &[&str],
        cutoff: i64,
    ) -> Arc<PendingDeletion> {
        let trace_ids = traces
            .iter()
            .map(|trace| *span_row(*trace, 0, 0, 0).trace_id.as_bytes())
            .collect();
        Arc::new(PendingDeletion {
            id: Uuid::from_u128(1),
            seq: 1,
            project: project(1),
            cutoff,
            registered_at: 2,
            trace_ids,
            users: users.iter().map(|user| (*user).to_owned()).collect(),
            sessions: sessions
                .iter()
                .map(|session| (*session).to_owned())
                .collect(),
        })
    }

    /// The predicate's value for every row of `batch`: `None` is a null.
    fn values_of(predicate: &Arc<dyn PhysicalExpr>, batch: &RecordBatch) -> Vec<Option<bool>> {
        let array = predicate
            .evaluate(batch)
            .unwrap()
            .into_array(batch.num_rows())
            .unwrap();
        let array = array.as_boolean_opt().unwrap();
        (0..array.len())
            .map(|row| {
                if array.is_null(row) {
                    None
                } else {
                    Some(array.value(row))
                }
            })
            .collect()
    }

    #[test]
    fn with_keeps_seq_order() {
        let one = project(1);
        let tombstones = Tombstones::default()
            .with(pending(one, 3))
            .with(pending(one, 1))
            .with(pending(one, 2));
        let seqs: Vec<_> = tombstones
            .for_project(one)
            .iter()
            .map(|deletion| deletion.seq)
            .collect();
        assert_eq!(seqs, vec![1, 2, 3]);
        assert_eq!(tombstones.known_seq(), 3);
        let all: Vec<_> = tombstones
            .all()
            .iter()
            .map(|deletion| deletion.seq)
            .collect();
        assert_eq!(all, vec![1, 2, 3]);
    }

    #[test]
    fn without_removes_and_drops_an_emptied_project() {
        let one = project(1);
        let two = project(2);
        let first = pending(one, 1);
        let id = first.id;
        let tombstones = Tombstones::default()
            .with(first)
            .with(pending(two, 2))
            .without(one, id);
        assert!(tombstones.for_project(one).is_empty());
        assert!(!tombstones.by_project.contains_key(&one));
        assert_eq!(tombstones.for_project(two).len(), 1);
        assert_eq!(tombstones.known_seq(), 2);
    }

    #[test]
    fn for_project_of_an_unknown_project_is_empty() {
        let tombstones = Tombstones::default().with(pending(project(1), 1));
        assert!(tombstones.for_project(project(2)).is_empty());
    }

    #[test]
    fn a_traces_deletion_covers_a_trace_up_to_the_cutoff() {
        let cutoff = 1_000;
        let batch = span_rows(&[
            span_row(1, 0, 10, cutoff),
            span_row(1, 1, 20, cutoff + 1),
            span_row(2, 0, 30, cutoff),
        ]);
        let deletion = scoped(&[1], &[], &[], cutoff);
        let predicate = covered_predicate(
            FileTable::Spans,
            &spans_schema(),
            std::slice::from_ref(&deletion),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            values_of(&predicate, &batch),
            vec![Some(true), Some(false), Some(false)]
        );
    }

    #[test]
    fn a_user_deletion_covers_the_user_but_not_a_null_user_id() {
        let mut covered = span_row(1, 0, 10, 1);
        covered.user_id = Some("u".to_owned());
        let plain = span_row(2, 0, 20, 1);
        let batch = span_rows(&[covered, plain]);
        let deletion = scoped(&[], &["u"], &[], 1_000);
        let predicate = covered_predicate(
            FileTable::Spans,
            &spans_schema(),
            std::slice::from_ref(&deletion),
        )
        .unwrap()
        .unwrap();
        // Trace 1 carries the user id, trace 2 does not. The guard keeps the
        // expression false, never null, on the null user id.
        assert_eq!(values_of(&predicate, &batch), vec![Some(true), Some(false)]);
    }

    #[test]
    fn session_ids_cover_scores_but_not_spans() {
        let mut covered = span_row(1, 0, 10, 1);
        covered.session_id = Some("s".to_owned());
        let mut other = span_row(2, 0, 20, 1);
        other.session_id = Some("s".to_owned());
        let spans_batch = span_rows(&[covered, other]);
        let mut score = score_row(0x11, 0, 1, 1, 1, false, "sess");
        score.target = ScoreTarget::Session {
            session_id: "s".to_owned(),
        };
        let scores_batch = score_rows(&[score]);
        let deletion = scoped(&[], &["someone-else"], &["s"], 1_000);

        let on_scores = covered_predicate(
            FileTable::Scores,
            &scores_schema(),
            std::slice::from_ref(&deletion),
        )
        .unwrap()
        .unwrap();
        assert_eq!(values_of(&on_scores, &scores_batch), vec![Some(true)]);

        // `session_id` matches `scores` only: both spans stay.
        let on_spans = covered_predicate(
            FileTable::Spans,
            &spans_schema(),
            std::slice::from_ref(&deletion),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            values_of(&on_spans, &spans_batch),
            vec![Some(false), Some(false)]
        );
    }

    #[test]
    fn a_row_covered_by_either_of_two_deletions_is_covered() {
        let batch = span_rows(&[
            span_row(1, 0, 10, 1),
            span_row(2, 0, 20, 1),
            span_row(3, 0, 30, 1),
        ]);
        let deletions = [scoped(&[1], &[], &[], 1_000), scoped(&[2], &[], &[], 1_000)];
        let predicate = covered_predicate(FileTable::Spans, &spans_schema(), &deletions)
            .unwrap()
            .unwrap();
        assert_eq!(
            values_of(&predicate, &batch),
            vec![Some(true), Some(true), Some(false)]
        );
    }

    #[test]
    fn empty_deletions_build_no_predicate_and_traces_tests_only_the_trace_id() {
        assert!(
            covered_predicate(FileTable::Spans, &spans_schema(), &[])
                .unwrap()
                .is_none()
        );
        // A partial carries no per-row ingest time: a row ingested after the
        // cut-off is still covered, and a user scope covers no partial at all.
        let traces = trace_batch(&[span_row(1, 0, 10, 1_000)]);
        let by_trace = scoped(&[1], &[], &[], 1);
        let predicate = covered_predicate(FileTable::Traces, &traces_schema(), &[by_trace])
            .unwrap()
            .unwrap();
        assert_eq!(values_of(&predicate, &traces), vec![Some(true)]);
        let by_user = scoped(&[], &["u"], &["s"], 1_000);
        assert!(
            covered_predicate(FileTable::Traces, &traces_schema(), &[by_user])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_session_score_is_not_covered_by_a_traces_deletion() {
        let mut score = score_row(0x11, 0, 1, 1, 1, false, "sess");
        score.target = ScoreTarget::Session {
            session_id: "s".to_owned(),
        };
        let batch = score_rows(&[score]);
        let deletion = scoped(&[1], &[], &[], 1_000);
        let predicate = covered_predicate(
            FileTable::Scores,
            &scores_schema(),
            std::slice::from_ref(&deletion),
        )
        .unwrap()
        .unwrap();
        // A null `trace_id` makes the term false, not null.
        assert_eq!(values_of(&predicate, &batch), vec![Some(false)]);
    }

    #[test]
    fn drop_covered_keeps_the_uncovered_rows() {
        let batch = span_rows(&[
            span_row(1, 0, 10, 1),
            span_row(2, 0, 20, 1),
            span_row(3, 0, 30, 1_000),
        ]);
        let deletion = scoped(&[1, 2], &[], &[], 1);
        let predicate = covered_predicate(
            FileTable::Spans,
            &spans_schema(),
            std::slice::from_ref(&deletion),
        )
        .unwrap()
        .unwrap();
        let kept = drop_covered(&batch, &predicate).unwrap();
        assert_eq!(kept.num_rows(), 1);
        let trace = kept
            .column_by_name(spans::TRACE_ID)
            .unwrap()
            .as_any()
            .downcast_ref::<arrow::array::FixedSizeBinaryArray>()
            .unwrap();
        assert_eq!(trace.value(0), span_row(3, 0, 0, 0).trace_id.as_bytes());
    }
}
