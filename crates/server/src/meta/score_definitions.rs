//! `score_definitions` rows (spec 04, D-I-26). A name is unique among live (non-archived) rows.

use llmobs_core::enums::ScoreDataType;
use llmobs_core::ids::ProjectId;
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::{MetaError, unique_or};

/// A project holds at most this many live definitions.
const PROJECT_CAP: i64 = 1_000;

/// One stored definition.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DefinitionRow {
    /// UUIDv7.
    pub(crate) id: Uuid,
    /// Owning project.
    pub(crate) project_id: ProjectId,
    /// Display name. Unique among live definitions of the project.
    pub(crate) name: String,
    /// Immutable kind of value.
    pub(crate) data_type: ScoreDataType,
    /// The labels of a categorical score; empty otherwise.
    pub(crate) categories: Vec<DefinitionCategory>,
    /// Lower bound of a numeric score.
    pub(crate) min_value: Option<f64>,
    /// Upper bound of a numeric score.
    pub(crate) max_value: Option<f64>,
    /// Free text.
    pub(crate) description: Option<String>,
    /// Set when archived; `None` for a live definition.
    pub(crate) archived_at: Option<i64>,
    /// Creation time, nanoseconds.
    pub(crate) created_at: i64,
    /// Last change time, nanoseconds.
    pub(crate) updated_at: i64,
}

/// One label of a categorical score, with an optional numeric mapping.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DefinitionCategory {
    /// The label.
    pub(crate) label: String,
    /// The numeric value the label maps to, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) value: Option<f64>,
}

/// The editable columns. Insert takes them with the id and project; update replaces them all.
/// `data_type` is immutable and is not here.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DefinitionFields {
    /// Display name.
    pub(crate) name: String,
    /// The labels of a categorical score; empty otherwise.
    pub(crate) categories: Vec<DefinitionCategory>,
    /// Lower bound of a numeric score.
    pub(crate) min_value: Option<f64>,
    /// Upper bound of a numeric score.
    pub(crate) max_value: Option<f64>,
    /// Free text.
    pub(crate) description: Option<String>,
    /// Set to archive, `None` to restore.
    pub(crate) archived_at: Option<i64>,
}

/// Every definition of `project`, ordered by name then id. `include_archived = false`
/// returns the live definitions only.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn list_definitions(
    pool: &SqlitePool,
    project: ProjectId,
    include_archived: bool,
) -> Result<Vec<DefinitionRow>, MetaError> {
    let records: Vec<DefinitionRecord> = if include_archived {
        sqlx::query_as(
            "SELECT id, project_id, name, data_type, categories, min_value, max_value, \
             description, archived_at, created_at, updated_at \
             FROM score_definitions WHERE project_id = ? ORDER BY name, id",
        )
        .bind(project.to_string())
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as(
            "SELECT id, project_id, name, data_type, categories, min_value, max_value, \
             description, archived_at, created_at, updated_at \
             FROM score_definitions WHERE project_id = ? AND archived_at IS NULL \
             ORDER BY name, id",
        )
        .bind(project.to_string())
        .fetch_all(pool)
        .await?
    };
    records.into_iter().map(row_from).collect()
}

/// The definition `id` in `project`, archived or not.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn get_definition(
    pool: &SqlitePool,
    project: ProjectId,
    id: Uuid,
) -> Result<Option<DefinitionRow>, MetaError> {
    let record: Option<DefinitionRecord> = sqlx::query_as(
        "SELECT id, project_id, name, data_type, categories, min_value, max_value, \
         description, archived_at, created_at, updated_at \
         FROM score_definitions WHERE project_id = ? AND id = ?",
    )
    .bind(project.to_string())
    .bind(id.to_string())
    .fetch_optional(pool)
    .await?;
    record.map(row_from).transpose()
}

