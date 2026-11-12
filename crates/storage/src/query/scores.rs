//! `scores` table provider over one snapshot's files and memtable.
//!
//! The live row of a score is the latest version. `scan` builds that as a plan:
//! the files and the memtable, then the greatest `(updated_at, ingest_time)`.

use std::sync::Arc;

use async_trait::async_trait;
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::DataFusionError;
use datafusion::datasource::provider_as_source;
use datafusion::functions_window::expr_fn::row_number;
use datafusion::logical_expr::{
    Expr, ExprFunctionExt, LogicalPlanBuilder, SortExpr, TableProviderFilterPushDown, TableType,
    col, lit,
};
use datafusion::physical_plan::ExecutionPlan;
use llmobs_core::ids::ProjectId;
use llmobs_core::schema::{scores, scores_schema};

#[cfg(test)]
use super::overlapping_files;
use super::{ScanUnion, scan_union};
use crate::catalog::Catalog;
#[cfg(test)]
use crate::catalog::FileMeta;
use crate::memtable::MemSnapshot;
use crate::wal::Table;

/// Scores of one project at one read snapshot.
///
/// Every version of one score shares `created_at`; only `updated_at` changes.
/// Pruning on `created_at` therefore cannot separate a version from the others.
#[derive(Debug)]
pub(crate) struct ScoresProvider {
    project: ProjectId,
    snapshot: Arc<MemSnapshot>,
    catalog: Arc<Catalog>,
    target_partitions: usize,
}

impl ScoresProvider {
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

    /// Memtable batches the raw scan would read for `filters`.
    #[cfg(test)]
    pub(crate) fn candidate_batches(&self, filters: &[Expr]) -> usize {
        super::rows_for(self.project, &self.snapshot, scores::CREATED_AT, filters).len()
    }

    /// Files the raw scan would open for `filters`.
    #[cfg(test)]
    pub(crate) fn candidate_files(&self, filters: &[Expr]) -> Vec<Arc<FileMeta>> {
        overlapping_files(
            &self.catalog,
            Table::Scores,
            self.project,
            scores::CREATED_AT,
            filters,
            None,
        )
    }
}

/// Files and memtable of `scores`, before last-writer-wins. Not registered.
#[derive(Debug)]
struct RawScores {
    project: ProjectId,
    snapshot: Arc<MemSnapshot>,
    catalog: Arc<Catalog>,
    target_partitions: usize,
}

#[async_trait]
impl TableProvider for ScoresProvider {
    fn schema(&self) -> arrow::datatypes::SchemaRef {
        scores_schema()
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
        let raw = Arc::new(RawScores {
            project: self.project,
            snapshot: Arc::clone(&self.snapshot),
            catalog: Arc::clone(&self.catalog),
            target_partitions: self.target_partitions,
        });
        let window = row_number()
            .partition_by(vec![col(scores::SCORE_ID)])
            .order_by(vec![
                SortExpr::new(col(scores::UPDATED_AT), false, true),
                SortExpr::new(col(scores::INGEST_TIME), false, true),
            ])
            .build()?
            .alias("rn");
        let live = col("rn")
            .eq(lit(1_u64))
            .and(col(scores::DELETED).eq(lit(false)));
        let mut plan = LogicalPlanBuilder::scan_with_filters(
            "scores_raw",
            provider_as_source(raw),
            None,
            created_at_filters(filters),
        )?
        .window(vec![window])?
        .filter(live)?
        .project(output_columns(projection)?)?;
        if filters.is_empty()
            && let Some(fetch) = limit
        {
            plan = plan.limit(0, Some(fetch))?;
        }
        state.create_physical_plan(&plan.build()?).await
    }
}

/// A filter is passed to the raw scan only when every column it names is
/// `created_at`. Other columns can differ between versions, so they must not
/// drop a row before the window picks the latest one.
fn created_at_filters(filters: &[Expr]) -> Vec<Expr> {
    filters
        .iter()
        .filter(|expr| {
            let columns = expr.column_refs();
            !columns.is_empty()
                && columns
                    .iter()
                    .all(|column| column.name == scores::CREATED_AT)
        })
        .cloned()
        .collect()
}

