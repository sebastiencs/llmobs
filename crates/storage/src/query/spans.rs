//! `spans` table provider over one snapshot's files and memtable.

use std::sync::Arc;

use async_trait::async_trait;
use datafusion::catalog::{Session, TableProvider};
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::physical_plan::ExecutionPlan;
use llmobs_core::ids::ProjectId;
use llmobs_core::schema::{spans, spans_schema};

#[cfg(test)]
use super::overlapping_files;
use super::{ScanUnion, scan_union};
use crate::catalog::Catalog;
#[cfg(test)]
use crate::catalog::FileMeta;
use crate::memtable::MemSnapshot;
use crate::wal::Table;

/// Spans of one project at one read snapshot.
#[derive(Debug)]
pub(crate) struct SpansProvider {
    project: ProjectId,
    snapshot: Arc<MemSnapshot>,
    catalog: Arc<Catalog>,
    target_partitions: usize,
}

impl SpansProvider {
    pub(crate) fn new(
        project: ProjectId,
        snapshot: Arc<MemSnapshot>,
        catalog: Arc<Catalog>,
        target_partitions: usize,
    ) -> Self {
        Self {
            project,
            snapshot,
            catalog,
            target_partitions,
        }
    }

    /// Memtable batches the scan would read for `filters`, before distribution.
    #[cfg(test)]
    pub(crate) fn candidate_batches(&self, filters: &[Expr]) -> usize {
        super::rows_for(self.project, &self.snapshot, spans::START_TIME, filters).len()
    }

    /// Files the scan would open for `filters`.
    #[cfg(test)]
    pub(crate) fn candidate_files(&self, filters: &[Expr]) -> Vec<Arc<FileMeta>> {
        overlapping_files(
            &self.catalog,
            Table::Spans,
            self.project,
            spans::START_TIME,
            filters,
        )
    }
}

#[async_trait]
impl TableProvider for SpansProvider {
    fn schema(&self) -> arrow::datatypes::SchemaRef {
        spans_schema()
    }

    fn table_type(&self) -> TableType {
        TableType::Base
    }

    fn supports_filters_pushdown(
        &self,
        filters: &[&Expr],
    ) -> datafusion::error::Result<Vec<TableProviderFilterPushDown>> {
        Ok(vec![TableProviderFilterPushDown::Inexact; filters.len()])
    }

