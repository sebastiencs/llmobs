//! `orgs` rows and slug rules (spec 04, spec 11).
#![cfg_attr(not(test), allow(dead_code))]

use llmobs_core::ids::OrgId;
use sqlx::{SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::{MetaError, unique_or};
use crate::auth::{OrgRole, parse_org_role};

/// Why a slug was rejected. The message does not contain the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SlugError {
    /// The shape is wrong.
    #[error("invalid slug")]
    Invalid,
    /// The slug is reserved for a UI route.
    #[error("reserved slug")]
    Reserved,
}

/// An `orgs` row. `limits` is not read in D.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrgRow {
    /// UUIDv7.
    pub(crate) id: OrgId,
    /// Display name.
    pub(crate) name: String,
    /// URL slug, unique across orgs.
    pub(crate) slug: String,
    /// `active` or `deleting`.
    pub(crate) state: String,
    /// Creation time, nanoseconds.
    pub(crate) created_at: i64,
    /// Last change to this row, nanoseconds.
    pub(crate) updated_at: i64,
}

/// UI routes a slug must not take. Project slugs do not use this list.
const RESERVED: &[&str] = &[
    "account", "admin", "api", "assets", "cli-auth", "healthz", "invite", "login", "logout", "mcp",
    "readyz", "reset", "settings", "setup", "signup", "static", "v1", "verify",
];

#[derive(Debug, sqlx::FromRow)]
struct OrgRecord {
    id: String,
    name: String,
    slug: String,
    state: String,
    created_at: i64,
    updated_at: i64,
}

/// 2–48 characters of `[a-z0-9-]`, no leading, trailing, or doubled `-`, and not reserved.
///
/// # Errors
///
/// Returns [`SlugError::Invalid`] for the shape and [`SlugError::Reserved`] for a UI route.
pub(crate) fn validate_slug(slug: &str) -> Result<(), SlugError> {
    validate_slug_shape(slug)?;
    if RESERVED.contains(&slug) {
        return Err(SlugError::Reserved);
    }
    Ok(())
}

/// Shape only. Project slugs use this and skip [`RESERVED`].
pub(crate) fn validate_slug_shape(slug: &str) -> Result<(), SlugError> {
    let chars: Vec<char> = slug.chars().collect();
    let length = chars.len();
    if !(2..=48).contains(&length) {
        return Err(SlugError::Invalid);
    }
    if !chars[0].is_ascii_alphanumeric() || !chars[length - 1].is_ascii_alphanumeric() {
        return Err(SlugError::Invalid);
    }
    if chars.windows(2).any(|pair| pair == ['-', '-'])
        || chars
            .iter()
            .any(|ch| !ch.is_ascii_lowercase() && !ch.is_ascii_digit() && *ch != '-')
    {
        return Err(SlugError::Invalid);
    }
    Ok(())
}

/// Lowercase, collapse non-alphanumerics to one `-`, trim, cut to 48.
///
/// Empty becomes `"org"`. A reserved result gets `-1` appended.
pub(crate) fn slug_from_name(name: &str) -> String {
    derive_slug(name, "org", true)
}

/// [`slug_from_name`] for a project: empty becomes `"project"`, and reserved words stay.
pub(crate) fn project_slug_from_name(name: &str) -> String {
    derive_slug(name, "project", false)
}

/// `base`, then `base-2` … `base-99`, each cut so the result is at most 48 characters.
pub(crate) fn slug_candidates(base: &str) -> Vec<String> {
    let mut slugs = vec![fit(base, None)];
    for suffix in 2..=99 {
        slugs.push(fit(base, Some(suffix)));
    }
    slugs
}

