//! `evaluators` rows (spec 04, D-I-19). `secret_enc` is opaque bytes here.

use std::fmt;

use llmobs_core::ids::ProjectId;
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::{MetaError, unique_or};

/// A project holds at most this many evaluators.
const PROJECT_CAP: i64 = 500;

/// One stored evaluator. `Debug` is written by hand and prints the lengths of `config`,
/// `output_score` and `secret_enc` only, because a judge config holds the prompt template.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct EvaluatorRow {
    /// UUIDv7, chosen by the caller: the AAD of `secret_enc` needs it before the insert.
    pub(crate) id: Uuid,
    /// Owning project.
    pub(crate) project_id: ProjectId,
    /// Display name, unique in the project.
    pub(crate) name: String,
    /// Stored kind name (`EvaluatorKind::as_str`). Immutable.
    pub(crate) kind: String,
    /// JSON text of the kind's configuration, without any sealed value.
    pub(crate) config: String,
    /// JSON text of the output score.
    pub(crate) output_score: String,
    /// The judge's connection, when the evaluator is a judge.
    pub(crate) connection_id: Option<Uuid>,
    /// Sealed webhook document `{signing_secret, url, headers}`.
    pub(crate) secret_enc: Option<Vec<u8>>,
    /// Starts at 1 and increases on every change to config, output or sealed values.
    pub(crate) version: u32,
    /// User who created the evaluator.
    pub(crate) created_by: Option<Uuid>,
    /// Creation time, nanoseconds.
    pub(crate) created_at: i64,
    /// Last change time, nanoseconds.
    pub(crate) updated_at: i64,
}

impl fmt::Debug for EvaluatorRow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvaluatorRow")
            .field("id", &self.id)
            .field("project_id", &self.project_id)
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("config_bytes", &self.config.len())
            .field("output_score_bytes", &self.output_score.len())
            .field("connection_id", &self.connection_id)
            .field("secret_enc", &self.secret_enc.as_deref().map(<[u8]>::len))
            .field("version", &self.version)
            .field("created_by", &self.created_by)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

/// The editable columns. Insert takes them with the id, project and author; update replaces them all.
pub(crate) struct EvaluatorFields {
    /// Display name, unique in the project.
    pub(crate) name: String,
    /// JSON text of the kind's configuration, without any sealed value.
    pub(crate) config: String,
    /// JSON text of the output score.
    pub(crate) output_score: String,
    /// The judge's connection, when the evaluator is a judge.
    pub(crate) connection_id: Option<Uuid>,
    /// Sealed webhook document `{signing_secret, url, headers}`.
    pub(crate) secret_enc: Option<Vec<u8>>,
    /// The version to store.
    pub(crate) version: u32,
}

/// Every evaluator of `project`, ordered by name then id.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn list_evaluators(
    pool: &SqlitePool,
    project: ProjectId,
) -> Result<Vec<EvaluatorRow>, MetaError> {
    let records: Vec<EvaluatorRecord> = sqlx::query_as(
        "SELECT id, project_id, name, kind, config, output_score, connection_id, secret_enc, \
         version, created_by, created_at, updated_at \
         FROM evaluators WHERE project_id = ? ORDER BY name, id",
    )
    .bind(project.to_string())
    .fetch_all(pool)
    .await?;
    records.into_iter().map(row_from).collect()
}

