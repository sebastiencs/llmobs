//! The deletion pass: rewrite the files that hold covered rows, raise the
//! clean ones, then mark the deletions applied (spec 03 §7).
//!
//! The pass runs after each compaction pass and is also exposed as
//! [`crate::Storage::apply_deletions`] for tests. Compaction and the rewriter
//! never run on the same file: both claim their files first.

use std::sync::{Arc, PoisonError};

use arrow::datatypes::SchemaRef;
use chrono::NaiveDate;
use datafusion::datasource::physical_plan::ParquetSource;
use datafusion::execution::TaskContext;
use datafusion::execution::context::SessionConfig;
use datafusion::physical_expr::PhysicalExpr;
use datafusion::physical_plan::execute_stream;
use datafusion::physical_plan::filter::FilterExec;
use datafusion::physical_plan::limit::GlobalLimitExec;
use futures::StreamExt;
use llmobs_core::ids::ProjectId;
use llmobs_core::schema::{scores_schema, spans_schema};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::catalog::{CatalogEdit, FileId, FileMeta, FileTable};
use crate::compaction::job::{ClaimGuard, Compactor};
use crate::compaction::merge::{MergeContext, file_scan, ordering_of, read_error};
use crate::compaction::plan::{Job, JobKind, Policy};
use crate::deletion::{PendingDeletion, Tombstones, applicable, covered_predicate, key_columns};
use crate::error::{CommitError, CompactionError};
use crate::memtable::MemSnapshot;
use crate::read_state::{ReadState, publish_catalog, publish_tombstones};

/// What one deletion pass did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeletionReport {
    /// Files rewritten without their covered rows.
    pub rewritten: u64,
    /// Files checked and found clean, whose `deletions_applied_seq` was raised.
    pub raised: u64,
    /// Deletions marked applied.
    pub applied: u64,
    /// Rewrites or raise commits that failed or lost a conflict; a later pass
    /// redoes them.
    pub failed: u64,
    /// Projects with a deletion applied in this pass, in order, without repeats.
    pub applied_projects: Vec<ProjectId>,
}

/// What one candidate came to.
enum Checked {
    /// The file was rewritten without its covered rows.
    Rewritten,
    /// The file holds no covered row: its id and the `deletions_applied_seq`
    /// the check saw, for the raise.
    Clean { id: FileId, applied: u64 },
    /// The rewrite failed or lost a conflict; logged where it ran.
    Failed,
}

