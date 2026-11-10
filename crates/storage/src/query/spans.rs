//! `spans` table provider over the memtable snapshot.

use std::sync::Arc;

use async_trait::async_trait;
use datafusion::catalog::{Session, TableProvider};
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::physical_plan::ExecutionPlan;
use llmobs_core::ids::ProjectId;
use llmobs_core::schema::{spans, spans_schema};

use super::{memory_plan, rows_for};
use crate::memtable::MemSnapshot;

/// Spans of one project at one read snapshot.
#[derive(Debug)]
pub(crate) struct SpansProvider {
    project: ProjectId,
    snapshot: Arc<MemSnapshot>,
    target_partitions: usize,
}

impl SpansProvider {
    pub(crate) fn new(
        project: ProjectId,
        snapshot: Arc<MemSnapshot>,
        target_partitions: usize,
    ) -> Self {
        Self {
            project,
            snapshot,
            target_partitions,
        }
    }

    /// Memtable batches the scan would read for `filters`, before distribution.
    #[cfg(test)]
    pub(crate) fn candidate_batches(&self, filters: &[Expr]) -> usize {
        rows_for(self.project, &self.snapshot, spans::START_TIME, filters).len()
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
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> datafusion::error::Result<Arc<dyn ExecutionPlan>> {
        let batches = rows_for(self.project, &self.snapshot, spans::START_TIME, filters);
        memory_plan(
            batches,
            spans_schema(),
            projection,
            filters,
            limit,
            self.target_partitions,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use arrow::array::{Array, Int64Array, RecordBatch, StringArray};
    use arrow::datatypes::DataType;
    use datafusion::execution::context::SessionContext;
    use datafusion::logical_expr::{BinaryExpr, Cast, Expr, Operator, col, lit};
    use llmobs_core::ids::ProjectId;
    use llmobs_core::schema::spans;

    use super::super::DAY_NS;
    use super::*;
    use crate::config::{MemtableConfig, QueryConfig, StorageConfig, WalConfig};
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
            wal: WalConfig::default(),
            memtable: MemtableConfig::default(),
            query: QueryConfig {
                target_partitions: 2,
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
}