/// The evaluator `id` in `project`.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn get_evaluator(
    pool: &SqlitePool,
    project: ProjectId,
    id: Uuid,
) -> Result<Option<EvaluatorRow>, MetaError> {
    let record: Option<EvaluatorRecord> = sqlx::query_as(
        "SELECT id, project_id, name, kind, config, output_score, connection_id, secret_enc, \
         version, created_by, created_at, updated_at \
         FROM evaluators WHERE project_id = ? AND id = ?",
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
pub(crate) async fn get_evaluator_tx(
    conn: &mut SqliteConnection,
    project: ProjectId,
    id: Uuid,
) -> Result<Option<EvaluatorRow>, MetaError> {
    let record: Option<EvaluatorRecord> = sqlx::query_as(
        "SELECT id, project_id, name, kind, config, output_score, connection_id, secret_enc, \
         version, created_by, created_at, updated_at \
         FROM evaluators WHERE project_id = ? AND id = ?",
    )
    .bind(project.to_string())
    .bind(id.to_string())
    .fetch_optional(&mut *conn)
    .await?;
    record.map(row_from).transpose()
}

/// Inserts an evaluator. `id` is chosen by the caller, because the AAD of the
/// sealed column needs it before the insert.
///
/// A duplicate name is [`MetaError::Conflict`] `"an evaluator with this name
/// already exists"`; the project's 501st evaluator is `"a project has at most
/// 500 evaluators"`.
///
/// # Errors
///
/// Returns [`MetaError`] when the insert fails or a stored value does not decode.
pub(crate) async fn insert_evaluator(
    conn: &mut SqliteConnection,
    id: Uuid,
    project: ProjectId,
    kind: &'static str,
    fields: EvaluatorFields,
    created_by: Option<Uuid>,
    now: i64,
) -> Result<EvaluatorRow, MetaError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM evaluators WHERE project_id = ?")
        .bind(project.to_string())
        .fetch_one(&mut *conn)
        .await?;
    if count >= PROJECT_CAP {
        return Err(MetaError::Conflict("a project has at most 500 evaluators"));
    }
    sqlx::query(
        "INSERT INTO evaluators \
         (id, project_id, name, kind, config, output_score, connection_id, secret_enc, version, \
         created_by, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(project.to_string())
    .bind(fields.name)
    .bind(kind)
    .bind(fields.config)
    .bind(fields.output_score)
    .bind(fields.connection_id.map(|value| value.to_string()))
    .bind(fields.secret_enc)
    .bind(i64::from(fields.version))
    .bind(created_by.map(|user| user.to_string()))
    .bind(now)
    .bind(now)
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, "an evaluator with this name already exists"))?;
    get_evaluator_tx(conn, project, id)
        .await?
        .ok_or(MetaError::Corrupt {
            table: "evaluators",
            column: "id",
        })
}

/// Replaces every editable column and bumps `updated_at`. `None` means the
/// evaluator is missing or belongs to another project. `kind` and `created_by`
/// are kept.
///
/// # Errors
///
/// Returns [`MetaError`] when the update fails or a stored value does not decode.
pub(crate) async fn update_evaluator(
    conn: &mut SqliteConnection,
    project: ProjectId,
    id: Uuid,
    fields: EvaluatorFields,
    now: i64,
) -> Result<Option<EvaluatorRow>, MetaError> {
    let result = sqlx::query(
        "UPDATE evaluators SET name = ?, config = ?, output_score = ?, connection_id = ?, \
         secret_enc = ?, version = ?, updated_at = ? \
         WHERE project_id = ? AND id = ?",
    )
    .bind(fields.name)
    .bind(fields.config)
    .bind(fields.output_score)
    .bind(fields.connection_id.map(|value| value.to_string()))
    .bind(fields.secret_enc)
    .bind(i64::from(fields.version))
    .bind(now)
    .bind(project.to_string())
    .bind(id.to_string())
    .execute(&mut *conn)
    .await
    .map_err(|error| unique_or(error, "an evaluator with this name already exists"))?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_evaluator_tx(conn, project, id)
        .await?
        .ok_or(MetaError::Corrupt {
            table: "evaluators",
            column: "id",
        })
        .map(Some)
}

