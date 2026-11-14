//! `projects` rows (spec 04).
#![cfg_attr(not(test), allow(dead_code))]

use llmobs_core::ids::{OrgId, ProjectId};
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::orgs::{project_slug_from_name, slug_candidates};
use super::{MetaError, unique_or};

/// A `projects` row. `settings` is not read in D.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProjectRow {
    /// UUIDv7.
    pub(crate) id: ProjectId,
    /// Owning org.
    pub(crate) org_id: OrgId,
    /// Display name.
    pub(crate) name: String,
    /// URL slug, unique inside the org.
    pub(crate) slug: String,
    /// Days to keep data. Absent means no expiry.
    pub(crate) retention_days: Option<u32>,
    /// `active` or `deleting`.
    pub(crate) state: String,
    /// Creation time, nanoseconds.
    pub(crate) created_at: i64,
    /// Last change to this row, nanoseconds.
    pub(crate) updated_at: i64,
}

/// Fields [`update_project`] writes. An outer `None` leaves the column alone.
/// `retention_days: Some(None)` clears it.
#[derive(Debug, Clone)]
pub(crate) struct ProjectPatch {
    /// New display name.
    pub(crate) name: Option<String>,
    /// New retention, or clear it.
    pub(crate) retention_days: Option<Option<u32>>,
}

#[derive(Debug, sqlx::FromRow)]
struct ProjectRecord {
    id: String,
    org_id: String,
    name: String,
    slug: String,
    retention_days: Option<i64>,
    state: String,
    created_at: i64,
    updated_at: i64,
}

/// Inserts a project. A duplicate `(org_id, slug)` is [`MetaError::Conflict`] `"slug taken"`.
///
/// # Errors
///
/// Returns [`MetaError`] when the insert fails or `retention_days` is outside `1..=36500`.
pub(crate) async fn insert_project(
    conn: &mut SqliteConnection,
    org_id: OrgId,
    name: &str,
    slug: &str,
    retention_days: Option<u32>,
    now: i64,
) -> Result<ProjectRow, MetaError> {
    let retention = encode_retention(retention_days)?;
    let id = ProjectId::from_uuid(Uuid::now_v7());
    let record: ProjectRecord = sqlx::query_as(
        "INSERT INTO projects (id, org_id, name, slug, retention_days, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         RETURNING id, org_id, name, slug, retention_days, state, created_at, updated_at",
    )
    .bind(id.to_string())
    .bind(org_id.to_string())
    .bind(name)
    .bind(slug)
    .bind(retention)
    .bind(now)
    .bind(now)
    .fetch_one(&mut *conn)
    .await
    .map_err(|error| unique_or(error, "slug taken"))?;
    project_row(record)
}