fn derive_slug(name: &str, empty: &str, reserved_suffix: bool) -> String {
    let lower = name.to_lowercase();
    let mut slug = String::new();
    let mut dash = false;
    for ch in lower.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            if dash && !slug.is_empty() {
                slug.push('-');
            }
            dash = false;
            slug.push(ch);
        } else {
            dash = true;
        }
    }
    let mut slug = fit(&slug, None);
    if slug.is_empty() {
        slug = empty.to_owned();
    }
    if reserved_suffix && RESERVED.contains(&slug.as_str()) {
        slug.push_str("-1");
    }
    slug
}

fn fit(base: &str, suffix: Option<u32>) -> String {
    let suffix = suffix.map(|n| format!("-{n}")).unwrap_or_default();
    let room = 48 - suffix.len();
    let mut stem: String = base.chars().take(room).collect();
    while stem.ends_with('-') {
        stem.pop();
    }
    format!("{stem}{suffix}")
}

/// Inserts an org. A duplicate slug is [`MetaError::Conflict`] `"slug taken"`.
///
/// # Errors
///
/// Returns [`MetaError`] when the insert fails.
pub(crate) async fn insert_org(
    conn: &mut SqliteConnection,
    name: &str,
    slug: &str,
    now: i64,
) -> Result<OrgRow, MetaError> {
    let id = OrgId::from_uuid(Uuid::now_v7());
    let record: OrgRecord = sqlx::query_as(
        "INSERT INTO orgs (id, name, slug, created_at, updated_at) VALUES (?, ?, ?, ?, ?) \
         RETURNING id, name, slug, state, created_at, updated_at",
    )
    .bind(id.to_string())
    .bind(name)
    .bind(slug)
    .bind(now)
    .bind(now)
    .fetch_one(&mut *conn)
    .await
    .map_err(|error| unique_or(error, "slug taken"))?;
    org_row(record)
}

/// Tries [`slug_from_name`], then `-2` … `-99`, in this transaction.
///
/// # Errors
///
/// Returns [`MetaError::Conflict`] `"slug taken"` when every candidate is used.
pub(crate) async fn insert_org_with_derived_slug(
    conn: &mut SqliteConnection,
    name: &str,
    now: i64,
) -> Result<OrgRow, MetaError> {
    for slug in slug_candidates(&slug_from_name(name)) {
        match insert_org(conn, name, &slug, now).await {
            Ok(row) => return Ok(row),
            Err(MetaError::Conflict(_)) => {}
            Err(error) => return Err(error),
        }
    }
    Err(MetaError::Conflict("slug taken"))
}

