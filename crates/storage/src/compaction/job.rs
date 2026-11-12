//! Compaction jobs: claim the inputs, merge slice by slice, write, stage,
//! publish and commit one catalog edit, then publish the catalog to readers.

use std::collections::HashSet;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use arrow::array::RecordBatch;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::catalog::{CatalogEdit, Committer, FileId, FileMeta, FileState, FileTable};
use crate::compaction::merge::{MergeContext, merge_slice};
use crate::compaction::plan::{Job, JobKind, Policy, plan};
use crate::compaction::slice::{slice_count, slices, time_values};
use crate::error::{CommitError, CompactionError, ObjectError};
use crate::hooks::{HookPoint, Hooks};
use crate::objects::Objects;
use crate::parquet::{FileWriter, footer_size_from_tail, time_column, write_file};
use crate::read_state::{Published, publish_catalog};

/// Shared by every job: built once in `Storage::open`.
pub(crate) struct Compactor {
    committer: Arc<Committer>,
    objects: Arc<Objects>,
    published: Arc<Published>,
    merge: MergeContext,
    /// Read at the start of each pass. Tests replace it ([`Storage::set_policy`]).
    policy: Mutex<Policy>,
    threads: usize,
    claims: Mutex<HashSet<FileId>>,
    /// Set by [`crate::Storage::shutdown`]; jobs check it before each slice and
    /// before committing.
    stopping: AtomicBool,
    hooks: Hooks,
}

impl std::fmt::Debug for Compactor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let claims = self
            .claims
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        formatter
            .debug_struct("Compactor")
            .field("threads", &self.threads)
            .field("claims", &claims)
            .finish()
    }
}

/// What a pass did. Byte counts are object sizes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompactionReport {
    /// Jobs that committed.
    pub jobs: u64,
    /// Jobs that failed or lost a conflict (logged); a later pass redoes them.
    pub failed: u64,
    /// Files the committed jobs removed, `traces` included.
    pub input_files: u64,
    /// Files the committed jobs added, `traces` included.
    pub output_files: u64,
    /// Bytes of the `spans` and `scores` inputs of committed jobs.
    pub input_bytes: u64,
    /// Bytes of the `spans` and `scores` outputs of committed jobs.
    pub output_bytes: u64,
    /// The part of `output_bytes` that is `spans`.
    pub spans_output_bytes: u64,
}

impl CompactionReport {
    fn add(self, other: CompactionReport) -> CompactionReport {
        CompactionReport {
            jobs: self.jobs + other.jobs,
            failed: self.failed + other.failed,
            input_files: self.input_files + other.input_files,
            output_files: self.output_files + other.output_files,
            input_bytes: self.input_bytes.saturating_add(other.input_bytes),
            output_bytes: self.output_bytes.saturating_add(other.output_bytes),
            spans_output_bytes: self
                .spans_output_bytes
                .saturating_add(other.spans_output_bytes),
        }
    }
}

/// Removes its ids from the claims when dropped. Created as soon as ids are
/// claimed; lives until the job's task ends, whatever the outcome.
struct ClaimGuard {
    compactor: Arc<Compactor>,
    ids: Vec<FileId>,
}

impl ClaimGuard {
    fn add(&mut self, id: FileId) {
        self.compactor
            .claims
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id);
        self.ids.push(id);
    }
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        let mut claims = self
            .compactor
            .claims
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for id in &self.ids {
            claims.remove(id);
        }
    }
}

impl Compactor {
    /// Built once in `Storage::open`. `merge` carries the compaction runtime
    /// with the `compaction.max_memory` pool and the registered object store.
    pub(crate) fn new(
        committer: Arc<Committer>,
        objects: Arc<Objects>,
        published: Arc<Published>,
        merge: MergeContext,
        policy: Policy,
        threads: usize,
        hooks: Hooks,
    ) -> Self {
        Self {
            committer,
            objects,
            published,
            merge,
            policy: Mutex::new(policy),
            threads,
            claims: Mutex::new(HashSet::new()),
            stopping: AtomicBool::new(false),
            hooks,
        }
    }