/// The same read on the writer, inside a transaction.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn get_definition_tx(
    conn: &mut SqliteConnection,
    project: ProjectId,
    id: Uuid,
) -> Result<Option<DefinitionRow>, MetaError> {
    let record: Option<DefinitionRecord> = sqlx::query_as(
        "SELECT id, project_id, name, data_type, categories, min_value, max_value, \
         description, archived_at, created_at, updated_at \
         FROM score_definitions WHERE project_id = ? AND id = ?",
    )
    .bind(project.to_string())
    .bind(id.to_string())
    .fetch_optional(&mut *conn)
    .await?;
    record.map(row_from).transpose()
}

/// Inserts a definition. `id` is chosen by the caller.
///
/// A live definition past the project's 1 000th is [`MetaError::Conflict`]
/// `"a project has at most 1000 live score definitions"`. A live duplicate name
/// is `"a score definition with this name already exists"`. An archived row
/// does not count toward the cap, and may share a name with a live one.
///
/// # Errors
///
/// Returns [`MetaError`] when the insert fails or a stored value does not decode.
pub(crate) async fn insert_definition(
    conn: &mut SqliteConnection,
    id: Uuid,
    project: ProjectId,
    data_type: ScoreDataType,
    fields: DefinitionFields,
    now: i64,
) -> Result<DefinitionRow, MetaError> {
    if fields.archived_at.is_none() {
        let live: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM score_definitions WHERE project_id = ? AND archived_at IS NULL",
        )
        .bind(project.to_string())
        .fetch_one(&mut *conn)
        .await?;
        if live >= PROJECT_CAP {
            return Err(MetaError::Conflict(
                "a project has at most 1000 live score definitions",
            ));
        }
    }
    let categories = categories_json(&fields.categories)?;
    sqlx::query(
        "INSERT INTO score_definitions \
         (id, project_id, name, data_type, categories, min_value, max_value, description, \
         archived_at, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(project.to_string())
    .bind(fields.name)
    .bind(data_type.as_str())
    .bind(categories)
    .bind(fields.min_value)
    .bind(fields.max_value)
    .bind(fields.description)
    .bind(fields.archived_at)
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, "a score definition with this name already exists"))?;
    get_definition_tx(conn, project, id)
        .await?
        .ok_or(MetaError::Corrupt {
            table: "score_definitions",
            column: "id",
        })
}

/// Replaces every editable column and bumps `updated_at`. `None` means the
/// definition is missing or belongs to another project. `data_type` is kept.
///
/// Restoring a row (`archived_at` cleared) checks the live cap, excluding the
/// row itself. A live row never exceeds the cap, so an ordinary edit is never
/// refused by it.
///
/// # Errors
///
/// Returns [`MetaError`] when the update fails or a stored value does not decode.
pub(crate) async fn update_definition(
    conn: &mut SqliteConnection,
    project: ProjectId,
    id: Uuid,
    fields: DefinitionFields,
    now: i64,
) -> Result<Option<DefinitionRow>, MetaError> {
    if fields.archived_at.is_none() {
        let others: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM score_definitions \
             WHERE project_id = ? AND archived_at IS NULL AND id <> ?",
        )
        .bind(project.to_string())
        .bind(id.to_string())
        .fetch_one(&mut *conn)
        .await?;
        if others >= PROJECT_CAP {
            return Err(MetaError::Conflict(
                "a project has at most 1000 live score definitions",
            ));
        }
    }
    let categories = categories_json(&fields.categories)?;
    let result = sqlx::query(
        "UPDATE score_definitions SET name = ?, categories = ?, min_value = ?, max_value = ?, \
         description = ?, archived_at = ?, updated_at = ? \
         WHERE project_id = ? AND id = ?",
    )
    .bind(fields.name)
    .bind(categories)
    .bind(fields.min_value)
    .bind(fields.max_value)
    .bind(fields.description)
    .bind(fields.archived_at)
    .bind(now)
    .bind(project.to_string())
    .bind(id.to_string())
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, "a score definition with this name already exists"))?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_definition_tx(conn, project, id)
        .await?
        .ok_or(MetaError::Corrupt {
            table: "score_definitions",
            column: "id",
        })
        .map(Some)
}

#[derive(sqlx::FromRow)]
struct DefinitionRecord {
    id: String,
    project_id: String,
    name: String,
    data_type: String,
    categories: Option<String>,
    min_value: Option<f64>,
    max_value: Option<f64>,
    description: Option<String>,
    archived_at: Option<i64>,
    created_at: i64,
    updated_at: i64,
}