impl Compactor {
    /// One deletion pass at `now` (spec 03 §7): check the candidate files,
    /// rewrite the covered ones, raise the clean ones, then mark every
    /// deletion applied whose rows are gone.
    pub(crate) async fn deletion_pass(
        self: &Arc<Self>,
        now: i64,
    ) -> Result<DeletionReport, CompactionError> {
        // 1. The tombstones as last published.
        let tombstones = Arc::clone(&self.published.load().tombstones);
        let pending = tombstones.all();
        if pending.is_empty() {
            return Ok(DeletionReport::default());
        }
        let known = tombstones.known_seq();
        let policy = *self.policy.lock().unwrap_or_else(PoisonError::into_inner);
        let mut report = DeletionReport::default();

        // 2. One warning per deletion past `storage.deletion.max_delay`.
        {
            let mut overdue = self
                .overdue_logged
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            for deletion in &pending {
                if deletion.registered_at.saturating_add(self.max_delay) < now
                    && overdue.insert(deletion.id)
                {
                    // Never log the scope: its ids name the person the
                    // deletion is about (spec 03 §7).
                    tracing::warn!(
                        deletion_id = %deletion.id,
                        project = %deletion.project,
                        seq = deletion.seq,
                        "deletion is past storage.deletion.max_delay"
                    );
                }
            }
        }

        // 3. Candidates: the active `spans` and `scores` files a pending
        // deletion may cover, per project in id order.
        let catalog = self.committer.catalog().await;
        let mut projects: Vec<ProjectId> =
            pending.iter().map(|deletion| deletion.project).collect();
        projects.sort();
        projects.dedup();
        let mut candidates: Vec<Arc<FileMeta>> = Vec::new();
        for project in projects {
            let deletions = tombstones.for_project(project);
            for table in [FileTable::Spans, FileTable::Scores] {
                for file in catalog.files(table, project, NaiveDate::MIN..=NaiveDate::MAX) {
                    let covering = applicable(
                        &deletions,
                        Some(file.deletions_applied_seq),
                        file.min_ingest_time,
                    );
                    if !covering.is_empty() {
                        candidates.push(Arc::clone(file));
                    }
                }
            }
        }

        // 4. Claim: a file a running job holds is left for a later pass.
        let mut claimed: Vec<(Arc<FileMeta>, ClaimGuard)> = Vec::new();
        {
            let mut claims = self.claims.lock().unwrap_or_else(PoisonError::into_inner);
            for file in candidates {
                if claims.contains(&file.id) {
                    continue;
                }
                claims.insert(file.id);
                claimed.push((
                    Arc::clone(&file),
                    ClaimGuard {
                        compactor: Arc::clone(self),
                        ids: vec![file.id],
                    },
                ));
            }
        }

        // 5. Check and rewrite, at most `threads` at a time. Each guard moves
        // into its task; the claim is released when the task ends.
        let semaphore = Arc::new(Semaphore::new(self.threads.max(1)));
        let mut set: JoinSet<Result<Checked, CompactionError>> = JoinSet::new();
        for (file, guard) in claimed {
            let permit = match Arc::clone(&semaphore).acquire_owned().await {
                Ok(permit) => permit,
                // The semaphore is never closed.
                Err(_) => unreachable!("the compaction semaphore is never closed"),
            };
            let compactor = Arc::clone(self);
            let tombstones = Arc::clone(&tombstones);
            set.spawn(async move {
                let _permit = permit;
                let _guard = guard;
                check_and_rewrite(&compactor, &tombstones, &file, policy).await
            });
        }
        let mut clean: Vec<(FileId, u64)> = Vec::new();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(Ok(Checked::Rewritten)) => report.rewritten += 1,
                Ok(Ok(Checked::Clean { id, applied })) => clean.push((id, applied)),
                Ok(Ok(Checked::Failed)) => report.failed += 1,
                Ok(Err(error)) => return Err(error),
                Err(_) => {
                    report.failed += 1;
                    tracing::warn!("deletion task failed");
                }
            }
        }

        // 6. Raise every clean file to the known sequence in one edit.
        let raise: Vec<(FileId, u64)> = clean
            .into_iter()
            .filter(|(_, applied)| *applied < known)
            .map(|(id, _)| (id, known))
            .collect();
        if !raise.is_empty() {
            let raised = u64::try_from(raise.len()).unwrap_or(u64::MAX);
            let edit = CatalogEdit {
                add: Vec::new(),
                remove: Vec::new(),
                flushed_seq: None,
                sources: Vec::new(),
                raise,
            };
            let published = Arc::clone(&self.published);
            match self
                .committer
                .commit(&edit, move |catalog| {
                    publish_catalog(&published, catalog);
                })
                .await
            {
                Ok(_) => report.raised += raised,
                Err(error) => {
                    // A conflict means the file was compacted or rewritten
                    // meanwhile; the next pass checks it again.
                    report.failed += 1;
                    if matches!(error, CommitError::Conflict { .. }) {
                        tracing::info!(
                            %error,
                            "raising deletions_applied_seq lost a conflict; a later pass redoes it"
                        );
                    } else {
                        tracing::warn!(%error, "raising deletions_applied_seq failed");
                    }
                }
            }
        }

        // 7. Every claim is released: the guards moved into the tasks, and the
        // tasks have all ended.

        // 8. Applied. One published snapshot holds the catalog and both
        // memtables: a flush that commits between two reads could otherwise
        // drop its batch from the newer state while the older catalog still
        // lacks the file, and the deletion would be marked applied with its
        // rows still in that file (spec 03 §7).
        let state = self.published.load();
        for deletion in &pending {
            if !applied_in_state(&state, deletion) {
                continue;
            }
            let published = Arc::clone(&self.published);
            match self
                .committer
                .mark_applied(deletion.project, deletion.id, now, move |tombstones| {
                    publish_tombstones(&published, tombstones);
                })
                .await
            {
                Ok(true) => {
                    report.applied += 1;
                    if !report.applied_projects.contains(&deletion.project) {
                        report.applied_projects.push(deletion.project);
                    }
                    tracing::info!(
                        deletion_id = %deletion.id,
                        project = %deletion.project,
                        seq = deletion.seq,
                        "deletion applied"
                    );
                    self.overdue_logged
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&deletion.id);
                }
                Ok(false) => {}
                Err(_) => {
                    // The catalog error text holds no scope value, but keep
                    // the line short.
                    report.failed += 1;
                    tracing::warn!(
                        deletion_id = %deletion.id,
                        "marking a deletion applied failed"
                    );
                }
            }
        }

        // 9. The report.
        Ok(report)
    }
}