    /// One pass: plan under the claims lock, then run the jobs with at most
    /// `threads` at a time.
    pub(crate) async fn pass(
        self: &Arc<Self>,
        now: i64,
    ) -> Result<CompactionReport, CompactionError> {
        let catalog = self.committer.catalog().await;
        let mut spawned: Vec<(Job, ClaimGuard)> = Vec::new();
        {
            let mut claims = self.claims.lock().unwrap_or_else(PoisonError::into_inner);
            let policy = *self.policy.lock().unwrap_or_else(PoisonError::into_inner);
            for job in plan(&catalog, &claims, &policy, now) {
                for file in &job.inputs {
                    claims.insert(file.id);
                }
                let guard = ClaimGuard {
                    compactor: Arc::clone(self),
                    ids: job.inputs.iter().map(|file| file.id).collect(),
                };
                spawned.push((job, guard));
            }
        }

        let semaphore = Arc::new(Semaphore::new(self.threads.max(1)));
        let mut set: JoinSet<Result<CompactionReport, CompactionError>> = JoinSet::new();
        for (job, guard) in spawned {
            let permit = match Arc::clone(&semaphore).acquire_owned().await {
                Ok(permit) => permit,
                // The semaphore is never closed.
                Err(_) => unreachable!("the compaction semaphore is never closed"),
            };
            let compactor = Arc::clone(self);
            let policy = *self.policy.lock().unwrap_or_else(PoisonError::into_inner);
            set.spawn(async move {
                let _permit = permit;
                let _guard = guard;
                let table = job.table.as_str();
                let project = job.project.to_string();
                let day = job.day.to_string();
                let kind = kind_label(job.kind);
                let inputs = job.inputs.len();
                let outcome = compactor.run(job, policy).await;
                if let Err(error) = &outcome {
                    // The error text never carries row content.
                    if matches!(error, CompactionError::Commit(CommitError::Conflict { .. })) {
                        tracing::info!(
                            table,
                            project,
                            day,
                            kind,
                            inputs,
                            %error,
                            "compaction job lost a conflict; a later pass redoes it"
                        );
                    } else {
                        tracing::warn!(
                            table, project, day, kind, inputs, %error, "compaction job failed"
                        );
                    }
                }
                outcome
            });
        }

        let mut report = CompactionReport::default();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok(Ok(one)) => report = report.add(one),
                Ok(Err(_)) => {
                    // Logged where the job ran.
                    report.failed += 1;
                }
                Err(_) => {
                    report.failed += 1;
                    tracing::warn!("compaction task failed");
                }
            }
        }
        Ok(report)
    }

    /// Stops jobs before their next slice or commit.
    pub(crate) fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
    }

    /// Replaces the policy read at the start of each pass.
    #[cfg(test)]
    pub(crate) fn set_policy(&self, policy: Policy) {
        *self.policy.lock().unwrap_or_else(PoisonError::into_inner) = policy;
    }

    /// How many files are currently claimed. Tests assert this empties.
    #[cfg(test)]
    pub(crate) fn claimed(&self) -> usize {
        self.claims
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Runs one job to its commit.
    async fn run(
        self: &Arc<Self>,
        job: Job,
        policy: Policy,
    ) -> Result<CompactionReport, CompactionError> {
        let mut guard = ClaimGuard {
            compactor: Arc::clone(self),
            ids: Vec::new(),
        };
        // 1. The traces files derived from the inputs (spans only).
        let mut traces_ids: Vec<FileId> = Vec::new();
        if job.table == FileTable::Spans {
            let input_ids: Vec<FileId> = job.inputs.iter().map(|file| file.id).collect();
            let pairs = self.committer.derived_traces(&input_ids).await?;
            for input in &input_ids {
                if pairs.iter().filter(|(_, spans)| spans == input).count() != 1 {
                    return Err(CompactionError::Input {
                        reason: "spans file without exactly one traces file",
                    });
                }
            }
            for (traces, spans) in &pairs {
                if !input_ids.contains(spans) {
                    return Err(CompactionError::Input {
                        reason: "traces file shared outside the job",
                    });
                }
                traces_ids.push(*traces);
            }
            {
                let mut claims = self.claims.lock().unwrap_or_else(PoisonError::into_inner);
                if traces_ids.iter().any(|id| claims.contains(id)) {
                    return Err(CompactionError::Input {
                        reason: "traces file claimed",
                    });
                }
                for id in &traces_ids {
                    claims.insert(*id);
                }
            }
            for id in traces_ids.clone() {
                guard.add(id);
            }
        }

        // 2. The slices and their row counts.
        let input_bytes = job
            .inputs
            .iter()
            .fold(0_u64, |sum, file| sum.saturating_add(file.size));
        let values = time_values(self.objects.store(), &job.inputs, time_column(job.table)).await?;
        let slice_list = slices(&values, slice_count(input_bytes, policy.target_file_size));
        let counts: Vec<usize> = slice_list
            .iter()
            .map(|slice| {
                let start = match slice.lower {
                    Some(lower) => values.partition_point(|value| *value < lower),
                    None => 0,
                };
                let end = match slice.upper {
                    Some(upper) => values.partition_point(|value| *value < upper),
                    None => values.len(),
                };
                end - start
            })
            .collect();
        drop(values);

        // 3. Every slice, in order.
        let mut add: Vec<FileMeta> = Vec::new();
        let mut sources: Vec<(FileId, FileId)> = Vec::new();
        let mut output_bytes = 0_u64;
        for (slice, expected_rows) in slice_list.iter().zip(&counts) {
            if self.stopping.load(Ordering::Acquire) {
                return Err(CompactionError::Stopped);
            }
            let (path, id) = Objects::table_file_path(job.table, job.project, job.day, 1);
            let traces = (job.table == FileTable::Spans)
                .then(|| Objects::table_file_path(FileTable::Traces, job.project, job.day, 1));
            let mut staged = vec![path.clone()];
            if let Some((traces_path, _)) = &traces {
                staged.push(traces_path.clone());
            }
            self.committer.stage(&staged).await?;
            self.hooks.pause(HookPoint::CompactionAfterStage);
            if self.hooks.fail(HookPoint::CompactionAfterStage).is_some() {
                return Err(CompactionError::Fault);
            }

            let (target, file) = self.objects.create_output(&path).await?;
            let writer = FileWriter::new(
                job.table,
                1,
                file,
                u64::try_from(*expected_rows).unwrap_or(u64::MAX),
            )?;
            let merged = merge_slice(
                &self.merge,
                job.table,
                &job.inputs,
                *slice,
                job.drop_deleted,
                writer,
            )
            .await;
            let merged = match merged {
                Ok(merged) => merged,
                Err(error) => {
                    let _ = self.objects.discard_output(target).await;
                    return Err(error);
                }
            };
            if merged.rows == 0 {
                // A scores day close can drop every row. The staged rows stay
                // for GC.
                let _ = self.objects.discard_output(target).await;
                continue;
            }
            let crate::compaction::merge::MergedSlice {
                writer,
                partials,
                rows,
            } = merged;
            let temp = target.temp().to_path_buf();
            let finished = tokio::task::spawn_blocking(move || {
                let (file, summary) = writer.finish()?;
                let file_len = file
                    .metadata()
                    .map_err(|source| ObjectError::io("reading compaction output", &temp, source))?
                    .len();
                let mut file = file;
                file.seek(SeekFrom::End(-8)).map_err(|source| {
                    ObjectError::io("reading compaction output", &temp, source)
                })?;
                let mut tail = [0_u8; 8];
                file.read_exact(&mut tail).map_err(|source| {
                    ObjectError::io("reading compaction output", &temp, source)
                })?;
                let footer_size = footer_size_from_tail(&tail, file_len)?;
                Ok::<_, CompactionError>((file, summary, footer_size))
            })
            .await
            .map_err(|_| CompactionError::Task)??;
            let (file, summary, footer_size) = finished;

            let size = self.objects.publish_output(target, file).await?;
            output_bytes = output_bytes.saturating_add(size);
            let fully_merged = job.whole_partition
                && (job.table == FileTable::Spans || job.kind == JobKind::DayClose);
            add.push(FileMeta {
                id,
                table: job.table,
                project: job.project,
                day: job.day,
                level: 1,
                path,
                size,
                footer_size,
                rows,
                min_time: summary.min_time,
                max_time: summary.max_time,
                min_ingest_time: summary.min_ingest_time,
                max_ingest_time: summary.max_ingest_time,
                fully_merged,
            });

            if job.table == FileTable::Spans {
                let Some((traces_path, traces_id)) = traces else {
                    return Err(CompactionError::Input {
                        reason: "traces are not merged",
                    });
                };
                let partials: Vec<Arc<RecordBatch>> = partials.into_iter().map(Arc::new).collect();
                let written = tokio::task::spawn_blocking(move || {
                    write_file(FileTable::Traces, 1, &partials)
                })
                .await
                .map_err(|_| CompactionError::Task)??;
                self.objects
                    .put_durable(&traces_path, written.bytes.clone())
                    .await?;
                add.push(FileMeta {
                    id: traces_id,
                    table: FileTable::Traces,
                    project: job.project,
                    day: job.day,
                    level: 1,
                    path: traces_path,
                    size: u64::try_from(written.bytes.len()).unwrap_or(u64::MAX),
                    footer_size: written.footer_size,
                    rows: written.rows,
                    min_time: written.min_time,
                    max_time: written.max_time,
                    min_ingest_time: written.min_ingest_time,
                    max_ingest_time: written.max_ingest_time,
                    fully_merged,
                });
                sources.push((traces_id, id));
            }
        }

        // 4. Everything is written and published.
        self.hooks.pause(HookPoint::CompactionAfterWrite);
        if self.hooks.fail(HookPoint::CompactionAfterWrite).is_some() {
            return Err(CompactionError::Fault);
        }
        if self.stopping.load(Ordering::Acquire) {
            return Err(CompactionError::Stopped);
        }

        // 5-6. The edit. Empty `add` is legal: a scores day close may drop
        // every row, and removing the inputs is then the right result.
        let mut remove: Vec<(FileId, FileState)> = job
            .inputs
            .iter()
            .map(|file| (file.id, FileState::Active))
            .collect();
        for id in &traces_ids {
            remove.push((*id, FileState::Active));
        }
        let input_files = u64::try_from(remove.len()).unwrap_or(u64::MAX);
        let output_files = u64::try_from(add.len()).unwrap_or(u64::MAX);
        let edit = CatalogEdit {
            add,
            remove,
            flushed_seq: None,
            sources,
        };
        let published = Arc::clone(&self.published);
        self.committer
            .commit(&edit, move |catalog| {
                publish_catalog(&published, catalog);
            })
            .await?;

        self.hooks.pause(HookPoint::CompactionAfterCommit);
        if let Some(error) = self.hooks.fail(HookPoint::CompactionAfterCommit) {
            // The commit is durable. Retrying it would write a second copy.
            tracing::warn!(
                %error,
                "compaction commit hook failed after the catalog commit"
            );
        }

        // 8. The report.
        let report = CompactionReport {
            jobs: 1,
            failed: 0,
            input_files,
            output_files,
            input_bytes,
            output_bytes,
            spans_output_bytes: if job.table == FileTable::Spans {
                output_bytes
            } else {
                0
            },
        };
        tracing::info!(
            table = job.table.as_str(),
            project = %job.project,
            day = %job.day,
            kind = kind_label(job.kind),
            jobs = report.jobs,
            input_files = report.input_files,
            output_files = report.output_files,
            input_bytes = report.input_bytes,
            output_bytes = report.output_bytes,
            "compacted a partition"
        );
        Ok(report)
    }
}