/// Deletes the evaluator. `false` means it is missing or belongs to another project.
///
/// # Errors
///
/// Returns [`MetaError`] when the delete fails.
pub(crate) async fn delete_evaluator(
    conn: &mut SqliteConnection,
    project: ProjectId,
    id: Uuid,
) -> Result<bool, MetaError> {
    let result = sqlx::query("DELETE FROM evaluators WHERE project_id = ? AND id = ?")
        .bind(project.to_string())
        .bind(id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// True when any evaluator, in any project, uses connection `id`.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails.
pub(crate) async fn connection_in_use(
    conn: &mut SqliteConnection,
    id: Uuid,
) -> Result<bool, MetaError> {
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM evaluators WHERE connection_id = ?")
        .bind(id.to_string())
        .fetch_one(&mut *conn)
        .await?;
    Ok(count > 0)
}

#[derive(sqlx::FromRow)]
struct EvaluatorRecord {
    id: String,
    project_id: String,
    name: String,
    kind: String,
    config: String,
    output_score: String,
    connection_id: Option<String>,
    secret_enc: Option<Vec<u8>>,
    version: i64,
    created_by: Option<String>,
    created_at: i64,
    updated_at: i64,
}

fn row_from(record: EvaluatorRecord) -> Result<EvaluatorRow, MetaError> {
    Ok(EvaluatorRow {
        id: parse_uuid(&record.id, "id")?,
        project_id: ProjectId::from_uuid(parse_uuid(&record.project_id, "project_id")?),
        name: record.name,
        kind: record.kind,
        config: record.config,
        output_score: record.output_score,
        connection_id: optional_uuid(record.connection_id, "connection_id")?,
        secret_enc: record.secret_enc,
        version: u32::try_from(record.version).map_err(|_| corrupt("version"))?,
        created_by: optional_uuid(record.created_by, "created_by")?,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

fn corrupt(column: &'static str) -> MetaError {
    MetaError::Corrupt {
        table: "evaluators",
        column,
    }
}

fn parse_uuid(value: &str, column: &'static str) -> Result<Uuid, MetaError> {
    value.parse().map_err(|_| corrupt(column))
}

fn optional_uuid(value: Option<String>, column: &'static str) -> Result<Option<Uuid>, MetaError> {
    value.map(|text| parse_uuid(&text, column)).transpose()
}

#[cfg(test)]
mod tests {
    use llmobs_core::ids::{OrgId, ProjectId};
    use uuid::Uuid;

    use super::{EvaluatorFields, EvaluatorRow, PROJECT_CAP};
    use crate::meta::connections::{ConnectionFields, insert_connection};
    use crate::meta::evaluators::{
        connection_in_use, delete_evaluator, get_evaluator, insert_evaluator, list_evaluators,
        update_evaluator,
    };
    use crate::meta::orgs::insert_org;
    use crate::meta::projects::insert_project;
    use crate::meta::users::{NewUser, insert_user};
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

    fn fields(name: &str) -> EvaluatorFields {
        EvaluatorFields {
            name: name.to_owned(),
            config: r#"{"model":"gpt-5-mini"}"#.to_owned(),
            output_score: r#"{"name":"relevance","data_type":"numeric"}"#.to_owned(),
            connection_id: None,
            secret_enc: None,
            version: 1,
        }
    }

    async fn insert(db: &MetaDb, project: ProjectId, name: &str) -> EvaluatorRow {
        let name = name.to_owned();
        db.write(move |conn| {
            Box::pin(async move {
                insert_evaluator(
                    conn,
                    Uuid::now_v7(),
                    project,
                    "llm_judge",
                    fields(&name),
                    None,
                    NOW,
                )
                .await
            })
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn insert_list_get_round_trip_of_every_column() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;
        let author = db
            .write(|conn| {
                Box::pin(async move {
                    insert_user(
                        conn,
                        NewUser {
                            email: "ada@example.com".to_owned(),
                            name: None,
                            password_hash: None,
                            email_verified_at: None,
                            is_instance_admin: false,
                        },
                        NOW,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        let connection = db
            .write(move |conn| {
                Box::pin(async move {
                    insert_connection(
                        conn,
                        Uuid::now_v7(),
                        acme,
                        ConnectionFields {
                            name: "main".to_owned(),
                            adapter: "openai".to_owned(),
                            base_url: None,
                            secret_enc: vec![1],
                            api_key_hint: "WXYZ".to_owned(),
                            extra_headers_enc: None,
                            extra_header_names: Vec::new(),
                            default_model: None,
                            structured_output: "native".to_owned(),
                            max_concurrency: 4,
                            requests_per_minute: 300,
                        },
                        None,
                        NOW,
                    )
                    .await
                })
            })
            .await
            .unwrap();

        let id = Uuid::now_v7();
        let inserted = db
            .write(move |conn| {
                Box::pin(async move {
                    insert_evaluator(
                        conn,
                        id,
                        support,
                        "webhook",
                        EvaluatorFields {
                            name: "Hook".to_owned(),
                            config: r#"{"url_host":"hooks.example.com"}"#.to_owned(),
                            output_score: r#"{"name":"hook","data_type":"boolean"}"#.to_owned(),
                            connection_id: Some(connection.id),
                            secret_enc: Some(vec![4, 5, 6]),
                            version: 3,
                        },
                        Some(author.id),
                        NOW,
                    )
                    .await
                })
            })
            .await
            .unwrap();

        let expected = EvaluatorRow {
            id,
            project_id: support,
            name: "Hook".to_owned(),
            kind: "webhook".to_owned(),
            config: r#"{"url_host":"hooks.example.com"}"#.to_owned(),
            output_score: r#"{"name":"hook","data_type":"boolean"}"#.to_owned(),
            connection_id: Some(connection.id),
            secret_enc: Some(vec![4, 5, 6]),
            version: 3,
            created_by: Some(author.id),
            created_at: NOW,
            updated_at: NOW,
        };
        assert_eq!(inserted, expected);
        let read = get_evaluator(db.reader(), support, id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(read, expected);
        assert!(
            get_evaluator(db.reader(), ProjectId::from_uuid(Uuid::now_v7()), id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn list_is_ordered_by_name() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;
        insert(&db, support, "b").await;
        insert(&db, support, "a").await;
        let names: Vec<String> = list_evaluators(db.reader(), support)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.name)
            .collect();
        assert_eq!(names, ["a", "b"]);
    }

    #[tokio::test]
    async fn duplicate_name_is_a_conflict() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;
        insert(&db, support, "same").await;
        let error = db
            .write(move |conn| {
                Box::pin(async move {
                    insert_evaluator(
                        conn,
                        Uuid::now_v7(),
                        support,
                        "check",
                        fields("same"),
                        None,
                        NOW,
                    )
                    .await
                })
            })
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                MetaError::Conflict("an evaluator with this name already exists")
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn project_cap_is_500() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;
        db.write(move |conn| {
            Box::pin(async move {
                for index in 0..PROJECT_CAP {
                    insert_evaluator(
                        conn,
                        Uuid::now_v7(),
                        support,
                        "check",
                        fields(&format!("e-{index:03}")),
                        None,
                        NOW,
                    )
                    .await?;
                }
                Ok(())
            })
        })
        .await
        .unwrap();
        let error = db
            .write(move |conn| {
                Box::pin(async move {
                    insert_evaluator(
                        conn,
                        Uuid::now_v7(),
                        support,
                        "check",
                        fields("one-too-many"),
                        None,
                        NOW,
                    )
                    .await
                })
            })
            .await
            .unwrap_err();
        assert!(
            matches!(
                error,
                MetaError::Conflict("a project has at most 500 evaluators")
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn update_unknown_is_none_and_known_replaces_columns() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;
        let row = insert(&db, support, "before").await;

        let missing = db
            .write(move |conn| {
                Box::pin(async move {
                    update_evaluator(conn, support, Uuid::now_v7(), fields("nope"), NOW + 1).await
                })
            })
            .await
            .unwrap();
        assert!(missing.is_none());

        let later = NOW + 500;
        let id = row.id;
        let updated = db
            .write(move |conn| {
                Box::pin(async move {
                    update_evaluator(
                        conn,
                        support,
                        id,
                        EvaluatorFields {
                            name: "after".to_owned(),
                            config: r#"{"model":"other"}"#.to_owned(),
                            output_score: r#"{"name":"after","data_type":"numeric"}"#.to_owned(),
                            connection_id: None,
                            secret_enc: Some(vec![7]),
                            version: 2,
                        },
                        later,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.name, "after");
        assert_eq!(updated.version, 2);
        assert_eq!(updated.secret_enc, Some(vec![7]));
        assert_eq!(updated.kind, "llm_judge");
        assert_eq!(updated.created_at, NOW);
        assert_eq!(updated.updated_at, later);
    }

    #[tokio::test]
    async fn delete_reports_whether_a_row_went() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;
        let row = insert(&db, support, "gone").await;
        let id = row.id;
        let first = db
            .write(move |conn| Box::pin(async move { delete_evaluator(conn, support, id).await }))
            .await
            .unwrap();
        let second = db
            .write(move |conn| Box::pin(async move { delete_evaluator(conn, support, id).await }))
            .await
            .unwrap();
        assert!(first);
        assert!(!second);
    }

    #[tokio::test]
    async fn connection_in_use_follows_the_rows() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;
        let connection_id = Uuid::now_v7();
        db.write(move |conn| {
            Box::pin(async move {
                insert_connection(
                    conn,
                    connection_id,
                    acme,
                    ConnectionFields {
                        name: "main".to_owned(),
                        adapter: "openai".to_owned(),
                        base_url: None,
                        secret_enc: vec![1],
                        api_key_hint: "WXYZ".to_owned(),
                        extra_headers_enc: None,
                        extra_header_names: Vec::new(),
                        default_model: None,
                        structured_output: "native".to_owned(),
                        max_concurrency: 4,
                        requests_per_minute: 300,
                    },
                    None,
                    NOW,
                )
                .await
                .map(|_| ())
            })
        })
        .await
        .unwrap();
        let judge = db
            .write(move |conn| {
                Box::pin(async move {
                    insert_evaluator(
                        conn,
                        Uuid::now_v7(),
                        support,
                        "llm_judge",
                        EvaluatorFields {
                            connection_id: Some(connection_id),
                            ..fields("judge")
                        },
                        None,
                        NOW,
                    )
                    .await
                })
            })
            .await
            .unwrap();

        let used = db
            .write(move |conn| {
                Box::pin(async move { connection_in_use(conn, connection_id).await })
            })
            .await
            .unwrap();
        assert!(used);

        let judge_id = judge.id;
        db.write(move |conn| {
            Box::pin(async move { delete_evaluator(conn, support, judge_id).await })
        })
        .await
        .unwrap();
        let used = db
            .write(move |conn| {
                Box::pin(async move { connection_in_use(conn, connection_id).await })
            })
            .await
            .unwrap();
        assert!(!used);
    }

    #[tokio::test]
    async fn deleting_the_org_cascades() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;
        insert(&db, support, "gone").await;
        db.write(move |conn| {
            Box::pin(async move {
                sqlx::query("DELETE FROM orgs WHERE id = ?")
                    .bind(acme.to_string())
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM evaluators")
            .fetch_one(db.reader())
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn debug_hides_the_sealed_bytes_and_the_prompt() {
        let (_dir, db) = crate::meta::test_db().await;
        let acme = org(&db).await;
        let support = project(&db, acme).await;
        let id = Uuid::now_v7();
        let config = r#"{"prompt":"Grade the answer against the secret rubric"}"#;
        let row = db
            .write(move |conn| {
                Box::pin(async move {
                    insert_evaluator(
                        conn,
                        id,
                        support,
                        "webhook",
                        EvaluatorFields {
                            config: config.to_owned(),
                            secret_enc: Some(vec![0xAB; 17]),
                            ..fields("hook")
                        },
                        None,
                        NOW,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        let text = format!("{row:?}");
        assert!(text.contains("secret_enc: Some(17)"), "{text}");
        assert!(!text.contains("171, 171"), "{text}");
        assert!(
            text.contains(&format!("config_bytes: {}", config.len())),
            "{text}"
        );
        assert!(
            text.contains(&format!("output_score_bytes: {}", row.output_score.len())),
            "{text}"
        );
        assert!(!text.contains("Grade the answer"), "{text}");
        assert!(!text.contains("secret rubric"), "{text}");
    }
}