/// Step 5 for one claimed candidate: check it for a covered row, then rewrite
/// it or hand it back for the raise.
async fn check_and_rewrite(
    compactor: &Arc<Compactor>,
    tombstones: &Arc<Tombstones>,
    file: &Arc<FileMeta>,
    policy: Policy,
) -> Result<Checked, CompactionError> {
    let schema = table_schema(file.table)?;
    let deletions = applicable(
        &tombstones.for_project(file.project),
        Some(file.deletions_applied_seq),
        file.min_ingest_time,
    );
    // Not `None`: the list is not empty and every pending deletion has at
    // least one term for `spans` and `scores`. Treated as clean regardless.
    let Some(covered) = covered_predicate(file.table, &schema, &deletions).map_err(|_| {
        CompactionError::Deletion {
            reason: "building the deletion filter",
        }
    })?
    else {
        return Ok(Checked::Clean {
            id: file.id,
            applied: file.deletions_applied_seq,
        });
    };
    if !holds_covered(&compactor.merge, file, &schema, &covered, &deletions).await? {
        return Ok(Checked::Clean {
            id: file.id,
            applied: file.deletions_applied_seq,
        });
    }
    let job = Job {
        table: file.table,
        project: file.project,
        day: file.day,
        kind: JobKind::Rewrite,
        inputs: vec![Arc::clone(file)],
        whole_partition: false,
        drop_deleted: false,
    };
    let table = file.table.as_str();
    let project = file.project.to_string();
    let day = file.day.to_string();
    match compactor.run(job, policy).await {
        Ok(_) => Ok(Checked::Rewritten),
        Err(error) => {
            // Logged like a failed compaction job: the label only, since the
            // error may quote the covered-row predicate's literals.
            if matches!(error, CompactionError::Commit(CommitError::Conflict { .. })) {
                tracing::info!(
                    table,
                    project,
                    day,
                    kind = "rewrite",
                    inputs = 1,
                    %error,
                    "compaction job lost a conflict; a later pass redoes it"
                );
            } else {
                tracing::warn!(
                    table,
                    project,
                    day,
                    kind = "rewrite",
                    inputs = 1,
                    error = error.label(),
                    "compaction job failed"
                );
            }
            Ok(Checked::Failed)
        }
    }
}