fn kind_label(kind: JobKind) -> &'static str {
    match kind {
        JobKind::L0Merge => "l0-merge",
        JobKind::DayClose => "day-close",
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use arrow::array::{AsArray, RecordBatch};
    use chrono::{Datelike, NaiveDate};
    use datafusion::execution::context::{SessionConfig, SessionContext};
    use llmobs_core::batch::scores_to_record_batch;
    use llmobs_core::ids::ProjectId;
    use llmobs_core::score::{Score, ScoreTarget, ScoreValue};
    use uuid::Uuid;

    use crate::Storage;
    use crate::catalog::FileTable;
    use crate::compaction::plan::Policy;
    use crate::config::{
        CompactionConfig, FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig,
        WalConfig,
    };
    use crate::hooks::HookPoint;
    use crate::test_support::{rows, span_row, span_rows};
    use crate::views::QueryContext;

    const DAY_NS: i64 = 86_400_000_000_000;

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 3, 5).unwrap()
    }

    /// Day D at `hour:minute:second`, in ns.
    fn at(hour: i64, minute: i64, second: i64) -> i64 {
        let midnight = (i64::from(day().num_days_from_ce()) - 719_163) * DAY_NS;
        midnight + (hour * 3_600 + minute * 60 + second) * 1_000_000_000
    }

    /// `date` at 00:00:00, in ns.
    fn midnight(date: NaiveDate) -> i64 {
        (i64::from(date.num_days_from_ce()) - 719_163) * DAY_NS
    }

    fn config_in(dir: &Path, compaction: CompactionConfig, flush_age: Duration) -> StorageConfig {
        StorageConfig {
            data_dir: dir.to_path_buf(),
            object_store: None,
            s3: S3Config::default(),
            wal: WalConfig::default(),
            flush: FlushConfig {
                max_age: flush_age,
                ..FlushConfig::default()
            },
            memtable: MemtableConfig::default(),
            query: QueryConfig::default(),
            anchor: Default::default(),
            gc: Default::default(),
            compaction,
        }
    }

    /// `[storage.compaction]` for hand-run passes with `l0_files` set.
    fn quiet_compaction(l0_files: usize) -> CompactionConfig {
        CompactionConfig {
            interval: Duration::from_secs(3_600),
            l0_files,
            ..CompactionConfig::default()
        }
    }

    /// Appends `batch`, shuts down and reopens: one L0 file per partition.
    async fn flush_cycle(config: &StorageConfig, storage: Storage, batch: RecordBatch) -> Storage {
        storage.append_spans(project(), batch).await.unwrap();
        storage.shutdown().await.unwrap();
        Storage::open(config.clone()).await.unwrap()
    }

    async fn flush_scores(config: &StorageConfig, storage: Storage, batch: RecordBatch) -> Storage {
        storage.append_scores(project(), batch).await.unwrap();
        storage.shutdown().await.unwrap();
        Storage::open(config.clone()).await.unwrap()
    }

    /// `(trace_id, span_id, ingest_time)` of every `spans` row, as text.
    async fn spans_rows(storage: &Storage) -> Vec<Vec<String>> {
        view_rows(
            storage,
            "SELECT trace_id, span_id, ingest_time FROM spans ORDER BY trace_id, span_id",
        )
        .await
    }

    /// `(score_id, name)` of every `scores` view row, as text.
    async fn scores_rows(storage: &Storage) -> Vec<Vec<String>> {
        view_rows(
            storage,
            "SELECT score_id, name FROM scores ORDER BY score_id",
        )
        .await
    }

    async fn view_rows(storage: &Storage, sql: &str) -> Vec<Vec<String>> {
        let query = storage.query(project(), Default::default()).unwrap();
        let ctx = QueryContext::new(query, None).unwrap();
        text_rows(&rows(&ctx, sql).await)
    }

    fn text_rows(batches: &[RecordBatch]) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        for batch in batches {
            let formatters: Vec<_> = (0..batch.num_columns())
                .map(|index| {
                    arrow::util::display::ArrayFormatter::try_new(
                        batch.column(index),
                        &arrow::util::display::FormatOptions::default(),
                    )
                    .unwrap()
                })
                .collect();
            for row in 0..batch.num_rows() {
                out.push(
                    formatters
                        .iter()
                        .map(|column| column.value(row).to_string())
                        .collect(),
                );
            }
        }
        out
    }

    /// Rows of the raw provider behind the `traces` view, read with SQL.
    async fn raw_traces(storage: &Storage, columns: &str) -> Vec<RecordBatch> {
        let query = storage.query(project(), Default::default()).unwrap();
        let ctx =
            SessionContext::new_with_config_rt(SessionConfig::new(), Arc::clone(&query.runtime));
        ctx.register_table("traces", Arc::clone(&query.traces))
            .unwrap();
        ctx.sql(&format!("SELECT {columns} FROM traces"))
            .await
            .unwrap()
            .collect()
            .await
            .unwrap()
    }

    /// `(trace_id, partial_count, span_count)` from the raw traces provider.
    async fn partial_counts(storage: &Storage) -> Vec<(Vec<u8>, i32, i64)> {
        let batches = raw_traces(storage, "trace_id, partial_count, span_count").await;
        let mut out = Vec::new();
        for batch in batches {
            let ids = batch.column(0).as_fixed_size_binary();
            let partials = batch
                .column(1)
                .as_primitive::<arrow::datatypes::Int32Type>();
            let spans = batch
                .column(2)
                .as_primitive::<arrow::datatypes::Int64Type>();
            for row in 0..batch.num_rows() {
                out.push((
                    ids.value(row).to_vec(),
                    partials.value(row),
                    spans.value(row),
                ));
            }
        }
        out.sort();
        out
    }

    async fn obsolete_count(dir: &Path) -> i64 {
        use sqlx::ConnectOptions;
        let mut connection = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(dir.join("catalog.db"))
            .read_only(true)
            .connect()
            .await
            .unwrap();
        sqlx::query_scalar("SELECT COUNT(*) FROM data_files WHERE state = 'obsolete'")
            .fetch_one(&mut connection)
            .await
            .unwrap()
    }

    /// `(score_id, deleted)` of every row of the active scores files, read
    /// from the objects themselves.
    fn score_file_rows(dir: &Path, storage: &Storage) -> Vec<(String, bool)> {
        let mut out = Vec::new();
        for file in storage.catalog_files(crate::wal::Table::Scores) {
            let object = std::fs::File::open(dir.join("objects").join(&file.path)).unwrap();
            let builder =
                parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(object)
                    .unwrap();
            let schema = Arc::clone(builder.schema());
            let batches: Vec<RecordBatch> = builder
                .build()
                .unwrap()
                .map(|batch| batch.unwrap())
                .collect();
            let combined = arrow::compute::concat_batches(&schema, &batches).unwrap();
            let ids = combined
                .column_by_name(llmobs_core::schema::scores::SCORE_ID)
                .unwrap()
                .as_string::<i32>();
            let deleted = combined
                .column_by_name(llmobs_core::schema::scores::DELETED)
                .unwrap()
                .as_boolean();
            for row in 0..combined.num_rows() {
                out.push((ids.value(row).to_owned(), deleted.value(row)));
            }
        }
        out.sort();
        out
    }

    fn wait_at(storage: &Storage, point: HookPoint) {
        let started = Instant::now();
        while storage.hooks().waiting_at(point) == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "compaction never reached {point:?}"
            );
            std::thread::yield_now();
        }
    }

    fn policy(l0_files: usize, target_file_size: u64) -> Policy {
        Policy {
            target_file_size,
            ..Policy::from(&quiet_compaction(l0_files))
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_l0_merge_rewrites_a_partition() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(3), Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        for index in 0..3_u8 {
            let base = at(10, 0, 0) + i64::from(index) * 60 * 1_000_000_000;
            let batch = span_rows(&[
                span_row(0, 10 + index, base, base + 1_000_000_000),
                span_row(1, 20 + index, base + 2_000_000_000, base + 3_000_000_000),
            ]);
            storage = flush_cycle(&config, storage, batch).await;
        }
        assert_eq!(storage.catalog_files(crate::wal::Table::Spans).len(), 3);
        let before_spans = spans_rows(&storage).await;
        let before_traces = view_rows(
            &storage,
            "SELECT trace_id, span_count, error_count FROM traces ORDER BY trace_id",
        )
        .await;

        let report = storage.compact(at(11, 0, 0)).await.unwrap();
        assert_eq!(
            (
                report.jobs,
                report.failed,
                report.input_files,
                report.output_files
            ),
            (1, 0, 6, 2),
            "{report:?}"
        );

        let spans_files = storage.catalog_files(crate::wal::Table::Spans);
        assert_eq!(spans_files.len(), 1);
        assert_eq!(spans_files[0].level, 1);
        let traces_files = storage.catalog_paths(FileTable::Traces);
        assert_eq!(traces_files.len(), 1);
        assert_eq!(obsolete_count(dir.path()).await, 6);

        assert_eq!(spans_rows(&storage).await, before_spans);
        assert_eq!(
            view_rows(
                &storage,
                "SELECT trace_id, span_count, error_count FROM traces ORDER BY trace_id"
            )
            .await,
            before_traces
        );
        let partials = partial_counts(&storage).await;
        assert_eq!(partials.len(), 2, "{partials:?}");
        assert!(partials.iter().all(|(_, count, _)| *count == 1));

        storage.shutdown().await.unwrap();
        drop(storage);
        let storage = Storage::open(config).await.unwrap();
        assert_eq!(storage.catalog_files(crate::wal::Table::Spans).len(), 1);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_merge_collapses_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(3), Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        // The same (trace, span) in two flushes, the later ingest newer.
        let first = at(10, 0, 0);
        let second = at(10, 1, 0);
        storage = flush_cycle(
            &config,
            storage,
            span_rows(&[span_row(0, 1, first, first + 1_000_000_000)]),
        )
        .await;
        storage = flush_cycle(
            &config,
            storage,
            span_rows(&[span_row(0, 1, first, second + 1_000_000_000)]),
        )
        .await;
        storage = flush_cycle(
            &config,
            storage,
            span_rows(&[span_row(0, 2, second, second + 1_000_000_000)]),
        )
        .await;

        let before = partial_counts(&storage).await;
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].2, 3, "the read-time merge counts the dup twice");

        let report = storage.compact(at(11, 0, 0)).await.unwrap();
        assert_eq!(report.jobs, 1);

        let rows = spans_rows(&storage).await;
        assert_eq!(rows.len(), 2, "{rows:?}");
        let after = partial_counts(&storage).await;
        assert_eq!(after[0].2, 2, "one less than before the merge");
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_size_ratio_keeps_the_big_file_out() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(4), Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        // Four big files: 200 spans each, every span carrying 1 KiB of input.
        let mut noise = hex_noise(1, 1024);
        for index in 0..4_u8 {
            let base = at(10, 0, 0) + i64::from(index) * 60 * 1_000_000_000;
            let spans: Vec<_> = (0..200_u16)
                .map(|row| {
                    let start = base + i64::from(row) * 1_000_000;
                    let mut span =
                        span_row(0, u8::try_from(row % 200).unwrap_or(0), start, start + 1);
                    span.input = Some(noise.next().unwrap_or_default());
                    span
                })
                .collect();
            storage = flush_cycle(&config, storage, span_rows(&spans)).await;
        }
        storage.set_policy(policy(4, 10 * 1024 * 1024));
        let report = storage.compact(at(11, 0, 0)).await.unwrap();
        assert_eq!(report.jobs, 1);
        let big = storage.catalog_files(crate::wal::Table::Spans)[0].clone();

        // Two small files of 5 spans each.
        for index in 0..2_u8 {
            let base = at(10, 30, 0) + i64::from(index) * 60 * 1_000_000_000;
            let spans: Vec<_> = (200..205_u16)
                .map(|row| {
                    let start = base + i64::from(row) * 1_000_000;
                    span_row(0, u8::try_from(row).unwrap_or(0), start, start + 1)
                })
                .collect();
            storage = flush_cycle(&config, storage, span_rows(&spans)).await;
        }
        let files = storage.catalog_files(crate::wal::Table::Spans);
        let smalls: Vec<_> = files.iter().filter(|file| file.level == 0).collect();
        assert_eq!(smalls.len(), 2);
        let small_total: u64 = smalls.iter().map(|file| file.size).sum();
        assert!(
            small_total < 4 * big.size,
            "precondition: {small_total} is not below 4 x {}",
            big.size
        );

        let report = storage.compact(at(11, 30, 0)).await.unwrap();
        assert_eq!((report.jobs, report.failed), (1, 0), "{report:?}");
        assert!(
            report.input_bytes <= small_total,
            "the big L1 file was an input: {report:?}"
        );

        let files = storage.catalog_files(crate::wal::Table::Spans);
        let level_one: Vec<_> = files.iter().filter(|file| file.level == 1).collect();
        assert_eq!(level_one.len(), 2, "two L1 spans files");
        assert!(level_one.iter().any(|file| file.id == big.id));
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_day_close_merges_the_whole_partition() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        for index in 0..3_u8 {
            let base = at(10, 0, 0) + i64::from(index) * 60 * 1_000_000_000;
            storage = flush_cycle(
                &config,
                storage,
                span_rows(&[span_row(0, 10 + index, base, base + 1_000_000_000)]),
            )
            .await;
        }

        let now = midnight(day().succ_opt().unwrap()) + (2 * 3_600) * 1_000_000_000 + 1;
        let report = storage.compact(now).await.unwrap();
        assert_eq!(
            (report.jobs, report.input_files, report.output_files),
            (1, 6, 2)
        );

        let spans_files = storage.catalog_files(crate::wal::Table::Spans);
        assert_eq!(spans_files.len(), 1);
        assert!(spans_files[0].fully_merged);
        let traces_files = storage.catalog_paths(FileTable::Traces);
        assert_eq!(traces_files.len(), 1);
        assert!(
            partial_counts(&storage)
                .await
                .iter()
                .all(|(_, count, _)| *count == 1)
        );

        // The day is closed: nothing more to do.
        let again = storage.compact(now).await.unwrap();
        assert_eq!((again.jobs, again.failed), (0, 0), "{again:?}");
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_big_day_close_slices_its_output() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        for index in 0..3_u8 {
            let base = at(0, 0, 0) + i64::from(index) * 3_600 * 1_000_000_000;
            let spans: Vec<_> = (0..50_u8)
                .map(|row| {
                    let start = base + i64::from(row) * 40_000_000_000;
                    span_row(0, index * 50 + row, start, start + 1)
                })
                .collect();
            storage = flush_cycle(&config, storage, span_rows(&spans)).await;
        }
        let total: u64 = storage
            .catalog_files(crate::wal::Table::Spans)
            .iter()
            .map(|file| file.size)
            .sum();
        storage.set_policy(policy(8, total / 3));

        let before_spans = spans_rows(&storage).await;
        let now = midnight(day().succ_opt().unwrap()) + (2 * 3_600) * 1_000_000_000 + 1;
        let report = storage.compact(now).await.unwrap();
        assert_eq!(report.jobs, 1);

        let files = storage.catalog_files(crate::wal::Table::Spans);
        assert!(files.len() >= 2, "{} spans files", files.len());
        assert_eq!(storage.catalog_paths(FileTable::Traces).len(), files.len());
        let ranges: Vec<_> = files
            .iter()
            .map(|file| (file.min_time, file.max_time))
            .collect();
        for (index, (min, max)) in ranges.iter().enumerate() {
            for (other_min, other_max) in ranges.iter().skip(index + 1) {
                assert!(
                    max <= other_min || other_max <= min,
                    "ranges overlap: {ranges:?}"
                );
            }
        }
        assert_eq!(spans_rows(&storage).await, before_spans);
        storage.shutdown().await.unwrap();
    }

    fn score_version(id: u128, updated_at: i64, ingest: i64, deleted: bool, name: &str) -> Score {
        Score {
            project_id: project(),
            score_id: Uuid::from_u128(id),
            target: ScoreTarget::Trace {
                trace_id: "4bf92f3577b34da6a3ce929d0e0e4736".parse().unwrap(),
            },
            session_id: None,
            user_id: None,
            name: name.to_owned(),
            value: ScoreValue::Numeric(0.5),
            reason: None,
            source: llmobs_core::enums::ScoreSource::Api,
            evaluator_id: None,
            rule_id: None,
            experiment_id: None,
            dataset_item_id: None,
            author_user_id: None,
            metadata: Default::default(),
            created_at: at(10, 0, 0),
            updated_at,
            deleted,
            ingest_time: ingest,
        }
    }

    /// Score `s` in three L0 files (newest deleted), score `t` live once.
    async fn score_setup(config: &StorageConfig) -> Storage {
        let mut storage = Storage::open(config.clone()).await.unwrap();
        let batches = [
            vec![
                score_version(0x11, at(10, 0, 1), at(10, 0, 1), false, "s-old"),
                score_version(0x22, at(10, 0, 5), at(10, 0, 5), false, "t"),
            ],
            vec![score_version(
                0x11,
                at(10, 0, 3),
                at(10, 0, 3),
                true,
                "s-deleted",
            )],
            vec![score_version(
                0x11,
                at(10, 0, 2),
                at(10, 0, 2),
                false,
                "s-middle",
            )],
        ];
        for batch in batches {
            storage = flush_scores(config, storage, scores_to_record_batch(&batch).unwrap()).await;
        }
        storage
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn scores_merge_and_day_close_drop_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(3), Duration::from_secs(3_600));
        let storage = score_setup(&config).await;
        assert_eq!(storage.catalog_files(crate::wal::Table::Scores).len(), 3);

        // The L0 merge keeps the deleted row in the file; the view hides it.
        let report = storage.compact(at(11, 0, 0)).await.unwrap();
        assert_eq!(
            (report.jobs, report.input_files, report.output_files),
            (1, 3, 1)
        );
        let files = storage.catalog_files(crate::wal::Table::Scores);
        assert_eq!(files.len(), 1);
        assert!(!files[0].fully_merged);
        // The provider already applies the view's rules, so the file itself is
        // what shows the deleted row survived the L0 merge.
        let stored = score_file_rows(dir.path(), &storage);
        assert_eq!(stored.len(), 2, "{stored:?}");
        assert!(
            stored
                .iter()
                .any(|(id, deleted)| id.ends_with("22") && !*deleted),
            "{stored:?}"
        );
        assert_eq!(scores_rows(&storage).await.len(), 1, "the view hides s");

        // The day close drops the deleted winner entirely.
        let now = midnight(day().succ_opt().unwrap()) + (2 * 3_600) * 1_000_000_000 + 1;
        let report = storage.compact(now).await.unwrap();
        assert_eq!(report.jobs, 1);
        let files = storage.catalog_files(crate::wal::Table::Scores);
        assert_eq!(files.len(), 1);
        assert!(files[0].fully_merged);
        assert_eq!(scores_rows(&storage).await.len(), 1);
        let stored = score_file_rows(dir.path(), &storage);
        assert_eq!(stored.len(), 1, "only t survives: {stored:?}");
        let again = storage.compact(now).await.unwrap();
        assert_eq!(again.jobs, 0);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_job_that_loses_its_inputs_fails_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(3), Duration::from_secs(3_600));
        let mut opened = Storage::open(config.clone()).await.unwrap();
        for index in 0..3_u8 {
            let base = at(10, 0, 0) + i64::from(index) * 60 * 1_000_000_000;
            let batch = span_rows(&[
                span_row(0, 10 + index, base, base + 1_000_000_000),
                span_row(1, 20 + index, base + 2_000_000_000, base + 3_000_000_000),
            ]);
            opened = flush_cycle(&config, opened, batch).await;
        }
        let storage = Arc::new(opened);

        let hooks = storage.hooks().clone();
        hooks.hold(HookPoint::CompactionAfterWrite);
        let racing = Arc::clone(&storage);
        let compacting = tokio::spawn(async move { racing.compact(at(11, 0, 0)).await });
        wait_at(&storage, HookPoint::CompactionAfterWrite);

        // Another writer removes one input pair while the job is parked.
        let removed = {
            use sqlx::ConnectOptions;
            let mut connection = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(dir.path().join("catalog.db"))
                .read_only(true)
                .connect()
                .await
                .unwrap();
            sqlx::query_as::<_, (String, String)>(
                "SELECT s.id, t.id FROM data_files AS s
                 JOIN file_sources AS fs ON fs.spans_file_id = s.id
                 JOIN data_files AS t ON t.id = fs.traces_file_id
                 WHERE s.table_name = 'spans' AND s.state = 'active'
                 ORDER BY s.id LIMIT 1",
            )
            .fetch_one(&mut connection)
            .await
            .unwrap()
        };
        let edit = crate::catalog::CatalogEdit {
            add: Vec::new(),
            remove: vec![
                (
                    crate::catalog::FileId(Uuid::parse_str(&removed.0).unwrap()),
                    crate::catalog::FileState::Active,
                ),
                (
                    crate::catalog::FileId(Uuid::parse_str(&removed.1).unwrap()),
                    crate::catalog::FileState::Active,
                ),
            ],
            flushed_seq: None,
            sources: Vec::new(),
        };
        storage.commit_test_edit(&edit).await.unwrap();

        hooks.release(HookPoint::CompactionAfterWrite);
        let report = compacting.await.unwrap().unwrap();
        assert_eq!((report.jobs, report.failed), (0, 1), "{report:?}");
        assert_eq!(storage.compaction_claims(), 0);

        storage.collect_staged(Duration::ZERO).await.unwrap();
        assert!(storage.staged_paths().await.unwrap().is_empty());
        let spans = spans_rows(&storage).await;
        assert_eq!(spans.len(), 4, "one file's rows are gone: {spans:?}");
        let mut seen = std::collections::HashSet::new();
        for row in &spans {
            assert!(seen.insert(row.clone()), "a span appears twice: {row:?}");
        }
        Arc::try_unwrap(storage)
            .unwrap_or_else(|_| panic!("storage still shared"))
            .shutdown()
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn two_passes_share_one_job() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(3), Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        for index in 0..3_u8 {
            let base = at(10, 0, 0) + i64::from(index) * 60 * 1_000_000_000;
            storage = flush_cycle(
                &config,
                storage,
                span_rows(&[span_row(0, 10 + index, base, base + 1_000_000_000)]),
            )
            .await;
        }

        let storage = Arc::new(storage);
        let hooks = storage.hooks().clone();
        hooks.hold(HookPoint::CompactionAfterStage);
        let first = {
            let storage = Arc::clone(&storage);
            tokio::spawn(async move { storage.compact(at(11, 0, 0)).await })
        };
        wait_at(&storage, HookPoint::CompactionAfterStage);
        let second = {
            let storage = Arc::clone(&storage);
            tokio::spawn(async move { storage.compact(at(11, 0, 0)).await })
        };
        let second = second.await.unwrap().unwrap();
        hooks.release(HookPoint::CompactionAfterStage);
        let first = first.await.unwrap().unwrap();

        assert_eq!(first.jobs + second.jobs, 1, "{first:?} {second:?}");
        assert_eq!(first.failed + second.failed, 0);
        Arc::try_unwrap(storage)
            .unwrap_or_else(|_| panic!("storage still shared"))
            .shutdown()
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_query_pins_the_files_it_reads() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(2), Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        for index in 0..2_u8 {
            let base = at(10, 0, 0) + i64::from(index) * 60 * 1_000_000_000;
            storage = flush_cycle(
                &config,
                storage,
                span_rows(&[span_row(0, 10 + index, base, base + 1_000_000_000)]),
            )
            .await;
        }
        let pinned = storage.query(project(), Default::default()).unwrap();
        let before = {
            let ctx =
                QueryContext::new(storage.query(project(), Default::default()).unwrap(), None)
                    .unwrap();
            text_rows(&rows(&ctx, "SELECT trace_id, span_id FROM spans ORDER BY span_id").await)
        };

        let report = storage.compact(at(11, 0, 0)).await.unwrap();
        assert_eq!(report.jobs, 1);
        let collected = storage.collect_obsolete(Duration::ZERO).await.unwrap();
        assert_eq!(collected.deleted, 0, "{collected:?}");
        assert!(collected.in_use >= 1, "{collected:?}");

        // The old query still returns the old rows; dropped with its context,
        // it releases the files.
        {
            let ctx = QueryContext::new(pinned, None).unwrap();
            assert_eq!(
                text_rows(
                    &rows(&ctx, "SELECT trace_id, span_id FROM spans ORDER BY span_id").await
                ),
                before
            );
        }
        let collected = storage.collect_obsolete(Duration::ZERO).await.unwrap();
        assert!(collected.deleted >= 1, "{collected:?}");
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_flush_during_a_job_commits_alongside_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = config_in(dir.path(), quiet_compaction(2), Duration::from_millis(100));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        for index in 0..2_u8 {
            let base = at(10, 0, 0) + i64::from(index) * 60 * 1_000_000_000;
            storage = flush_cycle(
                &config,
                storage,
                span_rows(&[span_row(0, 10 + index, base, base + 1_000_000_000)]),
            )
            .await;
        }

        let storage = Arc::new(storage);
        let hooks = storage.hooks().clone();
        hooks.hold(HookPoint::CompactionAfterWrite);
        let racing = {
            let handle = Arc::clone(&storage);
            tokio::spawn(async move { handle.compact(at(11, 0, 0)).await })
        };
        wait_at(&storage, HookPoint::CompactionAfterWrite);

        // New rows flush by age while the job is parked.
        let base = at(10, 5, 0);
        storage
            .append_spans(
                project(),
                span_rows(&[span_row(0, 30, base, base + 1_000_000_000)]),
            )
            .await
            .unwrap();
        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "the new rows did not flush"
            );
            if storage.catalog_files(crate::wal::Table::Spans).len() == 3
                && storage.published_rows(crate::wal::Table::Spans) == 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        hooks.release(HookPoint::CompactionAfterWrite);
        let report = racing.await.unwrap().unwrap();
        assert_eq!(report.jobs, 1, "{report:?}");

        let files = storage.catalog_files(crate::wal::Table::Spans);
        assert_eq!(files.len(), 2, "the L1 output and the new L0 file");
        assert!(files.iter().any(|file| file.level == 0), "{files:?}");
        let spans = spans_rows(&storage).await;
        assert_eq!(spans.len(), 3, "{spans:?}");
        let mut seen = std::collections::HashSet::new();
        for row in &spans {
            assert!(seen.insert(row.clone()), "a span appears twice: {row:?}");
        }
        Arc::try_unwrap(storage)
            .unwrap_or_else(|_| panic!("storage still shared"))
            .shutdown()
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_background_task_compacts() {
        let dir = tempfile::tempdir().unwrap();
        let compaction = CompactionConfig {
            interval: Duration::from_millis(200),
            l0_files: 2,
            ..CompactionConfig::default()
        };
        let config = config_in(dir.path(), compaction, Duration::from_secs(3_600));
        let mut storage = Storage::open(config.clone()).await.unwrap();
        for index in 0..2_u8 {
            let base = at(10, 0, 0) + i64::from(index) * 60 * 1_000_000_000;
            storage = flush_cycle(
                &config,
                storage,
                span_rows(&[span_row(0, 10 + index, base, base + 1_000_000_000)]),
            )
            .await;
        }

        let started = Instant::now();
        loop {
            let level_one = storage
                .catalog_files(crate::wal::Table::Spans)
                .into_iter()
                .filter(|file| file.level == 1)
                .count();
            if level_one >= 1 {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(15),
                "the task never compacted"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        storage.shutdown().await.unwrap();
    }

    /// Endless pseudo-random hex strings, `seed`ed, `width` characters each.
    struct HexNoise {
        state: u64,
        width: usize,
    }

    impl Iterator for HexNoise {
        type Item = String;
        fn next(&mut self) -> Option<String> {
            let mut out = String::new();
            while out.len() < self.width.max(1) {
                self.state = self
                    .state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                out.push(
                    char::from_digit(u32::from((self.state >> 33) as u8 % 16), 16).unwrap_or('0'),
                );
            }
            Some(out)
        }
    }

    fn hex_noise(seed: u64, width: usize) -> HexNoise {
        HexNoise { state: seed, width }
    }

    /// The M4 exit criterion: a day of steady ingest is rewritten once by the
    /// L0 merges and once by the day close, and never again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn write_amplification_of_a_synthetic_day() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config_in(dir.path(), quiet_compaction(8), Duration::from_secs(3_600));
        config.compaction.target_file_size = bytesize::ByteSize::mib(64);
        config.compaction.l0_bytes = bytesize::ByteSize::mib(64);
        let wa_policy = Policy::from(&config.compaction);
        let mut storage = Storage::open(config.clone()).await.unwrap();

        // Day D in 48 half-hour steps. Each step appends 40 traces × 10 spans
        // with 200 hex characters of input per span, so the files are not
        // mostly Parquet overhead.
        let mut noise = hex_noise(1, 200);
        let mut flushed = 0_u64;
        let mut rewritten = 0_u64;
        for step in 0..48_u16 {
            let step_start = at(0, 0, 0) + i64::from(step) * 1_800 * 1_000_000_000;
            let mut spans = Vec::with_capacity(400);
            for trace in 0..40_u16 {
                for span in 0..10_u8 {
                    // 400 spans spread over the half hour, 4.5 s apart, so the
                    // newest row of a step is seconds old when it compacts.
                    let start =
                        step_start + (i64::from(trace) * 10 + i64::from(span)) * 4_500_000_000;
                    let mut row = span_row(step * 40 + trace, span, start, start + 1_000_000_000);
                    row.input = Some(noise.next().unwrap_or_default());
                    spans.push(row);
                }
            }
            storage = flush_cycle(&config, storage, span_rows(&spans)).await;
            storage.set_policy(wa_policy);
            // L0 files accumulate between merges; this step's file is the
            // newest by `max_ingest_time`.
            let new_file = storage
                .catalog_files(crate::wal::Table::Spans)
                .into_iter()
                .filter(|file| file.level == 0)
                .max_by_key(|file| file.max_ingest_time)
                .expect("an L0 file per step");
            flushed = flushed.saturating_add(new_file.size);

            let now = step_start + 1_800 * 1_000_000_000 + 1_000_000_000;
            let report = storage.compact(now).await.unwrap();
            assert_eq!(report.failed, 0, "{report:?}");
            rewritten = rewritten.saturating_add(report.spans_output_bytes);
        }

        let l1_before_close = storage
            .catalog_files(crate::wal::Table::Spans)
            .into_iter()
            .filter(|file| file.level == 1)
            .count();
        // One day close of every L1 file.
        let close_at = midnight(day().succ_opt().unwrap()) + (2 * 3_600) * 1_000_000_000 + 1;
        for _ in 0..10 {
            let report = storage.compact(close_at).await.unwrap();
            assert_eq!(report.failed, 0, "{report:?}");
            rewritten = rewritten.saturating_add(report.spans_output_bytes);
            if report.jobs == 0 {
                break;
            }
        }

        let files = storage.catalog_files(crate::wal::Table::Spans);
        let wa = rewritten as f64 / flushed as f64;
        println!(
            "write amplification: flushed {flushed} B, rewritten {rewritten} B, wa {wa:.2}, \
             L1 files before close {l1_before_close}, spans files after {}",
            files.len()
        );
        assert_eq!(l1_before_close, 6);
        assert!(wa <= 3.0, "write amplification {wa:.2} exceeds 3");
        assert_eq!(files.len(), 1, "exactly one spans file remains");
        assert_eq!(files[0].level, 1);
        assert!(files[0].fully_merged);

        let spans = spans_rows(&storage).await;
        assert_eq!(spans.len(), 48 * 400, "every span present once");
        let mut seen = std::collections::HashSet::new();
        for row in &spans {
            assert!(seen.insert(row.clone()), "a span appears twice: {row:?}");
        }
        storage.shutdown().await.unwrap();
    }
}