/// Tries [`project_slug_from_name`], then `-2` … `-99`, in this transaction.
///
/// # Errors
///
/// Returns [`MetaError::Conflict`] `"slug taken"` when every candidate is used.
pub(crate) async fn insert_project_with_derived_slug(
    conn: &mut SqliteConnection,
    org_id: OrgId,
    name: &str,
    retention_days: Option<u32>,
    now: i64,
) -> Result<ProjectRow, MetaError> {
    for slug in slug_candidates(&project_slug_from_name(name)) {
        match insert_project(conn, org_id, name, &slug, retention_days, now).await {
            Ok(row) => return Ok(row),
            Err(MetaError::Conflict(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Err(MetaError::Conflict("slug taken"))
}

/// The project with this id.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn project_by_id(
    pool: &SqlitePool,
    id: ProjectId,
) -> Result<Option<ProjectRow>, MetaError> {
    fetch_project(
        sqlx::query_as(
            "SELECT id, org_id, name, slug, retention_days, state, created_at, updated_at \
             FROM projects WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(pool)
        .await?,
    )
}

/// Deletes the project row; foreign keys cascade to its keys, overrides and every
/// project-owned table (D-M-7). A missing row is not an error.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the delete fails.
pub(crate) async fn delete_project(
    conn: &mut SqliteConnection,
    id: ProjectId,
) -> Result<(), MetaError> {
    sqlx::query("DELETE FROM projects WHERE id = ?")
        .bind(id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Every project id of the org, whatever its state, ordered by id.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored id does not parse.
pub(crate) async fn project_ids_of_org<'e, E>(
    executor: E,
    org: OrgId,
) -> Result<Vec<ProjectId>, MetaError>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    let texts: Vec<String> =
        sqlx::query_scalar("SELECT id FROM projects WHERE org_id = ? ORDER BY id")
            .bind(org.to_string())
            .fetch_all(executor)
            .await?;
    texts
        .iter()
        .map(|text| {
            text.parse::<ProjectId>().map_err(|_| MetaError::Corrupt {
                table: "projects",
                column: "id",
            })
        })
        .collect()
}

/// The project addressed by org slug and project slug.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn project_by_slugs(
    pool: &SqlitePool,
    org_slug: &str,
    project_slug: &str,
) -> Result<Option<ProjectRow>, MetaError> {
    fetch_project(
        sqlx::query_as(
            "SELECT projects.id, projects.org_id, projects.name, projects.slug, \
             projects.retention_days, projects.state, projects.created_at, projects.updated_at \
             FROM projects JOIN orgs ON orgs.id = projects.org_id \
             WHERE orgs.slug = ? AND projects.slug = ?",
        )
        .bind(org_slug)
        .bind(project_slug)
        .fetch_optional(pool)
        .await?,
    )
}

/// Projects in the org, ordered by name.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored value does not decode.
pub(crate) async fn projects_of_org(
    pool: &SqlitePool,
    org_id: OrgId,
) -> Result<Vec<ProjectRow>, MetaError> {
    let records: Vec<ProjectRecord> = sqlx::query_as(
        "SELECT id, org_id, name, slug, retention_days, state, created_at, updated_at \
         FROM projects WHERE org_id = ? ORDER BY name, id",
    )
    .bind(org_id.to_string())
    .fetch_all(pool)
    .await?;
    records.into_iter().map(project_row).collect()
}

/// `(org, project, retention_days)` of every project with `state = 'active'` in an active
/// org, ordered by project id. Projects without retention are included: an org cap
/// (D-M-15) applies to them too.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a value does not decode.
pub(crate) async fn projects_for_retention(
    pool: &SqlitePool,
) -> Result<Vec<(OrgId, ProjectId, Option<u32>)>, MetaError> {
    let rows: Vec<(String, String, Option<i64>)> = sqlx::query_as(
        "SELECT projects.org_id, projects.id, projects.retention_days \
         FROM projects JOIN orgs ON orgs.id = projects.org_id \
         WHERE projects.state = 'active' AND orgs.state = 'active' \
         ORDER BY projects.id",
    )
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(org_id, id, days)| {
            let org_id = org_id.parse().map_err(|_| MetaError::Corrupt {
                table: "projects",
                column: "org_id",
            })?;
            let id = id.parse().map_err(|_| MetaError::Corrupt {
                table: "projects",
                column: "id",
            })?;
            Ok((org_id, id, decode_retention(days)?))
        })
        .collect()
}

/// Applies the patch and sets `updated_at`.
///
/// # Errors
///
/// Returns [`MetaError`] when the update fails or retention is outside `1..=36500`.
pub(crate) async fn update_project(
    conn: &mut SqliteConnection,
    id: ProjectId,
    patch: ProjectPatch,
    now: i64,
) -> Result<(), MetaError> {
    let id_text = id.to_string();
    match (&patch.name, patch.retention_days) {
        (Some(name), Some(days)) => {
            let days = encode_retention(days)?;
            sqlx::query(
                "UPDATE projects SET name = ?, retention_days = ?, updated_at = ? WHERE id = ?",
            )
            .bind(name)
            .bind(days)
            .bind(now)
            .bind(id_text)
            .execute(&mut *conn)
            .await?;
        }
        (Some(name), None) => {
            sqlx::query("UPDATE projects SET name = ?, updated_at = ? WHERE id = ?")
                .bind(name)
                .bind(now)
                .bind(id_text)
                .execute(&mut *conn)
                .await?;
        }
        (None, Some(days)) => {
            let days = encode_retention(days)?;
            sqlx::query("UPDATE projects SET retention_days = ?, updated_at = ? WHERE id = ?")
                .bind(days)
                .bind(now)
                .bind(id_text)
                .execute(&mut *conn)
                .await?;
        }
        (None, None) => {
            sqlx::query("UPDATE projects SET updated_at = ? WHERE id = ?")
                .bind(now)
                .bind(id_text)
                .execute(&mut *conn)
                .await?;
        }
    }
    Ok(())
}

fn encode_retention(days: Option<u32>) -> Result<Option<i64>, MetaError> {
    match days {
        None => Ok(None),
        Some(days) if (1..=36_500).contains(&days) => Ok(Some(i64::from(days))),
        Some(_) => Err(MetaError::Corrupt {
            table: "projects",
            column: "retention_days",
        }),
    }
}

/// Decodes a stored `retention_days`: NULL is `None`, `1..=36_500` is `Some`, anything else is corrupt.
fn decode_retention(value: Option<i64>) -> Result<Option<u32>, MetaError> {
    let corrupt = MetaError::Corrupt {
        table: "projects",
        column: "retention_days",
    };
    match value {
        None => Ok(None),
        Some(days) if (1..=36_500).contains(&days) => {
            Ok(Some(u32::try_from(days).map_err(|_| corrupt)?))
        }
        Some(_) => Err(corrupt),
    }
}

fn fetch_project(record: Option<ProjectRecord>) -> Result<Option<ProjectRow>, MetaError> {
    record.map(project_row).transpose()
}

fn project_row(record: ProjectRecord) -> Result<ProjectRow, MetaError> {
    let id = record.id.parse().map_err(|_| MetaError::Corrupt {
        table: "projects",
        column: "id",
    })?;
    let org_id = record.org_id.parse().map_err(|_| MetaError::Corrupt {
        table: "projects",
        column: "org_id",
    })?;
    let retention_days = decode_retention(record.retention_days)?;
    Ok(ProjectRow {
        id,
        org_id,
        name: record.name,
        slug: record.slug,
        retention_days,
        state: record.state,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::orgs::insert_org;
    use crate::meta::test_db;

    #[tokio::test]
    async fn slugs_are_unique_per_org_and_patches_round_trip() {
        let (_dir, db) = test_db().await;
        let (left, right, project) = db
            .write(|conn| {
                Box::pin(async move {
                    let left = insert_org(conn, "Left", "left", 1).await?;
                    let right = insert_org(conn, "Right", "right", 1).await?;
                    let project = insert_project(conn, left.id, "API", "login", None, 1).await?;
                    let other = insert_project(conn, right.id, "API", "login", Some(30), 1).await?;
                    assert_eq!(project.slug, "login");
                    assert_eq!(other.slug, "login");
                    let derived =
                        insert_project_with_derived_slug(conn, left.id, "!!!", None, 1).await?;
                    assert_eq!(derived.slug, "project");
                    update_project(
                        conn,
                        project.id,
                        ProjectPatch {
                            name: Some("Logs".to_owned()),
                            retention_days: Some(Some(7)),
                        },
                        2,
                    )
                    .await?;
                    update_project(
                        conn,
                        project.id,
                        ProjectPatch {
                            name: None,
                            retention_days: Some(None),
                        },
                        3,
                    )
                    .await?;
                    Ok((left, right, project))
                })
            })
            .await
            .unwrap();

        let loaded = project_by_slugs(db.reader(), "left", "login")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(loaded.id, project.id);
        assert_eq!(loaded.name, "Logs");
        assert_eq!(loaded.retention_days, None);
        assert_eq!(loaded.updated_at, 3);
        let names: Vec<_> = projects_of_org(db.reader(), left.id)
            .await
            .unwrap()
            .into_iter()
            .map(|project| project.name)
            .collect();
        assert_eq!(names, ["!!!", "Logs"]);
        assert!(
            project_by_id(db.reader(), project.id)
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            project_by_slugs(db.reader(), "right", "login")
                .await
                .unwrap()
                .unwrap()
                .org_id,
            right.id
        );

        db.write(move |conn| {
            Box::pin(async move {
                sqlx::query("UPDATE projects SET retention_days = 0 WHERE id = ?")
                    .bind(project.id.to_string())
                    .execute(&mut *conn)
                    .await?;
                Ok(())
            })
        })
        .await
        .unwrap();
        let error = project_by_id(db.reader(), project.id).await.unwrap_err();
        assert!(
            matches!(
                error,
                MetaError::Corrupt {
                    table: "projects",
                    column: "retention_days",
                }
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn projects_for_retention_lists_active_ones() {
        let (_dir, db) = test_db().await;
        let (org, ids, deleting) = db
            .write(|conn| {
                Box::pin(async move {
                    let org = insert_org(conn, "Acme", "acme", 1).await?;
                    let mut ids = Vec::new();
                    for (slug, days) in [("none", None), ("week", Some(7)), ("month", Some(30))] {
                        let project = insert_project(conn, org.id, slug, slug, days, 1).await?;
                        ids.push((project.id, days));
                    }
                    let deleting = insert_project(conn, org.id, "gone", "gone", Some(1), 1).await?;
                    sqlx::query("UPDATE projects SET state = 'deleting' WHERE id = ?")
                        .bind(deleting.id.to_string())
                        .execute(&mut *conn)
                        .await?;
                    Ok((org, ids, deleting))
                })
            })
            .await
            .unwrap();

        let mut expected: Vec<(OrgId, ProjectId, Option<u32>)> =
            ids.iter().map(|(id, days)| (org.id, *id, *days)).collect();
        expected.sort_by_key(|(_, id, _)| *id);
        let listed = projects_for_retention(db.reader()).await.unwrap();
        assert_eq!(listed, expected);
        assert!(listed.iter().all(|(_, id, _)| *id != deleting.id));
    }
}
