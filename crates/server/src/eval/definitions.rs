//! Live score definitions, cached per project (D-I-26), and the evaluator fit rule.
//!
//! Score writes read the cache; a definition write drops the project's entry at once.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use llmobs_core::ids::ProjectId;
use llmobs_eval::{JudgeOutput, OutputScore};

use crate::meta::score_definitions::{DefinitionRow, list_definitions};
use crate::meta::{MetaDb, MetaError};

/// How long a project's definitions are reused (D-I-26).
/// An entry is expired when `now - loaded_at >= TTL_NS`.
const TTL_NS: i64 = 10_000_000_000;

/// At most this many projects are cached; the map is cleared when it is full.
const CACHE_PROJECTS: usize = 1_024;

/// A project's live definitions with the time they were loaded.
type Entries = HashMap<ProjectId, (i64, Arc<Vec<DefinitionRow>>)>;

/// Live definitions per project, reused for 10 s and dropped on writes.
#[derive(Default)]
pub(crate) struct DefinitionCache {
    entries: Mutex<Entries>,
}

impl DefinitionCache {
    /// The live definitions of `project`, from the cache or from `meta.db`.
    ///
    /// # Errors
    ///
    /// Returns [`MetaError`] when the query fails or a stored value does not decode.
    pub(crate) async fn live(
        &self,
        meta: &MetaDb,
        project: ProjectId,
        now_ns: i64,
    ) -> Result<Arc<Vec<DefinitionRow>>, MetaError> {
        {
            let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((loaded_at, rows)) = entries.get(&project)
                && now_ns.saturating_sub(*loaded_at) < TTL_NS
            {
                return Ok(Arc::clone(rows));
            }
        }
        // The lock is released before the query, so no other request waits on the database.
        let rows = Arc::new(list_definitions(meta.reader(), project, false).await?);
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.len() >= CACHE_PROJECTS && !entries.contains_key(&project) {
            entries.clear();
        }
        entries.insert(project, (now_ns, Arc::clone(&rows)));
        Ok(rows)
    }

    /// Drops `project` after a definition write.
    pub(crate) fn forget(&self, project: ProjectId) {
        self.entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&project);
    }
}

impl fmt::Debug for DefinitionCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self
            .entries
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        formatter
            .debug_struct("DefinitionCache")
            .field("projects", &count)
            .finish()
    }
}