    async fn scan(
        &self,
        state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> datafusion::error::Result<Arc<dyn ExecutionPlan>> {
        scan_union(&ScanUnion {
            project: self.project,
            snapshot: &self.snapshot,
            catalog: &self.catalog,
            table: Table::Spans,
            time_column: spans::START_TIME,
            schema: spans_schema(),
            projection,
            filters,
            limit,
            target_partitions: self.target_partitions,
            session: state,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use arrow::array::{Array, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray};
    use arrow::datatypes::DataType;
    use datafusion::execution::context::{SessionConfig, SessionContext};
    use datafusion::logical_expr::{BinaryExpr, Cast, Expr, Operator, col, lit};
    use llmobs_core::ids::ProjectId;
    use llmobs_core::schema::spans;

    use crate::ProjectQuery;
    use crate::hooks::HookPoint;

    use super::super::DAY_NS;
    use super::*;
    use crate::config::{
        FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig, WalConfig,
    };
    use crate::wal::spans_at;

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn other_project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000002".parse().unwrap()
    }

    fn config_in(dir: &Path) -> StorageConfig {
        StorageConfig {
            data_dir: dir.to_path_buf(),
            object_store: None,
            s3: S3Config::default(),
            wal: WalConfig::default(),
            flush: FlushConfig::default(),
            memtable: MemtableConfig::default(),
            query: QueryConfig {
                target_partitions: 2,
                ..QueryConfig::default()
            },
        }
    }

    fn with_project(batch: &RecordBatch, project: ProjectId) -> RecordBatch {
        let schema = batch.schema();
        let index = schema.index_of(spans::PROJECT_ID).unwrap();
        let mut columns = batch.columns().to_vec();
        columns[index] = Arc::new(StringArray::from(vec![
            project.to_string();
            batch.num_rows()
        ]));
        RecordBatch::try_new(schema, columns).unwrap()
    }

    async fn collect(ctx: &SessionContext, sql: &str) -> Vec<RecordBatch> {
        ctx.sql(sql).await.unwrap().collect().await.unwrap()
    }

    fn count_of(batches: &[RecordBatch]) -> i64 {
        let mut total = 0;
        for batch in batches {
            let column = batch
                .column(0)
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap();
            total += column.value(0);
        }
        total
    }

    fn nano(value: i64) -> Expr {
        use datafusion::common::ScalarValue;
        Expr::Literal(
            ScalarValue::TimestampNanosecond(Some(value), Some(Arc::from("UTC"))),
            None,
        )
    }

    fn start_ge(value: i64) -> Expr {
        Expr::BinaryExpr(BinaryExpr::new(
            Box::new(col(spans::START_TIME)),
            Operator::GtEq,
            Box::new(nano(value)),
        ))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sql_reads_count_trace_id_and_a_projection() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        storage
            .append_spans(project(), spans_at(&[1, 2]))
            .await
            .unwrap();
        let query = storage.query(project()).unwrap();
        assert_eq!(query.target_partitions.get(), 2);
        let ctx = SessionContext::new();
        ctx.register_table("spans_t", Arc::clone(&query.spans))
            .unwrap();

        let count = collect(&ctx, "SELECT count(*) FROM spans_t").await;
        assert_eq!(count_of(&count), 2);

        let by_trace = collect(
            &ctx,
            "SELECT name FROM spans_t WHERE trace_id = arrow_cast(X'4bf92f3577b34da6a3ce929d0e0e4736', 'FixedSizeBinary(16)')",
        )
        .await;
        assert_eq!(by_trace.iter().map(RecordBatch::num_rows).sum::<usize>(), 2);

        let missing = collect(
            &ctx,
            "SELECT name FROM spans_t WHERE trace_id = arrow_cast(X'00000000000000000000000000000000', 'FixedSizeBinary(16)')",
        )
        .await;
        assert_eq!(missing.iter().map(RecordBatch::num_rows).sum::<usize>(), 0);

        let projected = collect(&ctx, "SELECT name, span_kind FROM spans_t").await;
        assert_eq!(projected[0].num_columns(), 2);
        let names = projected[0]
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(names.value(0), "root");
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn time_pruning_keeps_the_matching_days_only() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        storage
            .append_spans(project(), spans_at(&[1_000]))
            .await
            .unwrap();
        storage
            .append_spans(project(), spans_at(&[DAY_NS + 1_000]))
            .await
            .unwrap();
        let query = storage.query(project()).unwrap();
        let spans = query.spans.downcast_ref::<SpansProvider>().unwrap();
        let bound = start_ge(DAY_NS);
        assert_eq!(spans.candidate_batches(std::slice::from_ref(&bound)), 1);

        let or = bound.clone().or(lit(false));
        assert_eq!(spans.candidate_batches(&[or]), 2);
        let wrapped = Expr::BinaryExpr(BinaryExpr::new(
            Box::new(Expr::BinaryExpr(BinaryExpr::new(
                Box::new(col(spans::START_TIME)),
                Operator::Plus,
                Box::new(lit(0_i64)),
            ))),
            Operator::GtEq,
            Box::new(nano(DAY_NS)),
        ));
        assert_eq!(spans.candidate_batches(&[wrapped]), 2);
        let cast = Expr::Cast(Cast::new(Box::new(col(spans::START_TIME)), DataType::Utf8));
        assert_eq!(spans.candidate_batches(&[cast]), 2);

        let ctx = SessionContext::new();
        ctx.register_table("spans_t", Arc::clone(&query.spans))
            .unwrap();
        let later = collect(
            &ctx,
            "SELECT count(*) FROM spans_t WHERE start_time >= TIMESTAMP '1970-01-02'",
        )
        .await;
        assert_eq!(count_of(&later), 1);
        let unpruned = collect(
            &ctx,
            "SELECT count(*) FROM spans_t WHERE start_time >= TIMESTAMP '1970-01-02' OR name = 'missing'",
        )
        .await;
        assert_eq!(count_of(&unpruned), 1);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn each_project_sees_only_its_rows() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        storage
            .append_spans(project(), spans_at(&[1]))
            .await
            .unwrap();
        storage
            .append_spans(
                other_project(),
                with_project(&spans_at(&[2]), other_project()),
            )
            .await
            .unwrap();
        for (id, expected) in [(project(), 1), (other_project(), 1)] {
            let query = storage.query(id).unwrap();
            let ctx = SessionContext::new();
            ctx.register_table("spans_t", query.spans).unwrap();
            let count = collect(&ctx, "SELECT count(*) FROM spans_t").await;
            assert_eq!(count_of(&count), expected);
        }
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_query_keeps_the_snapshot_it_was_opened_on() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        let before = storage.query(project()).unwrap();
        storage
            .append_spans(project(), spans_at(&[1]))
            .await
            .unwrap();
        let after = storage.query(project()).unwrap();

        let old = SessionContext::new();
        old.register_table("spans_t", before.spans).unwrap();
        let new = SessionContext::new();
        new.register_table("spans_t", after.spans).unwrap();
        assert_eq!(
            count_of(&collect(&old, "SELECT count(*) FROM spans_t").await),
            0
        );
        assert_eq!(
            count_of(&collect(&new, "SELECT count(*) FROM spans_t").await),
            1
        );
        storage.shutdown().await.unwrap();
    }

    fn aged(dir: &Path) -> StorageConfig {
        let mut config = config_in(dir);
        config.flush.max_age = Duration::from_millis(100);
        config
    }

    fn session(query: &ProjectQuery) -> SessionContext {
        SessionContext::new_with_config_rt(SessionConfig::new(), Arc::clone(&query.runtime))
    }

    /// Run SQL off the test runtime.
    ///
    /// A flush pause blocks a Tokio worker. A query polled on that runtime can
    /// be queued behind the blocked worker and never start.
    async fn collect_off_runtime(query: &ProjectQuery, sql: &str) -> Vec<RecordBatch> {
        let runtime = Arc::clone(&query.runtime);
        let provider = Arc::clone(&query.spans);
        let sql = sql.to_owned();
        tokio::task::spawn_blocking(move || {
            let local = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            local.block_on(async move {
                let ctx = SessionContext::new_with_config_rt(SessionConfig::new(), runtime);
                ctx.register_table("spans_t", provider).unwrap();
                ctx.sql(&sql).await.unwrap().collect().await.unwrap()
            })
        })
        .await
        .unwrap()
    }

    fn provider(query: &ProjectQuery) -> &SpansProvider {
        query.spans.downcast_ref::<SpansProvider>().unwrap()
    }

    fn start_times(batches: &[RecordBatch]) -> Vec<i64> {
        let mut values = Vec::new();
        for batch in batches {
            let column = batch
                .column_by_name(spans::START_TIME)
                .unwrap()
                .as_any()
                .downcast_ref::<TimestampNanosecondArray>()
                .unwrap();
            for index in 0..column.len() {
                values.push(column.value(index));
            }
        }
        values.sort_unstable();
        values
    }

    fn plan_text(batches: &[RecordBatch]) -> String {
        let mut text = String::new();
        for batch in batches {
            for column in batch.columns() {
                let Some(values) = column.as_any().downcast_ref::<StringArray>() else {
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

    async fn wait_flushed(storage: &crate::Storage, files: usize) {
        let started = Instant::now();
        loop {
            let query = storage.query(project()).unwrap();
            let spans = provider(&query);
            if spans.candidate_files(&[]).len() == files && spans.candidate_batches(&[]) == 0 {
                return;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "flush did not publish {files} files"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    struct Release {
        hooks: crate::hooks::Hooks,
        point: HookPoint,
    }

    impl Drop for Release {
        fn drop(&mut self) {
            self.hooks.release(self.point);
        }
    }

    async fn wait_held(storage: &crate::Storage, point: HookPoint) {
        let started = Instant::now();
        while storage.hooks().waiting_at(point) == 0 {
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "flush did not reach {point:?}"
            );
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn rows_split_across_files_and_the_memtable_are_returned_once() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(aged(dir.path())).await.unwrap();
        storage
            .append_spans(project(), spans_at(&[10, 20]))
            .await
            .unwrap();
        wait_flushed(&storage, 1).await;
        let hooks = storage.hooks().clone();
        hooks.hold(HookPoint::FlushAfterFreeze);
        let _release = Release {
            hooks: hooks.clone(),
            point: HookPoint::FlushAfterFreeze,
        };
        storage
            .append_spans(project(), spans_at(&[30, 40]))
            .await
            .unwrap();

        let query = storage.query(project()).unwrap();
        let spans = provider(&query);
        assert_eq!(spans.candidate_files(&[]).len(), 1);
        assert!(spans.candidate_batches(&[]) >= 1);
        let times =
            start_times(&collect_off_runtime(&query, "SELECT start_time FROM spans_t").await);
        assert_eq!(times, vec![10, 20, 30, 40]);
        drop(_release);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_flush_never_shows_a_row_twice() {
        let points = [
            (HookPoint::FlushAfterFreeze, false),
            (HookPoint::FlushAfterStage, false),
            (HookPoint::FlushAfterWrite, false),
            (HookPoint::FlushAfterCommit, true),
            (HookPoint::FlushAfterTruncate, true),
        ];
        for (point, in_files) in points {
            let dir = tempfile::tempdir().unwrap();
            let storage = crate::Storage::open(aged(dir.path())).await.unwrap();
            let hooks = storage.hooks().clone();
            hooks.hold(point);
            let release = Release {
                hooks: hooks.clone(),
                point,
            };
            storage
                .append_spans(project(), spans_at(&[1, 2]))
                .await
                .unwrap();
            wait_held(&storage, point).await;
            let query = storage.query(project()).unwrap();
            let spans = provider(&query);
            let times =
                start_times(&collect_off_runtime(&query, "SELECT start_time FROM spans_t").await);
            assert_eq!(times, vec![1, 2], "{point:?}");
            if in_files {
                assert!(!spans.candidate_files(&[]).is_empty(), "{point:?}");
                assert_eq!(spans.candidate_batches(&[]), 0, "{point:?}");
            } else {
                assert!(spans.candidate_files(&[]).is_empty(), "{point:?}");
                assert!(spans.candidate_batches(&[]) >= 1, "{point:?}");
            }
            if point == HookPoint::FlushAfterWrite {
                hooks.hold(HookPoint::FlushAfterCommit);
                let commit = Release {
                    hooks: hooks.clone(),
                    point: HookPoint::FlushAfterCommit,
                };
                drop(release);
                wait_held(&storage, HookPoint::FlushAfterCommit).await;
                let query = storage.query(project()).unwrap();
                let spans = provider(&query);
                assert!(!spans.candidate_files(&[]).is_empty());
                assert_eq!(spans.candidate_batches(&[]), 0);
                let times = start_times(
                    &collect_off_runtime(&query, "SELECT start_time FROM spans_t").await,
                );
                assert_eq!(times, vec![1, 2]);
                drop(commit);
            } else {
                drop(release);
            }
            storage.shutdown().await.unwrap();
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn start_time_bounds_skip_files_from_other_days() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(aged(dir.path())).await.unwrap();
        storage
            .append_spans(project(), spans_at(&[1_000]))
            .await
            .unwrap();
        storage
            .append_spans(project(), spans_at(&[DAY_NS + 1_000]))
            .await
            .unwrap();
        wait_flushed(&storage, 2).await;
        let query = storage.query(project()).unwrap();
        let spans = provider(&query);
        let bound = start_ge(DAY_NS);
        assert_eq!(spans.candidate_files(std::slice::from_ref(&bound)).len(), 1);
        assert_eq!(spans.candidate_files(&[]).len(), 2);
        let ctx = session(&query);
        ctx.register_table("spans_t", Arc::clone(&query.spans))
            .unwrap();
        let later = collect(
            &ctx,
            "SELECT count(*) FROM spans_t WHERE start_time >= TIMESTAMP '1970-01-02'",
        )
        .await;
        assert_eq!(count_of(&later), 1);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_trace_id_equality_reads_the_flushed_file() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(aged(dir.path())).await.unwrap();
        storage
            .append_spans(project(), spans_at(&[1, 2]))
            .await
            .unwrap();
        wait_flushed(&storage, 1).await;
        let query = storage.query(project()).unwrap();
        let ctx = session(&query);
        ctx.register_table("spans_t", Arc::clone(&query.spans))
            .unwrap();
        let by_trace = collect(
            &ctx,
            "SELECT name FROM spans_t WHERE trace_id = arrow_cast(X'4bf92f3577b34da6a3ce929d0e0e4736', 'FixedSizeBinary(16)')",
        )
        .await;
        assert_eq!(by_trace.iter().map(RecordBatch::num_rows).sum::<usize>(), 2);
        let missing = collect(
            &ctx,
            "SELECT name FROM spans_t WHERE trace_id = arrow_cast(X'00000000000000000000000000000000', 'FixedSizeBinary(16)')",
        )
        .await;
        assert_eq!(missing.iter().map(RecordBatch::num_rows).sum::<usize>(), 0);
        let explained = collect(
            &ctx,
            "EXPLAIN ANALYZE SELECT name FROM spans_t WHERE trace_id = arrow_cast(X'4bf92f3577b34da6a3ce929d0e0e4736', 'FixedSizeBinary(16)')",
        )
        .await;
        let plan = plan_text(&explained);
        assert!(
            plan.contains("row_groups_pruned_bloom_filter=1 total → 1 matched"),
            "{plan}"
        );
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn files_of_another_project_are_not_scanned() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(aged(dir.path())).await.unwrap();
        storage
            .append_spans(project(), spans_at(&[1]))
            .await
            .unwrap();
        storage
            .append_spans(
                other_project(),
                with_project(&spans_at(&[2]), other_project()),
            )
            .await
            .unwrap();
        let started = Instant::now();
        loop {
            let left = storage.query(project()).unwrap();
            let right = storage.query(other_project()).unwrap();
            let left_spans = provider(&left);
            let right_spans = right.spans.downcast_ref::<SpansProvider>().unwrap();
            if left_spans.candidate_files(&[]).len() == 1
                && left_spans.candidate_batches(&[]) == 0
                && right_spans.candidate_files(&[]).len() == 1
                && right_spans.candidate_batches(&[]) == 0
            {
                assert!(
                    left_spans
                        .candidate_files(&[])
                        .iter()
                        .all(|file| file.project == project())
                );
                assert!(
                    right_spans
                        .candidate_files(&[])
                        .iter()
                        .all(|file| file.project == other_project())
                );
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "both projects did not flush"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        for (id, expected) in [(project(), 1_i64), (other_project(), 1)] {
            let query = storage.query(id).unwrap();
            let ctx = session(&query);
            ctx.register_table("spans_t", query.spans).unwrap();
            let count = collect(&ctx, "SELECT count(*) FROM spans_t").await;
            assert_eq!(count_of(&count), expected);
        }
        storage.shutdown().await.unwrap();
    }
}