fn output_columns(projection: Option<&Vec<usize>>) -> datafusion::error::Result<Vec<Expr>> {
    let schema = scores_schema();
    let fields = schema.fields();
    let expr_at = |index: usize| -> datafusion::error::Result<Expr> {
        let field = fields.get(index).ok_or_else(|| {
            DataFusionError::Internal(format!(
                "projection index {index} is outside the scores schema"
            ))
        })?;
        Ok(col(field.name()))
    };
    match projection {
        Some(indices) => indices.iter().copied().map(expr_at).collect(),
        None => Ok(fields.iter().map(|field| col(field.name())).collect()),
    }
}

#[async_trait]
impl TableProvider for RawScores {
    fn schema(&self) -> arrow::datatypes::SchemaRef {
        scores_schema()
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
            table: Table::Scores,
            time_column: scores::CREATED_AT,
            around: None,
            schema: scores_schema(),
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

    use arrow::array::{Array, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray};
    use datafusion::execution::context::SessionContext;
    use datafusion::logical_expr::Expr;
    use llmobs_core::batch::scores_to_record_batch;
    use llmobs_core::enums::ScoreSource;
    use llmobs_core::ids::{ProjectId, TraceId};
    use llmobs_core::schema::scores;
    use llmobs_core::score::{Score, ScoreTarget, ScoreValue};

    use crate::config::{
        FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig, WalConfig,
    };

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn config_in(dir: &Path) -> StorageConfig {
        StorageConfig {
            data_dir: dir.to_path_buf(),
            object_store: None,
            s3: S3Config::default(),
            wal: WalConfig::default(),
            flush: FlushConfig::default(),
            memtable: MemtableConfig::default(),
            query: QueryConfig::default(),
            anchor: Default::default(),
        }
    }

    /// Versions of one score must share `created_at`. The live view does not
    /// recompute it, and day pruning would drop a version that landed elsewhere.
    fn version(
        name: &str,
        created_at: i64,
        updated_at: i64,
        ingest_time: i64,
        deleted: bool,
    ) -> Score {
        Score {
            project_id: project(),
            score_id: "018f6b4e-7c1a-7c1a-8c1a-0000000000ab".parse().unwrap(),
            target: ScoreTarget::Trace {
                trace_id: "4bf92f3577b34da6a3ce929d0e0e4736"
                    .parse::<TraceId>()
                    .unwrap(),
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
            metadata: std::collections::BTreeMap::new(),
            created_at,
            updated_at,
            deleted,
            ingest_time,
        }
    }

    fn batch_of(rows: &[Score]) -> RecordBatch {
        assert!(
            rows.windows(2)
                .all(|pair| pair[0].created_at == pair[1].created_at),
            "versions of one score must share created_at"
        );
        scores_to_record_batch(rows).unwrap()
    }

    async fn names(storage: &crate::Storage) -> Vec<String> {
        let query = storage.query(project(), Default::default()).unwrap();
        let ctx = SessionContext::new();
        ctx.register_table("scores_t", query.scores).unwrap();
        let batches = ctx
            .sql("SELECT name, created_at FROM scores_t")
            .await
            .unwrap()
            .collect()
            .await
            .unwrap();
        let mut found = Vec::new();
        for batch in &batches {
            let name = batch
                .column_by_name(scores::NAME)
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let created = batch
                .column_by_name(scores::CREATED_AT)
                .unwrap()
                .as_any()
                .downcast_ref::<TimestampNanosecondArray>()
                .unwrap();
            for index in 0..batch.num_rows() {
                assert_eq!(created.value(index), 1_000);
                found.push(name.value(index).to_owned());
            }
        }
        found
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_latest_version_is_the_one_returned() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        let rows = [
            version("v1", 1_000, 10, 1, false),
            version("v2", 1_000, 20, 2, false),
            version("v3", 1_000, 30, 3, false),
        ];
        storage
            .append_scores(project(), batch_of(&rows))
            .await
            .unwrap();
        assert_eq!(names(&storage).await, vec!["v3".to_owned()]);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_deleted_latest_version_hides_the_score() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        let rows = [
            version("v1", 1_000, 10, 1, false),
            version("gone", 1_000, 40, 4, true),
        ];
        storage
            .append_scores(project(), batch_of(&rows))
            .await
            .unwrap();
        let query = storage.query(project(), Default::default()).unwrap();
        let ctx = SessionContext::new();
        ctx.register_table("scores_t", query.scores).unwrap();
        let count = ctx
            .sql("SELECT count(*) FROM scores_t")
            .await
            .unwrap()
            .collect()
            .await
            .unwrap();
        let column = count[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(column.value(0), 0);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ingest_time_breaks_a_tie_on_updated_at() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(config_in(dir.path())).await.unwrap();
        let rows = [
            version("early", 1_000, 20, 5, false),
            version("late", 1_000, 20, 9, false),
        ];
        storage
            .append_scores(project(), batch_of(&rows))
            .await
            .unwrap();
        assert_eq!(names(&storage).await, vec!["late".to_owned()]);
        storage.shutdown().await.unwrap();
    }

    fn aged(dir: &Path) -> StorageConfig {
        let mut config = config_in(dir);
        config.flush.max_age = std::time::Duration::from_millis(100);
        config
    }

    async fn query_off(storage: &crate::Storage, sql: &str) -> Vec<RecordBatch> {
        let query = storage.query(project(), Default::default()).unwrap();
        let runtime = std::sync::Arc::clone(&query.runtime);
        let provider = std::sync::Arc::clone(&query.scores);
        let sql = sql.to_owned();
        tokio::task::spawn_blocking(move || {
            let local = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            local.block_on(async move {
                let ctx = SessionContext::new_with_config_rt(
                    datafusion::execution::context::SessionConfig::new(),
                    runtime,
                );
                ctx.register_table("scores_t", provider).unwrap();
                ctx.sql(&sql).await.unwrap().collect().await.unwrap()
            })
        })
        .await
        .unwrap()
    }

    fn scores_of(query: &crate::ProjectQuery) -> &super::ScoresProvider {
        query
            .scores
            .downcast_ref::<super::ScoresProvider>()
            .unwrap()
    }

    async fn wait_flushed(storage: &crate::Storage, files: usize) {
        let started = std::time::Instant::now();
        loop {
            let query = storage.query(project(), Default::default()).unwrap();
            let provider = scores_of(&query);
            if provider.candidate_files(&[]).len() == files && provider.candidate_batches(&[]) == 0
            {
                return;
            }
            assert!(
                started.elapsed() < std::time::Duration::from_secs(5),
                "flush did not publish {files} score files"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    fn created_ge(value: i64) -> Expr {
        use datafusion::common::ScalarValue;
        use datafusion::logical_expr::{BinaryExpr, Operator, col};
        Expr::BinaryExpr(BinaryExpr::new(
            Box::new(col(scores::CREATED_AT)),
            Operator::GtEq,
            Box::new(Expr::Literal(
                ScalarValue::TimestampNanosecond(Some(value), Some(std::sync::Arc::from("UTC"))),
                None,
            )),
        ))
    }

    async fn live_names(storage: &crate::Storage) -> Vec<String> {
        let batches = query_off(storage, "SELECT name FROM scores_t").await;
        let mut found = Vec::new();
        for batch in &batches {
            let name = batch
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            for index in 0..batch.num_rows() {
                found.push(name.value(index).to_owned());
            }
        }
        found
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn versions_spread_across_files_and_the_memtable_resolve_to_the_latest() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(aged(dir.path())).await.unwrap();
        storage
            .append_scores(project(), batch_of(&[version("v1", 1_000, 10, 1, false)]))
            .await
            .unwrap();
        wait_flushed(&storage, 1).await;
        storage
            .append_scores(project(), batch_of(&[version("v2", 1_000, 20, 2, false)]))
            .await
            .unwrap();
        wait_flushed(&storage, 2).await;

        let hooks = storage.hooks().clone();
        hooks.hold(crate::hooks::HookPoint::FlushAfterFreeze);
        storage
            .append_scores(project(), batch_of(&[version("v3", 1_000, 30, 3, false)]))
            .await
            .unwrap();

        let query = storage.query(project(), Default::default()).unwrap();
        let provider = scores_of(&query);
        assert_eq!(provider.candidate_files(&[]).len(), 2);
        assert!(provider.candidate_batches(&[]) >= 1);
        let future = created_ge(super::super::DAY_NS);
        assert!(
            provider
                .candidate_files(std::slice::from_ref(&future))
                .is_empty()
        );
        assert_eq!(live_names(&storage).await, vec!["v3".to_owned()]);

        let projected = query_off(&storage, "SELECT name FROM scores_t").await;
        assert_eq!(projected[0].num_columns(), 1);
        let hidden = query_off(
            &storage,
            "SELECT count(*) FROM scores_t WHERE created_at >= TIMESTAMP '1970-01-02'",
        )
        .await;
        let count = hidden[0]
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(count.value(0), 0);

        hooks.release(crate::hooks::HookPoint::FlushAfterFreeze);
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_deleted_version_in_a_later_file_hides_the_score() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(aged(dir.path())).await.unwrap();
        storage
            .append_scores(project(), batch_of(&[version("v1", 1_000, 10, 1, false)]))
            .await
            .unwrap();
        wait_flushed(&storage, 1).await;
        storage
            .append_scores(project(), batch_of(&[version("gone", 1_000, 40, 4, true)]))
            .await
            .unwrap();
        wait_flushed(&storage, 2).await;
        assert!(live_names(&storage).await.is_empty());
        storage.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn ingest_time_breaks_a_tie_across_a_file_and_the_memtable() {
        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(aged(dir.path())).await.unwrap();
        storage
            .append_scores(
                project(),
                batch_of(&[version("early", 1_000, 20, 5, false)]),
            )
            .await
            .unwrap();
        wait_flushed(&storage, 1).await;
        let hooks = storage.hooks().clone();
        hooks.hold(crate::hooks::HookPoint::FlushAfterFreeze);
        storage
            .append_scores(project(), batch_of(&[version("late", 1_000, 20, 9, false)]))
            .await
            .unwrap();
        assert_eq!(live_names(&storage).await, vec!["late".to_owned()]);
        hooks.release(crate::hooks::HookPoint::FlushAfterFreeze);
        storage.shutdown().await.unwrap();
    }

    /// D154 limits `around` to `spans` and `traces`. A `trace_id` lookup on
    /// scores must still see a file whose `created_at` is far from the hint.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn around_does_not_prune_scores_files() {
        use datafusion::common::ScalarValue;
        use datafusion::logical_expr::{BinaryExpr, Operator, col};

        let dir = tempfile::tempdir().unwrap();
        let storage = crate::Storage::open(aged(dir.path())).await.unwrap();
        storage
            .append_scores(project(), batch_of(&[version("v1", 1_000, 10, 1, false)]))
            .await
            .unwrap();
        wait_flushed(&storage, 1).await;

        let trace = "4bf92f3577b34da6a3ce929d0e0e4736"
            .parse::<TraceId>()
            .unwrap();
        let filter = Expr::BinaryExpr(BinaryExpr::new(
            Box::new(col(scores::TRACE_ID)),
            Operator::Eq,
            Box::new(Expr::Literal(
                ScalarValue::FixedSizeBinary(16, Some(trace.as_bytes().to_vec())),
                None,
            )),
        ));
        let around = 3 * 86_400_000_000_000;
        let query = storage
            .query(
                project(),
                crate::QueryOptions {
                    around: Some(around),
                },
            )
            .unwrap();
        assert_eq!(scores_of(&query).candidate_files(&[filter]).len(), 1);
        storage.shutdown().await.unwrap();
    }
}