/// Does `file` hold a row covered by `deletions`? A `LIMIT 1` scan of the key
/// columns with the covered-row predicate pushed down (spec 03 §7):
/// statistics prune row groups on `ingest_time`, the bloom filters on
/// `trace_id` (plus `user_id` and `session_id` for `spans`).
async fn holds_covered(
    ctx: &MergeContext,
    file: &Arc<FileMeta>,
    schema: &SchemaRef,
    covered_full: &Arc<dyn PhysicalExpr>,
    deletions: &[Arc<PendingDeletion>],
) -> Result<bool, CompactionError> {
    let keys = key_columns(file.table);
    let mut key_indices = Vec::with_capacity(keys.len());
    for name in keys {
        key_indices.push(
            schema
                .index_of(name)
                .map_err(|_| CompactionError::Deletion {
                    reason: "building the deletion filter",
                })?,
        );
    }
    let projected =
        Arc::new(
            schema
                .project(&key_indices)
                .map_err(|_| CompactionError::Deletion {
                    reason: "building the deletion filter",
                })?,
        );
    let Some(covered_projected) =
        covered_predicate(file.table, &projected, deletions).map_err(|_| {
            CompactionError::Deletion {
                reason: "building the deletion filter",
            }
        })?
    else {
        return Ok(false);
    };
    let ordering = ordering_of(file.table, schema)?;
    let source = ParquetSource::new(Arc::clone(schema))
        .with_pushdown_filters(true)
        .with_predicate(Arc::clone(covered_full));
    let scan = file_scan(&[file], source, &ordering, Some(key_indices))?;
    let filter = FilterExec::try_new(Arc::clone(&covered_projected), scan)
        .map_err(|source| CompactionError::Read { source })?;
    let limit = GlobalLimitExec::new(Arc::new(filter), 0, Some(1));
    let task = Arc::new(
        TaskContext::default()
            .with_session_config(SessionConfig::new().with_batch_size(ctx.batch_size))
            .with_runtime(Arc::clone(&ctx.runtime)),
    );
    let mut stream = execute_stream(Arc::new(limit), task).map_err(read_error)?;
    while let Some(item) = stream.next().await {
        if item.map_err(read_error)?.num_rows() > 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The stored schema of a rewritten table.
fn table_schema(table: FileTable) -> Result<SchemaRef, CompactionError> {
    match table {
        FileTable::Spans => Ok(spans_schema()),
        FileTable::Scores => Ok(scores_schema()),
        FileTable::Traces => Err(CompactionError::Input {
            reason: "traces are not rewritten",
        }),
    }
}

/// Step 8's test for one deletion, against one published snapshot: every
/// `spans` and `scores` file of its project that could hold a covered row is
/// clean of it, and no memtable batch could hold one (spec 03 §7).
fn applied_in_state(state: &ReadState, deletion: &PendingDeletion) -> bool {
    for table in [FileTable::Spans, FileTable::Scores] {
        let clean = state
            .catalog
            .files(table, deletion.project, NaiveDate::MIN..=NaiveDate::MAX)
            .filter(|file| file.min_ingest_time <= deletion.cutoff)
            .all(|file| file.deletions_applied_seq >= deletion.seq);
        if !clean {
            return false;
        }
    }
    !batch_in_scope(&state.spans, deletion) && !batch_in_scope(&state.scores, deletion)
}

/// Does the snapshot hold a batch of the project with `min_ingest_time` at or
/// before the cut-off? A batch that held covered rows is dropped by the next
/// flush, which writes none of them (spec 03 §7).
fn batch_in_scope(snapshot: &MemSnapshot, deletion: &PendingDeletion) -> bool {
    snapshot
        .partitions
        .iter()
        .filter(|(key, _)| key.project == deletion.project)
        .flat_map(|(_, batches)| batches)
        .any(|batch| batch.min_ingest_time <= deletion.cutoff)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, PoisonError};
    use std::time::{Duration, Instant};

    use arrow::array::{Array, RecordBatch};
    use llmobs_core::score::ScoreTarget;
    use uuid::Uuid;

    use crate::QueryOptions;
    use crate::Storage;
    use crate::catalog::{FileMeta, FileTable};
    use crate::compaction::job::tests::{
        at, config_in, flush_cycle, flush_scores, project, quiet_compaction, score_file_rows,
        wait_at,
    };
    use crate::deletion::{Deletion, DeletionScope, DeletionStatus};
    use crate::hooks::HookPoint;
    use crate::test_support::{rows, score_row, score_rows, span_row, span_rows};
    use crate::views::QueryContext;
    use crate::wal::Table;

    /// One `traces` deletion of the given traces, cut off at `cutoff`.
    fn traces_deletion(traces: &[u16], cutoff: i64) -> Deletion {
        Deletion {
            id: Uuid::now_v7(),
            project: project(),
            cutoff,
            scope: DeletionScope::Traces {
                trace_ids: traces
                    .iter()
                    .map(|trace| span_row(*trace, 0, 0, 0).trace_id)
                    .collect(),
            },
        }
    }

    fn trace_bytes(trace: u16) -> [u8; 16] {
        *span_row(trace, 0, 0, 0).trace_id.as_bytes()
    }

    /// The raw 16-byte trace ids stored in one Parquet object.
    fn object_trace_ids(dir: &Path, file: &FileMeta, column: &str) -> Vec<[u8; 16]> {
        let bytes = std::fs::read(dir.join("objects").join(&file.path)).unwrap();
        let builder = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
            bytes::Bytes::from(bytes),
        )
        .unwrap();
        let schema = Arc::clone(builder.schema());
        let batches: Vec<RecordBatch> = builder
            .build()
            .unwrap()
            .map(|batch| batch.unwrap())
            .collect();
        let all = arrow::compute::concat_batches(&schema, &batches).unwrap();
        let array = all
            .column_by_name(column)
            .unwrap()
            .as_any()
            .downcast_ref::<arrow::array::FixedSizeBinaryArray>()
            .unwrap();
        (0..array.len())
            .map(|row| <[u8; 16]>::try_from(array.value(row)).unwrap())
            .collect()
    }

    fn spans_trace_ids(dir: &Path, file: &FileMeta) -> Vec<[u8; 16]> {
        object_trace_ids(dir, file, llmobs_core::schema::spans::TRACE_ID)
    }

    fn traces_trace_ids(dir: &Path, file: &FileMeta) -> Vec<[u8; 16]> {
        object_trace_ids(dir, file, llmobs_core::schema::traces::TRACE_ID)
    }

    /// The first `i64` of the first row of `sql`.
    async fn scalar(storage: &Storage, sql: &str) -> i64 {
        let query = storage.query(project(), QueryOptions::default()).unwrap();
        let ctx = QueryContext::new(query, None).unwrap();
        let batches = rows(&ctx, sql).await;
        let column = batches[0]
            .column(0)
            .as_any()
            .downcast_ref::<arrow::array::Int64Array>()
            .unwrap();
        column.value(0)
    }

    async fn span_count(storage: &Storage, trace: u16) -> i64 {
        let hex = span_row(trace, 0, 0, 0).trace_id.to_hex();
        scalar(
            storage,
            &format!("SELECT count(*) FROM spans WHERE trace_id = '{hex}'"),
        )
        .await
    }

    /// Every string cell of `sql`'s result, one per line (EXPLAIN plans).
    async fn text(storage: &Storage, sql: &str) -> String {
        let query = storage.query(project(), QueryOptions::default()).unwrap();
        let ctx = QueryContext::new(query, None).unwrap();
        let batches = rows(&ctx, sql).await;
        let mut text = String::new();
        for batch in &batches {
            for column in batch.columns() {
                let Some(values) = column.as_any().downcast_ref::<arrow::array::StringArray>()
                else {
                    continue;
                };
                for index in 0..values.len() {
                    text.push_str(values.value(index));
                    text.push('\n');
                }
            }
        }
        text
    }

    /// `EXPLAIN` lines whose predicate names `ingest_time`: the tombstone
    /// filter, absent once the tombstones are empty.
    fn ingest_time_filters(plan: &str) -> usize {
        plan.lines()
            .filter(|line| {
                (line.contains("FilterExec") || line.contains("predicate="))
                    && line.contains("ingest_time")
            })
            .count()
    }

    /// The deletion's status, unwrapped.
    async fn status(storage: &Storage, deletion: &Deletion) -> DeletionStatus {
        storage.deletion_status(deletion.id).await.unwrap()
    }

    /// The ids the pass has warned about as overdue, copied out.
    fn overdue_logged(storage: &Storage) -> std::collections::HashSet<Uuid> {
        storage
            .compactor
            .overdue_logged
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    // 1. Compaction drops covered rows.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn compaction_drops_covered_rows() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(3), Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        for index in 0..3_u8 {
            let base = at(10, 0, 0) + i64::from(index) * 60 * 1_000_000_000;
            storage = flush_cycle(
                &config,
                storage,
                span_rows(&[
                    span_row(1, 10 + index, base, base + 1_000_000_000),
                    span_row(2, 20 + index, base, base + 2_000_000_000),
                ]),
            )
            .await;
        }
        assert_eq!(storage.catalog_files(Table::Spans).len(), 3);
        let deletion = traces_deletion(&[1], at(11, 0, 0));
        storage.register_deletion(deletion.clone()).await.unwrap();

        let report = storage.compact(at(11, 0, 0)).await.unwrap();
        assert_eq!((report.jobs, report.failed), (1, 0), "{report:?}");
        let files = storage.catalog_files(Table::Spans);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].level, 1);
        assert_eq!(files[0].deletions_applied_seq, 1);
        let stored = spans_trace_ids(dir.path(), &files[0]);
        assert_eq!(stored, vec![trace_bytes(2); 3], "trace 2 only, three spans");
        let traces_files = storage.catalog_files(FileTable::Traces);
        assert_eq!(traces_files.len(), 1);
        assert_eq!(traces_files[0].deletions_applied_seq, 1);

        let report = storage.apply_deletions(at(11, 0, 0)).await.unwrap();
        assert_eq!(
            (report.rewritten, report.applied, report.failed),
            (0, 1, 0),
            "{report:?}"
        );
        assert_eq!(
            status(&storage, &deletion).await,
            DeletionStatus::Applied {
                applied_at: at(11, 0, 0)
            }
        );
        storage.shutdown().await.unwrap();
    }

    // 2. Rewrite of an L0 and an L1 file.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_rewrite_keeps_each_inputs_level_and_fully_merged() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(2), Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        let times = [at(10, 0, 0), at(10, 1, 0)];
        for (index, time) in times.iter().enumerate() {
            storage = flush_cycle(
                &config,
                storage,
                span_rows(&[
                    span_row(1, index as u8, *time, time + 1),
                    span_row(2, index as u8, *time, time + 1),
                ]),
            )
            .await;
        }
        let report = storage.compact(at(11, 0, 0)).await.unwrap();
        assert_eq!((report.jobs, report.failed), (1, 0), "{report:?}");
        let l1 = storage.catalog_files(Table::Spans);
        assert_eq!(l1.len(), 1);
        assert_eq!(l1[0].level, 1);
        assert!(l1[0].fully_merged);

        // A late flush makes an L0 file of the same partition: both levels
        // now hold trace 1.
        let late = at(10, 2, 0);
        storage = flush_cycle(
            &config,
            storage,
            span_rows(&[
                span_row(1, 2, late, late + 1),
                span_row(2, 2, late, late + 1),
            ]),
        )
        .await;
        assert_eq!(storage.catalog_files(Table::Spans).len(), 2);

        let deletion = traces_deletion(&[1], at(12, 0, 0));
        storage.register_deletion(deletion.clone()).await.unwrap();
        let report = storage.apply_deletions(at(12, 0, 0)).await.unwrap();
        assert_eq!(
            (report.rewritten, report.applied, report.failed),
            (2, 1, 0),
            "{report:?}"
        );

        let files = storage.catalog_files(Table::Spans);
        assert_eq!(files.len(), 2);
        let mut levels: Vec<(u8, bool)> = Vec::new();
        for file in &files {
            assert!(!spans_trace_ids(dir.path(), file).contains(&trace_bytes(1)));
            assert_eq!(file.deletions_applied_seq, 1);
            levels.push((file.level, file.fully_merged));
        }
        assert!(levels.contains(&(0, false)), "the L0 rewrite: {levels:?}");
        assert!(levels.contains(&(1, true)), "the L1 rewrite: {levels:?}");

        // The reopen checks the `file_sources` pairs of the new files.
        storage.shutdown().await.unwrap();
        let storage = Storage::open(config).await.unwrap();
        assert_eq!(storage.catalog_files(Table::Spans).len(), 2);
        assert_eq!(storage.catalog_files(FileTable::Traces).len(), 2);
        assert_eq!(
            status(&storage, &deletion).await,
            DeletionStatus::Applied {
                applied_at: at(12, 0, 0)
            }
        );
        let plan = text(&storage, "EXPLAIN SELECT * FROM spans").await;
        assert_eq!(
            ingest_time_filters(&plan),
            0,
            "the published tombstones are empty:\n{plan}"
        );
        storage.shutdown().await.unwrap();
    }

    // 3. A clean candidate is raised, not rewritten.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_clean_candidate_is_raised_without_a_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        let time = at(10, 0, 0);
        storage = flush_cycle(
            &config,
            storage,
            span_rows(&[span_row(2, 0, time, time + 1)]),
        )
        .await;
        let id = storage.catalog_files(Table::Spans)[0].id;

        let deletion = traces_deletion(&[1], at(11, 0, 0));
        storage.register_deletion(deletion.clone()).await.unwrap();
        let report = storage.apply_deletions(at(11, 0, 0)).await.unwrap();
        assert_eq!(
            (
                report.rewritten,
                report.raised,
                report.applied,
                report.failed
            ),
            (0, 1, 1, 0),
            "{report:?}"
        );
        let files = storage.catalog_files(Table::Spans);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].id, id, "the file is unchanged");
        assert_eq!(files[0].deletions_applied_seq, 1);
        storage.shutdown().await.unwrap();
    }

    // 4. Rows only in the memtable wait for the flush.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_memtable_only_deletion_waits_for_the_flush() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
        let storage = Storage::open(config.clone()).await.unwrap();
        let time = at(10, 0, 0);
        storage
            .append_spans(project(), span_rows(&[span_row(1, 0, time, time + 1)]))
            .await
            .unwrap();
        let deletion = traces_deletion(&[1], time + 1);
        storage.register_deletion(deletion.clone()).await.unwrap();

        let report = storage.apply_deletions(at(11, 0, 0)).await.unwrap();
        assert_eq!((report.applied, report.failed), (0, 0), "{report:?}");
        assert_eq!(status(&storage, &deletion).await, DeletionStatus::Pending);

        // The flush drops the covered rows and writes no file (M5.3).
        storage.shutdown().await.unwrap();
        let storage = Storage::open(config).await.unwrap();
        assert!(storage.catalog_files(Table::Spans).is_empty());
        let report = storage.apply_deletions(at(11, 0, 0)).await.unwrap();
        assert_eq!((report.applied, report.failed), (1, 0), "{report:?}");
        storage.shutdown().await.unwrap();
    }

    // 5. A file whose rows are all covered disappears.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_fully_covered_file_is_removed_by_the_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        let time = at(10, 0, 0);
        storage = flush_cycle(
            &config,
            storage,
            span_rows(&[
                span_row(1, 0, time, time + 1),
                span_row(1, 1, time, time + 1),
            ]),
        )
        .await;
        assert_eq!(storage.catalog_files(Table::Spans).len(), 1);

        let deletion = traces_deletion(&[1], at(11, 0, 0));
        storage.register_deletion(deletion.clone()).await.unwrap();
        let report = storage.apply_deletions(at(11, 0, 0)).await.unwrap();
        assert_eq!(
            (report.rewritten, report.applied, report.failed),
            (1, 1, 0),
            "{report:?}"
        );
        assert!(storage.catalog_files(Table::Spans).is_empty());
        assert!(storage.catalog_files(FileTable::Traces).is_empty());
        assert_eq!(
            scalar(&storage, "SELECT count(*) FROM spans").await,
            0,
            "the view is empty"
        );
        storage.shutdown().await.unwrap();
    }

    // 6. The milestone race: a compaction started before a deletion commits
    //    after it, and the rows stay gone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_compaction_started_before_a_deletion_is_rewritten_afterwards() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(2), Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        for index in 0..2_u8 {
            let base = at(10, 0, 0) + i64::from(index) * 60 * 1_000_000_000;
            storage = flush_cycle(
                &config,
                storage,
                span_rows(&[
                    span_row(1, 10 + index, base, base + 1_000_000_000),
                    span_row(2, 20 + index, base, base + 2_000_000_000),
                ]),
            )
            .await;
        }
        let storage = Arc::new(storage);
        let hooks = storage.hooks().clone();
        hooks.hold(HookPoint::CompactionAfterWrite);
        let racing = {
            let storage = Arc::clone(&storage);
            tokio::spawn(async move { storage.compact(at(11, 0, 0)).await })
        };
        wait_at(&storage, HookPoint::CompactionAfterWrite);

        // Registered while the job is parked: its outputs record the older
        // known sequence, so they stay candidates for the rewriter.
        let deletion = traces_deletion(&[1], at(11, 0, 0));
        storage.register_deletion(deletion.clone()).await.unwrap();
        hooks.release(HookPoint::CompactionAfterWrite);
        racing.await.unwrap().unwrap();

        let files = storage.catalog_files(Table::Spans);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].deletions_applied_seq, 0);
        assert!(
            spans_trace_ids(dir.path(), &files[0]).contains(&trace_bytes(1)),
            "the L1 output still holds trace 1"
        );
        assert_eq!(span_count(&storage, 1).await, 0, "the query hides it");
        assert_eq!(span_count(&storage, 2).await, 2);

        let report = storage.apply_deletions(at(11, 0, 0)).await.unwrap();
        assert_eq!(
            (report.rewritten, report.applied, report.failed),
            (1, 1, 0),
            "{report:?}"
        );
        for file in storage.catalog_files(Table::Spans) {
            assert!(!spans_trace_ids(dir.path(), &file).contains(&trace_bytes(1)));
        }
        for file in storage.catalog_files(FileTable::Traces) {
            assert!(!traces_trace_ids(dir.path(), &file).contains(&trace_bytes(1)));
        }
        assert_eq!(
            status(&storage, &deletion).await,
            DeletionStatus::Applied {
                applied_at: at(11, 0, 0)
            }
        );
        Arc::try_unwrap(storage)
            .unwrap_or_else(|_| panic!("storage still shared"))
            .shutdown()
            .await
            .unwrap();
    }

    // 7. A file claimed by a running job is left alone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_deletion_pass_leaves_claimed_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(2), Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        for index in 0..2_u8 {
            let base = at(10, 0, 0) + i64::from(index) * 60 * 1_000_000_000;
            storage = flush_cycle(
                &config,
                storage,
                span_rows(&[
                    span_row(1, 10 + index, base, base + 1_000_000_000),
                    span_row(2, 20 + index, base, base + 2_000_000_000),
                ]),
            )
            .await;
        }
        let deletion = traces_deletion(&[1], at(11, 0, 0));
        storage.register_deletion(deletion.clone()).await.unwrap();

        let storage = Arc::new(storage);
        let hooks = storage.hooks().clone();
        hooks.hold(HookPoint::CompactionAfterStage);
        let compacting = {
            let storage = Arc::clone(&storage);
            tokio::spawn(async move { storage.compact(at(11, 0, 0)).await })
        };
        wait_at(&storage, HookPoint::CompactionAfterStage);

        let report = storage.apply_deletions(at(11, 0, 0)).await.unwrap();
        assert_eq!(
            (
                report.rewritten,
                report.raised,
                report.applied,
                report.failed
            ),
            (0, 0, 0, 0),
            "{report:?}"
        );
        assert_eq!(storage.catalog_files(Table::Spans).len(), 2);
        assert_eq!(status(&storage, &deletion).await, DeletionStatus::Pending);

        hooks.release(HookPoint::CompactionAfterStage);
        compacting.await.unwrap().unwrap();
        // The merge applied the tombstones already; the next pass only marks
        // the deletion applied.
        let report = storage.apply_deletions(at(11, 0, 0)).await.unwrap();
        assert_eq!(
            (report.rewritten, report.applied, report.failed),
            (0, 1, 0),
            "{report:?}"
        );
        let files = storage.catalog_files(Table::Spans);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].deletions_applied_seq, 1);
        assert!(!spans_trace_ids(dir.path(), &files[0]).contains(&trace_bytes(1)));
        Arc::try_unwrap(storage)
            .unwrap_or_else(|_| panic!("storage still shared"))
            .shutdown()
            .await
            .unwrap();
    }

    // 8. A `user` deletion rewrites spans and scores, and keeps another
    //    user's span of the session.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_user_deletion_rewrites_spans_and_scores() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        let time = at(10, 0, 0);
        let mut mine = span_row(1, 0, time, time + 1);
        mine.user_id = Some("u".to_owned());
        let mut shared = span_row(2, 0, time, time + 1);
        shared.user_id = Some("v".to_owned());
        shared.session_id = Some("s".to_owned());
        storage = flush_cycle(&config, storage, span_rows(&[mine, shared])).await;
        let mut session_score = score_row(0x11, 0, time, time, time + 1, false, "session");
        session_score.target = ScoreTarget::Session {
            session_id: "s".to_owned(),
        };
        let kept = score_row(0x22, 3, time, time, time + 1, false, "kept");
        storage = flush_scores(&config, storage, score_rows(&[session_score, kept])).await;

        let deletion = Deletion {
            id: Uuid::now_v7(),
            project: project(),
            cutoff: time + 1,
            scope: DeletionScope::User {
                user_id: "u".to_owned(),
                trace_ids: Vec::new(),
                session_ids: vec!["s".to_owned()],
            },
        };
        storage.register_deletion(deletion.clone()).await.unwrap();
        let report = storage.apply_deletions(at(11, 0, 0)).await.unwrap();
        assert_eq!(
            (report.rewritten, report.applied, report.failed),
            (2, 1, 0),
            "{report:?}"
        );

        let spans_files = storage.catalog_files(Table::Spans);
        assert_eq!(spans_files.len(), 1);
        assert_eq!(
            spans_trace_ids(dir.path(), &spans_files[0]),
            vec![trace_bytes(2)],
            "the other user's span of the session stays"
        );
        let stored = score_file_rows(dir.path(), &storage);
        assert_eq!(stored.len(), 1, "{stored:?}");
        assert!(
            stored[0].0.ends_with("22"),
            "the score outside the scope stays: {stored:?}"
        );
        assert_eq!(
            status(&storage, &deletion).await,
            DeletionStatus::Applied {
                applied_at: at(11, 0, 0)
            }
        );
        storage.shutdown().await.unwrap();
    }

    // 9. An overdue deletion is warned about once.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_overdue_deletion_is_warned_about_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
        config.deletion.max_delay = Duration::from_secs(3_600);
        let storage = Storage::open(config.clone()).await.unwrap();
        let time = at(10, 0, 0);
        storage
            .append_spans(project(), span_rows(&[span_row(1, 0, time, time + 1)]))
            .await
            .unwrap();
        let deletion = traces_deletion(&[1], time + 1);
        storage.register_deletion(deletion.clone()).await.unwrap();

        let now = crate::catalog::now_ns() + 2 * 3_600 * 1_000_000_000;
        let report = storage.apply_deletions(now).await.unwrap();
        assert_eq!(report.applied, 0, "the memtable batch still blocks it");
        let overdue = overdue_logged(&storage);
        assert_eq!(overdue.len(), 1);
        assert!(overdue.contains(&deletion.id));

        let report = storage.apply_deletions(now + 1).await.unwrap();
        assert_eq!(report.applied, 0);
        let overdue = overdue_logged(&storage);
        assert_eq!(overdue.len(), 1, "the second pass warns about nothing new");
        storage.shutdown().await.unwrap();
    }

    // 10. A flush prepared before the registration does not make the
    //     deletion look applied.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_flush_in_flight_does_not_make_a_deletion_applied() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(8), Duration::from_millis(100));
        let storage = Arc::new(Storage::open(config.clone()).await.unwrap());
        let hooks = storage.hooks().clone();
        hooks.hold(HookPoint::FlushAfterWrite);
        let time = at(10, 0, 0);
        storage
            .append_spans(project(), span_rows(&[span_row(1, 0, time, time + 1)]))
            .await
            .unwrap();
        wait_at(&storage, HookPoint::FlushAfterWrite);

        // The flush was prepared before the registration: its file will keep
        // the row and record `deletions_applied_seq = 0`.
        let deletion = traces_deletion(&[1], time + 1);
        storage.register_deletion(deletion.clone()).await.unwrap();
        let report = storage.apply_deletions(at(11, 0, 0)).await.unwrap();
        assert_eq!(report.applied, 0, "{report:?}");
        assert_eq!(status(&storage, &deletion).await, DeletionStatus::Pending);

        hooks.release(HookPoint::FlushAfterWrite);
        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "the flush never committed"
            );
            if storage.catalog_files(Table::Spans).len() == 1
                && storage.published_rows(Table::Spans) == 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let files = storage.catalog_files(Table::Spans);
        assert_eq!(files[0].deletions_applied_seq, 0);
        assert!(
            spans_trace_ids(dir.path(), &files[0]).contains(&trace_bytes(1)),
            "the flushed file still holds trace 1"
        );
        // Never applied while an active file still holds a covered row.
        assert_eq!(status(&storage, &deletion).await, DeletionStatus::Pending);

        let report = storage.apply_deletions(at(11, 0, 0)).await.unwrap();
        assert_eq!(
            (report.rewritten, report.applied, report.failed),
            (1, 1, 0),
            "{report:?}"
        );
        for file in storage.catalog_files(Table::Spans) {
            assert!(!spans_trace_ids(dir.path(), &file).contains(&trace_bytes(1)));
        }
        assert_eq!(
            status(&storage, &deletion).await,
            DeletionStatus::Applied {
                applied_at: at(11, 0, 0)
            }
        );
        Arc::try_unwrap(storage)
            .unwrap_or_else(|_| panic!("storage still shared"))
            .shutdown()
            .await
            .unwrap();
    }

    // 11. A session-target score survives a covering merge.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_session_score_survives_a_covering_merge() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(2), Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        let time = at(10, 0, 0);
        let mut session_score = score_row(0x11, 0, time, time, time, false, "session");
        session_score.target = ScoreTarget::Session {
            session_id: "s".to_owned(),
        };
        let on_trace = score_row(0x22, 1, time, time, time, false, "on-trace-1");
        storage = flush_scores(&config, storage, score_rows(&[session_score])).await;
        storage = flush_scores(&config, storage, score_rows(&[on_trace])).await;
        assert_eq!(storage.catalog_files(Table::Scores).len(), 2);

        let deletion = traces_deletion(&[1], time + 1);
        storage.register_deletion(deletion.clone()).await.unwrap();
        let report = storage.compact(at(11, 0, 0)).await.unwrap();
        assert_eq!((report.jobs, report.failed), (1, 0), "{report:?}");
        let files = storage.catalog_files(Table::Scores);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].deletions_applied_seq, 1);
        let stored = score_file_rows(dir.path(), &storage);
        assert_eq!(stored.len(), 1, "{stored:?}");
        assert!(
            stored[0].0.ends_with("11"),
            "the session score survives the merge: {stored:?}"
        );
        storage.shutdown().await.unwrap();
    }
}