/// Whether an evaluator's output fits a live definition of its score name (spec 06,
/// D-I-26). The types must be equal. A numeric judge's range must lie inside the
/// definition's bounds, and a categorical judge's labels must all be definition
/// categories. Checks and webhooks (`judge_output` `None`) need only the equal types.
// evaluator saves use it from I2.6 on
#[allow(dead_code)]
pub(crate) fn fits_definition(
    output: &OutputScore,
    judge_output: Option<&JudgeOutput>,
    definition: &DefinitionRow,
) -> bool {
    if output.data_type != definition.data_type {
        return false;
    }
    match judge_output {
        Some(JudgeOutput::Numeric { min, max }) => {
            definition.min_value.is_none_or(|bound| *min >= bound)
                && definition.max_value.is_none_or(|bound| *max <= bound)
        }
        Some(JudgeOutput::Categorical { labels, .. }) => labels.iter().all(|label| {
            definition
                .categories
                .iter()
                .any(|category| category.label == *label)
        }),
        Some(JudgeOutput::Boolean) | None => true,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use llmobs_core::enums::ScoreDataType;
    use llmobs_core::ids::{OrgId, ProjectId};
    use llmobs_eval::{JudgeOutput, OutputScore};
    use uuid::Uuid;

    use super::{DefinitionCache, fits_definition};
    use crate::meta::orgs::insert_org;
    use crate::meta::projects::insert_project;
    use crate::meta::score_definitions::{
        DefinitionCategory, DefinitionFields, DefinitionRow, insert_definition,
    };
    use crate::meta::{MetaDb, MetaError};

    const NOW: i64 = 1_000;

    fn row(
        data_type: ScoreDataType,
        min_value: Option<f64>,
        max_value: Option<f64>,
        labels: &[&str],
    ) -> DefinitionRow {
        DefinitionRow {
            id: Uuid::nil(),
            project_id: ProjectId::from_uuid(Uuid::nil()),
            name: "relevance".to_owned(),
            data_type,
            categories: labels
                .iter()
                .map(|label| DefinitionCategory {
                    label: (*label).to_owned(),
                    value: None,
                })
                .collect(),
            min_value,
            max_value,
            description: None,
            archived_at: None,
            created_at: NOW,
            updated_at: NOW,
        }
    }

    fn output(data_type: ScoreDataType) -> OutputScore {
        OutputScore {
            name: "relevance".to_owned(),
            data_type,
            categories: None,
        }
    }

    #[test]
    fn a_different_type_never_fits() {
        let definition = row(ScoreDataType::Numeric, None, None, &[]);
        assert!(!fits_definition(
            &output(ScoreDataType::Boolean),
            None,
            &definition
        ));
        assert!(fits_definition(
            &output(ScoreDataType::Numeric),
            None,
            &definition
        ));
    }

    #[test]
    fn a_numeric_range_must_lie_inside_the_bounds() {
        let definition = row(ScoreDataType::Numeric, Some(0.0), Some(1.0), &[]);
        let fits = |min: f64, max: f64| {
            fits_definition(
                &output(ScoreDataType::Numeric),
                Some(&JudgeOutput::Numeric { min, max }),
                &definition,
            )
        };
        assert!(fits(0.0, 1.0));
        assert!(fits(0.2, 0.8));
        assert!(!fits(-0.1, 1.0));
        assert!(!fits(0.0, 1.1));

        // An unset bound constrains nothing on its side.
        let open = row(ScoreDataType::Numeric, None, Some(5.0), &[]);
        assert!(fits_definition(
            &output(ScoreDataType::Numeric),
            Some(&JudgeOutput::Numeric {
                min: -100.0,
                max: 5.0
            }),
            &open
        ));
        assert!(!fits_definition(
            &output(ScoreDataType::Numeric),
            Some(&JudgeOutput::Numeric { min: 0.0, max: 5.5 }),
            &open
        ));
    }

    #[test]
    fn every_label_must_be_a_definition_category() {
        let definition = row(ScoreDataType::Categorical, None, None, &["good", "bad"]);
        let judge = |labels: &[&str]| JudgeOutput::Categorical {
            labels: labels.iter().map(|label| (*label).to_owned()).collect(),
            label_values: BTreeMap::new(),
        };
        let categorical = output(ScoreDataType::Categorical);
        assert!(fits_definition(
            &categorical,
            Some(&judge(&["good", "bad"])),
            &definition
        ));
        assert!(fits_definition(
            &categorical,
            Some(&judge(&["bad"])),
            &definition
        ));
        assert!(!fits_definition(
            &categorical,
            Some(&judge(&["good", "meh"])),
            &definition
        ));
    }

    #[test]
    fn checks_and_webhooks_need_only_the_type() {
        let definition = row(ScoreDataType::Categorical, None, None, &["good"]);
        assert!(fits_definition(
            &output(ScoreDataType::Categorical),
            None,
            &definition
        ));
        assert!(!fits_definition(
            &output(ScoreDataType::Numeric),
            None,
            &definition
        ));
        let boolean = row(ScoreDataType::Boolean, None, None, &[]);
        assert!(fits_definition(
            &output(ScoreDataType::Boolean),
            Some(&JudgeOutput::Boolean),
            &boolean
        ));
    }

    async fn seed_project(db: &MetaDb) -> (OrgId, ProjectId) {
        db.write(|conn| {
            Box::pin(async move {
                let org = insert_org(conn, "Acme", "acme", NOW).await?;
                let project = insert_project(conn, org.id, "Support", "support", None, NOW).await?;
                Ok::<_, MetaError>((org.id, project.id))
            })
        })
        .await
        .unwrap()
    }

    async fn define(db: &MetaDb, project: ProjectId, name: &str, max: f64) {
        let fields = DefinitionFields {
            name: name.to_owned(),
            categories: Vec::new(),
            min_value: Some(0.0),
            max_value: Some(max),
            description: None,
            archived_at: None,
        };
        db.write(move |conn| {
            Box::pin(async move {
                insert_definition(
                    conn,
                    Uuid::now_v7(),
                    project,
                    ScoreDataType::Numeric,
                    fields,
                    NOW,
                )
                .await
            })
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn live_caches_until_ten_seconds_and_forget_drops_at_once() {
        let (_dir, db) = crate::meta::test_db().await;
        let (_org, project) = seed_project(&db).await;
        define(&db, project, "relevance", 1.0).await;
        let cache = DefinitionCache::default();
        let start = NOW;
        let at = |delta: Duration| start + i64::try_from(delta.as_nanos()).unwrap();

        let first = cache.live(&db, project, start).await.unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].max_value, Some(1.0));

        // A change in meta.db is not seen before the entry expires.
        define(&db, project, "tone", 5.0).await;
        let cached = cache
            .live(&db, project, at(Duration::from_millis(9_999)))
            .await
            .unwrap();
        assert_eq!(cached.len(), 1);

        // It is seen at exactly 10 s.
        let fresh = cache
            .live(&db, project, at(Duration::from_secs(10)))
            .await
            .unwrap();
        assert_eq!(fresh.len(), 2);

        // A forget drops the entry before its time is up.
        define(&db, project, "helpful", 3.0).await;
        cache.forget(project);
        let after_forget = cache
            .live(
                &db,
                project,
                at(Duration::from_secs(10) + Duration::from_millis(1)),
            )
            .await
            .unwrap();
        assert_eq!(after_forget.len(), 3);
    }
}