/// The org with this id.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or the id does not parse.
pub(crate) async fn org_by_id(pool: &SqlitePool, id: OrgId) -> Result<Option<OrgRow>, MetaError> {
    fetch_org(
        sqlx::query_as(
            "SELECT id, name, slug, state, created_at, updated_at FROM orgs WHERE id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(pool)
        .await?,
    )
}

/// The org with this slug.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or the id does not parse.
pub(crate) async fn org_by_slug(
    pool: &SqlitePool,
    slug: &str,
) -> Result<Option<OrgRow>, MetaError> {
    fetch_org(
        sqlx::query_as(
            "SELECT id, name, slug, state, created_at, updated_at FROM orgs WHERE slug = ?",
        )
        .bind(slug)
        .fetch_optional(pool)
        .await?,
    )
}

/// Replaces the display name. The slug stays.
///
/// # Errors
///
/// Returns [`MetaError::Db`] when the update fails.
pub(crate) async fn rename_org(
    conn: &mut SqliteConnection,
    id: OrgId,
    name: &str,
    now: i64,
) -> Result<(), MetaError> {
    sqlx::query("UPDATE orgs SET name = ?, updated_at = ? WHERE id = ?")
        .bind(name)
        .bind(now)
        .bind(id.to_string())
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Orgs the user belongs to, with that membership's role, ordered by name.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a role or id does not decode.
pub(crate) async fn orgs_of_user(
    pool: &SqlitePool,
    user_id: Uuid,
) -> Result<Vec<(OrgRow, OrgRole)>, MetaError> {
    let records: Vec<MembershipRecord> = sqlx::query_as(
        "SELECT orgs.id, orgs.name, orgs.slug, orgs.state, orgs.created_at, orgs.updated_at, \
         org_members.role FROM org_members JOIN orgs ON orgs.id = org_members.org_id \
         WHERE org_members.user_id = ? ORDER BY orgs.name, orgs.id",
    )
    .bind(user_id.to_string())
    .fetch_all(pool)
    .await?;
    records
        .into_iter()
        .map(|record| {
            let role = parse_org_role(&record.role).ok_or(MetaError::Corrupt {
                table: "org_members",
                column: "role",
            })?;
            Ok((org_row(record.org())?, role))
        })
        .collect()
}

#[derive(Debug, sqlx::FromRow)]
struct MembershipRecord {
    id: String,
    name: String,
    slug: String,
    state: String,
    created_at: i64,
    updated_at: i64,
    role: String,
}

impl MembershipRecord {
    fn org(self) -> OrgRecord {
        OrgRecord {
            id: self.id,
            name: self.name,
            slug: self.slug,
            state: self.state,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

fn fetch_org(record: Option<OrgRecord>) -> Result<Option<OrgRow>, MetaError> {
    record.map(org_row).transpose()
}

fn org_row(record: OrgRecord) -> Result<OrgRow, MetaError> {
    let id = record.id.parse().map_err(|_| MetaError::Corrupt {
        table: "orgs",
        column: "id",
    })?;
    Ok(OrgRow {
        id,
        name: record.name,
        slug: record.slug,
        state: record.state,
        created_at: record.created_at,
        updated_at: record.updated_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::test_db;

    #[test]
    fn slugs_match_the_shape_and_the_reserved_list() {
        assert!(validate_slug("acme").is_ok());
        assert!(validate_slug("a1").is_ok());
        for slug in ["a", "-a", "a-", "a--b", "A", &"a".repeat(49)] {
            assert_eq!(validate_slug(slug), Err(SlugError::Invalid), "{slug}");
        }
        assert_eq!(validate_slug("admin"), Err(SlugError::Reserved));
        assert_eq!(slug_from_name("Acme, Inc."), "acme-inc");
        assert_eq!(slug_from_name("!!!"), "org");
        assert_eq!(slug_from_name("Admin"), "admin-1");
        assert_eq!(project_slug_from_name("!!!"), "project");
        assert!(validate_slug_shape("login").is_ok());
    }

    #[tokio::test]
    async fn derived_slugs_skip_taken_values() {
        let (_dir, db) = test_db().await;
        db.write(|conn| {
            Box::pin(async move {
                let first = insert_org_with_derived_slug(conn, "Acme, Inc.", 1).await?;
                assert_eq!(first.slug, "acme-inc");
                let second = insert_org_with_derived_slug(conn, "Acme, Inc.", 1).await?;
                assert_eq!(second.slug, "acme-inc-2");
                let third = insert_org_with_derived_slug(conn, "Acme, Inc.", 1).await?;
                assert_eq!(third.slug, "acme-inc-3");

                let bang = insert_org_with_derived_slug(conn, "!!!", 1).await?;
                assert_eq!(bang.slug, "org");
                for _ in 0..98 {
                    insert_org_with_derived_slug(conn, "!!!", 1).await?;
                }
                let error = insert_org_with_derived_slug(conn, "!!!", 1)
                    .await
                    .unwrap_err();
                assert!(
                    matches!(error, MetaError::Conflict("slug taken")),
                    "{error}"
                );
                rename_org(conn, first.id, "Acme", 2).await?;
                Ok(())
            })
        })
        .await
        .unwrap();

        let org = org_by_slug(db.reader(), "acme-inc").await.unwrap().unwrap();
        assert_eq!(org.name, "Acme");
        assert_eq!(org.state, "active");
        assert_eq!(
            org_by_id(db.reader(), org.id).await.unwrap().unwrap().slug,
            "acme-inc"
        );
    }
}