fn row_from(record: DefinitionRecord) -> Result<DefinitionRow, MetaError> {
    Ok(DefinitionRow {
        id: parse_uuid(&record.id, "id")?,
        project_id: ProjectId::from_uuid(parse_uuid(&record.project_id, "project_id")?),
        name: record.name,
        data_type: ScoreDataType::parse(&record.data_type).map_err(|_| corrupt("data_type"))?,
        categories: match record.categories {
            None => Vec::new(),
            Some(text) => serde_json::from_str(&text).map_err(|_| corrupt("categories"))?,
        },
        min_value: record.min_value,
        max_value: record.max_value,
        description: record.description,
        archived_at: record.archived_at,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

fn corrupt(column: &'static str) -> MetaError {
    MetaError::Corrupt {
        table: "score_definitions",
        column,
    }
}

fn parse_uuid(value: &str, column: &'static str) -> Result<Uuid, MetaError> {
    value.parse().map_err(|_| corrupt(column))
}

/// `NULL` for no categories, so a numeric definition stores nothing.
fn categories_json(categories: &[DefinitionCategory]) -> Result<Option<String>, MetaError> {
    if categories.is_empty() {
        return Ok(None);
    }
    serde_json::to_string(categories)
        .map(Some)
        .map_err(|_| corrupt("categories"))
}

#[cfg(test)]
mod tests {
    use llmobs_core::enums::ScoreDataType;
    use llmobs_core::ids::{OrgId, ProjectId};
    use uuid::Uuid;

    use super::{DefinitionCategory, DefinitionFields, DefinitionRow, PROJECT_CAP};
    use crate::meta::orgs::insert_org;
    use crate::meta::projects::insert_project;
    use crate::meta::score_definitions::{
        get_definition, insert_definition, list_definitions, update_definition,
    };
    use crate::meta::{MetaDb, MetaError};

    const NOW: i64 = 1_000;

    async fn org(db: &MetaDb) -> OrgId {
        db.write(|conn| Box::pin(async move { insert_org(conn, "Acme", "acme", NOW).await }))
            .await
            .unwrap()
            .id
    }

    async fn project(db: &MetaDb, org: OrgId) -> ProjectId {
        db.write(move |conn| {
            Box::pin(async move {
                insert_project(conn, org, "Support", "support", None, NOW)
                    .await
                    .map(|row| row.id)
            })
        })
        .await
        .unwrap()
    }

    fn numeric(name: &str) -> DefinitionFields {
        DefinitionFields {
            name: name.to_owned(),
            categories: Vec::new(),
            min_value: Some(0.0),
            max_value: Some(1.0),
            description: None,
            archived_at: None,
        }
    }

    fn categorical(name: &str, archived_at: Option<i64>) -> DefinitionFields {
        DefinitionFields {
            name: name.to_owned(),
            categories: vec![
                DefinitionCategory {
                    label: "good".to_owned(),
                    value: Some(1.0),
                },
                DefinitionCategory {
                    label: "bad".to_owned(),
                    value: None,
                },
            ],
            min_value: None,
            max_value: None,
            description: Some("tone".to_owned()),
            archived_at,
        }
    }

    async fn insert(
        db: &MetaDb,
        project: ProjectId,
        data_type: ScoreDataType,
        fields: DefinitionFields,
    ) -> Result<DefinitionRow, MetaError> {
        db.write(move |conn| {
            Box::pin(async move {
                insert_definition(conn, Uuid::now_v7(), project, data_type, fields, NOW).await
            })
        })
        .await
    }

    #[tokio::test]
    async fn round_trip_with_and_without_categories() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;

        let plain = insert(&db, support, ScoreDataType::Numeric, numeric("relevance"))
            .await
            .unwrap();
        let tone = insert(
            &db,
            support,
            ScoreDataType::Categorical,
            categorical("tone", None),
        )
        .await
        .unwrap();

        let expected_plain = DefinitionRow {
            id: plain.id,
            project_id: support,
            name: "relevance".to_owned(),
            data_type: ScoreDataType::Numeric,
            categories: Vec::new(),
            min_value: Some(0.0),
            max_value: Some(1.0),
            description: None,
            archived_at: None,
            created_at: NOW,
            updated_at: NOW,
        };
        assert_eq!(plain, expected_plain);
        assert_eq!(
            get_definition(db.reader(), support, plain.id)
                .await
                .unwrap()
                .unwrap(),
            expected_plain
        );

        let read = get_definition(db.reader(), support, tone.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read.data_type, ScoreDataType::Categorical);
        assert_eq!(read.categories, categorical("tone", None).categories);
        assert_eq!(read.description.as_deref(), Some("tone"));
        assert_eq!(read.min_value, None);
    }

    #[tokio::test]
    async fn duplicate_live_name_is_a_conflict() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;
        insert(&db, support, ScoreDataType::Numeric, numeric("same"))
            .await
            .unwrap();
        let error = insert(&db, support, ScoreDataType::Numeric, numeric("same"))
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                MetaError::Conflict("a score definition with this name already exists")
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn an_archived_duplicate_is_allowed() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;
        insert(&db, support, ScoreDataType::Numeric, numeric("same"))
            .await
            .unwrap();
        let archived = DefinitionFields {
            archived_at: Some(NOW),
            ..numeric("same")
        };
        insert(&db, support, ScoreDataType::Numeric, archived)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn restoring_into_a_clash_is_a_conflict() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;
        insert(&db, support, ScoreDataType::Numeric, numeric("same"))
            .await
            .unwrap();
        let archived = insert(
            &db,
            support,
            ScoreDataType::Numeric,
            DefinitionFields {
                archived_at: Some(NOW),
                ..numeric("same")
            },
        )
        .await
        .unwrap();

        let id = archived.id;
        let error = db
            .write(move |conn| {
                Box::pin(async move {
                    update_definition(conn, support, id, numeric("same"), NOW + 1).await
                })
            })
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                MetaError::Conflict("a score definition with this name already exists")
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn include_archived_filters() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;
        insert(&db, support, ScoreDataType::Numeric, numeric("live"))
            .await
            .unwrap();
        insert(
            &db,
            support,
            ScoreDataType::Numeric,
            DefinitionFields {
                archived_at: Some(NOW),
                ..numeric("old")
            },
        )
        .await
        .unwrap();

        let live: Vec<String> = list_definitions(db.reader(), support, false)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.name)
            .collect();
        assert_eq!(live, ["live"]);
        let all: Vec<String> = list_definitions(db.reader(), support, true)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.name)
            .collect();
        assert_eq!(all, ["live", "old"]);
    }

    #[tokio::test]
    async fn update_keeps_the_data_type() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;
        let row = insert(
            &db,
            support,
            ScoreDataType::Categorical,
            categorical("tone", None),
        )
        .await
        .unwrap();
        let id = row.id;
        let updated = db
            .write(move |conn| {
                Box::pin(async move {
                    update_definition(
                        conn,
                        support,
                        id,
                        DefinitionFields {
                            description: None,
                            ..categorical("tone-2", None)
                        },
                        NOW + 5,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.name, "tone-2");
        assert_eq!(updated.data_type, ScoreDataType::Categorical);
        assert_eq!(updated.description, None);
        assert_eq!(updated.updated_at, NOW + 5);
        assert_eq!(updated.created_at, NOW);
    }

    #[tokio::test]
    async fn the_live_cap_is_1000() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;
        db.write(move |conn| {
            Box::pin(async move {
                for index in 0..PROJECT_CAP {
                    insert_definition(
                        conn,
                        Uuid::now_v7(),
                        support,
                        ScoreDataType::Numeric,
                        numeric(&format!("s-{index:04}")),
                        NOW,
                    )
                    .await?;
                }
                Ok(())
            })
        })
        .await
        .unwrap();
        let error = insert(&db, support, ScoreDataType::Numeric, numeric("one-more"))
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                MetaError::Conflict("a project has at most 1000 live score definitions")
            ),
            "{error}"
        );
    }
}
