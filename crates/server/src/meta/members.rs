//! `org_members` and `project_members` (spec 04).
#![cfg_attr(not(test), allow(dead_code))]

use llmobs_core::ids::{OrgId, ProjectId};
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::users::UserRow;
use super::{MetaError, unique_or};
use crate::auth::{
    OrgRole, ProjectOverride, org_role_str, parse_org_role, parse_project_override,
    project_override_str,
};

/// One org membership, with the user, ordered lookups return these by email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MemberRow {
    /// The member.
    pub(crate) user: UserRow,
    /// Org role.
    pub(crate) role: OrgRole,
    /// `org_members.created_at`.
    pub(crate) joined_at: i64,
}

/// The user's role in the org.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or the stored role is unknown.
pub(crate) async fn org_role(
    pool: &SqlitePool,
    org_id: OrgId,
    user_id: Uuid,
) -> Result<Option<OrgRole>, MetaError> {
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM org_members WHERE org_id = ? AND user_id = ?")
            .bind(org_id.to_string())
            .bind(user_id.to_string())
            .fetch_optional(pool)
            .await?;
    role.map(|role| decode_org_role(&role)).transpose()
}

/// [`org_role`] on the writer connection.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or the stored role is unknown.
pub(crate) async fn org_role_tx(
    conn: &mut SqliteConnection,
    org_id: OrgId,
    user_id: Uuid,
) -> Result<Option<OrgRole>, MetaError> {
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM org_members WHERE org_id = ? AND user_id = ?")
            .bind(org_id.to_string())
            .bind(user_id.to_string())
            .fetch_optional(&mut *conn)
            .await?;
    role.map(|role| decode_org_role(&role)).transpose()
}

/// Inserts a membership. A second insert is [`MetaError::Conflict`] `"already a member"`.
///
/// # Errors
///
/// Returns [`MetaError`] when the insert fails.
pub(crate) async fn add_org_member(
    conn: &mut SqliteConnection,
    org_id: OrgId,
    user_id: Uuid,
    role: OrgRole,
    now: i64,
) -> Result<(), MetaError> {
    sqlx::query("INSERT INTO org_members (org_id, user_id, role, created_at) VALUES (?, ?, ?, ?)")
        .bind(org_id.to_string())
        .bind(user_id.to_string())
        .bind(org_role_str(role))
        .bind(now)
        .execute(&mut *conn)
        .await
        .map_err(|error| unique_or(error, "already a member"))?;
    Ok(())
}

