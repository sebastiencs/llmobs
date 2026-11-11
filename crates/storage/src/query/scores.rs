//! `scores` table provider over the memtable snapshot.
//!
//! The live row of a score is the latest version. M1 computes that eagerly
//! inside `scan`. M2 replaces this step with a plan over files and the memtable.

use std::sync::Arc;

use arrow::array::{Array, BooleanArray, RecordBatch, StringArray, UInt32Array};
use arrow::compute::{
    SortColumn, SortOptions, concat_batches, filter_record_batch, lexsort_to_indices,
    take_record_batch,
};
use async_trait::async_trait;
use datafusion::catalog::{Session, TableProvider};
use datafusion::common::DataFusionError;
use datafusion::logical_expr::{Expr, TableProviderFilterPushDown, TableType};
use datafusion::physical_plan::ExecutionPlan;
use llmobs_core::ids::ProjectId;
use llmobs_core::schema::{scores, scores_schema};

use super::{memory_plan, rows_for};
use crate::memtable::MemSnapshot;

/// Scores of one project at one read snapshot.
///
/// Every version of one score shares `created_at`; only `updated_at` changes.
/// Pruning on `created_at` therefore cannot separate a version from the others.
#[derive(Debug)]
pub(crate) struct ScoresProvider {
    project: ProjectId,
    snapshot: Arc<MemSnapshot>,
    target_partitions: usize,
}

impl ScoresProvider {
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
        _state: &dyn Session,
        projection: Option<&Vec<usize>>,
        filters: &[Expr],
        limit: Option<usize>,
    ) -> datafusion::error::Result<Arc<dyn ExecutionPlan>> {
        let selected = rows_for(self.project, &self.snapshot, scores::CREATED_AT, filters);
        let live = latest_version(&selected)?;
        let batches = if live.num_rows() == 0 {
            Vec::new()
        } else {
            vec![live]
        };
        memory_plan(
            batches,
            scores_schema(),
            projection,
            filters,
            limit,
            self.target_partitions,
        )
    }
}

/// First row of each `score_id` after sorting by `updated_at` and `ingest_time`
/// descending, then the rows that are not deleted.
///
/// Versions of one score share `created_at`, so this concat is the whole score
/// whenever the `created_at` day was selected.
fn latest_version(batches: &[RecordBatch]) -> datafusion::error::Result<RecordBatch> {
    let schema = scores_schema();
    if batches.is_empty() {
        return Ok(RecordBatch::new_empty(schema));
    }
    let combined = concat_batches(&schema, batches).map_err(DataFusionError::from)?;
    if combined.num_rows() == 0 {
        return Ok(combined);
    }
    let indices = lexsort_to_indices(
        &[
            sort_column(&combined, scores::SCORE_ID, false)?,
            sort_column(&combined, scores::UPDATED_AT, true)?,
            sort_column(&combined, scores::INGEST_TIME, true)?,
        ],
        None,
    )
    .map_err(DataFusionError::from)?;
    let ids = string_column(&combined, scores::SCORE_ID)?;
    let mut keep = Vec::new();
    let mut previous: Option<u32> = None;
    for index in indices.values() {
        if let Some(prev) = previous
            && same_score(ids, prev, *index)
        {
            continue;
        }
        previous = Some(*index);
        keep.push(*index);
    }
    let taken = take_rows(&combined, &keep)?;
    let deleted = bool_column(&taken, scores::DELETED)?;
    let live = arrow::compute::not(deleted).map_err(DataFusionError::from)?;
    filter_record_batch(&taken, &live).map_err(DataFusionError::from)
}

fn sort_column(
    batch: &RecordBatch,
    name: &'static str,
    descending: bool,
) -> datafusion::error::Result<SortColumn> {
    Ok(SortColumn {
        values: Arc::clone(column(batch, name)?),
        options: Some(SortOptions {
            descending,
            nulls_first: !descending,
        }),
    })
}

fn column<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> datafusion::error::Result<&'a Arc<dyn Array>> {
    batch
        .column_by_name(name)
        .ok_or_else(|| DataFusionError::Internal(format!("scores batch is missing column {name}")))
}

fn string_column<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> datafusion::error::Result<&'a StringArray> {
    column(batch, name)?
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| DataFusionError::Internal(format!("scores column {name} is not utf8")))
}

fn bool_column<'a>(
    batch: &'a RecordBatch,
    name: &'static str,
) -> datafusion::error::Result<&'a BooleanArray> {
    column(batch, name)?
        .as_any()
        .downcast_ref::<BooleanArray>()
        .ok_or_else(|| DataFusionError::Internal(format!("scores column {name} is not boolean")))
}

fn same_score(ids: &StringArray, left: u32, right: u32) -> bool {
    let Some(left) = usize::try_from(left).ok() else {
        return false;
    };
    let Some(right) = usize::try_from(right).ok() else {
        return false;
    };
    if ids.is_null(left) || ids.is_null(right) {
        return false;
    }
    ids.value(left) == ids.value(right)
}

fn take_rows(batch: &RecordBatch, indices: &[u32]) -> datafusion::error::Result<RecordBatch> {
    take_record_batch(batch, &UInt32Array::from(indices.to_vec())).map_err(DataFusionError::from)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use arrow::array::{Array, Int64Array, RecordBatch, StringArray, TimestampNanosecondArray};
    use datafusion::execution::context::SessionContext;
    use llmobs_core::batch::scores_to_record_batch;
    use llmobs_core::enums::ScoreSource;
    use llmobs_core::ids::{ProjectId, TraceId};
    use llmobs_core::schema::scores;
    use llmobs_core::score::{Score, ScoreTarget, ScoreValue};

    use crate::config::{MemtableConfig, QueryConfig, StorageConfig, WalConfig};

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn config_in(dir: &Path) -> StorageConfig {
        StorageConfig {
            data_dir: dir.to_path_buf(),
            wal: WalConfig::default(),
            memtable: MemtableConfig::default(),
            query: QueryConfig::default(),
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
        let query = storage.query(project()).unwrap();
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
        let query = storage.query(project()).unwrap();
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
}