/// Replaces the org role. `org_members` has no `updated_at`.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn set_org_role(
    conn: &mut SqliteConnection,
    org_id: OrgId,
    user_id: Uuid,
    role: OrgRole,
) -> Result<(), MetaError> {
    sqlx::query("UPDATE org_members SET role = ? WHERE org_id = ? AND user_id = ?")
        .bind(org_role_str(role))
        .bind(org_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Deletes the membership and this user's overrides on that org's projects.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when a delete fails.
pub(crate) async fn remove_org_member(
    conn: &mut SqliteConnection,
    org_id: OrgId,
    user_id: Uuid,
) -> Result<(), MetaError> {
    clear_project_overrides(conn, org_id, user_id).await?;
    sqlx::query("DELETE FROM org_members WHERE org_id = ? AND user_id = ?")
        .bind(org_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Deletes this user's overrides on the org's projects. The membership stays.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the delete fails.
pub(crate) async fn clear_project_overrides(
    conn: &mut SqliteConnection,
    org_id: OrgId,
    user_id: Uuid,
) -> Result<(), MetaError> {
    sqlx::query(
        "DELETE FROM project_members WHERE user_id = ? AND project_id IN \
         (SELECT id FROM projects WHERE org_id = ?)",
    )
    .bind(user_id.to_string())
    .bind(org_id.to_string())
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Memberships whose role is `owner`.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the query fails.
pub(crate) async fn count_owners(
    conn: &mut SqliteConnection,
    org_id: OrgId,
) -> Result<i64, MetaError> {
    sqlx::query_scalar("SELECT COUNT(*) FROM org_members WHERE org_id = ? AND role = 'owner'")
        .bind(org_id.to_string())
        .fetch_one(&mut *conn)
        .await
        .map_err(MetaError::from)
}

/// Members of the org, ordered by email.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a row does not decode.
pub(crate) async fn org_members(
    pool: &SqlitePool,
    org_id: OrgId,
) -> Result<Vec<MemberRow>, MetaError> {
    let records: Vec<MemberRecord> = sqlx::query_as(
        "SELECT users.id, users.email, users.name, users.password_hash, users.email_verified_at, \
         users.is_instance_admin, users.disabled_at, users.created_at, users.updated_at, \
         org_members.role, org_members.created_at AS joined_at \
         FROM org_members JOIN users ON users.id = org_members.user_id \
         WHERE org_members.org_id = ? ORDER BY users.email",
    )
    .bind(org_id.to_string())
    .fetch_all(pool)
    .await?;
    records.into_iter().map(member_row).collect()
}

/// The user's project override, if one is stored.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or the stored role is unknown.
pub(crate) async fn project_override(
    pool: &SqlitePool,
    project_id: ProjectId,
    user_id: Uuid,
) -> Result<Option<ProjectOverride>, MetaError> {
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM project_members WHERE project_id = ? AND user_id = ?")
            .bind(project_id.to_string())
            .bind(user_id.to_string())
            .fetch_optional(pool)
            .await?;
    role.map(|role| decode_override(&role)).transpose()
}

/// Inserts or replaces the project override.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the write fails.
pub(crate) async fn set_project_override(
    conn: &mut SqliteConnection,
    project_id: ProjectId,
    user_id: Uuid,
    role: ProjectOverride,
) -> Result<(), MetaError> {
    sqlx::query(
        "INSERT INTO project_members (project_id, user_id, role) VALUES (?, ?, ?) \
         ON CONFLICT(project_id, user_id) DO UPDATE SET role = excluded.role",
    )
    .bind(project_id.to_string())
    .bind(user_id.to_string())
    .bind(project_override_str(role))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Deletes the project override.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the delete fails.
pub(crate) async fn remove_project_override(
    conn: &mut SqliteConnection,
    project_id: ProjectId,
    user_id: Uuid,
) -> Result<(), MetaError> {
    sqlx::query("DELETE FROM project_members WHERE project_id = ? AND user_id = ?")
        .bind(project_id.to_string())
        .bind(user_id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Overrides on the project, ordered by user id.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a stored role is unknown.
pub(crate) async fn project_overrides(
    pool: &SqlitePool,
    project_id: ProjectId,
) -> Result<Vec<(Uuid, ProjectOverride)>, MetaError> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT user_id, role FROM project_members WHERE project_id = ? ORDER BY user_id",
    )
    .bind(project_id.to_string())
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(|(user_id, role)| {
            let user_id = Uuid::parse_str(&user_id).map_err(|_| MetaError::Corrupt {
                table: "project_members",
                column: "user_id",
            })?;
            Ok((user_id, decode_override(&role)?))
        })
        .collect()
}

#[derive(Debug, sqlx::FromRow)]
struct MemberRecord {
    id: String,
    email: String,
    name: Option<String>,
    password_hash: Option<String>,
    email_verified_at: Option<i64>,
    is_instance_admin: i64,
    disabled_at: Option<i64>,
    created_at: i64,
    updated_at: i64,
    role: String,
    joined_at: i64,
}

fn member_row(record: MemberRecord) -> Result<MemberRow, MetaError> {
    let id = Uuid::parse_str(&record.id).map_err(|_| MetaError::Corrupt {
        table: "users",
        column: "id",
    })?;
    let is_instance_admin = match record.is_instance_admin {
        0 => false,
        1 => true,
        _ => {
            return Err(MetaError::Corrupt {
                table: "users",
                column: "is_instance_admin",
            });
        }
    };
    Ok(MemberRow {
        user: UserRow {
            id,
            email: record.email,
            name: record.name,
            password_hash: record.password_hash,
            email_verified_at: record.email_verified_at,
            is_instance_admin,
            disabled_at: record.disabled_at,
            created_at: record.created_at,
            updated_at: record.updated_at,
        },
        role: decode_org_role(&record.role)?,
        joined_at: record.joined_at,
    })
}

fn decode_org_role(value: &str) -> Result<OrgRole, MetaError> {
    parse_org_role(value).ok_or(MetaError::Corrupt {
        table: "org_members",
        column: "role",
    })
}

fn decode_override(value: &str) -> Result<ProjectOverride, MetaError> {
    parse_project_override(value).ok_or(MetaError::Corrupt {
        table: "project_members",
        column: "role",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::OrgRole;
    use crate::meta::orgs::insert_org;
    use crate::meta::projects::insert_project;
    use crate::meta::test_db;
    use crate::meta::users::{NewUser, insert_user, normalize_email};

    #[tokio::test]
    async fn memberships_roles_and_override_removal() {
        let (_dir, db) = test_db().await;
        let (ada, bob, org_a, org_b, project_a, project_b) = db
            .write(|conn| {
                Box::pin(async move {
                    let ada = insert_user(conn, user("ada@example.com"), 1).await?;
                    let bob = insert_user(conn, user("bob@example.com"), 1).await?;
                    let org_a = insert_org(conn, "Alpha", "alpha", 1).await?;
                    let org_b = insert_org(conn, "Beta", "beta", 1).await?;
                    add_org_member(conn, org_a.id, ada.id, OrgRole::Owner, 1).await?;
                    add_org_member(conn, org_a.id, bob.id, OrgRole::Member, 2).await?;
                    add_org_member(conn, org_b.id, ada.id, OrgRole::Viewer, 1).await?;
                    let again = add_org_member(conn, org_a.id, ada.id, OrgRole::Owner, 1).await;
                    assert!(
                        matches!(again, Err(MetaError::Conflict("already a member"))),
                        "{again:?}"
                    );
                    set_org_role(conn, org_a.id, bob.id, OrgRole::Admin).await?;
                    assert_eq!(
                        org_role_tx(conn, org_a.id, bob.id).await?,
                        Some(OrgRole::Admin)
                    );
                    assert_eq!(count_owners(conn, org_a.id).await?, 1);

                    let project_a = insert_project(conn, org_a.id, "App", "app", None, 1).await?;
                    let project_b = insert_project(conn, org_b.id, "App", "app", None, 1).await?;
                    set_project_override(conn, project_a.id, ada.id, ProjectOverride::None).await?;
                    set_project_override(conn, project_a.id, ada.id, ProjectOverride::Viewer)
                        .await?;
                    set_project_override(conn, project_b.id, ada.id, ProjectOverride::Admin)
                        .await?;
                    remove_org_member(conn, org_a.id, ada.id).await?;
                    Ok((ada, bob, org_a, org_b, project_a, project_b))
                })
            })
            .await
            .unwrap();

        assert_eq!(org_role(db.reader(), org_a.id, ada.id).await.unwrap(), None);
        assert_eq!(
            project_override(db.reader(), project_a.id, ada.id)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            project_override(db.reader(), project_b.id, ada.id)
                .await
                .unwrap(),
            Some(ProjectOverride::Admin)
        );
        let overrides = project_overrides(db.reader(), project_b.id).await.unwrap();
        assert_eq!(overrides, vec![(ada.id, ProjectOverride::Admin)]);

        let members = org_members(db.reader(), org_a.id).await.unwrap();
        assert_eq!(
            members
                .iter()
                .map(|member| (member.user.email.as_str(), member.role))
                .collect::<Vec<_>>(),
            vec![("bob@example.com", OrgRole::Admin)]
        );
        assert_eq!(members[0].joined_at, 2);

        let orgs = crate::meta::orgs::orgs_of_user(db.reader(), ada.id)
            .await
            .unwrap();
        assert_eq!(
            orgs.iter()
                .map(|(org, role)| (org.slug.as_str(), *role))
                .collect::<Vec<_>>(),
            vec![("beta", OrgRole::Viewer)]
        );

        db.write(move |conn| {
            Box::pin(async move {
                remove_project_override(conn, project_b.id, ada.id).await?;
                sqlx::query("UPDATE org_members SET role = 'superuser' WHERE user_id = ?")
                    .bind(bob.id.to_string())
                    .execute(&mut *conn)
                    .await?;
                sqlx::query("DELETE FROM users WHERE id = ?")
                    .bind(bob.id.to_string())
                    .execute(&mut *conn)
                    .await?;
                let _ = (org_b, project_a);
                Ok(())
            })
        })
        .await
        .unwrap();
        assert!(
            project_override(db.reader(), project_b.id, ada.id)
                .await
                .unwrap()
                .is_none()
        );

        let (_dir, db) = test_db().await;
        let bob = db
            .write(|conn| {
                Box::pin(async move {
                    let bob = insert_user(conn, user("bob@example.com"), 1).await?;
                    let org = insert_org(conn, "Alpha", "alpha", 1).await?;
                    add_org_member(conn, org.id, bob.id, OrgRole::Member, 1).await?;
                    sqlx::query("UPDATE org_members SET role = 'superuser' WHERE user_id = ?")
                        .bind(bob.id.to_string())
                        .execute(&mut *conn)
                        .await?;
                    Ok(bob)
                })
            })
            .await
            .unwrap();
        let org = crate::meta::orgs::org_by_slug(db.reader(), "alpha")
            .await
            .unwrap()
            .unwrap();
        let error = org_role(db.reader(), org.id, bob.id).await.unwrap_err();
        assert!(
            matches!(
                error,
                MetaError::Corrupt {
                    table: "org_members",
                    column: "role",
                }
            ),
            "{error}"
        );

        db.write(move |conn| {
            Box::pin(async move {
                let project = insert_project(conn, org.id, "App", "app", None, 1).await?;
                set_project_override(conn, project.id, bob.id, ProjectOverride::Member).await?;
                sqlx::query("DELETE FROM users WHERE id = ?")
                    .bind(bob.id.to_string())
                    .execute(&mut *conn)
                    .await?;
                let members: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM org_members WHERE user_id = ?")
                        .bind(bob.id.to_string())
                        .fetch_one(&mut *conn)
                        .await?;
                let overrides: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM project_members WHERE user_id = ?")
                        .bind(bob.id.to_string())
                        .fetch_one(&mut *conn)
                        .await?;
                assert_eq!(members, 0);
                assert_eq!(overrides, 0);
                Ok(())
            })
        })
        .await
        .unwrap();
    }

    fn user(email: &str) -> NewUser {
        NewUser {
            email: normalize_email(email).unwrap(),
            name: None,
            password_hash: None,
            email_verified_at: None,
            is_instance_admin: false,
        }
    }
}
